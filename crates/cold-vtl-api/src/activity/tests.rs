use super::*;
use sqlx::SqlitePool;

struct Fixture {
    state: crate::AppState,
    root: std::path::PathBuf,
    vtl: String,
    vtl_name: String,
    second: String,
    destination: String,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The shared S3 fixture, a second VTL `Second` and the accounts `admin`
/// and `reader`.
async fn setup() -> Fixture {
    let (state, root) = crate::s3_settings::test_support::state_fixture().await;
    let pool = state.catalog.pool();
    let vtl = state.catalog.vtl_id().await.unwrap().to_string();
    let vtl_name: String = sqlx::query_scalar("SELECT name FROM vtls WHERE id=?")
        .bind(&vtl)
        .fetch_one(&pool)
        .await
        .unwrap();
    let second = cold_vtl_core::VtlId::new().to_string();
    sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'Second','ready')")
        .bind(&second)
        .execute(&pool)
        .await
        .unwrap();
    for (id, role) in [("admin", "admin"), ("reader", "viewer")] {
        sqlx::query("INSERT INTO auth_users(id,username,password_hash,role) VALUES(?,?,'x',?)")
            .bind(id)
            .bind(id)
            .bind(role)
            .execute(&pool)
            .await
            .unwrap();
    }
    let destination = state.catalog.destination().await.unwrap().id;
    Fixture {
        state,
        root,
        vtl,
        vtl_name,
        second,
        destination,
    }
}

/// A job created and last updated at `datetime('now', age)`.
#[allow(clippy::too_many_arguments)]
async fn job(
    pool: &SqlitePool,
    id: &str,
    vtl: &str,
    kind: &str,
    subject: &str,
    status: &str,
    progress: i64,
    age: &str,
) {
    sqlx::query("INSERT INTO jobs(id,vtl_id,kind,subject,status,progress,created_at,updated_at) VALUES(?,?,?,?,?,?,datetime('now',?),datetime('now',?))")
        .bind(id).bind(vtl).bind(kind).bind(subject).bind(status).bind(progress).bind(age).bind(age)
        .execute(pool).await.unwrap();
}

/// An OVH request for bucket `vc-a`, submitted by `job_id`, last updated at
/// `datetime('now', age)`.
async fn request(fixture: &Fixture, job_id: &str, operation: &str, phase: &str, age: &str) {
    let pool = fixture.state.catalog.pool();
    let prepare = format!("prepare-{job_id}");
    job(
        &pool,
        &prepare,
        &fixture.vtl,
        "archive_prepare",
        "vc-a",
        "completed",
        100,
        age,
    )
    .await;
    sqlx::query("INSERT INTO archive_requests(vtl_id,job_id,prepare_job_id,destination_id,phase,request,updated_at,operation) VALUES(?,?,?,?,?,'{\"confirm_bucket\":\"vc-a\"}',datetime('now',?),?)")
        .bind(&fixture.vtl).bind(job_id).bind(&prepare).bind(&fixture.destination).bind(phase).bind(age).bind(operation)
        .execute(&pool).await.unwrap();
}

/// A date as stored, so that assertions never straddle a clock second.
async fn stored(pool: &SqlitePool, sql: &'static str) -> String {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn active_jobs_of_every_vtl_with_their_name() {
    let fixture = setup().await;
    let pool = fixture.state.catalog.pool();
    job(
        &pool,
        "export",
        &fixture.vtl,
        "export",
        "CVT002L8",
        "running",
        41,
        "-2 minutes",
    )
    .await;
    job(
        &pool,
        "restore",
        &fixture.second,
        "restore",
        "CVT009L8",
        "queued",
        0,
        "-1 minutes",
    )
    .await;
    job(
        &pool,
        "observe",
        &fixture.vtl,
        "archive_refresh",
        "vc-a",
        "running",
        0,
        "-1 minutes",
    )
    .await;
    job(
        &pool,
        "create",
        &fixture.vtl,
        "create",
        "CVT010L8",
        "running",
        0,
        "-1 minutes",
    )
    .await;
    let activity = read(&pool, "admin").await.unwrap();
    let ids: Vec<_> = activity
        .active
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(ids, ["job:restore", "job:export"]);
    assert_eq!(activity.active[0].vtl_name, "Second");
    assert_eq!(activity.active[0].progress, None);
    assert_eq!(activity.active[1].vtl_name, fixture.vtl_name);
    assert_eq!(activity.active[1].vtl_id, fixture.vtl);
    assert_eq!(activity.active[1].progress, Some(41));
    assert_eq!(activity.active[1].source, "job");
    assert_eq!(activity.active[1].kind, "export");
    assert_eq!(activity.active[1].subject, "CVT002L8");
    assert!(activity.recent.is_empty());
}

