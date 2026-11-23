//! Per-VTL schedule: when the export bucket changes and when inactive buckets
//! are archived automatically. Every date is a UTC calendar day.
use super::*;
use cold_vtl_core::{CivilDate, RotationSchedule};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleUpdate {
    revision: i64,
    rotation: String,
    #[serde(default)]
    weekday: Option<u8>,
    #[serde(default)]
    month_day: Option<u8>,
    #[serde(default)]
    archive_after_days: Option<u16>,
    #[serde(default)]
    accept_archive_terms: bool,
}

pub(crate) struct Schedule {
    pub revision: i64,
    pub rotation: RotationSchedule,
    pub archive_after_days: Option<i64>,
    pub terms_accepted: bool,
}

pub(crate) async fn load(state: &AppState) -> Result<Schedule, ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    type Row = (
        i64,
        String,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<String>,
    );
    let row: Option<Row> = sqlx::query_as("SELECT revision,rotation,weekday,month_day,archive_after_days,archive_terms_accepted_at FROM storage_schedules WHERE vtl_id=?")
        .bind(&vtl).fetch_optional(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    let Some((revision, rotation, weekday, month_day, archive_after_days, accepted)) = row else {
        return Ok(Schedule {
            revision: 0,
            rotation: RotationSchedule::Manual,
            archive_after_days: None,
            terms_accepted: false,
        });
    };
    let small = |value: Option<i64>| {
        value
            .map(|v| u8::try_from(v).map_err(|_| ApiError::conflict("Invalid saved schedule.")))
            .transpose()
    };
    Ok(Schedule {
        revision,
        rotation: RotationSchedule::from_parts(&rotation, small(weekday)?, small(month_day)?)
            .map_err(ApiError::conflict)?,
        archive_after_days,
        terms_accepted: accepted.is_some(),
    })
}

async fn today(state: &AppState) -> Result<CivilDate, ApiError> {
    let value: String = sqlx::query_scalar("SELECT strftime('%Y-%m-%d','now')")
        .fetch_one(&state.catalog.pool())
        .await
        .map_err(ApiError::conflict)?;
    CivilDate::parse(&value).ok_or_else(|| ApiError::conflict("Invalid catalogue date."))
}

/// Last day a bucket was switched for this VTL (automatically, or manually:
/// the replaced destination's `deactivated_at`), or its rotation changed.
async fn served(state: &AppState) -> Result<Option<CivilDate>, ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    let value: Option<String> = sqlx::query_scalar("SELECT MAX(v) FROM (SELECT rotation_served_at AS v FROM storage_schedules WHERE vtl_id=?1 UNION ALL SELECT updated_at FROM bucket_creations WHERE vtl_id=?1 AND phase='switched' UNION ALL SELECT updated_at FROM bucket_rotations WHERE vtl_id=?1 AND phase='switched' UNION ALL SELECT deactivated_at FROM storage_destinations WHERE vtl_id=?1)")
        .bind(&vtl).fetch_one(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    Ok(value.as_deref().and_then(CivilDate::parse))
}

/// Whether the next export must first create the bucket of a new period.
pub(crate) async fn rotation_due(state: &AppState) -> Result<bool, ApiError> {
    let schedule = load(state).await?;
    Ok(schedule
        .rotation
        .is_due(today(state).await?, served(state).await?))
}

fn gate(vtl: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex, OnceLock},
    };
    static GATES: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    GATES
        .get_or_init(Default::default)
        .lock()
        .expect("rotation gates poisoned")
        .entry(vtl.to_owned())
        .or_default()
        .clone()
}

/// Before an export: when a new period started and no bucket was switched in
/// it, create and activate the period's bucket, then let the export go there.
/// The work runs in its own task, serialised per VTL, so a dropped HTTP
/// request never interrupts a provider mutation and concurrent exports of the
/// same period create a single bucket.
pub(crate) async fn rotate_if_due(state: &AppState) -> Result<(), ApiError> {
    if state.catalog.pinned() {
        return Ok(());
    }
    let vtl = state.catalog.vtl_id().await?.to_string();
    let gate = gate(&vtl);
    let worker = state.clone();
    tokio::spawn(async move {
        let _serial = gate.lock_owned().await;
        let outcome = async {
            if !rotation_due(&worker).await? || !super::buckets::creation_supported(&worker).await?
            {
                return Ok(());
            }
            super::buckets::create_for_schedule(&worker)
                .await
                .map(|_| ())
        }
        .await;
        // The export request may be gone: keep the reason in the log too.
        if let Err(error) = &outcome {
            eprintln!("Period bucket for VTL {vtl} not created: {}", error.message);
        }
        outcome
    })
    .await
    .map_err(|_| {
        ApiError::conflict(
            "The creation of the new period's bucket was interrupted. The export was not started.",
        )
    })?
}

