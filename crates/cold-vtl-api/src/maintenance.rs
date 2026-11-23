use crate::{ApiError, AppState, auth::Principal};
use axum::{Extension, Json, extract::State};
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio::io::AsyncWriteExt;

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct EnterRequest {
    #[serde(default)]
    pub confirm_idle: bool,
}

async fn invoke(
    state: &AppState,
    action: &str,
    actor: Option<&str>,
    confirm_idle: bool,
) -> Result<Value, ApiError> {
    let path = state
        .config
        .maintenance_helper
        .as_ref()
        .ok_or_else(|| ApiError::conflict("automated maintenance is not configured"))?;
    let transition = matches!(action, "enter" | "leave");
    let mut child = crate::vtl_runtime::helper_command(state, path)
        .await?
        .arg(action)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Transitions own a durable journal and a process lock. Do not interrupt recovery
        // half-way through if the HTTP client or API daemon disappears.
        .kill_on_drop(!transition)
        .spawn()
        .map_err(ApiError::conflict)?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(
                json!({"actor":actor,"confirm_idle":confirm_idle})
                    .to_string()
                    .as_bytes(),
            )
            .await
            .map_err(ApiError::conflict)?;
    }
    let output = tokio::time::timeout(
        // Includes the drive's 240 s graceful stop and the other service/lease checks.
        Duration::from_secs(if transition { 600 } else { 12 }),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| {
        ApiError::conflict(
            "maintenance adapter timed out; inspect maintenance status before retrying",
        )
    })?
    .map_err(ApiError::conflict)?;
    let body: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| ApiError::conflict("maintenance adapter returned an invalid response"))?;
    if !output.status.success() {
        return Err(ApiError::conflict(
            body.get("error")
                .and_then(Value::as_str)
                .unwrap_or("maintenance operation failed"),
        ));
    }
    Ok(body)
}

pub async fn check(state: &AppState) -> Result<(), ApiError> {
    crate::tape_storage::ensure_available(state).await?;
    let result = invoke(state, "check", None, false).await?;
    if result.get("phase").and_then(Value::as_str) != Some("maintenance")
        || result.get("ready").and_then(Value::as_bool) != Some(true)
    {
        return Err(ApiError::conflict(
            "enter managed maintenance before changing local media",
        ));
    }
    Ok(())
}

pub async fn status(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    if state.config.maintenance_helper.is_none() {
        return Ok(Json(
            json!({"configured":false,"phase":"unconfigured","ready":false}),
        ));
    }
    Ok(Json(invoke(&state, "status", None, false).await?))
}

/// Entering or leaving maintenance waits for every VTL job and for OVH work
/// that can change the active destination, never for an older bucket's archive.
async fn ready_for_transition(state: &AppState) -> Result<(), ApiError> {
    if state
        .catalog
        .list_jobs()
        .await?
        .iter()
        .any(|job| matches!(job.status.as_str(), "queued" | "running"))
    {
        return Err(ApiError::conflict(
            "wait for every active VTL job before changing maintenance state",
        ));
    }
    crate::archive::media_idle(state).await
}

async fn transition(
    state: AppState,
    principal: Principal,
    action: &str,
    confirm_idle: bool,
) -> Result<Json<Value>, ApiError> {
    if principal.user.role != "admin" {
        return Err(ApiError {
            status: axum::http::StatusCode::FORBIDDEN,
            message: "administrator required".into(),
        });
    }
    let _lock = state
        .operations
        .try_lock()
        .map_err(|_| ApiError::conflict("another operation is being prepared"))?;
    ready_for_transition(&state).await?;
    // The helper independently checks for surviving worker processes after a daemon crash.
    let result = invoke(&state, action, Some(&principal.user.username), confirm_idle).await;
    let message = match &result {
        Ok(value) => value.to_string(),
        Err(error) => error.message.clone(),
    };
    // Journal records actor, result and time without passwords or tokens.
    sqlx::query("INSERT INTO maintenance_events (actor,action,success,message) VALUES (?,?,?,?)")
        .bind(&principal.user.id)
        .bind(action)
        .bind(result.is_ok())
        .bind(message)
        .execute(&state.catalog.pool())
        .await
        .map_err(|_| {
            ApiError::conflict(
                "maintenance finished but its audit record could not be saved; inspect status",
            )
        })?;
    result.map(Json)
}
pub async fn enter(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    body: Option<Json<EnterRequest>>,
) -> Result<Json<Value>, ApiError> {
    // A missing or malformed body means "not confirmed", the safe default.
    let confirm = body.map(|Json(b)| b.confirm_idle).unwrap_or(false);
    transition(state, principal, "enter", confirm).await
}
pub async fn leave(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    transition(state, principal, "leave", false).await
}
pub async fn events(State(state): State<AppState>) -> Result<Json<Vec<Value>>, ApiError> {
    use sqlx::Row;
    let rows=sqlx::query("SELECT e.id,e.action,e.success,e.message,e.created_at,u.username FROM maintenance_events e LEFT JOIN auth_users u ON u.id=e.actor ORDER BY e.id DESC LIMIT 30").fetch_all(&state.catalog.pool()).await.map_err(|_| ApiError::conflict("maintenance history unavailable"))?;
    Ok(Json(rows.iter().map(|r|json!({"id":r.get::<i64,_>("id"),"action":r.get::<String,_>("action"),"success":r.get::<bool,_>("success"),"message":r.get::<String,_>("message"),"created_at":r.get::<String,_>("created_at"),"actor":r.get::<Option<String>,_>("username")})).collect()))
}

#[cfg(test)]
mod tests {
    use super::EnterRequest;
    #[test]
    fn enter_request_defaults_and_rejects_unknown_fields() {
        let empty: EnterRequest = serde_json::from_str("{}").unwrap();
        assert!(!empty.confirm_idle);
        let yes: EnterRequest = serde_json::from_str(r#"{"confirm_idle":true}"#).unwrap();
        assert!(yes.confirm_idle);
        assert!(serde_json::from_str::<EnterRequest>(r#"{"confirm":true}"#).is_err());
    }

    #[tokio::test]
    async fn local_media_and_maintenance_ignore_an_older_bucket_archive() {
        use crate::s3_settings::test_support::{older_bucket_archive, state_fixture};
        let (state, root) = state_fixture().await;
        older_bucket_archive(&state, "uncertain").await;
        // Archiving an older bucket reads and locks no local cartridge.
        crate::cartridges::idle(&state).await.unwrap();
        super::ready_for_transition(&state).await.unwrap();
        // An uncertain archive of the active bucket still blocks both.
        let vtl = state.catalog.vtl_id().await.unwrap().to_string();
        let active = state.catalog.destination().await.unwrap().id;
        sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id,destination_id) VALUES('active-archive','archive_submit','current-bucket','failed',?,?)")
            .bind(&vtl).bind(&active).execute(&state.catalog.pool()).await.unwrap();
        sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,operation,vtl_id,destination_id) VALUES('active-archive','active-archive','uncertain','{}','archive',?,?)")
            .bind(&vtl).bind(&active).execute(&state.catalog.pool()).await.unwrap();
        assert!(crate::cartridges::idle(&state).await.is_err());
        assert!(super::ready_for_transition(&state).await.is_err());
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }
}