#[tokio::test]
async fn accepted_request_replaces_its_finished_submit_job() {
    let fixture = setup().await;
    let pool = fixture.state.catalog.pool();
    job(
        &pool,
        "submit",
        &fixture.vtl,
        "archive_submit",
        "vc-a",
        "completed",
        100,
        "-1 hours",
    )
    .await;
    request(&fixture, "submit", "archive", "accepted", "-1 hours").await;
    let activity = read(&pool, "admin").await.unwrap();
    assert_eq!(activity.active.len(), 1);
    let item = &activity.active[0];
    assert_eq!(item.id, "provider:submit:accepted");
    assert_eq!(item.source, "provider");
    assert_eq!(item.kind, "archive");
    assert_eq!(item.subject, "vc-a");
    assert_eq!(
        item.destination_id.as_deref(),
        Some(fixture.destination.as_str())
    );
    assert_eq!(
        item.started_at,
        stored(&pool, "SELECT created_at FROM jobs WHERE id='submit'").await
    );
    assert!(activity.recent.is_empty());
}

#[tokio::test]
async fn running_submit_is_listed_alone() {
    let fixture = setup().await;
    let pool = fixture.state.catalog.pool();
    job(
        &pool,
        "submit",
        &fixture.vtl,
        "archive_submit",
        "vc-a",
        "running",
        10,
        "-1 minutes",
    )
    .await;
    request(&fixture, "submit", "archive", "submitting", "-1 minutes").await;
    let activity = read(&pool, "admin").await.unwrap();
    let ids: Vec<_> = activity
        .active
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(ids, ["job:submit"]);
    assert!(activity.recent.is_empty());
}

#[tokio::test]
async fn finished_requests_are_recent_with_outcome_and_date() {
    let fixture = setup().await;
    let pool = fixture.state.catalog.pool();
    for (id, kind) in [
        ("archived", "archive_submit"),
        ("restored", "recall_submit"),
        ("uncertain", "archive_submit"),
        ("refused", "archive_submit"),
    ] {
        job(
            &pool,
            id,
            &fixture.vtl,
            kind,
            "vc-a",
            "completed",
            100,
            "-3 days",
        )
        .await;
    }
    request(&fixture, "archived", "archive", "archived", "-2 hours").await;
    sqlx::query(
        "UPDATE archive_requests SET archived_at=datetime('now','-1 days') WHERE job_id='archived'",
    )
    .execute(&pool)
    .await
    .unwrap();
    request(&fixture, "restored", "recall", "restored", "-2 hours").await;
    sqlx::query(
        "UPDATE archive_requests SET restored_at=datetime('now','-2 days') WHERE job_id='restored'",
    )
    .execute(&pool)
    .await
    .unwrap();
    // OVH's answer was lost: the submit failed, the request is uncertain, and
    // each observation rewrites its updated_at.
    sqlx::query("UPDATE jobs SET status='failed',updated_at=datetime('now','-3 hours') WHERE id='uncertain'")
        .execute(&pool)
        .await
        .unwrap();
    request(&fixture, "uncertain", "archive", "uncertain", "-1 minutes").await;
    request(&fixture, "refused", "archive", "failed", "-4 hours").await;
    let activity = read(&pool, "admin").await.unwrap();
    assert!(activity.active.is_empty());
    let recent: Vec<_> = activity
        .recent
        .iter()
        .map(|item| {
            (
                item.id.as_str(),
                item.source,
                item.kind.as_str(),
                item.outcome,
            )
        })
        .collect();
    assert_eq!(
        recent,
        [
            (
                "provider:uncertain:uncertain",
                "provider",
                "archive",
                Outcome::Uncertain
            ),
            (
                "provider:refused:failed",
                "provider",
                "archive",
                Outcome::Failure
            ),
            (
                "provider:archived:archived",
                "provider",
                "archive",
                Outcome::Success
            ),
            (
                "provider:restored:restored",
                "provider",
                "recall",
                Outcome::Success
            ),
        ]
    );
    assert_eq!(
        activity.recent[0].finished_at,
        stored(&pool, "SELECT updated_at FROM jobs WHERE id='uncertain'").await
    );
    assert_eq!(
        activity.recent[2].finished_at,
        stored(
            &pool,
            "SELECT archived_at FROM archive_requests WHERE job_id='archived'"
        )
        .await
    );
    assert_eq!(
        activity.recent[3].finished_at,
        stored(
            &pool,
            "SELECT restored_at FROM archive_requests WHERE job_id='restored'"
        )
        .await
    );
    assert_eq!(activity.recent[3].subject, "vc-a");
}

#[tokio::test]
async fn failed_submit_is_one_failed_job() {
    let fixture = setup().await;
    let pool = fixture.state.catalog.pool();
    job(
        &pool,
        "submit",
        &fixture.vtl,
        "archive_submit",
        "vc-a",
        "failed",
        0,
        "-1 hours",
    )
    .await;
    request(&fixture, "submit", "archive", "failed", "-1 hours").await;
    let activity = read(&pool, "admin").await.unwrap();
    assert_eq!(activity.recent.len(), 1);
    assert_eq!(activity.recent[0].id, "job:submit");
    assert_eq!(activity.recent[0].source, "job");
    assert_eq!(activity.recent[0].outcome, Outcome::Failure);
}

