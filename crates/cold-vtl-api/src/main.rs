mod activity;
mod archive;
mod auth;
mod bandwidth;
mod build_info;
mod cartridges;
mod catalog;
mod deletion;
mod destinations;
mod history;
mod maintenance;
mod media_access;
mod notifications;
mod operations;
mod provisioning;
mod recovery;
mod reintegration;
mod remote_delete;
mod s3_settings;
mod storage;
mod storage_routes;
mod tape_storage;
mod telemetry;
mod vtl_runtime;
mod vtls;
mod web;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{StatusCode, header},
    middleware,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use catalog::{Catalog, Job};
use cold_vtl_core::{Cartridge, CartridgeState};
use serde::Serialize;
use std::{env, net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::net::TcpListener;

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLES_CSS: &str = include_str!("../web/styles.css");

#[derive(Clone)]
struct AppState {
    catalog: Catalog,
    config: Arc<AppConfig>,
    operations: Arc<crate::vtl_runtime::OperationLocks>,
}

#[derive(Clone)]
struct AppConfig {
    storage_socket: Option<PathBuf>,
    media_root: PathBuf,
    worker_path: PathBuf,
    restore_root: PathBuf,
    quiesce_check: Option<PathBuf>,
    maintenance_helper: Option<PathBuf>,
    cartridge_helper: Option<PathBuf>,
    reintegration_helper: Option<PathBuf>,
    archive_helper: Option<PathBuf>,
    telemetry_dir: Option<PathBuf>,
    telemetry_retention_days: u32,
}

#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
    service: &'static str,
    version: &'static str,
    revision: &'static str,
}

#[derive(Debug, Serialize)]
struct ServiceStatus {
    status: &'static str,
    cartridges: usize,
    jobs: usize,
    media_root: String,
    version: &'static str,
    revision: &'static str,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if admin_command().await? {
        return Ok(());
    }
    let auth_config = auth::Config::from_env()?;
    let listen = env::var("COLD_VTL_LISTEN").unwrap_or_else(|_| "127.0.0.1:8443".to_owned());
    let address: SocketAddr = listen.parse()?;
    let database_path = env::var("COLD_VTL_DATABASE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/cold-vtl/catalog.db"));
    let media_root = env::var("COLD_VTL_MEDIA_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/mhvtl"));
    let worker_path = env::var("COLD_VTL_WORKER")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/usr/local/bin/cold-vtl-worker"));
    let restore_root = env::var_os("COLD_VTL_RESTORE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/cold-vtl/restores"));
    let quiesce_check = env::var_os("COLD_VTL_QUIESCE_CHECK").map(PathBuf::from);
    let ui_dir = env::var_os("COLD_VTL_UI_DIR").map(PathBuf::from);
    if let Some(path) = &ui_dir {
        web::validate_directory(path).await?;
    }
    let capacity_bytes = env::var("COLD_VTL_CARTRIDGE_CAPACITY_BYTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(2 * 1024 * 1024 * 1024);

    let catalog = Catalog::open(&database_path).await?;
    if !catalog.state_dir().join("storage-isolated").exists() {
        catalog
            .initialize_legacy(&media_root, &restore_root)
            .await?;
    }
    // Host-managed storage never imports directories from an unverified mount.
    // Cartridges created through the controller are already catalogued.
    let discovered = if catalog.state_dir().join("storage-managed").exists() {
        0
    } else {
        catalog.discover(&media_root, capacity_bytes).await?
    };
    let auth = auth::Auth::new(catalog.pool(), auth_config)
        .await
        .map_err(|e| e.message)?;
    let state = AppState {
        catalog,
        operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
        config: Arc::new(AppConfig {
            storage_socket: env::var_os("COLD_VTL_STORAGE_SOCKET").map(PathBuf::from),
            media_root,
            worker_path,
            restore_root,
            quiesce_check,
            maintenance_helper: env::var_os("COLD_VTL_MAINTENANCE_HELPER").map(PathBuf::from),
            cartridge_helper: env::var_os("COLD_VTL_CARTRIDGE_HELPER").map(PathBuf::from),
            archive_helper: env::var_os("COLD_VTL_ARCHIVE_HELPER").map(PathBuf::from),
            telemetry_dir: env::var_os("COLD_VTL_TELEMETRY_DIR").map(PathBuf::from),
            telemetry_retention_days: telemetry::history::retention_days(
                env::var("COLD_VTL_TELEMETRY_RETENTION_DAYS")
                    .ok()
                    .as_deref(),
            )?,
            reintegration_helper: env::var_os("COLD_VTL_REINTEGRATION_HELPER").map(PathBuf::from),
        }),
    };
    archive::monitor(state.clone());
    archive::schedule_monitor(state.clone());
    s3_settings::buckets::recover_interrupted(&state).await?;
    s3_settings::buckets::monitor(state.clone());
    recovery::resume(state.clone())
        .await
        .map_err(|error| error.message)?;
    notifications::monitor(state.clone());
    operations::reconcile_monitor(state.clone());
    let _telemetry_history = telemetry::history::monitor(state.clone());
    let public_url =
        env::var("COLD_VTL_PUBLIC_URL").unwrap_or_else(|_| "https://localhost:8443".into());
    println!(
        "cold-vtld listening at {public_url}; discovered {discovered} cartridge(s); authentication required"
    );
    let app = app(state, ui_dir, auth).into_make_service_with_connect_info::<SocketAddr>();
    match (
        env::var_os("COLD_VTL_TLS_CERT"),
        env::var_os("COLD_VTL_TLS_KEY"),
    ) {
        (Some(cert), Some(key)) => {
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
            let handle = axum_server::Handle::new();
            let shutdown = handle.clone();
            tokio::spawn(async move {
                shutdown_signal().await;
                shutdown.graceful_shutdown(Some(std::time::Duration::from_secs(15)));
            });
            axum_server::bind_rustls(address, tls)
                .handle(handle)
                .serve(app)
                .await?;
        }
        (None, None) => {
            if !address.ip().is_loopback()
                && env::var("COLD_VTL_ALLOW_INSECURE_HTTP").as_deref() != Ok("true")
            {
                return Err("native TLS required on a non-loopback listener; use a loopback listener behind an HTTPS proxy".into());
            }
            let listener = TcpListener::bind(address).await?;
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await?;
        }
        _ => return Err("configure both COLD_VTL_TLS_CERT and COLD_VTL_TLS_KEY".into()),
    }

    Ok(())
}

