//! Host storage configuration; paths are validated on the host, not in Docker.
use crate::{ApiError, AppState, auth::Principal};
use axum::{Extension, Json, extract::State, http::StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Apply {
    path: String,
    revision: u64,
    request_id: String,
    check_id: String,
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
fn validate(path: &str) -> Result<(), ApiError> {
    if !path.starts_with('/')
        || path.len() > 4096
        || path.chars().any(char::is_control)
        || path.split('/').any(|p| p == "..")
    {
        return Err(ApiError::bad_request(
            "Enter an absolute path to a folder on the VM.",
        ));
    }
    Ok(())
}
async fn invoke(path: &Path, value: Value) -> Result<Value, ApiError> {
    let failure = || {
        ApiError::conflict(
            "Storage host service unavailable. Check the VM, then check the status before retrying.",
        )
    };
    tokio::time::timeout(Duration::from_secs(12), async {
        let mut stream = UnixStream::connect(path).await.map_err(|_| failure())?;
        let mut payload = serde_json::to_vec(&value).map_err(|_| failure())?;
        payload.push(b'\n');
        stream.write_all(&payload).await.map_err(|_| failure())?;
        let mut data = Vec::new();
        BufReader::new(stream.take(65537))
            .read_until(b'\n', &mut data)
            .await
            .map_err(|_| failure())?;
        if data.len() > 65536 || data.last() != Some(&b'\n') {
            return Err(failure());
        }
        let response: Value = serde_json::from_slice(&data).map_err(|_| failure())?;
        if response["ok"] != true {
            return Err(ApiError::conflict(
                response["error"]["message"]
                    .as_str()
                    .unwrap_or("The storage request was refused."),
            ));
        }
        if !response["result"].is_object() {
            return Err(failure());
        }
        Ok(response["result"].clone())
    })
    .await
    .map_err(|_| failure())?
}
async fn invoke_scoped(
    path: &Path,
    id: cold_vtl_core::VtlId,
    mut value: Value,
) -> Result<Value, ApiError> {
    value["vtl_id"] = json!(id.to_string());
    let response = invoke(path, value).await?;
    if response["vtl_id"] != id.to_string() {
        return Err(ApiError::conflict(
            "Storage response belongs to another VTL.",
        ));
    }
    Ok(response)
}
async fn legacy_has_data(state: &AppState) -> Result<bool, ApiError> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cartridges WHERE vtl_id=?)")
        .bind(state.catalog.vtl_id().await?.to_string())
        .fetch_one(&state.catalog.pool())
        .await
        .map_err(ApiError::conflict)
}
async fn request(state: &AppState, value: Value) -> Result<Value, ApiError> {
    let id = state.catalog.vtl_id().await?;
    if crate::vtl_runtime::isolated(state) {
        return invoke_scoped(socket(state)?, id, value).await;
    }
    if id.to_string() != crate::s3_settings::destinations::LEGACY {
        return Err(ApiError::conflict(
            "Install isolated storage before configuring this VTL.",
        ));
    }
    let mut response = invoke(socket(state)?, value).await?;
    response["vtl_id"] = json!(id.to_string());
    response["data_locked"] = json!(legacy_has_data(state).await?);
    response["change_allowed"] = json!(false);
    response["change_lock_reason"] = json!("legacy_requires_migration");
    Ok(response)
}
fn socket(state: &AppState) -> Result<&Path, ApiError> {
    state
        .config
        .storage_socket
        .as_deref()
        .ok_or_else(|| ApiError::conflict("Install the storage host service to change this path."))
}
pub async fn ensure_available(state: &AppState) -> Result<(), ApiError> {
    let root = state.catalog.state_dir();
    if state.config.storage_socket.is_none() && !root.join("storage-managed").exists() {
        return Ok(());
    }
    let gate = if crate::vtl_runtime::isolated(state) {
        root.join("vtls")
            .join(state.catalog.vtl_id().await?.to_string())
    } else {
        root.to_path_buf()
    };
    if gate.join("storage-block").exists() {
        return Err(ApiError::conflict(
            "Storage blocked: finish or recover its configuration.",
        ));
    }
    let value = request(state, json!({"action":"status"})).await?;
    if value["phase"] != "ready" {
        return Err(ApiError::conflict(
            "Configure available storage before using tapes.",
        ));
    }
    Ok(())
}
pub async fn status(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    let id = state.catalog.vtl_id().await?;
    if !crate::vtl_runtime::isolated(&state)
        && id.to_string() != crate::s3_settings::destinations::LEGACY
    {
        return Err(ApiError::conflict(
            "Install isolated storage before configuring this VTL.",
        ));
    }
    if state.config.storage_socket.is_none()
        && !state.catalog.state_dir().join("storage-managed").exists()
    {
        return Ok(Json(
            json!({"vtl_id":id.to_string(),"data_locked":legacy_has_data(&state).await?,"change_allowed":false,"change_lock_reason":"legacy_requires_migration","mode":"legacy","phase":"ready","revision":0,"path":null,"capacity":null,"operation":null,"message":"Existing storage preserved. Install the host service to configure a path on the VM."}),
        ));
    }
    Ok(Json(request(&state, json!({"action":"status"})).await?))
}
pub async fn check(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(input): Json<Check>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    validate(&input.path)?;
    Ok(Json(
        request(&state, json!({"action":"check","path":input.path})).await?,
    ))
}
pub async fn apply(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(input): Json<Apply>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&principal)?;
    validate(&input.path)?;
    if input.request_id.is_empty()
        || input.request_id.len() > 128
        || input.check_id.is_empty()
        || input.check_id.len() > 128
    {
        return Err(ApiError::bad_request("Check the path before saving."));
    }
    let _lock = state
        .operations
        .try_lock()
        .map_err(|_| ApiError::conflict("An operation is in progress."))?;
    let value=request(&state,json!({"action":"apply","path":input.path,"revision":input.revision,"request_id":input.request_id,"check_id":input.check_id,"actor":principal.user.username})).await?;
    Ok((StatusCode::ACCEPTED, Json(value)))
}

pub async fn scoped_status(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    principal: Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    status(
        State(crate::vtls::scoped_state(state, id).await?),
        principal,
    )
    .await
}
pub async fn scoped_check(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    principal: Extension<Principal>,
    input: Json<Check>,
) -> Result<Json<Value>, ApiError> {
    check(
        State(crate::vtls::scoped_state(state, id).await?),
        principal,
        input,
    )
    .await
}
pub async fn scoped_apply(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    principal: Extension<Principal>,
    input: Json<Apply>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    apply(
        State(crate::vtls::scoped_state(state, id).await?),
        principal,
        input,
    )
    .await
}

#[cfg(test)]
mod tests;
