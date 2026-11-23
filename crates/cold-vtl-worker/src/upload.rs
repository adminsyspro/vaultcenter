//! Bounded file transport with a durable, exclusive resume journal.
use crate::*;
use cold_vtl_provider_s3::MultipartPart;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
};

const HASH_BUFFER_BYTES: usize = 1024 * 1024;
const UPLOAD_CONCURRENCY: usize = 4;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    endpoint: String,
    region: String,
    bucket: String,
    prefix: String,
    vtl: Option<String>,
    destination: Option<String>,
    revision: Option<String>,
    source: PathBuf,
    segment_size: u64,
    part_size: u64,
    manifest: Manifest,
}
#[derive(Debug, Default, Serialize, Deserialize)]
struct Transfer {
    create_intent: bool,
    upload_id: Option<String>,
    parts: BTreeMap<i32, MultipartPart>,
    complete_intent: bool,
    verified: bool,
}
impl Transfer {
    fn mark_verified(&mut self) {
        self.verified = true;
        // An assembled, verified object no longer needs multipart receipts. Keep
        // the journal bounded by the manifest plus one active upload's 10000 parts.
        self.parts.clear();
        self.upload_id = None;
        self.create_intent = false;
        self.complete_intent = false;
    }
}
#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    version: u32,
    identity: Identity,
    transfers: BTreeMap<String, Transfer>,
}
struct JournalFile {
    directory: PathBuf,
    path: PathBuf,
    _lock: File,
}
impl JournalFile {
    fn open(directory: &Path, identity: &Identity) -> Result<(Self, Journal)> {
        reject_symlinks(directory)?;
        let absolute = std::env::current_dir()?.join(directory);
        let mut directory = PathBuf::new();
        for component in absolute.components() {
            match component {
                Component::ParentDir => {
                    directory.pop();
                }
                Component::CurDir => {}
                other => directory.push(other),
            }
        }
        if directory.starts_with(&identity.source) {
            return Err("upload state must be outside cartridge media".into());
        }
        // Persist every newly created directory entry before recording an upload
        // intent. Otherwise a power loss could erase the entire journal directory.
        let mut ancestor = PathBuf::new();
        for component in directory.components() {
            ancestor.push(component);
            if !ancestor.exists() {
                match std::fs::DirBuilder::new().mode(0o700).create(&ancestor) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.into()),
                }
            }
            // Also sync existing ancestors: a previous process may have died
            // between mkdir and fsync, leaving an entry visible but not durable.
            File::open(&ancestor)?.sync_all()?;
            if let Some(parent) = ancestor.parent() {
                File::open(parent)?.sync_all()?;
            }
        }
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        // Stable across changed source/settings: such retries must fail identity validation.
        let scope = serde_json::to_vec(&serde_json::json!({
            "vtl": identity.vtl, "destination": identity.destination,
            "barcode": identity.manifest.barcode, "generation": identity.manifest.generation,
        }))?;
        let name = sha256_hex(&scope);
        let path = directory.join(format!("{name}.json"));
        let lock_path = directory.join(format!("{name}.lock"));
        reject_symlinks(&lock_path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lock_path)?;
        lock.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        lock.try_lock()
            .map_err(|_| "another export owns this generation's upload journal")?;
        reject_symlinks(&path)?;
        let state = match File::open(&path) {
            Ok(file) => {
                let state: Journal = serde_json::from_reader(file.take(64 * 1024 * 1024))?;
                if state.version != 1 || state.identity != *identity {
                    return Err(
                        "upload journal source, destination or settings changed; refusing resume"
                            .into(),
                    );
                }
                state
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Journal {
                version: 1,
                identity: identity.clone(),
                transfers: BTreeMap::new(),
            },
            Err(e) => return Err(e.into()),
        };
        let handle = Self {
            directory,
            path,
            _lock: lock,
        };
        handle.save(&state)?;
        Ok((handle, state))
    }
    fn save(&self, state: &Journal) -> Result<()> {
        let temporary = self.path.with_extension("tmp");
        // A previous crash can leave only our private temporary file.
        reject_symlinks(&temporary)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        serde_json::to_writer(&mut file, state)?;
        file.sync_all()?;
        std::fs::rename(temporary, &self.path)?;
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
    fn remove(&self) -> Result<()> {
        std::fs::remove_file(&self.path)?;
        File::open(&self.directory)?.sync_all()?;
        // Keep the lock inode: unlinking it allows a second process to lock a new inode.
        Ok(())
    }
}

fn geometry(segment_mib: usize, part_mib: u32) -> Result<(u64, u64)> {
    if !(16..=1048576).contains(&segment_mib) || !(16..=1024).contains(&part_mib) {
        return Err(
            "segment size must be 16..1048576 MiB and multipart part size 16..1024 MiB".into(),
        );
    }
    if (segment_mib as u64).div_ceil(part_mib as u64) > 10000 {
        return Err("segment requires more than 10000 multipart parts".into());
    }
    Ok((
        segment_mib as u64 * 1024 * 1024,
        part_mib as u64 * 1024 * 1024,
    ))
}
fn default_state_directory() -> PathBuf {
    std::env::var_os("COLD_VTL_DATABASE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/cold-vtl/catalog.db"))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("uploads")
}

