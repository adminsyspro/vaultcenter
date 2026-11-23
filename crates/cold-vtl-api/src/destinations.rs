use crate::{ApiError, AppState};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
#[derive(Default, Deserialize)]
pub struct Selection {
    pub destination_id: Option<String>,
}
pub async fn select(mut state: AppState, selection: Selection) -> Result<AppState, ApiError> {
    crate::s3_settings::destinations::initialize_legacy(&state).await?;
    if let Some(id) = selection.destination_id {
        state.catalog = state
            .catalog
            .for_destination(id)
            .await
            .map_err(|_| ApiError::not_found("Destination not found in this VTL."))?;
    }
    Ok(state)
}
pub async fn buckets(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    crate::s3_settings::destinations::initialize_legacy(&state).await?;
    use sqlx::Row;
    let rows = sqlx::query("SELECT d.id,d.identity,d.active,d.created_at,(SELECT MAX(revision) FROM storage_destination_revisions WHERE destination_id=d.id) AS revision,(SELECT COUNT(*) FROM remote_generations WHERE destination_id=d.id AND vtl_id=d.vtl_id) AS copies,(SELECT observed_status FROM archive_requests WHERE destination_id=d.id AND vtl_id=d.vtl_id AND observed_status IS NOT NULL ORDER BY updated_at DESC LIMIT 1) AS observed,(SELECT phase FROM bucket_rotations WHERE (source_destination_id=d.id OR next_destination_id=d.id) AND vtl_id=d.vtl_id ORDER BY created_at DESC LIMIT 1) AS rotation FROM storage_destinations d WHERE d.vtl_id=? AND d.identity IS NOT NULL AND d.retired_at IS NULL ORDER BY d.created_at,d.id")
        .bind(state.catalog.vtl_id().await?.to_string()).fetch_all(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    Ok(Json(json!(rows.iter().map(|r| {
        let identity: Value = serde_json::from_str(r.get("identity")).unwrap_or(Value::Null);
        json!({"id":r.get::<String,_>("id"),"bucket":identity["bucket"],"active":r.get::<bool,_>("active"),"created_at":r.get::<String,_>("created_at"),"configuration_revision":r.get::<Option<i64>,_>("revision"),"copy_count":r.get::<i64,_>("copies"),"observed_status":r.get::<Option<String>,_>("observed"),"rotation_phase":r.get::<Option<String>,_>("rotation")})
    }).collect::<Vec<_>>())) )
}
pub async fn status(
    State(state): State<AppState>,
    Query(selection): Query<Selection>,
) -> Result<Response, ApiError> {
    Ok(
        crate::storage::status(State(select(state, selection).await?))
            .await?
            .into_response(),
    )
}
pub async fn access(
    State(state): State<AppState>,
    Query(selection): Query<Selection>,
) -> Result<Response, ApiError> {
    Ok(
        crate::storage::access(State(select(state, selection).await?))
            .await?
            .into_response(),
    )
}
pub async fn overview(
    State(state): State<AppState>,
    Query(selection): Query<Selection>,
) -> Result<Response, ApiError> {
    Ok(
        crate::archive::overview(State(select(state, selection).await?))
            .await?
            .into_response(),
    )
}
pub async fn refresh(
    State(state): State<AppState>,
    Query(selection): Query<Selection>,
    principal: Extension<crate::auth::Principal>,
) -> Result<Response, ApiError> {
    Ok(
        crate::archive::refresh(State(select(state, selection).await?), principal)
            .await?
            .into_response(),
    )
}
pub async fn submit(
    State(state): State<AppState>,
    Query(selection): Query<Selection>,
    principal: Extension<crate::auth::Principal>,
    body: Json<crate::archive::Confirmation>,
) -> Result<Response, ApiError> {
    Ok(
        crate::archive::submit(State(select(state, selection).await?), principal, body)
            .await?
            .into_response(),
    )
}
pub async fn recall_submit(
    State(state): State<AppState>,
    Query(selection): Query<Selection>,
    principal: Extension<crate::auth::Principal>,
    body: Json<crate::archive::RecallConfirmation>,
) -> Result<Response, ApiError> {
    Ok(
        crate::archive::recall_submit(State(select(state, selection).await?), principal, body)
            .await?
            .into_response(),
    )
}
pub async fn generations(
    State(state): State<AppState>,
    Query(selection): Query<Selection>,
    path: Path<String>,
) -> Result<Response, ApiError> {
    Ok(
        crate::operations::generations(State(select(state, selection).await?), path)
            .await?
            .into_response(),
    )
}
pub async fn reconcile(
    State(state): State<AppState>,
    Query(selection): Query<Selection>,
    path: Path<String>,
) -> Result<Response, ApiError> {
    Ok(
        crate::operations::reconcile(State(select(state, selection).await?), path)
            .await?
            .into_response(),
    )
}
pub async fn restore(
    State(state): State<AppState>,
    Query(selection): Query<Selection>,
    path: Path<String>,
    body: Json<crate::operations::RestoreRequest>,
) -> Result<Response, ApiError> {
    Ok(
        crate::operations::restore(State(select(state, selection).await?), path, body)
            .await?
            .into_response(),
    )
}
pub async fn export(
    State(state): State<AppState>,
    Query(selection): Query<Selection>,
    path: Path<String>,
) -> Result<Response, ApiError> {
    Ok(
        crate::export_cartridge(State(select(state, selection).await?), path)
            .await?
            .into_response(),
    )
}
