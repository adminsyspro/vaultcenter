use super::*;

pub(crate) const A: &str = "00000000-0000-4000-8000-000000000021";

#[test]
fn idle_credit_is_bounded() {
    let mut b = Budget::new(A, "boot", 1, Some(1_000_000), 0);
    assert!(
        b.take(0, "boot", 1, Some(1_000_000), 65_536)
            .unwrap()
            .is_some()
    );
    assert!(
        b.take(100_000_000, "boot", 1, Some(1_000_000), 65_536)
            .unwrap()
            .is_none()
    );
    assert!(
        b.take(100_000_000, "boot", 1, Some(1_000_000), 34_464)
            .unwrap()
            .is_none()
    );
    assert!(
        b.take(100_000_000, "boot", 1, Some(1_000_000), 1)
            .unwrap()
            .is_some()
    );
    assert!(
        b.take(100_000_000_000, "boot", 1, Some(1_000_000), 65_536)
            .unwrap()
            .is_none()
    );
    assert!(
        b.take(100_000_000_000, "boot", 1, Some(1_000_000), 34_465)
            .unwrap()
            .is_some()
    );
}

#[test]
fn changed_limit_does_not_refill() {
    let mut b = Budget::new(A, "boot", 1, Some(1_000_000), 0);
    assert!(
        b.take(100_000_000, "boot", 2, Some(50_000_000), 65_536)
            .unwrap()
            .is_some()
    );
    assert!(
        b.take(101_310_720, "boot", 2, Some(50_000_000), 65_536)
            .unwrap()
            .is_none()
    );
    assert!(
        b.take(101_310_720, "boot", 2, Some(50_000_000), 1)
            .unwrap()
            .is_some()
    );
}

#[test]
fn new_boot_drops_old_clock() {
    let mut b = Budget::new(A, "old", 1, Some(1_000_000), 900_000_000_000);
    b.tokens = 100_000;
    assert!(b.take(0, "new", 1, Some(1_000_000), 1).unwrap().is_some());
    assert_eq!(b.tokens, 0);
    assert!(
        b.take(1_000, "new", 1, Some(1_000_000), 1)
            .unwrap()
            .is_none()
    );
}

#[test]
fn fractional_credit_and_high_rate_do_not_overflow() {
    let mut b = Budget::new(A, "boot", 1, Some(1_000_000), 0);
    for t in 1..1000 {
        assert!(b.take(t, "boot", 1, Some(1_000_000), 1).unwrap().is_some());
    }
    assert!(
        b.take(1000, "boot", 1, Some(1_000_000), 1)
            .unwrap()
            .is_none()
    );
    let mut high = Budget::new(A, "boot", 1, Some(100_000_000_000), 0);
    assert!(
        high.take(u64::MAX, "boot", 1, Some(100_000_000_000), 65_536)
            .unwrap()
            .is_none()
    );
    assert_eq!(high.tokens, 10_000_000_000 - 65_536);
}

pub(crate) struct Fixture(pub(crate) std::path::PathBuf);
impl Fixture {
    pub(crate) fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "vaultcenter-budget-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("vtls").join(A)).unwrap();
        let result = Self(root);
        result.config(1, Some(1_000_000));
        result
    }
    fn config(&self, revision: u64, rate: Option<u64>) {
        let file = self.0.join("vtls").join(A).join("bandwidth.json");
        std::fs::write(
            file,
            serde_json::to_vec(&serde_json::json!({"schema":1,"vtl_id":A,
            "revision":revision,"tape_write_bps":null,"s3_bps":rate}))
            .unwrap(),
        )
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn corrupt_ledger_fails_closed() {
    let f = Fixture::new();
    let limiter = VtlBandwidth::open(&f.0, A).unwrap();
    limiter.acquire(1).await.unwrap();
    std::fs::write(
        f.0.join("vtls").join(A).join("bandwidth-budget.json"),
        b"broken",
    )
    .unwrap();
    assert!(limiter.acquire(1).await.is_err());
}

#[tokio::test]
async fn cancel_wait_releases_lock() {
    let f = Fixture::new();
    let limiter = VtlBandwidth::open(&f.0, A).unwrap();
    let worker = limiter.clone();
    let task = tokio::spawn(async move {
        for _ in 0..100 {
            worker.acquire(65_536).await.unwrap();
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    task.abort();
    let _ = task.await;
    tokio::time::timeout(std::time::Duration::from_secs(1), limiter.acquire(65_536))
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn aggregate_processes_share_one_budget() {
    if let Ok(root) = std::env::var("VAULTCENTER_BUDGET_TEST_CHILD") {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let limiter = VtlBandwidth::open(Path::new(&root), A).unwrap();
        rt.block_on(async {
            for _ in 0..8 {
                limiter.acquire(65_536).await.unwrap();
            }
        });
        return;
    }
    let f = Fixture::new();
    let start = std::time::Instant::now();
    let mut children = (0..3)
        .map(|_| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "bandwidth::tests::aggregate_processes_share_one_budget",
                    "--nocapture",
                ])
                .env("VAULTCENTER_BUDGET_TEST_CHILD", &f.0)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }
    assert!(
        start.elapsed() >= std::time::Duration::from_millis(1500),
        "three processes must share 1 MB/s"
    );
}

#[test]
fn missing_rate_field_is_not_unlimited() {
    let f = Fixture::new();
    let path = f.0.join("vtls").join(A).join("bandwidth.json");
    std::fs::write(
        path,
        format!(r#"{{"schema":1,"vtl_id":"{A}","revision":1,"tape_write_bps":null}}"#),
    )
    .unwrap();
    assert!(VtlBandwidth::open(&f.0, A).is_err());
}

#[tokio::test]
async fn revision_rollback_is_an_error_not_an_endless_wait() {
    let f = Fixture::new();
    let limiter = VtlBandwidth::open(&f.0, A).unwrap();
    limiter.acquire(1).await.unwrap();
    let path = f.0.join("vtls").join(A).join("bandwidth-budget.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["revision"] = 2.into();
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    let result =
        tokio::time::timeout(std::time::Duration::from_millis(200), limiter.acquire(1)).await;
    assert!(
        result.is_ok(),
        "stale configuration must fail without looping"
    );
    assert!(result.unwrap().is_err());
}

#[test]
fn rate_change_without_new_revision_keeps_last_valid_rate() {
    let f = Fixture::new();
    let limiter = VtlBandwidth::open(&f.0, A).unwrap();
    f.config(1, None);
    limiter.cached.lock().unwrap().checked = Instant::now() - Duration::from_secs(2);
    assert_eq!(limiter.config().unwrap().s3_bps, Some(1_000_000));
}
