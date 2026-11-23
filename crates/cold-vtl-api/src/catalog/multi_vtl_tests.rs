use super::*;

async fn fixture() -> (Catalog, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("cold-vtl-multi-{}", uuid::Uuid::new_v4()));
    let catalog = Catalog::open(&root.join("catalog.db")).await.unwrap();
    (catalog, root)
}

#[tokio::test]
async fn multi_vtl_migration_creates_one_legacy_library() {
    let (catalog, root) = fixture().await;
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM vtls")
        .fetch_all(&catalog.pool())
        .await
        .unwrap();
    assert_eq!(ids, vec!["00000000-0000-4000-8000-000000000001"]);
    let violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&catalog.pool())
        .await
        .unwrap();
    assert!(violations.is_empty());
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn multi_vtl_duplicate_barcode_is_scoped_to_its_library() {
    let (catalog, root) = fixture().await;
    sqlx::query(
        "INSERT INTO vtls(id,name) VALUES('00000000-0000-4000-8000-000000000002','Second library')",
    )
    .execute(&catalog.pool())
    .await
    .unwrap();
    for vtl in [
        "00000000-0000-4000-8000-000000000001",
        "00000000-0000-4000-8000-000000000002",
    ] {
        sqlx::query("INSERT INTO cartridges(vtl_id,barcode,capacity_bytes,state) VALUES(?,'TAPE01',100,'writable')")
            .bind(vtl).execute(&catalog.pool()).await.unwrap();
    }
    let duplicate = sqlx::query("INSERT INTO cartridges(vtl_id,barcode,capacity_bytes,state) VALUES('00000000-0000-4000-8000-000000000002','TAPE01',100,'writable')")
        .execute(&catalog.pool()).await;
    assert!(duplicate.is_err());
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM cartridges")
        .fetch_all(&catalog.pool())
        .await
        .unwrap();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    for id in ids {
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn multi_vtl_all_pool_connections_enforce_foreign_keys() {
    let (catalog, root) = fixture().await;
    let pool = catalog.pool();
    let mut connections = Vec::new();
    for _ in 0..5 {
        connections.push(pool.acquire().await.unwrap());
    }
    for connection in &mut connections {
        let result = sqlx::query("INSERT INTO cartridges(vtl_id,barcode,capacity_bytes,state) VALUES('missing','TAPE01',100,'writable')")
            .execute(&mut **connection).await;
        assert!(
            result
                .unwrap_err()
                .as_database_error()
                .unwrap()
                .is_foreign_key_violation(),
            "unknown VTL must fail specifically on its foreign key"
        );
    }
    drop(connections);
    pool.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn multi_vtl_resolution_never_guesses_between_libraries() {
    let (catalog, root) = fixture().await;
    let legacy = catalog.resolve_vtl(None).await.unwrap();
    let second = cold_vtl_core::VtlId::new();
    sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'Second','ready')")
        .bind(second.to_string())
        .execute(&catalog.pool())
        .await
        .unwrap();
    assert!(matches!(
        catalog.resolve_vtl(None).await,
        Err(CatalogError::AmbiguousVtl)
    ));
    assert_eq!(catalog.resolve_vtl(Some(legacy)).await.unwrap(), legacy);
    assert_eq!(catalog.resolve_vtl(Some(second)).await.unwrap(), second);
    assert!(matches!(
        catalog.resolve_vtl(Some(cold_vtl_core::VtlId::new())).await,
        Err(CatalogError::MissingVtl)
    ));
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

async fn legacy_fixture() -> (SqlitePool, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("cold-vtl-legacy-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(root.join("catalog.db"))
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let mut migrations = sqlx::migrate!("./migrations");
    migrations.migrations = migrations
        .iter()
        .filter(|m| m.version < 8)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    migrations.run(&pool).await.unwrap();
    sqlx::raw_sql("INSERT INTO cartridges(barcode,capacity_bytes,bytes_written,generation,state,created_at) VALUES('TAPE01',100,42,3,'verified','2026-01-01');
        INSERT INTO remote_generations(barcode,generation,metadata) VALUES('TAPE01',3,'{\"object_key\":\"original/key\"}');
        INSERT INTO jobs(id,kind,subject,status,destination) VALUES('restore','restore','TAPE01','completed','/restore/original'),('prepare','archive-prepare','bucket','completed',NULL),('archive','archive','bucket','completed',NULL);
        INSERT INTO archive_requests(job_id,prepare_job_id,phase,request) VALUES('archive','prepare','accepted','{\"reference\":\"original\"}');
        INSERT INTO maintenance_events(actor,action,success,message) VALUES('admin','enter',1,'original');
        INSERT INTO s3_settings(id,revision,configuration,actor) VALUES(1,7,'{\"bucket\":\"original\"}','admin');
        INSERT INTO s3_reconcile_required(barcode) VALUES('TAPE01');")
        .execute(&pool).await.unwrap();
    (pool, root)
}

#[tokio::test]
async fn multi_vtl_migration_preserves_existing_data_and_ids_after_reopen() {
    let (pool, root) = legacy_fixture().await;
    pool.close().await;
    let catalog = Catalog::open(&root.join("catalog.db")).await.unwrap();
    let pool = catalog.pool();
    let id: String = sqlx::query_scalar("SELECT id FROM cartridges WHERE barcode='TAPE01'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let cartridge = catalog
        .cartridge(&Barcode::parse("TAPE01").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            cartridge.bytes_written,
            cartridge.generation,
            cartridge.state
        ),
        (42, 3, CartridgeState::Verified)
    );
    for (query, expected) in [
        ("SELECT created_at FROM cartridges", "2026-01-01"),
        (
            "SELECT metadata FROM remote_generations",
            "{\"object_key\":\"original/key\"}",
        ),
        (
            "SELECT destination FROM jobs WHERE id='restore'",
            "/restore/original",
        ),
        (
            "SELECT request FROM archive_requests",
            "{\"reference\":\"original\"}",
        ),
        ("SELECT message FROM maintenance_events", "original"),
        (
            "SELECT configuration FROM storage_destination_revisions WHERE revision=7",
            "{\"bucket\":\"original\"}",
        ),
        ("SELECT barcode FROM s3_reconcile_required", "TAPE01"),
    ] {
        assert_eq!(
            sqlx::query_scalar::<_, String>(query)
                .fetch_one(&pool)
                .await
                .unwrap(),
            expected
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT cartridge_id FROM jobs WHERE id='restore'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        id
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap()
            .is_empty()
    );
    pool.close().await;
    let reopened = Catalog::open(&root.join("catalog.db")).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT id FROM cartridges")
            .fetch_one(&reopened.pool())
            .await
            .unwrap(),
        id
    );
    reopened.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn multi_vtl_migration_failure_rolls_back_all_changes() {
    let (pool, root) = legacy_fixture().await;
    // Force a failure at the final rename, after cartridge, copy and job table rebuilds.
    sqlx::query("CREATE INDEX maintenance_events_legacy ON maintenance_events(actor)")
        .execute(&pool)
        .await
        .unwrap();
    assert!(sqlx::migrate!("./migrations").run(&pool).await.is_err());
    let names: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        !names
            .iter()
            .any(|n| n == "vtls" || n == "storage_destinations" || n.ends_with("_legacy"))
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM cartridges WHERE barcode='TAPE01' AND bytes_written=42"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap(),
        7
    );
    sqlx::query("DROP INDEX maintenance_events_legacy")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn multi_vtl_catalog_scopes_reads_updates_and_job_ownership() {
    let (catalog, root) = fixture().await;
    let first = catalog.resolve_vtl(None).await.unwrap();
    let second = cold_vtl_core::VtlId::new();
    sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'Second','ready')")
        .bind(second.to_string())
        .execute(&catalog.pool())
        .await
        .unwrap();
    let a = catalog.for_vtl(first).await.unwrap();
    let b = catalog.for_vtl(second).await.unwrap();
    sqlx::query(
        "INSERT INTO storage_destinations(id,vtl_id,active) VALUES('second-destination',?,1)",
    )
    .bind(second.to_string())
    .execute(&catalog.pool())
    .await
    .unwrap();
    let mut cartridge = Cartridge {
        media_type: None,
        segment_size_mib: 256,
        multipart_part_size_mib: 256,
        barcode: Barcode::parse("TAPE01").unwrap(),
        capacity_bytes: 100,
        bytes_written: 4,
        generation: 1,
        state: CartridgeState::Sealed,
    };
    a.create_cartridge(&cartridge).await.unwrap();
    b.create_cartridge(&cartridge).await.unwrap();
    assert!(matches!(
        catalog.list_cartridges().await,
        Err(CatalogError::AmbiguousVtl)
    ));
    cartridge.bytes_written = 10;
    a.update_cartridge(&cartridge).await.unwrap();
    assert_eq!(
        b.cartridge(&cartridge.barcode)
            .await
            .unwrap()
            .unwrap()
            .bytes_written,
        4
    );
    let job_a = a
        .reserve_job("a", "export", &cartridge, Some(1), None)
        .await
        .unwrap();
    assert!(b.job(&job_a.id).await.unwrap().is_none());
    assert!(matches!(
        b.update_job("a", "running", 10, None).await,
        Err(CatalogError::MissingJob)
    ));
    assert!(!b.has_active_job(&cartridge.barcode).await.unwrap());
    assert!(matches!(
        b.finish_job("a", "completed", "wrong library", None, None)
            .await,
        Err(CatalogError::MissingJob)
    ));
    assert_eq!(a.job("a").await.unwrap().unwrap().status, "queued");
    assert!(
        b.reserve_job("b", "export", &cartridge, Some(1), None)
            .await
            .is_ok()
    );
    assert_eq!(a.list_jobs().await.unwrap().len(), 1);
    assert_eq!(b.list_jobs().await.unwrap().len(), 1);
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn multi_vtl_copy_and_job_reject_mixed_library_identities() {
    let (catalog, root) = fixture().await;
    let first = catalog.resolve_vtl(None).await.unwrap();
    let second = cold_vtl_core::VtlId::new();
    sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'Second','ready')")
        .bind(second.to_string())
        .execute(&catalog.pool())
        .await
        .unwrap();
    for vtl in [first, second] {
        catalog
            .for_vtl(vtl)
            .await
            .unwrap()
            .create_cartridge(&Cartridge::new(Barcode::parse("TAPE01").unwrap(), 100).unwrap())
            .await
            .unwrap();
    }
    let other: String = sqlx::query_scalar("SELECT id FROM cartridges WHERE vtl_id=?")
        .bind(second.to_string())
        .fetch_one(&catalog.pool())
        .await
        .unwrap();
    for query in [
        "INSERT INTO remote_generations(vtl_id,cartridge_id,barcode,generation,metadata) VALUES(?,?,'TAPE01',1,'{}')",
        "INSERT INTO jobs(vtl_id,cartridge_id,id,kind,subject,status) VALUES(?,?,'bad','export','TAPE01','queued')",
    ] {
        let error = sqlx::query(query)
            .bind(first.to_string())
            .bind(&other)
            .execute(&catalog.pool())
            .await
            .unwrap_err();
        assert!(
            error
                .as_database_error()
                .unwrap()
                .is_foreign_key_violation(),
            "{error}"
        );
    }
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn multi_vtl_legacy_roots_are_initialized_once_without_reassociation() {
    let (catalog, root) = fixture().await;
    let media = root.join("media");
    let restore = root.join("restore");
    let (a, b) = tokio::join!(
        catalog.initialize_legacy(&media, &restore),
        catalog.initialize_legacy(&media, &restore)
    );
    a.unwrap();
    b.unwrap();
    assert!(
        catalog
            .initialize_legacy(&root.join("other"), &restore)
            .await
            .is_err()
    );
    let paths: (String, String) = sqlx::query_as("SELECT media_root,restore_root FROM vtls")
        .fetch_one(&catalog.pool())
        .await
        .unwrap();
    assert_eq!(
        paths,
        (
            media.to_str().unwrap().into(),
            restore.to_str().unwrap().into()
        )
    );
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn multi_vtl_archive_cannot_reference_another_library_job() {
    let (catalog, root) = fixture().await;
    let second = cold_vtl_core::VtlId::new();
    sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'Second','ready')")
        .bind(second.to_string())
        .execute(&catalog.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,subject,status) VALUES('submit-a','archive','bucket','completed')").execute(&catalog.pool()).await.unwrap();
    sqlx::query("INSERT INTO jobs(vtl_id,id,kind,subject,status) VALUES(?,'prepare-b','archive-prepare','bucket','completed')").bind(second.to_string()).execute(&catalog.pool()).await.unwrap();
    let error=sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request) VALUES('submit-a','prepare-b','accepted','{}')").execute(&catalog.pool()).await.unwrap_err();
    assert!(
        error
            .as_database_error()
            .unwrap()
            .is_foreign_key_violation()
    );
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn archive_pin_migration_recovers_only_exact_proven_preparation_revisions() {
    use serde_json::json;
    let root = std::env::temp_dir().join(format!("vtl-pin-migration-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(root.join("catalog.db"))
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await
        .unwrap();
    let mut migrations = sqlx::migrate!("./migrations");
    migrations.migrations = migrations
        .iter()
        .filter(|m| m.version < 10)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    migrations.run(&pool).await.unwrap();
    let vtl = "00000000-0000-4000-8000-000000000001";
    let dest = "00000000-0000-4000-8000-000000000002";
    sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES('foreign-vtl','Foreign','ready')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO storage_destinations(id,vtl_id,active) VALUES('foreign-dest','foreign-vtl',1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    for (destination, revision, bucket) in [
        (dest, 3, "original-bucket"),
        (dest, 9, "newer-bucket"),
        (dest, 4, "wrong-bucket"),
        ("foreign-dest", 5, "original-bucket"),
    ] {
        sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,?,?)").bind(destination).bind(revision).bind(json!({"bucket":bucket}).to_string()).execute(&pool).await.unwrap();
    }
    for (name, revision, receipt_bucket, request) in [
        (
            "accepted",
            json!(3),
            "original-bucket",
            json!({"confirm_bucket":"original-bucket"}).to_string(),
        ),
        (
            "uncertain",
            json!(3),
            "original-bucket",
            json!({"confirm_bucket":"original-bucket"}).to_string(),
        ),
        (
            "partial",
            json!(3),
            "original-bucket",
            json!({"confirm_bucket":"original-bucket"}).to_string(),
        ),
        (
            "recall",
            json!(3),
            "original-bucket",
            json!({"confirm_bucket":"original-bucket"}).to_string(),
        ),
        (
            "wrong-config-bucket",
            json!(4),
            "original-bucket",
            json!({"confirm_bucket":"original-bucket"}).to_string(),
        ),
        (
            "missing-revision",
            json!(8),
            "original-bucket",
            json!({"confirm_bucket":"original-bucket"}).to_string(),
        ),
        (
            "foreign-revision",
            json!(5),
            "original-bucket",
            json!({"confirm_bucket":"original-bucket"}).to_string(),
        ),
        (
            "wrong-receipt-bucket",
            json!(3),
            "another-bucket",
            json!({"confirm_bucket":"original-bucket"}).to_string(),
        ),
        (
            "string-revision",
            json!("3"),
            "original-bucket",
            json!({"confirm_bucket":"original-bucket"}).to_string(),
        ),
        (
            "invalid-request",
            json!(3),
            "original-bucket",
            "invalid-json".into(),
        ),
        (
            "conflicting-pin",
            json!(3),
            "original-bucket",
            json!({"confirm_bucket":"original-bucket"}).to_string(),
        ),
    ] {
        let prepare = format!("{name}-prepare");
        let receipt = json!({"config_revision":revision,"scope":{"bucket":receipt_bucket}});
        sqlx::query("INSERT INTO jobs(id,kind,subject,status,message) VALUES(?,'archive_prepare',?,'completed',?)").bind(&prepare).bind(name).bind(receipt.to_string()).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO jobs(id,kind,subject,status) VALUES(?,'archive_submit',?,'completed')",
        )
        .bind(name)
        .bind(name)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,vtl_id,destination_id) VALUES(?,?,?,?,?,?)").bind(name).bind(&prepare).bind(if name=="uncertain" {"uncertain"} else {"accepted"}).bind(request).bind(vtl).bind(dest).execute(&pool).await.unwrap();
        if name == "recall" {
            sqlx::query(
                "UPDATE jobs SET kind=replace(kind,'archive_','recall_') WHERE id IN (?,?)",
            )
            .bind(name)
            .bind(&prepare)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("UPDATE archive_requests SET operation='recall' WHERE job_id=?")
                .bind(name)
                .execute(&pool)
                .await
                .unwrap();
        }
        if name == "partial" {
            sqlx::query("UPDATE jobs SET destination_id=? WHERE id=?")
                .bind(dest)
                .bind(name)
                .execute(&pool)
                .await
                .unwrap();
        }
        if name == "conflicting-pin" {
            sqlx::query("UPDATE jobs SET destination_id=?,configuration_revision=9 WHERE id=?")
                .bind(dest)
                .bind(name)
                .execute(&pool)
                .await
                .unwrap();
        }
    }
    sqlx::query("INSERT INTO jobs(id,kind,subject,status) VALUES('unproven-export','export','TAPE01','completed')").execute(&pool).await.unwrap();
    pool.close().await;
    let catalog = Catalog::open(&root.join("catalog.db"))
        .await
        .unwrap()
        .for_vtl(vtl.parse().unwrap())
        .await
        .unwrap();
    for name in ["accepted", "uncertain", "partial", "recall"] {
        for job in [name.to_owned(), format!("{name}-prepare")] {
            let job = catalog.job(&job).await.unwrap().unwrap();
            assert_eq!(job.destination_id.as_deref(), Some(dest));
            assert_eq!(job.configuration_revision, Some(3));
        }
    }
    for name in [
        "wrong-config-bucket",
        "missing-revision",
        "foreign-revision",
        "wrong-receipt-bucket",
        "string-revision",
        "invalid-request",
    ] {
        for job in [name.to_owned(), format!("{name}-prepare")] {
            let job = catalog.job(&job).await.unwrap().unwrap();
            assert!(job.destination_id.is_none(), "{name}");
            assert!(job.configuration_revision.is_none(), "{name}");
        }
    }
    assert_eq!(
        catalog
            .job("conflicting-pin")
            .await
            .unwrap()
            .unwrap()
            .configuration_revision,
        Some(9)
    );
    assert!(
        catalog
            .job("conflicting-pin-prepare")
            .await
            .unwrap()
            .unwrap()
            .destination_id
            .is_none()
    );
    assert!(
        catalog
            .job("unproven-export")
            .await
            .unwrap()
            .unwrap()
            .destination_id
            .is_none()
    );
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&catalog.pool())
            .await
            .unwrap()
            .is_empty()
    );
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}