#[tokio::test]
async fn recent_window_limit_and_order() {
    let fixture = setup().await;
    let pool = fixture.state.catalog.pool();
    for hours in 1..=22 {
        job(
            &pool,
            &format!("export-{hours:02}"),
            &fixture.vtl,
            "export",
            &format!("CVT{hours:03}L8"),
            "completed",
            100,
            &format!("-{hours} hours"),
        )
        .await;
    }
    job(
        &pool,
        "old",
        &fixture.vtl,
        "export",
        "CVT099L8",
        "completed",
        100,
        "-8 days",
    )
    .await;
    let activity = read(&pool, "admin").await.unwrap();
    let ids: Vec<_> = activity.recent.iter().map(|item| item.id.clone()).collect();
    let expected: Vec<_> = (1..=20)
        .map(|hours| format!("job:export-{hours:02}"))
        .collect();
    assert_eq!(ids, expected);
    assert!(
        activity
            .recent
            .iter()
            .all(|item| item.outcome == Outcome::Success)
    );
}

#[tokio::test]
async fn unread_follows_each_account() {
    let fixture = setup().await;
    let pool = fixture.state.catalog.pool();
    job(
        &pool,
        "one",
        &fixture.vtl,
        "export",
        "CVT001L8",
        "completed",
        100,
        "-2 hours",
    )
    .await;
    job(
        &pool,
        "two",
        &fixture.second,
        "restore",
        "CVT002L8",
        "failed",
        0,
        "-1 hours",
    )
    .await;
    let before = read(&pool, "admin").await.unwrap();
    assert_eq!(before.unread, 2);
    assert_eq!(before.seen_at, None);
    assert!(before.recent.iter().all(|item| item.unread));
    let seen = mark_seen(&pool, "admin").await.unwrap();
    let after = read(&pool, "admin").await.unwrap();
    assert_eq!(after.unread, 0);
    assert_eq!(after.seen_at.as_deref(), Some(seen.as_str()));
    assert!(after.recent.iter().all(|item| !item.unread));
    assert_eq!(read(&pool, "reader").await.unwrap().unread, 2);
    // Seen half an hour ago, then an operation finished ten minutes ago.
    sqlx::query("UPDATE activity_seen SET seen_at=datetime('now','-30 minutes')")
        .execute(&pool)
        .await
        .unwrap();
    job(
        &pool,
        "three",
        &fixture.vtl,
        "reconcile",
        "CVT003L8",
        "completed",
        100,
        "-10 minutes",
    )
    .await;
    let later = read(&pool, "admin").await.unwrap();
    assert_eq!(later.unread, 1);
    assert_eq!(later.recent[0].id, "job:three");
    assert!(later.recent[0].unread);
    mark_seen(&pool, "admin").await.unwrap();
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM activity_seen")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    assert_eq!(read(&pool, "admin").await.unwrap().unread, 0);
}

#[tokio::test]
async fn uncertain_submit_follows_its_request_until_archived() {
    let fixture = setup().await;
    let pool = fixture.state.catalog.pool();
    job(
        &pool,
        "submit",
        &fixture.vtl,
        "archive_submit",
        "vc-a",
        "failed",
        100,
        "-2 hours",
    )
    .await;
    request(&fixture, "submit", "archive", "uncertain", "-1 minutes").await;
    let activity = read(&pool, "admin").await.unwrap();
    let recent: Vec<_> = activity
        .recent
        .iter()
        .map(|item| (item.id.as_str(), item.outcome))
        .collect();
    assert_eq!(recent, [("provider:submit:uncertain", Outcome::Uncertain)]);
    // OVH reports « Archiving », then « Archived ».
    sqlx::query("UPDATE archive_requests SET phase='accepted' WHERE job_id='submit'")
        .execute(&pool)
        .await
        .unwrap();
    let activity = read(&pool, "admin").await.unwrap();
    let active: Vec<_> = activity
        .active
        .iter()
        .map(|item| item.id.as_str())
        .collect();
    assert_eq!(active, ["provider:submit:accepted"]);
    assert!(activity.recent.is_empty());
    sqlx::query("UPDATE archive_requests SET phase='archived' WHERE job_id='submit'")
        .execute(&pool)
        .await
        .unwrap();
    let activity = read(&pool, "admin").await.unwrap();
    assert!(activity.active.is_empty());
    let recent: Vec<_> = activity
        .recent
        .iter()
        .map(|item| (item.id.as_str(), item.outcome))
        .collect();
    assert_eq!(recent, [("provider:submit:archived", Outcome::Success)]);
}
