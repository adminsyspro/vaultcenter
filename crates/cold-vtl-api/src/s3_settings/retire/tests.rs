use super::*;
use crate::s3_settings::test_support::{calls, dispatch_worker, fake_worker, fixture};
use axum::response::IntoResponse;

/// An inactive bucket `missing-bucket` with its own connection revision.
async fn older(state: &AppState) -> String {
    let vtl = state.catalog.vtl_id().await.unwrap().to_string();
    let pool = state.catalog.pool();
    let identity = json!({"provider":"ovh_cold_archive_v1","endpoint":cold_vtl_core::OVH_ARCHIVE_ENDPOINT,"region":"rbx-archive","bucket":"missing-bucket","prefix":"vtl"});
    let mut config = identity.clone();
    config["access_key_id"] = json!("key");
    config["secret_access_key"] = json!("private-secret");
    sqlx::query(
        "INSERT INTO storage_destinations(id,vtl_id,identity,active) VALUES('older',?,?,0)",
    )
    .bind(&vtl)
    .bind(identity.to_string())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES('older',2,?)")
        .bind(config.to_string())
        .execute(&pool)
        .await
        .unwrap();
    "older".into()
}

async fn listed(state: &AppState) -> Vec<String> {
    let Json(value) = crate::destinations::buckets(State(state.clone()))
        .await
        .unwrap();
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|bucket| bucket["bucket"].as_str().unwrap().to_owned())
        .collect()
}

async fn retired_at(state: &AppState, id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT retired_at FROM storage_destinations WHERE id=?")
        .bind(id)
        .fetch_one(&state.catalog.pool())
        .await
        .unwrap()
}

async fn refused(state: &AppState, id: &str) -> (axum::http::StatusCode, String) {
    let error = retire(State(state.clone()), Path(id.to_owned()))
        .await
        .unwrap_err();
    let response = error.into_response();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    (status, value["error"].as_str().unwrap().to_owned())
}

