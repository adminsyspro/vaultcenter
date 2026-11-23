use crate::{ApiError, AppState};
use axum::{Json, extract::State};
use serde_json::{Value, json};
use sqlx::Row;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub async fn require_access(state: &AppState, write: bool) -> Result<(), ApiError> {
    let value = access(State(state.clone())).await?.0;
    if value[if write { "writable" } else { "readable" }] != true {
        return Err(ApiError::conflict(if write {
            "The bucket is closed to writes. Choose an available bucket for new exports; see S3 storage."
        } else {
            "S3 copies unreachable: recall the bucket in S3 storage and wait for Recall complete."
        }));
    }
    Ok(())
}

pub async fn access(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    crate::s3_settings::destinations::initialize_legacy(&state).await?;
    let pin = state.catalog.destination().await?;
    let output = tokio::time::timeout(
        Duration::from_secs(12),
        state
            .catalog
            .worker_for_destination(&state.config.worker_path, pin.clone())
            .await?
            .arg("storage-access")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| ApiError::conflict("S3 access not confirmed: timed out."))?
    .map_err(|_| ApiError::conflict("S3 access not confirmed."))?;
    if !output.status.success() {
        return Err(ApiError::conflict(
            "S3 access not confirmed: check the network and the configuration.",
        ));
    }
    let mut value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| ApiError::conflict("Invalid S3 access diagnostic."))?;
    value["destination_id"] = json!(pin.id);
    value["active"] = json!(pin.active);
    value["configuration_revision"] = json!(pin.revision);
    Ok(Json(value))
}

pub async fn status(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    crate::s3_settings::destinations::initialize_legacy(&state).await?;
    let pin = state.catalog.destination().await?;
    let output = tokio::time::timeout(
        Duration::from_secs(25),
        state
            .catalog
            .worker_for_destination(&state.config.worker_path, pin.clone())
            .await?
            .arg("storage-status")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| ApiError::conflict("Storage not confirmed: read timed out."))?
    .map_err(|_| ApiError::conflict("The storage diagnostic cannot start."))?;
    if !output.status.success() {
        return Err(ApiError::conflict(
            "Storage not confirmed: check the S3 configuration and connectivity.",
        ));
    }
    let mut probe: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| ApiError::conflict("Invalid storage diagnostic response."))?;
    probe["config_revision"] = json!(pin.revision);
    probe["configuration_revision"] = json!(pin.revision);
    probe["destination_id"] = json!(pin.id);
    probe["active"] = json!(pin.active);
    let vtl = state.catalog.vtl_id().await?.to_string();
    let rows = sqlx::query("SELECT barcode,generation,metadata,checked_at FROM remote_generations WHERE vtl_id=? AND destination_id=?")
        .bind(&vtl).bind(&pin.id).fetch_all(&state.catalog.pool())
        .await
        .map_err(ApiError::conflict)?;
    let mut known = Vec::new();
    for row in rows {
        let metadata: Value =
            serde_json::from_str(row.get::<&str, _>("metadata")).map_err(ApiError::conflict)?;
        known.push(json!({"barcode":row.get::<String,_>("barcode"),"generation":row.get::<i64,_>("generation"),"key":metadata["manifest_key"],"checked_at":row.get::<String,_>("checked_at")}));
    }
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE vtl_id=? AND status IN ('queued','running')",
    )
    .bind(&vtl)
    .fetch_one(&state.catalog.pool())
    .await
    .map_err(ApiError::conflict)?;
    probe["managed"] = json!(crate::archive::managed(&state.catalog.pool(), &pin.id).await?);
    probe["next_bucket"] = if pin.active {
        json!(crate::s3_settings::next_bucket_name(&state, &vtl, &[]).await?)
    } else {
        Value::Null
    };
    Ok(Json(report(probe, &known, active)))
}

fn state_label(raw: Option<&str>) -> (&'static str, &'static str) {
    match raw {
        Some("None") => ("Available", "Read and write"),
        Some("Archiving") => ("Archiving", "Object listing only"),
        Some("Archived") => ("Archived", "Object listing only"),
        Some("Restoring") => ("Recall in progress", "Object listing only"),
        Some("Restored") => ("Recall complete", "Read-only"),
        Some("Deleting") => ("Deletion in progress", "Object listing only"),
        Some("Flushed") => ("Archive emptied", "Object listing only"),
        _ => ("Unknown state", "Access not confirmed"),
    }
}

