//! Automatic archiving: N days after a bucket stopped being the active
//! destination, once per day, never forced. Only buckets VaultCenter created
//! empty are archived automatically; an attached bucket stays manual. A bucket
//! must hold at least one copy, and must have stopped being active after
//! automatic archiving was enabled: older buckets stay manual (R29).
use crate::{ApiError, AppState};
use serde_json::json;
use sqlx::SqlitePool;
use std::time::Duration;

const ACTOR: &str = "planification";

/// When destination `d` last stopped being the active one: the switched
/// creation that replaced it, or a manual change of bucket (`deactivated_at`).
/// The single source of that time for the selection below.
macro_rules! inactive_since {
    () => {
        "(SELECT MAX(v) FROM (SELECT c.updated_at AS v FROM bucket_creations c WHERE c.source_destination_id=d.id AND c.phase='switched' UNION ALL SELECT d.deactivated_at))"
    };
}

/// The oldest bucket of `vtl` due for automatic archiving today.
pub(crate) async fn due_archive(
    pool: &SqlitePool,
    vtl: &str,
) -> Result<Option<(String, String)>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT d.id,json_extract(d.identity,'$.bucket') FROM storage_destinations d
         JOIN storage_schedules s ON s.vtl_id=d.vtl_id
         WHERE d.vtl_id=?1 AND d.active=0 AND d.identity IS NOT NULL
           AND s.archive_after_days IS NOT NULL AND s.archive_terms_accepted_at IS NOT NULL
           AND (EXISTS(SELECT 1 FROM bucket_creations c WHERE c.next_destination_id=d.id AND c.phase='switched')
                OR EXISTS(SELECT 1 FROM bucket_rotations r WHERE r.next_destination_id=d.id AND r.phase IN ('ready','switched')))
           AND ",
        inactive_since!(),
        " <= datetime('now','-'||s.archive_after_days||' days')
           AND ",
        inactive_since!(),
        " > s.archive_enabled_at
           AND EXISTS(SELECT 1 FROM remote_generations g WHERE g.vtl_id=d.vtl_id AND g.destination_id=d.id)
           AND NOT EXISTS(SELECT 1 FROM archive_requests a WHERE a.destination_id=d.id AND a.operation='archive'
               AND a.phase IN ('queued','submitting','uncertain','accepted','archived'))
           AND NOT EXISTS(SELECT 1 FROM scheduled_archive_attempts t WHERE t.destination_id=d.id AND t.attempted_on=date('now'))
         ORDER BY d.created_at,d.id LIMIT 1"
    ))
    .bind(vtl)
    .fetch_optional(pool)
    .await
}

/// Refusals reach Activité and e-mails as readable sentences: a technical error
/// (lower-case or unpunctuated, e.g. from SQLite or the system) is replaced.
fn refusal_reason(reason: &str) -> &str {
    let sentence = reason.chars().next().is_some_and(char::is_uppercase)
        && reason.ends_with(['.', '!', '?', '»']);
    if sentence {
        reason
    } else {
        "A technical error prevented the request from being sent to OVH."
    }
}

/// A refused automatic archive is an operation in Activité and, through the
/// `notification_job_terminal` trigger, a failure notification. The day's
/// attempt is recorded in the same transaction, so it is never consumed
/// without its failed operation.
pub(crate) async fn record_refusal(
    state: &AppState,
    destination: &str,
    bucket: &str,
    reason: &str,
) -> Result<(), ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    let revision = state.catalog.destination().await?.revision;
    let id = uuid::Uuid::new_v4().to_string();
    let mut tx = state
        .catalog
        .pool()
        .begin()
        .await
        .map_err(ApiError::conflict)?;
    sqlx::query("INSERT INTO jobs(id,kind,subject,status,progress,vtl_id,destination_id,configuration_revision) VALUES(?,'archive_submit',?,'queued',0,?,?,?)")
        .bind(&id).bind(bucket).bind(&vtl).bind(destination).bind(revision)
        .execute(&mut *tx).await.map_err(ApiError::conflict)?;
    sqlx::query("UPDATE jobs SET status='failed',progress=100,message=?,updated_at=CURRENT_TIMESTAMP WHERE id=?")
        .bind(format!("Automatic archiving was refused. {} A new attempt will take place tomorrow.", refusal_reason(reason)))
        .bind(&id).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    record_attempt(&mut tx, destination, &vtl, &id).await?;
    tx.commit().await.map_err(ApiError::conflict)
}

