//! Scoped, administrator-only projections of root-owned transfer configuration.
use crate::{
    ApiError, AppState,
    auth::Principal,
    provisioning::{admin, invoke},
};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use serde_json::{Value, json};

pub async fn status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let state = crate::vtls::scoped_state(state, id).await?;
    invoke(&state, "bandwidth-status", json!({}))
        .await
        .map(Json)
}
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Extension(principal): Extension<Principal>,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let state = crate::vtls::scoped_state(state, id).await?;
    invoke(&state, "bandwidth-update", input).await.map(Json)
}
pub async fn connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let state = crate::vtls::scoped_state(state, id).await?;
    invoke(&state, "pbs-connection-status", json!({}))
        .await
        .map(Json)
}
pub async fn client(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let state = crate::vtls::scoped_state(state, id).await?;
    invoke(&state, "client-status", json!({})).await.map(Json)
}
pub async fn update_client(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Extension(principal): Extension<Principal>,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let state = crate::vtls::scoped_state(state, id).await?;
    invoke(&state, "client-update", input).await.map(Json)
}
pub async fn update_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Extension(principal): Extension<Principal>,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let state = crate::vtls::scoped_state(state, id).await?;
    invoke(&state, "pbs-connection-update", input)
        .await
        .map(Json)
}