fn app(state: AppState, ui_dir: Option<PathBuf>, auth: auth::Auth) -> Router {
    let router = Router::new()
        .route("/v1/status", get(service_status))
        .route(
            "/v1/settings/s3",
            get(s3_settings::get).put(s3_settings::apply),
        )
        .route("/v1/settings/s3/test", post(s3_settings::test))
        .route("/v1/storage/buckets", get(destinations::buckets))
        .route("/v1/storage/history", get(history::history))
        .route("/v1/storage", get(destinations::status))
        .route("/v1/storage/access", get(destinations::access))
        .route("/v1/storage/recall", post(destinations::recall_submit))
        .route(
            "/v1/storage/archive",
            get(destinations::overview).post(destinations::submit),
        )
        .route("/v1/storage/archive/refresh", post(destinations::refresh))
        .route("/v1/maintenance", get(maintenance::status))
        .route("/v1/maintenance/events", get(maintenance::events))
        .route("/v1/maintenance/enter", post(maintenance::enter))
        .route("/v1/maintenance/leave", post(maintenance::leave))
        .route("/v1/reintegrations", get(reintegration::status))
        .route(
            "/v1/reintegrations/:id/rollback",
            post(reintegration::rollback),
        )
        .route(
            "/v1/cartridges/:barcode/reintegrate",
            post(reintegration::start),
        )
        .route(
            "/v1/cartridges",
            get(list_cartridges).post(cartridges::create),
        )
        .route(
            "/v1/cartridges/deletion",
            get(deletion::options).post(deletion::preview),
        )
        .route("/v1/cartridges/delete-local", post(deletion::delete))
        .route("/v1/cartridges/options", get(cartridges::options))
        .route("/v1/cartridges/:barcode/seal", post(seal_cartridge))
        .route("/v1/cartridges/:barcode/export", post(destinations::export))
        .route(
            "/v1/cartridges/:barcode/generations",
            get(destinations::generations),
        )
        .route(
            "/v1/cartridges/:barcode/reconcile",
            post(destinations::reconcile),
        )
        .route(
            "/v1/cartridges/:barcode/restore",
            post(destinations::restore),
        )
        .route("/v1/jobs", get(list_jobs))
        .route("/v1/jobs/:id", get(get_job))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            vtls::legacy_context,
        ))
        .route(
            "/v1/notifications/settings",
            get(notifications::get).put(notifications::put),
        )
        .route("/v1/activity", get(activity::activity))
        .route("/v1/activity/seen", post(activity::seen))
        .route("/v1/notifications/preview", get(notifications::preview))
        .route("/v1/notifications/test", post(notifications::test))
        .route(
            "/v1/notifications/deliveries",
            get(notifications::deliveries),
        )
        .route("/v1/settings/tape-storage", get(tape_storage::status))
        .route("/v1/settings/tape-storage/check", post(tape_storage::check))
        .route("/v1/settings/tape-storage/apply", post(tape_storage::apply))
        .route(
            "/v1/vtls/:vtl_id/settings/tape-storage",
            get(tape_storage::scoped_status),
        )
        .route(
            "/v1/vtls/:vtl_id/settings/tape-storage/check",
            post(tape_storage::scoped_check),
        )
        .route(
            "/v1/vtls/:vtl_id/settings/tape-storage/apply",
            post(tape_storage::scoped_apply),
        )
        .route("/v1/vtls", get(vtls::list).post(vtls::create))
        .route("/v1/vtls/:vtl_id", get(vtls::show).patch(vtls::rename))
        .route(
            "/v1/vtls/:vtl_id/bandwidth",
            get(bandwidth::status).put(bandwidth::update),
        )
        .route(
            "/v1/vtls/:vtl_id/pbs-connection",
            get(bandwidth::connection).put(bandwidth::update_connection),
        )
        .route(
            "/v1/vtls/:vtl_id/client",
            get(bandwidth::client).put(bandwidth::update_client),
        )
        .route("/v1/vtls/:vtl_id/provisioning", get(provisioning::status))
        .route("/v1/vtls/:vtl_id/prepare", post(provisioning::prepare))
        .route("/v1/vtls/:vtl_id/apply", post(provisioning::apply))
        .route("/v1/vtls/:vtl_id/activate", post(provisioning::activate))
        .route("/v1/vtls/:vtl_id/status", get(vtls::status))
        .route("/v1/vtls/:vtl_id/telemetry", get(vtls::telemetry))
        .route(
            "/v1/vtls/:vtl_id/telemetry/history",
            get(vtls::telemetry_history),
        )
        .route(
            "/v1/vtls/:vtl_id/cartridges/:barcode/generations",
            get(vtls::generations),
        )
        .route("/v1/vtls/:vtl_id/storage/buckets", get(vtls::buckets))
        .route("/v1/vtls/:vtl_id/storage/history", get(vtls::history))
        .route(
            "/v1/vtls/:vtl_id/storage/connection",
            get(storage_routes::connection).put(storage_routes::save_connection),
        )
        .route(
            "/v1/vtls/:vtl_id/storage/schedule",
            get(storage_routes::schedule).put(storage_routes::save_schedule),
        )
        .route(
            "/v1/vtls/:vtl_id/storage/recovery",
            get(storage_routes::recovery_inventory).post(storage_routes::recover),
        )
        .route(
            "/v1/vtls/:vtl_id/storage/copies/delete",
            post(storage_routes::delete_copy),
        )
        .route(
            "/v1/vtls/:vtl_id/storage/recovery/check",
            post(storage_routes::recovery_check),
        )
        .route(
            "/v1/vtls/:vtl_id/storage/buckets/discover",
            get(storage_routes::discover),
        )
        .route(
            "/v1/vtls/:vtl_id/storage/buckets/attach",
            post(storage_routes::attach),
        )
        .route(
            "/v1/vtls/:vtl_id/storage/buckets/create",
            post(storage_routes::create),
        )
        .route(
            "/v1/vtls/:vtl_id/storage/buckets/:destination_id/retire",
            post(storage_routes::retire),
        )
        .route("/v1/vtls/:vtl_id/storage", get(vtls::storage))
        .route("/v1/vtls/:vtl_id/storage/access", get(vtls::access))
        .route("/v1/vtls/:vtl_id/maintenance", get(vtls::maintenance))
        .route("/v1/vtls/:vtl_id/maintenance/events", get(vtls::events))
        .route("/v1/vtls/:vtl_id/maintenance/enter", post(vtls::enter))
        .route("/v1/vtls/:vtl_id/maintenance/leave", post(vtls::leave))
        .route("/v1/vtls/:vtl_id/reintegrations", get(vtls::reintegrations))
        .route(
            "/v1/vtls/:vtl_id/cartridges/:barcode/reintegrate",
            post(vtls::reintegrate),
        )
        .route(
            "/v1/vtls/:vtl_id/reintegrations/:operation_id/rollback",
            post(vtls::rollback),
        )
        .route(
            "/v1/vtls/:vtl_id/storage/archive",
            get(vtls::archive).post(vtls::archive_submit),
        )
        .route(
            "/v1/vtls/:vtl_id/storage/archive/refresh",
            post(vtls::archive_refresh),
        )
        .route("/v1/vtls/:vtl_id/storage/recall", post(vtls::recall_submit))
        .route(
            "/v1/vtls/:vtl_id/cartridges/:barcode/seal",
            post(vtls::seal),
        )
        .route(
            "/v1/vtls/:vtl_id/cartridges/:barcode/export",
            post(vtls::export),
        )
        .route(
            "/v1/vtls/:vtl_id/cartridges/:barcode/reconcile",
            post(vtls::reconcile),
        )
        .route(
            "/v1/vtls/:vtl_id/cartridges/:barcode/restore",
            post(vtls::restore),
        )
        .route(
            "/v1/vtls/:vtl_id/settings/s3",
            get(vtls::s3_get).put(vtls::s3_apply),
        )
        .route("/v1/vtls/:vtl_id/settings/s3/test", post(vtls::s3_test))
        .route("/v1/vtls/:vtl_id/destinations", get(vtls::destinations))
        .route(
            "/v1/vtls/:vtl_id/cartridges",
            get(vtls::cartridges).post(vtls::create_cartridge),
        )
        .route(
            "/v1/vtls/:vtl_id/cartridges/options",
            get(vtls::cartridge_options),
        )
        .route(
            "/v1/vtls/:vtl_id/cartridges/batch",
            post(vtls::create_cartridge_batch),
        )
        .route(
            "/v1/vtls/:vtl_id/cartridges/deletion",
            get(vtls::deletion_options).post(vtls::deletion_preview),
        )
        .route(
            "/v1/vtls/:vtl_id/cartridges/delete-local",
            post(vtls::delete_local),
        )
        .route("/v1/vtls/:vtl_id/cartridges/:barcode", get(vtls::cartridge))
        .route("/v1/vtls/:vtl_id/jobs", get(vtls::jobs))
        .route("/v1/vtls/:vtl_id/jobs/:job_id", get(vtls::job))
        .route_layer(middleware::from_fn_with_state(
            auth.clone(),
            auth::require_auth,
        ))
        .with_state(state)
        .merge(auth::routes(auth))
        .route("/health", get(health));
    match ui_dir {
        Some(path) => web::with_static_ui(router, path),
        None => router
            .route("/", get(index))
            .route("/app.js", get(javascript))
            .route("/styles.css", get(stylesheet)),
    }
    .layer(middleware::from_fn(auth::security_headers))
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        APP_JS,
    )
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLES_CSS,
    )
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        service: "cold-vtld",
        version: build_info::VERSION,
        revision: build_info::REVISION,
    })
}