/// Today's attempt for `destination`, answered by operation `job`.
async fn record_attempt(
    connection: &mut sqlx::SqliteConnection,
    destination: &str,
    vtl: &str,
    job: &str,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO scheduled_archive_attempts(destination_id,vtl_id,attempted_on,job_id) VALUES(?,?,date('now'),?) ON CONFLICT(destination_id) DO UPDATE SET attempted_on=excluded.attempted_on,job_id=excluded.job_id")
        .bind(destination).bind(vtl).bind(job).execute(connection).await.map_err(ApiError::conflict)?;
    Ok(())
}

/// A running job or an unresolved OVH operation postpones the archive to the
/// next pass: not a failure, and no attempt is recorded.
async fn postponed(state: &AppState, vtl: &str) -> Result<bool, ApiError> {
    let busy: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE vtl_id=? AND status IN ('queued','running')",
    )
    .bind(vtl)
    .fetch_one(&state.catalog.pool())
    .await
    .map_err(ApiError::conflict)?;
    Ok(busy > 0 || super::idle(state).await.is_err())
}

/// One pass over every VTL with automatic archiving; a VTL's failure is
/// logged and never stops the others.
pub(crate) async fn run_once(state: &AppState) -> Result<(), ApiError> {
    let vtls: Vec<String> = sqlx::query_scalar(
        "SELECT vtl_id FROM storage_schedules WHERE archive_after_days IS NOT NULL",
    )
    .fetch_all(&state.catalog.pool())
    .await
    .map_err(ApiError::conflict)?;
    for vtl in vtls {
        if let Err(error) = run_vtl(state, &vtl).await {
            eprintln!("Automatic archive failed for VTL {vtl}: {}", error.message);
        }
    }
    Ok(())
}

async fn run_vtl(state: &AppState, vtl: &str) -> Result<(), ApiError> {
    let scoped = crate::vtls::installed_state(state.clone(), vtl.to_owned()).await?;
    let Some((destination, bucket)) = due_archive(&scoped.catalog.pool(), vtl)
        .await
        .map_err(ApiError::conflict)?
    else {
        return Ok(());
    };
    if postponed(&scoped, vtl).await? {
        return Ok(());
    }
    let selected = crate::destinations::select(
        scoped,
        crate::destinations::Selection {
            destination_id: Some(destination.clone()),
        },
    )
    .await?;
    attempt(&selected, vtl, &destination, &bucket).await
}

/// Starts the archive of `destination` as the scheduler. The attempt is
/// recorded only once `start` has answered; a VTL that became busy meanwhile
/// (`start` re-checks under the operations lock) is postponed, not refused.
async fn attempt(
    selected: &AppState,
    vtl: &str,
    destination: &str,
    bucket: &str,
) -> Result<(), ApiError> {
    let confirmation = json!({"confirm_bucket":bucket,"acknowledge_billing":true,"exclusive_bucket":false,"next_bucket":null,"scheduled":true});
    match super::start(
        selected.clone(),
        ACTOR,
        "archive",
        "submit",
        Some(confirmation),
    )
    .await
    {
        Ok((_, axum::Json(job))) => {
            let mut connection = selected
                .catalog
                .pool()
                .acquire()
                .await
                .map_err(ApiError::conflict)?;
            record_attempt(&mut connection, destination, vtl, &job.id).await
        }
        Err(_) if postponed(selected, vtl).await? => Ok(()),
        Err(error) => record_refusal(selected, destination, bucket, &error.message).await,
    }
}