#[tokio::test]
async fn bucket_absent_at_ovh_without_copies_is_retired_and_hidden() {
    let (state, root, _) = fixture().await;
    let id = older(&state).await;
    dispatch_worker(&root, "absent", "empty");
    assert_eq!(listed(&state).await, ["current-bucket", "missing-bucket"]);
    let Json(value) = retire(State(state.clone()), Path(id.clone()))
        .await
        .unwrap();
    assert_eq!(value["destination_id"], "older");
    assert_eq!(value["bucket"], "missing-bucket");
    assert!(retired_at(&state, &id).await.is_some());
    assert_eq!(listed(&state).await, ["current-bucket"]);
    assert_eq!(calls(&root), ["bucket-presence"]);
    // Retiring twice is refused: the bucket is no longer listed.
    assert_eq!(
        refused(&state, &id).await.0,
        axum::http::StatusCode::NOT_FOUND
    );
    state.catalog.pool().close().await;
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn active_bucket_is_never_retired() {
    let (state, root, _) = fixture().await;
    let active = state.catalog.destination().await.unwrap().id;
    dispatch_worker(&root, "absent", "empty");
    let (status, message) = refused(&state, &active).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert!(message.contains("active"), "{message}");
    assert!(calls(&root).is_empty());
    state.catalog.pool().close().await;
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn bucket_holding_a_copy_is_never_retired() {
    let (state, root, _) = fixture().await;
    let id = older(&state).await;
    let tape =
        cold_vtl_core::Cartridge::new(cold_vtl_core::Barcode::parse("TAPE01").unwrap(), 1024)
            .unwrap();
    state.catalog.create_cartridge(&tape).await.unwrap();
    let vtl = state.catalog.vtl_id().await.unwrap().to_string();
    sqlx::query("INSERT INTO remote_generations(vtl_id,destination_id,barcode,generation,metadata,cartridge_id,checked_at) SELECT vtl_id,'older',barcode,1,'{}',id,CURRENT_TIMESTAMP FROM cartridges WHERE vtl_id=? AND barcode='TAPE01'")
        .bind(&vtl).execute(&state.catalog.pool()).await.unwrap();
    dispatch_worker(&root, "absent", "empty");
    let (status, message) = refused(&state, &id).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert!(message.contains("copy"), "{message}");
    assert!(calls(&root).is_empty());
    assert!(retired_at(&state, &id).await.is_none());
    state.catalog.pool().close().await;
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn bucket_with_an_ovh_request_or_a_job_in_progress_is_never_retired() {
    let (state, root, _) = fixture().await;
    let id = older(&state).await;
    let vtl = state.catalog.vtl_id().await.unwrap().to_string();
    let pool = state.catalog.pool();
    sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id,destination_id) VALUES('submit','archive_submit','missing-bucket','completed',?,'older')")
        .bind(&vtl).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,operation,vtl_id,destination_id) VALUES('submit','submit','accepted','{\"confirm_bucket\":\"missing-bucket\"}','archive',?,'older')")
        .bind(&vtl).execute(&pool).await.unwrap();
    dispatch_worker(&root, "absent", "empty");
    let (status, message) = refused(&state, &id).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert!(message.contains("in progress"), "{message}");
    sqlx::query("UPDATE archive_requests SET phase='failed'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id,destination_id) VALUES('read','reconcile','TAPE01','running',?,'older')")
        .bind(&vtl).execute(&pool).await.unwrap();
    assert_eq!(
        refused(&state, &id).await.0,
        axum::http::StatusCode::CONFLICT
    );
    assert!(calls(&root).is_empty());
    // Finished history does not keep the bucket listed.
    sqlx::query("UPDATE jobs SET status='completed' WHERE id='read'")
        .execute(&pool)
        .await
        .unwrap();
    let _ = retire(State(state.clone()), Path(id.clone()))
        .await
        .unwrap();
    state.catalog.pool().close().await;
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn bucket_not_confirmed_absent_by_ovh_is_kept() {
    let (state, root, _) = fixture().await;
    let id = older(&state).await;
    dispatch_worker(&root, "exists", "empty");
    let (status, message) = refused(&state, &id).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert!(message.contains("OVH"), "{message}");
    assert!(retired_at(&state, &id).await.is_none());
    // No answer from OVH: kept too.
    fake_worker(&root, "lost", 1);
    assert_eq!(
        refused(&state, &id).await.0,
        axum::http::StatusCode::CONFLICT
    );
    assert!(retired_at(&state, &id).await.is_none());
    state.catalog.pool().close().await;
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn unknown_or_foreign_bucket_is_not_found() {
    let (state, root, _) = fixture().await;
    dispatch_worker(&root, "absent", "empty");
    assert_eq!(
        refused(&state, "nope").await.0,
        axum::http::StatusCode::NOT_FOUND
    );
    let other = cold_vtl_core::VtlId::new().to_string();
    let pool = state.catalog.pool();
    sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'Other','ready')")
        .bind(&other)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO storage_destinations(id,vtl_id,identity,active) VALUES('foreign',?,'{\"bucket\":\"x\"}',0)").bind(&other).execute(&pool).await.unwrap();
    assert_eq!(
        refused(&state, "foreign").await.0,
        axum::http::StatusCode::NOT_FOUND
    );
    assert!(calls(&root).is_empty());
    state.catalog.pool().close().await;
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn activating_a_retired_bucket_shows_it_again() {
    let (state, root, _) = fixture().await;
    let id = older(&state).await;
    dispatch_worker(&root, "absent", "empty");
    let _ = retire(State(state.clone()), Path(id.clone()))
        .await
        .unwrap();
    let vtl = state.catalog.vtl_id().await.unwrap().to_string();
    sqlx::query("UPDATE storage_destinations SET active=0 WHERE vtl_id=?")
        .bind(&vtl)
        .execute(&state.catalog.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE storage_destinations SET active=1 WHERE id=?")
        .bind(&id)
        .execute(&state.catalog.pool())
        .await
        .unwrap();
    assert!(retired_at(&state, &id).await.is_none());
    assert!(listed(&state).await.contains(&"missing-bucket".to_owned()));
    state.catalog.pool().close().await;
    std::fs::remove_dir_all(root).ok();
}
