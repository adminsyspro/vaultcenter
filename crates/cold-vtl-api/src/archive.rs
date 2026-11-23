use crate::{ApiError, AppState, auth::Principal, catalog::Job};
use axum::{Extension, Json, extract::State, http::StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

mod scheduled;
pub use scheduled::monitor as schedule_monitor;

/// The bucket name comes from the window that shows it; nothing is retyped.
#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Confirmation {
    confirm_bucket: String,
    acknowledge_billing: bool,
    /// Required only for a bucket VaultCenter did not create.
    #[serde(default)]
    exclusive_bucket: bool,
    #[serde(default)]
    next_bucket: Option<String>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecallConfirmation {
    confirm_bucket: String,
    acknowledge_retrieval_cost: bool,
}

fn administrator(principal: &Principal) -> Result<(), ApiError> {
    if principal.user.role != "admin" {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            message: "administrator required".into(),
        });
    }
    Ok(())
}

/// Buckets VaultCenter created empty, by a creation or by an archive rotation.
pub(crate) async fn managed(pool: &sqlx::SqlitePool, destination: &str) -> Result<bool, ApiError> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM bucket_creations WHERE next_destination_id=?1 AND phase='switched') OR EXISTS(SELECT 1 FROM bucket_rotations WHERE next_destination_id=?1 AND phase IN ('ready','switched'))")
        .bind(destination)
        .fetch_one(pool)
        .await
        .map_err(ApiError::conflict)
}

pub async fn recall_submit(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<RecallConfirmation>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    administrator(&principal)?;
    if !request.acknowledge_retrieval_cost || request.confirm_bucket.is_empty() {
        return Err(ApiError::bad_request(
            "Accept the recall charges for the whole bucket.",
        ));
    }
    start(
        state,
        &principal.user.username,
        "recall",
        "submit",
        Some(serde_json::to_value(request).map_err(ApiError::bad_request)?),
    )
    .await
}

/// Bucket creations and rotations not settled yet: either may change the
/// active destination, so every guard below waits for them (`?1` = VTL).
/// A macro so each query stays one static string.
macro_rules! pending_buckets {
    () => {
        "(SELECT COUNT(*) FROM bucket_rotations WHERE vtl_id=?1 AND phase IN ('reserved','creating','ready','uncertain')) + (SELECT COUNT(*) FROM bucket_creations WHERE vtl_id=?1 AND phase IN ('reserved','creating','uncertain'))"
    };
}

pub async fn idle(state: &AppState) -> Result<(), ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    let active: i64 = sqlx::query_scalar(concat!("SELECT (SELECT COUNT(*) FROM jobs WHERE vtl_id=?1 AND (kind LIKE 'archive_%' OR kind LIKE 'recall_%') AND status IN ('queued','running')) + (SELECT COUNT(*) FROM archive_requests WHERE vtl_id=?1 AND phase IN ('queued','submitting','uncertain')) + ", pending_buckets!()))
        .bind(&vtl)
        .fetch_one(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    if active != 0 {
        return Err(ApiError::conflict(
            "OVH operation in progress or uncertain result: check S3 storage before any other operation.",
        ));
    }
    Ok(())
}

/// Local media operations only wait for OVH work that can change the active
/// destination: an archive or recall of the active bucket, or a pending bucket
/// creation or rotation. Archiving an older bucket never touches local media.
pub async fn media_idle(state: &AppState) -> Result<(), ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    let pending: i64 = sqlx::query_scalar(concat!("SELECT (SELECT COUNT(*) FROM archive_requests WHERE vtl_id=?1 AND phase IN ('queued','submitting','uncertain') AND destination_id IN (SELECT id FROM storage_destinations WHERE vtl_id=?1 AND active=1)) + ", pending_buckets!()))
        .bind(&vtl).fetch_one(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    if pending != 0 {
        return Err(ApiError::conflict(
            "An OVH operation is in progress on the active bucket. Wait for it to finish in S3 storage.",
        ));
    }
    Ok(())
}