async fn service_status(State(state): State<AppState>) -> Result<Json<ServiceStatus>, ApiError> {
    let cartridges = state.catalog.list_cartridges().await?;
    let jobs = state.catalog.list_jobs().await?;
    Ok(Json(ServiceStatus {
        status: "ok",
        cartridges: cartridges.len(),
        jobs: jobs.len(),
        media_root: state.config.media_root.display().to_string(),
        version: build_info::VERSION,
        revision: build_info::REVISION,
    }))
}

async fn list_cartridges(
    State(state): State<AppState>,
) -> Result<Json<Vec<catalog::CartridgeView>>, ApiError> {
    Ok(Json(state.catalog.cartridge_views(None).await?))
}

async fn seal_cartridge(
    State(state): State<AppState>,
    Path(raw_barcode): Path<String>,
) -> Result<Json<Cartridge>, ApiError> {
    // A cartridge synchronising with the bucket says so, before "job active".
    s3_settings::require_not_synchronising_for(&state, &raw_barcode, s3_settings::Retry::Operation)
        .await?;
    let _lock = state.operations.lock().await;
    let mut cartridge = operations::available_cartridge(&state, raw_barcode).await?;
    deletion::require_local(&state, cartridge.barcode.as_str()).await?;
    let _guard = media_access::protect(&state, cartridge.barcode.as_str()).await?;
    s3_settings::require_reconciled_for(
        &state,
        cartridge.barcode.as_str(),
        s3_settings::Retry::Operation,
    )
    .await?;
    cartridge
        .transition_to(CartridgeState::Sealed)
        .map_err(ApiError::conflict)?;
    cartridge.bytes_written =
        catalog::directory_size(&state.config.media_root.join(cartridge.barcode.as_str()))
            .map_err(ApiError::conflict)?;
    state.catalog.update_cartridge(&cartridge).await?;
    Ok(Json(cartridge))
}

