use clap::{Parser, Subcommand};
use cold_vtl_core::{
    Barcode, CartridgeFile, Manifest, ReconcileOutput, RemoteGeneration, Segment,
    safe_manifest_path,
};
use cold_vtl_provider_s3::{S3Config, S3Error, S3Store};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs::{File, OpenOptions},
    io::{BufReader, Read, Write},
    path::{Component, Path, PathBuf},
};
use walkdir::WalkDir;
mod delete_copy;
mod recovery_inventory;
mod storage;
mod stream_upload;
mod upload;
mod upload_history;

fn configured_bandwidth(
    root: &Path,
    id: Option<&str>,
) -> Result<Option<cold_vtl_provider_s3::bandwidth::VtlBandwidth>, Box<dyn Error>> {
    if !root.join("storage-isolated").exists() {
        return Ok(None);
    }
    let id = id.ok_or("A transfer on an isolated appliance requires a VTL identity")?;
    Ok(Some(cold_vtl_provider_s3::bandwidth::VtlBandwidth::open(
        root, id,
    )?))
}

async fn load_config() -> Result<S3Config, Box<dyn Error>> {
    let path = std::env::var_os("COLD_VTL_DATABASE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/cold-vtl/catalog.db"));
    let pin = [
        "COLD_VTL_VTL_ID",
        "COLD_VTL_DESTINATION_ID",
        "COLD_VTL_CONFIGURATION_REVISION",
    ]
    .map(std::env::var);
    if pin.iter().any(Result::is_ok) {
        let [vtl, destination, revision] = pin;
        return pinned_config(
            &path,
            &vtl.map_err(|_| "missing VTL pin")?,
            &destination.map_err(|_| "missing destination pin")?,
            revision
                .map_err(|_| "missing configuration revision pin")?
                .parse()?,
        )
        .await;
    }
    match saved_config(&path).await? {
        Some(config) => Ok(config),
        None => Ok(S3Config::from_env()?),
    }
}
async fn pinned_config(
    path: &Path,
    vtl: &str,
    destination: &str,
    revision: i64,
) -> Result<S3Config, Box<dyn Error>> {
    use sqlx::Connection;
    if revision <= 0 || !path.is_file() {
        return Err("Pinned S3 configuration is unavailable".into());
    }
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .read_only(true);
    let mut db = sqlx::SqliteConnection::connect_with(&options).await?;
    let value: Option<String> = sqlx::query_scalar("SELECT r.configuration FROM storage_destination_revisions r JOIN storage_destinations d ON d.id=r.destination_id WHERE d.vtl_id=? AND d.id=? AND r.revision=?")
        .bind(vtl).bind(destination).bind(revision).fetch_optional(&mut db).await?;
    serde_json::from_str(&value.ok_or("Pinned S3 configuration is missing or foreign")?)
        .map_err(|_| "Pinned S3 configuration is invalid".into())
}
async fn saved_config(path: &Path) -> Result<Option<S3Config>, Box<dyn Error>> {
    use sqlx::Connection;
    if path.exists() {
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .read_only(true);
        let mut db = sqlx::SqliteConnection::connect_with(&options).await?;
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='s3_settings'",
        )
        .fetch_one(&mut db)
        .await?;
        if exists > 0 {
            let saved: Option<String> =
                sqlx::query_scalar("SELECT configuration FROM s3_settings WHERE id=1")
                    .fetch_optional(&mut db)
                    .await?;
            if let Some(value) = saved {
                return serde_json::from_str(&value)
                    .map(Some)
                    .map_err(|_| "Invalid saved S3 configuration.".into());
            }
        }
    }
    Ok(None)
}

const DEFAULT_SEGMENT_SIZE_MIB: usize = 1048576;

