pub mod buckets;
pub mod destinations;
pub mod retire;
pub mod schedule;
#[cfg(test)]
pub(crate) mod test_support;
use crate::{ApiError, AppState, auth::Principal};
use axum::{Extension, Json, extract::State, http::StatusCode};
use cold_vtl_core::S3Provider;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{env, os::unix::fs::PermissionsExt, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

// Never derive Debug or return this structure through an HTTP response.
#[derive(Clone, Deserialize, Serialize)]
struct Configuration {
    #[serde(default)]
    provider: Option<S3Provider>,
    endpoint: String,
    region: String,
    bucket: String,
    prefix: String,
    access_key_id: String,
    secret_access_key: String,
}
impl Configuration {
    fn provider(&self) -> S3Provider {
        self.provider
            .unwrap_or_else(|| S3Provider::infer(&self.endpoint))
    }
    fn environment() -> Option<Self> {
        let endpoint = env::var("AWS_ENDPOINT_URL").ok()?;
        Some(Self {
            provider: Some(S3Provider::infer(&endpoint)),
            region: env::var("COLD_VTL_S3_REGION")
                .or_else(|_| env::var("AWS_REGION"))
                .or_else(|_| env::var("AWS_DEFAULT_REGION"))
                .unwrap_or_else(|_| {
                    if endpoint.contains("rbx-archive") {
                        "rbx-archive"
                    } else if endpoint.contains("eu-west-par") {
                        "eu-west-par"
                    } else {
                        "us-east-1"
                    }
                    .into()
                }),
            endpoint,
            bucket: env::var("COLD_VTL_BUCKET").ok()?,
            prefix: env::var("COLD_VTL_PREFIX")
                .unwrap_or_else(|_| "cold-vtl".into())
                .trim_matches('/')
                .into(),
            access_key_id: env::var("AWS_ACCESS_KEY_ID").ok()?,
            secret_access_key: env::var("AWS_SECRET_ACCESS_KEY").ok()?,
        })
    }
    fn same_destination(&self, other: &Self) -> bool {
        self.provider() == other.provider()
            && self.endpoint.trim_end_matches('/') == other.endpoint.trim_end_matches('/')
            && self.region == other.region
            && self.bucket == other.bucket
            && self.prefix == other.prefix
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    #[serde(default)]
    provider: Option<S3Provider>,
    revision: i64,
    endpoint: String,
    region: String,
    bucket: String,
    prefix: String,
    access_key_id: String,
    /// Null preserves the current secret. An empty string is invalid.
    secret_access_key: Option<String>,
}
fn admin(principal: &Principal) -> Result<(), ApiError> {
    if principal.user.role != "admin" {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            message: "administrator required".into(),
        });
    }
    Ok(())
}
async fn legacy_configuration(state: &AppState) -> Result<(i64, Option<Configuration>), ApiError> {
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT revision,configuration FROM s3_settings WHERE id=1")
            .fetch_optional(&state.catalog.pool())
            .await
            .map_err(ApiError::conflict)?;
    match row {
        Some((revision, value)) => Ok((
            revision,
            Some(
                serde_json::from_str(&value)
                    .map_err(|_| ApiError::conflict("Invalid saved S3 configuration."))?,
            ),
        )),
        None => Ok((0, Configuration::environment())),
    }
}
async fn current(state: &AppState) -> Result<(i64, Option<Configuration>), ApiError> {
    destinations::current(state).await
}
pub async fn get(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let (revision, config) = current(&state).await?;
    if let Some(c) = &config {
        c.provider()
            .validate_endpoint(&c.endpoint)
            .map_err(ApiError::conflict)?;
    }
    let pending: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM s3_reconcile_required WHERE vtl_id=?")
            .bind(state.catalog.vtl_id().await?.to_string())
            .fetch_one(&state.catalog.pool())
            .await
            .map_err(ApiError::conflict)?;
    let fields = config.as_ref().map(|c| json!({"provider":c.provider(),"endpoint":c.endpoint,"region":c.region,"bucket":c.bucket,"prefix":c.prefix,"access_key_id":c.access_key_id}));
    Ok(Json(
        json!({"providers":S3Provider::ALL.map(S3Provider::definition),"revision":revision,"configured":config.is_some(),"configuration":fields,
        "secret_configured":config.as_ref().is_some_and(|c|!c.secret_access_key.is_empty()),"reconcile_required":pending}),
    ))
}
fn candidate(
    update: Update,
    revision: i64,
    old: Option<&Configuration>,
) -> Result<Configuration, ApiError> {
    if update.revision != revision {
        return Err(ApiError::conflict(
            "The configuration has changed. Reload it before continuing.",
        ));
    }
    let endpoint = url::Url::parse(&update.endpoint)
        .map_err(|_| ApiError::bad_request("Invalid HTTPS endpoint."))?;
    if endpoint.scheme() != "https"
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.path() != "/"
        || update.endpoint.len() > 2048
    {
        return Err(ApiError::bad_request(
            "The endpoint must be an HTTPS origin without a path, credentials or parameters.",
        ));
    }
    let provider = update
        .provider
        .unwrap_or_else(|| S3Provider::infer(&endpoint.origin().ascii_serialization()));
    provider
        .validate_endpoint(&endpoint.origin().ascii_serialization())
        .map_err(ApiError::bad_request)?;
    let bucket = update.bucket;
    if !(3..=63).contains(&bucket.len())
        || !bucket
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        || !bucket.as_bytes()[0].is_ascii_alphanumeric()
        || !bucket.as_bytes()[bucket.len() - 1].is_ascii_alphanumeric()
        || bucket.contains("..")
        || bucket.parse::<std::net::IpAddr>().is_ok()
    {
        return Err(ApiError::bad_request(
            "Invalid bucket name (3 to 63 lowercase letters, numbers, dots or hyphens).",
        ));
    }
    if update.region.is_empty()
        || update.region.len() > 64
        || !update
            .region
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || update.prefix.len() > 512
        || update.prefix.chars().any(char::is_control)
        || update.prefix.contains('\\')
        || update.prefix.split('/').any(|p| p == ".." || p == ".")
        || update.access_key_id.is_empty()
        || update.access_key_id.len() > 256
        || update.access_key_id.chars().any(char::is_whitespace)
    {
        return Err(ApiError::bad_request(
            "Invalid region, prefix or access key.",
        ));
    }
    let secret = match update.secret_access_key {
        Some(value) => value,
        None => old
            .filter(|c| c.access_key_id == update.access_key_id)
            .map(|c| c.secret_access_key.clone())
            .ok_or_else(|| {
                ApiError::bad_request("Enter the secret associated with this access key.")
            })?,
    };
    if secret.is_empty() || secret.len() > 4096 || secret.chars().any(char::is_control) {
        return Err(ApiError::bad_request("Empty or invalid S3 secret."));
    }
    Ok(Configuration {
        provider: Some(provider),
        endpoint: endpoint.origin().ascii_serialization(),
        region: update.region,
        bucket,
        prefix: update.prefix.trim_matches('/').into(),
        access_key_id: update.access_key_id,
        secret_access_key: secret,
    })
}
async fn probe(state: &AppState, config: &Configuration) -> Result<Value, ApiError> {
    let work = async {
        let mut child = Command::new(&state.config.worker_path)
            .arg("config-probe")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| ())?;
        let mut input = child.stdin.take().ok_or(())?;
        input
            .write_all(&serde_json::to_vec(config).map_err(|_| ())?)
            .await
            .map_err(|_| ())?;
        drop(input);
        let result = child.wait_with_output().await.map_err(|_| ())?;
        if !result.status.success() {
            return Err(());
        }
        let value: Value = serde_json::from_slice(&result.stdout).map_err(|_| ())?;
        if value["status"] != "ok" {
            return Err(());
        }
        // Whitelist the response; a worker can never reflect submitted credentials.
        Ok(json!({"status":"ok","read_tested":true,"write_tested":false}))
    };
    tokio::time::timeout(Duration::from_secs(25),work).await
        .map_err(|_|ApiError::conflict("S3 test interrupted: timeout. Configuration unchanged."))?
        .map_err(|_|ApiError::conflict("S3 connection refused: check endpoint, region, bucket, credentials and permission to list objects. Configuration unchanged."))
}
pub async fn test(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(update): Json<Update>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let _lock = state.operations.lock().await;
    let (revision, old) = current(&state).await?;
    let config = candidate(update, revision, old.as_ref())?;
    Ok(Json(probe(&state, &config).await?))
}
async fn idle(state: &AppState, destination_changed: bool) -> Result<(), ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    let pending: i64 = sqlx::query_scalar("SELECT (SELECT COUNT(*) FROM jobs WHERE vtl_id=? AND status IN ('queued','running')) + (SELECT COUNT(*) FROM archive_requests WHERE vtl_id=? AND ? AND phase IN ('queued','submitting','uncertain','accepted'))")
        .bind(&vtl).bind(&vtl).bind(destination_changed).fetch_one(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    if pending > 0 {
        return Err(ApiError::conflict(
            "Wait for jobs to finish and archiving or recall requests to be resolved before changing S3.",
        ));
    }
    if vtl == destinations::LEGACY && state.config.reintegration_helper.is_some() {
        let status = crate::reintegration::status(State(state.clone())).await?.0;
        if !status["pending"].is_null() {
            return Err(ApiError::conflict(
                "Complete PBS reintegration before changing S3.",
            ));
        }
    }
    Ok(())
}
async fn private_catalog(state: &AppState) -> Result<(), ApiError> {
    use sqlx::Row;
    let rows = sqlx::query("PRAGMA database_list")
        .fetch_all(&state.catalog.pool())
        .await
        .map_err(ApiError::conflict)?;
    let file = rows
        .iter()
        .find(|r| r.get::<String, _>("name") == "main")
        .map(|r| r.get::<String, _>("file"))
        .ok_or_else(|| ApiError::conflict("Persistent catalogue required."))?;
    let path = std::path::Path::new(&file);
    let parent = path
        .parent()
        .ok_or_else(|| ApiError::conflict("Invalid catalogue directory."))?;
    if std::fs::metadata(parent)
        .map_err(|_| ApiError::conflict("Catalogue unavailable."))?
        .permissions()
        .mode()
        & 0o077
        != 0
    {
        return Err(ApiError::conflict(
            "The catalogue directory must be protected with mode 0700 before saving an S3 secret.",
        ));
    }
    for name in [file.clone(), format!("{file}-wal"), format!("{file}-shm")] {
        if std::path::Path::new(&name).exists() {
            std::fs::set_permissions(name, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| ApiError::conflict("Unable to protect the S3 catalogue."))?;
        }
    }
    Ok(())
}
pub async fn apply(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(update): Json<Update>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let _lock = state.operations.lock().await;
    let (revision, old) = current(&state).await?;
    let config = candidate(update, revision, old.as_ref())?;
    let changed = old.as_ref().is_none_or(|c| !c.same_destination(&config));
    idle(&state, changed).await?;
    probe(&state, &config).await?;
    idle(&state, changed).await?;
    private_catalog(&state).await?;
    let destination_id =
        destinations::save(&state, &config, revision, changed, &principal.user.username).await?;
    if changed {
        background_synchronisation(&state);
    }
    Ok(Json(
        json!({"status":"applied","revision":revision+1,"destination_id":destination_id,"destination_changed":changed,
        "message":if changed {"Configuration applied. Cartridges are synchronising with this bucket in the background."} else {"Configuration applied. Future S3 access will use these credentials."}}),
    ))
}
/// Refuse to send a cartridge still to synchronise with the active bucket
/// while its synchronisation runs.
pub async fn require_not_synchronising(state: &AppState, barcode: &str) -> Result<(), ApiError> {
    require_not_synchronising_for(state, barcode, Retry::Export).await
}

