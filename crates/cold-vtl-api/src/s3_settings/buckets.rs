//! Monthly destination operations. Remote discovery never changes export routing.
use super::*;
use axum::http::StatusCode;
use destinations::Identity;
use sqlx::{Sqlite, Transaction};

/// Job message while the provider is asked about an uncertain creation.
pub(crate) const CHECKING: &str = "Checking the creation. The provider has not answered yet. VaultCenter will try again in 5 minutes.";
pub(crate) const ABSENT: &str = "The bucket was not created. The provider confirms it does not exist. You can start the creation again.";
const RECHECK: Duration = Duration::from_secs(300);
/// An attempt still unreadable this long after entering `uncertain` (its
/// `updated_at`, set on that transition and untouched by checks, which only
/// write `checked_at`) is closed without adoption, releasing the VTL.
const GIVE_UP: &str = "-24 hours";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attach {
    pub revision: i64,
    pub bucket: String,
    pub prefix: Option<String>,
    /// Make this existing bucket the active destination, with the saved connection.
    #[serde(default)]
    pub activate: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub revision: i64,
    pub bucket: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionUpdate {
    revision: i64,
    provider: Option<S3Provider>,
    endpoint: String,
    region: String,
    #[serde(default = "default_prefix")]
    prefix: String,
    access_key_id: String,
    secret_access_key: Option<String>,
}
fn default_prefix() -> String {
    "cold-vtl".into()
}

async fn connection(state: &AppState) -> Result<(i64, Option<Configuration>), ApiError> {
    let (revision, active) = current(state).await?;
    let saved: Option<String> =
        sqlx::query_scalar("SELECT configuration FROM s3_connections WHERE vtl_id=?")
            .bind(state.catalog.vtl_id().await?.to_string())
            .fetch_optional(&state.catalog.pool())
            .await
            .map_err(ApiError::conflict)?;
    Ok((
        revision,
        match saved {
            Some(value) => Some(
                serde_json::from_str(&value)
                    .map_err(|_| ApiError::conflict("Invalid saved connection."))?,
            ),
            None => active,
        },
    ))
}

pub async fn get_connection(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    connection_view(&state).await.map(Json)
}

async fn connection_view(state: &AppState) -> Result<Value, ApiError> {
    let (revision, config) = connection(state).await?;
    let fields = config.as_ref().map(|c|json!({"provider":c.provider(),"endpoint":c.endpoint,"region":c.region,"prefix":c.prefix,"access_key_id":c.access_key_id}));
    let credentials = match &config {
        Some(config) => Some(credentials(state, revision, config).await?),
        None => None,
    };
    Ok(
        json!({"revision":revision,"configured":config.is_some(),"configuration":fields,"secret_configured":config.as_ref().is_some_and(|c| !c.secret_access_key.is_empty()),"credentials":credentials}),
    )
}

/// How long a credential check of one connection revision is reused.
const CREDENTIALS_TTL: Duration = Duration::from_secs(300);

/// Last credential check per catalogue and VTL; a newer revision or an
/// older check is checked again. Never holds keys.
struct Checked {
    revision: i64,
    status: &'static str,
    at: std::time::Instant,
    checked_at: String,
}
type CredentialKey = (std::path::PathBuf, String);
static CREDENTIALS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<CredentialKey, Checked>>,
> = std::sync::LazyLock::new(Default::default);

async fn credential_key(state: &AppState) -> Result<CredentialKey, ApiError> {
    Ok((
        state.catalog.state_dir().to_path_buf(),
        state.catalog.vtl_id().await?.to_string(),
    ))
}

fn cached_credentials(key: &CredentialKey, revision: i64) -> Option<Value> {
    let cache = CREDENTIALS.lock().expect("credential cache poisoned");
    cache
        .get(key)
        .filter(|checked| checked.revision == revision && checked.at.elapsed() < CREDENTIALS_TTL)
        .map(|checked| json!({"status":checked.status,"checked_at":checked.checked_at}))
}

/// The provider's answer to the saved keys, checked at most every 5 minutes
/// per revision: ok, list_denied, invalid, unreachable or unknown.
async fn credentials(
    state: &AppState,
    revision: i64,
    config: &Configuration,
) -> Result<Value, ApiError> {
    let key = credential_key(state).await?;
    if let Some(cached) = cached_credentials(&key, revision) {
        return Ok(cached);
    }
    let status = match credential_status(state, config).await {
        Ok(status) => status,
        Err(error) => {
            eprintln!("Credential check: {}", error.message);
            "unreachable"
        }
    };
    let checked_at: String = sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%SZ','now')")
        .fetch_one(&state.catalog.pool())
        .await
        .map_err(ApiError::conflict)?;
    CREDENTIALS
        .lock()
        .expect("credential cache poisoned")
        .insert(
            key,
            Checked {
                revision,
                status,
                at: std::time::Instant::now(),
                checked_at: checked_at.clone(),
            },
        );
    Ok(json!({"status":status,"checked_at":checked_at}))
}

fn forget_credentials(key: &CredentialKey) {
    CREDENTIALS
        .lock()
        .expect("credential cache poisoned")
        .remove(key);
}

pub async fn save_connection(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<ConnectionUpdate>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    store_connection(&state, request).await.map(Json)
}

async fn store_connection(state: &AppState, request: ConnectionUpdate) -> Result<Value, ApiError> {
    let _lock = state.operations.lock().await;
    let (revision, old) = connection(state).await?;
    let mut config = candidate(
        Update {
            revision: request.revision,
            provider: request.provider,
            endpoint: request.endpoint,
            region: request.region,
            bucket: "connection-validation".into(),
            prefix: request.prefix,
            access_key_id: request.access_key_id,
            secret_access_key: request.secret_access_key,
        },
        revision,
        old.as_ref(),
    )?;
    // A connection is not a storage destination and does not claim any bucket.
    config.bucket.clear();
    let warning = check_credentials(state, &config).await?;
    private_catalog(state).await?;
    let cached = credential_key(state).await?;
    // Anything settlement cannot resolve refuses the save before a probe,
    // so that an activation below can only be followed by a database error.
    settleable_with_keys(state, &config).await?;
    // Accepted keys first settle a creation left uncertain, which would
    // otherwise refuse the save below; an activation moves the revision on.
    let (revision, activated) = settle_with_keys(state, &config, revision).await?;
    if let Err(error) = write_connection(state, &config, revision).await {
        if let Some(id) = activated {
            eprintln!(
                "S3 connection not saved after bucket creation {id} was activated with the new keys; the saved connection still holds the previous keys: {}",
                error.message
            );
        }
        return Err(error);
    }
    forget_credentials(&cached);
    Ok(json!({"status":"saved","revision":revision+1,"warning":warning}))
}

async fn write_connection(
    state: &AppState,
    config: &Configuration,
    revision: i64,
) -> Result<(), ApiError> {
    let (mut tx, vtl) = write_lock(state, revision).await?;
    no_rotation(&mut tx, &vtl).await?;
    sqlx::query("INSERT INTO s3_connections(vtl_id,revision,configuration) VALUES(?,?,?) ON CONFLICT(vtl_id) DO UPDATE SET revision=excluded.revision,configuration=excluded.configuration")
        .bind(&vtl).bind(revision+1).bind(serde_json::to_string(config).map_err(|_|ApiError::conflict("Invalid connection."))?).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    tx.commit().await.map_err(ApiError::conflict)
}

/// Whether keys are for the same provider, endpoint and region.
fn same_endpoint(a: &Configuration, b: &Configuration) -> bool {
    (a.provider(), &a.endpoint, &a.region) == (b.provider(), &b.endpoint, &b.region)
}

/// Refuse a save that settlement with these keys could not unblock: a
/// pending rotation, a creation still running, more than one uncertain
/// creation, or one made for another provider, endpoint or region.
async fn settleable_with_keys(state: &AppState, config: &Configuration) -> Result<(), ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    let pool = state.catalog.pool();
    let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bucket_rotations WHERE vtl_id=?1 AND phase IN ('reserved','creating','ready','uncertain')) OR EXISTS(SELECT 1 FROM bucket_creations WHERE vtl_id=?1 AND phase IN ('reserved','creating'))")
        .bind(&vtl).fetch_one(&pool).await.map_err(ApiError::conflict)?;
    let uncertain: Vec<String> = sqlx::query_scalar(
        "SELECT configuration FROM bucket_creations WHERE vtl_id=? AND phase='uncertain'",
    )
    .bind(&vtl)
    .fetch_all(&pool)
    .await
    .map_err(ApiError::conflict)?;
    let foreign = match uncertain.as_slice() {
        [] => false,
        [snapshot] => !same_endpoint(
            &serde_json::from_str(snapshot)
                .map_err(|_| ApiError::conflict("Invalid saved configuration."))?,
            config,
        ),
        _ => true,
    };
    if blocked || foreign {
        return Err(ApiError::conflict(
            "A bucket creation is still running or has to be checked.",
        ));
    }
    Ok(())
}

#[cfg(test)]
async fn age_credentials(state: &AppState, age: Duration) {
    let key = credential_key(state).await.unwrap();
    let mut cache = CREDENTIALS.lock().unwrap();
    let checked = cache.get_mut(&key).unwrap();
    checked.at = std::time::Instant::now().checked_sub(age).unwrap();
}

/// Settle this VTL's uncertain creations with keys about to be saved (the
/// caller holds the operations lock and has checked them with the provider).
/// Returns the connection revision once settled and the creation activated
/// with these keys, if any. Rotations are left as is.
async fn settle_with_keys(
    state: &AppState,
    config: &Configuration,
    revision: i64,
) -> Result<(i64, Option<String>), ApiError> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT job_id FROM bucket_creations WHERE vtl_id=? AND phase='uncertain'",
    )
    .bind(state.catalog.vtl_id().await?.to_string())
    .fetch_all(&state.catalog.pool())
    .await
    .map_err(ApiError::conflict)?;
    let mut revision = revision;
    let mut activated = None;
    for id in ids {
        if settle_creation(state, &id, None, Some((config, revision))).await?
            == Settlement::Activated
        {
            revision = connection(state).await?.0;
            activated = Some(id);
        }
    }
    Ok((revision, activated))
}

/// Refuse keys the provider rejects before they are saved; keys whose policy
/// only forbids listing are kept with a warning, since creation may still work.
async fn check_credentials(
    state: &AppState,
    config: &Configuration,
) -> Result<Option<&'static str>, ApiError> {
    match credential_status(state, config).await? {
        "ok" => Ok(None),
        "list_denied" => Ok(Some(
            "Credentials accepted, but listing buckets is denied: bucket discovery will be unavailable.",
        )),
        "invalid" => Err(ApiError::bad_request(
            "Credentials rejected by the provider: check the access key and secret key (the access key is not the user ID).",
        )),
        _ => Err(ApiError::bad_request(
            "S3 provider unreachable: check the endpoint and region.",
        )),
    }
}

/// Ask the provider about these keys: ok, list_denied, invalid, unreachable
/// (also after 30 s without answer) or unknown (unreadable answer, logged).
/// A worker that cannot run is an error.
async fn credential_status(
    state: &AppState,
    config: &Configuration,
) -> Result<&'static str, ApiError> {
    let Ok(output) = tokio::time::timeout(
        Duration::from_secs(30),
        connection_worker(state, "check-credentials", None, config),
    )
    .await
    else {
        eprintln!("Credential check: no answer within 30 s");
        return Ok("unreachable");
    };
    let output = output?;
    let status = serde_json::from_slice::<Value>(&output.stdout)
        .ok()
        .and_then(|v| v["status"].as_str().map(str::to_owned));
    Ok(match status.as_deref() {
        Some("ok") => "ok",
        Some("list_denied") => "list_denied",
        Some("invalid") => "invalid",
        Some("unreachable") => "unreachable",
        _ => {
            eprintln!(
                "Credential check: unreadable worker answer (exit {})",
                output.status
            );
            "unknown"
        }
    })
}