#[derive(Parser)]
#[command(name = "cold-vtl-worker")]
#[command(about = "Export and verify sealed virtual cartridges")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    DiscoverBuckets,
    CreateBucketFromConnection {
        #[arg(long)]
        bucket: String,
    },
    /// Classify the connection credentials with one listing call.
    CheckCredentials,
    /// Report whether a bucket exists (`exists`, `absent` or `unknown`).
    BucketPresence {
        #[arg(long)]
        bucket: String,
    },
    /// Report whether a bucket is `empty` (no object, upload or version, and
    /// writable), `occupied`, or `unknown` when the provider cannot be read.
    ConfirmEmptyBucket {
        #[arg(long)]
        bucket: String,
    },
    RecoveryInventory,
    /// Preview or delete one pinned remote generation. Never changes local media.
    DeleteCopy {
        #[arg(long)]
        barcode: String,
        #[arg(long)]
        generation: u64,
        #[arg(long)]
        manifest_sha256: String,
        #[arg(long)]
        execute: bool,
    },
    /// Read-only bucket status and inventory; never requests tiering changes.
    StorageStatus,
    /// Create a new, empty OVH bucket once; never adopt an existing bucket.
    CreateBucket {
        #[arg(long)]
        bucket: String,
    },
    RecallInspect,
    StorageAccess,
    /// Deep read-only verification of every object of the bucket (never local media).
    ArchiveInspect,
    ConfigProbe,
    Check,
    BucketStatus,
    ExportDirectory {
        #[arg(long)]
        barcode: String,
        #[arg(long, default_value_t = 1)]
        generation: u64,
        #[arg(long)]
        source: PathBuf,
        #[arg(long, default_value_t = DEFAULT_SEGMENT_SIZE_MIB)]
        segment_size_mib: usize,
        #[arg(long, default_value_t = 256)]
        multipart_part_size_mib: u32,
        #[arg(long)]
        upload_state_dir: Option<PathBuf>,
        #[arg(long)]
        skip_deep_verify: bool,
    },
    Verify {
        #[arg(long)]
        barcode: String,
        #[arg(long, default_value_t = 1)]
        generation: u64,
        #[arg(long)]
        deep: bool,
    },
    Reconcile {
        #[arg(long)]
        barcode: String,
        #[arg(
            long,
            required_unless_present = "remote_only",
            conflicts_with = "remote_only"
        )]
        source: Option<PathBuf>,
        #[arg(long)]
        remote_only: bool,
    },
    Restore {
        #[arg(long)]
        barcode: String,
        #[arg(long, default_value_t = 1)]
        generation: u64,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        manifest_sha256: Option<String>,
    },
    ArchiveBucket {
        #[arg(long)]
        confirm_bucket: String,
        #[arg(long)]
        acknowledge_minimum_1_tib_billing: bool,
        #[arg(long)]
        lock_days: Option<i32>,
        #[arg(long)]
        expected_receipt: Option<String>,
    },
    RequestBucketRestore {
        #[arg(long)]
        confirm_bucket: String,
        #[arg(long)]
        acknowledge_retrieval_cost: bool,
        #[arg(long)]
        expected_receipt: Option<String>,
    },
}

#[derive(Serialize)]
struct CheckOutput<'a> {
    status: &'static str,
    bucket: &'a str,
    endpoint: &'a str,
    region: &'a str,
    prefix: &'a str,
}

#[derive(Serialize)]
struct ExportOutput {
    status: &'static str,
    manifest_key: String,
    segment_count: usize,
    logical_size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    receipt: Option<cold_vtl_core::ExportReceipt>,
}

#[derive(Serialize)]
struct BucketStatusOutput<'a> {
    bucket: &'a str,
    status: String,
    archive_lock_until: Option<String>,
}

