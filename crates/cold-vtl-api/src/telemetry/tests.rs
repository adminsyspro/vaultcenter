use super::*;
use serde_json::{Value, json};
fn fixture(id: VtlId) -> Value {
    let mut value: Value = serde_json::from_str(include_str!(
        "../../../../deploy/telemetry/snapshot-v1.example.json"
    ))
    .unwrap();
    value["vtl_id"] = json!(id.to_string());
    value["generated_at"] = json!(100_000);
    value["observed_at"] = json!(100_000);
    value["status"] = json!("available");
    value["reason"] = Value::Null;
    value["drives"][0]["state"] = json!("writing");
    value["drives"][0]["observed_at"] = json!(100_000);
    value
}
#[tokio::test]
async fn telemetry_cache_bounds_identity_freshness_and_allowlist() {
    let id = VtlId::new();
    let root = std::env::temp_dir().join(format!("telemetry-{}", id));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!("{id}.json"));
    assert_eq!(
        read_snapshot(None, id, 100_000).await.reason.as_deref(),
        Some("not_configured")
    );
    assert_eq!(
        read_snapshot(Some(&root), id, 100_000)
            .await
            .reason
            .as_deref(),
        Some("source_missing")
    );
    let mut base = fixture(id);
    base["drives"][0]["barcode"] = json!("SAME001L8");
    let second = VtlId::new();
    let mut other = fixture(second);
    other["drives"][0]["barcode"] = json!("SAME001L8");
    other["drives"][0]["state"] = json!("reading");
    std::fs::write(root.join(format!("{second}.json")), other.to_string()).unwrap();
    assert_eq!(
        read_snapshot(Some(&root), second, 100_000).await.drives[0].state,
        "reading"
    );
    std::fs::write(&path, base.to_string()).unwrap();
    assert_eq!(
        read_snapshot(Some(&root), id, 100_000).await.drives[0].state,
        "writing"
    );
    let stale = read_snapshot(Some(&root), id, 120_000).await;
    assert_eq!(stale.status, "stale");
    assert_eq!(stale.drives[0].state, "unknown");
    for (key, value) in [
        ("version", json!(2)),
        ("vtl_id", json!(VtlId::new().to_string())),
        ("generated_at", json!(106_000)),
        ("source", json!("secret")),
        ("drives", json!([{}])),
    ] {
        let mut invalid = base.clone();
        invalid[key] = value;
        std::fs::write(&path, invalid.to_string()).unwrap();
        let result = read_snapshot(Some(&root), id, 100_000).await;
        assert_eq!(result.reason.as_deref(), Some("invalid_snapshot"), "{key}");
        assert!(result.drives.is_empty());
    }
    let mut invalid = base.clone();
    invalid["drives"][0]["write_rate_bps"] = json!(-1);
    std::fs::write(&path, invalid.to_string()).unwrap();
    assert_eq!(
        read_snapshot(Some(&root), id, 100_000).await.status,
        "unavailable"
    );
    invalid = base.clone();
    invalid["drives"] = json!(vec![base["drives"][0].clone(); 65]);
    std::fs::write(&path, invalid.to_string()).unwrap();
    assert_eq!(
        read_snapshot(Some(&root), id, 100_000).await.status,
        "unavailable"
    );
    for contents in ["{".to_owned(), " ".repeat(1_048_577)] {
        std::fs::write(&path, contents).unwrap();
        assert_eq!(
            read_snapshot(Some(&root), id, 100_000).await.status,
            "unavailable"
        );
    }
    std::fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", &path).unwrap();
    assert_eq!(
        read_snapshot(Some(&root), id, 100_000).await.status,
        "unavailable"
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(
        read_snapshot(Some(&root), id, 100_000).await.status,
        "unavailable"
    );
    std::fs::remove_dir_all(root).unwrap();
}