fn selected_config(
    old: &Configuration,
    revision: i64,
    request: Attach,
) -> Result<Configuration, ApiError> {
    candidate(
        Update {
            provider: Some(old.provider()),
            revision: request.revision,
            endpoint: old.endpoint.clone(),
            region: old.region.clone(),
            bucket: request.bucket,
            prefix: request.prefix.unwrap_or_else(|| old.prefix.clone()),
            access_key_id: old.access_key_id.clone(),
            secret_access_key: None,
        },
        revision,
        Some(old),
    )
}

/// Whether VaultCenter can create buckets with this VTL's saved connection.
pub(crate) async fn creation_supported(state: &AppState) -> Result<bool, ApiError> {
    Ok(connection(state)
        .await?
        .1
        .is_some_and(|config| config.provider() == S3Provider::OvhColdArchiveV1))
}

async fn configured(state: &AppState) -> Result<(i64, Configuration), ApiError> {
    let (revision, config) = connection(state).await?;
    Ok((
        revision,
        config
            .ok_or_else(|| ApiError::conflict("Configure the S3 connection before continuing."))?,
    ))
}

pub async fn discover(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let (revision, config) = configured(&state).await?;
    let discovery = async {
        let output = connection_worker(&state, "discover-buckets", None, &config).await?;
        if !output.status.success() {
            return Err(ApiError::conflict("Discovery unavailable."));
        }
        serde_json::from_slice::<Value>(&output.stdout)
            .map_err(|_| ApiError::conflict("Invalid discovery result."))
    };
    let value = tokio::time::timeout(Duration::from_secs(30), discovery)
        .await
        .ok()
        .and_then(Result::ok);
    let complete = value.as_ref().is_some_and(|v| v["complete"] == true);
    let rows: Vec<(String, String, bool)> = sqlx::query_as("SELECT id,identity,active FROM storage_destinations WHERE vtl_id=? AND identity IS NOT NULL")
        .bind(state.catalog.vtl_id().await?.to_string()).fetch_all(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    let mut buckets = Vec::new();
    if let Some(names) = value.as_ref().and_then(|v| v["buckets"].as_array()) {
        for name in names.iter().filter_map(Value::as_str) {
            // Treat worker output as untrusted; never reflect arbitrary strings.
            if selected_config(
                &config,
                revision,
                Attach {
                    revision,
                    bucket: name.into(),
                    prefix: None,
                    activate: false,
                },
            )
            .is_err()
            {
                continue;
            }
            let destination = rows.iter().find(|(_, identity, _)| {
                serde_json::from_str::<Value>(identity)
                    .ok()
                    .is_some_and(|v| v["bucket"] == name && v["endpoint"] == config.endpoint)
            });
            buckets.push(json!({"name":name,"attached":destination.is_some(),"destination_id":destination.map(|d|&d.0),"active":destination.is_some_and(|d|d.2)}));
        }
    }
    let vtl = state.catalog.vtl_id().await?.to_string();
    let remote: Vec<String> = value
        .as_ref()
        .and_then(|v| v["buckets"].as_array())
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let suggested = super::next_bucket_name(&state, &vtl, &remote).await?;
    let creations: Vec<(String, String, String, Option<String>)> = sqlx::query_as("SELECT c.job_id,c.next_bucket,c.phase,j.message FROM bucket_creations c JOIN jobs j ON j.id=c.job_id WHERE c.vtl_id=? ORDER BY c.created_at DESC,c.job_id DESC LIMIT 100")
        .bind(&vtl).fetch_all(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    let creations: Vec<Value> = creations
        .into_iter()
        .map(|(job_id, bucket, phase, message)| json!({"job_id":job_id,"bucket":bucket,"phase":phase,"message":message}))
        .collect();
    Ok(Json(
        json!({"buckets":buckets,"creations":creations,"complete":complete,"error":if complete {None} else {Some("Discovery incomplete or denied. You can attach a known bucket manually.")},"manual_attach_available":true,"revision":revision,"suggested_bucket":suggested,"creation_supported":config.provider()==S3Provider::OvhColdArchiveV1}),
    ))
}

async fn write_lock<'a>(
    state: &'a AppState,
    revision: i64,
) -> Result<(Transaction<'a, Sqlite>, String), ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    let mut tx = state
        .catalog
        .pool()
        .begin()
        .await
        .map_err(ApiError::conflict)?;
    sqlx::query("UPDATE vtls SET provisioning_revision=provisioning_revision WHERE id=?")
        .bind(&vtl)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    let actual: i64 = sqlx::query_scalar("SELECT MAX(COALESCE((SELECT MAX(r.revision) FROM storage_destination_revisions r JOIN storage_destinations d ON d.id=r.destination_id WHERE d.vtl_id=?1),0),COALESCE((SELECT revision FROM s3_connections WHERE vtl_id=?1),0))")
        .bind(&vtl).fetch_one(&mut *tx).await.map_err(ApiError::conflict)?;
    if actual != revision {
        return Err(ApiError::conflict(
            "The S3 configuration has changed. Reload it.",
        ));
    }
    Ok((tx, vtl))
}

async fn collision(
    tx: &mut Transaction<'_, Sqlite>,
    vtl: &str,
    config: &Configuration,
) -> Result<Option<String>, ApiError> {
    let identity = Identity::from_config(config)?;
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id,vtl_id,identity FROM storage_destinations WHERE identity IS NOT NULL",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(ApiError::conflict)?;
    for (id, owner, stored) in rows {
        let other: Identity = serde_json::from_str(&stored)
            .map_err(|_| ApiError::conflict("Invalid saved identity."))?;
        if owner == vtl && identity == other {
            return Ok(Some(id));
        }
        if identity.overlaps(&other, owner != vtl) {
            return Err(ApiError::conflict(
                "This bucket or prefix already belongs to another destination.",
            ));
        }
    }
    Ok(None)
}

async fn no_rotation(tx: &mut Transaction<'_, Sqlite>, vtl: &str) -> Result<(), ApiError> {
    let pending: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bucket_rotations WHERE vtl_id=?1 AND phase IN ('reserved','creating','ready','uncertain')) OR EXISTS(SELECT 1 FROM bucket_creations WHERE vtl_id=?1 AND phase IN ('reserved','creating','uncertain'))")
        .bind(vtl).fetch_one(&mut **tx).await.map_err(ApiError::conflict)?;
    if pending {
        return Err(ApiError::conflict(
            "A bucket creation is still running or has to be checked.",
        ));
    }
    Ok(())
}

async fn insert_destination(
    tx: &mut Transaction<'_, Sqlite>,
    vtl: &str,
    config: &Configuration,
    revision: i64,
) -> Result<String, ApiError> {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO storage_destinations(id,vtl_id,identity,active) VALUES(?,?,?,0)")
        .bind(&id)
        .bind(vtl)
        .bind(serde_json::to_string(&Identity::from_config(config)?).map_err(ApiError::conflict)?)
        .execute(&mut **tx)
        .await
        .map_err(ApiError::conflict)?;
    sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,?,?)")
        .bind(&id).bind(revision).bind(serde_json::to_string(config).map_err(|_| ApiError::conflict("Invalid configuration."))?)
        .execute(&mut **tx).await.map_err(ApiError::conflict)?;
    Ok(id)
}

pub async fn attach(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<Attach>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let _lock = state.operations.lock().await;
    let (revision, old) = configured(&state).await?;
    let activate = request.activate;
    let config = selected_config(&old, revision, request)?;
    probe(&state, &config).await?;
    private_catalog(&state).await?;
    if activate {
        let value = activate_existing(&state, &config, revision, &principal.user.username).await?;
        super::background_synchronisation(&state);
        return Ok(Json(value));
    }
    Ok(Json(persist_attachment(&state, &config, revision).await?))
}

/// Use an existing bucket as the active destination. Its content is unknown:
/// every cartridge is flagged, and the caller starts their synchronisation in
/// the background. Like any manual change, it serves the current period.
#[allow(private_interfaces)]
pub(crate) async fn activate_existing(
    state: &AppState,
    config: &Configuration,
    revision: i64,
    actor: &str,
) -> Result<Value, ApiError> {
    let changed = current(state)
        .await?
        .1
        .is_none_or(|active| !active.same_destination(config));
    super::idle(state, changed).await?;
    {
        let (mut tx, vtl) = write_lock(state, revision).await?;
        no_rotation(&mut tx, &vtl).await?;
        tx.rollback().await.map_err(ApiError::conflict)?;
    }
    let id = destinations::save(state, config, revision, changed, actor).await?;
    Ok(json!({"status":"activated","destination_id":id,"revision":revision+1,"active":true}))
}

async fn persist_attachment(
    state: &AppState,
    config: &Configuration,
    revision: i64,
) -> Result<Value, ApiError> {
    let (mut tx, vtl) = write_lock(state, revision).await?;
    no_rotation(&mut tx, &vtl).await?;
    let existing = collision(&mut tx, &vtl, config).await?;
    let (id, revision) = match existing {
        Some(id) => {
            // Attaching a retired bucket again lists it again.
            sqlx::query("UPDATE storage_destinations SET retired_at=NULL WHERE id=?")
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(ApiError::conflict)?;
            let saved: String = sqlx::query_scalar("SELECT configuration FROM storage_destination_revisions WHERE destination_id=? ORDER BY revision DESC LIMIT 1")
                .bind(&id).fetch_one(&mut *tx).await.map_err(ApiError::conflict)?;
            let old: Configuration = serde_json::from_str(&saved)
                .map_err(|_| ApiError::conflict("Invalid saved configuration."))?;
            if old.same_destination(config)
                && old.access_key_id == config.access_key_id
                && old.secret_access_key == config.secret_access_key
            {
                (id, revision)
            } else {
                sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,?,?)")
                    .bind(&id).bind(revision+1).bind(serde_json::to_string(config).map_err(|_|ApiError::conflict("Invalid configuration."))?)
                    .execute(&mut *tx).await.map_err(ApiError::conflict)?;
                (id, revision + 1)
            }
        }
        None => (
            insert_destination(&mut tx, &vtl, config, revision + 1).await?,
            revision + 1,
        ),
    };
    let active: bool = sqlx::query_scalar("SELECT active FROM storage_destinations WHERE id=?")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    if active && vtl == destinations::LEGACY {
        sqlx::query("INSERT INTO s3_settings(id,revision,configuration,actor) VALUES(1,?,?,'bucket_attach') ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,configuration=excluded.configuration,actor=excluded.actor,updated_at=CURRENT_TIMESTAMP")
            .bind(revision).bind(serde_json::to_string(config).map_err(|_|ApiError::conflict("Invalid configuration."))?).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    }
    tx.commit().await.map_err(ApiError::conflict)?;
    Ok(json!({"status":"attached","destination_id":id,"revision":revision,"active":active}))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<Create>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&principal)?;
    let (id, config, revision) = reserve_creation(&state, request.revision, request.bucket).await?;
    let job = state
        .catalog
        .job(&id)
        .await?
        .ok_or_else(|| ApiError::conflict("Job not found."))?;
    tokio::spawn(
        async move { finish_creation(&state, &id, &config, revision, Origin::Manual).await },
    );
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::to_value(job).map_err(ApiError::conflict)?),
    ))
}