/// Commands that receive a candidate connection on stdin instead of reading
/// the catalogue: they probe a provider before anything is saved.
fn stdin_configuration(command: &Command) -> bool {
    matches!(
        command,
        Command::ConfigProbe
            | Command::DiscoverBuckets
            | Command::CheckCredentials
            | Command::BucketPresence { .. }
            | Command::ConfirmEmptyBucket { .. }
            | Command::CreateBucketFromConnection { .. }
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let config: S3Config = if stdin_configuration(&cli.command) {
        serde_json::from_reader(std::io::stdin().take(16384))
            .map_err(|_| "Invalid S3 configuration")?
    } else {
        load_config().await?
    };
    config.validate_provider()?;
    let mut store = S3Store::connect(config).await;
    // Access probes and bucket creation only perform control requests, no object payloads.
    if !(stdin_configuration(&cli.command) || matches!(cli.command, Command::CreateBucket { .. })) {
        let database = std::env::var_os("COLD_VTL_DATABASE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/lib/cold-vtl/catalog.db"));
        let id = std::env::var("COLD_VTL_VTL_ID").ok();
        if let Some(limiter) = configured_bandwidth(
            database.parent().ok_or("Database parent missing")?,
            id.as_deref(),
        )? {
            store = store.with_bandwidth(limiter);
        }
    }

    match cli.command {
        Command::DiscoverBuckets => print_json(&store.discover_buckets().await)?,
        Command::DeleteCopy {
            barcode,
            generation,
            manifest_sha256,
            execute,
        } => {
            print_json(
                &delete_copy::run(
                    &store,
                    &Barcode::parse(barcode)?,
                    generation,
                    &manifest_sha256,
                    execute,
                )
                .await?,
            )?;
        }
        Command::RecoveryInventory => print_json(&recovery_inventory::inventory(&store).await?)?,
        Command::BucketPresence { bucket } => {
            print_json(&serde_json::json!({"status":store.bucket_presence(&bucket).await}))?
        }
        Command::ConfirmEmptyBucket { bucket } => {
            // S3Error text is the SDK's own summary (no keys or signatures).
            let status = match store.managed_bucket_is_empty(&bucket).await {
                Ok(true) => "empty",
                Ok(false) => "occupied",
                Err(error) => {
                    eprintln!("confirm-empty-bucket {bucket}: {error}");
                    "unknown"
                }
            };
            print_json(&serde_json::json!({ "status": status }))?
        }
        Command::CheckCredentials => {
            print_json(&serde_json::json!({"status":store.check_credentials().await}))?
        }
        Command::CreateBucket { bucket } | Command::CreateBucketFromConnection { bucket } => {
            require_legacy_archive_endpoint(store.config())?;
            match store.create_empty_ovh_bucket(&bucket).await {
                Ok(()) => print_json(&serde_json::json!({"status":"created","bucket":bucket}))?,
                // A definitive refusal is reported on stdout so callers need no manual resolution.
                Err(S3Error::NotCreated(reason)) => {
                    print_json(
                        &serde_json::json!({"status":"not_created","reason":reason,"bucket":bucket}),
                    )?;
                    std::process::exit(3);
                }
                Err(error) => return Err(error.into()),
            }
        }
        Command::ConfigProbe => {
            if store.probe_access().await.is_err() {
                return Err(
                    "S3 connection refused: check endpoint, region, bucket and credentials.".into(),
                );
            }
            print_json(
                &serde_json::json!({"status":"ok", "read_tested":true, "write_tested":false}),
            )?;
        }
        Command::StorageStatus => print_json(&storage::status(&store).await)?,
        Command::StorageAccess => print_json(&storage::access(&store).await?)?,
        Command::RecallInspect => print_probe(storage::recall_inspect(&store).await)?,
        Command::ArchiveInspect => print_probe(storage::inspect(&store).await)?,
        Command::Check => {
            store.check_bucket().await?;
            let config = store.config();
            print_json(&CheckOutput {
                status: "ok",
                bucket: &config.bucket,
                endpoint: &config.endpoint,
                region: &config.region,
                prefix: &config.prefix,
            })?;
        }
        Command::BucketStatus => {
            require_legacy_archive_endpoint(store.config())?;
            let status = store.bucket_status().await?;
            print_json(&BucketStatusOutput {
                bucket: &store.config().bucket,
                status: status.status,
                archive_lock_until: status.archive_lock_until,
            })?;
        }
        Command::ExportDirectory {
            barcode,
            generation,
            source,
            segment_size_mib,
            multipart_part_size_mib,
            upload_state_dir,
            skip_deep_verify,
        } => {
            let barcode = Barcode::parse(barcode)?;
            let manifest = upload::export_directory(
                &store,
                barcode,
                generation,
                &source,
                segment_size_mib,
                multipart_part_size_mib,
                upload_state_dir.as_deref(),
                !skip_deep_verify,
            )
            .await?;
            let manifest_key = manifest_key(store.config(), &manifest.barcode, generation);
            let receipt = (manifest.format_version == 3)
                .then(|| -> Result<_, Box<dyn Error>> {
                    let proof = cold_vtl_core::ExportReceipt {
                        scope: cold_vtl_core::ExportScope {
                            job_id: std::env::var("COLD_VTL_EXPORT_JOB_ID").ok(),
                            vtl_id: std::env::var("COLD_VTL_VTL_ID").ok(),
                            destination_id: std::env::var("COLD_VTL_DESTINATION_ID").ok(),
                            configuration_revision: std::env::var(
                                "COLD_VTL_CONFIGURATION_REVISION",
                            )
                            .ok()
                            .map(|v| v.parse())
                            .transpose()?,
                            source: source
                                .canonicalize()?
                                .to_str()
                                .ok_or("non UTF-8 source")?
                                .into(),
                        },
                        generation: RemoteGeneration {
                            barcode: manifest.barcode.clone(),
                            generation,
                            format_version: 3,
                            manifest_key: manifest_key.clone(),
                            manifest_sha256: sha256_hex(&serde_json::to_vec_pretty(&manifest)?),
                            logical_size_bytes: manifest.logical_size_bytes,
                            file_count: manifest.files.len(),
                            segment_count: manifest.segments.len(),
                            local_match: true,
                        },
                        integrity: cold_vtl_core::IntegrityValidation {
                            status: "verified".into(),
                            method: "s3_sha256".into(),
                            verified_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)?
                                .as_secs(),
                            verified_segments: manifest.segments.len(),
                            verified_bytes: manifest.logical_size_bytes,
                        },
                    };
                    proof.validate(&manifest.barcode, generation)?;
                    Ok(proof)
                })
                .transpose()?;
            print_json(&ExportOutput {
                status: "verified",
                manifest_key,
                segment_count: manifest.segments.len(),
                logical_size_bytes: manifest.logical_size_bytes,
                receipt,
            })?;
        }
        Command::Verify {
            barcode,
            generation,
            deep,
        } => {
            let barcode = Barcode::parse(barcode)?;
            let key = manifest_key(store.config(), &barcode, generation);
            let manifest: Manifest =
                serde_json::from_slice(&read_manifest_bytes(&store, &key).await?)?;
            validate_identity(&manifest, store.config(), &barcode, generation)?;
            verify_manifest(&store, &manifest, deep).await?;
            print_json(&ExportOutput {
                status: "verified",
                manifest_key: key,
                segment_count: manifest.segments.len(),
                logical_size_bytes: manifest.logical_size_bytes,
                receipt: None,
            })?;
        }
        Command::Reconcile {
            barcode,
            source,
            remote_only: _,
        } => {
            storage::require_access(&store, false).await?;
            let barcode = Barcode::parse(barcode)?;
            let _media_guard = source
                .as_deref()
                .map(cold_vtl_core::MediaReadGuard::acquire)
                .transpose()?;
            let local = source.as_deref().map(fingerprint).transpose()?;
            let prefix = store
                .config()
                .object_key(&format!("cartridges/{barcode}/manifests/"));
            let mut generations = Vec::new();
            for key in store.list_keys(&prefix).await? {
                let Some(generation) = key
                    .strip_prefix(&prefix)
                    .and_then(|s| s.strip_suffix(".json"))
                    .and_then(|s| s.parse::<u64>().ok())
                else {
                    continue;
                };
                if key != manifest_key(store.config(), &barcode, generation) {
                    continue;
                }
                let bytes = read_manifest_bytes(&store, &key).await?;
                let manifest: Manifest = serde_json::from_slice(&bytes)?;
                validate_identity(&manifest, store.config(), &barcode, generation)?;
                verify_manifest(&store, &manifest, true).await?;
                let local_match = manifest.format_version >= 2
                    && local.as_ref().is_some_and(|files| manifest.files == *files);
                generations.push(RemoteGeneration {
                    barcode: barcode.clone(),
                    generation,
                    format_version: manifest.format_version,
                    manifest_key: key,
                    manifest_sha256: sha256_hex(&bytes),
                    logical_size_bytes: manifest.logical_size_bytes,
                    file_count: manifest.files.len(),
                    segment_count: manifest.segments.len(),
                    local_match,
                });
            }
            if local != source.as_deref().map(fingerprint).transpose()? {
                return Err("source changed during reconciliation".into());
            }
            generations.sort_by_key(|g| g.generation);
            print_json(&ReconcileOutput {
                local_size_bytes: local.iter().flatten().map(|f| f.size_bytes).sum(),
                generations,
            })?;
        }
        Command::Restore {
            barcode,
            generation,
            destination,
            manifest_sha256,
        } => {
            storage::require_access(&store, false).await?;
            let barcode = Barcode::parse(barcode)?;
            let key = manifest_key(store.config(), &barcode, generation);
            let bytes = read_manifest_bytes(&store, &key).await?;
            if manifest_sha256.is_some_and(|expected| sha256_hex(&bytes) != expected) {
                return Err(
                    "manifest changed since reconciliation; refresh generations before restoring"
                        .into(),
                );
            }
            let manifest: Manifest = serde_json::from_slice(&bytes)?;
            validate_identity(&manifest, store.config(), &barcode, generation)?;
            let history =
                upload_history::Recorder::restore_from_env(barcode.as_str(), generation).await;
            if let Some(history) = &history {
                history.set_total(manifest.logical_size_bytes);
                history.phase("downloading");
                history.set_active(true);
            }
            let result = restore_directory(&store, &manifest, &destination, history.as_ref()).await;
            if let Some(history) = history {
                history.set_active(false);
                history.close().await;
            }
            result?;
            // Persist an integrity receipt in the catalogue job, separately from the
            // mutable restore directory. Reintegration checks every file again.
            print_json(&serde_json::json!({
                "status": "restored", "barcode": barcode, "generation": generation,
                "manifest_key": key, "manifest_sha256": sha256_hex(&bytes),
                "segment_count": manifest.segments.len(),
                "logical_size_bytes": manifest.logical_size_bytes,
                "files": fingerprint(&destination)?,
            }))?;
        }
        Command::ArchiveBucket {
            confirm_bucket,
            acknowledge_minimum_1_tib_billing,
            lock_days,
            expected_receipt,
        } => {
            require_legacy_archive_endpoint(store.config())?;
            require_bucket_confirmation(store.config(), &confirm_bucket)?;
            if !acknowledge_minimum_1_tib_billing {
                return Err("archive refused: pass --acknowledge-minimum-1-tib-billing".into());
            }
            if let Some(expected) = expected_receipt {
                // The bucket must still be exactly what the controller verified.
                let receipt = storage::inspect(&store).await?;
                if receipt["digest"].as_str() != Some(&expected) {
                    return Err("archive verification changed; verify again".into());
                }
            }
            let before = store.bucket_status().await?;
            if before.status != "None" || before.archive_lock_until.is_some() {
                return Err("first archive requires an available unlocked bucket".into());
            }
            let pending_uploads = store.incomplete_multipart_uploads().await?;
            if pending_uploads != 0 {
                return Err(format!(
                    "archive refused: {pending_uploads} incomplete multipart upload(s)"
                )
                .into());
            }
            store.request_ovh_archive("cold-vtl", lock_days).await?;
            // A successful PUT acknowledges submission, not completion on tapes.
            print_json(
                &serde_json::json!({"bucket":store.config().bucket,"status":"request_accepted"}),
            )?;
        }
        Command::RequestBucketRestore {
            confirm_bucket,
            acknowledge_retrieval_cost,
            expected_receipt,
        } => {
            require_legacy_archive_endpoint(store.config())?;
            require_bucket_confirmation(store.config(), &confirm_bucket)?;
            if !acknowledge_retrieval_cost {
                return Err("restore refused: pass --acknowledge-retrieval-cost".into());
            }
            let receipt = storage::recall_inspect(&store).await?;
            if expected_receipt
                .is_some_and(|expected| receipt["digest"].as_str() != Some(&expected))
            {
                return Err("The bucket changed since its state was read. Read its state again, then start the recall again.".into());
            }
            store.request_ovh_restore("cold-vtl").await?;
            print_json(
                &serde_json::json!({"bucket":store.config().bucket,"status":"request_accepted"}),
            )?;
        }
    }

    Ok(())
}

