use crate::{ApiError, AppState, auth::Principal, catalog::Job};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    restore_job_id: String,
    #[serde(default)]
    replace: bool,
}

pub(crate) async fn invoke(
    state: &AppState,
    action: &str,
    request: Value,
) -> Result<Value, ApiError> {
    let helper = state
        .config
        .reintegration_helper
        .as_ref()
        .ok_or_else(|| ApiError::conflict("reintegration is not configured"))?;
    let mut child = crate::vtl_runtime::helper_command(state, helper)
        .await?
        .arg(action)
        // Status does not consume a request. Supplying a pipe races its early
        // exit and can turn a valid journal response into a broken-pipe error.
        .stdin(if action == "status" {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(action == "status")
        .spawn()
        .map_err(ApiError::conflict)?;
    if let Some(mut input) = child.stdin.take() {
        input
            .write_all(request.to_string().as_bytes())
            .await
            .map_err(ApiError::conflict)?;
    }
    // File copies can be large. The durable adapter owns recovery; HTTP submits
    // a job immediately and disconnection must not cancel an in-progress rename.
    let output = child.wait_with_output().await.map_err(ApiError::conflict)?;
    let body: Value = serde_json::from_slice(&output.stdout).map_err(ApiError::conflict)?;
    if !output.status.success() {
        return Err(ApiError::conflict(
            body.get("error")
                .and_then(Value::as_str)
                .unwrap_or("reintegration failed; inspect recovery status"),
        ));
    }
    Ok(body)
}

pub async fn status(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    if state.config.reintegration_helper.is_none() {
        return Ok(Json(
            json!({"configured":false,"pending":null,"operations":[]}),
        ));
    }
    Ok(Json(invoke(&state, "status", json!({})).await?))
}

fn valid_restore(job: &Job, barcode: &str) -> bool {
    let receipt = job
        .message
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    job.kind == "restore"
        && job.status == "completed"
        && job.subject == barcode
        && job.destination.is_some()
        && job.generation.is_some_and(|g| g > 0)
        && receipt.is_some_and(|r| {
            r["status"] == "restored"
                && r["barcode"] == barcode
                && r["generation"].as_i64() == job.generation
                && r["files"].as_array().is_some_and(|f| !f.is_empty())
        })
}

pub async fn start(
    State(state): State<AppState>,
    Path(barcode): Path<String>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<Request>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let _lock = state.operations.lock().await;
    ready(&state, &principal).await?;
    crate::deletion::require_local(&state, &barcode).await?;
    let cartridge = crate::operations::available_cartridge(&state, barcode.clone()).await?;
    let source = state
        .catalog
        .job(&request.restore_job_id)
        .await?
        .ok_or_else(|| ApiError::not_found("restore job not found"))?;
    if !valid_restore(&source, &barcode) {
        return Err(ApiError::conflict(
            "select a completed restore with an integrity receipt; older restores must be repeated",
        ));
    }
    if invoke(&state, "status", json!({})).await?["pending"].is_object() {
        return Err(ApiError::conflict(
            "recover the pending reintegration first",
        ));
    }
    let id = Uuid::new_v4().to_string();
    let job = state
        .catalog
        .reserve_job(
            &id,
            "reintegrate",
            &cartridge,
            source.generation.map(|g| g as u64),
            source.destination.as_deref(),
        )
        .await?;
    tokio::spawn(run(
        state.clone(),
        id.clone(),
        "reintegrate",
        json!({
            "job_id":id,"restore_job_id":source.id,"actor":principal.user.username,"replace":request.replace
        }),
    ));
    Ok((StatusCode::ACCEPTED, Json(job)))
}

pub async fn rollback(
    State(state): State<AppState>,
    Path(operation_id): Path<String>,
    Extension(principal): Extension<Principal>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let _lock = state.operations.lock().await;
    ready(&state, &principal).await?;
    let overview = invoke(&state, "status", json!({})).await?;
    let operation = overview["operations"]
        .as_array()
        .and_then(|values| values.iter().find(|v| v["id"] == operation_id))
        .ok_or_else(|| ApiError::not_found("reintegration not found"))?;
    let barcode = operation["barcode"]
        .as_str()
        .ok_or_else(|| ApiError::conflict("invalid journal"))?;
    if operation["phase"] == "rolled_back" && overview["pending"]["id"] != operation_id {
        return Err(ApiError::conflict("previous cartridge already restored"));
    }
    // An interrupted first installation may have no cartridge row yet. The
    // durable helper journal is the authority for rollback, not local presence.
    let id = Uuid::new_v4().to_string();
    let vtl = state.catalog.vtl_id().await?.to_string();
    sqlx::query("INSERT INTO jobs(id,vtl_id,cartridge_id,kind,subject,status,generation) VALUES(?,?,(SELECT id FROM cartridges WHERE vtl_id=? AND barcode=?),'rollback',?,'queued',?)")
        .bind(&id).bind(&vtl).bind(&vtl).bind(barcode).bind(barcode)
        .bind(operation["original"]["generation"].as_i64())
        .execute(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    let job = state
        .catalog
        .job(&id)
        .await?
        .ok_or_else(|| ApiError::conflict("rollback reservation missing"))?;
    tokio::spawn(run(
        state.clone(),
        id.clone(),
        "rollback",
        json!({
            "job_id":id,"operation_id":operation_id,"actor":principal.user.username
        }),
    ));
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn ready(state: &AppState, principal: &Principal) -> Result<(), ApiError> {
    if principal.user.role != "admin" {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            message: "administrator required".into(),
        });
    }
    if state.config.maintenance_helper.is_none() || state.config.reintegration_helper.is_none() {
        return Err(ApiError::conflict(
            "managed maintenance and reintegration adapters required",
        ));
    }
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE status IN ('queued','running') AND vtl_id=?",
    )
    .bind(state.catalog.vtl_id().await?.to_string())
    .fetch_one(&state.catalog.pool())
    .await
    .map_err(ApiError::conflict)?;
    if active != 0 {
        return Err(ApiError::conflict("wait for all active VTL jobs"));
    }
    Ok(())
}

async fn run(state: AppState, id: String, action: &'static str, request: Value) {
    let result = async {
        state
            .catalog
            .update_job(
                &id,
                "running",
                10,
                Some("reservation PBS, verification and cartridge replacement"),
            )
            .await
            .map_err(ApiError::conflict)?;
        invoke(&state, action, request).await
    }
    .await;
    let (status, message) = match result {
        Ok(value) => ("completed", value.to_string()),
        Err(error) => ("failed", error.message),
    };
    let _ = state
        .catalog
        .finish_job(&id, status, &message, None, None)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_complete_identity_bound_restore_receipts_are_eligible() {
        let mut job = Job {
            destination_id: None,
            configuration_revision: None,
            id: "a".into(),
            kind: "restore".into(),
            generation: Some(2),
            destination: Some("/restores/a".into()),
            subject: "TAPE01".into(),
            status: "completed".into(),
            progress: 100,
            message: Some(
                json!({"status":"restored","barcode":"TAPE01","generation":2,"files":[{}]})
                    .to_string(),
            ),
            created_at: "".into(),
            updated_at: "".into(),
        };
        assert!(valid_restore(&job, "TAPE01"));
        assert!(!valid_restore(&job, "TAPE02"));
        job.generation = Some(3);
        assert!(!valid_restore(&job, "TAPE01"));
        job.generation = Some(2);
        job.status = "failed".into();
        assert!(!valid_restore(&job, "TAPE01"));
        job.status = "completed".into();
        job.message = Some("{\"status\":\"restored\"}".into());
        assert!(!valid_restore(&job, "TAPE01"));
    }
}