/// Validate a creation and persist its job before any provider call.
async fn reserve_creation(
    state: &AppState,
    seen_revision: i64,
    bucket: String,
) -> Result<(String, Configuration, i64), ApiError> {
    let _lock = state.operations.lock().await;
    let (revision, old) = configured(state).await?;
    let config = selected_config(
        &old,
        revision,
        Attach {
            revision: seen_revision,
            bucket,
            prefix: None,
            activate: false,
        },
    )?;
    if config.provider() != S3Provider::OvhColdArchiveV1 {
        return Err(ApiError::bad_request(
            "Creation is available for OVH Cold Archive v1 only.",
        ));
    }
    if !cold_vtl_core::valid_managed_bucket_name(&config.bucket) {
        return Err(ApiError::bad_request(
            "This bucket name is not valid. After the vc- prefix, use only lowercase letters, digits or hyphens.",
        ));
    }
    private_catalog(state).await?;
    let pin = state.catalog.destination().await.ok();
    let (mut tx, vtl) = write_lock(state, revision).await?;
    no_rotation(&mut tx, &vtl).await?;
    if collision(&mut tx, &vtl, &config).await?.is_some() {
        return Err(ApiError::conflict(
            "This bucket is already attached. Creation refused.",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO jobs(id,kind,subject,status,progress,vtl_id,destination_id,configuration_revision) VALUES(?,'bucket_create',?,'queued',0,?,?,?)")
        .bind(&id).bind(&config.bucket).bind(&vtl).bind(pin.as_ref().map(|p|&p.id)).bind(pin.as_ref().and_then(|p|p.revision)).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    sqlx::query("INSERT INTO bucket_creations(job_id,vtl_id,source_destination_id,next_bucket,configuration,revision,phase) VALUES(?,?,?,?,?,?,'reserved')")
        .bind(&id).bind(&vtl).bind(pin.as_ref().map(|p|&p.id)).bind(&config.bucket).bind(serde_json::to_string(&config).map_err(|_|ApiError::conflict("Invalid configuration."))?).bind(revision).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    tx.commit().await.map_err(ApiError::conflict)?;
    Ok((id, config, revision))
}

/// Who asked for a creation: an administrator, or the schedule at an export.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Manual,
    Schedule,
}

/// Run a reserved creation to its end. An ambiguous outcome is checked with
/// the provider at once, then by `monitor`; the creation is never re-sent.
/// Returns `None` when the provider answered (bucket activated or creation
/// refused, see the job), otherwise the outcome of the check (`Pending` when
/// it could not be recorded or run).
async fn finish_creation(
    state: &AppState,
    id: &str,
    config: &Configuration,
    revision: i64,
    origin: Origin,
) -> Option<Settlement> {
    if run_create(state, id, config, revision, origin)
        .await
        .is_ok()
    {
        return None;
    }
    if let Err(error) = mark_checking(state, id).await {
        eprintln!(
            "Bucket creation {id}: pending check not recorded: {}",
            error.message
        );
        return Some(Settlement::Pending);
    }
    match settle(state, id, Some(origin)).await {
        Ok(settlement) => Some(settlement),
        Err(error) => {
            eprintln!("Bucket creation {id}: check failed: {}", error.message);
            Some(Settlement::Pending)
        }
    }
}

/// Create and activate the bucket of a new period, waiting for the outcome.
/// A creation that ends without an active bucket refuses the export with a
/// complete sentence: not created (provider refusal, bucket absent), not
/// usable (the bucket exists but was not activated), or still to be checked.
/// Errors raised before the creation started are returned as is.
pub(crate) async fn create_for_schedule(state: &AppState) -> Result<String, ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    let (revision, _) = configured(state).await?;
    let bucket = super::next_bucket_name(state, &vtl, &[]).await?;
    let (id, config, revision) = reserve_creation(state, revision, bucket.clone()).await?;
    let checked = finish_creation(state, &id, &config, revision, Origin::Schedule).await;
    let job = state
        .catalog
        .job(&id)
        .await?
        .ok_or_else(|| ApiError::conflict("Job not found."))?;
    let message = job.message.unwrap_or_default();
    match (job.status.as_str(), checked) {
        ("completed", _) => Ok(bucket),
        // Refused by the provider (job written by `run_create`) or confirmed absent.
        ("failed", None | Some(Settlement::Absent)) => {
            let reason = creation_refusal_reason(&message);
            Err(ApiError::conflict(format!(
                "The new period's bucket was not created. {reason} The export was not started."
            )))
        }
        // Not activated, occupied, expired, or closed by a concurrent check.
        ("failed", Some(_)) => Err(ApiError::conflict(format!(
            "The new period's bucket could not be used. {message} The export was not started."
        ))),
        _ => Err(ApiError::conflict(
            "The creation of the new period's bucket still has to be checked. The export was not started.",
        )),
    }
}

async fn mark_checking(state: &AppState, id: &str) -> Result<(), ApiError> {
    let mut tx = state
        .catalog
        .pool()
        .begin()
        .await
        .map_err(ApiError::conflict)?;
    let changed = sqlx::query("UPDATE bucket_creations SET phase='uncertain',updated_at=CURRENT_TIMESTAMP WHERE job_id=? AND phase IN ('reserved','creating')")
        .bind(id).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    if changed.rows_affected() == 1 {
        sqlx::query("UPDATE jobs SET status='running',progress=50,message=?,updated_at=CURRENT_TIMESTAMP WHERE id=?")
            .bind(CHECKING).bind(id).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    }
    tx.commit().await.map_err(ApiError::conflict)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Settlement {
    Activated,
    Absent,
    Occupied,
    NotActivated,
    /// Rotation only: the next bucket exists, empty; it stays attached, unused.
    Released,
    /// Unreadable for 24 hours: closed without adoption.
    Expired,
    Pending,
    Settled,
}

/// What the provider says about a name whose creation is uncertain.
enum Found {
    Absent,
    /// Exists, empty and writable.
    Empty,
    /// Exists with content: never adopted.
    Occupied,
    /// Unreadable answer: the attempt stays pending.
    Unknown,
}

/// The decision shared by creations and rotations: presence first, then
/// emptiness only for an existing bucket. Both probes are read-only.
async fn inspect(state: &AppState, bucket: &str, config: &Configuration) -> Found {
    match probe_status(state, "bucket-presence", bucket, config)
        .await
        .as_deref()
    {
        Some("absent") => Found::Absent,
        Some("exists") => match probe_status(state, "confirm-empty-bucket", bucket, config)
            .await
            .as_deref()
        {
            Some("empty") => Found::Empty,
            Some("occupied") => Found::Occupied,
            _ => Found::Unknown,
        },
        _ => Found::Unknown,
    }
}

/// True only when the provider answers that `bucket` does not exist.
pub(super) async fn confirmed_absent(
    state: &AppState,
    bucket: &str,
    config: &Configuration,
) -> bool {
    probe_status(state, "bucket-presence", bucket, config)
        .await
        .as_deref()
        == Some("absent")
}

/// Settle one uncertain creation by asking the provider about its name.
/// Absent: the attempt fails with its reason. Present, empty and writable:
/// the creation is finished and the bucket activated (its random suffix was
/// reserved in the catalogue). Present with content: never adopted.
/// Unreadable provider: stays pending for the next check. `origin` is known
/// only when the check follows the creation at once (`None` in the
/// background): it decides what an occupied name tells the reader to do.
pub(crate) async fn settle(
    state: &AppState,
    id: &str,
    origin: Option<Origin>,
) -> Result<Settlement, ApiError> {
    settle_creation(state, id, origin, None).await
}

/// `settle`, or with `offered` keys about to be saved at a connection
/// revision (the caller holds the operations lock): the provider is asked
/// with them and an empty bucket is activated with them at that revision.
/// Keys for another provider, endpoint or region leave the attempt pending.
async fn settle_creation(
    state: &AppState,
    id: &str,
    origin: Option<Origin>,
    offered: Option<(&Configuration, i64)>,
) -> Result<Settlement, ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    let row: Option<(String, String, i64, bool)> = sqlx::query_as("SELECT next_bucket,configuration,revision,updated_at<=datetime('now',?) FROM bucket_creations WHERE job_id=? AND vtl_id=? AND phase='uncertain'")
        .bind(GIVE_UP).bind(id).bind(&vtl).fetch_optional(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    let Some((bucket, configuration, revision, expired)) = row else {
        return Ok(Settlement::Settled);
    };
    let mut config: Configuration = serde_json::from_str(&configuration)
        .map_err(|_| ApiError::conflict("Invalid saved configuration."))?;
    let mut revision = revision;
    if let Some((keys, current)) = offered {
        if !same_endpoint(keys, &config) {
            return Ok(Settlement::Pending);
        }
        config.access_key_id = keys.access_key_id.clone();
        config.secret_access_key = keys.secret_access_key.clone();
        revision = current;
    }
    sqlx::query("UPDATE bucket_creations SET checked_at=CURRENT_TIMESTAMP WHERE job_id=?")
        .bind(id)
        .execute(&state.catalog.pool())
        .await
        .map_err(ApiError::conflict)?;
    let (settlement, phase, message) = match inspect(state, &bucket, &config).await {
        Found::Absent => (Settlement::Absent, "abandoned", ABSENT.to_owned()),
        Found::Empty => match if offered.is_some() {
            activate_locked(state, id, &config, revision).await
        } else {
            activate_created(state, id, &config, revision).await
        } {
            Ok(()) => return Ok(Settlement::Activated),
            Err(error) => (
                Settlement::NotActivated,
                "abandoned",
                format!(
                    "The bucket {bucket} was created empty, but it was not activated. {} You can use it from the advanced options.",
                    error.message
                ),
            ),
        },
        Found::Occupied => (
            Settlement::Occupied,
            "occupied",
            format!(
                "The bucket {bucket} already exists and contains data. It will not be used.{}",
                match origin {
                    Some(Origin::Manual) => " Choose another name.",
                    Some(Origin::Schedule) => " The next export will propose a new name.",
                    None => "",
                }
            ),
        ),
        Found::Unknown if expired => (
            Settlement::Expired,
            "abandoned",
            format!(
                "The creation could not be checked for 24 hours. The bucket {bucket} was not used. Check it at OVH, then start the creation again."
            ),
        ),
        Found::Unknown => return Ok(Settlement::Pending),
    };
    if close_creation(state, id, phase, &message).await? {
        Ok(settlement)
    } else {
        // Settled meanwhile by a concurrent check.
        Ok(Settlement::Settled)
    }
}

/// Close a still-uncertain creation; an attempt settled meanwhile (by a
/// concurrent check) keeps its job untouched and `false` is returned.
async fn close_creation(
    state: &AppState,
    id: &str,
    phase: &str,
    message: &str,
) -> Result<bool, ApiError> {
    let mut tx = state
        .catalog
        .pool()
        .begin()
        .await
        .map_err(ApiError::conflict)?;
    let changed = sqlx::query("UPDATE bucket_creations SET phase=?,updated_at=CURRENT_TIMESTAMP WHERE job_id=? AND phase='uncertain'")
        .bind(phase).bind(id).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    let closed = changed.rows_affected() == 1;
    if closed {
        sqlx::query("UPDATE jobs SET status='failed',progress=100,message=?,updated_at=CURRENT_TIMESTAMP WHERE id=?")
            .bind(message).bind(id).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    }
    tx.commit().await.map_err(ApiError::conflict)?;
    Ok(closed)
}

/// Read-only probe; any failure, timeout or malformed output means "unknown"
/// and its cause is logged (never the configuration or worker output).
async fn probe_status(
    state: &AppState,
    operation: &str,
    bucket: &str,
    config: &Configuration,
) -> Option<String> {
    let output = match tokio::time::timeout(
        Duration::from_secs(60),
        connection_worker(state, operation, Some(bucket), config),
    )
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            eprintln!("Bucket check {operation} {bucket}: {}", error.message);
            return None;
        }
        Err(_) => {
            eprintln!("Bucket check {operation} {bucket}: no answer within 60 s");
            return None;
        }
    };
    let status = serde_json::from_slice::<Value>(&output.stdout)
        .ok()
        .and_then(|value| value["status"].as_str().map(str::to_owned));
    let expected: &[&str] = match operation {
        "bucket-presence" => &["exists", "absent"],
        _ => &["empty", "occupied"],
    };
    match status {
        Some(status) if expected.contains(&status.as_str()) => Some(status),
        Some(status) if status == "unknown" => {
            eprintln!("Bucket check {operation} {bucket}: provider answer unreadable");
            None
        }
        Some(_) => {
            eprintln!("Bucket check {operation} {bucket}: unexpected status");
            None
        }
        None => {
            eprintln!(
                "Bucket check {operation} {bucket}: malformed worker output (exit {})",
                output.status
            );
            None
        }
    }
}

