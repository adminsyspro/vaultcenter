use crate::{ApiError, AppState, catalog::Job};
use axum::{
    Json,
    extract::{Path, Query, Request, State},
    middleware::Next,
    response::Response,
};
use cold_vtl_core::{Barcode, Cartridge, VtlId};
use serde::{Deserialize, Serialize};
use sqlx::Row;

#[derive(Serialize)]
pub struct Library {
    id: String,
    name: String,
    lifecycle: String,
    provisioning_revision: i64,
    storage: Option<LibraryStorage>,
}
#[derive(Serialize, Deserialize)]
struct LibraryStorage {
    provider: String,
    bucket: String,
    prefix: String,
}
fn library(row: sqlx::sqlite::SqliteRow) -> Result<Library, ApiError> {
    let storage: Option<String> = row.try_get("storage").map_err(ApiError::conflict)?;
    Ok(Library {
        storage: storage
            .map(|value| {
                serde_json::from_str(&value).map_err(|_| ApiError::conflict("Invalid S3 identity."))
            })
            .transpose()?,
        id: row.try_get("id").map_err(ApiError::conflict)?,
        name: row.try_get("name").map_err(ApiError::conflict)?,
        lifecycle: row.try_get("lifecycle").map_err(ApiError::conflict)?,
        provisioning_revision: row
            .try_get("provisioning_revision")
            .map_err(ApiError::conflict)?,
    })
}
pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<Library>>, ApiError> {
    crate::s3_settings::destinations::initialize_legacy(&state).await?;
    sqlx::query("SELECT id,name,lifecycle,provisioning_revision,(SELECT identity FROM storage_destinations d WHERE d.vtl_id=vtls.id AND d.active=1) AS storage FROM vtls ORDER BY created_at,id")
        .fetch_all(&state.catalog.pool())
        .await
        .map_err(ApiError::conflict)?
        .into_iter()
        .map(library)
        .collect::<Result<Vec<_>, _>>()
        .map(Json)
}
pub async fn show(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Library>, ApiError> {
    let id: VtlId = id.parse().map_err(ApiError::bad_request)?;
    let row = sqlx::query("SELECT id,name,lifecycle,provisioning_revision,(SELECT identity FROM storage_destinations d WHERE d.vtl_id=vtls.id AND d.active=1) AS storage FROM vtls WHERE id=?")
        .bind(id.to_string())
        .fetch_optional(&state.catalog.pool())
        .await
        .map_err(ApiError::conflict)?
        .ok_or_else(|| ApiError::not_found("VTL does not exist"))?;
    library(row).map(Json)
}
async fn catalog(state: &AppState, id: String) -> Result<crate::catalog::Catalog, ApiError> {
    let id = id.parse::<VtlId>().map_err(ApiError::bad_request)?;
    Ok(state.catalog.for_vtl(id).await?)
}
pub async fn cartridges(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<crate::catalog::CartridgeView>>, ApiError> {
    Ok(Json(
        catalog(&state, id).await?.cartridge_views(None).await?,
    ))
}
pub async fn cartridge(
    State(state): State<AppState>,
    Path((id, barcode)): Path<(String, String)>,
) -> Result<Json<crate::catalog::CartridgeView>, ApiError> {
    let barcode = Barcode::parse(barcode).map_err(ApiError::bad_request)?;
    Ok(Json(
        catalog(&state, id)
            .await?
            .cartridge_views(Some(&barcode))
            .await?
            .pop()
            .ok_or_else(|| ApiError::not_found("cartridge does not exist in this VTL"))?,
    ))
}
pub async fn jobs(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<crate::JobsQuery>,
) -> Result<Json<Vec<Job>>, ApiError> {
    Ok(Json(query.read(&catalog(&state, id).await?).await?))
}
pub async fn job(
    State(state): State<AppState>,
    Path((id, job_id)): Path<(String, String)>,
) -> Result<Json<Job>, ApiError> {
    Ok(Json(
        catalog(&state, id)
            .await?
            .job(&job_id)
            .await?
            .ok_or_else(|| ApiError::not_found("job does not exist in this VTL"))?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateLibrary {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameLibrary {
    name: String,
    expected_name: String,
}

/// Display metadata only: no hardware, storage or provisioning change.
pub async fn rename(
    State(state): State<AppState>,
    Path(id): Path<String>,
    axum::Extension(principal): axum::Extension<crate::auth::Principal>,
    Json(input): Json<RenameLibrary>,
) -> Result<Json<Library>, ApiError> {
    if principal.user.role != "admin" {
        return Err(ApiError {
            status: axum::http::StatusCode::FORBIDDEN,
            message: "administrator required".into(),
        });
    }
    let id = id
        .parse::<VtlId>()
        .map_err(ApiError::bad_request)?
        .to_string();
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "The name must contain between 1 and 100 characters, without control characters.",
        ));
    }
    let mut tx = state
        .catalog
        .pool()
        .begin()
        .await
        .map_err(ApiError::conflict)?;
    // Serialize with creation/other renames before checking uniqueness. Renaming
    // never waits for a running media job and does not alter runtime revisions.
    let found = sqlx::query("UPDATE vtls SET name=name WHERE id=?")
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    if found.rows_affected() == 0 {
        return Err(ApiError::not_found("VTL does not exist"));
    }
    let current: String = sqlx::query_scalar("SELECT name FROM vtls WHERE id=?")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    if current != input.expected_name {
        return Err(ApiError::conflict(
            "This library's name has changed. Refresh the list and retry.",
        ));
    }
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM vtls WHERE id<>?")
        .bind(&id)
        .fetch_all(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    if names
        .iter()
        .any(|other| other.to_lowercase() == name.to_lowercase())
    {
        return Err(ApiError::conflict("A library already has this name."));
    }
    sqlx::query("UPDATE vtls SET name=? WHERE id=?")
        .bind(name)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    let row=sqlx::query("SELECT id,name,lifecycle,provisioning_revision,(SELECT identity FROM storage_destinations d WHERE d.vtl_id=vtls.id AND d.active=1) AS storage FROM vtls WHERE id=?").bind(&id).fetch_one(&mut *tx).await.map_err(ApiError::conflict)?;
    let result = library(row)?;
    tx.commit().await.map_err(ApiError::conflict)?;
    Ok(Json(result))
}

/// Register only a draft. Provisioning and activation remain separate operations.
pub async fn create(
    State(state): State<AppState>,
    Json(input): Json<CreateLibrary>,
) -> Result<(axum::http::StatusCode, Json<Library>), ApiError> {
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "The name must contain between 1 and 100 characters, without control characters.",
        ));
    }
    let _lock = state.operations.lock().await;
    let mut tx = state
        .catalog
        .pool()
        .begin()
        .await
        .map_err(ApiError::conflict)?;
    // Acquire the writer lock before checking names, including other daemon connections.
    sqlx::query("UPDATE vtls SET provisioning_revision=provisioning_revision WHERE id='00000000-0000-4000-8000-000000000001'")
        .execute(&mut *tx).await.map_err(ApiError::conflict)?;
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM vtls")
        .fetch_all(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    if names
        .iter()
        .any(|existing| existing.to_lowercase() == name.to_lowercase())
    {
        return Err(ApiError::conflict("A library already has this name."));
    }
    let id = VtlId::new().to_string();
    sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,?,'draft')")
        .bind(&id)
        .bind(name)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    tx.commit().await.map_err(ApiError::conflict)?;
    if crate::vtl_runtime::isolated(&state) {
        let scoped = scoped_state(state.clone(), id.clone()).await?;
        crate::provisioning::invoke(&scoped, "bandwidth-initialize", serde_json::json!({})).await
            .map_err(|_| ApiError::conflict("Library created; limit initialisation not confirmed. Check the list and finish its provisioning."))?;
    }
    Ok((
        axum::http::StatusCode::CREATED,
        Json(Library {
            id,
            name: name.into(),
            lifecycle: "draft".into(),
            provisioning_revision: 1,
            storage: None,
        }),
    ))
}
/// Every old route, including raw SQL and helper-backed endpoints, fails closed
/// once the implicit single-library context becomes ambiguous.
pub async fn legacy_context(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    state.catalog.resolve_vtl(None).await?;
    if crate::vtl_runtime::isolated(&state) {
        return Err(ApiError::conflict(
            "In isolated mode, select a VTL explicitly.",
        ));
    }
    Ok(next.run(request).await)
}

pub(crate) async fn scoped_state(mut state: AppState, id: String) -> Result<AppState, ApiError> {
    state.catalog = catalog(&state, id).await?;
    state.operations = std::sync::Arc::new(state.operations.for_vtl(state.catalog.vtl_id().await?));
    Ok(state)
}
pub async fn s3_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: axum::Extension<crate::auth::Principal>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::s3_settings::get(State(scoped_state(state, id).await?), principal).await
}
pub async fn s3_apply(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: axum::Extension<crate::auth::Principal>,
    input: Json<crate::s3_settings::Update>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::s3_settings::apply(State(scoped_state(state, id).await?), principal, input).await
}
pub async fn s3_test(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: axum::Extension<crate::auth::Principal>,
    input: Json<crate::s3_settings::Update>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::s3_settings::test(State(scoped_state(state, id).await?), principal, input).await
}
pub async fn destinations(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: axum::Extension<crate::auth::Principal>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::s3_settings::destinations::list(State(scoped_state(state, id).await?), principal).await
}

