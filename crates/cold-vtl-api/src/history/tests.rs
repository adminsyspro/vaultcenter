use super::*;
use sqlx::SqlitePool;

type Line = (String, i64, String, Option<String>);

/// Register lines as (barcode, generation, recorded_at, removed_at).
async fn register(pool: &SqlitePool) -> Vec<Line> {
    sqlx::query_as("SELECT barcode,generation,recorded_at,removed_at FROM copy_register ORDER BY barcode,generation")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// The shared S3 fixture with cartridge TAPE01: (state, root, vtl, active destination).
async fn setup() -> (crate::AppState, std::path::PathBuf, String, String) {
    let (state, root) = crate::s3_settings::test_support::state_fixture().await;
    let tape =
        cold_vtl_core::Cartridge::new(cold_vtl_core::Barcode::parse("TAPE01").unwrap(), 1024)
            .unwrap();
    state.catalog.create_cartridge(&tape).await.unwrap();
    let vtl = state.catalog.vtl_id().await.unwrap().to_string();
    let destination = state.catalog.destination().await.unwrap().id;
    (state, root, vtl, destination)
}

async fn insert_copy(
    pool: &SqlitePool,
    vtl: &str,
    destination: &str,
    generation: i64,
    metadata: &str,
    checked_at: &str,
) {
    sqlx::query("INSERT INTO remote_generations(vtl_id,destination_id,barcode,generation,metadata,cartridge_id,checked_at) SELECT vtl_id,?,barcode,?,?,id,? FROM cartridges WHERE vtl_id=? AND barcode='TAPE01'")
        .bind(destination).bind(generation).bind(metadata).bind(checked_at).bind(vtl)
        .execute(pool).await.unwrap();
}

async fn delete_copy(pool: &SqlitePool, generation: i64) {
    sqlx::query("DELETE FROM remote_generations WHERE barcode='TAPE01' AND generation=?")
        .bind(generation)
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn export_records_a_copy_once() {
    let (state, root, vtl, destination) = setup().await;
    let pool = state.catalog.pool();
    insert_copy(
        &pool,
        &vtl,
        &destination,
        1,
        r#"{"logical_size_bytes":42,"manifest_sha256":"ab"}"#,
        "2026-10-01 10:00:00",
    )
    .await;
    assert_eq!(
        register(&pool).await,
        vec![("TAPE01".into(), 1, "2026-10-01 10:00:00".into(), None)]
    );
    let (bytes, manifest): (Option<i64>, Option<String>) =
        sqlx::query_as("SELECT bytes,manifest_sha256 FROM copy_register")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((bytes, manifest.as_deref()), (Some(42), Some("ab")));
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn resync_keeps_export_date_and_removes_nothing() {
    let (state, root, vtl, destination) = setup().await;
    let pool = state.catalog.pool();
    insert_copy(&pool, &vtl, &destination, 1, "{}", "2026-10-01 10:00:00").await;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("DELETE FROM remote_generations WHERE barcode='TAPE01'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO remote_generations(vtl_id,destination_id,barcode,generation,metadata,cartridge_id,checked_at) SELECT vtl_id,?,barcode,1,'{}',id,'2026-10-20 10:00:00' FROM cartridges WHERE vtl_id=? AND barcode='TAPE01'")
        .bind(&destination).bind(&vtl).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        register(&pool).await,
        vec![("TAPE01".into(), 1, "2026-10-01 10:00:00".into(), None)]
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn sync_update_refreshes_size() {
    let (state, root, vtl, destination) = setup().await;
    let pool = state.catalog.pool();
    insert_copy(
        &pool,
        &vtl,
        &destination,
        1,
        r#"{"logical_size_bytes":42}"#,
        "2026-10-01 10:00:00",
    )
    .await;
    sqlx::query(r#"UPDATE remote_generations SET metadata='{"logical_size_bytes":99}'"#)
        .execute(&pool)
        .await
        .unwrap();
    let bytes: Option<i64> = sqlx::query_scalar("SELECT bytes FROM copy_register")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(bytes, Some(99));
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn deleted_copy_is_kept_with_its_removal_date() {
    let (state, root, vtl, destination) = setup().await;
    let pool = state.catalog.pool();
    insert_copy(&pool, &vtl, &destination, 1, "{}", "2026-10-01 10:00:00").await;
    delete_copy(&pool, 1).await;
    let lines = register(&pool).await;
    assert_eq!(lines.len(), 1);
    assert!(lines[0].3.is_some(), "{lines:?}");
    insert_copy(&pool, &vtl, &destination, 1, "{}", "2026-10-02 10:00:00").await;
    assert_eq!(
        register(&pool).await,
        vec![("TAPE01".into(), 1, "2026-10-01 10:00:00".into(), None)]
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn new_generation_after_s3_delete_is_a_new_line() {
    let (state, root, vtl, destination) = setup().await;
    let pool = state.catalog.pool();
    insert_copy(&pool, &vtl, &destination, 1, "{}", "2026-10-01 10:00:00").await;
    delete_copy(&pool, 1).await;
    insert_copy(&pool, &vtl, &destination, 2, "{}", "2026-10-03 10:00:00").await;
    let lines = register(&pool).await;
    assert_eq!(lines.len(), 2);
    assert!(lines[0].3.is_some());
    assert_eq!(
        lines[1],
        ("TAPE01".into(), 2, "2026-10-03 10:00:00".into(), None)
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn archived_at_is_set_once() {
    let (state, root, vtl, destination) = setup().await;
    let pool = state.catalog.pool();
    sqlx::query("INSERT INTO jobs(id,vtl_id,kind,subject,status) VALUES('submit',?,'archive_submit','current-bucket','completed')").bind(&vtl).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,vtl_id,destination_id) VALUES('submit','submit','accepted','{}',?,?)").bind(&vtl).bind(&destination).execute(&pool).await.unwrap();
    sqlx::query("UPDATE archive_requests SET phase='archived'")
        .execute(&pool)
        .await
        .unwrap();
    let first: Option<String> = sqlx::query_scalar("SELECT archived_at FROM archive_requests")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(first.is_some());
    sqlx::query("UPDATE archive_requests SET archived_at='2026-01-01 00:00:00'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE archive_requests SET phase='archived',observed_at='2026-10-30 00:00:00'")
        .execute(&pool)
        .await
        .unwrap();
    let second: Option<String> = sqlx::query_scalar("SELECT archived_at FROM archive_requests")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(second.as_deref(), Some("2026-01-01 00:00:00"));
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn migration_backfills_existing_copies_and_archives() {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let mut before = sqlx::migrate!("./migrations");
    before.migrations = before
        .migrations
        .iter()
        .filter(|m| m.version < 25)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    before.run(&pool).await.unwrap();
    let (vtl, destination) = (
        "00000000-0000-4000-8000-000000000001",
        "00000000-0000-4000-8000-000000000002",
    );
    for barcode in ["TAPE01", "TAPE02"] {
        sqlx::query("INSERT INTO cartridges(vtl_id,barcode,capacity_bytes,state) VALUES(?,?,1024,'writable')").bind(vtl).bind(barcode).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO remote_generations(vtl_id,destination_id,barcode,generation,metadata,cartridge_id,checked_at) SELECT vtl_id,?,barcode,1,'{}',id,'2026-10-20 10:00:00' FROM cartridges WHERE barcode=?")
            .bind(destination).bind(barcode).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO jobs(id,vtl_id,kind,subject,status,generation,destination_id,updated_at) VALUES('export',?,'export','TAPE01','completed',1,?,'2026-10-05 09:00:00')")
        .bind(vtl).bind(destination).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO jobs(id,vtl_id,kind,subject,status) VALUES('submit',?,'archive_submit','bucket','completed')").bind(vtl).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,vtl_id,destination_id,observed_at) VALUES('submit','submit','archived','{}',?,?,'2026-10-21 08:00:00')")
        .bind(vtl).bind(destination).execute(&pool).await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    assert_eq!(
        register(&pool).await,
        vec![
            ("TAPE01".into(), 1, "2026-10-05 09:00:00".into(), None),
            ("TAPE02".into(), 1, "2026-10-20 10:00:00".into(), None),
        ]
    );
    let archived: Option<String> = sqlx::query_scalar("SELECT archived_at FROM archive_requests")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(archived.as_deref(), Some("2026-10-21 08:00:00"));
}

/// Records an archive or recall request of `destination`, with its own job.
async fn request(
    pool: &SqlitePool,
    vtl: &str,
    destination: &str,
    id: &str,
    operation: &str,
    phase: &str,
    updated_at: &str,
) {
    sqlx::query(
        "INSERT INTO jobs(id,vtl_id,kind,subject,status) VALUES(?,?,?,'bucket','completed')",
    )
    .bind(id)
    .bind(vtl)
    .bind(format!("{operation}_submit"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,operation,vtl_id,destination_id,updated_at) VALUES(?,?,?,'{}',?,?,?,?)")
        .bind(id).bind(id).bind(phase).bind(operation).bind(vtl).bind(destination).bind(updated_at)
        .execute(pool).await.unwrap();
}

async fn only_row(pool: &SqlitePool, vtl: &str) -> HistoryRow {
    let mut rows = rows(pool, vtl).await.unwrap();
    assert_eq!(rows.len(), 1);
    rows.remove(0)
}

#[tokio::test]
async fn rows_join_bucket_archive_recall_and_local_deletion() {
    let (state, root, vtl, destination) = setup().await;
    let pool = state.catalog.pool();
    insert_copy(
        &pool,
        &vtl,
        &destination,
        1,
        r#"{"logical_size_bytes":42}"#,
        "2026-10-21 10:00:00",
    )
    .await;
    request(
        &pool,
        &vtl,
        &destination,
        "a",
        "archive",
        "archived",
        "2026-10-23 08:00:00",
    )
    .await;
    sqlx::query("UPDATE archive_requests SET archived_at='2026-10-23 08:00:00'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO cartridge_deletions(vtl_id,barcode,request_id,deleted_at) VALUES(?,'TAPE01','r','2026-10-24 09:00:00')")
        .bind(&vtl).execute(&pool).await.unwrap();
    let row = only_row(&pool, &vtl).await;
    assert_eq!(
        (row.barcode.as_str(), row.generation, row.bytes),
        ("TAPE01", 1, Some(42))
    );
    assert_eq!(row.exported_at, "2026-10-21 10:00:00");
    assert_eq!(row.destination_id, destination);
    assert_eq!(row.bucket.as_deref(), Some("current-bucket"));
    assert_eq!(row.bucket_state, BucketState::Archived);
    assert_eq!(row.archived_at.as_deref(), Some("2026-10-23 08:00:00"));
    assert_eq!((row.recalled_at, row.readable_until), (None, None));
    assert_eq!(row.local_deleted_at.as_deref(), Some("2026-10-24 09:00:00"));
    assert_eq!((row.removed_at, row.removal), (None, None));
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn bucket_state_follows_latest_request() {
    let (state, root, vtl, destination) = setup().await;
    let pool = state.catalog.pool();
    insert_copy(&pool, &vtl, &destination, 1, "{}", "2026-10-01 10:00:00").await;
    assert_eq!(
        only_row(&pool, &vtl).await.bucket_state,
        BucketState::Available
    );
    let cases: &[(&[(&str, &str)], BucketState)] = &[
        (&[("archive", "queued")], BucketState::Archiving),
        (&[("archive", "submitting")], BucketState::Archiving),
        (&[("archive", "accepted")], BucketState::Archiving),
        (&[("archive", "uncertain")], BucketState::Unknown),
        (
            &[("archive", "archived"), ("recall", "uncertain")],
            BucketState::Unknown,
        ),
        (&[("archive", "archived")], BucketState::Archived),
        (
            &[("archive", "archived"), ("recall", "accepted")],
            BucketState::Recalling,
        ),
        (
            &[("archive", "archived"), ("archive", "failed")],
            BucketState::Archived,
        ),
    ];
    for (requests, expected) in cases {
        sqlx::query("DELETE FROM archive_requests")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM jobs")
            .execute(&pool)
            .await
            .unwrap();
        for (index, (operation, phase)) in requests.iter().enumerate() {
            let at = format!("2026-10-2{index} 10:00:00");
            request(
                &pool,
                &vtl,
                &destination,
                &format!("j{index}"),
                operation,
                phase,
                &at,
            )
            .await;
        }
        assert_eq!(
            only_row(&pool, &vtl).await.bucket_state,
            *expected,
            "{requests:?}"
        );
    }
    sqlx::query("DELETE FROM archive_requests")
        .execute(&pool)
        .await
        .unwrap();
    request(
        &pool,
        &vtl,
        &destination,
        "recall",
        "recall",
        "restored",
        "2026-10-25 10:00:00",
    )
    .await;
    sqlx::query("UPDATE archive_requests SET restored_at=datetime('now','-1 day')")
        .execute(&pool)
        .await
        .unwrap();
    let row = only_row(&pool, &vtl).await;
    let (restored, until): (String, String) =
        sqlx::query_as("SELECT restored_at,datetime(restored_at,'+30 days') FROM archive_requests")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row.bucket_state, BucketState::Recalled);
    assert_eq!(row.recalled_at, Some(restored));
    assert_eq!(row.readable_until, Some(until));
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn expired_recall_reads_as_archived() {
    let (state, root, vtl, destination) = setup().await;
    let pool = state.catalog.pool();
    insert_copy(&pool, &vtl, &destination, 1, "{}", "2026-10-01 10:00:00").await;
    request(
        &pool,
        &vtl,
        &destination,
        "recall",
        "recall",
        "restored",
        "2026-10-25 10:00:00",
    )
    .await;
    sqlx::query("UPDATE archive_requests SET restored_at=datetime('now','-31 days')")
        .execute(&pool)
        .await
        .unwrap();
    let row = only_row(&pool, &vtl).await;
    assert_eq!(row.bucket_state, BucketState::Archived);
    assert!(row.recalled_at.is_some());
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn removal_cause_comes_from_a_completed_s3_delete() {
    let (state, root, vtl, destination) = setup().await;
    let pool = state.catalog.pool();
    for generation in 1..=3 {
        insert_copy(
            &pool,
            &vtl,
            &destination,
            generation,
            "{}",
            "2026-10-01 10:00:00",
        )
        .await;
    }
    delete_copy(&pool, 1).await;
    delete_copy(&pool, 2).await;
    sqlx::query("INSERT INTO jobs(id,vtl_id,kind,subject,status,generation,destination_id) VALUES('delete',?,'s3_delete','TAPE01','completed',1,?)")
        .bind(&vtl).bind(&destination).execute(&pool).await.unwrap();
    let removals: Vec<_> = rows(&pool, &vtl)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.generation, row.removal, row.removed_at.is_some()))
        .collect();
    assert_eq!(
        removals,
        vec![
            (1, Some("s3_deleted"), true),
            (2, Some("catalogue"), true),
            (3, None, false)
        ]
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn destination_without_identity_still_lists_the_copy() {
    let (state, root, vtl, destination) = setup().await;
    let pool = state.catalog.pool();
    insert_copy(&pool, &vtl, &destination, 1, "{}", "2026-10-01 10:00:00").await;
    sqlx::query("UPDATE storage_destinations SET identity=NULL")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(only_row(&pool, &vtl).await.bucket, None);
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn rows_are_isolated_by_vtl() {
    let (state, root, vtl, destination) = setup().await;
    let pool = state.catalog.pool();
    insert_copy(&pool, &vtl, &destination, 1, "{}", "2026-10-01 10:00:00").await;
    sqlx::query("INSERT INTO vtls(id,name) VALUES('other','Autre')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO storage_destinations(id,vtl_id,active) VALUES('other-destination','other',1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO cartridges(vtl_id,barcode,capacity_bytes,state) VALUES('other','TAPE01',1024,'writable')").execute(&pool).await.unwrap();
    insert_copy(
        &pool,
        "other",
        "other-destination",
        1,
        "{}",
        "2026-10-02 10:00:00",
    )
    .await;
    assert_eq!(only_row(&pool, &vtl).await.destination_id, destination);
    assert_eq!(
        only_row(&pool, "other").await.destination_id,
        "other-destination"
    );
    std::fs::remove_dir_all(root).ok();
}

fn sample(bucket_state: BucketState) -> HistoryRow {
    HistoryRow {
        barcode: "CVT001L8".into(),
        generation: 1,
        bytes: Some(42),
        exported_at: "2026-10-22 18:50:41".into(),
        destination_id: "d".into(),
        bucket: Some("vc-a".into()),
        removed_at: None,
        removal: None,
        bucket_state,
        archived_at: None,
        recalled_at: None,
        readable_until: None,
        local_deleted_at: None,
    }
}

#[test]
fn csv_has_bom_semicolons_headers_and_utc_dates() {
    let mut removed = sample(BucketState::Archived);
    removed.bucket = Some("odd;\"name".into());
    removed.removed_at = Some("2026-10-23 08:00:00".into());
    removed.removal = Some("s3_deleted");
    let mut catalogue = sample(BucketState::Available);
    catalogue.removed_at = Some("2026-10-24 08:00:00".into());
    catalogue.removal = Some("catalogue");
    catalogue.bytes = None;
    let text = csv(&[sample(BucketState::Archived), removed, catalogue]);
    let lines: Vec<&str> = text.split("\r\n").collect();
    assert_eq!(
        lines[0],
        "\u{feff}Bande;Génération;Bucket;Taille (octets);Exportée le;État du bucket;Archivé le;Rappelé le;Lisible jusqu’au;Retirée le;Cause du retrait;Supprimée en local le"
    );
    assert_eq!(
        lines[1],
        "CVT001L8;1;vc-a;42;2026-10-22T18:50:41Z;Archivé;;;;;;"
    );
    assert_eq!(
        lines[2],
        "CVT001L8;1;\"odd;\"\"name\";42;2026-10-22T18:50:41Z;Archivé;;;;2026-10-23T08:00:00Z;Supprimée de S3;"
    );
    assert_eq!(
        lines[3],
        "CVT001L8;1;vc-a;;2026-10-22T18:50:41Z;Disponible;;;;2026-10-24T08:00:00Z;Retirée du catalogue;"
    );
    assert_eq!(lines[4], "");
}

#[test]
fn csv_state_labels_match_the_interface() {
    for (state, label) in [
        (BucketState::Available, "Disponible"),
        (BucketState::Archiving, "Archivage en cours"),
        (BucketState::Archived, "Archivé"),
        (BucketState::Recalling, "Rappel en cours"),
        (BucketState::Recalled, "Rappelé"),
        (BucketState::Unknown, "À vérifier"),
    ] {
        assert!(
            csv(&[sample(state)]).contains(&format!(";{label};")),
            "{label}"
        );
    }
}

#[test]
fn csv_filename_is_ascii_and_dated() {
    assert_eq!(
        csv_filename("Bibliothèque Veeam", "2026-10-22"),
        "historique-biblioth-que-veeam-2026-10-22.csv"
    );
    assert_eq!(
        csv_filename("  ", "2026-10-22"),
        "historique-vtl-2026-10-22.csv"
    );
}