#[allow(clippy::too_many_arguments)]
pub async fn export_directory(
    store: &S3Store,
    barcode: Barcode,
    generation: u64,
    source: &Path,
    segment_size_mib: usize,
    multipart_part_size_mib: u32,
    state_directory: Option<&Path>,
    deep_verify: bool,
) -> Result<Manifest> {
    let (segment_size, part_size) = geometry(segment_size_mib, multipart_part_size_mib)?;
    let history = crate::upload_history::Recorder::from_env(barcode.as_str(), generation).await;
    let directory = state_directory
        .map(Path::to_path_buf)
        .unwrap_or_else(default_state_directory);
    let path = crate::stream_upload::journal_path(store, &barcode, generation, &directory)?;
    reject_symlinks(&path)?;
    let legacy = if path.exists() {
        let value: serde_json::Value =
            serde_json::from_reader(File::open(&path)?.take(64 * 1024 * 1024))?;
        value["version"] == 1
    } else {
        false
    };
    let result = if legacy {
        export_ranges(
            store,
            barcode,
            generation,
            source,
            segment_size,
            part_size,
            &directory,
            deep_verify,
            history.as_ref(),
        )
        .await
    } else {
        crate::stream_upload::export(
            store,
            barcode,
            generation,
            source,
            segment_size,
            part_size,
            &directory,
            history.as_ref(),
        )
        .await
    };
    if let Some(history) = history {
        history.set_active(false);
        history.close().await;
    }
    result
}

fn prepare(
    store: &S3Store,
    barcode: Barcode,
    generation: u64,
    source: &Path,
    segment_size: u64,
) -> Result<Manifest> {
    let mut manifest = Manifest {
        format_version: 2,
        barcode,
        generation,
        logical_size_bytes: 0,
        files: Vec::new(),
        segments: Vec::new(),
    };
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    for path in cartridge_files(source)? {
        let relative = path
            .strip_prefix(source)?
            .to_str()
            .ok_or("non-UTF8 cartridge path")?
            .to_owned();
        safe_relative_path(&relative)?;
        let mut file = File::open(&path)?;
        let mut file_hash = Sha256::new();
        let mut offset = 0;
        loop {
            let mut size = 0;
            let mut hash = Sha256::new();
            while size < segment_size {
                let limit = (segment_size - size).min(buffer.len() as u64) as usize;
                let read = file.read(&mut buffer[..limit])?;
                if read == 0 {
                    break;
                }
                hash.update(&buffer[..read]);
                file_hash.update(&buffer[..read]);
                size += read as u64;
            }
            if size == 0 {
                break;
            }
            let checksum = hex::encode(hash.finalize());
            let sequence = manifest.segments.len() as u64;
            let key = store.config().object_key(&format!(
                "cartridges/{}/generations/{generation}/segments/{sequence:08}-{checksum}.bin",
                manifest.barcode
            ));
            manifest.segments.push(Segment {
                sequence,
                source_path: relative.clone(),
                source_offset_bytes: offset,
                size_bytes: size,
                checksum_sha256: checksum,
                object_key: key,
                s3_checksum: None,
            });
            offset += size;
            manifest.logical_size_bytes += size;
        }
        manifest.files.push(CartridgeFile {
            path: relative,
            size_bytes: offset,
            checksum_sha256: hex::encode(file_hash.finalize()),
        });
    }
    validate_identity(&manifest, store.config(), &manifest.barcode, generation)?;
    Ok(manifest)
}

#[allow(clippy::too_many_arguments)]
async fn export_ranges(
    store: &S3Store,
    barcode: Barcode,
    generation: u64,
    source: &Path,
    segment_size: u64,
    part_size: u64,
    state_directory: &Path,
    deep_verify: bool,
    history: Option<&crate::upload_history::Recorder>,
) -> Result<Manifest> {
    if generation == 0
        || segment_size == 0
        || part_size == 0
        || segment_size.div_ceil(part_size) > 10000
    {
        return Err("invalid export geometry or generation".into());
    }
    reject_symlinks(source)?;
    let source = source.canonicalize()?;
    let _media_guard = cold_vtl_core::MediaReadGuard::acquire(&source)?;
    // Own protection independently of the API, including after its process dies.
    let manifest = prepare(store, barcode, generation, &source, segment_size)?;
    if let Some(history) = history {
        history.set_total(manifest.logical_size_bytes);
    }
    if fingerprint(&source)? != manifest.files {
        return Err("source changed during export preparation".into());
    }
    let config = store.config();
    let identity = Identity {
        endpoint: config.endpoint.clone(),
        region: config.region.clone(),
        bucket: config.bucket.clone(),
        prefix: config.prefix.clone(),
        vtl: std::env::var("COLD_VTL_VTL_ID").ok(),
        destination: std::env::var("COLD_VTL_DESTINATION_ID").ok(),
        revision: std::env::var("COLD_VTL_CONFIGURATION_REVISION").ok(),
        source: source.clone(),
        segment_size,
        part_size,
        manifest: manifest.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err("manifest exceeds 16 MiB".into());
    }
    let (journal_file, mut journal) = JournalFile::open(state_directory, &identity)?;
    storage::require_access(store, true).await?;
    store.check_bucket().await?;
    let key = manifest_key(config, &manifest.barcode, generation);
    if store.object_exists(&key).await? {
        if read_manifest_bytes(store, &key).await? != bytes {
            return Err("manifest already exists with different content".into());
        }
        if let Some(history) = history {
            history.phase("verifying");
        }
        verify_manifest(store, &manifest, true).await?;
        if let Some(history) = history {
            history.reuse(manifest.logical_size_bytes);
            history.phase("finalizing");
        }
        if fingerprint(&source)? != manifest.files {
            return Err("source changed during export".into());
        }
        journal_file.remove()?;
        return Ok(manifest);
    }
    for segment in &manifest.segments {
        transfer(
            store,
            &source,
            segment,
            part_size,
            &journal_file,
            &mut journal,
            deep_verify,
            history,
        )
        .await?;
    }
    if let Some(history) = history {
        history.phase("finalizing");
    }
    if fingerprint(&source)? != manifest.files {
        return Err("source changed during export; manifest was not published".into());
    }
    let checksum = sha256_hex(&bytes);
    // Even a lost PutObject reply is recoverable through the exact-byte comparison above.
    store
        .put_bytes(&key, bytes.clone(), "application/json", &checksum)
        .await?;
    store
        .verify_head(&key, bytes.len() as u64, &checksum)
        .await?;
    if read_manifest_bytes(store, &key).await? != bytes {
        return Err("published manifest differs".into());
    }
    journal_file.remove()?;
    Ok(manifest)
}