/// Hourly: an attempt is made at most once per bucket and day.
pub fn monitor(state: AppState) {
    if state.config.archive_helper.is_none() {
        return;
    }
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(3600));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) = run_once(&state).await {
                eprintln!("Automatic archive pass failed: {}", error.message);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3_settings::test_support::older_bucket_archive;
    use serde_json::Value;

    /// The shared archive fixture plus two inactive buckets replaced 10 days
    /// ago: `old-managed` (created by VaultCenter) and `old-attached`, with a
    /// 7-day automatic archive schedule whose OVH terms are accepted.
    async fn setup() -> (AppState, std::path::PathBuf, String) {
        let (state, root) = crate::s3_settings::test_support::archive_fixture(
            r#"cat >> "$(dirname "$0")/input.json""#,
        )
        .await;
        let pool = state.catalog.pool();
        let vtl = state.catalog.vtl_id().await.unwrap().to_string();
        let active = state.catalog.destination().await.unwrap().id;
        let identity = |bucket: &str| {
            json!({"provider":"ovh_cold_archive_v1","endpoint":cold_vtl_core::OVH_ARCHIVE_ENDPOINT,"region":"rbx-archive","bucket":bucket,"prefix":"vtl"}).to_string()
        };
        for (id, bucket) in [("old-managed", "vc-old"), ("old-attached", "legacy-bucket")] {
            sqlx::query("INSERT INTO storage_destinations(id,vtl_id,identity,active,created_at) VALUES(?,?,?,0,'2026-01-01 00:00:00')").bind(id).bind(&vtl).bind(identity(bucket)).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,1,?)").bind(id).bind(identity(bucket)).execute(&pool).await.unwrap();
        }
        // old-managed was created by VaultCenter; both were replaced 10 days ago.
        for (job, source, next, age) in [
            ("c1", None, Some("old-managed"), "-40 days"),
            ("c2", Some("old-managed"), Some(active.as_str()), "-10 days"),
            ("c3", Some("old-attached"), None, "-10 days"),
        ] {
            sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id) VALUES(?,'bucket_create',?,'completed',?)").bind(job).bind(job).bind(&vtl).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO bucket_creations(job_id,vtl_id,source_destination_id,next_destination_id,next_bucket,configuration,revision,phase,updated_at) VALUES(?,?,?,?,?,'{}',1,'switched',datetime('now',?))")
                .bind(job).bind(&vtl).bind(source).bind(next).bind(job).bind(age).execute(&pool).await.unwrap();
        }
        // Both hold a copy of TAPE01.
        let tape =
            cold_vtl_core::Cartridge::new(cold_vtl_core::Barcode::parse("TAPE01").unwrap(), 1024)
                .unwrap();
        state.catalog.create_cartridge(&tape).await.unwrap();
        for destination in ["old-managed", "old-attached"] {
            sqlx::query("INSERT INTO remote_generations(vtl_id,destination_id,barcode,generation,metadata,cartridge_id) SELECT vtl_id,?,barcode,1,'{}',id FROM cartridges WHERE vtl_id=? AND barcode='TAPE01'")
                .bind(destination).bind(&vtl).execute(&pool).await.unwrap();
        }
        // Automatic archiving was enabled 30 days ago, before both were replaced.
        sqlx::query("INSERT INTO storage_schedules(vtl_id,revision,rotation,archive_after_days,archive_terms_accepted_at,archive_terms_accepted_by,archive_enabled_at) VALUES(?,1,'manual',7,CURRENT_TIMESTAMP,'admin',datetime('now','-30 days'))").bind(&vtl).execute(&pool).await.unwrap();
        (state, root, vtl)
    }

    #[tokio::test]
    async fn empty_managed_bucket_is_never_due_and_records_no_failure() {
        let (state, root, vtl) = setup().await;
        let pool = state.catalog.pool();
        sqlx::query("DELETE FROM remote_generations WHERE destination_id='old-managed'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(due_archive(&pool, &vtl).await.unwrap(), None);
        run_once(&state).await.unwrap();
        assert_eq!(outcome(&pool).await, (0, 0, 0));
        pool.close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn only_buckets_that_became_inactive_after_enabling_are_due() {
        let (state, root, vtl) = setup().await;
        let pool = state.catalog.pool();
        // Enabled 5 days ago: old-managed was replaced 10 days ago, before.
        sqlx::query("UPDATE storage_schedules SET archive_enabled_at=datetime('now','-5 days')")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(due_archive(&pool, &vtl).await.unwrap(), None);
        run_once(&state).await.unwrap();
        assert_eq!(outcome(&pool).await, (0, 0, 0));
        // Enabled 12 days ago: replaced after enabling, and 7 days have passed.
        sqlx::query("UPDATE storage_schedules SET archive_enabled_at=datetime('now','-12 days')")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            due_archive(&pool, &vtl).await.unwrap(),
            Some(("old-managed".into(), "vc-old".into()))
        );
        // Not yet N days: replaced 6 days ago, after enabling.
        sqlx::query(
            "UPDATE bucket_creations SET updated_at=datetime('now','-6 days') WHERE job_id='c2'",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(due_archive(&pool, &vtl).await.unwrap(), None);
        pool.close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn due_archive_selects_only_old_managed_unarchived_buckets_once_a_day() {
        let (state, root, vtl) = setup().await;
        let pool = state.catalog.pool();
        assert_eq!(
            due_archive(&pool, &vtl).await.unwrap(),
            Some(("old-managed".into(), "vc-old".into()))
        );
        sqlx::query("UPDATE storage_schedules SET archive_after_days=14")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(due_archive(&pool, &vtl).await.unwrap(), None);
        sqlx::query("UPDATE storage_schedules SET archive_after_days=7")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO scheduled_archive_attempts(destination_id,vtl_id,attempted_on) VALUES('old-managed',?,date('now'))").bind(&vtl).execute(&pool).await.unwrap();
        assert_eq!(due_archive(&pool, &vtl).await.unwrap(), None);
        sqlx::query("UPDATE scheduled_archive_attempts SET attempted_on=date('now','-1 day')")
            .execute(&pool)
            .await
            .unwrap();
        assert!(due_archive(&pool, &vtl).await.unwrap().is_some());
        sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id) VALUES('a1','archive_submit','vc-old','completed',?)").bind(&vtl).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,operation,vtl_id,destination_id) VALUES('a1','a1','accepted','{}','archive',?,'old-managed')").bind(&vtl).execute(&pool).await.unwrap();
        // Archived (or being archived): and the attached bucket is never a candidate.
        assert_eq!(due_archive(&pool, &vtl).await.unwrap(), None);
        sqlx::query("UPDATE archive_requests SET phase='archived'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(due_archive(&pool, &vtl).await.unwrap(), None);
        sqlx::query("UPDATE archive_requests SET phase='failed'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(due_archive(&pool, &vtl).await.unwrap().is_some());
        // Without the OVH terms accepted (R18) there is no delay: the schedule
        // cannot hold one, so nothing is archived automatically.
        assert!(
            sqlx::query("UPDATE storage_schedules SET archive_terms_accepted_at=NULL")
                .execute(&pool)
                .await
                .is_err()
        );
        sqlx::query(
            "UPDATE storage_schedules SET archive_after_days=NULL,archive_terms_accepted_at=NULL",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(due_archive(&pool, &vtl).await.unwrap(), None);
        sqlx::query("UPDATE storage_schedules SET archive_after_days=7,archive_terms_accepted_at=CURRENT_TIMESTAMP")
            .execute(&pool)
            .await
            .unwrap();
        assert!(due_archive(&pool, &vtl).await.unwrap().is_some());
        sqlx::query("UPDATE storage_schedules SET archive_after_days=NULL")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(due_archive(&pool, &vtl).await.unwrap(), None);
        pool.close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn managed_bucket_replaced_by_an_existing_bucket_is_still_archived() {
        let (state, root, vtl) = setup().await;
        let pool = state.catalog.pool();
        // old-managed was replaced through attach-as-active, not by a creation.
        sqlx::query("DELETE FROM bucket_creations WHERE source_destination_id='old-managed'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(due_archive(&pool, &vtl).await.unwrap(), None);
        sqlx::query("UPDATE storage_destinations SET deactivated_at=datetime('now','-6 days') WHERE id IN ('old-managed','old-attached')")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            due_archive(&pool, &vtl).await.unwrap(),
            None,
            "7 days not elapsed"
        );
        sqlx::query("UPDATE storage_destinations SET deactivated_at=datetime('now','-8 days') WHERE id IN ('old-managed','old-attached')")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            due_archive(&pool, &vtl).await.unwrap(),
            Some(("old-managed".into(), "vc-old".into()))
        );
        // The latest end of activity counts: reactivated then replaced again yesterday.
        sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id) VALUES('c4','bucket_create','c4','completed',?)").bind(&vtl).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO bucket_creations(job_id,vtl_id,source_destination_id,next_bucket,configuration,revision,phase,updated_at) VALUES('c4',?,'old-managed','c4','{}',1,'switched',datetime('now','-1 day'))")
            .bind(&vtl).execute(&pool).await.unwrap();
        assert_eq!(due_archive(&pool, &vtl).await.unwrap(), None);
        pool.close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn busy_vtl_postpones_without_recording_an_attempt() {
        let (state, root, vtl) = setup().await;
        let pool = state.catalog.pool();
        sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id) VALUES('busy','reconcile','TAPE01','running',?)").bind(&vtl).execute(&pool).await.unwrap();
        run_once(&state).await.unwrap();
        sqlx::query("UPDATE jobs SET status='completed' WHERE id='busy'")
            .execute(&pool)
            .await
            .unwrap();
        // An unresolved OVH operation (here an older bucket's archive) also postpones.
        older_bucket_archive(&state, "uncertain").await;
        run_once(&state).await.unwrap();
        let attempts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scheduled_archive_attempts")
            .fetch_one(&pool)
            .await
            .unwrap();
        let requests: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM archive_requests WHERE destination_id='old-managed'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((attempts, requests), (0, 0));
        pool.close().await;
        std::fs::remove_dir_all(root).ok();
    }

    /// Counts of (failed archive operations, attempts, archive requests) for old-managed.
    async fn outcome(pool: &sqlx::SqlitePool) -> (i64, i64, i64) {
        sqlx::query_as("SELECT (SELECT COUNT(*) FROM jobs WHERE kind='archive_submit' AND status='failed'),(SELECT COUNT(*) FROM scheduled_archive_attempts),(SELECT COUNT(*) FROM archive_requests WHERE destination_id='old-managed')")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn vtl_busy_after_the_pre_check_is_postponed_not_refused() {
        let (state, root, vtl) = setup().await;
        let pool = state.catalog.pool();
        let (destination, bucket) = due_archive(&pool, &vtl).await.unwrap().unwrap();
        let selected = crate::destinations::select(
            state.clone(),
            crate::destinations::Selection {
                destination_id: Some(destination.clone()),
            },
        )
        .await
        .unwrap();
        // A job starts between the pre-check and `start`, which refuses.
        sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id) VALUES('busy','export','TAPE01','queued',?)").bind(&vtl).execute(&pool).await.unwrap();
        attempt(&selected, &vtl, &destination, &bucket)
            .await
            .unwrap();
        assert_eq!(outcome(&pool).await, (0, 0, 0));
        pool.close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn refused_start_is_recorded_as_the_day_attempt() {
        let (mut state, root, _) = setup().await;
        let mut config = (*state.config).clone();
        config.archive_helper = None;
        state.config = std::sync::Arc::new(config);
        let pool = state.catalog.pool();
        run_once(&state).await.unwrap();
        let (job, message): (String, String) = sqlx::query_as(
            "SELECT id,message FROM jobs WHERE kind='archive_submit' AND status='failed'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            message,
            "Automatic archiving was refused. Archive adapter not configured. A new attempt will take place tomorrow."
        );
        let recorded: Option<String> = sqlx::query_scalar("SELECT job_id FROM scheduled_archive_attempts WHERE destination_id='old-managed' AND attempted_on=date('now')").fetch_one(&pool).await.unwrap();
        assert_eq!(recorded, Some(job));
        run_once(&state).await.unwrap();
        assert_eq!(outcome(&pool).await, (1, 1, 0), "one refusal a day");
        pool.close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn technical_reasons_are_replaced_by_a_french_sentence() {
        assert_eq!(
            refusal_reason("Copy TAPE01 is corrupted."),
            "Copy TAPE01 is corrupted."
        );
        for technical in [
            "error returned from database: (code: 5) database is locked",
            "No such file or directory (os error 2)",
            "",
        ] {
            assert_eq!(
                refusal_reason(technical),
                "A technical error prevented the request from being sent to OVH."
            );
        }
    }

    #[tokio::test]
    async fn scheduled_archive_starts_once_as_the_scheduler() {
        let (state, root, _) = setup().await;
        let pool = state.catalog.pool();
        run_once(&state).await.unwrap();
        let (job, request): (String, String) = sqlx::query_as(
            "SELECT job_id,request FROM archive_requests WHERE destination_id='old-managed'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let request: Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["actor"], "planification");
        assert_eq!(request["confirm_bucket"], "vc-old");
        assert_eq!(request["exclusive_bucket"], false);
        let recorded: Option<String> = sqlx::query_scalar("SELECT job_id FROM scheduled_archive_attempts WHERE destination_id='old-managed' AND attempted_on=date('now')").fetch_one(&pool).await.unwrap();
        assert_eq!(recorded.as_deref(), Some(job.as_str()));
        sqlx::query("UPDATE jobs SET status='failed' WHERE id=?")
            .bind(&job)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE archive_requests SET phase='failed'")
            .execute(&pool)
            .await
            .unwrap();
        run_once(&state).await.unwrap();
        let requests: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM archive_requests")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(requests, 1, "a failed attempt waits for the next day");
        pool.close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn refusal_is_reported_in_activity_and_notifications() {
        let (state, root, _) = setup().await;
        let pool = state.catalog.pool();
        sqlx::query("UPDATE notification_settings SET configuration=json('{\"enabled\":true,\"recipients\":[\"ops@example.test\"],\"events\":{\"archive\":{\"success\":true,\"failure\":true}}}') WHERE id=1").execute(&pool).await.unwrap();
        let selected = crate::destinations::select(
            state.clone(),
            crate::destinations::Selection {
                destination_id: Some("old-managed".into()),
            },
        )
        .await
        .unwrap();
        record_refusal(
            &selected,
            "old-managed",
            "vc-old",
            "Copy TAPE01 is corrupted.",
        )
        .await
        .unwrap();
        let (status, message): (String, String) =
            sqlx::query_as("SELECT status,message FROM jobs WHERE kind='archive_submit'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "failed");
        assert!(
            message.starts_with("Automatic archiving was refused. Copy TAPE01 is corrupted."),
            "{message}"
        );
        let payload: String = sqlx::query_scalar("SELECT payload FROM notification_events")
            .fetch_one(&pool)
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(
            (payload["kind"].as_str(), payload["outcome"].as_str()),
            (Some("archive_submit"), Some("failure"))
        );
        pool.close().await;
        std::fs::remove_dir_all(root).ok();
    }
}