// Until the new hardware controller is available, only the installed legacy VTL
// may invoke device/cloud workers. Empty drafts must never inherit its adapters.
pub(crate) async fn installed_state(state: AppState, id: String) -> Result<AppState, ApiError> {
    let mut scoped = scoped_state(state, id).await?;
    let id = scoped.catalog.vtl_id().await?;
    if crate::vtl_runtime::isolated(&scoped) {
        let runtime = crate::vtl_runtime::runtime_for_catalog(&scoped.catalog, id).await?;
        let mut config = (*scoped.config).clone();
        config.media_root = runtime.media_root;
        config.restore_root = runtime.restore_root;
        scoped.config = std::sync::Arc::new(config);
        return Ok(scoped);
    }
    if id.to_string() != crate::s3_settings::destinations::LEGACY
        || scoped.catalog.resolve_vtl(None).await? != id
    {
        return Err(ApiError::conflict(
            "This VTL is not activated yet. Configure its storage before provisioning.",
        ));
    }
    Ok(scoped)
}
pub async fn status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let catalog = catalog(&state, id).await?;
    let root: Option<String> = sqlx::query_scalar("SELECT media_root FROM vtls WHERE id=?")
        .bind(catalog.vtl_id().await?.to_string())
        .fetch_one(&catalog.pool())
        .await
        .map_err(ApiError::conflict)?;
    Ok(Json(
        serde_json::json!({"status":"ok","cartridges":catalog.list_cartridges().await?.len(),"jobs":catalog.list_jobs().await?.len(),"media_root":root.unwrap_or_default()}),
    ))
}
pub async fn generations(
    State(state): State<AppState>,
    axum::extract::Query(selection): axum::extract::Query<crate::destinations::Selection>,
    Path((id, barcode)): Path<(String, String)>,
) -> Result<Json<Vec<cold_vtl_core::RemoteGeneration>>, ApiError> {
    crate::operations::generations(
        State(crate::destinations::select(scoped_state(state, id).await?, selection).await?),
        Path(barcode),
    )
    .await
}