async fn verify_manifest(
    store: &S3Store,
    manifest: &Manifest,
    deep: bool,
) -> Result<(), Box<dyn Error>> {
    let mut total_size = 0_u64;
    for segment in &manifest.segments {
        if let Some(checksum) = &segment.s3_checksum {
            store
                .verify_native_head(&segment.object_key, segment.size_bytes, checksum)
                .await?;
        } else {
            store
                .verify_head(
                    &segment.object_key,
                    segment.size_bytes,
                    &segment.checksum_sha256,
                )
                .await?;
        }
        if deep {
            verify_downloaded_segment(
                store,
                &segment.object_key,
                segment.size_bytes,
                &segment.checksum_sha256,
            )
            .await?;
        }
        total_size += segment.size_bytes;
    }

    if total_size != manifest.logical_size_bytes {
        return Err(format!(
            "manifest logical size mismatch: expected {}, got {total_size}",
            manifest.logical_size_bytes
        )
        .into());
    }

    Ok(())
}

async fn verify_downloaded_segment(
    store: &S3Store,
    key: &str,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(), Box<dyn Error>> {
    stream_segment(store, key, expected_size, expected_sha256, |_| Ok(())).await
}

async fn restore_directory(
    store: &S3Store,
    manifest: &Manifest,
    destination: &Path,
    history: Option<&upload_history::Recorder>,
) -> Result<(), Box<dyn Error>> {
    if destination.exists() {
        return Err(format!(
            "restore destination already exists: {}",
            destination.display()
        )
        .into());
    }
    manifest.validate()?;
    reject_symlinks(destination)?;
    // Exclusive creation prevents overwriting an existing directory or symlink.
    std::fs::create_dir(destination)?;
    let incomplete = destination.join(".cold-vtl-incomplete");
    std::fs::write(
        &incomplete,
        b"Restore incomplete; do not mount this cartridge.\n",
    )?;

    let mut total_size = 0_u64;
    for segment in &manifest.segments {
        let relative = safe_relative_path(&segment.source_path)?;
        let target = destination.join(relative);
        reject_symlinks(&target)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let current_size = target
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if current_size != segment.source_offset_bytes {
            return Err(format!(
                "unexpected restore offset for {}: expected {}, got {current_size}",
                segment.source_path, segment.source_offset_bytes
            )
            .into());
        }
        reject_symlinks(&target)?;
        let mut output = OpenOptions::new().create(true).append(true).open(&target)?;
        stream_segment(
            store,
            &segment.object_key,
            segment.size_bytes,
            &segment.checksum_sha256,
            |chunk| {
                output.write_all(chunk)?;
                if let Some(history) = history {
                    history.acknowledge(chunk.len() as u64);
                }
                Ok(())
            },
        )
        .await?;
        output.sync_all()?;
        total_size += segment.size_bytes;
    }

    if let Some(history) = history {
        history.set_active(false);
        history.phase("verifying");
    }
    for file in &manifest.files {
        let relative = safe_relative_path(&file.path)?;
        let target = destination.join(relative);
        reject_symlinks(&target)?;
        if !target.exists() && file.size_bytes == 0 {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            File::create(&target)?;
        }
        let (size, checksum) = sha256_file(&target)?;
        if size != file.size_bytes || checksum != file.checksum_sha256 {
            return Err(format!("restored file verification failed: {}", file.path).into());
        }
    }

    if total_size != manifest.logical_size_bytes {
        return Err("restored logical size does not match the manifest".into());
    }
    std::fs::remove_file(incomplete)?;
    Ok(())
}

fn fingerprint(source: &Path) -> Result<Vec<CartridgeFile>, Box<dyn Error>> {
    let mut files = Vec::new();
    for path in cartridge_files(source)? {
        let (size_bytes, checksum_sha256) = sha256_file(&path)?;
        files.push(CartridgeFile {
            path: path.strip_prefix(source)?.to_string_lossy().into_owned(),
            size_bytes,
            checksum_sha256,
        });
    }
    Ok(files)
}

fn validate_identity(
    manifest: &Manifest,
    config: &S3Config,
    barcode: &Barcode,
    generation: u64,
) -> Result<(), Box<dyn Error>> {
    manifest.validate()?;
    if &manifest.barcode != barcode || manifest.generation != generation {
        return Err("manifest identity mismatch".into());
    }
    let prefix = config.object_key(&format!(
        "cartridges/{barcode}/generations/{generation}/segments/"
    ));
    for segment in &manifest.segments {
        if !segment.object_key.starts_with(&prefix) {
            return Err("segment is outside the requested generation".into());
        }
    }
    if manifest
        .files
        .iter()
        .any(|f| f.path.split('/').next() == Some(".cold-vtl-incomplete"))
        || manifest
            .segments
            .iter()
            .any(|s| s.source_path.split('/').next() == Some(".cold-vtl-incomplete"))
    {
        return Err("reserved file name in manifest".into());
    }
    Ok(())
}

fn cartridge_files(source: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    reject_symlinks(source)?;
    if !source.is_dir() {
        return Err("source is not a directory".into());
    }
    let mut files = Vec::new();
    for entry in WalkDir::new(source).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() {
            files.push(entry.into_path());
        } else if !entry.file_type().is_dir() {
            return Err("symlink or special file in cartridge".into());
        }
    }
    files.sort();
    Ok(files)
}