/// A copy may be deleted from the selected destination unless an archive or
/// recall of that same bucket is pending, or a bucket creation or rotation is.
/// Never `media_idle`: it would allow deleting from an older bucket under archive.
/// A bucket OVH accepted to archive, or archived (read-only), keeps its copies.
pub async fn destination_idle(state: &AppState) -> Result<(), ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    let destination = state.catalog.destination().await?.id;
    let archived: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM archive_requests WHERE vtl_id=? AND destination_id=? AND phase IN ('accepted','archived'))")
        .bind(&vtl).bind(&destination).fetch_one(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    if archived {
        return Err(ApiError::conflict(
            "This bucket is archived at OVH or being archived. Its copies cannot be deleted.",
        ));
    }
    let pending: i64 = sqlx::query_scalar(concat!("SELECT (SELECT COUNT(*) FROM archive_requests WHERE vtl_id=?1 AND destination_id=?2 AND phase IN ('queued','submitting','uncertain')) + ", pending_buckets!()))
        .bind(&vtl).bind(&destination).fetch_one(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    if pending != 0 {
        return Err(ApiError::conflict(
            "An OVH operation is in progress on this bucket. Wait for it to finish in S3 storage.",
        ));
    }
    Ok(())
}

pub async fn overview(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    use sqlx::Row;
    let rows = sqlx::query("SELECT job_id,prepare_job_id,phase,operation,request,observed_status,observed_at,updated_at,CASE WHEN restored_at IS NOT NULL THEN strftime('%Y-%m-%dT%H:%M:%SZ',restored_at,'+30 days') END AS restored_until FROM archive_requests WHERE vtl_id=? AND destination_id=? ORDER BY updated_at DESC LIMIT 30")
        .bind(state.catalog.vtl_id().await?.to_string()).bind(state.catalog.destination().await?.id)
        .fetch_all(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    Ok(Json(
        json!({"configured":state.config.archive_helper.is_some(),"operations":rows.iter().map(|r|json!({
        "operation":r.get::<String,_>("operation"),"observed_status":r.get::<Option<String>,_>("observed_status"),
        "observed_at":r.get::<Option<String>,_>("observed_at"),
        "restored_until":r.get::<Option<String>,_>("restored_until"),
        "bucket":serde_json::from_str::<Value>(r.get::<&str,_>("request")).ok().and_then(|v|v["confirm_bucket"].as_str().map(str::to_owned)),
        "job_id":r.get::<String,_>("job_id"),"prepare_job_id":r.get::<String,_>("prepare_job_id"),
        "phase":r.get::<String,_>("phase"),"updated_at":r.get::<String,_>("updated_at")
    })).collect::<Vec<_>>()}),
    ))
}

pub async fn submit(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<Confirmation>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    administrator(&principal)?;
    if !request.acknowledge_billing || request.confirm_bucket.is_empty() {
        return Err(ApiError::bad_request(
            "Accept the OVH terms before archiving.",
        ));
    }
    start(
        state,
        &principal.user.username,
        "archive",
        "submit",
        Some(serde_json::to_value(request).map_err(ApiError::bad_request)?),
    )
    .await
}

pub async fn refresh(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    administrator(&principal)?;
    start(state, &principal.user.username, "archive", "refresh", None).await
}

pub(crate) async fn start(
    state: AppState,
    actor: &str,
    operation: &'static str,
    action: &'static str,
    confirmation: Option<Value>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let _lock = state.operations.lock().await;
    crate::s3_settings::destinations::initialize_legacy(&state).await?;
    let pin = state.catalog.destination().await?;
    let vtl = state.catalog.vtl_id().await?.to_string();
    let helper = state
        .config
        .archive_helper
        .clone()
        .ok_or_else(|| ApiError::conflict("Archive adapter not configured."))?;
    if action != "refresh" {
        idle(&state).await?;
    }
    if operation == "recall" {
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM archive_requests WHERE vtl_id=? AND destination_id=? AND operation='recall' AND phase='accepted'",
        )
        .bind(&vtl).bind(&pin.id).fetch_one(&state.catalog.pool())
        .await
        .map_err(ApiError::conflict)?;
        if pending > 0 {
            return Err(ApiError::conflict(
                "A recall has already been accepted. Wait for the availability observed in tracking.",
            ));
        }
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE vtl_id=? AND status IN ('queued','running')",
    )
    .bind(&vtl)
    .fetch_one(&state.catalog.pool())
    .await
    .map_err(ApiError::conflict)?;
    if count > 0 {
        return Err(ApiError::conflict("Wait for all VTL jobs to finish."));
    }
    let mut request = json!({"actor":actor});
    let mut subject = "Stockage S3".to_owned();
    if let Some(ref confirmation) = confirmation {
        let bucket: Option<String> = sqlx::query_scalar("SELECT json_extract(identity,'$.bucket') FROM storage_destinations WHERE id=? AND vtl_id=?")
            .bind(&pin.id).bind(&vtl).fetch_one(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
        let Some(bucket) =
            bucket.filter(|name| confirmation["confirm_bucket"].as_str() == Some(name.as_str()))
        else {
            return Err(ApiError::conflict(
                "The bucket shown no longer matches this destination. Reopen its window.",
            ));
        };
        if operation == "archive"
            && confirmation["exclusive_bucket"] != true
            && !managed(&state.catalog.pool(), &pin.id).await?
        {
            return Err(ApiError::bad_request(
                "Confirm that this bucket is reserved for this VTL.",
            ));
        }
        request = confirmation.clone();
        request["actor"] = json!(actor);
        subject = bucket;
    }
    let id = Uuid::new_v4().to_string();
    request["job_id"] = json!(id);
    request["vtl_id"] = json!(vtl);
    request["destination_id"] = json!(pin.id);
    request["configuration_revision"] = json!(pin.revision);
    if operation == "archive"
        && action == "submit"
        && !pin.active
        && request["next_bucket"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty())
    {
        return Err(ApiError::bad_request(
            "Archiving an old bucket does not create an additional destination.",
        ));
    }
    if operation == "archive" && action == "submit" && pin.active {
        let next = request["next_bucket"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_owned();
        let next = if next.is_empty() {
            crate::s3_settings::next_bucket_name(&state, &vtl, &[]).await?
        } else {
            next
        };
        if !cold_vtl_core::valid_managed_bucket_name(&next) || request["confirm_bucket"] == next {
            return Err(ApiError::bad_request(
                "Invalid next bucket name (vc- prefix required) or identical to the current bucket.",
            ));
        }
        request["next_bucket"] = json!(next);
    }
    let mut tx = state
        .catalog
        .pool()
        .begin()
        .await
        .map_err(ApiError::conflict)?;
    sqlx::query("INSERT INTO jobs(id,kind,subject,status,progress,vtl_id,destination_id,configuration_revision) VALUES(?,?,?,'queued',0,?,?,?)")
        .bind(&id)
        .bind(format!("{operation}_{action}"))
        .bind(subject).bind(&vtl).bind(&pin.id).bind(pin.revision)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    if confirmation.is_some() {
        sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,operation,vtl_id,destination_id) VALUES(?,?,'queued',?,?,?,?)")
            // An archive or recall is one request: its own job is its preparation.
            .bind(&id).bind(&id).bind(request.to_string()).bind(operation).bind(&vtl).bind(&pin.id).execute(&mut *tx).await
            .map_err(|_|ApiError::conflict("This request has already been recorded. Check the archive tracking."))?;
    }
    if operation == "archive" && action == "submit" && pin.active {
        sqlx::query("INSERT INTO bucket_rotations(job_id,vtl_id,source_destination_id,next_bucket,phase) VALUES(?,?,?,?, 'reserved')")
            .bind(&id).bind(&vtl).bind(&pin.id).bind(request["next_bucket"].as_str().unwrap()).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    }
    tx.commit().await.map_err(ApiError::conflict)?;
    let job = state
        .catalog
        .job(&id)
        .await?
        .ok_or_else(|| ApiError::conflict("Job not found."))?;
    let state_copy = state.clone();
    tokio::spawn(async move {
        let result = async {
            let mut child = crate::vtl_runtime::helper_command(&state_copy, &helper)
                .await?
                .arg(if operation == "recall" {
                    format!("recall-{action}")
                } else {
                    action.to_owned()
                })
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(false)
                .spawn()
                .map_err(ApiError::conflict)?;
            if let Some(mut input) = child.stdin.take() {
                input
                    .write_all(request.to_string().as_bytes())
                    .await
                    .map_err(ApiError::conflict)?;
            }
            if !child.wait().await.map_err(ApiError::conflict)?.success() {
                return Err(ApiError::conflict("Adapter stopped."));
            }
            Ok::<_, ApiError>(())
        }
        .await;
        // The adapter owns job completion. On a start/transport failure, keep the
        // persistent request for read-only recovery; never retry the cloud PUT.
        if result.is_err()
            && state_copy
                .catalog
                .job(&id)
                .await
                .ok()
                .flatten()
                .is_some_and(|j| matches!(j.status.as_str(), "queued" | "running"))
            && let Err(error) = state_copy
                .catalog
                .finish_job(
                    &id,
                    "failed",
                    "Adapter interrupted: use the archive tracking.",
                    None,
                    None,
                )
                .await
        {
            eprintln!("Archive job {id}: failure not recorded: {error}");
        }
    });
    Ok((StatusCode::ACCEPTED, Json(job)))
}

/// Poll only outstanding requests. Observations never replay a cloud mutation.
pub fn monitor(state: AppState) {
    let Some(helper) = state.config.archive_helper.clone() else {
        return;
    };
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(30));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            let ids:Vec<String>=sqlx::query_scalar("SELECT DISTINCT vtl_id FROM archive_requests WHERE phase IN ('submitting','uncertain','accepted') UNION SELECT vtl_id FROM bucket_rotations WHERE phase='ready'")
                .fetch_all(&state.catalog.pool()).await.unwrap_or_default();
            let mut pending = tokio::task::JoinSet::new();
            for id in ids {
                let Ok(scoped) = crate::vtls::installed_state(state.clone(), id).await else {
                    continue;
                };
                let helper = helper.clone();
                pending.spawn(async move {
                    if let Ok(mut command) =
                        crate::vtl_runtime::helper_command(&scoped, &helper).await
                    {
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(25),
                            command
                                .arg("observe")
                                .stdin(Stdio::null())
                                .stdout(Stdio::null())
                                .stderr(Stdio::null())
                                .kill_on_drop(true)
                                .status(),
                        )
                        .await;
                    }
                });
            }
            while pending.join_next().await.is_some() {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3_settings::test_support::archive_fixture;
    #[tokio::test]
    async fn pending_bucket_intent_blocks_media_even_after_api_restart() {
        use std::sync::Arc;
        let root = std::env::temp_dir().join(format!("cold-vtl-archive-{}", Uuid::new_v4()));
        let path = root.join("catalog.db");
        let catalog = crate::catalog::Catalog::open(&path).await.unwrap();
        sqlx::query("INSERT INTO jobs(id,kind,subject,status) VALUES('prepare','archive_prepare','bucket','completed'),('submit','archive_submit','bucket','running')")
            .execute(&catalog.pool()).await.unwrap();
        sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request) VALUES('submit','prepare','submitting','{}')")
            .execute(&catalog.pool()).await.unwrap();
        catalog.pool().close().await;
        let state = AppState {
            catalog: crate::catalog::Catalog::open(&path).await.unwrap(),
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(crate::AppConfig {
                storage_socket: None,
                media_root: root.clone(),
                restore_root: root.clone(),
                worker_path: root.join("must-not-run"),
                quiesce_check: None,
                maintenance_helper: None,
                cartridge_helper: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        };
        assert_eq!(
            state.catalog.job("submit").await.unwrap().unwrap().status,
            "failed"
        );
        assert!(idle(&state).await.is_err());
        let cartridge =
            cold_vtl_core::Cartridge::new(cold_vtl_core::Barcode::parse("TAPE01").unwrap(), 1024)
                .unwrap();
        state.catalog.create_cartridge(&cartridge).await.unwrap();
        assert!(
            crate::operations::available_cartridge(&state, "TAPE01".into())
                .await
                .is_err()
        );
        sqlx::query("UPDATE archive_requests SET phase='accepted'")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        assert!(idle(&state).await.is_ok());
        assert!(
            crate::operations::available_cartridge(&state, "TAPE01".into())
                .await
                .is_ok()
        );
        assert!(serde_json::from_value::<Confirmation>(json!({"confirm_bucket":"b","acknowledge_billing":true,"exclusive_bucket":true,"lock_days":180})).is_err());
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn archive_is_one_request_without_maintenance_bound_to_the_shown_bucket() {
        let (state, root) = archive_fixture(r#"cat > "$(dirname "$0")/input.json""#).await;
        let input = root.join("input.json");
        let wrong = start(state.clone(), "admin", "archive", "submit", Some(json!({"confirm_bucket":"other-bucket","acknowledge_billing":true,"exclusive_bucket":true}))).await.unwrap_err();
        assert_eq!(wrong.status, StatusCode::CONFLICT);
        let shared = start(state.clone(), "admin", "archive", "submit", Some(json!({"confirm_bucket":"current-bucket","acknowledge_billing":true,"exclusive_bucket":false}))).await.unwrap_err();
        assert!(shared.message.contains("reserved"), "{}", shared.message);
        let (status, Json(job)) = start(state.clone(), "admin", "archive", "submit", Some(json!({"confirm_bucket":"current-bucket","acknowledge_billing":true,"exclusive_bucket":true,"next_bucket":null}))).await.unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(job.kind, "archive_submit");
        let (prepare, phase, destination): (String, String, String) = sqlx::query_as(
            "SELECT prepare_job_id,phase,destination_id FROM archive_requests WHERE job_id=?",
        )
        .bind(&job.id)
        .fetch_one(&state.catalog.pool())
        .await
        .unwrap();
        assert_eq!(
            (prepare.as_str(), phase.as_str()),
            (job.id.as_str(), "queued")
        );
        // The controller trusts the intent: it must name the job's frozen destination.
        assert_eq!(Some(destination), job.destination_id);
        for _ in 0..50 {
            if std::fs::metadata(&input).is_ok_and(|m| m.len() > 0) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let sent: Value = serde_json::from_slice(&std::fs::read(&input).unwrap()).unwrap();
        assert_eq!(sent["job_id"], job.id);
        assert_eq!(sent["actor"], "admin");
        assert_eq!(sent["confirm_bucket"], "current-bucket");
        assert!(sent["next_bucket"].as_str().unwrap().starts_with("vc-"));
        let rotation: String =
            sqlx::query_scalar("SELECT phase FROM bucket_rotations WHERE job_id=?")
                .bind(&job.id)
                .fetch_one(&state.catalog.pool())
                .await
                .unwrap();
        assert_eq!(rotation, "reserved");
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn recall_intent_names_the_frozen_destination_of_its_job() {
        let (state, root) = archive_fixture("cat >/dev/null").await;
        let wrong = start(
            state.clone(),
            "admin",
            "recall",
            "submit",
            Some(json!({"confirm_bucket":"other-bucket","acknowledge_retrieval_cost":true})),
        )
        .await
        .unwrap_err();
        assert_eq!(wrong.status, StatusCode::CONFLICT);
        let (status, Json(job)) = start(
            state.clone(),
            "admin",
            "recall",
            "submit",
            Some(json!({"confirm_bucket":"current-bucket","acknowledge_retrieval_cost":true})),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(job.kind, "recall_submit");
        let (prepare, destination, operation): (String, String, String) = sqlx::query_as(
            "SELECT prepare_job_id,destination_id,operation FROM archive_requests WHERE job_id=?",
        )
        .bind(&job.id)
        .fetch_one(&state.catalog.pool())
        .await
        .unwrap();
        assert_eq!(
            (prepare.as_str(), operation.as_str()),
            (job.id.as_str(), "recall")
        );
        assert_eq!(Some(destination), job.destination_id);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn buckets_created_by_vaultcenter_need_no_exclusive_confirmation() {
        let (state, root) = archive_fixture("cat >/dev/null").await;
        let pin = state.catalog.destination().await.unwrap();
        let vtl = state.catalog.vtl_id().await.unwrap().to_string();
        assert!(!managed(&state.catalog.pool(), &pin.id).await.unwrap());
        sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id) VALUES('created','bucket_create','current-bucket','completed',?)").bind(&vtl).execute(&state.catalog.pool()).await.unwrap();
        sqlx::query("INSERT INTO bucket_creations(job_id,vtl_id,next_destination_id,next_bucket,configuration,revision,phase) VALUES('created',?,?,'current-bucket','{}',1,'switched')").bind(&vtl).bind(&pin.id).execute(&state.catalog.pool()).await.unwrap();
        assert!(managed(&state.catalog.pool(), &pin.id).await.unwrap());
        let (status, _) = start(state.clone(), "admin", "archive", "submit", Some(json!({"confirm_bucket":"current-bucket","acknowledge_billing":true,"exclusive_bucket":false}))).await.unwrap();
        assert_eq!(status, StatusCode::ACCEPTED);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn restored_recall_reports_the_end_of_its_read_only_window() {
        let (state, root) = archive_fixture("cat >/dev/null").await;
        let pin = state.catalog.destination().await.unwrap();
        let vtl = state.catalog.vtl_id().await.unwrap().to_string();
        sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id,destination_id,configuration_revision) VALUES('recall','recall_submit','current-bucket','completed',?,?,1)").bind(&vtl).bind(&pin.id).execute(&state.catalog.pool()).await.unwrap();
        sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,operation,vtl_id,destination_id) VALUES('recall','recall','accepted','{\"confirm_bucket\":\"current-bucket\"}','recall',?,?)").bind(&vtl).bind(&pin.id).execute(&state.catalog.pool()).await.unwrap();
        assert_eq!(
            overview(State(state.clone())).await.unwrap().0["operations"][0]["restored_until"],
            Value::Null
        );
        sqlx::query("UPDATE archive_requests SET phase='restored' WHERE job_id='recall'")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        let expected: String = sqlx::query_scalar("SELECT strftime('%Y-%m-%dT%H:%M:%SZ',restored_at,'+30 days') FROM archive_requests WHERE job_id='recall'")
            .fetch_one(&state.catalog.pool()).await.unwrap();
        assert_eq!(
            overview(State(state.clone())).await.unwrap().0["operations"][0]["restored_until"],
            expected
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn remote_delete_waits_only_for_an_archive_of_its_own_bucket() {
        use crate::s3_settings::test_support::{older_bucket_archive, uncertain_rotation};
        let (state, root) = archive_fixture("cat >/dev/null").await;
        let active = state.catalog.destination().await.unwrap().id;
        older_bucket_archive(&state, "uncertain").await;
        let select = |id: String| {
            crate::destinations::select(
                state.clone(),
                crate::destinations::Selection {
                    destination_id: Some(id),
                },
            )
        };
        let older = select("older".into()).await.unwrap();
        let refused = destination_idle(&older).await.unwrap_err();
        assert_eq!(refused.status, StatusCode::CONFLICT);
        let current = select(active.clone()).await.unwrap();
        destination_idle(&current).await.unwrap();
        // Once OVH accepted the archive (or archived the bucket), its copies stay.
        for phase in ["accepted", "archived"] {
            sqlx::query("UPDATE archive_requests SET phase=? WHERE job_id='older-archive'")
                .bind(phase)
                .execute(&state.catalog.pool())
                .await
                .unwrap();
            let refused = destination_idle(&older).await.unwrap_err();
            assert!(
                refused.message.contains("archived"),
                "{phase}: {}",
                refused.message
            );
            destination_idle(&current).await.unwrap();
        }
        sqlx::query("UPDATE archive_requests SET phase='failed' WHERE job_id='older-archive'")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        destination_idle(&older).await.unwrap();
        // A pending rotation may change the active bucket: every deletion waits.
        uncertain_rotation(&state, "vc-next-bucket").await;
        assert!(destination_idle(&current).await.is_err());
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn archiving_an_older_bucket_does_not_block_exports_to_the_active_one() {
        let (state, root) = archive_fixture("cat >/dev/null").await;
        let vtl = state.catalog.vtl_id().await.unwrap().to_string();
        let active = state.catalog.destination().await.unwrap().id;
        let cartridge =
            cold_vtl_core::Cartridge::new(cold_vtl_core::Barcode::parse("TAPE01").unwrap(), 1024)
                .unwrap();
        state.catalog.create_cartridge(&cartridge).await.unwrap();
        sqlx::query(
            "INSERT INTO storage_destinations(id,vtl_id,identity,active) VALUES('older',?,'{}',0)",
        )
        .bind(&vtl)
        .execute(&state.catalog.pool())
        .await
        .unwrap();
        for (job, destination) in [
            ("old-archive", "older"),
            ("active-archive", active.as_str()),
        ] {
            sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id) VALUES(?,'archive_submit',?,'failed',?)").bind(job).bind(job).bind(&vtl).execute(&state.catalog.pool()).await.unwrap();
            sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,vtl_id,destination_id) VALUES(?,?,'failed','{}',?,?)").bind(job).bind(job).bind(&vtl).bind(destination).execute(&state.catalog.pool()).await.unwrap();
        }
        sqlx::query("UPDATE archive_requests SET phase='submitting' WHERE job_id='old-archive'")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        crate::operations::available_cartridge(&state, "TAPE01".into())
            .await
            .unwrap();
        sqlx::query("UPDATE archive_requests SET phase='submitting' WHERE job_id='active-archive'")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        assert!(
            crate::operations::available_cartridge(&state, "TAPE01".into())
                .await
                .is_err()
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }
}