pub async fn storage(
    State(state): State<AppState>,
    axum::extract::Query(selection): axum::extract::Query<crate::destinations::Selection>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(crate::storage::status(State(
        crate::destinations::select(installed_state(state, id).await?, selection).await?,
    ))
    .await?
    .into_response())
}

pub async fn access(
    State(state): State<AppState>,
    axum::extract::Query(selection): axum::extract::Query<crate::destinations::Selection>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(crate::storage::access(State(
        crate::destinations::select(installed_state(state, id).await?, selection).await?,
    ))
    .await?
    .into_response())
}

pub async fn maintenance(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(
        crate::maintenance::status(State(installed_state(state, id).await?))
            .await?
            .into_response(),
    )
}

pub async fn events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(
        crate::maintenance::events(State(installed_state(state, id).await?))
            .await?
            .into_response(),
    )
}

pub async fn enter(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: axum::Extension<crate::auth::Principal>,
    body: Option<Json<crate::maintenance::EnterRequest>>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(
        crate::maintenance::enter(State(installed_state(state, id).await?), principal, body)
            .await?
            .into_response(),
    )
}

pub async fn leave(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: axum::Extension<crate::auth::Principal>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(
        crate::maintenance::leave(State(installed_state(state, id).await?), principal)
            .await?
            .into_response(),
    )
}

pub async fn reintegrations(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(
        crate::reintegration::status(State(installed_state(state, id).await?))
            .await?
            .into_response(),
    )
}

pub async fn reintegrate(
    State(state): State<AppState>,
    Path((id, resource)): Path<(String, String)>,
    principal: axum::Extension<crate::auth::Principal>,
    body: Json<crate::reintegration::Request>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(crate::reintegration::start(
        State(installed_state(state, id).await?),
        Path(resource),
        principal,
        body,
    )
    .await?
    .into_response())
}

pub async fn rollback(
    State(state): State<AppState>,
    Path((id, resource)): Path<(String, String)>,
    principal: axum::Extension<crate::auth::Principal>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(crate::reintegration::rollback(
        State(installed_state(state, id).await?),
        Path(resource),
        principal,
    )
    .await?
    .into_response())
}

pub async fn archive(
    State(state): State<AppState>,
    axum::extract::Query(selection): axum::extract::Query<crate::destinations::Selection>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(crate::archive::overview(State(
        crate::destinations::select(installed_state(state, id).await?, selection).await?,
    ))
    .await?
    .into_response())
}

pub async fn archive_submit(
    State(state): State<AppState>,
    axum::extract::Query(selection): axum::extract::Query<crate::destinations::Selection>,
    Path(id): Path<String>,
    principal: axum::Extension<crate::auth::Principal>,
    body: Json<crate::archive::Confirmation>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(crate::archive::submit(
        State(crate::destinations::select(installed_state(state, id).await?, selection).await?),
        principal,
        body,
    )
    .await?
    .into_response())
}

