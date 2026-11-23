use super::*;
use std::{
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    sync::Arc,
};

fn script(path: &std::path::Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

async fn fixture() -> (AppState, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("online-export-{}", uuid::Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
        .await
        .unwrap();
    let id = catalog.vtl_id().await.unwrap().to_string();
    fs::create_dir_all(root.join("media/TAPE01")).unwrap();
    script(
        &root.join("helper"),
        &format!(
            "#!/bin/sh\ncat >/dev/null\necho '{{\"vtl_id\":\"{id}\",\"media_lock_version\":1}}'\n"
        ),
    );
    script(
        &root.join("maintenance"),
        "#!/bin/sh\ncat >/dev/null\nexit 1\n",
    );
    let state = AppState {
        catalog,
        operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
        config: Arc::new(crate::AppConfig {
            storage_socket: None,
            media_root: root.join("media"),
            restore_root: root.join("restores"),
            worker_path: root.join("worker"),
            maintenance_helper: Some(root.join("maintenance")),
            cartridge_helper: Some(root.join("helper")),
            quiesce_check: None,
            reintegration_helper: None,
            archive_helper: None,
            telemetry_dir: None,
            telemetry_retention_days: 30,
        }),
    };
    (state, root)
}

#[tokio::test]
async fn online_guard_excludes_drive_without_stopping_library() {
    let (state, root) = fixture().await;
    let guard = protect(&state, "TAPE01").await.unwrap();
    let drive = File::open(root.join("media/TAPE01")).unwrap();
    assert!(drive.try_lock().is_err());
    guard.check(&state).await.unwrap();
    drop(guard);
    drive.try_lock().unwrap();
    assert!(protect(&state, "TAPE01").await.is_err());
    drop(drive);
    script(
        &root.join("helper"),
        "#!/bin/sh\ncat >/dev/null\necho '{\"vtl_id\":\"foreign\",\"media_lock_version\":1}'\n",
    );
    assert!(protect(&state, "TAPE01").await.is_err());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn writable_cartridge_sends_directly_and_keeps_protection_through_verification() {
    use axum::extract::{Path, State};
    use cold_vtl_core::{Barcode, Cartridge, CartridgeState};
    let (state, root) = fixture().await;
    let tape = Cartridge::new(Barcode::parse("TAPE01").unwrap(), 1024).unwrap();
    state.catalog.create_cartridge(&tape).await.unwrap();
    // Worker boundaries use real subprocesses; observe the API-held lock in both.
    script(
        &root.join("worker"),
        &format!(
            r#"#!/usr/bin/env python3
import sys,json,fcntl,os
if sys.argv[1]=='storage-access': print(json.dumps({{'readable':True,'writable':True}}));sys.exit()
fd=os.open({source:?},os.O_RDONLY)
try: fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
except BlockingIOError: pass
else: raise RuntimeError('API released cartridge protection')
if sys.argv[1]=='export-directory': print('{{"status":"verified"}}')
else: print(json.dumps({{'local_size_bytes':15,'generations':[{{'barcode':'TAPE01','generation':1,'format_version':2,'manifest_key':'vtl/cartridges/TAPE01/manifests/1.json','manifest_sha256':'a'*64,'logical_size_bytes':15,'file_count':1,'segment_count':1,'local_match':True}}]}}))
"#,
            source = root.join("media/TAPE01").to_string_lossy()
        ),
    );
    let result = crate::export_cartridge(State(state.clone()), Path("TAPE01".into())).await;
    assert!(result.is_ok(), "direct send refused: {:?}", result.err());
    let (_, axum::Json(job)) = result.unwrap();
    for _ in 0..100 {
        if state.catalog.job(&job.id).await.unwrap().unwrap().status == "completed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let finished = state.catalog.job(&job.id).await.unwrap().unwrap();
    assert_eq!(finished.status, "completed", "{:?}", finished.message);
    assert_eq!(
        state
            .catalog
            .cartridge(&tape.barcode)
            .await
            .unwrap()
            .unwrap()
            .state,
        CartridgeState::Verified
    );
    File::open(root.join("media/TAPE01"))
        .unwrap()
        .try_lock()
        .unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn reconciliation_keeps_protection_from_preflight_through_worker_completion() {
    use axum::extract::{Path, State};
    use cold_vtl_core::{Barcode, Cartridge};
    let (state, root) = fixture().await;
    let tape = Cartridge::new(Barcode::parse("TAPE01").unwrap(), 1024).unwrap();
    state.catalog.create_cartridge(&tape).await.unwrap();
    script(
        &root.join("worker"),
        &format!(
            r#"#!/usr/bin/env python3
import sys,json,fcntl,os
fd=os.open({source:?},os.O_RDONLY)
try: fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
except BlockingIOError: pass
else: raise RuntimeError('API released cartridge protection at '+sys.argv[1])
with open({observed:?},'a') as output: output.write(sys.argv[1]+'\n')
if sys.argv[1]=='storage-access': print(json.dumps({{'readable':True,'writable':True}}))
elif sys.argv[1]=='reconcile': print(json.dumps({{'local_size_bytes':15,'generations':[]}}))
else: raise RuntimeError('unexpected worker action')
"#,
            source = root.join("media/TAPE01").to_string_lossy(),
            observed = root.join("observed").to_string_lossy()
        ),
    );
    let result = crate::operations::reconcile(State(state.clone()), Path("TAPE01".into())).await;
    assert!(
        result.is_ok(),
        "reconciliation preflight lost protection: {:?}",
        result.err()
    );
    let (_, axum::Json(job)) = result.unwrap();
    for _ in 0..100 {
        if state.catalog.job(&job.id).await.unwrap().unwrap().status == "completed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let finished = state.catalog.job(&job.id).await.unwrap().unwrap();
    assert_eq!(finished.status, "completed", "{:?}", finished.message);
    assert_eq!(
        fs::read_to_string(root.join("observed")).unwrap(),
        "storage-access\nreconcile\n"
    );
    assert_eq!(
        state
            .catalog
            .cartridge(&tape.barcode)
            .await
            .unwrap()
            .unwrap()
            .bytes_written,
        15
    );
    File::open(root.join("media/TAPE01"))
        .unwrap()
        .try_lock()
        .unwrap();
    fs::remove_dir_all(root).unwrap();
}

async fn native_fixture() -> (AppState, std::path::PathBuf) {
    let (state, root) = fixture().await;
    sqlx::query("UPDATE storage_destinations SET identity=? WHERE active=1")
        .bind(r#"{"provider":"s3_compatible","endpoint":"http://127.0.0.1:9000","region":"local","bucket":"test-bucket","prefix":"vtl"}"#).execute(&state.catalog.pool()).await.unwrap();
    let destination = state.catalog.destination().await.unwrap().id;
    sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,1,?)")
        .bind(destination).bind(r#"{"provider":"s3_compatible","endpoint":"http://127.0.0.1:9000","region":"local","bucket":"test-bucket","prefix":"vtl","access_key_id":"test","secret_access_key":"test"}"#).execute(&state.catalog.pool()).await.unwrap();
    (state, root)
}

#[tokio::test]
async fn native_export_receipt_finishes_without_reconciliation_and_persists_evidence() {
    use axum::extract::{Path, State};
    use cold_vtl_core::{Barcode, Cartridge};
    let (state, root) = native_fixture().await;
    let mut tape = Cartridge::new(Barcode::parse("TAPE01").unwrap(), 1024).unwrap();
    tape.generation = 2;
    state.catalog.create_cartridge(&tape).await.unwrap();
    let old = serde_json::json!({"barcode":"TAPE01","generation":1,"format_version":2,"manifest_key":"vtl/cartridges/TAPE01/manifests/1.json","manifest_sha256":"b".repeat(64),"logical_size_bytes":10,"file_count":1,"segment_count":1,"local_match":true});
    sqlx::query("INSERT INTO remote_generations(vtl_id,destination_id,barcode,generation,metadata,cartridge_id) SELECT ?,?,'TAPE01',1,?,id FROM cartridges WHERE barcode='TAPE01'")
        .bind(state.catalog.vtl_id().await.unwrap().to_string()).bind(state.catalog.destination().await.unwrap().id).bind(old.to_string()).execute(&state.catalog.pool()).await.unwrap();
    script(
        &root.join("worker"),
        r#"#!/usr/bin/env python3
import sys,json,os
if sys.argv[1]=='storage-access': print(json.dumps({'readable':True,'writable':True}));sys.exit()
if sys.argv[1]!='export-directory': raise RuntimeError('unexpected deep reconciliation')
value={'status':'verified','receipt':{'generation':{'barcode':'TAPE01','generation':2,'format_version':3,'manifest_key':'vtl/cartridges/TAPE01/manifests/2.json','manifest_sha256':'a'*64,'logical_size_bytes':15,'file_count':1,'segment_count':1,'local_match':True},'integrity':{'status':'verified','method':'s3_sha256','verified_at':1792065762,'verified_segments':1,'verified_bytes':15}}}
value['receipt']['scope']={'job_id':os.environ['COLD_VTL_EXPORT_JOB_ID'],'vtl_id':os.environ['COLD_VTL_VTL_ID'],'destination_id':os.environ['COLD_VTL_DESTINATION_ID'],'configuration_revision':int(os.environ['COLD_VTL_CONFIGURATION_REVISION']),'source':os.path.realpath(sys.argv[sys.argv.index('--source')+1])}
print(json.dumps(value))
"#,
    );
    let (_, axum::Json(job)) = crate::export_cartridge(State(state.clone()), Path("TAPE01".into()))
        .await
        .unwrap();
    for _ in 0..100 {
        let finished = state.catalog.job(&job.id).await.unwrap().unwrap();
        if !matches!(finished.status.as_str(), "running" | "queued") {
            assert_eq!(finished.status, "completed", "{:?}", finished.message);
            let message: serde_json::Value =
                serde_json::from_str(finished.message.as_ref().unwrap()).unwrap();
            assert_eq!(message["receipt"]["integrity"]["status"], "verified");
            assert_eq!(message["receipt"]["integrity"]["verified_bytes"], 15);
            let generations = state.catalog.generations(&tape.barcode).await.unwrap();
            assert_eq!(
                generations.len(),
                2,
                "older backup disappeared from catalogue"
            );
            assert!(
                !generations
                    .iter()
                    .find(|g| g.generation == 1)
                    .unwrap()
                    .local_match
            );
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("export did not finish");
}

#[tokio::test]
async fn native_export_rejects_foreign_or_incomplete_integrity_receipts() {
    use axum::extract::{Path, State};
    use cold_vtl_core::{Barcode, Cartridge};
    for mutation in ["barcode", "count", "status", "scope", "prefix", "null"] {
        let (state, root) = native_fixture().await;
        state
            .catalog
            .create_cartridge(&Cartridge::new(Barcode::parse("TAPE01").unwrap(), 1024).unwrap())
            .await
            .unwrap();
        let mut value = serde_json::json!({"status":"verified","receipt":{
            "generation":{"barcode":"TAPE01","generation":1,"format_version":3,"manifest_key":"vtl/cartridges/TAPE01/manifests/1.json","manifest_sha256":"a".repeat(64),"logical_size_bytes":15,"file_count":1,"segment_count":1,"local_match":true},
            "integrity":{"status":"verified","method":"s3_sha256","verified_at":1792065762,"verified_segments":1,"verified_bytes":15}}});
        match mutation {
            "barcode" => value["receipt"]["generation"]["barcode"] = "TAPE02".into(),
            "count" => value["receipt"]["integrity"]["verified_bytes"] = 14.into(),
            "status" => value["receipt"]["integrity"]["status"] = "uploading".into(),
            "scope" => {
                value["receipt"]["scope"] = serde_json::json!({"vtl_id":"foreign","destination_id":"foreign","configuration_revision":1,"job_id":"foreign","source":"/foreign"})
            }
            "prefix" => {
                value["receipt"]["generation"]["manifest_key"] =
                    "foreign/cartridges/TAPE01/manifests/1.json".into()
            }
            _ => value["receipt"] = serde_json::Value::Null,
        }
        script(
            &root.join("worker"),
            &format!(
                "#!/usr/bin/env python3\nimport sys,json,os\nif sys.argv[1]=='storage-access': print(json.dumps({{'readable':True,'writable':True}}));sys.exit()\nvalue=json.loads({:?})\nif isinstance(value.get('receipt'),dict) and 'scope' not in value['receipt']: value['receipt']['scope']={{'job_id':os.environ['COLD_VTL_EXPORT_JOB_ID'],'vtl_id':os.environ['COLD_VTL_VTL_ID'],'destination_id':os.environ['COLD_VTL_DESTINATION_ID'],'configuration_revision':int(os.environ['COLD_VTL_CONFIGURATION_REVISION']),'source':os.path.realpath(sys.argv[sys.argv.index('--source')+1])}}\nprint(json.dumps(value))\n",
                value.to_string()
            ),
        );
        let (_, axum::Json(job)) =
            crate::export_cartridge(State(state.clone()), Path("TAPE01".into()))
                .await
                .unwrap();
        for _ in 0..100 {
            let finished = state.catalog.job(&job.id).await.unwrap().unwrap();
            if !matches!(finished.status.as_str(), "running" | "queued") {
                assert_eq!(finished.status, "failed", "accepted {mutation}");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            state.catalog.job(&job.id).await.unwrap().unwrap().status,
            "failed"
        );
    }
}

#[tokio::test]
async fn export_preserves_measured_occupancy_before_upload_and_after_failure() {
    use axum::extract::{Path, State};
    use cold_vtl_core::{Barcode, Cartridge, CartridgeState};
    let (state, root) = fixture().await;
    let tape = Cartridge::new(Barcode::parse("TAPE01").unwrap(), 10000).unwrap();
    state.catalog.create_cartridge(&tape).await.unwrap();
    fs::write(root.join("media/TAPE01/data"), vec![0; 4096]).unwrap();
    fs::create_dir(root.join("media/TAPE01/nested")).unwrap();
    fs::write(root.join("media/TAPE01/nested/index"), vec![0; 512]).unwrap();
    script(
        &root.join("worker"),
        "#!/bin/sh\nif [ \"$1\" = storage-access ]; then echo '{\"readable\":true,\"writable\":true}'; else exit 3; fi\n",
    );
    let (_, axum::Json(job)) = crate::export_cartridge(State(state.clone()), Path("TAPE01".into()))
        .await
        .unwrap();
    assert_eq!(
        state
            .catalog
            .cartridge(&tape.barcode)
            .await
            .unwrap()
            .unwrap()
            .bytes_written,
        4608,
        "reservation must persist occupancy before the worker finishes"
    );
    for _ in 0..100 {
        if state.catalog.job(&job.id).await.unwrap().unwrap().status == "failed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        state.catalog.job(&job.id).await.unwrap().unwrap().status,
        "failed"
    );
    let failed = state
        .catalog
        .cartridge(&tape.barcode)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.state, CartridgeState::Failed);
    assert_eq!(
        failed.bytes_written, 4608,
        "a failed export must not reset occupancy"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn sealing_captures_measured_occupancy_without_reading_payload() {
    use axum::extract::{Path, State};
    use cold_vtl_core::{Barcode, Cartridge, CartridgeState};
    let (state, root) = fixture().await;
    let tape = Cartridge::new(Barcode::parse("TAPE01").unwrap(), 2 * 1024_u64.pow(4)).unwrap();
    state.catalog.create_cartridge(&tape).await.unwrap();
    File::create(root.join("media/TAPE01/data"))
        .unwrap()
        .set_len(1024_u64.pow(4))
        .unwrap();
    let axum::Json(sealed) = crate::seal_cartridge(State(state.clone()), Path("TAPE01".into()))
        .await
        .unwrap();
    assert_eq!(sealed.state, CartridgeState::Sealed);
    assert_eq!(sealed.bytes_written, 1024_u64.pow(4));
    assert_eq!(
        state
            .catalog
            .cartridge(&tape.barcode)
            .await
            .unwrap()
            .unwrap()
            .bytes_written,
        1024_u64.pow(4)
    );
    fs::remove_dir_all(root).unwrap();
}