#[allow(clippy::too_many_arguments)]
async fn transfer(
    store: &S3Store,
    source: &Path,
    segment: &Segment,
    part_size: u64,
    file: &JournalFile,
    journal: &mut Journal,
    deep: bool,
    history: Option<&crate::upload_history::Recorder>,
) -> Result<()> {
    let key = &segment.object_key;
    let path = source.join(safe_relative_path(&segment.source_path)?);
    reject_symlinks(&path)?;
    journal.transfers.entry(key.clone()).or_default();
    let state = &journal.transfers[key];
    // Always verify a recovered object, including a lost CompleteMultipartUpload response.
    if state.verified || state.complete_intent || segment.size_bytes <= part_size {
        if store.object_exists(key).await? {
            if let Some(history) = history {
                history.phase("verifying");
            }
            store
                .verify_head(key, segment.size_bytes, &segment.checksum_sha256)
                .await?;
            verify_downloaded_segment(store, key, segment.size_bytes, &segment.checksum_sha256)
                .await?;
            if let Some(history) = history {
                history.reuse(segment.size_bytes);
            }
            journal.transfers.get_mut(key).unwrap().mark_verified();
            file.save(journal)?;
            return Ok(());
        }
        if state.verified {
            return Err("previously verified object disappeared".into());
        }
    }
    if let Some(history) = history {
        history.phase("uploading");
        history.set_active(true);
    }
    if segment.size_bytes <= part_size {
        store
            .put_file_range(
                key,
                &path,
                segment.source_offset_bytes,
                segment.size_bytes,
                &segment.checksum_sha256,
            )
            .await?;
        if let Some(history) = history {
            history.acknowledge(segment.size_bytes);
        }
    } else {
        let state = &journal.transfers[key];
        if state.upload_id.is_none() {
            if state.create_intent {
                return Err("ambiguous CreateMultipartUpload: inspect the owned upload before recovery; refusing to create an orphan".into());
            }
            journal.transfers.get_mut(key).unwrap().create_intent = true;
            file.save(journal)?;
            let upload_id = store
                .create_multipart(key, &segment.checksum_sha256)
                .await?;
            journal.transfers.get_mut(key).unwrap().upload_id = Some(upload_id);
            file.save(journal)?;
        }
        let upload_id = journal.transfers[key].upload_id.clone().unwrap();
        let remote = store.list_multipart_parts(key, &upload_id).await?;
        let count = segment.size_bytes.div_ceil(part_size) as i32;
        let mut remote_parts = BTreeMap::new();
        for part in remote {
            if part.part_number < 1
                || part.part_number > count
                || remote_parts.insert(part.part_number, part).is_some()
            {
                return Err("unexpected remote multipart part".into());
            }
        }
        for (number, confirmed) in &journal.transfers[key].parts {
            if *number < 1 || *number > count || confirmed.part_number != *number {
                return Err("invalid confirmed multipart part number".into());
            }
            let expected_size =
                (segment.size_bytes - (*number as u64 - 1) * part_size).min(part_size);
            let remote = remote_parts
                .get(number)
                .ok_or("confirmed multipart part disappeared")?;
            if remote.etag != confirmed.etag
                || remote.size != confirmed.size
                || remote.size != expected_size
            {
                return Err("confirmed multipart part differs remotely".into());
            }
        }
        if let Some(history) = history {
            history.reuse(
                journal.transfers[key]
                    .parts
                    .values()
                    .map(|part| part.size)
                    .sum(),
            );
        }
        let mut pending = (1..=count)
            .filter(|number| !journal.transfers[key].parts.contains_key(number))
            .collect::<Vec<_>>()
            .into_iter();
        let mut tasks = tokio::task::JoinSet::new();
        let mut failure: Option<Box<dyn Error>> = None;
        loop {
            while failure.is_none() && tasks.len() < UPLOAD_CONCURRENCY {
                let Some(number) = pending.next() else {
                    break;
                };
                let store = store.clone();
                let path = path.clone();
                let key = key.clone();
                let id = upload_id.clone();
                let relative = (number as u64 - 1) * part_size;
                let length = (segment.size_bytes - relative).min(part_size);
                let offset = segment.source_offset_bytes + relative;
                // Unknown remote parts (e.g. lost responses) are safely overwritten at
                // the same number, never trusted without a locally confirmed ETag.
                tasks.spawn(async move {
                    store
                        .upload_part_file(&key, &id, number, &path, offset, length)
                        .await
                });
            }
            let Some(result) = tasks.join_next().await else {
                break;
            };
            match result {
                Ok(Ok(part)) => {
                    if let Some(history) = history {
                        history.acknowledge(part.size);
                    }
                    journal
                        .transfers
                        .get_mut(key)
                        .unwrap()
                        .parts
                        .insert(part.part_number, part);
                    if let Err(error) = file.save(journal) {
                        failure = Some(error);
                    }
                }
                Ok(Err(error)) => {
                    failure = Some(error.into());
                }
                Err(error) => {
                    failure = Some(error.into());
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        journal.transfers.get_mut(key).unwrap().complete_intent = true;
        file.save(journal)?;
        let parts = journal.transfers[key]
            .parts
            .values()
            .cloned()
            .collect::<Vec<_>>();
        store.complete_multipart(key, &upload_id, &parts).await?;
    }
    if let Some(history) = history {
        history.set_active(false);
        history.phase("verifying");
    }
    store
        .verify_head(key, segment.size_bytes, &segment.checksum_sha256)
        .await?;
    if deep {
        verify_downloaded_segment(store, key, segment.size_bytes, &segment.checksum_sha256).await?;
    }
    journal.transfers.get_mut(key).unwrap().mark_verified();
    file.save(journal)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, Bytes},
        extract::State,
        http::{Request, Response, StatusCode},
    };
    use base64::{Engine, engine::general_purpose::STANDARD};
    use std::sync::{Arc, Mutex};

    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            static ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "worker-multipart-{}-{}",
                std::process::id(),
                ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            Self { root }
        }
        fn source(&self) -> PathBuf {
            let source = self.root.join("source");
            std::fs::create_dir(&source).unwrap();
            std::fs::write(source.join("data.0"), b"abcdefghijklmno").unwrap();
            std::fs::write(source.join("empty"), b"").unwrap();
            source
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[derive(Default)]
    struct Fake {
        objects: BTreeMap<String, (Vec<u8>, String)>,
        parts: BTreeMap<i32, Vec<u8>>,
        checksum: String,
        native: bool,
        native_objects: BTreeMap<String, String>,
        uploads: usize,
        attempts: BTreeMap<i32, usize>,
        fail_part: bool,
        lose_complete: bool,
        refuse_complete: bool,
        truncate_complete: bool,
        lose_create: bool,
        lose_part_once: bool,
        lose_manifest_once: bool,
        mutate_source: Option<PathBuf>,
        active: usize,
        max_active: usize,
        payload_gets: usize,
        omit_native_checksum: bool,
    }
    type Shared = Arc<Mutex<Fake>>;
    fn response(code: StatusCode, body: impl Into<Body>) -> Response<Body> {
        Response::builder()
            .status(code)
            .header("content-type", "application/xml")
            .body(body.into())
            .unwrap()
    }
    async fn serve(State(shared): State<Shared>, request: Request<Body>) -> Response<Body> {
        let (head, body) = request.into_parts();
        let is_part = head.uri.query().is_some_and(|q| q.contains("partNumber"));
        if is_part {
            {
                let mut state = shared.lock().unwrap();
                state.active += 1;
                state.max_active = state.max_active.max(state.active);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let raw = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();
        let data = if head
            .headers
            .get("content-encoding")
            .is_some_and(|v| v.to_str().unwrap().contains("aws-chunked"))
        {
            let mut remaining = raw.as_ref();
            let mut decoded = Vec::new();
            loop {
                let end = remaining.windows(2).position(|p| p == b"\r\n").unwrap();
                let length = usize::from_str_radix(
                    std::str::from_utf8(&remaining[..end])
                        .unwrap()
                        .split(';')
                        .next()
                        .unwrap(),
                    16,
                )
                .unwrap();
                remaining = &remaining[end + 2..];
                if length == 0 {
                    break;
                }
                decoded.extend_from_slice(&remaining[..length]);
                remaining = &remaining[length + 2..];
            }
            Bytes::from(decoded)
        } else {
            raw
        };
        let key = head.uri.path().to_owned();
        let query = head.uri.query().unwrap_or("");
        let mut state = shared.lock().unwrap();
        if is_part {
            state.active -= 1;
        }
        match head.method.as_str() {
            "HEAD" => {
                if key == "/test-bucket" || key == "/test-bucket/" {
                    return response(StatusCode::OK, "");
                }
                if let Some(checksum) = state.native_objects.get(&key) {
                    return Response::builder()
                        .status(200)
                        .header("content-length", state.objects[&key].0.len())
                        .header("x-amz-checksum-sha256", checksum)
                        .header(
                            "x-amz-checksum-type",
                            if checksum.contains('-') {
                                "COMPOSITE"
                            } else {
                                "FULL_OBJECT"
                            },
                        )
                        .body(Body::empty())
                        .unwrap();
                }
                match state.objects.get(&key) {
                    Some((bytes, checksum)) => Response::builder()
                        .status(200)
                        .header("content-length", bytes.len())
                        .header("x-amz-meta-cold-vtl-sha256", checksum)
                        .body(Body::empty())
                        .unwrap(),
                    None => response(StatusCode::NOT_FOUND, ""),
                }
            }
            "POST" if query.contains("uploads") => {
                state.uploads += 1;
                state.native = head.headers.contains_key("x-amz-checksum-algorithm");
                state.checksum = head
                    .headers
                    .get("x-amz-meta-cold-vtl-sha256")
                    .map(|h| h.to_str().unwrap().to_owned())
                    .unwrap_or_default();
                if state.lose_create {
                    return response(
                        StatusCode::BAD_REQUEST,
                        "<Error><Code>RequestTimeout</Code></Error>",
                    );
                }
                response(
                    StatusCode::OK,
                    "<InitiateMultipartUploadResult><UploadId>owned-upload</UploadId></InitiateMultipartUploadResult>",
                )
            }
            "PUT" if query.contains("partNumber") => {
                let number: i32 = query
                    .split('&')
                    .find_map(|s| s.strip_prefix("partNumber="))
                    .unwrap()
                    .parse()
                    .unwrap();
                *state.attempts.entry(number).or_default() += 1;
                if state.fail_part && number == 2 {
                    return response(
                        StatusCode::BAD_REQUEST,
                        "<Error><Code>InvalidRequest</Code></Error>",
                    );
                }
                if state.native
                    && head
                        .headers
                        .get("x-amz-checksum-sha256")
                        .and_then(|h| h.to_str().ok())
                        != Some(STANDARD.encode(Sha256::digest(&data)).as_str())
                {
                    return response(
                        StatusCode::BAD_REQUEST,
                        "<Error><Code>BadDigest</Code></Error>",
                    );
                }
                state.parts.insert(number, data.to_vec());
                if state.lose_part_once && number == 2 {
                    state.lose_part_once = false;
                    return response(
                        StatusCode::BAD_REQUEST,
                        "<Error><Code>InvalidRequest</Code></Error>",
                    );
                }
                let mut builder = Response::builder()
                    .status(200)
                    .header("etag", format!("\"part-{number}\""));
                if state.native && !state.omit_native_checksum {
                    builder = builder.header(
                        "x-amz-checksum-sha256",
                        STANDARD.encode(Sha256::digest(&data)),
                    );
                }
                builder.body(Body::empty()).unwrap()
            }
            "GET" if query.contains("uploadId") => {
                let mut body = "<ListPartsResult><IsTruncated>false</IsTruncated>".to_owned();
                for (number, part) in &state.parts {
                    body.push_str(&format!("<Part><PartNumber>{number}</PartNumber><ETag>\"part-{number}\"</ETag><Size>{}</Size></Part>", part.len()));
                    if state.native {
                        let suffix = "</Part>";
                        body.truncate(body.len() - suffix.len());
                        body.push_str(&format!(
                            "<ChecksumSHA256>{}</ChecksumSHA256></Part>",
                            STANDARD.encode(Sha256::digest(part))
                        ));
                    }
                }
                body.push_str("</ListPartsResult>");
                response(StatusCode::OK, body)
            }
            "POST" if query.contains("uploadId") => {
                if state.refuse_complete {
                    state.refuse_complete = false;
                    return response(
                        StatusCode::BAD_REQUEST,
                        "<Error><Code>InvalidPart</Code><Message>part missing</Message></Error>",
                    );
                }
                let bytes = state.parts.values().flatten().copied().collect();
                let checksum = state.checksum.clone();
                let native_checksum = if state.native {
                    let mut hash = Sha256::new();
                    for part in state.parts.values() {
                        hash.update(Sha256::digest(part));
                    }
                    Some(format!(
                        "{}-{}",
                        STANDARD.encode(hash.finalize()),
                        state.parts.len()
                    ))
                } else {
                    None
                };
                if let Some(c) = &native_checksum {
                    state.native_objects.insert(key.clone(), c.clone());
                }
                state.objects.insert(key, (bytes, checksum));
                state.parts.clear();
                if let Some(path) = state.mutate_source.take() {
                    std::fs::write(path, b"changed during upload").unwrap();
                }
                if state.lose_complete {
                    state.lose_complete = false;
                    return response(
                        StatusCode::BAD_REQUEST,
                        "<Error><Code>RequestTimeout</Code></Error>",
                    );
                }
                if state.truncate_complete {
                    state.truncate_complete = false;
                    return response(StatusCode::OK, "<CompleteMultipartUploadResult><ETag>");
                }
                response(
                    StatusCode::OK,
                    format!(
                        "<CompleteMultipartUploadResult><ETag>\"completed\"</ETag>{}</CompleteMultipartUploadResult>",
                        native_checksum
                            .map(|c| format!("<ChecksumSHA256>{c}</ChecksumSHA256>"))
                            .unwrap_or_default()
                    ),
                )
            }
            "PUT" => {
                if let Some(expected) = head.headers.get("x-amz-checksum-sha256") {
                    let checksum = STANDARD.encode(Sha256::digest(&data));
                    if expected.to_str().unwrap() != checksum {
                        return response(
                            StatusCode::BAD_REQUEST,
                            "<Error><Code>BadDigest</Code></Error>",
                        );
                    }
                    state.native_objects.insert(key.clone(), checksum.clone());
                    state
                        .objects
                        .insert(key.clone(), (data.to_vec(), String::new()));
                    if key.contains("/manifests/") && state.lose_manifest_once {
                        state.lose_manifest_once = false;
                        return response(
                            StatusCode::BAD_REQUEST,
                            "<Error><Code>InvalidRequest</Code></Error>",
                        );
                    }
                    return Response::builder()
                        .status(200)
                        .header("x-amz-checksum-sha256", checksum)
                        .body(Body::empty())
                        .unwrap();
                }
                let checksum = head.headers["x-amz-meta-cold-vtl-sha256"]
                    .to_str()
                    .unwrap()
                    .to_owned();
                state.objects.insert(key.clone(), (data.to_vec(), checksum));
                if key.contains("/manifests/") && state.lose_manifest_once {
                    state.lose_manifest_once = false;
                    return response(
                        StatusCode::BAD_REQUEST,
                        "<Error><Code>InvalidRequest</Code></Error>",
                    );
                }
                response(StatusCode::OK, "")
            }
            "GET" => {
                if key.contains("/segments/") {
                    state.payload_gets += 1;
                }
                match state.objects.get(&key) {
                    Some((bytes, _)) => response(StatusCode::OK, Bytes::copy_from_slice(bytes)),
                    None => response(
                        StatusCode::NOT_FOUND,
                        "<Error><Code>NoSuchKey</Code></Error>",
                    ),
                }
            }
            _ => response(StatusCode::BAD_REQUEST, ""),
        }
    }
    async fn fake() -> (S3Store, Shared, tokio::task::JoinHandle<()>) {
        let state = Arc::new(Mutex::new(Fake::default()));
        let app = Router::new().fallback(serve).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config: S3Config = serde_json::from_value(serde_json::json!({"provider":"s3_compatible", "endpoint":endpoint,"region":"local", "bucket":"test-bucket","prefix":"vtl","access_key_id":"test","secret_access_key":"test"})).unwrap();
        (S3Store::connect(config).await, state, task)
    }
    async fn run(store: &S3Store, source: &Path, state: &Path) -> Result<Manifest> {
        export_ranges(
            store,
            Barcode::parse("TAPE01")?,
            1,
            source,
            1024,
            4,
            state,
            true,
            None,
        )
        .await
    }
    async fn stream(store: &S3Store, source: &Path, state: &Path) -> Result<Manifest> {
        crate::stream_upload::export(
            store,
            Barcode::parse("TAPE01")?,
            1,
            source,
            1024,
            4,
            state,
            None,
        )
        .await
    }
    #[tokio::test]
    async fn streaming_roundtrip_without_payload_readback_and_resumes_parts() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().fail_part = true;
        assert!(stream(&store, &source, &state).await.is_err());
        fake.lock().unwrap().fail_part = false;
        let manifest = stream(&store, &source, &state).await.unwrap();
        assert_eq!(manifest.format_version, 3);
        assert_eq!(fake.lock().unwrap().attempts[&1], 1);
        assert_eq!(fake.lock().unwrap().payload_gets, 0);
        assert_eq!(manifest.files, fingerprint(&source).unwrap());
        let restored = fixture.root.join("restored");
        restore_directory(&store, &manifest, &restored, None)
            .await
            .unwrap();
        assert_eq!(fingerprint(&restored).unwrap(), manifest.files);
        server.abort();
    }
    #[tokio::test]
    async fn streaming_lost_completion_and_changed_source_are_safe() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        // The object assembled by S3 resolves a lost response without a resume.
        fake.lock().unwrap().lose_complete = true;
        let manifest = stream(&store, &source, &state).await.unwrap();
        assert_eq!(fake.lock().unwrap().uploads, 1);
        assert_eq!(manifest.files, fingerprint(&source).unwrap());
        std::fs::write(source.join("data.0"), b"modified source").unwrap();
        assert!(
            stream(&store, &source, &state)
                .await
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
        server.abort();
    }
    #[tokio::test]
    async fn streaming_resolves_an_unreadable_completion_by_head() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().truncate_complete = true;
        let manifest = stream(&store, &source, &state).await.unwrap();
        assert_eq!(fake.lock().unwrap().uploads, 1);
        assert_eq!(fake.lock().unwrap().payload_gets, 0);
        assert_eq!(manifest.files, fingerprint(&source).unwrap());
        server.abort();
    }
    #[tokio::test]
    async fn archive_check_trusts_native_checksums_and_reads_legacy_copies() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let (store, shared, server) = fake().await;
        let native = stream(&store, &source, &fixture.root.join("native"))
            .await
            .unwrap();
        crate::storage::verify_copy(&store, &native).await.unwrap();
        assert_eq!(shared.lock().unwrap().payload_gets, 0);
        let segment = native.segments[0].object_key.clone();
        shared
            .lock()
            .unwrap()
            .native_objects
            .insert(format!("/test-bucket/{segment}"), "AAAA-1".into());
        assert!(crate::storage::verify_copy(&store, &native).await.is_err());
        server.abort();

        let (store, shared, server) = fake().await;
        let legacy = run(&store, &source, &fixture.root.join("legacy"))
            .await
            .unwrap();
        crate::storage::verify_copy(&store, &legacy).await.unwrap();
        assert!(shared.lock().unwrap().payload_gets > 0);
        let key = format!("/test-bucket/{}", legacy.segments[0].object_key);
        shared.lock().unwrap().objects.get_mut(&key).unwrap().0[0] ^= 1;
        assert!(crate::storage::verify_copy(&store, &legacy).await.is_err());
        server.abort();
    }
    #[tokio::test]
    async fn streaming_reports_a_refused_completion_then_resumes_it() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().refuse_complete = true;
        let error = stream(&store, &source, &state)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("InvalidPart"), "{error}");
        let manifest = stream(&store, &source, &state).await.unwrap();
        assert_eq!(fake.lock().unwrap().uploads, 1);
        assert_eq!(manifest.files, fingerprint(&source).unwrap());
        server.abort();
    }
    #[tokio::test]
    async fn streaming_rejects_unsupported_checksum_without_manifest() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().omit_native_checksum = true;
        assert!(stream(&store, &source, &state).await.is_err());
        assert!(
            !fake
                .lock()
                .unwrap()
                .objects
                .keys()
                .any(|key| key.contains("manifests/"))
        );
        server.abort();
    }
    #[tokio::test]
    async fn streaming_starts_on_a_sparse_tib_without_scanning_it() {
        let fixture = Fixture::new();
        let source = fixture.source();
        OpenOptions::new()
            .write(true)
            .open(source.join("data.0"))
            .unwrap()
            .set_len(1024u64.pow(4))
            .unwrap();
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().fail_part = true;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream(&store, &source, &fixture.root.join("uploads")),
        )
        .await;
        assert!(result.unwrap().is_err());
        assert_eq!(fake.lock().unwrap().parts[&1], vec![0x61, 0x62, 0x63, 0x64]);
        server.abort();
    }
    #[tokio::test]
    async fn streaming_rejects_source_changed_during_send_and_ambiguous_create() {
        for mutate in [true, false] {
            let fixture = Fixture::new();
            let source = fixture.source();
            let state = fixture.root.join("uploads");
            let (store, fake, server) = fake().await;
            if mutate {
                fake.lock().unwrap().mutate_source = Some(source.join("data.0"));
            } else {
                fake.lock().unwrap().lose_create = true;
            }
            assert!(stream(&store, &source, &state).await.is_err());
            assert!(
                !fake
                    .lock()
                    .unwrap()
                    .objects
                    .keys()
                    .any(|k| k.contains("/manifests/"))
            );
            if !mutate {
                assert!(
                    stream(&store, &source, &state)
                        .await
                        .unwrap_err()
                        .to_string()
                        .contains("ambiguous")
                );
                assert_eq!(fake.lock().unwrap().uploads, 1);
            }
            server.abort();
        }
    }
    #[tokio::test]
    async fn streaming_multisegment_and_completed_retry_restore_identically() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        let manifest = crate::stream_upload::export(
            &store,
            Barcode::parse("TAPE01").unwrap(),
            1,
            &source,
            8,
            4,
            &state,
            None,
        )
        .await
        .unwrap();
        assert_eq!(manifest.segments.len(), 2);
        let again = crate::stream_upload::export(
            &store,
            Barcode::parse("TAPE01").unwrap(),
            1,
            &source,
            8,
            4,
            &state,
            None,
        )
        .await
        .unwrap();
        assert_eq!(manifest, again);
        assert_eq!(fake.lock().unwrap().uploads, 2);
        assert_eq!(fake.lock().unwrap().payload_gets, 0);
        verify_manifest(&store, &manifest, false).await.unwrap();
        let restored = fixture.root.join("restored");
        restore_directory(&store, &manifest, &restored, None)
            .await
            .unwrap();
        assert_eq!(fingerprint(&restored).unwrap(), manifest.files);
        server.abort();
    }
    #[tokio::test]
    async fn streaming_recovers_lost_manifest_receipt_without_reupload() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().lose_manifest_once = true;
        assert!(stream(&store, &source, &state).await.is_err());
        let manifest = stream(&store, &source, &state).await.unwrap();
        assert_eq!(manifest.format_version, 3);
        assert_eq!(fake.lock().unwrap().uploads, 1);
        assert_eq!(fake.lock().unwrap().payload_gets, 0);
        server.abort();
    }
    #[tokio::test]
    async fn legacy_journal_resumes_through_public_export_entrypoint() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().lose_manifest_once = true;
        assert!(
            export_ranges(
                &store,
                Barcode::parse("TAPE01").unwrap(),
                1,
                &source,
                16 * 1024 * 1024,
                16 * 1024 * 1024,
                &state,
                true,
                None
            )
            .await
            .is_err()
        );
        let manifest = export_directory(
            &store,
            Barcode::parse("TAPE01").unwrap(),
            1,
            &source,
            16,
            16,
            Some(&state),
            true,
        )
        .await
        .unwrap();
        assert_eq!(manifest.format_version, 2);
        server.abort();
    }
    #[tokio::test]
    async fn loaded_source_is_rejected_before_any_s3_object_is_sent() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let drive = File::open(&source).unwrap();
        drive.try_lock().unwrap();
        let (store, fake, server) = fake().await;
        let result = run(&store, &source, &fixture.root.join("uploads")).await;
        assert!(result.is_err(), "loaded tape was exported");
        assert!(fake.lock().unwrap().objects.is_empty());
        drop(drive);
        run(&store, &source, &fixture.root.join("uploads"))
            .await
            .unwrap();
        assert!(!fake.lock().unwrap().objects.is_empty());
        server.abort();
    }
    #[tokio::test]
    async fn failed_upload_keeps_real_receipts_but_recovered_export_adds_no_rates() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state_dir = fixture.root.join("uploads");
        let database = fixture.root.join("history.db");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&database)
            .create_if_missing(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE vtls(id TEXT PRIMARY KEY); CREATE TABLE jobs(id TEXT PRIMARY KEY,vtl_id TEXT,subject TEXT,kind TEXT,status TEXT,generation INTEGER,progress INTEGER DEFAULT 10,message TEXT,updated_at TEXT,UNIQUE(vtl_id,id)); INSERT INTO vtls VALUES('vtl-a'); INSERT INTO jobs(id,vtl_id,subject,kind,status,generation) VALUES('job-a','vtl-a','TAPE01','export','running',1);").execute(&pool).await.unwrap();
        sqlx::raw_sql(include_str!(
            "../../cold-vtl-api/migrations/0016_s3_transfer_history.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().fail_part = true;
        let recorder =
            crate::upload_history::Recorder::open(&database, "vtl-a", "job-a", "TAPE01", 1)
                .await
                .unwrap();
        let result = export_ranges(
            &store,
            Barcode::parse("TAPE01").unwrap(),
            1,
            &source,
            1024,
            4,
            &state_dir,
            true,
            Some(&recorder),
        )
        .await;
        recorder.set_active(false);
        recorder.close().await;
        assert!(result.is_err());
        let recorded: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM s3_transfer_samples WHERE write_rate_bps>0")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            recorded > 0,
            "successful multipart receipts survive a later failed part"
        );
        fake.lock().unwrap().fail_part = false;
        run(&store, &source, &state_dir).await.unwrap();
        sqlx::query("DELETE FROM s3_transfer_samples")
            .execute(&pool)
            .await
            .unwrap();
        let recorder =
            crate::upload_history::Recorder::open(&database, "vtl-a", "job-a", "TAPE01", 1)
                .await
                .unwrap();
        export_ranges(
            &store,
            Barcode::parse("TAPE01").unwrap(),
            1,
            &source,
            1024,
            4,
            &state_dir,
            true,
            Some(&recorder),
        )
        .await
        .unwrap();
        recorder.set_active(false);
        recorder.close().await;
        let recorded: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM s3_transfer_samples WHERE write_rate_bps IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            recorded, 0,
            "recovering an existing manifest uploads no fresh payload"
        );
        pool.close().await;
        server.abort();
    }
    #[test]
    fn geometry_supports_a_tib_without_segment_sized_allocation() {
        assert_eq!(geometry(1048576, 256).unwrap(), (1 << 40, 256 << 20));
        assert!(geometry(1048576, 16).is_err());
        assert!(geometry(15, 256).is_err());
        assert!(geometry(1048577, 256).is_err());
        assert!(geometry(16, 1025).is_err());
        assert_eq!((1_u64 << 40).div_ceil(256 << 20), 4096);
    }
    #[tokio::test]
    async fn failed_upload_reuses_confirmed_parts_then_restores_exact_bytes() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state_dir = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().fail_part = true;
        assert!(run(&store, &source, &state_dir).await.is_err());
        {
            let mut state = fake.lock().unwrap();
            assert!(state.objects.is_empty());
            assert_eq!(state.uploads, 1);
            assert_eq!(state.attempts.get(&1), Some(&1));
            state.fail_part = false;
        }
        let manifest = run(&store, &source, &state_dir).await.unwrap();
        {
            let state = fake.lock().unwrap();
            assert_eq!(state.uploads, 1);
            assert_eq!(state.attempts.get(&1), Some(&1));
            assert_eq!(state.attempts.get(&2), Some(&2));
        }
        let destination = fixture.root.join("restore");
        restore_directory(&store, &manifest, &destination, None)
            .await
            .unwrap();
        assert_eq!(
            fingerprint(&destination).unwrap(),
            fingerprint(&source).unwrap()
        );
        // Retry after journal cleanup / lost final process output is idempotent.
        assert_eq!(run(&store, &source, &state_dir).await.unwrap(), manifest);
        server.abort();
    }
    #[tokio::test]
    async fn complete_response_loss_recovers_by_streaming_final_object() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state_dir = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().lose_complete = true;
        assert!(run(&store, &source, &state_dir).await.is_err());
        assert_eq!(fake.lock().unwrap().objects.len(), 1);
        run(&store, &source, &state_dir).await.unwrap();
        assert_eq!(fake.lock().unwrap().uploads, 1);
        server.abort();
    }
    #[tokio::test]
    async fn lost_create_and_changed_source_fail_closed_without_new_upload() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state_dir = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().lose_create = true;
        assert!(run(&store, &source, &state_dir).await.is_err());
        assert!(
            run(&store, &source, &state_dir)
                .await
                .unwrap_err()
                .to_string()
                .contains("ambiguous")
        );
        assert_eq!(fake.lock().unwrap().uploads, 1);
        std::fs::write(source.join("data.0"), b"different source").unwrap();
        assert!(
            run(&store, &source, &state_dir)
                .await
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
        assert_eq!(fake.lock().unwrap().uploads, 1);
        server.abort();
    }
    #[tokio::test]
    async fn tiny_file_with_tib_geometry_and_symlink_rejection() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state_dir = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        let manifest = export_directory(
            &store,
            Barcode::parse("TAPE01").unwrap(),
            1,
            &source,
            1048576,
            256,
            Some(&state_dir),
            true,
        )
        .await
        .unwrap();
        assert_eq!(manifest.segments.len(), 1);
        assert_eq!(fake.lock().unwrap().uploads, 1);
        std::os::unix::fs::symlink(source.join("data.0"), source.join("link")).unwrap();
        assert!(fingerprint(&source).is_err());
        server.abort();
    }
    #[tokio::test]
    async fn unreceipted_remote_part_is_reuploaded_and_concurrency_is_bounded() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state_dir = fixture.root.join("uploads");
        std::fs::write(source.join("data.0"), [42_u8; 100]).unwrap();
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().lose_part_once = true;
        assert!(run(&store, &source, &state_dir).await.is_err());
        assert!(fake.lock().unwrap().parts.contains_key(&2));
        let manifest = run(&store, &source, &state_dir).await.unwrap();
        {
            let state = fake.lock().unwrap();
            assert_eq!(state.attempts[&2], 2);
            assert_eq!(state.attempts[&1], 1);
            assert!(state.max_active > 1);
            assert!(state.max_active <= UPLOAD_CONCURRENCY);
        }
        restore_directory(&store, &manifest, &fixture.root.join("restored"), None)
            .await
            .unwrap();
        server.abort();
    }
    #[tokio::test]
    async fn changed_destination_and_source_during_upload_never_publish_manifest() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state_dir = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().fail_part = true;
        assert!(run(&store, &source, &state_dir).await.is_err());
        let mut different = store.config().clone();
        different.bucket = "changed-bucket".into();
        let different = S3Store::connect(different).await;
        assert!(
            run(&different, &source, &state_dir)
                .await
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
        {
            let mut state = fake.lock().unwrap();
            state.fail_part = false;
            state.mutate_source = Some(source.join("data.0"));
        }
        assert!(
            run(&store, &source, &state_dir)
                .await
                .unwrap_err()
                .to_string()
                .contains("source changed")
        );
        assert!(
            fake.lock()
                .unwrap()
                .objects
                .keys()
                .all(|key| !key.ends_with(".json"))
        );
        let journal_path = std::fs::read_dir(&state_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .unwrap();
        let journal: Journal =
            serde_json::from_slice(&std::fs::read(journal_path).unwrap()).unwrap();
        assert!(journal.transfers.values().all(|transfer| transfer.verified
            && transfer.parts.is_empty()
            && transfer.upload_id.is_none()));
        server.abort();
    }
    #[tokio::test]
    async fn tampered_confirmed_parts_and_completed_objects_are_not_trusted() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state_dir = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().fail_part = true;
        assert!(run(&store, &source, &state_dir).await.is_err());
        {
            let mut state = fake.lock().unwrap();
            state.parts.insert(1, b"wrongsize".to_vec());
            state.fail_part = false;
        }
        assert!(
            run(&store, &source, &state_dir)
                .await
                .unwrap_err()
                .to_string()
                .contains("differs remotely")
        );
        {
            let mut state = fake.lock().unwrap();
            state.parts.insert(1, b"abcd".to_vec());
            state.lose_complete = true;
        }
        assert!(run(&store, &source, &state_dir).await.is_err());
        {
            let mut state = fake.lock().unwrap();
            state.objects.values_mut().next().unwrap().0[0] = b'!';
        }
        assert!(
            run(&store, &source, &state_dir)
                .await
                .unwrap_err()
                .to_string()
                .contains("verification failed")
        );
        let manifest =
            prepare(&store, Barcode::parse("TAPE01").unwrap(), 1, &source, 1024).unwrap();
        let destination = fixture.root.join("restore");
        assert!(
            restore_directory(&store, &manifest, &destination, None)
                .await
                .is_err()
        );
        assert!(destination.join(".cold-vtl-incomplete").exists());
        server.abort();
    }
    #[tokio::test]
    async fn journal_is_private_exclusive_and_outside_media() {
        let fixture = Fixture::new();
        let source = fixture.source();
        let state_dir = fixture.root.join("uploads");
        let (store, fake, server) = fake().await;
        fake.lock().unwrap().fail_part = true;
        assert!(run(&store, &source, &state_dir).await.is_err());
        let path = std::fs::read_dir(&state_dir)
            .unwrap()
            .map(|p| p.unwrap().path())
            .find(|p| p.extension().is_some_and(|s| s == "json"))
            .unwrap();
        let journal: Journal = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let (_held, _) = JournalFile::open(&state_dir, &journal.identity).unwrap();
        assert!(JournalFile::open(&state_dir, &journal.identity).is_err());
        assert!(JournalFile::open(&source.join("state"), &journal.identity).is_err());
        assert!(!source.join("state").exists());
        let nested = fixture.root.join("new-parent/child/uploads");
        let (_nested, _) = JournalFile::open(&nested, &journal.identity).unwrap();
        assert!(nested.is_dir());
        server.abort();
    }
}