async fn view(state: &AppState) -> Result<Value, ApiError> {
    let schedule = load(state).await?;
    let (weekday, month_day) = match schedule.rotation {
        RotationSchedule::Weekly { weekday } => (Some(weekday), None),
        RotationSchedule::Monthly { month_day } => (None, Some(month_day)),
        RotationSchedule::Manual => (None, None),
    };
    let today = today(state).await?;
    Ok(json!({
        "revision": schedule.revision,
        "rotation": schedule.rotation.name(),
        "weekday": weekday,
        "month_day": month_day,
        "archive_after_days": schedule.archive_after_days,
        "archive_terms_accepted": schedule.terms_accepted,
        "rotation_supported": super::buckets::creation_supported(state).await?,
        "rotation_due": schedule.rotation.is_due(today, served(state).await?),
        "period_start": schedule.rotation.period_start(today).map(|day| day.to_string()),
        "active_bucket": current(state).await?.1.map(|config| config.bucket),
    }))
}

async fn save(state: &AppState, actor: &str, update: ScheduleUpdate) -> Result<(), ApiError> {
    let rotation = RotationSchedule::from_parts(&update.rotation, update.weekday, update.month_day)
        .map_err(ApiError::bad_request)?;
    if update
        .archive_after_days
        .is_some_and(|days| !(1..=365).contains(&days))
    {
        return Err(ApiError::bad_request(
            "Choose an automatic archiving delay of 1 to 365 days.",
        ));
    }
    let _lock = state.operations.lock().await;
    let ovh = super::buckets::creation_supported(state).await?;
    if rotation != RotationSchedule::Manual && !ovh {
        return Err(ApiError::bad_request(
            "Scheduled bucket changes require an OVH Cold Archive connection.",
        ));
    }
    if update.archive_after_days.is_some() && !ovh {
        return Err(ApiError::bad_request(
            "Automatic archiving requires an OVH Cold Archive connection.",
        ));
    }
    let saved = load(state).await?;
    if update.revision != saved.revision {
        return Err(ApiError::conflict(
            "The settings changed in the meantime. Reload them.",
        ));
    }
    if update.archive_after_days.is_some() && !update.accept_archive_terms && !saved.terms_accepted
    {
        return Err(ApiError::bad_request(
            "Accept the OVH conditions to enable automatic archiving.",
        ));
    }
    let (weekday, month_day) = match rotation {
        RotationSchedule::Weekly { weekday } => (Some(i64::from(weekday)), None),
        RotationSchedule::Monthly { month_day } => (None, Some(i64::from(month_day))),
        RotationSchedule::Manual => (None, None),
    };
    sqlx::query("INSERT INTO storage_schedules(vtl_id,revision,rotation,weekday,month_day,rotation_served_at,archive_after_days,archive_terms_accepted_at,archive_terms_accepted_by,archive_enabled_at)
        VALUES(?1,1,?2,?3,?4,CURRENT_TIMESTAMP,?5,COALESCE(CASE WHEN ?6 THEN CURRENT_TIMESTAMP END,(SELECT archive_terms_accepted_at FROM storage_schedules WHERE vtl_id=?1)),COALESCE(CASE WHEN ?6 THEN ?7 END,(SELECT archive_terms_accepted_by FROM storage_schedules WHERE vtl_id=?1)),CASE WHEN ?5 IS NOT NULL THEN CURRENT_TIMESTAMP END)
        ON CONFLICT(vtl_id) DO UPDATE SET
          revision=storage_schedules.revision+1,
          rotation_served_at=CASE WHEN storage_schedules.rotation IS excluded.rotation AND storage_schedules.weekday IS excluded.weekday AND storage_schedules.month_day IS excluded.month_day THEN storage_schedules.rotation_served_at ELSE CURRENT_TIMESTAMP END,
          rotation=excluded.rotation, weekday=excluded.weekday, month_day=excluded.month_day,
          archive_enabled_at=CASE WHEN excluded.archive_after_days IS NULL THEN NULL WHEN storage_schedules.archive_after_days IS NULL THEN CURRENT_TIMESTAMP ELSE storage_schedules.archive_enabled_at END,
          archive_after_days=excluded.archive_after_days,
          archive_terms_accepted_at=COALESCE(storage_schedules.archive_terms_accepted_at,excluded.archive_terms_accepted_at),
          archive_terms_accepted_by=COALESCE(storage_schedules.archive_terms_accepted_by,excluded.archive_terms_accepted_by),
          updated_at=CURRENT_TIMESTAMP")
        .bind(state.catalog.vtl_id().await?.to_string())
        .bind(rotation.name())
        .bind(weekday)
        .bind(month_day)
        .bind(update.archive_after_days.map(i64::from))
        .bind(update.accept_archive_terms && update.archive_after_days.is_some())
        .bind(actor)
        .execute(&state.catalog.pool())
        .await
        .map_err(ApiError::conflict)?;
    Ok(())
}

pub async fn get(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    Ok(Json(view(&state).await?))
}

pub async fn put(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Json(update): Json<ScheduleUpdate>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    save(&state, &principal.user.username, update).await?;
    Ok(Json(view(&state).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::s3_settings::test_support::*;

    fn update(
        revision: i64,
        rotation: &str,
        weekday: Option<u8>,
        month_day: Option<u8>,
        archive_after_days: Option<u16>,
        accept_archive_terms: bool,
    ) -> ScheduleUpdate {
        ScheduleUpdate {
            revision,
            rotation: rotation.into(),
            weekday,
            month_day,
            archive_after_days,
            accept_archive_terms,
        }
    }

    #[tokio::test]
    async fn schedule_defaults_to_manual_and_validates_every_field() {
        let (state, root, _) = fixture().await;
        let value = view(&state).await.unwrap();
        assert_eq!(value["revision"], 0);
        assert_eq!(value["rotation"], "manual");
        assert_eq!(value["rotation_due"], false);
        assert_eq!(value["rotation_supported"], true);
        assert_eq!(value["active_bucket"], "current-bucket");
        for invalid in [
            update(0, "weekly", None, None, None, false),
            update(0, "weekly", Some(8), None, None, false),
            update(0, "monthly", None, Some(0), None, false),
            update(0, "manual", Some(1), None, None, false),
            update(0, "manual", None, None, Some(0), true),
            update(0, "manual", None, None, Some(366), true),
            update(0, "manual", None, None, Some(7), false),
        ] {
            assert!(save(&state, "admin", invalid).await.is_err());
        }
        save(
            &state,
            "admin",
            update(0, "weekly", Some(1), None, None, false),
        )
        .await
        .unwrap();
        let stale = save(
            &state,
            "admin",
            update(0, "monthly", None, Some(31), None, false),
        )
        .await
        .unwrap_err();
        assert_eq!(stale.status, StatusCode::CONFLICT);
        save(
            &state,
            "admin",
            update(1, "monthly", None, Some(31), None, false),
        )
        .await
        .unwrap();
        let value = view(&state).await.unwrap();
        assert_eq!(value["revision"], 2);
        assert_eq!(value["rotation"], "monthly");
        assert_eq!(value["month_day"], 31);
        assert_eq!(value["weekday"], Value::Null);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn scheduled_rotation_requires_an_ovh_connection() {
        let (state, root, mut config) = fixture().await;
        config.provider = Some(S3Provider::S3Compatible);
        config.endpoint = "https://s3.example.test".into();
        config.bucket.clear();
        sqlx::query("INSERT INTO s3_connections(vtl_id,revision,configuration) VALUES(?,2,?)")
            .bind(state.catalog.vtl_id().await.unwrap().to_string())
            .bind(serde_json::to_string(&config).unwrap())
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        let refused = save(
            &state,
            "admin",
            update(0, "weekly", Some(1), None, None, false),
        )
        .await
        .unwrap_err();
        assert!(refused.message.contains("OVH"), "{}", refused.message);
        save(
            &state,
            "admin",
            update(0, "manual", None, None, None, false),
        )
        .await
        .unwrap();
        assert_eq!(view(&state).await.unwrap()["rotation_supported"], false);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn archive_terms_are_accepted_once_and_kept() {
        let (state, root, _) = fixture().await;
        save(
            &state,
            "admin",
            update(0, "manual", None, None, Some(7), true),
        )
        .await
        .unwrap();
        let by: Option<String> =
            sqlx::query_scalar("SELECT archive_terms_accepted_by FROM storage_schedules")
                .fetch_one(&state.catalog.pool())
                .await
                .unwrap();
        assert_eq!(by.as_deref(), Some("admin"));
        let first: Option<String> =
            sqlx::query_scalar("SELECT archive_terms_accepted_at FROM storage_schedules")
                .fetch_one(&state.catalog.pool())
                .await
                .unwrap();
        sqlx::query("UPDATE storage_schedules SET archive_terms_accepted_at='2001-01-01 00:00:00'")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        save(
            &state,
            "other",
            update(1, "manual", None, None, Some(14), false),
        )
        .await
        .unwrap();
        save(
            &state,
            "other",
            update(2, "manual", None, None, None, false),
        )
        .await
        .unwrap();
        // Re-accepting (by another admin) keeps the first acceptance.
        save(
            &state,
            "third",
            update(3, "manual", None, None, Some(30), true),
        )
        .await
        .unwrap();
        let (at, by): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT archive_terms_accepted_at,archive_terms_accepted_by FROM storage_schedules",
        )
        .fetch_one(&state.catalog.pool())
        .await
        .unwrap();
        assert!(first.is_some());
        assert_eq!(at.as_deref(), Some("2001-01-01 00:00:00"));
        assert_eq!(by.as_deref(), Some("admin"));
        let value = view(&state).await.unwrap();
        assert_eq!(value["archive_after_days"], 30);
        assert_eq!(value["archive_terms_accepted"], true);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn enabling_automatic_archiving_records_when_it_started() {
        let (state, root, _) = fixture().await;
        let enabled = || async {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT archive_enabled_at FROM storage_schedules",
            )
            .fetch_one(&state.catalog.pool())
            .await
            .unwrap()
        };
        save(
            &state,
            "admin",
            update(0, "manual", None, None, None, false),
        )
        .await
        .unwrap();
        assert_eq!(enabled().await, None);
        save(
            &state,
            "admin",
            update(1, "manual", None, None, Some(7), true),
        )
        .await
        .unwrap();
        assert!(enabled().await.is_some());
        // Changing the delay keeps the start.
        sqlx::query("UPDATE storage_schedules SET archive_enabled_at='2001-01-01 00:00:00'")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        save(
            &state,
            "admin",
            update(2, "weekly", Some(1), None, Some(14), false),
        )
        .await
        .unwrap();
        assert_eq!(enabled().await.as_deref(), Some("2001-01-01 00:00:00"));
        // Disabling clears it; enabling again starts anew.
        save(
            &state,
            "admin",
            update(3, "weekly", Some(1), None, None, false),
        )
        .await
        .unwrap();
        assert_eq!(enabled().await, None);
        save(
            &state,
            "admin",
            update(4, "weekly", Some(1), None, Some(7), false),
        )
        .await
        .unwrap();
        let again = enabled().await.unwrap();
        assert_ne!(again, "2001-01-01 00:00:00");
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn accepting_terms_without_a_delay_stores_nothing() {
        let (state, root, _) = fixture().await;
        save(&state, "admin", update(0, "manual", None, None, None, true))
            .await
            .unwrap();
        let (at, by): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT archive_terms_accepted_at,archive_terms_accepted_by FROM storage_schedules",
        )
        .fetch_one(&state.catalog.pool())
        .await
        .unwrap();
        assert_eq!((at, by), (None, None));
        assert_eq!(view(&state).await.unwrap()["archive_terms_accepted"], false);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn automatic_archiving_requires_an_ovh_connection() {
        let (state, root, mut config) = fixture().await;
        config.provider = Some(S3Provider::S3Compatible);
        config.endpoint = "https://s3.example.test".into();
        sqlx::query("INSERT INTO s3_connections(vtl_id,revision,configuration) VALUES(?,2,?)")
            .bind(state.catalog.vtl_id().await.unwrap().to_string())
            .bind(serde_json::to_string(&config).unwrap())
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        let refused = save(
            &state,
            "admin",
            update(0, "manual", None, None, Some(7), true),
        )
        .await
        .unwrap_err();
        assert!(refused.message.contains("OVH"), "{}", refused.message);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn changing_the_rotation_serves_the_current_period() {
        let (state, root, _) = fixture().await;
        save(
            &state,
            "admin",
            update(0, "weekly", Some(1), None, None, false),
        )
        .await
        .unwrap();
        assert!(!rotation_due(&state).await.unwrap());
        sqlx::query("UPDATE storage_schedules SET rotation_served_at='2000-01-03 00:00:00'")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        assert!(rotation_due(&state).await.unwrap());
        // Only a change of the rotation itself serves the period.
        save(
            &state,
            "admin",
            update(1, "weekly", Some(1), None, Some(7), true),
        )
        .await
        .unwrap();
        assert!(rotation_due(&state).await.unwrap());
        let vtl = state.catalog.vtl_id().await.unwrap().to_string();
        sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id) VALUES('switch','bucket_create','vc-x','completed',?)")
            .bind(&vtl).execute(&state.catalog.pool()).await.unwrap();
        sqlx::query("INSERT INTO bucket_creations(job_id,vtl_id,next_bucket,configuration,revision,phase) VALUES('switch',?,'vc-x','{}',1,'switched')")
            .bind(&vtl).execute(&state.catalog.pool()).await.unwrap();
        assert!(!rotation_due(&state).await.unwrap());
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn concurrent_exports_create_a_single_bucket_for_the_period() {
        let (state, root, _) = fixture().await;
        private_root(&root);
        due_weekly(&state).await;
        creating_worker(&root, "1", None);
        // The second export arrives while the first one's creation is in flight.
        let second = async {
            first_call(&root).await;
            rotate_if_due(&state).await
        };
        let (first, second) = tokio::join!(rotate_if_due(&state), second);
        first.unwrap();
        second.unwrap();
        assert_eq!(switched_creations(&state).await, 1);
        assert_eq!(calls(&root), vec!["create-bucket-from-connection"]);
        let active = current(&state).await.unwrap().1.unwrap().bucket;
        assert!(active.starts_with("vc-vtl-principale-"), "{active}");
        assert!(!rotation_due(&state).await.unwrap());
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn rotation_survives_a_dropped_export_request() {
        let (state, root, _) = fixture().await;
        private_root(&root);
        due_weekly(&state).await;
        creating_worker(&root, "1", None);
        let caller = state.clone();
        let request = tokio::spawn(async move { rotate_if_due(&caller).await });
        // Drop the request once the provider mutation is in flight.
        first_call(&root).await;
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        // The next export waits for the running creation, then finds the period served.
        rotate_if_due(&state).await.unwrap();
        assert_eq!(switched_creations(&state).await, 1);
        assert_eq!(calls(&root), vec!["create-bucket-from-connection"]);
        let status: String =
            sqlx::query_scalar("SELECT status FROM jobs WHERE kind='bucket_create'")
                .fetch_one(&state.catalog.pool())
                .await
                .unwrap();
        assert_eq!(status, "completed");
        assert!(!rotation_due(&state).await.unwrap());
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn refused_period_creation_refuses_the_export_with_its_reason() {
        let (state, root, _) = fixture().await;
        private_root(&root);
        due_weekly(&state).await;
        creating_worker(&root, "0", Some("credentials"));
        let error = rotate_if_due(&state).await.unwrap_err();
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(
            error.message,
            "The new period's bucket was not created. The provider rejected the credentials. Fix the connection, then try again. The export was not started."
        );
        assert_eq!(
            current(&state).await.unwrap().1.unwrap().bucket,
            "current-bucket"
        );
        assert!(rotation_due(&state).await.unwrap());
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn manual_schedule_or_an_explicit_destination_never_rotates() {
        let (state, root, _) = fixture().await;
        private_root(&root);
        creating_worker(&root, "0", None);
        rotate_if_due(&state).await.unwrap();
        due_weekly(&state).await;
        let destination = state.catalog.destination().await.unwrap().id;
        let pinned = AppState {
            catalog: state.catalog.for_destination(destination).await.unwrap(),
            ..state.clone()
        };
        rotate_if_due(&pinned).await.unwrap();
        assert!(calls(&root).is_empty());
        assert_eq!(switched_creations(&state).await, 0);
        assert_eq!(
            current(&state).await.unwrap().1.unwrap().bucket,
            "current-bucket"
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    /// Wait (10 s at most) until the worker received its first command.
    async fn first_call(root: &std::path::Path) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while calls(root).is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the worker was never called");
    }

    #[tokio::test]
    async fn a_taken_period_name_is_retried_by_the_next_export() {
        let (state, root, _) = fixture().await;
        private_root(&root);
        due_weekly(&state).await;
        creating_worker(&root, "0", Some("exists"));
        let error = rotate_if_due(&state).await.unwrap_err();
        assert_eq!(
            error.message,
            "The new period's bucket was not created. This name is already taken at the provider. The next export will propose a new name. The export was not started."
        );
        let job: Option<String> =
            sqlx::query_scalar("SELECT message FROM jobs WHERE kind='bucket_create'")
                .fetch_one(&state.catalog.pool())
                .await
                .unwrap();
        assert_eq!(
            job.as_deref(),
            Some(
                "The bucket was not created. This name is already taken at the provider. The next export will propose a new name."
            )
        );
        assert!(rotation_due(&state).await.unwrap());
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn an_occupied_period_name_is_retried_by_the_next_export() {
        let (state, root, _) = fixture().await;
        private_root(&root);
        due_weekly(&state).await;
        // The creation loses its answer; the name exists and holds data.
        dispatch_worker(&root, "exists", "occupied");
        let error = rotate_if_due(&state).await.unwrap_err();
        let bucket: String = sqlx::query_scalar("SELECT next_bucket FROM bucket_creations")
            .fetch_one(&state.catalog.pool())
            .await
            .unwrap();
        assert_eq!(
            error.message,
            format!(
                "The new period's bucket could not be used. The bucket {bucket} already exists and contains data. It will not be used. The next export will propose a new name. The export was not started."
            )
        );
        assert!(rotation_due(&state).await.unwrap());
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn an_unconfirmed_period_creation_stays_to_be_checked() {
        let (state, root, _) = fixture().await;
        private_root(&root);
        due_weekly(&state).await;
        // Every command, the creation and its presence check, loses its answer.
        fake_worker(&root, "lost", 1);
        let error = rotate_if_due(&state).await.unwrap_err();
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(
            error.message,
            "The creation of the new period's bucket still has to be checked. The export was not started."
        );
        assert_eq!(
            current(&state).await.unwrap().1.unwrap().bucket,
            "current-bucket"
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn errors_before_the_period_creation_are_returned_as_is() {
        let (state, root, config) = fixture().await;
        private_root(&root);
        due_weekly(&state).await;
        creating_worker(&root, "0", None);
        reserve(&state, &config).await;
        let error = rotate_if_due(&state).await.unwrap_err();
        assert_eq!(
            error.message,
            "A bucket creation is still running or has to be checked."
        );
        assert!(calls(&root).is_empty());
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn an_impossible_export_creates_no_period_bucket() {
        let (state, root, _) = fixture().await;
        private_root(&root);
        due_weekly(&state).await;
        creating_worker(&root, "0", None);
        let Err(error) =
            crate::export_cartridge(State(state.clone()), axum::extract::Path("TAPE01".into()))
                .await
        else {
            panic!("an unknown cartridge was exported");
        };
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert!(calls(&root).is_empty());
        let creations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM bucket_creations")
            .fetch_one(&state.catalog.pool())
            .await
            .unwrap();
        assert_eq!(creations, 0);
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn a_period_bucket_that_cannot_be_activated_is_reported_as_unusable() {
        let (state, root, _) = fixture().await;
        private_root(&root);
        due_weekly(&state).await;
        held_lost_worker(&root);
        let caller = state.clone();
        let export = tokio::spawn(async move { rotate_if_due(&caller).await });
        first_call(&root).await;
        // The active destination changes while the creation is in flight.
        sqlx::query("UPDATE bucket_creations SET source_destination_id=NULL")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        std::fs::write(root.join("release"), "").unwrap();
        let error = tokio::time::timeout(Duration::from_secs(10), export)
            .await
            .expect("the export never answered")
            .unwrap()
            .unwrap_err();
        let bucket: String = sqlx::query_scalar("SELECT next_bucket FROM bucket_creations")
            .fetch_one(&state.catalog.pool())
            .await
            .unwrap();
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(
            error.message,
            format!(
                "The new period's bucket could not be used. The bucket {bucket} was created empty, but it was not activated. The active destination has changed. You can use it from the advanced options. The export was not started."
            )
        );
        assert_eq!(
            calls(&root),
            vec![
                "create-bucket-from-connection",
                "bucket-presence",
                "confirm-empty-bucket"
            ]
        );
        assert_eq!(
            current(&state).await.unwrap().1.unwrap().bucket,
            "current-bucket"
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }
}