/// Settle a next bucket that an archive (archive-controller.py
/// `create_next_bucket`) left uncertain. That archive request failed before
/// the OVH submission, so whatever the provider says the rotation is released
/// (`failed`) and the active destination stays unchanged; a bucket with data
/// is detached and never adopted. A rotation whose request may have reached
/// OVH is never touched.
pub(crate) async fn settle_rotation(state: &AppState, id: &str) -> Result<Settlement, ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    type Row = (String, Option<String>, Option<String>, bool, bool);
    let row: Option<Row> = sqlx::query_as("SELECT r.next_bucket,r.next_destination_id,c.configuration,EXISTS(SELECT 1 FROM archive_requests a WHERE a.job_id=r.job_id AND a.phase IN ('queued','submitting','uncertain','accepted','archived')),r.updated_at<=datetime('now',?3) FROM bucket_rotations r JOIN jobs j ON j.id=r.job_id AND j.vtl_id=r.vtl_id LEFT JOIN storage_destination_revisions c ON c.destination_id=j.destination_id AND c.revision=j.configuration_revision WHERE r.job_id=?1 AND r.vtl_id=?2 AND r.phase='uncertain'")
        .bind(id).bind(&vtl).bind(GIVE_UP).fetch_optional(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    let Some((bucket, next, configuration, live, expired)) = row else {
        return Ok(Settlement::Settled);
    };
    if live {
        return Ok(Settlement::Pending);
    }
    let found = match configuration {
        Some(configuration) => {
            let config: Configuration = serde_json::from_str(&configuration)
                .map_err(|_| ApiError::conflict("Invalid saved configuration."))?;
            inspect(state, &bucket, &config).await
        }
        None => {
            // Without the archive's pinned configuration the outcome cannot be read.
            eprintln!("Bucket check {id}: archive configuration revision unavailable");
            Found::Unknown
        }
    };
    let (settlement, detach, message) = match found {
        Found::Absent => (
            Settlement::Absent,
            true,
            format!(
                "The next bucket {bucket} was not created. The provider confirms it does not exist. Start the archive again to get a new name."
            ),
        ),
        Found::Empty => (
            Settlement::Released,
            false,
            format!(
                "The next bucket {bucket} was created empty, but the archive was not sent. Start the archive again. The bucket {bucket} stays attached without being used."
            ),
        ),
        Found::Occupied => (
            Settlement::Occupied,
            true,
            format!(
                "The next bucket {bucket} already exists and contains data. It will not be used. Start the archive again to get a new name."
            ),
        ),
        Found::Unknown if expired => (
            Settlement::Expired,
            true,
            format!(
                "The creation could not be checked for 24 hours. The next bucket {bucket} was not used. Check it at OVH, then start the archive again to get a new name."
            ),
        ),
        Found::Unknown => return Ok(Settlement::Pending),
    };
    let mut tx = state
        .catalog
        .pool()
        .begin()
        .await
        .map_err(ApiError::conflict)?;
    let changed = sqlx::query("UPDATE bucket_rotations SET phase='failed',next_destination_id=CASE WHEN ?2 THEN NULL ELSE next_destination_id END,updated_at=CURRENT_TIMESTAMP WHERE job_id=?1 AND phase='uncertain'")
        .bind(id).bind(detach).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    if changed.rows_affected() != 1 {
        // Settled meanwhile by a concurrent check.
        return Ok(Settlement::Settled);
    }
    sqlx::query("UPDATE jobs SET message=?,updated_at=CURRENT_TIMESTAMP WHERE id=? AND vtl_id=?")
        .bind(&message)
        .bind(id)
        .bind(&vtl)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    if let Some(next) = next.filter(|_| detach) {
        // Never keep a foreign or missing bucket attached; keep any row still referenced.
        macro_rules! unused {
            () => {
                "NOT EXISTS(SELECT 1 FROM jobs WHERE destination_id=?1) AND NOT EXISTS(SELECT 1 FROM remote_generations WHERE destination_id=?1) AND NOT EXISTS(SELECT 1 FROM archive_requests WHERE destination_id=?1) AND NOT EXISTS(SELECT 1 FROM bucket_rotations WHERE source_destination_id=?1 OR next_destination_id=?1) AND NOT EXISTS(SELECT 1 FROM bucket_creations WHERE source_destination_id=?1 OR next_destination_id=?1)"
            };
        }
        sqlx::query(concat!(
            "DELETE FROM storage_destination_revisions WHERE destination_id=?1 AND ",
            unused!()
        ))
        .bind(&next)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
        sqlx::query(concat!(
            "DELETE FROM storage_destinations WHERE id=?1 AND active=0 AND NOT EXISTS(SELECT 1 FROM storage_destination_revisions WHERE destination_id=?1) AND ",
            unused!()
        ))
        .bind(&next)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    }
    tx.commit().await.map_err(ApiError::conflict)?;
    Ok(settlement)
}

/// Check every pending creation and archive rotation of every VTL once.
pub(crate) async fn settle_all(state: &AppState) -> Result<(), ApiError> {
    let rows: Vec<(String, String, bool)> = sqlx::query_as(
        "SELECT job_id,vtl_id,0 FROM bucket_creations WHERE phase='uncertain' UNION ALL SELECT job_id,vtl_id,1 FROM bucket_rotations WHERE phase='uncertain'",
    )
    .fetch_all(&state.catalog.pool())
    .await
    .map_err(ApiError::conflict)?;
    for (id, vtl, rotation) in rows {
        let scoped = match crate::vtls::scoped_state(state.clone(), vtl).await {
            Ok(scoped) => scoped,
            Err(error) => {
                eprintln!("Bucket check {id}: VTL unavailable: {}", error.message);
                continue;
            }
        };
        let settled = if rotation {
            settle_rotation(&scoped, &id).await
        } else {
            settle(&scoped, &id, None).await
        };
        if let Err(error) = settled {
            eprintln!("Bucket check {id}: failed: {}", error.message);
        }
    }
    Ok(())
}

/// Check uncertain creations and rotations at start, then every 5 minutes
/// (a pass with nothing uncertain only runs one query).
pub fn monitor(state: AppState) {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(RECHECK);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) = settle_all(&state).await {
                eprintln!("Bucket checks unavailable: {}", error.message);
            }
        }
    });
}

/// Start of a refused creation's job message, followed by its reason.
const NOT_CREATED: &str = "The bucket was not created. ";
/// `NOT_CREATED` as written by versions before English messages.
const NOT_CREATED_FRENCH: &str = "Le bucket n’a pas été créé. ";

/// The reason of a refused creation's job message, whatever its language.
fn creation_refusal_reason(message: &str) -> &str {
    message
        .strip_prefix(NOT_CREATED)
        .or_else(|| message.strip_prefix(NOT_CREATED_FRENCH))
        .unwrap_or(message)
}

/// Why the provider refused a creation, in the sentences following `NOT_CREATED`.
fn refusal(reason: Option<&str>, origin: Origin) -> &'static str {
    match reason {
        Some("credentials") => {
            "The provider rejected the credentials. Fix the connection, then try again."
        }
        Some("denied") => {
            "The provider denied access. Check the connection keys and the user's S3 policy."
        }
        Some("exists") if origin == Origin::Schedule => {
            "This name is already taken at the provider. The next export will propose a new name."
        }
        Some("exists") => "This name is already taken at the provider. Choose another name.",
        Some("name") => "The provider rejected this name.",
        Some("unreachable") => "The provider is unreachable. Check the endpoint, then try again.",
        _ => "The provider rejected the request.",
    }
}

async fn run_create(
    state: &AppState,
    id: &str,
    config: &Configuration,
    revision: i64,
    origin: Origin,
) -> Result<(), ApiError> {
    state
        .catalog
        .update_job(id, "running", 10, Some("Creating the new bucket."))
        .await?;
    sqlx::query("UPDATE bucket_creations SET phase='creating',updated_at=CURRENT_TIMESTAMP WHERE job_id=? AND phase='reserved'").bind(id).execute(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    let work = connection_worker(
        state,
        "create-bucket-from-connection",
        Some(&config.bucket),
        config,
    );
    let output = tokio::time::timeout(Duration::from_secs(180), work)
        .await
        .map_err(|_| ApiError::conflict("Creation timed out."))??;
    let response: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| ApiError::conflict("Creation not confirmed."))?;
    if response["status"] == "not_created" && response["bucket"] == config.bucket {
        // The provider refused before creating anything: close the attempt, nothing to resolve.
        let message = format!(
            "{NOT_CREATED}{}",
            refusal(response["reason"].as_str(), origin)
        );
        sqlx::query("UPDATE bucket_creations SET phase='abandoned',updated_at=CURRENT_TIMESTAMP WHERE job_id=? AND phase IN ('reserved','creating')").bind(id).execute(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
        state
            .catalog
            .update_job(id, "failed", 100, Some(&message))
            .await?;
        return Ok(());
    }
    if !output.status.success()
        || response["status"] != "created"
        || response["bucket"] != config.bucket
    {
        return Err(ApiError::conflict("Creation not confirmed."));
    }
    activate_created(state, id, config, revision).await
}

async fn activate_created(
    state: &AppState,
    id: &str,
    config: &Configuration,
    revision: i64,
) -> Result<(), ApiError> {
    let _lock = state.operations.lock().await;
    activate_locked(state, id, config, revision).await
}

/// `activate_created` for a caller already holding the operations lock.
async fn activate_locked(
    state: &AppState,
    id: &str,
    config: &Configuration,
    revision: i64,
) -> Result<(), ApiError> {
    let (mut tx, vtl) = write_lock(state, revision).await?;
    let source: Option<Option<String>> = sqlx::query_scalar("SELECT source_destination_id FROM bucket_creations WHERE job_id=? AND vtl_id=? AND phase IN ('creating','uncertain')")
        .bind(id).bind(&vtl).fetch_optional(&mut *tx).await.map_err(ApiError::conflict)?;
    let active: Option<String> =
        sqlx::query_scalar("SELECT id FROM storage_destinations WHERE vtl_id=? AND active=1")
            .bind(&vtl)
            .fetch_optional(&mut *tx)
            .await
            .map_err(ApiError::conflict)?;
    if source != Some(active) || collision(&mut tx, &vtl, config).await?.is_some() {
        return Err(ApiError::conflict("The active destination has changed."));
    }
    let next = insert_destination(&mut tx, &vtl, config, revision + 1).await?;
    sqlx::query("UPDATE storage_destinations SET active=0 WHERE vtl_id=?")
        .bind(&vtl)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    sqlx::query("UPDATE storage_destinations SET active=1 WHERE id=? AND vtl_id=?")
        .bind(&next)
        .bind(&vtl)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    if vtl == destinations::LEGACY {
        sqlx::query("INSERT INTO s3_settings(id,revision,configuration,actor) VALUES(1,?,?,'bucket_create') ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,configuration=excluded.configuration,actor=excluded.actor,updated_at=CURRENT_TIMESTAMP")
            .bind(revision+1).bind(serde_json::to_string(config).map_err(|_|ApiError::conflict("Invalid configuration."))?).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    }
    // The new bucket was created empty (absence confirmed, never adopted):
    // there are no remote copies to find, so exports need no synchronisation.
    sqlx::query("DELETE FROM s3_reconcile_required WHERE vtl_id=?")
        .bind(&vtl)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    sqlx::query("UPDATE bucket_creations SET phase='switched',next_destination_id=?,updated_at=CURRENT_TIMESTAMP WHERE job_id=? AND vtl_id=?").bind(&next).bind(id).bind(&vtl).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    sqlx::query("UPDATE jobs SET status='completed',progress=100,message='Bucket created and activated.',updated_at=CURRENT_TIMESTAMP WHERE id=? AND vtl_id=?").bind(id).bind(&vtl).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    // Synchronisations with the previous bucket are pointless now: they are
    // closed with the flags, then their workers are stopped.
    let cancelled: Vec<(String, String)> = sqlx::query_as("UPDATE jobs SET status='failed',progress=100,message=?,updated_at=CURRENT_TIMESTAMP WHERE vtl_id=? AND kind='reconcile' AND status IN ('queued','running') RETURNING id,subject")
        .bind(crate::operations::SYNC_CANCELLED).bind(&vtl).fetch_all(&mut *tx).await.map_err(ApiError::conflict)?;
    let (ids, barcodes): (Vec<String>, Vec<String>) = cancelled.into_iter().unzip();
    let stops = crate::operations::reconcile_stops(&ids);
    tx.commit().await.map_err(ApiError::conflict)?;
    // Under the operations lock: no new synchronisation of these cartridges
    // starts before their old worker units are stopped.
    crate::operations::stop_reconciles(&vtl, stops, barcodes).await;
    Ok(())
}

async fn connection_worker(
    state: &AppState,
    operation: &str,
    bucket: Option<&str>,
    config: &Configuration,
) -> Result<std::process::Output, ApiError> {
    let mut command = Command::new(&state.config.worker_path);
    command
        .arg(operation)
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .env_remove("AWS_SESSION_TOKEN");
    if let Some(bucket) = bucket {
        command.args(["--bucket", bucket]);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ApiError::conflict("S3 worker unavailable."))?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| ApiError::conflict("Worker input unavailable."))?;
    input
        .write_all(
            &serde_json::to_vec(config)
                .map_err(|_| ApiError::conflict("Invalid configuration."))?,
        )
        .await
        .map_err(|_| ApiError::conflict("Worker connection interrupted."))?;
    drop(input);
    child
        .wait_with_output()
        .await
        .map_err(|_| ApiError::conflict("S3 worker interrupted."))
}

