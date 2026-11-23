//! Explicit root-controller provisioning; storage readiness never activates devices.
use crate::{ApiError, AppState, auth::Principal};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prepare {
    revision: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Apply {
    token: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activate {}
pub(crate) fn admin(principal: &Principal) -> Result<(), ApiError> {
    if principal.user.role != "admin" {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            message: "administrator required".into(),
        });
    }
    Ok(())
}
pub(crate) async fn invoke(
    state: &AppState,
    action: &str,
    request: Value,
) -> Result<Value, ApiError> {
    let id = state.catalog.vtl_id().await?.to_string();
    if !crate::vtl_runtime::isolated(state) {
        if action == "status" {
            return Ok(
                json!({"vtl_id":id,"phase":"unavailable","qualified":false,"message":"Isolated provisioning must be installed on this appliance."}),
            );
        }
        return Err(ApiError::conflict(
            "Isolated provisioning is not installed.",
        ));
    }
    let mut child = tokio::process::Command::new("/usr/local/libexec/cold-vtl-vtl")
        .arg(action)
        .arg("--vtl")
        .arg(&id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(
            action == "status"
                || action.starts_with("bandwidth-")
                || action.starts_with("pbs-connection-")
                || action.starts_with("client-"),
        )
        .spawn()
        .map_err(ApiError::conflict)?;
    let work = async {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ApiError::conflict("Controller input unavailable."))?;
        stdin
            .write_all(request.to_string().as_bytes())
            .await
            .map_err(ApiError::conflict)?;
        drop(stdin);
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .ok_or_else(|| ApiError::conflict("Controller output unavailable."))?
            .take(65537)
            .read_to_end(&mut bytes)
            .await
            .map_err(ApiError::conflict)?;
        if bytes.len() > 65536 {
            return Err(ApiError::conflict("Controller response too large."));
        }
        let result: Value = serde_json::from_slice(&bytes)
            .map_err(|_| ApiError::conflict("Invalid controller response."))?;
        if !child.wait().await.map_err(ApiError::conflict)?.success() {
            return Err(ApiError::conflict(
                result["error"].as_str().unwrap_or("Provisioning refused."),
            ));
        }
        if result["vtl_id"] != id {
            return Err(ApiError::conflict(
                "Controller response belongs to another VTL.",
            ));
        }
        Ok(result)
    };
    tokio::time::timeout(
        Duration::from_secs(
            if action == "status"
                || action.starts_with("bandwidth-")
                || action.starts_with("pbs-connection-")
                || action.starts_with("client-")
            {
                12
            } else {
                300
            },
        ),
        work,
    )
    .await
    .map_err(|_| {
        ApiError::conflict(
            "Result not confirmed. Check provisioning before resuming the same request.",
        )
    })?
}
pub async fn status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let state = crate::vtls::scoped_state(state, id).await?;
    invoke(&state, "status", json!({})).await.map(Json)
}
pub async fn prepare(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Extension(principal): Extension<Principal>,
    Json(input): Json<Prepare>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let state = crate::vtls::scoped_state(state, id).await?;
    let _lock = state
        .operations
        .try_lock()
        .map_err(|_| ApiError::conflict("An operation of this VTL is in progress."))?;
    invoke(&state, "prepare", json!({"revision":input.revision}))
        .await
        .map(Json)
}
pub async fn apply(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Extension(principal): Extension<Principal>,
    Json(input): Json<Apply>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let state = crate::vtls::scoped_state(state, id).await?;
    if input.token.len() != 32 || !input.token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request("Prepare this VTL again."));
    }
    let _lock = state
        .operations
        .try_lock()
        .map_err(|_| ApiError::conflict("An operation of this VTL is in progress."))?;
    invoke(
        &state,
        "apply",
        json!({"token":input.token,"actor":principal.user.username}),
    )
    .await
    .map(Json)
}
pub async fn activate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Extension(principal): Extension<Principal>,
    Json(_input): Json<Activate>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let state = crate::vtls::scoped_state(state, id).await?;
    let _lock = state
        .operations
        .try_lock()
        .map_err(|_| ApiError::conflict("An operation of this VTL is in progress."))?;
    invoke(&state, "activate", json!({"actor":principal.user.username}))
        .await
        .map(Json)
}