async fn export_cartridge(
    State(state): State<AppState>,
    Path(raw_barcode): Path<String>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    // A cartridge synchronising with the bucket says so, before "job active".
    s3_settings::require_not_synchronising(&state, &raw_barcode).await?;
    // No bucket for an export that cannot run (checked again under the lock).
    let requested = operations::available_cartridge(&state, raw_barcode.clone()).await?;
    deletion::require_local(&state, requested.barcode.as_str()).await?;
    // A new period first gets its bucket, outside the VTL lock (the creation
    // takes it itself); the export below then goes to the active destination.
    s3_settings::schedule::rotate_if_due(&state).await?;
    let _lock = state.operations.lock().await;
    let mut cartridge = operations::available_cartridge(&state, raw_barcode).await?;
    deletion::require_local(&state, cartridge.barcode.as_str()).await?;
    let guard = media_access::protect(&state, cartridge.barcode.as_str()).await?;
    s3_settings::require_reconciled(&state, cartridge.barcode.as_str()).await?;
    storage::require_access(&state, true).await?;
    if matches!(
        cartridge.state,
        CartridgeState::Writable | CartridgeState::Failed
    ) {
        // Only change the in-memory state here. The catalogue validates failed
        // export provenance and atomically reserves the same generation below.
        cartridge
            .transition_to(CartridgeState::Sealed)
            .map_err(ApiError::conflict)?;
    }
    cartridge
        .transition_to(CartridgeState::Uploading)
        .map_err(ApiError::conflict)?;
    // Capture the local estimate while the media guard is held. After sealing,
    // the UI uses the catalogue, including when the upload later fails.
    // This only reads file metadata, never the cartridge payload.
    cartridge.bytes_written =
        catalog::directory_size(&state.config.media_root.join(cartridge.barcode.as_str()))
            .map_err(ApiError::conflict)?;
    let job = state
        .catalog
        .reserve_job(
            &uuid::Uuid::new_v4().to_string(),
            "export",
            &cartridge,
            Some(cartridge.generation),
            None,
        )
        .await?;
    tokio::spawn(run_export(state.clone(), cartridge, job.id.clone(), guard));
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn run_export(
    state: AppState,
    mut cartridge: Cartridge,
    job_id: String,
    guard: media_access::Guard,
) {
    let result = async {
        state
            .catalog
            .update_job(
                &job_id,
                "running",
                10,
                Some("upload and integrity verification started"),
            )
            .await
            .map_err(|e| e.to_string())?;
        let source = state.config.media_root.join(cartridge.barcode.as_str());
        let mut command = state
            .catalog
            .worker_command(&state.config.worker_path, Some(&job_id))
            .await
            .map_err(|e| e.to_string())?;
        command.env("COLD_VTL_EXPORT_JOB_ID", &job_id);
        configure_export(&mut command, &cartridge, &source);
        let output = operations::worker_output(&mut command).await?;
        let value: serde_json::Value = serde_json::from_str(&output).map_err(|e| e.to_string())?;
        let generations = if let Some(receipt) = value.get("receipt") {
            let receipt: cold_vtl_core::ExportReceipt =
                serde_json::from_value(receipt.clone()).map_err(|e| e.to_string())?;
            if value["status"] != "verified" {
                return Err("export worker did not report verified integrity".into());
            }
            receipt.validate(&cartridge.barcode, cartridge.generation)?;
            let job = state.catalog.job(&job_id).await.map_err(|e| e.to_string())?
                .ok_or("export job disappeared")?;
            let expected = cold_vtl_core::ExportScope {
                job_id: Some(job_id.clone()),
                vtl_id: Some(state.catalog.vtl_id().await.map_err(|e| e.to_string())?.to_string()),
                destination_id: job.destination_id.clone(),
                configuration_revision: Some(job.configuration_revision.unwrap_or(0)),
                source: source.canonicalize().map_err(|e| e.to_string())?
                    .to_str().ok_or("non UTF-8 source")?.into(),
            };
            let config: String = sqlx::query_scalar(
                "SELECT configuration FROM storage_destination_revisions WHERE destination_id=? AND revision=?",
            )
            .bind(&job.destination_id)
            .bind(job.configuration_revision)
            .fetch_one(&state.catalog.pool())
            .await
            .map_err(|e| e.to_string())?;
            let config: serde_json::Value = serde_json::from_str(&config).map_err(|e| e.to_string())?;
            let prefix = config["prefix"].as_str().ok_or("missing pinned S3 prefix")?.trim_matches('/');
            let suffix = format!("cartridges/{}/manifests/{}.json", cartridge.barcode, cartridge.generation);
            let expected_key = if prefix.is_empty() { suffix } else { format!("{prefix}/{suffix}") };
            receipt.validate_scope(&expected, &expected_key)?;
            cold_vtl_core::ReconcileOutput {
                local_size_bytes: receipt.generation.logical_size_bytes,
                generations: vec![receipt.generation],
            }
        } else {
            // Older workers do not provide native integrity evidence.
            let mut inventory = state
                .catalog
                .worker_command(&state.config.worker_path, Some(&job_id))
                .await
                .map_err(|e| e.to_string())?;
            inventory
                .arg("reconcile")
                .arg("--barcode")
                .arg(cartridge.barcode.as_str())
                .arg("--source")
                .arg(source);
            let result: cold_vtl_core::ReconcileOutput =
                serde_json::from_str(&operations::worker_output(&mut inventory).await?)
                    .map_err(|e| e.to_string())?;
            result
        };
        guard.check(&state).await.map_err(|e| e.message)?;
        if !generations.generations.iter().any(|generation| {
            generation.barcode == cartridge.barcode
                && generation.generation == cartridge.generation
                && generation.local_match
        }) {
            return Err("exported generation no longer matches the local cartridge".to_owned());
        }
        cartridge.bytes_written = generations.local_size_bytes;
        cartridge.state = CartridgeState::Verified;
        if value.get("receipt").is_some() {
            state.catalog
                .finish_verified_export(&job_id, &output, &cartridge, &generations.generations[0])
                .await
                .map_err(|e| e.to_string())
        } else {
            state
                .catalog
                .finish_job(
                    &job_id,
                    "completed",
                    &output,
                    Some(&cartridge),
                    Some(&generations.generations),
                )
                .await
                .map_err(|e| e.to_string())
        }
    }
    .await;
    if let Err(error) = result {
        cartridge.state = CartridgeState::Failed;
        let _ = state
            .catalog
            .finish_job(&job_id, "failed", &error, Some(&cartridge), None)
            .await;
    }
}

#[derive(Default, serde::Deserialize)]
struct JobsQuery {
    barcode: Option<String>,
    kind: Option<String>,
}
impl JobsQuery {
    async fn read(self, catalog: &Catalog) -> Result<Vec<Job>, ApiError> {
        let barcode = self
            .barcode
            .map(cold_vtl_core::Barcode::parse)
            .transpose()
            .map_err(ApiError::bad_request)?;
        if self.kind.as_deref().is_some_and(|kind| kind != "export") {
            return Err(ApiError::bad_request("Unsupported job history kind"));
        }
        Ok(catalog
            .filtered_jobs(barcode.as_ref(), self.kind.as_deref())
            .await?)
    }
}
async fn list_jobs(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<JobsQuery>,
) -> Result<Json<Vec<Job>>, ApiError> {
    Ok(Json(query.read(&state.catalog).await?))
}

async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Job>, ApiError> {
    state
        .catalog
        .job(&id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found(format!("job {id} not found")))
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(error: impl ToString) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: error.to_string(),
        }
    }

    fn conflict(error: impl ToString) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: error.to_string(),
        }
    }

    fn gone(error: impl ToString) -> Self {
        Self {
            status: StatusCode::GONE,
            message: error.to_string(),
        }
    }

    fn not_found(error: impl ToString) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: error.to_string(),
        }
    }
}