/// What the reader is told to start again once a synchronisation finishes.
#[derive(Clone, Copy)]
pub enum Retry {
    Export,
    Operation,
}

/// Same, with the retry wording of the refused operation (the export or the operation).
pub async fn require_not_synchronising_for(
    state: &AppState,
    barcode: &str,
    retry: Retry,
) -> Result<(), ApiError> {
    let running: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM s3_reconcile_required r JOIN jobs j ON j.vtl_id=r.vtl_id AND j.subject=r.barcode WHERE r.barcode=? AND r.vtl_id=? AND j.kind='reconcile' AND j.status IN ('queued','running'))",
    )
    .bind(barcode)
    .bind(state.catalog.vtl_id().await?.to_string())
    .fetch_one(&state.catalog.pool())
    .await
    .map_err(ApiError::conflict)?;
    if running {
        return Err(ApiError::conflict(match retry {
            Retry::Export => {
                "The cartridge is synchronising with the bucket. Start the export again when the synchronisation has finished."
            }
            Retry::Operation => {
                "The cartridge is synchronising with the bucket. Retry once the synchronisation has finished."
            }
        }));
    }
    Ok(())
}

/// A cartridge still to synchronise is never sent. The caller holds the
/// operations lock: a cartridge without its synchronisation running gets it
/// started now, outside the background limit, and the send is refused.
pub async fn require_reconciled(state: &AppState, barcode: &str) -> Result<(), ApiError> {
    require_reconciled_for(state, barcode, Retry::Export).await
}

