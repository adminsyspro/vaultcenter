//! An explicit, audited S3-only deletion. The job pins destination credentials.
use crate::{ApiError, AppState, auth::Principal, catalog::Job};
use axum::{Extension, Json, extract::State, http::StatusCode};
use cold_vtl_core::Barcode;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub destination_id: String,
    pub configuration_revision: Option<i64>,
    pub barcode: String,
    pub generation: u64,
    pub manifest_sha256: String,
    pub confirm_barcode: String,
    pub confirm_bucket: String,
}
fn validate(request: &Request) -> Result<(), ApiError> {
    Barcode::parse(&request.barcode).map_err(ApiError::bad_request)?;
    if request.confirm_barcode != request.barcode
        || request.generation == 0
        || request.generation >= i64::MAX as u64
        || request.manifest_sha256.len() != 64
        || !request
            .manifest_sha256
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(ApiError::bad_request(
            "Confirm the barcode of the selected copy.",
        ));
    }
    Ok(())
}
fn arguments(command: &mut tokio::process::Command, request: &Request) {
    command
        .arg("delete-copy")
        .arg("--barcode")
        .arg(&request.barcode)
        .arg("--generation")
        .arg(request.generation.to_string())
        .arg("--manifest-sha256")
        .arg(&request.manifest_sha256);
}
pub async fn start(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<Request>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    if principal.user.role != "admin" {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            message: "administrator required".into(),
        });
    }
    validate(&request)?;
    let state = crate::destinations::select(
        state,
        crate::destinations::Selection {
            destination_id: Some(request.destination_id.clone()),
        },
    )
    .await?;
    let guard = state.operations.lock().await;
    crate::archive::destination_idle(&state).await?;
    let pin = state.catalog.destination().await?;
    if pin.revision != request.configuration_revision {
        return Err(ApiError::conflict(
            "The S3 configuration changed. Refresh storage.",
        ));
    }
    let vtl = state.catalog.vtl_id().await?.to_string();
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE vtl_id=? AND status IN ('queued','running')",
    )
    .bind(&vtl)
    .fetch_one(&state.catalog.pool())
    .await
    .map_err(ApiError::conflict)?;
    if active != 0 {
        return Err(ApiError::conflict(
            "Wait for operations to finish before deleting an S3 copy.",
        ));
    }
    let mut command = state
        .catalog
        .worker_for_destination(&state.config.worker_path, pin.clone())
        .await?;
    arguments(&mut command, &request);
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::operations::worker_output(&mut command),
    )
    .await
    .map_err(|_| ApiError::conflict("The S3 check timed out. Refresh storage and retry."))?
    .map_err(ApiError::conflict)?;
    let preview: Value = serde_json::from_str(&output).map_err(ApiError::conflict)?;
    if preview["status"] != "checked"
        || preview["bucket"] != request.confirm_bucket
        || preview["manifest_sha256"] != request.manifest_sha256
    {
        return Err(ApiError::conflict(
            "The S3 copy changed. Refresh storage before deleting it.",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let mut tx = state
        .catalog
        .pool()
        .begin()
        .await
        .map_err(ApiError::conflict)?;
    sqlx::query("INSERT INTO jobs(id,vtl_id,cartridge_id,kind,subject,status,generation,destination_id,configuration_revision,message) VALUES(?,?,(SELECT id FROM cartridges WHERE vtl_id=? AND barcode=?),'s3_delete',?,'queued',?,?,?,?)")
        .bind(&id).bind(&vtl).bind(&vtl).bind(&request.barcode).bind(&request.barcode).bind(request.generation as i64).bind(&pin.id).bind(pin.revision)
        .bind(json!({"actor":principal.user.username,"bucket":request.confirm_bucket,"manifest_sha256":request.manifest_sha256,"objects":preview["objects"]}).to_string())
        .execute(&mut *tx).await.map_err(ApiError::conflict)?;
    // Invalidate assurance before touching S3, including partial failure or restart.
    sqlx::query("DELETE FROM remote_generations WHERE vtl_id=? AND destination_id=? AND barcode=? AND generation=?")
        .bind(&vtl).bind(&pin.id).bind(&request.barcode).bind(request.generation as i64).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    if pin.active {
        sqlx::query("INSERT OR IGNORE INTO s3_reconcile_required(vtl_id,barcode) SELECT vtl_id,barcode FROM cartridges WHERE vtl_id=? AND barcode=?")
            .bind(&vtl).bind(&request.barcode).execute(&mut *tx).await.map_err(ApiError::conflict)?;
        sqlx::query("UPDATE cartridges SET state=CASE WHEN state='verified' THEN 'sealed' ELSE state END,generation=generation+1,updated_at=CURRENT_TIMESTAMP WHERE vtl_id=? AND barcode=? AND generation=?")
            .bind(&vtl).bind(&request.barcode).bind(request.generation as i64).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    }
    tx.commit().await.map_err(ApiError::conflict)?;
    let job = state
        .catalog
        .job(&id)
        .await?
        .ok_or_else(|| ApiError::conflict("Deletion not recorded."))?;
    tokio::spawn(async move {
        let _guard = guard;
        let result:Result<(),ApiError>=async {
            state.catalog.update_job(&id,"running",0,Some("Deleting the objects belonging to this S3 copy.")).await?;
            let mut command=state.catalog.worker_command(&state.config.worker_path,Some(&id)).await?;
            arguments(&mut command,&request);command.arg("--execute");
            let output=crate::operations::worker_output(&mut command).await.map_err(ApiError::conflict)?;
            let receipt:Value=serde_json::from_str(&output).map_err(ApiError::conflict)?;
            if receipt["status"]!="deleted" || receipt["manifest_sha256"]!=request.manifest_sha256 {return Err(ApiError::conflict("S3 deletion not confirmed."));}
            state.catalog.finish_job(&id,"completed",&json!({"actor":principal.user.username,"bucket":request.confirm_bucket,"manifest_sha256":request.manifest_sha256,"status":"deleted","message":"S3 copy deleted. Local data is preserved."}).to_string(),None,None).await?;
            Ok(())
        }.await;
        if let Err(error) = result {
            let _ = state
                .catalog
                .finish_job(
                    &id,
                    "failed",
                    &format!(
                        "Deletion interrupted. Refresh storage, then retry on the same copy. {}",
                        error.message
                    ),
                    None,
                    None,
                )
                .await;
        }
    });
    Ok((StatusCode::ACCEPTED, Json(job)))
}