impl From<catalog::CatalogError> for ApiError {
    fn from(error: catalog::CatalogError) -> Self {
        match &error {
            catalog::CatalogError::AmbiguousVtl => return Self::conflict(error),
            catalog::CatalogError::MissingVtl | catalog::CatalogError::MissingJob => {
                return Self::not_found(error);
            }
            _ => {}
        }
        if matches!(&error, catalog::CatalogError::Conflict)
            || matches!(&error, catalog::CatalogError::Database(sqlx::Error::Database(db)) if db.is_unique_violation())
        {
            return Self::conflict("cartridge has an active job or its state changed");
        }
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

#[derive(clap::Parser)]
#[command(name = "cold-vtld")]
struct AdminCli {
    #[command(subcommand)]
    command: Option<AdminCommand>,
}
#[derive(clap::Subcommand)]
enum AdminCommand {
    /// Manage local access on the appliance. Passwords are read from standard input.
    CreateUser {
        username: String,
        #[arg(long, default_value = "admin")]
        role: String,
    },
    ResetPassword {
        username: String,
    },
}
async fn admin_command() -> Result<bool, Box<dyn std::error::Error>> {
    use clap::Parser;
    use std::io::Read;
    let Some(command) = AdminCli::parse().command else {
        return Ok(false);
    };
    let path =
        env::var("COLD_VTL_DATABASE").unwrap_or_else(|_| "/var/lib/cold-vtl/catalog.db".into());
    let pool = auth::admin_pool(std::path::Path::new(&path)).await?;
    let mut password = String::new();
    std::io::stdin().take(1026).read_to_string(&mut password)?;
    let password = password.trim_end_matches(['\r', '\n']).to_owned();
    match command {
        AdminCommand::CreateUser { username, role } => {
            auth::create_local_user(&pool, &username, password, &role)
                .await
                .map_err(|e| e.message)?
        }
        AdminCommand::ResetPassword { username } => {
            let hash = auth::hash_password(password).await.map_err(|e| e.message)?;
            let mut tx = pool.begin().await?;
            let id: String = sqlx::query_scalar("UPDATE auth_users SET password_hash=?,disabled=0,revision=revision+1 WHERE username=? AND issuer IS NULL RETURNING id").bind(hash).bind(username).fetch_one(&mut *tx).await?;
            sqlx::query("DELETE FROM auth_credentials WHERE user_id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
    }
    println!("Local account updated.");
    Ok(true)
}

fn configure_export(
    command: &mut tokio::process::Command,
    cartridge: &Cartridge,
    source: &std::path::Path,
) {
    command
        .arg("export-directory")
        .arg("--segment-size-mib")
        .arg(cartridge.segment_size_mib.to_string())
        .arg("--multipart-part-size-mib")
        .arg(cartridge.multipart_part_size_mib.to_string())
        .arg("--barcode")
        .arg(cartridge.barcode.as_str())
        .arg("--generation")
        .arg(cartridge.generation.to_string())
        .arg("--source")
        .arg(source);
}