/// Refuse symlinks in existing path components, including the source root.
fn reject_symlinks(path: &Path) -> Result<(), Box<dyn Error>> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(info) if info.file_type().is_symlink() => {
                return Err("symlink in worker path".into());
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

const MAX_MANIFEST_BYTES: usize = 16 * 1024 * 1024;
async fn read_manifest_bytes(store: &S3Store, key: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut stream = store.get_stream(key).await?;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > MAX_MANIFEST_BYTES - bytes.len() {
            return Err("manifest exceeds 16 MiB".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn stream_segment(
    store: &S3Store,
    key: &str,
    expected_size: u64,
    expected_sha256: &str,
    mut consume: impl FnMut(&[u8]) -> Result<(), Box<dyn Error>>,
) -> Result<(), Box<dyn Error>> {
    let mut stream = store.get_stream(key).await?;
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        size = size
            .checked_add(chunk.len() as u64)
            .ok_or("segment size overflow")?;
        if size > expected_size {
            return Err(format!("segment exceeds expected size: {key}").into());
        }
        hash.update(&chunk);
        consume(&chunk)?;
    }
    if size != expected_size || hex::encode(hash.finalize()) != expected_sha256 {
        return Err(format!("download verification failed for {key}").into());
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn sha256_file(path: &Path) -> Result<(u64, String), Box<dyn Error>> {
    reject_symlinks(path)?;
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    let mut size = 0_u64;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        size += count as u64;
    }
    Ok((size, hex::encode(hasher.finalize())))
}

fn safe_relative_path(raw: &str) -> Result<PathBuf, Box<dyn Error>> {
    let path = Path::new(raw);
    if !safe_manifest_path(raw)
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(format!("unsafe path in manifest: {raw:?}").into());
    }
    Ok(path.to_owned())
}

fn manifest_key(config: &S3Config, barcode: &Barcode, generation: u64) -> String {
    config.object_key(&format!(
        "cartridges/{}/manifests/{generation}.json",
        barcode.as_str()
    ))
}

fn require_legacy_archive_endpoint(config: &S3Config) -> Result<(), Box<dyn Error>> {
    config.validate_provider()?;
    if !config.provider().definition().managed_archive {
        return Err("Managed archive and recall are unavailable for this S3 provider".into());
    }
    Ok(())
}

fn require_bucket_confirmation(
    config: &S3Config,
    confirmation: &str,
) -> Result<(), Box<dyn Error>> {
    if confirmation != config.bucket {
        return Err("bucket confirmation does not match COLD_VTL_BUCKET".into());
    }
    Ok(())
}

fn print_probe(result: Result<serde_json::Value, Box<dyn Error>>) -> Result<(), Box<dyn Error>> {
    match result {
        Ok(receipt) => print_json(&receipt)?,
        Err(error) => {
            let message = if error
                .downcast_ref::<cold_vtl_provider_s3::S3Error>()
                .is_some()
            {
                "S3 check failed: verify access, network, archive configuration and copy integrity."
                    .to_owned()
            } else {
                error.to_string()
            };
            print_json(&serde_json::json!({"error":message}))?;
            return Err("cloud preparation refused".into());
        }
    }
    Ok(())
}

fn print_json(value: &impl Serialize) -> Result<(), serde_json::Error> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_commands_no_longer_take_a_local_media_root() {
        assert!(Cli::try_parse_from(["worker", "archive-inspect"]).is_ok());
        assert!(
            Cli::try_parse_from(["worker", "archive-inspect", "--media-root", "/opt/mhvtl"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "worker",
                "archive-bucket",
                "--confirm-bucket",
                "b",
                "--acknowledge-minimum-1-tib-billing",
                "--expected-receipt",
                "d"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "worker",
                "archive-bucket",
                "--confirm-bucket",
                "b",
                "--media-root",
                "/opt/mhvtl"
            ])
            .is_err()
        );
    }

    #[test]
    fn bucket_presence_takes_its_configuration_on_stdin_and_requires_a_bucket() {
        assert!(Cli::try_parse_from(["worker", "bucket-presence"]).is_err());
        let cli = Cli::try_parse_from([
            "worker",
            "bucket-presence",
            "--bucket",
            "vc-a-2026-10-13-abcd",
        ])
        .unwrap();
        assert!(stdin_configuration(&cli.command));
        match cli.command {
            Command::BucketPresence { bucket } => assert_eq!(bucket, "vc-a-2026-10-13-abcd"),
            _ => panic!("wrong command"),
        }
        let status = Cli::try_parse_from(["worker", "storage-status"]).unwrap();
        assert!(!stdin_configuration(&status.command));
    }

    #[test]
    fn confirm_empty_bucket_is_a_stdin_probe() {
        let cli =
            Cli::try_parse_from(["worker", "confirm-empty-bucket", "--bucket", "vc-x"]).unwrap();
        assert!(stdin_configuration(&cli.command));
        assert!(Cli::try_parse_from(["worker", "confirm-empty-bucket"]).is_err());
    }

    #[test]
    fn reconciliation_requires_an_explicit_local_or_remote_only_mode() {
        assert!(Cli::try_parse_from(["worker", "reconcile", "--barcode", "TAPE01"]).is_err());
        assert!(
            Cli::try_parse_from([
                "worker",
                "reconcile",
                "--barcode",
                "TAPE01",
                "--remote-only"
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "worker",
                "reconcile",
                "--barcode",
                "TAPE01",
                "--source",
                "/tmp/local",
                "--remote-only"
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "worker",
                "reconcile",
                "--barcode",
                "TAPE01",
                "--source",
                "/tmp/local"
            ])
            .is_ok()
        );
    }

    #[test]
    fn isolated_transfer_cannot_start_without_vtl_bandwidth() {
        let root = std::env::temp_dir().join(format!("worker-bandwidth-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert!(configured_bandwidth(&root, None).unwrap().is_none());
        std::fs::write(root.join("storage-isolated"), b"{}").unwrap();
        assert!(configured_bandwidth(&root, None).is_err());
        assert!(configured_bandwidth(&root, Some("00000000-0000-4000-8000-000000000021")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn pinned_configuration_never_follows_active_bucket_or_credentials() {
        use sqlx::Connection;
        let path = std::env::temp_dir().join(format!("vtl-worker-pins-{}.db", std::process::id()));
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let mut db = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE storage_destinations(id TEXT,vtl_id TEXT,active INTEGER)")
            .execute(&mut db)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE storage_destination_revisions(destination_id TEXT,revision INTEGER,configuration TEXT)").execute(&mut db).await.unwrap();
        sqlx::query("INSERT INTO storage_destinations VALUES('old','vtl',0),('new','vtl',1)")
            .execute(&mut db)
            .await
            .unwrap();
        let config = serde_json::json!({"endpoint":"https://example.test","region":"test","bucket":"old-bucket","prefix":"vtl","access_key_id":"old-key","secret_access_key":"old-secret"});
        sqlx::query("INSERT INTO storage_destination_revisions VALUES('old',1,?)")
            .bind(config.to_string())
            .execute(&mut db)
            .await
            .unwrap();
        let mut next = config.clone();
        next["bucket"] = serde_json::json!("new-bucket");
        next["secret_access_key"] = serde_json::json!("new-secret");
        sqlx::query("INSERT INTO storage_destination_revisions VALUES('new',2,?),('old',3,?)")
            .bind(next.to_string())
            .bind(next.to_string())
            .execute(&mut db)
            .await
            .unwrap();
        let selected = pinned_config(&path, "vtl", "old", 1).await.unwrap();
        assert_eq!(selected.bucket, "old-bucket");
        assert!(selected == serde_json::from_value::<S3Config>(config).unwrap());
        for (owner, dest, revision) in [
            ("foreign", "old", 1),
            ("vtl", "missing", 1),
            ("vtl", "old", 2),
            ("vtl", "old", 0),
        ] {
            assert!(pinned_config(&path, owner, dest, revision).await.is_err());
        }
        db.close().await.unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn saved_config_updates_each_launch_and_never_ignores_corrupt_settings() {
        use sqlx::Connection;
        let path =
            std::env::temp_dir().join(format!("vtl-worker-settings-{}.db", std::process::id()));
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let mut db = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        assert!(saved_config(&path).await.unwrap().is_none());
        sqlx::query("CREATE TABLE s3_settings(id INTEGER PRIMARY KEY,configuration TEXT)")
            .execute(&mut db)
            .await
            .unwrap();
        assert!(saved_config(&path).await.unwrap().is_none());
        let mut config = serde_json::json!({"endpoint":"https://example.test","region":"test","bucket":"first-bucket","prefix":"vtl","access_key_id":"key","secret_access_key":"secret"});
        sqlx::query("INSERT INTO s3_settings VALUES(1,?)")
            .bind(config.to_string())
            .execute(&mut db)
            .await
            .unwrap();
        assert_eq!(
            saved_config(&path).await.unwrap().unwrap().bucket,
            "first-bucket"
        );
        config["bucket"] = serde_json::json!("second-bucket");
        sqlx::query("UPDATE s3_settings SET configuration=?")
            .bind(config.to_string())
            .execute(&mut db)
            .await
            .unwrap();
        assert_eq!(
            saved_config(&path).await.unwrap().unwrap().bucket,
            "second-bucket"
        );
        sqlx::query("UPDATE s3_settings SET configuration='invalid'")
            .execute(&mut db)
            .await
            .unwrap();
        assert!(saved_config(&path).await.is_err());
        db.close().await.unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn export_cli_supports_large_segments_and_independent_parts() {
        let cli = Cli::try_parse_from([
            "worker",
            "export-directory",
            "--barcode",
            "TAPE01",
            "--source",
            "/tmp/media",
            "--multipart-part-size-mib",
            "256",
            "--upload-state-dir",
            "/tmp/uploads",
        ])
        .unwrap();
        match cli.command {
            Command::ExportDirectory {
                segment_size_mib, ..
            } => assert_eq!(segment_size_mib, 1048576),
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn provider_selection_controls_managed_archive_and_rejects_unknown_providers() {
        let mut value = serde_json::json!({"endpoint":cold_vtl_core::OVH_ARCHIVE_ENDPOINT,"region":"rbx-archive","bucket":"test-bucket","prefix":"vtl","access_key_id":"test-key","secret_access_key":"test-secret"});
        let legacy: S3Config = serde_json::from_value(value.clone()).unwrap();
        assert!(require_legacy_archive_endpoint(&legacy).is_ok());
        value["provider"] = serde_json::json!("s3_compatible");
        let mismatch: S3Config = serde_json::from_value(value.clone()).unwrap();
        assert!(mismatch.validate_provider().is_err());
        assert!(require_legacy_archive_endpoint(&mismatch).is_err());
        value["endpoint"] = serde_json::json!("https://s3.example.test");
        let generic: S3Config = serde_json::from_value(value.clone()).unwrap();
        assert!(generic.validate_provider().is_ok());
        assert!(require_legacy_archive_endpoint(&generic).is_err());
        value["provider"] = serde_json::json!("unknown");
        assert!(serde_json::from_value::<S3Config>(value).is_err());
    }

    #[test]
    fn restore_paths_must_stay_below_the_destination() {
        assert_eq!(
            safe_relative_path("data.0").unwrap(),
            PathBuf::from("data.0")
        );
        assert!(safe_relative_path("../etc/passwd").is_err());
        assert!(safe_relative_path("/etc/passwd").is_err());
        assert!(safe_relative_path("").is_err());
    }
}
