//! One source read, native SHA-256 validation, and durable hash checkpoints.
use crate::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use cold_vtl_core::NativeChecksum;
use cold_vtl_provider_s3::MultipartPart;
use serde::{Deserialize, Serialize};
use sha2::digest::common::hazmat::{SerializableState, SerializedState};
use std::{
    io::{Seek, SeekFrom},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct SourceFile {
    path: String,
    size: u64,
    dev: u64,
    ino: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
fn snapshot(source: &Path) -> Result<Vec<SourceFile>> {
    cartridge_files(source)?
        .into_iter()
        .map(|path| {
            let m = path.metadata()?;
            Ok(SourceFile {
                path: path
                    .strip_prefix(source)?
                    .to_str()
                    .ok_or("non UTF-8 source")?
                    .into(),
                size: m.len(),
                dev: m.dev(),
                ino: m.ino(),
                modified: (m.mtime(), m.mtime_nsec()),
                changed: (m.ctime(), m.ctime_nsec()),
            })
        })
        .collect()
}
fn save_hash(hash: &Sha256) -> String {
    hex::encode(SerializableState::serialize(hash))
}
fn restore_hash(value: &str) -> Result<Sha256> {
    let bytes = hex::decode(value)?;
    let state = SerializedState::<Sha256>::try_from(bytes.as_slice())
        .map_err(|_| "invalid SHA-256 checkpoint length")?;
    <Sha256 as SerializableState>::deserialize(&state)
        .map_err(|_| "invalid SHA-256 checkpoint".into())
}
fn composite(parts: &[MultipartPart]) -> Result<NativeChecksum> {
    let mut hash = Sha256::new();
    for (index, part) in parts.iter().enumerate() {
        if part.part_number != index as i32 + 1 {
            return Err("unordered multipart receipts".into());
        }
        let bytes = STANDARD.decode(
            part.checksum_sha256
                .as_deref()
                .ok_or("missing part checksum")?,
        )?;
        if bytes.len() != 32 {
            return Err("invalid part checksum".into());
        }
        hash.update(bytes);
    }
    let checksum = NativeChecksum {
        value: format!("{}-{}", STANDARD.encode(hash.finalize()), parts.len()),
        kind: "COMPOSITE".into(),
    };
    if !checksum.validate() {
        return Err("invalid composite checksum".into());
    }
    Ok(checksum)
}
#[derive(Serialize, Deserialize)]
struct Active {
    key: String,
    offset: u64,
    size: u64,
    upload: Option<String>,
    create_intent: bool,
    complete_intent: bool,
    parts: Vec<MultipartPart>,
    hash: String,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    version: u8,
    hash_format: String,
    identity: serde_json::Value,
    source: Vec<SourceFile>,
    manifest: Manifest,
    file_index: usize,
    file_offset: u64,
    file_hash: String,
    active: Option<Active>,
    nonce: String,
}
struct Disk {
    path: PathBuf,
    _lock: File,
}
pub(super) fn journal_path(
    store: &S3Store,
    barcode: &Barcode,
    generation: u64,
    directory: &Path,
) -> Result<PathBuf> {
    let scope = serde_json::json!({"vtl":std::env::var("COLD_VTL_VTL_ID").ok(),"destination":std::env::var("COLD_VTL_DESTINATION_ID").ok(),"barcode":barcode,"generation":generation});
    let _ = store;
    Ok(directory.join(format!("{}.json", sha256_hex(&serde_json::to_vec(&scope)?))))
}
impl Disk {
    fn open(path: PathBuf, source: &Path) -> Result<Self> {
        reject_symlinks(&path)?;
        let directory = path.parent().ok_or("missing journal directory")?;
        reject_symlinks(directory)?;
        let mut normalized = PathBuf::new();
        for component in std::env::current_dir()?.join(directory).components() {
            match component {
                Component::ParentDir => {
                    normalized.pop();
                }
                Component::CurDir => {}
                other => normalized.push(other),
            }
        }
        if normalized.starts_with(source) {
            return Err("upload state must be outside cartridge media".into());
        }
        std::fs::create_dir_all(directory)?;
        let directory = directory.canonicalize()?;
        if directory.starts_with(source) {
            return Err("upload state must be outside cartridge media".into());
        }
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        // Persist newly created ancestors before a remote upload can exist.
        for ancestor in directory.ancestors() {
            File::open(ancestor)?.sync_all()?;
        }
        let path = directory.join(path.file_name().ok_or("missing journal filename")?);
        let lock_path = path.with_extension("lock");
        reject_symlinks(&lock_path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lock_path)?;
        lock.try_lock()
            .map_err(|_| "another export owns the upload journal")?;
        Ok(Self { path, _lock: lock })
    }
    fn save(&self, journal: &Journal) -> Result<()> {
        let temp = self.path.with_extension("tmp");
        reject_symlinks(&temp)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        serde_json::to_writer(&mut file, journal)?;
        file.sync_all()?;
        std::fs::rename(temp, &self.path)?;
        File::open(self.path.parent().unwrap())?.sync_all()?;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn export(
    store: &S3Store,
    barcode: Barcode,
    generation: u64,
    source: &Path,
    segment_size: u64,
    part_size: u64,
    directory: &Path,
    history: Option<&upload_history::Recorder>,
) -> Result<Manifest> {
    if generation == 0
        || segment_size == 0
        || part_size == 0
        || segment_size.div_ceil(part_size) > 10000
    {
        return Err("invalid export geometry".into());
    }
    reject_symlinks(source)?;
    let source = source.canonicalize()?;
    let _guard = cold_vtl_core::MediaReadGuard::acquire(&source)?;
    let files = snapshot(&source)?;
    let total = files.iter().try_fold(0u64, |n, f| {
        n.checked_add(f.size).ok_or("source size overflow")
    })?;
    if let Some(h) = history {
        h.set_total(total);
    }
    let cfg = store.config();
    let identity = serde_json::json!({"endpoint":cfg.endpoint,"region":cfg.region,"bucket":cfg.bucket,"prefix":cfg.prefix,
        "vtl":std::env::var("COLD_VTL_VTL_ID").ok(),"destination":std::env::var("COLD_VTL_DESTINATION_ID").ok(),
        "revision":std::env::var("COLD_VTL_CONFIGURATION_REVISION").ok(),"source":source,"segment_size":segment_size,"part_size":part_size});
    let disk = Disk::open(
        journal_path(store, &barcode, generation, directory)?,
        &source,
    )?;
    let mut journal = if disk.path.exists() {
        let value: Journal =
            serde_json::from_reader(File::open(&disk.path)?.take(64 * 1024 * 1024))?;
        if value.version != 2
            || value.hash_format != "sha2-0.11-sha256-v1"
            || value.identity != identity
            || value.source != files
            || value.manifest.barcode != barcode
            || value.manifest.generation != generation
        {
            return Err(
                "upload journal source, destination or settings changed; refusing resume".into(),
            );
        }
        value
    } else {
        // Never adopt a published generation without the local receipt proving its source.
        if store
            .object_exists(&manifest_key(cfg, &barcode, generation))
            .await?
        {
            return Err("manifest exists without a matching upload journal".into());
        }
        Journal {
            version: 2,
            hash_format: "sha2-0.11-sha256-v1".into(),
            identity,
            source: files,
            manifest: Manifest {
                format_version: 3,
                barcode,
                generation,
                logical_size_bytes: total,
                files: vec![],
                segments: vec![],
            },
            file_index: 0,
            file_offset: 0,
            file_hash: save_hash(&Sha256::new()),
            active: None,
            nonce: uuid::Uuid::new_v4().simple().to_string(),
        }
    };
    if journal.file_index > journal.source.len() {
        return Err("invalid journal file cursor".into());
    }
    disk.save(&journal)?;
    storage::require_access(store, true).await?;
    store.check_bucket().await?;
    // Revalidate finished segments when resuming, without re-reading their payload.
    for segment in &journal.manifest.segments {
        store
            .verify_native_head(
                &segment.object_key,
                segment.size_bytes,
                segment
                    .s3_checksum
                    .as_ref()
                    .ok_or("missing segment receipt")?,
            )
            .await?;
    }
    if let Some(h) = history {
        h.reuse(journal.manifest.segments.iter().map(|s| s.size_bytes).sum());
    }
    while journal.file_index < journal.source.len() {
        let file_info = journal.source[journal.file_index].clone();
        if journal.file_offset > file_info.size {
            return Err("invalid file checkpoint offset".into());
        }
        let mut file_hash = restore_hash(&journal.file_hash)?;
        if journal.active.is_none() && journal.file_offset == file_info.size {
            journal.manifest.files.push(CartridgeFile {
                path: file_info.path,
                size_bytes: file_info.size,
                checksum_sha256: hex::encode(file_hash.finalize()),
            });
            journal.file_index += 1;
            journal.file_offset = 0;
            journal.file_hash = save_hash(&Sha256::new());
            disk.save(&journal)?;
            continue;
        }
        if journal.active.is_none() {
            let sequence = journal.manifest.segments.len();
            journal.active = Some(Active {
                key: cfg.object_key(&format!(
                    "cartridges/{}/generations/{generation}/segments/{sequence:08}-{}.bin",
                    journal.manifest.barcode, journal.nonce
                )),
                offset: journal.file_offset,
                size: (file_info.size - journal.file_offset).min(segment_size),
                upload: None,
                create_intent: false,
                complete_intent: false,
                parts: vec![],
                hash: save_hash(&Sha256::new()),
            });
            disk.save(&journal)?;
        }
        let active = journal.active.as_ref().unwrap();
        let key = active.key.clone();
        let length = active.size;
        let start = active.offset;
        let mut segment_hash = restore_hash(&active.hash)?;
        let confirmed: u64 = active.parts.iter().map(|p| p.size).sum();
        if start.checked_add(confirmed) != Some(journal.file_offset) || confirmed > length {
            return Err("inconsistent part checkpoint".into());
        }
        if let Some(h) = history {
            h.reuse(confirmed);
        }
        if active.complete_intent && store.object_exists(&key).await? {
            store
                .verify_native_head(&key, length, &composite(&active.parts)?)
                .await?;
        } else {
            if active.upload.is_none() {
                if active.create_intent {
                    return Err(
                        "ambiguous CreateMultipartUpload: inspect owned upload before recovery"
                            .into(),
                    );
                }
                journal.active.as_mut().unwrap().create_intent = true;
                disk.save(&journal)?;
                let id = store.create_checked_multipart(&key).await?;
                journal.active.as_mut().unwrap().upload = Some(id);
                disk.save(&journal)?;
            }
            let id = journal.active.as_ref().unwrap().upload.clone().unwrap();
            let remote = store.list_multipart_parts(&key, &id).await?;
            for receipt in &journal.active.as_ref().unwrap().parts {
                if !remote.iter().any(|p| p == receipt) {
                    return Err("confirmed part checksum, ETag or size differs remotely".into());
                }
            }
            if let Some(h) = history {
                h.phase("uploading");
                h.set_active(true);
            }
            let mut file = File::open(source.join(&file_info.path))?;
            file.seek(SeekFrom::Start(journal.file_offset))?;
            while journal.file_offset < start + length {
                let count = (start + length - journal.file_offset).min(part_size) as usize;
                let mut buffer = vec![0; count];
                file.read_exact(&mut buffer)?;
                file_hash.update(&buffer);
                segment_hash.update(&buffer);
                let checksum = STANDARD.encode(Sha256::digest(&buffer));
                let number = journal.active.as_ref().unwrap().parts.len() as i32 + 1;
                let receipt = store
                    .upload_checked_part(&key, &id, number, buffer, &checksum)
                    .await?;
                journal.file_offset += count as u64;
                journal.file_hash = save_hash(&file_hash);
                let active = journal.active.as_mut().unwrap();
                active.parts.push(receipt);
                active.hash = save_hash(&segment_hash);
                disk.save(&journal)?;
                if let Some(h) = history {
                    h.acknowledge(count as u64);
                }
            }
            journal.active.as_mut().unwrap().complete_intent = true;
            disk.save(&journal)?;
            let active = journal.active.as_ref().unwrap();
            let expected = composite(&active.parts)?;
            // A lost completion response does not undo the assembly: the HEAD
            // below proves the object, so only a missing object fails here.
            if let Err(error) = store
                .complete_checked_multipart(&key, &id, &active.parts, &expected.value)
                .await
                && !matches!(store.object_exists(&key).await, Ok(true))
            {
                return Err(error.into());
            }
            if let Some(h) = history {
                h.set_active(false);
                h.phase("verifying");
            }
            store.verify_native_head(&key, length, &expected).await?;
        }
        let active = journal.active.take().unwrap();
        journal.manifest.segments.push(Segment {
            sequence: journal.manifest.segments.len() as u64,
            source_path: file_info.path,
            source_offset_bytes: start,
            size_bytes: length,
            checksum_sha256: hex::encode(segment_hash.finalize()),
            object_key: key,
            s3_checksum: Some(composite(&active.parts)?),
        });
        disk.save(&journal)?;
    }
    if let Some(h) = history {
        h.set_active(false);
        h.phase("finalizing");
    }
    if snapshot(&source)? != journal.source {
        return Err("source changed during export; manifest not published".into());
    }
    validate_identity(
        &journal.manifest,
        cfg,
        &journal.manifest.barcode,
        generation,
    )?;
    let bytes = serde_json::to_vec_pretty(&journal.manifest)?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err("manifest exceeds 16 MiB".into());
    }
    let key = manifest_key(cfg, &journal.manifest.barcode, generation);
    if store.object_exists(&key).await? {
        if read_manifest_bytes(store, &key).await? != bytes {
            return Err("published manifest differs".into());
        }
    } else {
        store
            .put_checked_bytes(
                &key,
                bytes.clone(),
                &STANDARD.encode(Sha256::digest(&bytes)),
            )
            .await?;
        if read_manifest_bytes(store, &key).await? != bytes {
            return Err("published manifest differs".into());
        }
    }
    if snapshot(&source)? != journal.source {
        return Err("source changed while publishing export".into());
    }
    // Keep the completed receipt: a lost worker/API response is resumable without a source rehash.
    Ok(journal.manifest)
}
#[cfg(test)]
mod tests;