pub async fn archive_refresh(
    State(state): State<AppState>,
    axum::extract::Query(selection): axum::extract::Query<crate::destinations::Selection>,
    Path(id): Path<String>,
    principal: axum::Extension<crate::auth::Principal>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(crate::archive::refresh(
        State(crate::destinations::select(installed_state(state, id).await?, selection).await?),
        principal,
    )
    .await?
    .into_response())
}

pub async fn recall_submit(
    State(state): State<AppState>,
    axum::extract::Query(selection): axum::extract::Query<crate::destinations::Selection>,
    Path(id): Path<String>,
    principal: axum::Extension<crate::auth::Principal>,
    body: Json<crate::archive::RecallConfirmation>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(crate::archive::recall_submit(
        State(crate::destinations::select(installed_state(state, id).await?, selection).await?),
        principal,
        body,
    )
    .await?
    .into_response())
}

pub async fn seal(
    State(state): State<AppState>,
    Path((id, resource)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(
        crate::seal_cartridge(State(installed_state(state, id).await?), Path(resource))
            .await?
            .into_response(),
    )
}

pub async fn export(
    State(state): State<AppState>,
    axum::extract::Query(selection): axum::extract::Query<crate::destinations::Selection>,
    Path((id, resource)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(crate::export_cartridge(
        State(crate::destinations::select(installed_state(state, id).await?, selection).await?),
        Path(resource),
    )
    .await?
    .into_response())
}

pub async fn reconcile(
    State(state): State<AppState>,
    axum::extract::Query(selection): axum::extract::Query<crate::destinations::Selection>,
    Path((id, resource)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(crate::operations::reconcile(
        State(crate::destinations::select(installed_state(state, id).await?, selection).await?),
        Path(resource),
    )
    .await?
    .into_response())
}

pub async fn restore(
    State(state): State<AppState>,
    axum::extract::Query(selection): axum::extract::Query<crate::destinations::Selection>,
    Path((id, resource)): Path<(String, String)>,
    body: Json<crate::operations::RestoreRequest>,
) -> Result<Response, ApiError> {
    use axum::response::IntoResponse;
    Ok(crate::operations::restore(
        State(crate::destinations::select(installed_state(state, id).await?, selection).await?),
        Path(resource),
        body,
    )
    .await?
    .into_response())
}

pub async fn buckets(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::destinations::buckets(State(scoped_state(state, id).await?)).await
}

pub async fn history(
    State(state): State<AppState>,
    Path(id): Path<String>,
    query: Query<crate::history::HistoryQuery>,
) -> Result<axum::response::Response, ApiError> {
    crate::history::history(State(scoped_state(state, id).await?), query).await
}

pub async fn telemetry(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::telemetry::Telemetry>, ApiError> {
    let id = id.parse::<VtlId>().map_err(ApiError::bad_request)?;
    state.catalog.for_vtl(id).await?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    Ok(Json(
        crate::telemetry::read_snapshot(state.config.telemetry_dir.as_deref(), id, now).await,
    ))
}

pub async fn telemetry_history(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<crate::telemetry::history::HistoryQuery>,
) -> Result<Json<crate::telemetry::history::History>, ApiError> {
    let id = id.parse::<VtlId>().map_err(ApiError::bad_request)?;
    state.catalog.for_vtl(id).await?;
    Ok(Json(
        crate::telemetry::history::read_history(
            &state.catalog,
            id,
            state.config.telemetry_retention_days,
            query,
            crate::telemetry::history::now_ms(),
        )
        .await?,
    ))
}

pub async fn cartridge_options(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::cartridges::Options>, ApiError> {
    crate::cartridges::options(State(installed_state(state, id).await?)).await
}
pub async fn create_cartridge(
    State(state): State<AppState>,
    Path(id): Path<String>,
    input: Json<crate::cartridges::Create>,
) -> Result<(axum::http::StatusCode, Json<Cartridge>), ApiError> {
    crate::cartridges::create(State(installed_state(state, id).await?), input).await
}

pub async fn create_cartridge_batch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<crate::cartridges::BatchCreate>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::cartridges::batch(State(scoped_state(state, id).await?), Json(input)).await
}

pub async fn deletion_options(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::deletion::options(State(installed_state(state, id).await?)).await
}
pub async fn deletion_preview(
    State(state): State<AppState>,
    Path(id): Path<String>,
    input: Json<crate::deletion::Selection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::deletion::preview(State(installed_state(state, id).await?), input).await
}
pub async fn delete_local(
    State(state): State<AppState>,
    Path(id): Path<String>,
    input: Json<crate::deletion::Delete>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::deletion::delete(State(installed_state(state, id).await?), input).await
}
