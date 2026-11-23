//! A pinned remote copy can be recovered without a local cartridge catalogue.
use crate::{ApiError, AppState, auth::Principal, catalog::Job};
use axum::{
    Extension, Json,
    extract::{Query, State},
    http::StatusCode,
};
use cold_vtl_core::Barcode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub destination_id: String,
    pub barcode: String,
    pub generation: u64,
    pub manifest_sha256: String,
    pub target_vtl_id: String,
    #[serde(default)]
    pub replace: bool,
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
fn validate(request: &Request) -> Result<(), ApiError> {
    Barcode::parse(&request.barcode).map_err(ApiError::bad_request)?;
    if request.generation == 0
        || request.generation > i64::MAX as u64
        || request.manifest_sha256.len() != 64
        || !request
            .manifest_sha256
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(ApiError::bad_request(
            "Select a readable copy with its verified manifest SHA-256.",
        ));
    }
    Ok(())
}
async fn selected(state: AppState, request: &Request) -> Result<AppState, ApiError> {
    let id = state.catalog.vtl_id().await?.to_string();
    let state = crate::vtls::installed_state(state, id).await?;
    crate::destinations::select(
        state,
        crate::destinations::Selection {
            destination_id: Some(request.destination_id.clone()),
        },
    )
    .await
}
pub async fn inventory(
    State(state): State<AppState>,
    Query(selection): Query<crate::destinations::Selection>,
) -> Result<Json<Value>, ApiError> {
    let state = crate::destinations::select(state, selection).await?;
    let pin = state.catalog.destination().await?;
    let mut command = state
        .catalog
        .worker_command(&state.config.worker_path, None)
        .await?;
    command.arg("recovery-inventory");
    let output = crate::operations::worker_output(&mut command)
        .await
        .map_err(|error| {
            // OVH no longer knows the bucket: the list offers to retire it.
            if error.contains("NoSuchBucket") {
                ApiError::gone("This bucket does not exist at OVH.")
            } else {
                ApiError::conflict(error)
            }
        })?;
    let mut value: Value = serde_json::from_str(&output).map_err(ApiError::conflict)?;
    if let Some(copies) = value["copies"].as_array_mut() {
        for copy in copies {
            let deletion:Option<String>=sqlx::query_scalar("SELECT status FROM jobs WHERE vtl_id=? AND destination_id=? AND kind='s3_delete' AND subject=? AND generation=? ORDER BY created_at DESC,rowid DESC LIMIT 1")
                .bind(state.catalog.vtl_id().await?.to_string()).bind(&pin.id).bind(copy["barcode"].as_str().unwrap_or_default()).bind(copy["generation"].as_i64().unwrap_or(0))
                .fetch_optional(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
            copy["deletion_status"] = json!(deletion);
        }
    }
    value["destination_id"] = json!(pin.id);
    value["configuration_revision"] = json!(pin.revision);
    Ok(Json(value))
}
async fn inspect(
    source: &AppState,
    target: &AppState,
    request: &Request,
) -> Result<Value, ApiError> {
    // The selected copy is identified by its exact manifest hash, not barcode.
    let mut command = source
        .catalog
        .worker_command(&source.config.worker_path, None)
        .await?;
    command.arg("recovery-inventory");
    let output = crate::operations::worker_output(&mut command)
        .await
        .map_err(ApiError::conflict)?;
    let value: Value = serde_json::from_str(&output).map_err(ApiError::conflict)?;
    if value["access"]["readable"] != true {
        return Err(ApiError::conflict(
            "Recall this entire bucket in Storage S3 and wait until it is readable.",
        ));
    }
    let copy = value["copies"]
        .as_array()
        .and_then(|copies| {
            copies.iter().find(|c| {
                c["barcode"] == request.barcode
                    && c["generation"].as_u64() == Some(request.generation)
                    && c["manifest_sha256"] == request.manifest_sha256
            })
        })
        .ok_or_else(|| {
            ApiError::conflict(
                "The selected remote copy is unavailable or changed; refresh the inventory.",
            )
        })?;
    if target.config.maintenance_helper.is_none() || target.config.reintegration_helper.is_none() {
        return Err(ApiError::conflict(
            "Managed maintenance and recovery adapters required.",
        ));
    }
    crate::reintegration::invoke(target,"check",json!({"barcode":request.barcode,"replace":request.replace,"required_bytes":copy["logical_size_bytes"],"download_root":source.config.restore_root})).await
}
pub async fn check(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<Request>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    validate(&request)?;
    let source = selected(state.clone(), &request).await?;
    let target = crate::vtls::installed_state(state, request.target_vtl_id.clone()).await?;
    Ok(Json(inspect(&source, &target, &request).await?))
}
pub async fn start(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<Request>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    admin(&principal)?;
    validate(&request)?;
    let source = selected(state.clone(), &request).await?;
    let target = crate::vtls::installed_state(state, request.target_vtl_id.clone()).await?;
    let source_id = source.catalog.vtl_id().await?.to_string();
    let target_id = target.catalog.vtl_id().await?.to_string();
    let (first, second) = if source_id <= target_id {
        (&source, &target)
    } else {
        (&target, &source)
    };
    let _first = first.operations.lock().await;
    let _second = if source_id != target_id {
        Some(second.operations.lock().await)
    } else {
        None
    };
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE vtl_id=? AND status IN ('queued','running')",
    )
    .bind(&target_id)
    .fetch_one(&target.catalog.pool())
    .await
    .map_err(ApiError::conflict)?;
    if active != 0 {
        return Err(ApiError::conflict(
            "Wait for active jobs in the target VTL before starting recovery.",
        ));
    }
    let preflight = inspect(&source, &target, &request).await?;
    if preflight["can_restore"] != true {
        return Err(ApiError::conflict(
            preflight["reason"]
                .as_str()
                .unwrap_or("Recovery preflight failed."),
        ));
    }
    let id = Uuid::new_v4().to_string();
    let target_job = if source_id == target_id {
        id.clone()
    } else {
        Uuid::new_v4().to_string()
    };
    let pin = source.catalog.destination().await?;
    let mut tx = source
        .catalog
        .pool()
        .begin()
        .await
        .map_err(ApiError::conflict)?;
    for (vtl, job, kind, destination, revision) in [
        (
            &source_id,
            &id,
            "recovery",
            Some(pin.id.as_str()),
            pin.revision,
        ),
        (&target_id, &target_job, "recovery_install", None, None),
    ] {
        if kind == "recovery_install" && source_id == target_id {
            continue;
        }
        sqlx::query("INSERT INTO jobs(id,vtl_id,cartridge_id,kind,subject,status,generation,destination_id,configuration_revision) VALUES(?,?,(SELECT id FROM cartridges WHERE vtl_id=? AND barcode=?),?,?,'queued',?,?,?)")
            .bind(job).bind(vtl).bind(vtl).bind(&request.barcode).bind(kind).bind(&request.barcode).bind(request.generation as i64).bind(destination).bind(revision)
            .execute(&mut *tx).await.map_err(ApiError::conflict)?;
    }
    sqlx::query("INSERT INTO recovery_requests(job_id,target_job_id,source_vtl_id,target_vtl_id,request,actor) VALUES(?,?,?,?,?,?)")
        .bind(&id).bind(&target_job).bind(&source_id).bind(&target_id).bind(serde_json::to_string(&request).map_err(ApiError::conflict)?).bind(&principal.user.username)
        .execute(&mut *tx).await.map_err(ApiError::conflict)?;
    tx.commit().await.map_err(ApiError::conflict)?;
    let job = source
        .catalog
        .job(&id)
        .await?
        .ok_or_else(|| ApiError::conflict("Recovery reservation missing"))?;
    tokio::spawn(run(source, target, id));
    Ok((StatusCode::ACCEPTED, Json(job)))
}
async fn phase(state: &AppState, id: &str, phase: &str) -> Result<(), ApiError> {
    sqlx::query("UPDATE recovery_requests SET phase=?,updated_at=CURRENT_TIMESTAMP WHERE job_id=?")
        .bind(phase)
        .bind(id)
        .execute(&state.catalog.pool())
        .await
        .map_err(ApiError::conflict)?;
    Ok(())
}
async fn run(source: AppState, target: AppState, id: String) {
    let result = execute(&source, &target, &id).await;
    if let Err(error) = result {
        let _ = phase(&source, &id, "failed").await;
        let _ = source
            .catalog
            .finish_job(&id, "failed", &error.message, None, None)
            .await;
        let target_job: Result<String, _> =
            sqlx::query_scalar("SELECT target_job_id FROM recovery_requests WHERE job_id=?")
                .bind(&id)
                .fetch_one(&source.catalog.pool())
                .await;
        if let Ok(job) = target_job
            && job != id
        {
            let _ = target
                .catalog
                .finish_job(&job, "failed", &error.message, None, None)
                .await;
        }
    }
}
async fn execute(source: &AppState, target: &AppState, id: &str) -> Result<(), ApiError> {
    let row = sqlx::query("SELECT * FROM recovery_requests WHERE job_id=?")
        .bind(id)
        .fetch_one(&source.catalog.pool())
        .await
        .map_err(ApiError::conflict)?;
    let request: Request = serde_json::from_str(row.get("request")).map_err(ApiError::conflict)?;
    let target_job: String = row.get("target_job_id");
    let actor: String = row.get("actor");
    let current_phase: String = row.get("phase");
    if current_phase == "installing" {
        let status = crate::reintegration::invoke(target, "status", json!({})).await?;
        if let Some(journal) = status["operations"].as_array().and_then(|ops| {
            ops.iter()
                .find(|op| op["id"] == target_job && op["phase"] == "completed")
        }) {
            if !status["pending"].is_null() {
                return Err(ApiError::conflict(
                    "Installation journal requires recovery before completion.",
                ));
            }
            complete(source, target, id, &target_job, journal).await?;
            return Ok(());
        }
        return Err(ApiError::conflict(
            "Installation interrupted. Inspect reintegration status and recover the pending operation before retrying.",
        ));
    }
    source
        .catalog
        .update_job(
            id,
            "running",
            10,
            Some("Downloading selected remote copy; PBS remains available"),
        )
        .await?;
    if row.get::<Option<String>, _>("receipt").is_none() {
        phase(source, id, "downloading").await?;
        tokio::fs::create_dir_all(&source.config.restore_root)
            .await
            .map_err(ApiError::conflict)?;
        let root = tokio::fs::canonicalize(&source.config.restore_root)
            .await
            .map_err(ApiError::conflict)?;
        let media = tokio::fs::canonicalize(&source.config.media_root)
            .await
            .map_err(ApiError::conflict)?;
        if root.starts_with(&media) || media.starts_with(&root) {
            return Err(ApiError::conflict(
                "Restore root must be separate from media.",
            ));
        }
        // Preserve interrupted transfers for diagnosis; a new worker attempt owns a new directory.
        let destination = root.join(format!("recovery-{id}-{}", Uuid::new_v4()));
        sqlx::query("UPDATE jobs SET destination=? WHERE id=?")
            .bind(destination.to_string_lossy().as_ref())
            .bind(id)
            .execute(&source.catalog.pool())
            .await
            .map_err(ApiError::conflict)?;
        let mut command = source
            .catalog
            .worker_command(&source.config.worker_path, Some(id))
            .await?;
        command
            .env("COLD_VTL_RESTORE_JOB_ID", id)
            .arg("restore")
            .arg("--barcode")
            .arg(&request.barcode)
            .arg("--generation")
            .arg(request.generation.to_string())
            .arg("--manifest-sha256")
            .arg(&request.manifest_sha256)
            .arg("--destination")
            .arg(&destination);
        let output = crate::operations::worker_output(&mut command)
            .await
            .map_err(ApiError::conflict)?;
        let receipt: Value = serde_json::from_str(&output).map_err(ApiError::conflict)?;
        if receipt["status"] != "restored"
            || receipt["barcode"] != request.barcode
            || receipt["generation"].as_u64() != Some(request.generation)
            || receipt["manifest_sha256"] != request.manifest_sha256
            || receipt["files"].as_array().is_none_or(|f| f.is_empty())
        {
            return Err(ApiError::conflict(
                "Restored copy failed receipt validation.",
            ));
        }
        sqlx::query("UPDATE recovery_requests SET receipt=?,phase='downloaded',updated_at=CURRENT_TIMESTAMP WHERE job_id=?").bind(output).bind(id).execute(&source.catalog.pool()).await.map_err(ApiError::conflict)?;
    }
    let _lock = target.operations.lock().await;
    source
        .catalog
        .update_job(
            id,
            "running",
            80,
            Some(r#"{"restore_progress":{"phase":"installing","completed_bytes":0,"total_bytes":null}}"#),
        )
        .await?;
    if target_job != id {
        target
            .catalog
            .update_job(
                &target_job,
                "running",
                80,
                Some(r#"{"restore_progress":{"phase":"installing","completed_bytes":0,"total_bytes":null}}"#),
            )
            .await?;
    }
    phase(source, id, "installing").await?;
    let installed = crate::reintegration::invoke(
        target,
        "recover",
        json!({"job_id":target_job,"recovery_job_id":id,"actor":actor}),
    )
    .await?;
    complete(source, target, id, &target_job, &installed).await
}
async fn complete(
    source: &AppState,
    target: &AppState,
    id: &str,
    target_job: &str,
    value: &Value,
) -> Result<(), ApiError> {
    // Keep the persistent phase last: startup can re-read a completed install journal.
    source
        .catalog
        .finish_job(id, "completed", &value.to_string(), None, None)
        .await?;
    if target_job != id {
        target
            .catalog
            .finish_job(target_job, "completed", &value.to_string(), None, None)
            .await?;
    }
    phase(source, id, "completed").await
}
/// Called after startup. Every continuation is bound to persisted source and target IDs.
pub async fn resume(state: AppState) -> Result<(), ApiError> {
    let rows=sqlx::query("SELECT job_id,source_vtl_id,target_vtl_id FROM recovery_requests WHERE phase IN ('queued','downloading','downloaded','installing')").fetch_all(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    for row in rows {
        let id: String = row.get("job_id");
        let source = crate::vtls::installed_state(state.clone(), row.get("source_vtl_id")).await;
        let target = crate::vtls::installed_state(state.clone(), row.get("target_vtl_id")).await;
        match (source, target) {
            (Ok(source), Ok(target)) => {
                tokio::spawn(run(source, target, id));
            }
            _ => {
                let _=sqlx::query("UPDATE jobs SET status='failed',message='Recovery runtime unavailable; inspect the persisted recovery request' WHERE id=? OR id=(SELECT target_job_id FROM recovery_requests WHERE job_id=?)").bind(&id).bind(&id).execute(&state.catalog.pool()).await;
                let _ = phase(&state, &id, "failed").await;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppConfig, catalog::Catalog};
    use std::{os::unix::fs::PermissionsExt, sync::Arc};
    async fn setup() -> (std::path::PathBuf, AppState) {
        let root = std::env::temp_dir().join(format!("recovery-api-{}", Uuid::new_v4()));
        let catalog = Catalog::open(&root.join("catalog.db")).await.unwrap();
        std::fs::create_dir_all(root.join("media")).unwrap();
        let worker = root.join("worker");
        std::fs::write(&worker,r#"#!/usr/bin/env python3
import sys,json,os
args=sys.argv[1:]
if args==['recovery-inventory']:
 print(json.dumps({'copies':[{'barcode':'TAPE01','generation':7,'manifest_sha256':'a'*64,'logical_size_bytes':1024}], 'complete':True,'errors':[],'access':{'readable':True}}))
elif args[0]=='restore':
 assert args[args.index('--manifest-sha256')+1]=='a'*64
 assert os.environ['COLD_VTL_CONFIGURATION_REVISION']=='1'
 print(json.dumps({'status':'restored','barcode':'TAPE01','generation':7,'manifest_sha256':'a'*64,'files':[{'path':'mam','size_bytes':10,'checksum_sha256':'b'*64}]}))
else: sys.exit(2)
"#).unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).unwrap();
        let helper = root.join("helper");
        std::fs::write(&helper,r#"#!/usr/bin/env python3
import sys,json
request=json.load(sys.stdin)
if sys.argv[1]=='check': print(json.dumps({'can_restore':True,'conflict':False,'space_ok':True,'local_identity':'absent'}))
elif sys.argv[1]=='recover': print(json.dumps({'id':request['job_id'],'phase':'completed','barcode':'TAPE01'}))
else: sys.exit(2)
"#).unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let pin = catalog.destination().await.unwrap();
        sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,1,'{}')").bind(&pin.id).execute(&catalog.pool()).await.unwrap();
        let state = AppState {
            catalog,
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(AppConfig {
                storage_socket: None,
                media_root: root.join("media"),
                restore_root: root.join("restores"),
                worker_path: worker,
                quiesce_check: None,
                maintenance_helper: Some(helper.clone()),
                cartridge_helper: None,
                reintegration_helper: Some(helper),
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        };
        (root, state)
    }
    #[tokio::test]
    async fn bucket_missing_at_ovh_is_reported_as_gone() {
        use axum::response::IntoResponse;
        let (root, state) = setup().await;
        std::fs::write(
            &state.config.worker_path,
            "#!/bin/sh\necho 'Error: Request(\"service error: unhandled error (NoSuchBucket): The specified bucket does not exist.\")' >&2\nexit 1\n",
        )
        .unwrap();
        let error = inventory(State(state.clone()), Query(Default::default()))
            .await
            .unwrap_err();
        let response = error.into_response();
        assert_eq!(response.status(), axum::http::StatusCode::GONE);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"], "This bucket does not exist at OVH.");
        // Other worker failures keep their message.
        std::fs::write(
            &state.config.worker_path,
            "#!/bin/sh\necho 'AccessDenied' >&2\nexit 1\n",
        )
        .unwrap();
        let error = inventory(State(state.clone()), Query(Default::default()))
            .await
            .unwrap_err()
            .into_response();
        assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    fn request(destination_id: String) -> Request {
        Request {
            destination_id,
            barcode: "TAPE01".into(),
            generation: 7,
            manifest_sha256: "a".repeat(64),
            target_vtl_id: crate::s3_settings::destinations::LEGACY.into(),
            replace: false,
        }
    }
    async fn reserve(
        state: &AppState,
        request: &Request,
        phase: &str,
        receipt: Option<&str>,
    ) -> String {
        let id = Uuid::new_v4().to_string();
        let vtl = state.catalog.vtl_id().await.unwrap().to_string();
        sqlx::query("INSERT INTO jobs(id,vtl_id,kind,subject,status,generation,destination_id,configuration_revision) VALUES(?,?,'recovery','TAPE01','queued',7,?,1)").bind(&id).bind(&vtl).bind(&request.destination_id).execute(&state.catalog.pool()).await.unwrap();
        sqlx::query("INSERT INTO recovery_requests(job_id,target_job_id,source_vtl_id,target_vtl_id,request,actor,phase,receipt) VALUES(?,?,?,?,?,'admin',?,?)").bind(&id).bind(&id).bind(&vtl).bind(&vtl).bind(serde_json::to_string(request).unwrap()).bind(phase).bind(receipt).execute(&state.catalog.pool()).await.unwrap();
        id
    }
    #[tokio::test]
    async fn empty_catalogue_remote_inventory_and_durable_pipeline_need_no_fake_cartridge() {
        let (root, state) = setup().await;
        let destination = state.catalog.destination().await.unwrap().id;
        let Json(inventory) = inventory(
            State(state.clone()),
            Query(crate::destinations::Selection {
                destination_id: Some(destination.clone()),
            }),
        )
        .await
        .unwrap();
        assert_eq!(inventory["copies"][0]["barcode"], "TAPE01");
        let request = request(destination.clone());
        let preflight = inspect(&state, &state, &request).await.unwrap();
        assert_eq!(preflight["can_restore"], true);
        let id = reserve(&state, &request, "queued", None).await;
        // Rotation after reservation must not change worker credentials or destination.
        sqlx::query("UPDATE storage_destinations SET active=0 WHERE id=?")
            .bind(&destination)
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO storage_destinations(id,vtl_id,active) VALUES('new-month',?,1)")
            .bind(state.catalog.vtl_id().await.unwrap().to_string())
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        run(state.clone(), state.clone(), id.clone()).await;
        let job = state.catalog.job(&id).await.unwrap().unwrap();
        assert_eq!(job.status, "completed", "{:?}", job.message);
        assert_eq!(job.destination_id, Some(destination));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cartridges")
                .fetch_one(&state.catalog.pool())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT phase FROM recovery_requests WHERE job_id=?")
                .bind(&id)
                .fetch_one(&state.catalog.pool())
                .await
                .unwrap(),
            "completed"
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn interrupted_download_preserves_old_destination_and_uses_new_attempt() {
        let (root, state) = setup().await;
        let request = request(state.catalog.destination().await.unwrap().id);
        let id = reserve(&state, &request, "downloading", None).await;
        let old = root.join("restores/old-attempt");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join(".cold-vtl-incomplete"), "preserve").unwrap();
        sqlx::query("UPDATE jobs SET destination=? WHERE id=?")
            .bind(old.to_str())
            .bind(&id)
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        run(state.clone(), state.clone(), id.clone()).await;
        let job = state.catalog.job(&id).await.unwrap().unwrap();
        assert_eq!(job.status, "completed", "{:?}", job.message);
        assert_ne!(job.destination.as_deref(), old.to_str());
        assert!(old.join(".cold-vtl-incomplete").exists());
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn restart_finishes_completed_install_journal_without_reinstalling() {
        let (root, state) = setup().await;
        let request = request(state.catalog.destination().await.unwrap().id);
        let id = reserve(&state, &request, "installing", Some("{}")).await;
        let body =
            json!({"pending":null,"operations":[{"id":id,"phase":"completed","barcode":"TAPE01"}]})
                .to_string();
        std::fs::write(
            root.join("helper"),
            format!(
                "#!/bin/sh\n[ \"$1\" = status ] || exit 9\nprintf '%s' '{}'\n",
                body
            ),
        )
        .unwrap();
        run(state.clone(), state.clone(), id.clone()).await;
        assert_eq!(
            state.catalog.job(&id).await.unwrap().unwrap().status,
            "completed"
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn restart_preserves_pending_installation_for_guarded_rollback() {
        let (root, state) = setup().await;
        let request = request(state.catalog.destination().await.unwrap().id);
        let id = reserve(&state, &request, "installing", Some("{}")).await;
        let body =
            json!({"pending":{"id":id},"operations":[{"id":id,"phase":"replacing"}]}).to_string();
        std::fs::write(
            root.join("helper"),
            format!(
                "#!/bin/sh\n[ \"$1\" = status ] || exit 9\nprintf '%s' '{}'\n",
                body
            ),
        )
        .unwrap();
        run(state.clone(), state.clone(), id.clone()).await;
        let job = state.catalog.job(&id).await.unwrap().unwrap();
        assert_eq!(job.status, "failed");
        assert!(
            job.message
                .as_deref()
                .unwrap_or_default()
                .contains("pending operation"),
            "{:?}",
            job.message
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn foreign_destination_and_changed_hash_are_refused() {
        let (root, state) = setup().await;
        let mut request = request(state.catalog.destination().await.unwrap().id);
        request.manifest_sha256 = "b".repeat(64);
        assert!(inspect(&state, &state, &request).await.is_err());
        assert!(
            crate::destinations::select(
                state.clone(),
                crate::destinations::Selection {
                    destination_id: Some("foreign".into())
                }
            )
            .await
            .is_err()
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn submission_requires_exact_hash_and_nonzero_generation() {
        let mut request = request("destination".into());
        assert!(validate(&request).is_ok());
        request.generation = 0;
        assert!(validate(&request).is_err());
        request.generation = 7;
        for hash in [String::new(), "barcode-only".into(), "A".repeat(64)] {
            request.manifest_sha256 = hash;
            assert!(validate(&request).is_err());
        }
    }
}