/// Same, with the retry wording of the refused operation (the export or the operation).
pub async fn require_reconciled_for(
    state: &AppState,
    barcode: &str,
    retry: Retry,
) -> Result<(), ApiError> {
    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM s3_reconcile_required WHERE barcode=? AND vtl_id=?",
    )
    .bind(barcode)
    .bind(state.catalog.vtl_id().await?.to_string())
    .fetch_one(&state.catalog.pool())
    .await
    .map_err(ApiError::conflict)?;
    if pending == 0 {
        return Ok(());
    }
    require_not_synchronising_for(state, barcode, retry).await?;
    crate::operations::start_reconcile_locked(state, barcode.to_owned(), false).await?;
    Err(ApiError::conflict(match retry {
        Retry::Export => format!(
            "The synchronisation of {barcode} has started. Start the export again when it has finished."
        ),
        Retry::Operation => {
            format!("The synchronisation of {barcode} has started. Retry once it has finished.")
        }
    }))
}

/// Synchronise the flagged cartridges without waiting: the returned task runs
/// one pass, and the monitor retries every minute the cartridges in use. The
/// change that called it is already saved, so a failure is only logged.
pub(crate) fn background_synchronisation(state: &AppState) -> tokio::task::JoinHandle<()> {
    let base = state.clone();
    tokio::spawn(async move {
        match base.catalog.vtl_id().await {
            Ok(vtl) => crate::operations::reconcile_vtl(base, vtl.to_string()).await,
            Err(error) => eprintln!("Cartridge synchronisation not started: {error}"),
        }
    })
}

