use super::*;

#[tokio::test]
async fn recorder_persists_only_acknowledged_bytes_and_enforces_job_scope() {
    let path =
        std::env::temp_dir().join(format!("s3-history-{}-{}.db", std::process::id(), now_ms()));
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE TABLE vtls(id TEXT PRIMARY KEY); CREATE TABLE jobs(id TEXT PRIMARY KEY,vtl_id TEXT,subject TEXT,kind TEXT,status TEXT,generation INTEGER,progress INTEGER DEFAULT 10,message TEXT,updated_at TEXT,UNIQUE(vtl_id,id)); INSERT INTO vtls VALUES('vtl-a'); INSERT INTO jobs(id,vtl_id,subject,kind,status,generation) VALUES('job-a','vtl-a','TAPE01','export','running',1);").execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../cold-vtl-api/migrations/0016_s3_transfer_history.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        Recorder::open(&path, "vtl-b", "job-a", "TAPE01", 1)
            .await
            .is_err()
    );
    assert!(
        Recorder::open(&path, "vtl-a", "job-a", "OTHER", 1)
            .await
            .is_err()
    );
    assert!(
        Recorder::open(&path, "vtl-a", "job-a", "TAPE01", 2)
            .await
            .is_err()
    );
    let recorder = Recorder::open(&path, "vtl-a", "job-a", "TAPE01", 1)
        .await
        .unwrap();
    recorder.set_total(4096);
    recorder.phase("uploading");
    recorder.set_active(true);
    recorder.acknowledge(2048);
    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            let percent: i64 = sqlx::query_scalar("SELECT progress FROM jobs WHERE id='job-a'")
                .fetch_one(&pool)
                .await
                .unwrap();
            if percent == 50 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("progress must update while the export is still running");
    recorder.set_active(false);
    recorder.close().await;
    let message: Option<String> = sqlx::query_scalar("SELECT message FROM jobs WHERE id='job-a'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        message.is_some(),
        "worker must publish measured export progress, not leave the fixed API milestone"
    );
    let progress: serde_json::Value = serde_json::from_str(message.as_ref().unwrap()).unwrap();
    assert_eq!(
        progress["export_progress"],
        serde_json::json!({"phase":"uploading","completed_bytes":2048,"total_bytes":4096})
    );
    let percent: i64 = sqlx::query_scalar("SELECT progress FROM jobs WHERE id='job-a'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(percent, 50);
    let samples: Vec<Option<f64>> =
        sqlx::query_scalar("SELECT write_rate_bps FROM s3_transfer_samples ORDER BY at_ms")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(
        samples.first().unwrap().is_none(),
        "start boundary must be unknown"
    );
    assert!(samples.iter().flatten().any(|rate| *rate > 0.0));
    assert!(
        samples.last().unwrap().is_none(),
        "finished upload must end its line"
    );
    sqlx::query("DELETE FROM s3_transfer_samples")
        .execute(&pool)
        .await
        .unwrap();
    // A recovered object or old multipart receipt emits no newly transferred bytes.
    let recorder = Recorder::open(&path, "vtl-a", "job-a", "TAPE01", 1)
        .await
        .unwrap();
    recorder.set_total(4096);
    recorder.reuse(4096);
    recorder.phase("verifying");
    recorder.close().await;
    let (percent, status, message): (i64, String, String) =
        sqlx::query_as("SELECT progress,status,message FROM jobs WHERE id='job-a'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (percent, status.as_str()),
        (99, "running"),
        "uploaded bytes never complete verification"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&message).unwrap()["export_progress"]["completed_bytes"],
        4096
    );
    let measured: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM s3_transfer_samples WHERE write_rate_bps IS NOT NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(measured, 0);
    let recorder = Recorder::open(&path, "vtl-a", "job-a", "TAPE01", 1)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET status='failed',message='actual error' WHERE id='job-a'")
        .execute(&pool)
        .await
        .unwrap();
    recorder.close().await;
    let message: String = sqlx::query_scalar("SELECT message FROM jobs WHERE id='job-a'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        message, "actual error",
        "late progress must not overwrite a terminal result"
    );
    pool.close().await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn downloads_record_bytes_without_overwriting_terminal_jobs_or_export_progress() {
    let path = std::env::temp_dir().join(format!(
        "download-history-{}-{}.db",
        std::process::id(),
        now_ms()
    ));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::raw_sql("CREATE TABLE vtls(id TEXT PRIMARY KEY); CREATE TABLE jobs(id TEXT PRIMARY KEY,vtl_id TEXT,subject TEXT,kind TEXT,status TEXT,generation INTEGER,progress INTEGER DEFAULT 10,message TEXT,updated_at TEXT,UNIQUE(vtl_id,id)); INSERT INTO vtls VALUES('vtl-a'); INSERT INTO jobs(id,vtl_id,subject,kind,status,generation) VALUES('restore-a','vtl-a','TAPE01','recovery','running',1);").execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../../../cold-vtl-api/migrations/0016_s3_transfer_history.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        Recorder::open(&path, "vtl-a", "restore-a", "TAPE01", 1)
            .await
            .is_err()
    );
    assert!(
        Recorder::open_direction(&path, "vtl-b", "restore-a", "TAPE01", 1, true)
            .await
            .is_err()
    );
    let recorder = Recorder::open_direction(&path, "vtl-a", "restore-a", "TAPE01", 1, true)
        .await
        .unwrap();
    recorder.set_total(100);
    recorder.phase("downloading");
    recorder.set_active(true);
    recorder.acknowledge(25);
    recorder.close().await;
    let message: String = sqlx::query_scalar("SELECT message FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&message).unwrap();
    assert_eq!(value["restore_progress"]["completed_bytes"], 25);
    assert_eq!(value["restore_progress"]["phase"], "downloading");
    assert!(value.get("export_progress").is_none());
    let rates: Vec<Option<f64>> =
        sqlx::query_scalar("SELECT write_rate_bps FROM s3_transfer_samples ORDER BY at_ms")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(rates.iter().flatten().any(|r| *r > 0.0));
    assert!(rates.last().unwrap().is_none());
    let recorder = Recorder::open_direction(&path, "vtl-a", "restore-a", "TAPE01", 1, true)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET status='failed',message='checksum mismatch'")
        .execute(&pool)
        .await
        .unwrap();
    recorder.set_total(100);
    recorder.acknowledge(100);
    recorder.phase("verifying");
    recorder.close().await;
    let message: String = sqlx::query_scalar("SELECT message FROM jobs")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(message, "checksum mismatch");
    pool.close().await;
    std::fs::remove_file(path).unwrap();
}