fn report(mut probe: Value, known: &[Value], active: i64) -> Value {
    let generic = probe["provider"] == "s3_compatible";
    let (label, access) = if generic {
        (
            "S3 compatible",
            "Access depends on S3 permissions and object availability",
        )
    } else {
        state_label(probe["raw_status"].as_str())
    };
    let complete = probe["inventory"]["complete"] == true;
    let remote = probe["inventory"]["manifests"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let missing = if complete {
        known
            .iter()
            .filter(|k| !remote.iter().any(|r| r["key"] == k["key"]))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let uncatalogued = remote
        .iter()
        .filter(|r| !known.iter().any(|k| k["key"] == r["key"]))
        .count();
    let empty = probe["inventory"]["cartridges"]
        .as_array()
        .is_none_or(|cartridges| cartridges.is_empty());
    let mut checks = Vec::new();
    let mut check = |id, label, level, detail: String| {
        checks.push(json!({"id":id,"label":label,"level":level,"detail":detail}))
    };
    check(
        "provider",
        "S3-compatible storage",
        if probe["supported"] == true {
            "pass"
        } else {
            "unknown"
        },
        "Diagnostic specific to the configured Cold Archive v1 bucket.".into(),
    );
    check(
        "state",
        "State compatible with a first archive",
        match probe["raw_status"].as_str() {
            Some("None") => "pass",
            Some("Archiving" | "Archived" | "Restoring" | "Restored" | "Deleting" | "Flushed") => {
                "blocked"
            }
            _ => "unknown",
        },
        format!("{label} · {access}"),
    );
    check(
        "multipart",
        "Incomplete multipart uploads",
        match probe["multipart_uploads"].as_u64() {
            Some(0) => "pass",
            Some(_) => "blocked",
            None => "unknown",
        },
        probe["multipart_uploads"]
            .as_u64()
            .map(|n| format!("{n} incomplete upload(s) in the entire bucket."))
            .unwrap_or_else(|| "Unreadable: check access or network.".into()),
    );
    check(
        "inventory",
        "Bucket inventory",
        if complete { "pass" } else { "unknown" },
        if complete {
            "Current object listing complete; no content downloaded.".into()
        } else {
            "Inventory unavailable or limited to 100,000 objects; scope not confirmed.".into()
        },
    );
    let foreign = probe["inventory"]["outside_prefix"].as_u64().unwrap_or(0)
        + probe["inventory"]["unrecognized_objects"]
            .as_u64()
            .unwrap_or(0);
    check(
        "scope",
        "Objects outside the recognised scope",
        if !complete {
            "unknown"
        } else if foreign > 0 {
            "blocked"
        } else {
            "pass"
        },
        format!(
            "{foreign} object(s) outside the prefix or unrecognised. They would also be affected by archiving."
        ),
    );
    check(
        "catalogue",
        "Match with the catalogue",
        if !complete {
            "unknown"
        } else if uncatalogued > 0 || !missing.is_empty() || empty {
            "blocked"
        } else {
            "pass"
        },
        format!(
            "{uncatalogued} uncatalogued manifest(s), {} catalogue manifest(s) missing from the listing. Checksums are not re-verified by this diagnostic.",
            missing.len()
        ),
    );
    check(
        "jobs",
        "VTL jobs finished",
        if active == 0 { "pass" } else { "blocked" },
        format!("{active} job(s) running."),
    );
    check(
        "retention",
        "Archive lock",
        if probe["raw_status"].is_null() {
            "unknown"
        } else if probe["archive_lock_until"].is_null() {
            "pass"
        } else {
            "blocked"
        },
        probe["archive_lock_until"]
            .as_str()
            .map(|v| format!("Lock reported until {v}; scope and expiry to be checked."))
            .unwrap_or_else(|| {
                "No lock read; its presence must be checked again before any action.".into()
            }),
    );
    check("versions", "Historical object versions", "unknown", "The listing covers current objects. Historical versions and delete markers are not examined.".into());
    probe["state_label"] = json!(label);
    probe["object_access"] = json!(access);
    probe["checked_at"] = json!(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
    if generic {
        checks.retain(|item| {
            matches!(
                item["id"].as_str(),
                Some("inventory" | "catalogue" | "versions")
            )
        });
        checks.insert(0, json!({"id":"provider","label":"S3-compatible provider","level":"pass","detail":"Managed archiving and recall unavailable. Object availability depends on the provider."}));
    }
    probe["checks"] = json!(checks);
    probe["catalogue_generations"] = json!(known);
    probe["catalogue_missing_from_listing"] = json!(missing);
    probe["actions_available"] = json!(false);
    probe
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generic_storage_keeps_inventory_without_ovh_archive_claims() {
        let value = report(
            json!({"provider":"s3_compatible","supported":false,"raw_status":null,"inventory":{"complete":true,"manifests":[],"cartridges":[]}}),
            &[],
            0,
        );
        assert_eq!(value["state_label"], "S3 compatible");
        assert_eq!(value["inventory"]["complete"], true);
        assert_eq!(value["actions_available"], false);
        assert!(
            !value["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["id"] == "state" || c["id"] == "maintenance")
        );
    }
    #[tokio::test]
    async fn endpoint_runs_only_the_read_probe_and_does_not_create_jobs() {
        use std::{os::unix::fs::PermissionsExt, sync::Arc};
        let root = std::env::temp_dir().join(format!("cold-vtl-storage-{}", uuid::Uuid::new_v4()));
        let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
            .await
            .unwrap();
        let worker = root.join("worker");
        std::fs::write(&worker,"#!/bin/sh\n[ \"$#\" = 1 ] && [ \"$1\" = storage-status ] || exit 42\nprintf '%s' '{\"supported\":true,\"bucket\":\"test\",\"raw_status\":\"None\",\"multipart_uploads\":0,\"inventory\":{\"complete\":true,\"cartridges\":[],\"manifests\":[]}}'\n").unwrap();
        std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = AppState {
            catalog: catalog.clone(),
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(crate::AppConfig {
                storage_socket: None,
                media_root: root.join("media"),
                restore_root: root.join("restores"),
                worker_path: worker,
                quiesce_check: None,
                maintenance_helper: None,
                cartridge_helper: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        };
        let result = status(State(state)).await.unwrap().0;
        assert_eq!(result["bucket"], "test");
        assert_eq!(result["actions_available"], false);
        assert_eq!(result["managed"], false);
        assert!(result["next_bucket"].as_str().unwrap().starts_with("vc-"));
        assert!(result.get("maintenance").is_none());
        assert!(catalog.list_jobs().await.unwrap().is_empty());
        assert!(catalog.list_cartridges().await.unwrap().is_empty());
        catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn unknown_states_are_never_reported_as_available_and_restored_is_read_only() {
        assert_eq!(
            state_label(Some("Restored")),
            ("Recall complete", "Read-only")
        );
        for state in [None, Some("Enabled"), Some("unexpected")] {
            assert_eq!(state_label(state).0, "Unknown state");
        }
    }
    #[test]
    fn partial_inventory_and_denied_checks_remain_unknown() {
        let report = report(
            json!({"supported":true,"raw_status":null,"multipart_uploads":null,"inventory":{"complete":false}}),
            &[],
            0,
        );
        for id in ["state", "multipart", "inventory", "scope", "catalogue"] {
            assert_eq!(
                report["checks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["id"] == id)
                    .unwrap()["level"],
                "unknown"
            );
        }
        assert_eq!(report["actions_available"], false);
    }
    #[test]
    fn unknown_media_missing_manifests_and_active_jobs_block_prechecks() {
        let report = report(
            json!({"supported":true,"raw_status":"None","multipart_uploads":2,"inventory":{"complete":true,"outside_prefix":1,"unrecognized_objects":0,"cartridges":[{"barcode":"TAPE01"}],"manifests":[{"key":"other","barcode":"TAPE01","generation":2}]}}),
            &[json!({"key":"missing"})],
            1,
        );
        for id in ["multipart", "scope", "catalogue", "jobs"] {
            assert_eq!(
                report["checks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["id"] == id)
                    .unwrap()["level"],
                "blocked"
            );
        }
        assert_eq!(
            report["catalogue_missing_from_listing"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            !report["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["id"] == "maintenance" || c["id"] == "verified")
        );
    }
}
