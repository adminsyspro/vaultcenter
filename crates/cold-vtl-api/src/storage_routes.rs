//! Explicit VTL routing for connection-level buckets and remote recovery.
use crate::{ApiError, AppState, auth::Principal};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
};

pub async fn recovery_inventory(
    State(state): State<AppState>,
    Path(id): Path<String>,
    query: Query<crate::destinations::Selection>,
) -> Result<Response, ApiError> {
    Ok(
        crate::recovery::inventory(State(crate::vtls::scoped_state(state, id).await?), query)
            .await?
            .into_response(),
    )
}

pub async fn recovery_check(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: Extension<Principal>,
    input: Json<crate::recovery::Request>,
) -> Result<Response, ApiError> {
    Ok(crate::recovery::check(
        State(crate::vtls::scoped_state(state, id).await?),
        principal,
        input,
    )
    .await?
    .into_response())
}

pub async fn recover(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: Extension<Principal>,
    input: Json<crate::recovery::Request>,
) -> Result<Response, ApiError> {
    Ok(crate::recovery::start(
        State(crate::vtls::scoped_state(state, id).await?),
        principal,
        input,
    )
    .await?
    .into_response())
}

pub async fn connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: Extension<Principal>,
) -> Result<Response, ApiError> {
    Ok(crate::s3_settings::buckets::get_connection(
        State(crate::vtls::scoped_state(state, id).await?),
        principal,
    )
    .await?
    .into_response())
}

pub async fn save_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: Extension<Principal>,
    input: Json<crate::s3_settings::buckets::ConnectionUpdate>,
) -> Result<Response, ApiError> {
    Ok(crate::s3_settings::buckets::save_connection(
        State(crate::vtls::scoped_state(state, id).await?),
        principal,
        input,
    )
    .await?
    .into_response())
}

pub async fn discover(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: Extension<Principal>,
) -> Result<Response, ApiError> {
    Ok(crate::s3_settings::buckets::discover(
        State(crate::vtls::scoped_state(state, id).await?),
        principal,
    )
    .await?
    .into_response())
}

pub async fn attach(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: Extension<Principal>,
    input: Json<crate::s3_settings::buckets::Attach>,
) -> Result<Response, ApiError> {
    Ok(crate::s3_settings::buckets::attach(
        State(crate::vtls::scoped_state(state, id).await?),
        principal,
        input,
    )
    .await?
    .into_response())
}

pub async fn create(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: Extension<Principal>,
    input: Json<crate::s3_settings::buckets::Create>,
) -> Result<Response, ApiError> {
    Ok(crate::s3_settings::buckets::create(
        State(crate::vtls::scoped_state(state, id).await?),
        principal,
        input,
    )
    .await?
    .into_response())
}

pub async fn retire(
    State(state): State<AppState>,
    Path((id, destination)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    Ok(crate::s3_settings::retire::retire(
        State(crate::vtls::scoped_state(state, id).await?),
        Path(destination),
    )
    .await?
    .into_response())
}

pub async fn delete_copy(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: Extension<Principal>,
    input: Json<crate::remote_delete::Request>,
) -> Result<Response, ApiError> {
    Ok(crate::remote_delete::start(
        State(crate::vtls::scoped_state(state, id).await?),
        principal,
        input,
    )
    .await?
    .into_response())
}

pub async fn schedule(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: Extension<Principal>,
) -> Result<Response, ApiError> {
    Ok(crate::s3_settings::schedule::get(
        State(crate::vtls::scoped_state(state, id).await?),
        principal,
    )
    .await?
    .into_response())
}

pub async fn save_schedule(
    State(state): State<AppState>,
    Path(id): Path<String>,
    principal: Extension<Principal>,
    input: Json<crate::s3_settings::schedule::ScheduleUpdate>,
) -> Result<Response, ApiError> {
    Ok(crate::s3_settings::schedule::put(
        State(crate::vtls::scoped_state(state, id).await?),
        principal,
        input,
    )
    .await?
    .into_response())
}
