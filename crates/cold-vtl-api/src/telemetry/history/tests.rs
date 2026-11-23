use super::*;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

async fn setup() -> (Catalog, PathBuf, VtlId) {
    let root = std::env::temp_dir().join(format!("throughput-{}", uuid::Uuid::new_v4()));
    let catalog = Catalog::open(&root.join("catalog.db")).await.unwrap();
    let id = catalog.vtl_id().await.unwrap();
    (catalog, root, id)
}
fn snapshot(id: VtlId, at: i64) -> Value {
    json!({"version":1,"vtl_id":id.to_string(),"generated_at":at,"observed_at":at,"source":"mhvtl","status":"available","reason":null,
        "drives":[{"id":"11","label":"Reader","barcode":"SAME001L8","origin_slot":null,"state":"writing","observed_at":at,"bytes_written":null,"write_rate_bps":null}],"slots":[],
        "media":[{"barcode":"SAME001L8","observed_at":at,"local_size_bytes":10000,"local_growth_bps":125.0,"logical_used_bytes":null,"logical_capacity_bytes":null}]})
}
fn save(root: &Path, id: VtlId, value: &Value) {
    std::fs::write(root.join(format!("{id}.json")), value.to_string()).unwrap();
}
async fn count(catalog: &Catalog) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM telemetry_samples")
        .fetch_one(&catalog.pool())
        .await
        .unwrap()
}
fn selection(barcode: &str, drive: &str, window: u32, end: i64) -> HistoryQuery {
    HistoryQuery {
        barcode: Some(barcode.into()),
        drive_id: Some(drive.into()),
        window_seconds: Some(window),
        end_ms: Some(end),
        ..Default::default()
    }
}
async fn insert(
    catalog: &Catalog,
    id: VtlId,
    drive: &str,
    barcode: &str,
    at: i64,
    write: Option<f64>,
    growth: Option<f64>,
) {
    sqlx::query("INSERT INTO telemetry_samples(vtl_id,drive_id,barcode,at_ms,write_rate_bps,local_growth_bps) VALUES(?,?,?,?,?,?)")
        .bind(id.to_string()).bind(drive).bind(barcode).bind(at).bind(write).bind(growth).execute(&catalog.pool()).await.unwrap();
}
async fn cleanup(catalog: Catalog, root: PathBuf) {
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn retention_configuration_has_explicit_bounds() {
    assert_eq!(retention_days(None).unwrap(), 30);
    for valid in [1, 7, 30, 90] {
        assert_eq!(retention_days(Some(&valid.to_string())).unwrap(), valid);
    }
    for invalid in ["0", "91", "-1", "", "forever"] {
        assert!(retention_days(Some(invalid)).is_err(), "{invalid}");
    }
}

#[tokio::test]
async fn collector_persists_without_http_deduplicates_and_survives_reopen() {
    let (catalog, root, id) = setup().await;
    let now = 3_000_000_000;
    save(&root, id, &snapshot(id, now));
    assert_eq!(
        collect_once(&catalog, Some(&root), 30, now).await.unwrap(),
        1
    );
    assert_eq!(
        collect_once(&catalog, Some(&root), 30, now + 5000)
            .await
            .unwrap(),
        0,
        "Frozen cache must not gain new observations"
    );
    assert_eq!(count(&catalog).await, 1);
    let mut next = snapshot(id, now + 5000);
    next["drives"][0]["barcode"] = json!("NEXT001L8");
    next["media"][0]["barcode"] = json!("NEXT001L8");
    next["media"][0]["local_growth_bps"] = json!(250);
    save(&root, id, &next);
    assert_eq!(
        collect_once(&catalog, Some(&root), 30, now + 5000)
            .await
            .unwrap(),
        1
    );
    catalog.pool().close().await;
    let reopened = Catalog::open(&root.join("catalog.db")).await.unwrap();
    let history = read_history(
        &reopened,
        id,
        30,
        selection("SAME001L8", "11", 120, now + 5000),
        now + 5000,
    )
    .await
    .unwrap();
    assert_eq!(history.status, "available");
    let measured: Vec<_> = history
        .points
        .iter()
        .filter(|p| p.local_growth.is_some())
        .collect();
    assert_eq!(measured.len(), 1);
    assert_eq!(measured[0].local_growth, Some(125.0));
    assert!(history.points.iter().all(|p| p.write_rate.is_none()));
    cleanup(reopened, root).await;
}

#[tokio::test]
async fn background_monitor_collects_with_no_router_or_browser() {
    let (catalog, root, id) = setup().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    save(&root, id, &snapshot(id, now));
    let state = AppState {
        catalog: catalog.clone(),
        operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
        config: Arc::new(crate::AppConfig {
            storage_socket: None,
            media_root: root.join("media"),
            worker_path: root.join("worker"),
            restore_root: root.join("restore"),
            quiesce_check: None,
            maintenance_helper: None,
            cartridge_helper: None,
            reintegration_helper: None,
            archive_helper: None,
            telemetry_dir: Some(root.clone()),
            telemetry_retention_days: 30,
        }),
    };
    let task = monitor(state);
    let observed = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while count(&catalog).await == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    task.abort();
    let _ = task.await;
    assert!(
        observed.is_ok(),
        "Periodic collection must run without any HTTP request"
    );
    assert_eq!(count(&catalog).await, 1);
    cleanup(catalog, root).await;
}

#[tokio::test]
async fn collector_rejects_stale_ambiguous_demo_and_future_associations() {
    let (catalog, root, id) = setup().await;
    let now = 3_000_000_000;
    for case in [
        "demo",
        "stale_snapshot",
        "stale_drive",
        "unknown_drive",
        "ambiguous_barcode",
        "wrong_vtl",
        "future",
    ] {
        let mut value = snapshot(id, now);
        match case {
            "demo" => value["source"] = json!("demo"),
            "stale_snapshot" => value["generated_at"] = json!(now - 11000),
            "stale_drive" => value["drives"][0]["observed_at"] = json!(now - 16000),
            "unknown_drive" => value["drives"][0]["state"] = json!("unknown"),
            "ambiguous_barcode" => {
                let mut other = value["drives"][0].clone();
                other["id"] = json!("12");
                value["drives"].as_array_mut().unwrap().push(other);
            }
            "wrong_vtl" => value["vtl_id"] = json!(VtlId::new().to_string()),
            "future" => value["generated_at"] = json!(now + 1000),
            _ => unreachable!(),
        }
        save(&root, id, &value);
        assert_eq!(
            collect_once(&catalog, Some(&root), 30, now).await.unwrap(),
            0,
            "{case}"
        );
    }
    let mut unknown_metrics = snapshot(id, now);
    unknown_metrics["media"][0]["observed_at"] = json!(now - 11000);
    save(&root, id, &unknown_metrics);
    assert_eq!(
        collect_once(&catalog, Some(&root), 30, now).await.unwrap(),
        1
    );
    let pair: (Option<f64>, Option<f64>) =
        sqlx::query_as("SELECT write_rate_bps,local_growth_bps FROM telemetry_samples")
            .fetch_one(&catalog.pool())
            .await
            .unwrap();
    assert_eq!(pair, (None, None));
    cleanup(catalog, root).await;
}

#[tokio::test]
async fn identical_barcodes_and_drive_ids_never_share_vtl_samples() {
    let (catalog, root, first) = setup().await;
    let second = VtlId::new();
    sqlx::query("INSERT INTO vtls(id,name) VALUES(?,'Second')")
        .bind(second.to_string())
        .execute(&catalog.pool())
        .await
        .unwrap();
    let now = 3_000_000_000;
    for (id, growth) in [(first, 100), (second, 999)] {
        let mut value = snapshot(id, now);
        value["media"][0]["local_growth_bps"] = json!(growth);
        save(&root, id, &value);
    }
    assert_eq!(
        collect_once(&catalog, Some(&root), 30, now).await.unwrap(),
        2
    );
    for (id, expected) in [(first, 100.0), (second, 999.0)] {
        let history = read_history(
            &catalog,
            id,
            30,
            selection("SAME001L8", "11", 120, now),
            now,
        )
        .await
        .unwrap();
        assert_eq!(history.series.len(), 1);
        assert_eq!(history.points.last().unwrap().local_growth, Some(expected));
    }
    cleanup(catalog, root).await;
}

#[tokio::test]
async fn history_averages_bounded_buckets_keeps_gaps_and_never_blends_drives() {
    let (catalog, root, id) = setup().await;
    let end = 3_000_000_000;
    let start = end - 3_600_000;
    insert(
        &catalog,
        id,
        "11",
        "SAME001L8",
        start + 1000,
        Some(10.0),
        Some(30.0),
    )
    .await;
    insert(
        &catalog,
        id,
        "11",
        "SAME001L8",
        start + 4000,
        Some(20.0),
        None,
    )
    .await;
    insert(&catalog, id, "11", "SAME001L8", end, Some(40.0), Some(50.0)).await;
    insert(
        &catalog,
        id,
        "12",
        "SAME001L8",
        start + 1000,
        Some(999.0),
        Some(999.0),
    )
    .await;
    insert(
        &catalog,
        id,
        "11",
        "OLD001L8",
        start - 5000,
        Some(666.0),
        Some(666.0),
    )
    .await;
    let history = read_history(
        &catalog,
        id,
        30,
        selection("SAME001L8", "11", 3600, end),
        end,
    )
    .await
    .unwrap();
    assert_eq!(history.resolution_ms, 6000);
    assert_eq!(history.points.len(), 600);
    assert_eq!(history.points[0].at, start);
    assert_eq!(history.points[0].write_rate, Some(15.0));
    assert_eq!(history.points[0].local_growth, Some(30.0));
    assert_eq!(
        (history.points[1].write_rate, history.points[1].local_growth),
        (None, None)
    );
    assert_eq!(history.points.last().unwrap().write_rate, Some(40.0));
    assert_eq!(
        history.series.len(),
        2,
        "Inventory spans retention, not only selected barcode/window"
    );
    let mut auto = selection("SAME001L8", "11", 3600, end);
    auto.drive_id = None;
    let auto = read_history(&catalog, id, 30, auto, end).await.unwrap();
    assert_eq!(
        auto.points[0].write_rate,
        Some(15.0),
        "Choose most recent matching drive without mixing them"
    );
    let inventory = read_history(
        &catalog,
        id,
        30,
        HistoryQuery {
            end_ms: Some(end),
            ..Default::default()
        },
        end,
    )
    .await
    .unwrap();
    assert_eq!(inventory.series.len(), 3);
    assert!(inventory.points.is_empty());
    for window in [120, 300, 900, 3600, 21600, 86400, 604800, 2592000] {
        let history = read_history(
            &catalog,
            id,
            30,
            selection("MISSING", "11", window, end),
            end,
        )
        .await
        .unwrap();
        assert!(!history.points.is_empty() && history.points.len() <= 600);
        assert_eq!(history.status, "unavailable");
        assert!(
            history
                .points
                .iter()
                .all(|point| point.write_rate.is_none() && point.local_growth.is_none())
        );
    }
    let mut s3 = selection("SAME001L8", "11", 120, end);
    s3.kind = Some("s3".into());
    s3.transfer_id = Some("export-1".into());
    let absent = read_history(&catalog, id, 30, s3, end).await.unwrap();
    assert_eq!(absent.source, "s3");
    assert_eq!(absent.status, "unavailable");
    assert!(absent.series.is_empty());
    assert!(
        absent
            .points
            .iter()
            .all(|p| p.local_growth.is_none() && p.write_rate.is_none())
    );
    cleanup(catalog, root).await;
}

#[tokio::test]
async fn retention_purge_is_bounded_and_expired_rows_are_hidden_before_cleanup() {
    let (catalog, root, id) = setup().await;
    let now = 3 * DAY_MS;
    sqlx::query("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<10003) INSERT INTO telemetry_samples(vtl_id,drive_id,barcode,at_ms,local_growth_bps) SELECT ?,'11','OLD001L8',x,1 FROM n")
        .bind(id.to_string()).execute(&catalog.pool()).await.unwrap();
    insert(&catalog, id, "11", "FRESH01", now - 1000, None, Some(1.0)).await;
    let before = read_history(
        &catalog,
        id,
        1,
        selection("OLD001L8", "11", 604800, now),
        now,
    )
    .await
    .unwrap();
    assert_eq!(before.status, "unavailable");
    assert!(before.series.iter().all(|s| s.barcode != "OLD001L8"));
    collect_once(&catalog, None, 1, now).await.unwrap();
    assert_eq!(count(&catalog).await, 4);
    collect_once(&catalog, None, 1, now).await.unwrap();
    assert_eq!(count(&catalog).await, 1);
    cleanup(catalog, root).await;
}

#[tokio::test]
async fn s3_history_is_persistent_scoped_to_one_export_and_keeps_gaps() {
    let (catalog, root, id) = setup().await;
    let now = 3_000_000_000;
    let other = VtlId::new();
    sqlx::query("INSERT INTO vtls(id,name) VALUES(?,'Other')")
        .bind(other.to_string())
        .execute(&catalog.pool())
        .await
        .unwrap();
    for (vtl, job, barcode, rate) in [
        (id, "export-a", "SAME001L8", 100.0),
        (id, "export-b", "SAME001L8", 900.0),
        (other, "export-c", "SAME001L8", 999.0),
        (id, "export-d", "OTHER001", 800.0),
    ] {
        sqlx::query(
            "INSERT INTO jobs(id,vtl_id,kind,subject,status) VALUES(?,?,'export',?,'completed')",
        )
        .bind(job)
        .bind(vtl.to_string())
        .bind(barcode)
        .execute(&catalog.pool())
        .await
        .unwrap();
        sqlx::query("INSERT INTO s3_transfer_samples(vtl_id,transfer_id,barcode,at_ms,write_rate_bps) VALUES(?,?,?,?,?)").bind(vtl.to_string()).bind(job).bind(barcode).bind(now-10_000).bind(rate).execute(&catalog.pool()).await.unwrap();
    }
    sqlx::query("INSERT INTO s3_transfer_samples(vtl_id,transfer_id,barcode,at_ms,write_rate_bps,elapsed_ms) VALUES(?,'export-a','SAME001L8',?,300,1000)").bind(id.to_string()).bind(now-9000).execute(&catalog.pool()).await.unwrap();
    catalog.pool().close().await;
    let catalog = Catalog::open(&root.join("catalog.db")).await.unwrap();
    let query = || HistoryQuery {
        barcode: Some("SAME001L8".into()),
        kind: Some("s3".into()),
        transfer_id: Some("export-a".into()),
        window_seconds: Some(120),
        end_ms: Some(now),
        ..Default::default()
    };
    let history = read_history(&catalog, id, 30, query(), now).await.unwrap();
    assert_eq!(history.status, "available");
    let measured: Vec<_> = history.points.iter().filter_map(|p| p.write_rate).collect();
    assert_eq!(
        measured,
        vec![800.0 / 6.0],
        "partial final intervals must be weighted by their real duration"
    );
    assert!(history.points.iter().all(|p| p.local_growth.is_none()));
    assert!(history.points.last().unwrap().write_rate.is_none());
    let json = serde_json::to_value(&history).unwrap();
    assert!(
        json["series"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["transfer_id"] == "export-a")
    );
    let mut absent = query();
    absent.transfer_id = Some("export-c".into());
    assert_eq!(
        read_history(&catalog, id, 30, absent, now)
            .await
            .unwrap()
            .status,
        "unavailable"
    );
    let expired = read_history(&catalog, id, 1, query(), now + 2 * DAY_MS)
        .await
        .unwrap();
    assert_eq!(expired.status, "unavailable");
    collect_once(&catalog, None, 1, now + 2 * DAY_MS)
        .await
        .unwrap();
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM s3_transfer_samples")
        .fetch_one(&catalog.pool())
        .await
        .unwrap();
    assert_eq!(remaining, 0);
    cleanup(catalog, root).await;
}

#[tokio::test]
async fn download_history_never_mixes_uploads_or_other_libraries() {
    let (catalog, root, id) = setup().await;
    let now = 3_000_000_000;
    let other = VtlId::new();
    sqlx::query("INSERT INTO vtls(id,name) VALUES(?,'Other')")
        .bind(other.to_string())
        .execute(&catalog.pool())
        .await
        .unwrap();
    for (vtl, job, kind, rate) in [
        (id, "upload", "export", 100.0),
        (id, "download", "recovery", 200.0),
        (id, "legacy-download", "restore", 300.0),
        (other, "other-download", "recovery", 999.0),
    ] {
        sqlx::query(
            "INSERT INTO jobs(id,vtl_id,kind,subject,status) VALUES(?,?,?,'TAPE01','completed')",
        )
        .bind(job)
        .bind(vtl.to_string())
        .bind(kind)
        .execute(&catalog.pool())
        .await
        .unwrap();
        sqlx::query("INSERT INTO s3_transfer_samples(vtl_id,transfer_id,barcode,at_ms,write_rate_bps) VALUES(?,?,'TAPE01',?,?)").bind(vtl.to_string()).bind(job).bind(now-5000).bind(rate).execute(&catalog.pool()).await.unwrap();
    }
    let query = || HistoryQuery {
        kind: Some("s3_restore".into()),
        barcode: Some("TAPE01".into()),
        window_seconds: Some(120),
        end_ms: Some(now),
        ..Default::default()
    };
    let history = read_history(&catalog, id, 30, query(), now).await.unwrap();
    assert_eq!(history.source, "s3_restore");
    assert_eq!(history.series.len(), 2);
    for (job, rate) in [
        ("download", Some(200.0)),
        ("legacy-download", Some(300.0)),
        ("upload", None),
        ("other-download", None),
    ] {
        let mut q = query();
        q.transfer_id = Some(job.into());
        let result = read_history(&catalog, id, 30, q, now).await.unwrap();
        assert_eq!(result.points.iter().find_map(|p| p.write_rate), rate);
    }
    let mut q = query();
    q.kind = Some("s3".into());
    let result = read_history(&catalog, id, 30, q, now).await.unwrap();
    assert_eq!(result.series.len(), 1);
    assert_eq!(result.series[0].transfer_id.as_deref(), Some("upload"));
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

fn aggregate(kind: &str, drive: Option<&str>, end: i64) -> HistoryQuery {
    HistoryQuery {
        kind: Some(kind.into()),
        drive_id: drive.map(Into::into),
        window_seconds: Some(120),
        end_ms: Some(end),
        aggregate: Some(true),
        ..Default::default()
    }
}

#[tokio::test]
async fn aggregate_history_sums_tapes_per_bucket_keeps_gaps_and_scope() {
    let (catalog, root, id) = setup().await;
    let other = VtlId::new();
    sqlx::query("INSERT INTO vtls(id,name) VALUES(?,'Other')")
        .bind(other.to_string())
        .execute(&catalog.pool())
        .await
        .unwrap();
    let end = 3_000_000_000;
    let start = end - 120_000;
    for (vtl, drive, barcode, at, write, growth) in [
        (id, "11", "TAPEA", start + 1000, 100.0, Some(10.0)),
        (id, "11", "TAPEA", start + 2000, 300.0, Some(10.0)),
        (id, "12", "TAPEB", start + 1000, 50.0, Some(5.0)),
        (id, "11", "TAPEA", start + 5500, 100.0, None),
        (id, "11", "TAPEC", start + 8000, 40.0, None),
        (other, "11", "TAPEA", start + 1000, 1000.0, Some(1000.0)),
    ] {
        insert(&catalog, vtl, drive, barcode, at, Some(write), growth).await;
    }
    let history = read_history(&catalog, id, 30, aggregate("pbs", None, end), end)
        .await
        .unwrap();
    assert_eq!(history.status, "available");
    assert_eq!(history.points.len(), 24);
    assert_eq!(history.points[0].write_rate, Some(250.0));
    assert_eq!(history.points[0].local_growth, Some(15.0));
    assert_eq!(history.points[1].write_rate, Some(140.0));
    assert_eq!(history.points[1].local_growth, None);
    assert_eq!(history.points[2].write_rate, None);
    assert_eq!(history.series.len(), 3, "the inventory stays the usual one");
    let one = read_history(&catalog, id, 30, aggregate("pbs", Some("12"), end), end)
        .await
        .unwrap();
    assert_eq!(one.points[0].write_rate, Some(50.0));
    assert_eq!(one.points[1].write_rate, None);
    let empty = read_history(&catalog, VtlId::new(), 30, aggregate("pbs", None, end), end)
        .await
        .unwrap();
    assert_eq!(empty.status, "unavailable");
    assert!(empty.points.iter().all(|p| p.write_rate.is_none()));
    cleanup(catalog, root).await;
}

#[tokio::test]
async fn aggregate_s3_history_sums_concurrent_transfers_of_one_kind() {
    let (catalog, root, id) = setup().await;
    let end = 3_000_000_000;
    for (job, kind, barcode, rate) in [
        ("export-a", "export", "TAPEA", 10.0),
        ("export-b", "export", "TAPEB", 20.0),
        ("restore-a", "restore", "TAPEC", 99.0),
    ] {
        sqlx::query("INSERT INTO jobs(id,vtl_id,kind,subject,status) VALUES(?,?,?,?,'completed')")
            .bind(job)
            .bind(id.to_string())
            .bind(kind)
            .bind(barcode)
            .execute(&catalog.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO s3_transfer_samples(vtl_id,transfer_id,barcode,at_ms,write_rate_bps) VALUES(?,?,?,?,?)").bind(id.to_string()).bind(job).bind(barcode).bind(end-10_000).bind(rate).execute(&catalog.pool()).await.unwrap();
    }
    for (kind, expected) in [("s3", 30.0), ("s3_restore", 99.0)] {
        let history = read_history(&catalog, id, 30, aggregate(kind, None, end), end)
            .await
            .unwrap();
        assert_eq!(history.status, "available", "{kind}");
        let measured: Vec<_> = history.points.iter().filter_map(|p| p.write_rate).collect();
        assert_eq!(measured, vec![expected], "{kind}");
        assert!(history.points.last().unwrap().write_rate.is_none());
    }
    cleanup(catalog, root).await;
}

#[tokio::test]
async fn aggregate_history_refuses_a_barcode_or_transfer() {
    let (catalog, root, id) = setup().await;
    let end = 3_000_000_000;
    let mut with_barcode = aggregate("pbs", None, end);
    with_barcode.barcode = Some("TAPEA".into());
    let mut with_transfer = aggregate("s3", None, end);
    with_transfer.transfer_id = Some("x".into());
    for query in [with_barcode, with_transfer] {
        let error = read_history(&catalog, id, 30, query, end)
            .await
            .unwrap_err();
        assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);
    }
    cleanup(catalog, root).await;
}