/// Next bucket name for a VTL (`vc-<vtl>-<day>`, then `-2`, `-3`, …), skipping every
/// bucket this catalog already knows plus `remote` (names discovered at the provider).
pub(crate) async fn next_bucket_name(
    state: &AppState,
    vtl: &str,
    remote: &[String],
) -> Result<String, ApiError> {
    let pool = state.catalog.pool();
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM vtls WHERE id=?")
        .bind(vtl)
        .fetch_optional(&pool)
        .await
        .map_err(ApiError::conflict)?;
    let date: String = sqlx::query_scalar("SELECT strftime('%Y-%m-%d','now')")
        .fetch_one(&pool)
        .await
        .map_err(ApiError::conflict)?;
    let known: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT json_extract(identity,'$.bucket') FROM storage_destinations WHERE vtl_id=? AND identity IS NOT NULL
         UNION SELECT next_bucket FROM bucket_creations WHERE vtl_id=?
         UNION SELECT next_bucket FROM bucket_rotations WHERE vtl_id=?",
    )
    .bind(vtl)
    .bind(vtl)
    .bind(vtl)
    .fetch_all(&pool)
    .await
    .map_err(ApiError::conflict)?;
    let taken: Vec<String> = known
        .into_iter()
        .flatten()
        .chain(remote.iter().cloned())
        .collect();
    Ok(cold_vtl_core::suggested_managed_bucket_name(
        name.as_deref().unwrap_or(""),
        &vtl[..8.min(vtl.len())],
        &date,
        |candidate| taken.iter().any(|t| t == candidate),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_ovh_configuration_keeps_its_identity_without_reconciliation() {
        let legacy: Configuration = serde_json::from_value(json!({
            "endpoint": cold_vtl_core::OVH_ARCHIVE_ENDPOINT, "region":"rbx-archive", "bucket":"test-bucket",
            "prefix":"vtl", "access_key_id":"test-key", "secret_access_key":"test-secret"
        })).unwrap();
        assert_eq!(legacy.provider(), S3Provider::OvhColdArchiveV1);
        let mut explicit = legacy.clone();
        explicit.provider = Some(S3Provider::OvhColdArchiveV1);
        assert!(legacy.same_destination(&explicit));
        explicit.provider = Some(S3Provider::S3Compatible);
        assert!(!legacy.same_destination(&explicit));
    }
}
