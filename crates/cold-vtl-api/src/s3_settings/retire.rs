//! Retire a bucket that OVH no longer knows: it leaves the bucket list but
//! stays in the catalogue for the history of its jobs and copies.
use super::{Configuration, buckets};
use crate::{ApiError, AppState};
use axum::{
    Json,
    extract::{Path, State},
};
use serde_json::{Value, json};

/// Work still bound to the bucket: a job, an OVH request or a bucket change.
macro_rules! busy {
    () => {
        "EXISTS(SELECT 1 FROM jobs WHERE destination_id=?1 AND status IN ('queued','running'))
         OR EXISTS(SELECT 1 FROM archive_requests WHERE destination_id=?1 AND phase IN ('queued','submitting','uncertain','accepted'))
         OR EXISTS(SELECT 1 FROM bucket_rotations WHERE (source_destination_id=?1 OR next_destination_id=?1) AND phase IN ('reserved','creating','ready','uncertain'))
         OR EXISTS(SELECT 1 FROM bucket_creations WHERE (source_destination_id=?1 OR next_destination_id=?1) AND phase IN ('reserved','creating','uncertain'))"
    };
}

/// Retired only when inactive, without any copy, idle, and when OVH confirms
/// at that moment that the bucket does not exist.
pub async fn retire(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    let pool = state.catalog.pool();
    type Row = (Option<String>, bool, bool, bool, Option<String>);
    let row: Option<Row> = sqlx::query_as(concat!(
        "SELECT json_extract(d.identity,'$.bucket'),d.active,
          EXISTS(SELECT 1 FROM remote_generations WHERE destination_id=?1),",
        busy!(),
        ",(SELECT configuration FROM storage_destination_revisions WHERE destination_id=?1 ORDER BY revision DESC LIMIT 1)
         FROM storage_destinations d WHERE d.id=?1 AND d.vtl_id=?2 AND d.identity IS NOT NULL AND d.retired_at IS NULL"
    ))
    .bind(&id)
    .bind(&vtl)
    .fetch_optional(&pool)
    .await
    .map_err(ApiError::conflict)?;
    let Some((Some(bucket), active, copies, busy, configuration)) = row else {
        return Err(ApiError::not_found("Bucket not found for this VTL."));
    };
    if active {
        return Err(ApiError::conflict("The active bucket cannot be removed."));
    }
    if copies {
        return Err(ApiError::conflict(
            "A copy is recorded in this bucket: it cannot be removed.",
        ));
    }
    if busy {
        return Err(ApiError::conflict(
            "An operation is in progress on this bucket. Try again when it has finished.",
        ));
    }
    let config: Option<Configuration> = configuration.and_then(|c| serde_json::from_str(&c).ok());
    let absent = match &config {
        Some(config) => buckets::confirmed_absent(&state, &bucket, config).await,
        None => false,
    };
    if !absent {
        return Err(ApiError::conflict(format!(
            "OVH does not confirm that the bucket {bucket} no longer exists. It was not removed."
        )));
    }
    // The same guards again, atomically: nothing may have started meanwhile.
    let changed = sqlx::query(concat!(
        "UPDATE storage_destinations SET retired_at=CURRENT_TIMESTAMP
         WHERE id=?1 AND vtl_id=?2 AND active=0 AND retired_at IS NULL
          AND NOT EXISTS(SELECT 1 FROM remote_generations WHERE destination_id=?1) AND NOT (",
        busy!(),
        ")"
    ))
    .bind(&id)
    .bind(&vtl)
    .execute(&pool)
    .await
    .map_err(ApiError::conflict)?;
    if changed.rows_affected() != 1 {
        return Err(ApiError::conflict(
            "The bucket's state changed. Refresh the storage, then try again.",
        ));
    }
    Ok(Json(json!({"destination_id":id,"bucket":bucket})))
}

#[cfg(test)]
mod tests;
