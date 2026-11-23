//! Operations in progress and recently finished on every VTL, for the
//! header bell. Read from jobs and OVH requests; only "seen" is stored.
use crate::{ApiError, AppState, auth::Principal};
use axum::{Extension, Json, extract::State};
use serde::Serialize;
use sqlx::{Row, SqlitePool, sqlite::SqliteRow};

/// Job kinds shown to people; observations and cartridge creations are not.
macro_rules! kinds {
    () => {
        "'export','restore','recovery','recovery_install','reintegrate','rollback','s3_delete','reconcile','bucket_create','archive_submit','recall_submit'"
    };
}

#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Success,
    Failure,
    Uncertain,
}

#[derive(Serialize, Debug)]
pub struct ActiveItem {
    pub id: String,
    pub source: &'static str,
    pub kind: String,
    pub vtl_id: String,
    pub vtl_name: String,
    pub subject: String,
    pub destination_id: Option<String>,
    pub started_at: String,
    pub progress: Option<i64>,
}

#[derive(Serialize, Debug)]
pub struct RecentItem {
    pub id: String,
    pub source: &'static str,
    pub kind: String,
    pub vtl_id: String,
    pub vtl_name: String,
    pub subject: String,
    pub destination_id: Option<String>,
    pub outcome: Outcome,
    pub finished_at: String,
    pub unread: bool,
}

#[derive(Serialize, Debug)]
pub struct Activity {
    pub active: Vec<ActiveItem>,
    pub recent: Vec<RecentItem>,
    pub seen_at: Option<String>,
    pub unread: usize,
}

fn source(row: &SqliteRow) -> &'static str {
    if row.get::<String, _>("source") == "job" {
        "job"
    } else {
        "provider"
    }
}

/// An OVH request is listed once its submit job finished: a lost OVH answer
/// fails the job but leaves the request uncertain, and OVH may still archive
/// it. The job itself is listed while it runs, or when the request failed
/// with it.
pub async fn read(pool: &SqlitePool, user_id: &str) -> Result<Activity, sqlx::Error> {
    let active = sqlx::query(concat!(
        "WITH items AS (
          SELECT 'job:'||j.id AS id,'job' AS source,j.kind,j.vtl_id,j.subject,j.destination_id,j.created_at AS started_at,
           CASE WHEN j.status='running' THEN j.progress END AS progress
          FROM jobs j WHERE j.kind IN (",
        kinds!(),
        ") AND j.status IN ('queued','running')
          UNION ALL
          SELECT 'provider:'||a.job_id||':'||a.phase,'provider',a.operation,a.vtl_id,COALESCE(json_extract(a.request,'$.confirm_bucket'),''),
           a.destination_id,s.created_at,NULL
          FROM archive_requests a JOIN jobs s ON s.id=a.job_id AND s.status IN ('completed','failed')
          WHERE a.phase IN ('submitting','accepted'))
         SELECT i.*,v.name AS vtl_name FROM items i JOIN vtls v ON v.id=i.vtl_id
         ORDER BY i.started_at DESC,i.id DESC"
    ))
    .fetch_all(pool)
    .await?
    .iter()
    .map(|row| ActiveItem {
        id: row.get("id"),
        source: source(row),
        kind: row.get("kind"),
        vtl_id: row.get("vtl_id"),
        vtl_name: row.get("vtl_name"),
        subject: row.get("subject"),
        destination_id: row.get("destination_id"),
        started_at: row.get("started_at"),
        progress: row.get("progress"),
    })
    .collect();
    let seen_at: Option<String> =
        sqlx::query_scalar("SELECT seen_at FROM activity_seen WHERE user_id=?")
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    let recent: Vec<RecentItem> = sqlx::query(concat!(
        "WITH items AS (
          SELECT 'job:'||j.id AS id,'job' AS source,j.kind,j.vtl_id,j.subject,j.destination_id,
           CASE j.status WHEN 'completed' THEN 'success' ELSE 'failure' END AS outcome,j.updated_at AS finished_at
          FROM jobs j WHERE j.kind IN (",
        kinds!(),
        ") AND j.status IN ('completed','failed')
           AND NOT EXISTS(SELECT 1 FROM archive_requests a WHERE a.job_id=j.id AND NOT (a.phase='failed' AND j.status='failed'))
          UNION ALL
          SELECT 'provider:'||a.job_id||':'||a.phase,'provider',a.operation,a.vtl_id,COALESCE(json_extract(a.request,'$.confirm_bucket'),''),
           a.destination_id,CASE a.phase WHEN 'uncertain' THEN 'uncertain' WHEN 'failed' THEN 'failure' ELSE 'success' END,
           CASE a.phase WHEN 'archived' THEN COALESCE(a.archived_at,a.updated_at) WHEN 'restored' THEN COALESCE(a.restored_at,a.updated_at)
            WHEN 'uncertain' THEN s.updated_at ELSE a.updated_at END
          FROM archive_requests a JOIN jobs s ON s.id=a.job_id AND s.status IN ('completed','failed')
          WHERE a.phase IN ('archived','restored','uncertain') OR (a.phase='failed' AND s.status='completed'))
         SELECT i.*,v.name AS vtl_name,(?1 IS NULL OR i.finished_at>?1) AS unread FROM items i JOIN vtls v ON v.id=i.vtl_id
         WHERE i.finished_at>=datetime('now','-7 days')
         ORDER BY i.finished_at DESC,i.id DESC LIMIT 20"
    ))
    .bind(&seen_at)
    .fetch_all(pool)
    .await?
    .iter()
    .map(|row| RecentItem {
        id: row.get("id"),
        source: source(row),
        kind: row.get("kind"),
        vtl_id: row.get("vtl_id"),
        vtl_name: row.get("vtl_name"),
        subject: row.get("subject"),
        destination_id: row.get("destination_id"),
        outcome: match row.get::<String, _>("outcome").as_str() {
            "success" => Outcome::Success,
            "uncertain" => Outcome::Uncertain,
            _ => Outcome::Failure,
        },
        finished_at: row.get("finished_at"),
        unread: row.get("unread"),
    })
    .collect();
    let unread = recent.iter().filter(|item| item.unread).count();
    Ok(Activity {
        active,
        recent,
        seen_at,
        unread,
    })
}

/// Everything finished until now becomes read for this account.
pub async fn mark_seen(pool: &SqlitePool, user_id: &str) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO activity_seen(user_id,seen_at) VALUES(?,CURRENT_TIMESTAMP)
         ON CONFLICT(user_id) DO UPDATE SET seen_at=excluded.seen_at RETURNING seen_at",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await
}

pub async fn activity(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Activity>, ApiError> {
    read(&state.catalog.pool(), &principal.user.id)
        .await
        .map(Json)
        .map_err(ApiError::conflict)
}

pub async fn seen(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let seen_at = mark_seen(&state.catalog.pool(), &principal.user.id)
        .await
        .map_err(ApiError::conflict)?;
    Ok(Json(serde_json::json!({ "seen_at": seen_at })))
}

#[cfg(test)]
mod tests;