/// A restart never re-sends a creation: every attempt whose outcome is
/// unknown, including those marked uncertain before this version, becomes a
/// pending check that `monitor` settles with the provider.
pub async fn recover_interrupted(state: &AppState) -> Result<(), sqlx::Error> {
    let mut tx = state.catalog.pool().begin().await?;
    sqlx::query("UPDATE bucket_creations SET phase='uncertain',updated_at=CURRENT_TIMESTAMP WHERE phase IN ('reserved','creating')")
        .execute(&mut *tx).await?;
    sqlx::query("UPDATE jobs SET status='running',progress=50,message=?,updated_at=CURRENT_TIMESTAMP WHERE id IN (SELECT job_id FROM bucket_creations WHERE phase='uncertain')")
        .bind(CHECKING).execute(&mut *tx).await?;
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3_settings::test_support::*;

    #[test]
    fn refusal_reason_is_read_from_english_and_older_french_messages() {
        let reason = "The provider rejected this name.";
        assert_eq!(
            creation_refusal_reason(&format!("{NOT_CREATED}{reason}")),
            reason
        );
        assert_eq!(
            creation_refusal_reason("Le bucket n’a pas été créé. Le fournisseur a refusé ce nom."),
            "Le fournisseur a refusé ce nom."
        );
        assert_eq!(creation_refusal_reason(reason), reason);
    }

    #[tokio::test]
    async fn attach_is_inactive_idempotent_and_rejects_foreign_storage_and_stale_revision() {
        let (state, root, mut config) = fixture().await;
        let active = state.catalog.destination().await.unwrap().id;
        config.bucket = "historical-bucket".into();
        let attached = persist_attachment(&state, &config, 1).await.unwrap();
        assert_eq!(attached["active"], false);
        assert_eq!(attached["revision"], 2);
        assert_eq!(state.catalog.destination().await.unwrap().id, active);
        assert_eq!(
            persist_attachment(&state, &config, 2).await.unwrap(),
            attached
        );
        assert!(persist_attachment(&state, &config, 1).await.is_err());
        let foreign = cold_vtl_core::VtlId::new();
        sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'Other','ready')")
            .bind(foreign.to_string())
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        let foreign_state = AppState {
            catalog: state.catalog.for_vtl(foreign).await.unwrap(),
            ..state.clone()
        };
        assert!(
            persist_attachment(&foreign_state, &config, 0)
                .await
                .is_err()
        );
        assert_eq!(state.catalog.destination().await.unwrap().id, active);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn creation_activation_preserves_existing_job_pins_and_old_credentials() {
        let (state, root, mut config) = fixture().await;
        let old = state.catalog.destination().await.unwrap();
        let vtl = state.catalog.vtl_id().await.unwrap().to_string();
        sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id,destination_id,configuration_revision) VALUES('export','export','TEST01','running',?,?,1)").bind(vtl).bind(&old.id).execute(&state.catalog.pool()).await.unwrap();
        config.bucket = "next-bucket".into();
        config.secret_access_key = "new-secret".into();
        reserve(&state, &config).await;
        let cartridge =
            cold_vtl_core::Cartridge::new(cold_vtl_core::Barcode::parse("TEST01").unwrap(), 1024)
                .unwrap();
        state.catalog.create_cartridge(&cartridge).await.unwrap();
        sqlx::query("INSERT INTO s3_reconcile_required(vtl_id,barcode) VALUES(?,'TEST01')")
            .bind(state.catalog.vtl_id().await.unwrap().to_string())
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        activate_created(&state, "create", &config, 1)
            .await
            .unwrap();
        crate::s3_settings::require_reconciled(&state, "TEST01")
            .await
            .unwrap();
        let active = state.catalog.destination().await.unwrap();
        assert_ne!(active.id, old.id);
        assert_eq!(active.revision, Some(2));
        let pinned = state.catalog.job("export").await.unwrap().unwrap();
        assert_eq!(pinned.destination_id, Some(old.id.clone()));
        assert_eq!(pinned.configuration_revision, Some(1));
        assert_eq!(pinned.status, "running");
        let saved:String=sqlx::query_scalar("SELECT configuration FROM storage_destination_revisions WHERE destination_id=? AND revision=1").bind(old.id).fetch_one(&state.catalog.pool()).await.unwrap();
        assert!(saved.contains("private-secret"));
        assert!(!saved.contains("new-secret"));
        assert!(
            activate_created(&state, "create", &config, 1)
                .await
                .is_err()
        );
        assert_eq!(
            state.catalog.job("create").await.unwrap().unwrap().status,
            "completed"
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn reattaching_with_new_credentials_preserves_destination_and_pinned_jobs() {
        let (state, root, mut config) = fixture().await;
        let active = state.catalog.destination().await.unwrap().id;
        let vtl = state.catalog.vtl_id().await.unwrap().to_string();
        config.bucket = "historical-bucket".into();
        let attached = persist_attachment(&state, &config, 1).await.unwrap();
        let destination = attached["destination_id"].as_str().unwrap();
        sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id,destination_id,configuration_revision) VALUES('historical-read','reconcile','TEST01','running',?,?,2)")
            .bind(&vtl).bind(destination).execute(&state.catalog.pool()).await.unwrap();
        config.access_key_id = "replacement-key".into();
        config.secret_access_key = "replacement-secret".into();
        let refreshed = persist_attachment(&state, &config, 2).await.unwrap();
        assert_eq!(refreshed["destination_id"], destination);
        assert_eq!(refreshed["revision"], 3);
        assert_eq!(refreshed["active"], false);
        assert_eq!(state.catalog.destination().await.unwrap().id, active);
        let history = state
            .catalog
            .for_destination(destination.to_owned())
            .await
            .unwrap();
        assert_eq!(history.destination().await.unwrap().revision, Some(3));
        assert_eq!(
            state
                .catalog
                .job("historical-read")
                .await
                .unwrap()
                .unwrap()
                .configuration_revision,
            Some(2)
        );
        let old:String=sqlx::query_scalar("SELECT configuration FROM storage_destination_revisions WHERE destination_id=? AND revision=2").bind(destination).fetch_one(&state.catalog.pool()).await.unwrap();
        let new:String=sqlx::query_scalar("SELECT configuration FROM storage_destination_revisions WHERE destination_id=? AND revision=3").bind(destination).fetch_one(&state.catalog.pool()).await.unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&old).unwrap()["secret_access_key"],
            "private-secret"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&new).unwrap()["secret_access_key"],
            "replacement-secret"
        );
        assert_eq!(
            persist_attachment(&state, &config, 3).await.unwrap(),
            refreshed
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn reattaching_a_retired_bucket_shows_it_again() {
        let (state, root, mut config) = fixture().await;
        config.bucket = "returning-bucket".into();
        let attached = persist_attachment(&state, &config, 1).await.unwrap();
        let destination = attached["destination_id"].as_str().unwrap().to_owned();
        sqlx::query("UPDATE storage_destinations SET retired_at=CURRENT_TIMESTAMP WHERE id=?")
            .bind(&destination)
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        let again = persist_attachment(&state, &config, 2).await.unwrap();
        assert_eq!(again["destination_id"], destination.as_str());
        let retired: Option<String> =
            sqlx::query_scalar("SELECT retired_at FROM storage_destinations WHERE id=?")
                .bind(&destination)
                .fetch_one(&state.catalog.pool())
                .await
                .unwrap();
        assert_eq!(retired, None);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn restart_turns_interrupted_creations_into_pending_checks() {
        let (state, root, mut config) = fixture().await;
        config.bucket = "vc-possibly-created-2026-10-13-abcd".into();
        reserve(&state, &config).await;
        recover_interrupted(&state).await.unwrap();
        assert_eq!(creation_phase(&state, "create").await, "uncertain");
        let job = state.catalog.job("create").await.unwrap().unwrap();
        assert_eq!(job.status, "running");
        assert_eq!(job.message.as_deref(), Some(CHECKING));
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn unreachable_provider_keeps_the_check_pending_and_blocks_other_creations() {
        let (state, root, mut config) = fixture().await;
        let active = state.catalog.destination().await.unwrap().id;
        config.bucket = "vc-possibly-created-2026-10-13-abcd".into();
        reserve(&state, &config).await;
        recover_interrupted(&state).await.unwrap();
        dispatch_worker(&root, "unknown", "empty");
        settle_all(&state).await.unwrap();
        assert_eq!(creation_phase(&state, "create").await, "uncertain");
        assert_eq!(state.catalog.destination().await.unwrap().id, active);
        assert!(persist_attachment(&state, &config, 1).await.is_err());
        assert!(
            destinations::save(&state, &config, 1, true, "admin")
                .await
                .is_err()
        );
        let mut tx = state.catalog.pool().begin().await.unwrap();
        assert!(
            no_rotation(&mut tx, &state.catalog.vtl_id().await.unwrap().to_string())
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();
        assert_eq!(calls(&root), vec!["bucket-presence"]);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn absent_bucket_closes_the_attempt_with_its_reason() {
        let (state, root, mut config) = fixture().await;
        config.bucket = "vc-never-created-2026-10-13-abcd".into();
        reserve(&state, &config).await;
        recover_interrupted(&state).await.unwrap();
        dispatch_worker(&root, "absent", "empty");
        assert_eq!(
            settle(&state, "create", None).await.unwrap(),
            Settlement::Absent
        );
        assert_eq!(creation_phase(&state, "create").await, "abandoned");
        let job = state.catalog.job("create").await.unwrap().unwrap();
        assert_eq!(
            (job.status.as_str(), job.message.as_deref()),
            ("failed", Some(ABSENT))
        );
        let mut tx = state.catalog.pool().begin().await.unwrap();
        no_rotation(&mut tx, &state.catalog.vtl_id().await.unwrap().to_string())
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(
            settle(&state, "create", None).await.unwrap(),
            Settlement::Settled
        );
        assert_eq!(calls(&root), vec!["bucket-presence"]);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn existing_empty_bucket_is_activated_without_resending_the_creation() {
        let (state, root, mut config) = fixture().await;
        config.bucket = "vc-created-2026-10-13-abcd".into();
        reserve(&state, &config).await;
        recover_interrupted(&state).await.unwrap();
        dispatch_worker(&root, "exists", "empty");
        assert_eq!(
            settle(&state, "create", None).await.unwrap(),
            Settlement::Activated
        );
        assert_eq!(creation_phase(&state, "create").await, "switched");
        assert_eq!(
            state.catalog.job("create").await.unwrap().unwrap().status,
            "completed"
        );
        assert_eq!(
            current(&state).await.unwrap().1.unwrap().bucket,
            "vc-created-2026-10-13-abcd"
        );
        assert_eq!(
            calls(&root),
            vec!["bucket-presence", "confirm-empty-bucket"]
        );
        // A concurrent check finishing late never overwrites the activation.
        assert!(
            !close_creation(&state, "create", "abandoned", ABSENT)
                .await
                .unwrap()
        );
        assert_eq!(creation_phase(&state, "create").await, "switched");
        assert_eq!(
            state.catalog.job("create").await.unwrap().unwrap().status,
            "completed"
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn existing_bucket_with_content_is_never_adopted() {
        let (state, root, mut config) = fixture().await;
        let active = state.catalog.destination().await.unwrap().id;
        config.bucket = "vc-taken-2026-10-13-abcd".into();
        reserve(&state, &config).await;
        recover_interrupted(&state).await.unwrap();
        dispatch_worker(&root, "exists", "unknown");
        assert_eq!(
            settle(&state, "create", None).await.unwrap(),
            Settlement::Pending
        );
        assert_eq!(creation_phase(&state, "create").await, "uncertain");
        dispatch_worker(&root, "exists", "occupied");
        assert_eq!(
            settle(&state, "create", None).await.unwrap(),
            Settlement::Occupied
        );
        assert_eq!(creation_phase(&state, "create").await, "occupied");
        let job = state.catalog.job("create").await.unwrap().unwrap();
        assert_eq!(job.status, "failed");
        let message = job.message.unwrap();
        // Checked in the background: who asked is unknown, no instruction.
        assert_eq!(
            message,
            "The bucket vc-taken-2026-10-13-abcd already exists and contains data. It will not be used."
        );
        assert!(
            message.contains("vc-taken-2026-10-13-abcd") && message.contains("contains data"),
            "{message}"
        );
        assert_eq!(state.catalog.destination().await.unwrap().id, active);
        let mut tx = state.catalog.pool().begin().await.unwrap();
        no_rotation(&mut tx, &state.catalog.vtl_id().await.unwrap().to_string())
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        assert!(!calls(&root).iter().any(|c| c.starts_with("create-bucket")));
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn lost_creation_response_is_checked_at_once() {
        let (state, root, mut config) = fixture().await;
        config.bucket = "vc-lost-2026-10-13-abcd".into();
        reserve(&state, &config).await;
        dispatch_worker(&root, "absent", "empty");
        finish_creation(&state, "create", &config, 1, Origin::Manual).await;
        assert_eq!(creation_phase(&state, "create").await, "abandoned");
        assert_eq!(
            calls(&root),
            vec!["create-bucket-from-connection", "bucket-presence"]
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn empty_bucket_that_cannot_be_activated_is_closed_with_its_reason() {
        let (state, root, mut config) = fixture().await;
        let active = state.catalog.destination().await.unwrap().id;
        config.bucket = "vc-orphan-2026-10-13-abcd".into();
        reserve(&state, &config).await;
        recover_interrupted(&state).await.unwrap();
        // The active destination changed since the attempt was reserved.
        sqlx::query("UPDATE bucket_creations SET source_destination_id=NULL WHERE job_id='create'")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        dispatch_worker(&root, "exists", "empty");
        assert_eq!(
            settle(&state, "create", None).await.unwrap(),
            Settlement::NotActivated
        );
        assert_eq!(creation_phase(&state, "create").await, "abandoned");
        let job = state.catalog.job("create").await.unwrap().unwrap();
        assert_eq!(job.status, "failed");
        let message = job.message.unwrap();
        assert!(
            message.contains("vc-orphan-2026-10-13-abcd") && message.contains("created empty"),
            "{message}"
        );
        assert_eq!(state.catalog.destination().await.unwrap().id, active);
        assert_eq!(
            settle(&state, "create", None).await.unwrap(),
            Settlement::Settled
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn checks_unreadable_for_a_day_release_the_vtl_without_adoption() {
        let (state, root, mut config) = fixture().await;
        let active = state.catalog.destination().await.unwrap().id;
        let vtl = state.catalog.vtl_id().await.unwrap().to_string();
        config.bucket = "vc-silent-2026-10-13-abcd".into();
        reserve(&state, &config).await;
        recover_interrupted(&state).await.unwrap();
        dispatch_worker(&root, "unknown", "unknown");
        // Uncertain for less than a day: still pending, VTL still blocked.
        sqlx::query("UPDATE bucket_creations SET updated_at=datetime('now','-23 hours') WHERE job_id='create'")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        settle_all(&state).await.unwrap();
        assert_eq!(creation_phase(&state, "create").await, "uncertain");
        let mut tx = state.catalog.pool().begin().await.unwrap();
        assert!(no_rotation(&mut tx, &vtl).await.is_err());
        tx.rollback().await.unwrap();
        // Uncertain for more than a day: closed, never adopted.
        sqlx::query("UPDATE bucket_creations SET updated_at=datetime('now','-25 hours') WHERE job_id='create'")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        settle_all(&state).await.unwrap();
        assert_eq!(creation_phase(&state, "create").await, "abandoned");
        let job = state.catalog.job("create").await.unwrap().unwrap();
        assert_eq!(job.status, "failed");
        let message = job.message.unwrap();
        assert!(
            message.contains("24 hours")
                && message.contains("vc-silent-2026-10-13-abcd")
                && message.contains("OVH"),
            "{message}"
        );
        assert_eq!(state.catalog.destination().await.unwrap().id, active);
        let mut tx = state.catalog.pool().begin().await.unwrap();
        no_rotation(&mut tx, &vtl).await.unwrap();
        tx.rollback().await.unwrap();
        assert!(calls(&root).iter().all(|c| c == "bucket-presence"));
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn rotation_unreadable_for_a_day_is_released_and_detached() {
        let (state, root, _) = fixture().await;
        let active = state.catalog.destination().await.unwrap().id;
        let next = uncertain_rotation(&state, "vc-silent-2026-10-13-abcd").await;
        dispatch_worker(&root, "unknown", "unknown");
        assert_eq!(
            settle_rotation(&state, "archive").await.unwrap(),
            Settlement::Pending
        );
        assert_eq!(
            rotation_phase(&state).await,
            ("uncertain".into(), Some(next.clone()))
        );
        sqlx::query("UPDATE bucket_rotations SET updated_at=datetime('now','-25 hours') WHERE job_id='archive'")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        assert_eq!(
            settle_rotation(&state, "archive").await.unwrap(),
            Settlement::Expired
        );
        assert_eq!(rotation_phase(&state).await, ("failed".into(), None));
        assert!(!destination_exists(&state, &next).await);
        let message = archive_message(&state).await;
        assert!(
            message.contains("24 hours") && message.contains("OVH"),
            "{message}"
        );
        assert_eq!(state.catalog.destination().await.unwrap().id, active);
        let mut tx = state.catalog.pool().begin().await.unwrap();
        no_rotation(&mut tx, &state.catalog.vtl_id().await.unwrap().to_string())
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    async fn destination_exists(state: &AppState, id: &str) -> bool {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM storage_destinations WHERE id=?)")
            .bind(id)
            .fetch_one(&state.catalog.pool())
            .await
            .unwrap()
    }

    async fn archive_message(state: &AppState) -> String {
        let job = state.catalog.job("archive").await.unwrap().unwrap();
        assert_eq!(job.status, "failed");
        job.message.unwrap_or_default()
    }

    #[tokio::test]
    async fn absent_rotation_bucket_is_released_and_detached() {
        let (state, root, _) = fixture().await;
        let active = state.catalog.destination().await.unwrap().id;
        let next = uncertain_rotation(&state, "vc-next-2026-10-13-abcd").await;
        dispatch_worker(&root, "absent", "empty");
        settle_all(&state).await.unwrap();
        assert_eq!(rotation_phase(&state).await, ("failed".into(), None));
        assert!(!destination_exists(&state, &next).await);
        let message = archive_message(&state).await;
        assert!(
            message.contains("vc-next-2026-10-13-abcd") && message.contains("does not exist"),
            "{message}"
        );
        assert_eq!(state.catalog.destination().await.unwrap().id, active);
        let mut tx = state.catalog.pool().begin().await.unwrap();
        no_rotation(&mut tx, &state.catalog.vtl_id().await.unwrap().to_string())
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        assert_eq!(calls(&root), vec!["bucket-presence"]);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn empty_rotation_bucket_is_released_and_kept_unused() {
        let (state, root, _) = fixture().await;
        let active = state.catalog.destination().await.unwrap().id;
        let next = uncertain_rotation(&state, "vc-next-2026-10-13-abcd").await;
        dispatch_worker(&root, "exists", "empty");
        assert_eq!(
            settle_rotation(&state, "archive").await.unwrap(),
            Settlement::Released
        );
        assert_eq!(
            rotation_phase(&state).await,
            ("failed".into(), Some(next.clone()))
        );
        assert!(destination_exists(&state, &next).await);
        assert!(archive_message(&state).await.contains("created empty"));
        // The archive was never sent: the active bucket does not change.
        assert_eq!(state.catalog.destination().await.unwrap().id, active);
        assert_eq!(
            calls(&root),
            vec!["bucket-presence", "confirm-empty-bucket"]
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn uncertain_rotation_with_content_is_never_adopted() {
        let (state, root, _) = fixture().await;
        let active = state.catalog.destination().await.unwrap().id;
        let next = uncertain_rotation(&state, "vc-taken-2026-10-13-abcd").await;
        dispatch_worker(&root, "exists", "occupied");
        assert_eq!(
            settle_rotation(&state, "archive").await.unwrap(),
            Settlement::Occupied
        );
        assert_eq!(rotation_phase(&state).await, ("failed".into(), None));
        assert!(!destination_exists(&state, &next).await);
        let message = archive_message(&state).await;
        assert!(
            message.contains("contains data") && message.contains("to get a new name"),
            "{message}"
        );
        assert_eq!(state.catalog.destination().await.unwrap().id, active);
        // The occupied name stays recorded (next_bucket_name skips it) and no
        // destination ever points at it; only the read-only probes ran.
        let recorded: String =
            sqlx::query_scalar("SELECT next_bucket FROM bucket_rotations WHERE job_id='archive'")
                .fetch_one(&state.catalog.pool())
                .await
                .unwrap();
        assert_eq!(recorded, "vc-taken-2026-10-13-abcd");
        let attached: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM storage_destinations WHERE json_extract(identity,'$.bucket')='vc-taken-2026-10-13-abcd')")
            .fetch_one(&state.catalog.pool())
            .await
            .unwrap();
        assert!(!attached);
        assert_eq!(
            calls(&root),
            vec!["bucket-presence", "confirm-empty-bucket"]
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn uncertain_rotation_waits_while_unreadable_or_while_its_archive_is_live() {
        let (state, root, _) = fixture().await;
        let next = uncertain_rotation(&state, "vc-next-2026-10-13-abcd").await;
        dispatch_worker(&root, "unknown", "empty");
        assert_eq!(
            settle_rotation(&state, "archive").await.unwrap(),
            Settlement::Pending
        );
        dispatch_worker(&root, "exists", "unknown");
        assert_eq!(
            settle_rotation(&state, "archive").await.unwrap(),
            Settlement::Pending
        );
        assert_eq!(
            rotation_phase(&state).await,
            ("uncertain".into(), Some(next.clone()))
        );
        let mut tx = state.catalog.pool().begin().await.unwrap();
        assert!(
            no_rotation(&mut tx, &state.catalog.vtl_id().await.unwrap().to_string())
                .await
                .is_err()
        );
        tx.rollback().await.unwrap();
        // A request that may have reached OVH is never touched here.
        sqlx::query("UPDATE archive_requests SET phase='accepted' WHERE job_id='archive'")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        std::fs::remove_file(root.join("calls")).ok();
        dispatch_worker(&root, "absent", "empty");
        assert_eq!(
            settle_rotation(&state, "archive").await.unwrap(),
            Settlement::Pending
        );
        assert_eq!(rotation_phase(&state).await.0, "uncertain");
        assert!(calls(&root).is_empty());
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn bootstrap_connection_has_no_bucket_and_does_not_change_active_credentials() {
        let (state, root, mut config) = fixture().await;
        config.bucket.clear();
        config.secret_access_key = "connection-secret".into();
        sqlx::query("INSERT INTO s3_connections(vtl_id,revision,configuration) VALUES(?,2,?)")
            .bind(state.catalog.vtl_id().await.unwrap().to_string())
            .bind(serde_json::to_string(&config).unwrap())
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        let (revision, connection) = configured(&state).await.unwrap();
        assert_eq!(revision, 2);
        assert!(connection.bucket.is_empty());
        assert_eq!(connection.secret_access_key, "connection-secret");
        let (revision, active) = current(&state).await.unwrap();
        assert_eq!(revision, 2);
        assert_eq!(active.unwrap().secret_access_key, "private-secret");
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rejected_credentials_are_never_saved_and_list_denial_only_warns() {
        let (state, root, config) = fixture().await;
        fake_worker(&root, r#"{"status":"invalid"}"#, 0);
        assert!(check_credentials(&state, &config).await.is_err());
        fake_worker(&root, r#"{"status":"unreachable"}"#, 0);
        assert!(check_credentials(&state, &config).await.is_err());
        fake_worker(&root, r#"{"status":"list_denied"}"#, 0);
        assert!(check_credentials(&state, &config).await.unwrap().is_some());
        fake_worker(&root, r#"{"status":"ok"}"#, 0);
        assert!(check_credentials(&state, &config).await.unwrap().is_none());
        std::fs::remove_dir_all(root).ok();
    }

    /// Worker answering the credential check and both probes; each call logs
    /// its command and the access key it received to `root/calls`.
    fn keyed_worker(root: &std::path::Path, credentials: &str, presence: &str, emptiness: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            root.join("worker"),
            format!(
                "#!/bin/sh\ninput=$(cat)\nkey=$(printf '%s' \"$input\" | sed 's/.*\"access_key_id\":\"\\([^\"]*\\)\".*/\\1/')\necho \"$1 $key\" >> '{calls}'\ncase \"$1\" in\n  check-credentials) echo '{{\"status\":\"{credentials}\"}}' ;;\n  bucket-presence) echo '{{\"status\":\"{presence}\"}}' ;;\n  confirm-empty-bucket) echo '{{\"status\":\"{emptiness}\"}}' ;;\n  *) echo lost; exit 1 ;;\nesac\n",
                calls = root.join("calls").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(root.join("worker"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }

    /// The saved connection (revision 2, old keys) and an uncertain creation.
    async fn uncertain_with_connection(state: &AppState, config: &Configuration) {
        let mut connection = config.clone();
        connection.bucket.clear();
        sqlx::query("INSERT INTO s3_connections(vtl_id,revision,configuration) VALUES(?,2,?)")
            .bind(state.catalog.vtl_id().await.unwrap().to_string())
            .bind(serde_json::to_string(&connection).unwrap())
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        let mut creation = config.clone();
        creation.bucket = "vc-pending-2026-10-13-abcd".into();
        reserve(state, &creation).await;
        recover_interrupted(state).await.unwrap();
    }

    fn new_keys(revision: i64) -> ConnectionUpdate {
        ConnectionUpdate {
            revision,
            provider: Some(S3Provider::OvhColdArchiveV1),
            endpoint: cold_vtl_core::OVH_ARCHIVE_ENDPOINT.into(),
            region: "rbx-archive".into(),
            prefix: "vtl".into(),
            access_key_id: "new-key".into(),
            secret_access_key: Some("new-secret".into()),
        }
    }

    #[tokio::test]
    async fn credential_status_is_cached_per_revision_for_five_minutes() {
        let (state, root, mut config) = fixture().await;
        config.bucket.clear();
        let vtl = state.catalog.vtl_id().await.unwrap().to_string();
        sqlx::query("INSERT INTO s3_connections(vtl_id,revision,configuration) VALUES(?,2,?)")
            .bind(&vtl)
            .bind(serde_json::to_string(&config).unwrap())
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        let status = |view: Value| view["credentials"]["status"].as_str().map(str::to_owned);
        keyed_worker(&root, "invalid", "absent", "empty");
        let view = connection_view(&state).await.unwrap();
        assert_eq!(status(view.clone()).as_deref(), Some("invalid"));
        assert!(
            view["credentials"]["checked_at"]
                .as_str()
                .unwrap()
                .ends_with('Z')
        );
        assert!(!view.to_string().contains("private-secret"));
        // Within five minutes the same revision reuses the result.
        keyed_worker(&root, "ok", "absent", "empty");
        assert_eq!(
            status(connection_view(&state).await.unwrap()).as_deref(),
            Some("invalid")
        );
        assert_eq!(calls(&root), vec!["check-credentials key"]);
        // Five minutes later it is checked again.
        age_credentials(&state, Duration::from_secs(301)).await;
        assert_eq!(
            status(connection_view(&state).await.unwrap()).as_deref(),
            Some("ok")
        );
        // A new revision is checked at once.
        keyed_worker(&root, "list_denied", "absent", "empty");
        let pool = state.catalog.pool();
        let bump = |revision: i64| {
            sqlx::query("UPDATE s3_connections SET revision=? WHERE vtl_id=?")
                .bind(revision)
                .bind(&vtl)
                .execute(&pool)
        };
        bump(3).await.unwrap();
        assert_eq!(
            status(connection_view(&state).await.unwrap()).as_deref(),
            Some("list_denied")
        );
        // An unreadable answer is unknown; a missing worker is unreachable.
        fake_worker(&root, "lost", 1);
        bump(4).await.unwrap();
        assert_eq!(
            status(connection_view(&state).await.unwrap()).as_deref(),
            Some("unknown")
        );
        std::fs::remove_file(root.join("worker")).unwrap();
        bump(5).await.unwrap();
        assert_eq!(
            status(connection_view(&state).await.unwrap()).as_deref(),
            Some("unreachable")
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn a_save_refreshes_the_cached_credential_status() {
        let (state, root, config) = fixture().await;
        private_root(&root);
        let mut connection = config.clone();
        connection.bucket.clear();
        sqlx::query("INSERT INTO s3_connections(vtl_id,revision,configuration) VALUES(?,2,?)")
            .bind(state.catalog.vtl_id().await.unwrap().to_string())
            .bind(serde_json::to_string(&connection).unwrap())
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        keyed_worker(&root, "invalid", "absent", "empty");
        let view = connection_view(&state).await.unwrap();
        assert_eq!(view["credentials"]["status"], "invalid");
        keyed_worker(&root, "ok", "absent", "empty");
        store_connection(&state, new_keys(2)).await.unwrap();
        let view = connection_view(&state).await.unwrap();
        assert_eq!(view["credentials"]["status"], "ok");
        assert_eq!(calls(&root).last().unwrap(), "check-credentials new-key");
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn saving_good_keys_closes_an_absent_uncertain_creation() {
        let (state, root, config) = fixture().await;
        private_root(&root);
        uncertain_with_connection(&state, &config).await;
        keyed_worker(&root, "ok", "absent", "empty");
        let saved = store_connection(&state, new_keys(2)).await.unwrap();
        assert_eq!(saved["revision"], 3);
        assert_eq!(creation_phase(&state, "create").await, "abandoned");
        let job = state.catalog.job("create").await.unwrap().unwrap();
        assert_eq!(
            (job.status.as_str(), job.message.as_deref()),
            ("failed", Some(ABSENT))
        );
        assert!(calls(&root).contains(&"bucket-presence new-key".to_owned()));
        assert_eq!(configured(&state).await.unwrap().1.access_key_id, "new-key");
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn saving_good_keys_activates_an_empty_uncertain_creation_at_the_new_revision() {
        let (state, root, config) = fixture().await;
        private_root(&root);
        uncertain_with_connection(&state, &config).await;
        keyed_worker(&root, "ok", "exists", "empty");
        let saved = store_connection(&state, new_keys(2)).await.unwrap();
        assert_eq!(creation_phase(&state, "create").await, "switched");
        let job = state.catalog.job("create").await.unwrap().unwrap();
        assert_eq!(job.status, "completed");
        let active = current(&state).await.unwrap().1.unwrap();
        assert_eq!(active.bucket, "vc-pending-2026-10-13-abcd");
        assert_eq!(active.access_key_id, "new-key");
        assert_eq!(active.secret_access_key, "new-secret");
        // Activated at the revision following the one the save was based on.
        let pinned: i64 = sqlx::query_scalar("SELECT r.revision FROM storage_destination_revisions r JOIN storage_destinations d ON d.id=r.destination_id WHERE d.active=1")
            .fetch_one(&state.catalog.pool())
            .await
            .unwrap();
        assert_eq!(pinned, 3);
        assert_eq!(saved["revision"], 4);
        assert_eq!(connection(&state).await.unwrap().0, 4);
        assert_eq!(
            calls(&root),
            vec![
                "check-credentials new-key",
                "bucket-presence new-key",
                "confirm-empty-bucket new-key"
            ]
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn unknown_creation_still_refuses_the_save() {
        let (state, root, config) = fixture().await;
        private_root(&root);
        uncertain_with_connection(&state, &config).await;
        keyed_worker(&root, "ok", "unknown", "empty");
        let error = store_connection(&state, new_keys(2)).await.unwrap_err();
        assert_eq!(
            error.message,
            "A bucket creation is still running or has to be checked."
        );
        assert_eq!(creation_phase(&state, "create").await, "uncertain");
        assert_eq!(configured(&state).await.unwrap().1.access_key_id, "key");
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn switching_to_a_created_bucket_cancels_running_synchronisations() {
        use std::os::unix::fs::PermissionsExt;
        let (state, root, mut config) = fixture().await;
        let pid = root.join("reconcile.pid");
        std::fs::write(
            root.join("worker"),
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  reconcile) echo $$ > '{pid}'; exec sleep 60 ;;\n  bucket-presence) cat >/dev/null; echo '{{\"status\":\"exists\"}}' ;;\n  confirm-empty-bucket) cat >/dev/null; echo '{{\"status\":\"empty\"}}' ;;\n  *) exit 1 ;;\nesac\n",
                pid = pid.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(root.join("worker"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let cartridge =
            cold_vtl_core::Cartridge::new(cold_vtl_core::Barcode::parse("TAPE01").unwrap(), 1024)
                .unwrap();
        state.catalog.create_cartridge(&cartridge).await.unwrap();
        let vtl = state.catalog.vtl_id().await.unwrap().to_string();
        sqlx::query("INSERT INTO s3_reconcile_required(vtl_id,barcode) VALUES(?,'TAPE01')")
            .bind(&vtl)
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        state
            .catalog
            .reserve_job("sync", "reconcile", &cartridge, None, None)
            .await
            .unwrap();
        crate::operations::spawn_reconcile(state.clone(), cartridge, "sync".into(), None);
        let mut waited = 0;
        while !pid.exists() || state.catalog.job("sync").await.unwrap().unwrap().status != "running"
        {
            assert!(waited < 100, "synchronisation not started");
            tokio::time::sleep(Duration::from_millis(50)).await;
            waited += 1;
        }
        let process: i32 = std::fs::read_to_string(&pid)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        config.bucket = "vc-switch-2026-10-13-abcd".into();
        reserve(&state, &config).await;
        recover_interrupted(&state).await.unwrap();
        assert_eq!(
            settle(&state, "create", None).await.unwrap(),
            Settlement::Activated
        );
        // The job is closed with the switch, so nothing waits on it any more.
        let job = state.catalog.job("sync").await.unwrap().unwrap();
        assert_eq!(
            (job.status.as_str(), job.message.as_deref()),
            ("failed", Some(crate::operations::SYNC_CANCELLED))
        );
        assert_eq!(crate::operations::running_syncs(&state).await.unwrap(), 0);
        crate::s3_settings::require_not_synchronising(&state, "TAPE01")
            .await
            .unwrap();
        // Its worker is stopped, and the message survives the task's end.
        let mut waited = 0;
        while unsafe { libc::kill(process, 0) } == 0 {
            assert!(waited < 100, "synchronisation worker still running");
            tokio::time::sleep(Duration::from_millis(50)).await;
            waited += 1;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let job = state.catalog.job("sync").await.unwrap().unwrap();
        assert_eq!(
            (job.status.as_str(), job.message.as_deref()),
            ("failed", Some(crate::operations::SYNC_CANCELLED))
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn a_synchronisation_finishing_as_the_switch_commits_stays_cancelled() {
        use std::os::unix::fs::PermissionsExt;
        let (state, root, mut config) = fixture().await;
        let (held, release) = (root.join("held"), root.join("release"));
        std::fs::write(
            root.join("worker"),
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  reconcile) touch '{held}'; while [ ! -f '{release}' ]; do sleep 0.05; done; printf '%s' '{{\"generations\":[{{\"barcode\":\"TAPE01\",\"generation\":7,\"format_version\":2,\"manifest_key\":\"manifest\",\"manifest_sha256\":\"checksum\",\"logical_size_bytes\":1024,\"file_count\":5,\"segment_count\":1,\"local_match\":false}}],\"local_size_bytes\":1024}}' ;;\n  bucket-presence) cat >/dev/null; echo '{{\"status\":\"exists\"}}' ;;\n  confirm-empty-bucket) cat >/dev/null; echo '{{\"status\":\"empty\"}}' ;;\n  *) exit 1 ;;\nesac\n",
                held = held.display(),
                release = release.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(root.join("worker"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let cartridge =
            cold_vtl_core::Cartridge::new(cold_vtl_core::Barcode::parse("TAPE01").unwrap(), 1024)
                .unwrap();
        state.catalog.create_cartridge(&cartridge).await.unwrap();
        sqlx::query("INSERT INTO s3_reconcile_required(vtl_id,barcode) VALUES(?,'TAPE01')")
            .bind(state.catalog.vtl_id().await.unwrap().to_string())
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        state
            .catalog
            .reserve_job("sync", "reconcile", &cartridge, None, None)
            .await
            .unwrap();
        // The stop signal has not reached this task yet: only the job status protects it.
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(crate::operations::run_reconcile(
            state.clone(),
            cartridge,
            "sync".into(),
            None,
            stopped,
        ));
        let mut waited = 0;
        while !held.exists() {
            assert!(waited < 100, "synchronisation not started");
            tokio::time::sleep(Duration::from_millis(50)).await;
            waited += 1;
        }
        config.bucket = "vc-race-2026-10-13-abcd".into();
        reserve(&state, &config).await;
        recover_interrupted(&state).await.unwrap();
        assert_eq!(
            settle(&state, "create", None).await.unwrap(),
            Settlement::Activated
        );
        std::fs::write(&release, "").unwrap();
        task.await.unwrap();
        let job = state.catalog.job("sync").await.unwrap().unwrap();
        assert_eq!(
            (job.status.as_str(), job.message.as_deref()),
            ("failed", Some(crate::operations::SYNC_CANCELLED))
        );
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM remote_generations")
            .fetch_one(&state.catalog.pool())
            .await
            .unwrap();
        assert_eq!(rows, 0);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn pending_rotation_refuses_the_save_before_any_probe() {
        let (state, root, config) = fixture().await;
        private_root(&root);
        uncertain_with_connection(&state, &config).await;
        uncertain_rotation(&state, "vc-next-2026-10-14-abcd").await;
        let active = state.catalog.destination().await.unwrap().id;
        keyed_worker(&root, "ok", "exists", "empty");
        let error = store_connection(&state, new_keys(2)).await.unwrap_err();
        assert_eq!(
            error.message,
            "A bucket creation is still running or has to be checked."
        );
        assert_eq!(calls(&root), vec!["check-credentials new-key"]);
        assert_eq!(creation_phase(&state, "create").await, "uncertain");
        assert_eq!(state.catalog.destination().await.unwrap().id, active);
        assert_eq!(configured(&state).await.unwrap().1.access_key_id, "key");
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn keys_for_another_endpoint_do_not_settle_the_creation() {
        let (state, root, config) = fixture().await;
        private_root(&root);
        uncertain_with_connection(&state, &config).await;
        keyed_worker(&root, "ok", "absent", "empty");
        let mut update = new_keys(2);
        update.provider = Some(S3Provider::S3Compatible);
        update.endpoint = "https://s3.example.test".into();
        update.region = "elsewhere".into();
        assert!(store_connection(&state, update).await.is_err());
        assert_eq!(creation_phase(&state, "create").await, "uncertain");
        assert_eq!(calls(&root), vec!["check-credentials new-key"]);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn provider_refusal_closes_the_creation_without_manual_resolution() {
        let (state, root, mut config) = fixture().await;
        config.bucket = "vc-refused-2026-10-12-abcd".into();
        reserve(&state, &config).await;
        fake_worker(
            &root,
            r#"{"status":"not_created","reason":"credentials","bucket":"vc-refused-2026-10-12-abcd"}"#,
            3,
        );
        run_create(&state, "create", &config, 1, Origin::Manual)
            .await
            .unwrap();
        let phase: String =
            sqlx::query_scalar("SELECT phase FROM bucket_creations WHERE job_id='create'")
                .fetch_one(&state.catalog.pool())
                .await
                .unwrap();
        assert_eq!(phase, "abandoned");
        let job = state.catalog.job("create").await.unwrap().unwrap();
        assert_eq!(job.status, "failed");
        assert!(
            job.message
                .unwrap_or_default()
                .contains("The provider rejected the credentials.")
        );
        assert_eq!(
            current(&state).await.unwrap().1.unwrap().bucket,
            "current-bucket"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn next_bucket_name_is_dated_readable_and_skips_known_buckets() {
        let (state, root, mut config) = fixture().await;
        let vtl = state.catalog.vtl_id().await.unwrap().to_string();
        let day: String = sqlx::query_scalar("SELECT strftime('%Y-%m-%d','now')")
            .fetch_one(&state.catalog.pool())
            .await
            .unwrap();
        let base = format!("vc-vtl-principale-{day}");
        let first = super::super::next_bucket_name(&state, &vtl, &[])
            .await
            .unwrap();
        assert_eq!(first, base);
        assert!(cold_vtl_core::valid_managed_bucket_name(&first));
        config.bucket = first.clone();
        reserve(&state, &config).await;
        let next = super::super::next_bucket_name(&state, &vtl, &[])
            .await
            .unwrap();
        assert_eq!(next, format!("{base}-2"));
        // A name listed at the provider is skipped as well.
        let remote = super::super::next_bucket_name(&state, &vtl, &[format!("{base}-2")])
            .await
            .unwrap();
        assert_eq!(remote, format!("{base}-3"));
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn fresh_vtl_creates_its_first_destination_without_existing_bucket() {
        let (mut state, root, mut config) = fixture().await;
        let vtl = cold_vtl_core::VtlId::new();
        sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'Fresh','ready')")
            .bind(vtl.to_string())
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        state.catalog = state.catalog.for_vtl(vtl).await.unwrap();
        config.bucket.clear();
        sqlx::query("INSERT INTO s3_connections(vtl_id,revision,configuration) VALUES(?,1,?)")
            .bind(vtl.to_string())
            .bind(serde_json::to_string(&config).unwrap())
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        assert!(current(&state).await.unwrap().1.is_none());
        assert_eq!(configured(&state).await.unwrap().0, 1);
        config.bucket = "first-fresh-bucket".into();
        sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id) VALUES('first','bucket_create',?,'running',?)").bind(&config.bucket).bind(vtl.to_string()).execute(&state.catalog.pool()).await.unwrap();
        sqlx::query("INSERT INTO bucket_creations(job_id,vtl_id,next_bucket,configuration,revision,phase) VALUES('first',?,?,?,1,'creating')").bind(vtl.to_string()).bind(&config.bucket).bind(serde_json::to_string(&config).unwrap()).execute(&state.catalog.pool()).await.unwrap();
        activate_created(&state, "first", &config, 1).await.unwrap();
        assert_eq!(
            current(&state).await.unwrap().1.unwrap().bucket,
            "first-fresh-bucket"
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn existing_bucket_becomes_active_and_flags_every_cartridge() {
        let (state, root, mut config) = fixture().await;
        private_root(&root);
        for barcode in ["TAPE01", "TAPE02"] {
            let cartridge = cold_vtl_core::Cartridge::new(
                cold_vtl_core::Barcode::parse(barcode).unwrap(),
                1024,
            )
            .unwrap();
            state.catalog.create_cartridge(&cartridge).await.unwrap();
        }
        due_weekly(&state).await;
        assert!(super::super::schedule::rotation_due(&state).await.unwrap());
        let replaced = state.catalog.destination().await.unwrap().id;
        config.bucket = "existing-bucket".into();
        let value = activate_existing(&state, &config, 1, "admin")
            .await
            .unwrap();
        assert_eq!(value["status"], "activated");
        assert_eq!(value["active"], true);
        assert_eq!(value["revision"], 2);
        assert_eq!(
            current(&state).await.unwrap().1.unwrap().bucket,
            "existing-bucket"
        );
        let flagged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM s3_reconcile_required")
            .fetch_one(&state.catalog.pool())
            .await
            .unwrap();
        assert_eq!(flagged, 2);
        // Never sent before its synchronisation (here refused: no mhVTL).
        assert!(
            crate::s3_settings::require_reconciled(&state, "TAPE01")
                .await
                .is_err()
        );
        // A manual choice serves the current period (spec §4) and dates the
        // end of the replaced bucket's activity, for automatic archiving.
        assert!(!super::super::schedule::rotation_due(&state).await.unwrap());
        let ended: Vec<(String, bool)> = sqlx::query_as(
            "SELECT id,deactivated_at IS NOT NULL FROM storage_destinations WHERE identity IS NOT NULL ORDER BY active",
        )
        .fetch_all(&state.catalog.pool())
        .await
        .unwrap();
        assert_eq!(ended[0], (replaced, true));
        assert!(!ended[1].1, "the active bucket has not ended");
        assert!(
            serde_json::from_value::<Attach>(json!({"revision":1,"bucket":"b","activate":true}))
                .unwrap()
                .activate
        );
        assert!(
            !serde_json::from_value::<Attach>(json!({"revision":1,"bucket":"b"}))
                .unwrap()
                .activate
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn existing_bucket_activation_waits_for_jobs_and_creations() {
        let (state, root, mut config) = fixture().await;
        private_root(&root);
        config.bucket = "existing-bucket".into();
        reserve(&state, &config).await;
        let refused = activate_existing(&state, &config, 1, "admin")
            .await
            .unwrap_err();
        assert_eq!(refused.status, StatusCode::CONFLICT);
        assert_eq!(
            current(&state).await.unwrap().1.unwrap().bucket,
            "current-bucket"
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }
}
