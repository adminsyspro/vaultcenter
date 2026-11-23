//! Persisted samples belong to their observed VTL, drive, and barcode.
use crate::{ApiError, AppState, catalog::Catalog};
use cold_vtl_core::VtlId;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::path::Path;

const DAY_MS: i64 = 86_400_000;
const PRUNE_BATCH: i64 = 10_000;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuery {
    barcode: Option<String>,
    drive_id: Option<String>,
    kind: Option<String>,
    transfer_id: Option<String>,
    window_seconds: Option<u32>,
    end_ms: Option<i64>,
    /// One curve summing every series of the scope instead of one cartridge.
    aggregate: Option<bool>,
}
#[derive(Debug, Serialize)]
pub struct History {
    vtl_id: String,
    source: &'static str,
    status: &'static str,
    retention_days: u32,
    start_ms: i64,
    end_ms: i64,
    resolution_ms: i64,
    series: Vec<Series>,
    points: Vec<Point>,
}
#[derive(Debug, Serialize)]
struct Series {
    drive_id: String,
    barcode: String,
    last_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    transfer_id: Option<String>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Point {
    at: i64,
    write_rate: Option<f64>,
    local_growth: Option<f64>,
}

pub fn retention_days(value: Option<&str>) -> Result<u32, String> {
    match value {
        None => Ok(30),
        Some(value) => value
            .parse::<u32>()
            .ok()
            .filter(|days| (1..=90).contains(days))
            .ok_or_else(|| {
                "COLD_VTL_TELEMETRY_RETENTION_DAYS must be an integer between 1 and 90".into()
            }),
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// One independent sampling tick. A frozen snapshot keeps its original timestamp
/// and cannot gain repeated measurements, including after an API restart.
pub async fn collect_once(
    catalog: &Catalog,
    root: Option<&Path>,
    retention: u32,
    now: i64,
) -> Result<u64, sqlx::Error> {
    let pool = catalog.pool();
    let cutoff = now - i64::from(retention) * DAY_MS;
    sqlx::query("DELETE FROM telemetry_samples WHERE id IN (SELECT id FROM telemetry_samples WHERE at_ms<? ORDER BY at_ms LIMIT ?)")
        .bind(cutoff).bind(PRUNE_BATCH).execute(&pool).await?;
    sqlx::query("DELETE FROM s3_transfer_samples WHERE id IN (SELECT id FROM s3_transfer_samples WHERE at_ms<? ORDER BY at_ms LIMIT ?)")
        .bind(cutoff).bind(PRUNE_BATCH).execute(&pool).await?;
    let Some(root) = root else { return Ok(0) };
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM vtls ORDER BY id")
        .fetch_all(&pool)
        .await?;
    let mut added = 0;
    for raw_id in ids {
        let Ok(id) = raw_id.parse::<VtlId>() else {
            continue;
        };
        let snapshot = super::read_snapshot(Some(root), id, now).await;
        let Some(at) = snapshot.generated_at else {
            continue;
        };
        if snapshot.source != "mhvtl"
            || snapshot.status != "available"
            || at > now
            || !super::fresh(Some(at), now, 10_000)
        {
            continue;
        }
        let mut samples = Vec::new();
        for drive in &snapshot.drives {
            let Some(barcode) = drive.barcode.as_deref() else {
                continue;
            };
            let Some(observed) = drive.observed_at else {
                continue;
            };
            if !matches!(
                drive.state.as_str(),
                "loaded" | "idle" | "reading" | "writing" | "maintenance"
            ) || observed > at
                || !super::fresh(Some(observed), now, 15_000)
                || snapshot
                    .drives
                    .iter()
                    .filter(|other| other.barcode.as_deref() == Some(barcode))
                    .count()
                    != 1
            {
                continue;
            }
            let growth = snapshot
                .media
                .iter()
                .find(|media| media.barcode == barcode)
                .filter(|media| {
                    media
                        .observed_at
                        .is_some_and(|time| time >= observed && time <= at && now - time <= 10_000)
                })
                .and_then(|media| media.local_growth_bps);
            samples.push((&drive.id, barcode, drive.write_rate_bps, growth));
        }
        if samples.is_empty() {
            continue;
        }
        // At most 64 rows per snapshot; no file access while holding SQLite's writer lock.
        let mut tx = pool.begin().await?;
        for (drive, barcode, write, growth) in samples {
            added+=sqlx::query("INSERT INTO telemetry_samples(vtl_id,drive_id,barcode,at_ms,write_rate_bps,local_growth_bps) VALUES(?,?,?,?,?,?) ON CONFLICT(vtl_id,drive_id,barcode,at_ms) DO NOTHING")
                .bind(&raw_id).bind(drive).bind(barcode).bind(at).bind(write).bind(growth).execute(&mut *tx).await?.rows_affected();
        }
        tx.commit().await?;
    }
    Ok(added)
}

fn database_unavailable(_: sqlx::Error) -> ApiError {
    ApiError {
        status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
        message: "Throughput history is temporarily unavailable.".into(),
    }
}

pub async fn read_history(
    catalog: &Catalog,
    id: VtlId,
    retention: u32,
    query: HistoryQuery,
    now: i64,
) -> Result<History, ApiError> {
    let window = query.window_seconds.unwrap_or(120);
    let end = query.end_ms.unwrap_or(now);
    let kind = query.kind.as_deref().unwrap_or("pbs");
    let aggregate = query.aggregate == Some(true);
    if ![120, 300, 900, 3600, 21600, 86400, 604800, 2592000].contains(&window)
        || end < 0
        || end > now
        || !matches!(kind, "pbs" | "s3" | "s3_restore")
        || query
            .barcode
            .as_deref()
            .is_some_and(|value| !super::barcode(value))
        || query
            .drive_id
            .as_deref()
            .is_some_and(|value| !super::text(value))
        || query
            .transfer_id
            .as_deref()
            .is_some_and(|value| kind == "pbs" || !super::text(value))
        || (aggregate && (query.barcode.is_some() || query.transfer_id.is_some()))
    {
        return Err(ApiError::bad_request("Invalid history parameters."));
    }
    let window_ms = i64::from(window) * 1000;
    let start = end - window_ms;
    let resolution = 5000.max((window_ms + 599) / 600);
    let buckets = ((window_ms + resolution - 1) / resolution) as usize;
    let mut response = History {
        vtl_id: id.to_string(),
        source: match kind {
            "s3" => "s3",
            "s3_restore" => "s3_restore",
            _ => "mhvtl",
        },
        status: "unavailable",
        retention_days: retention,
        start_ms: start,
        end_ms: end,
        resolution_ms: resolution,
        series: vec![],
        points: vec![],
    };
    if query.barcode.is_some() || aggregate {
        response.points = (0..buckets)
            .map(|index| Point {
                at: start + index as i64 * resolution,
                write_rate: None,
                local_growth: None,
            })
            .collect();
    }
    if kind != "pbs" {
        return read_s3_history(catalog, query, response, now).await;
    }
    let pool = catalog.pool();
    let cutoff = now - i64::from(retention) * DAY_MS;
    // Inventory spans the retained history, so an unloaded cartridge remains selectable.
    // Bound the response independently of the number of samples in each series.
    let inventory=sqlx::query("SELECT drive_id,barcode,MAX(at_ms) AS last_at FROM telemetry_samples WHERE vtl_id=? AND at_ms>=? AND at_ms<=? AND (? IS NULL OR drive_id=?) GROUP BY drive_id,barcode ORDER BY last_at DESC,drive_id,barcode LIMIT 4096")
        .bind(&response.vtl_id).bind(cutoff).bind(end).bind(&query.drive_id).bind(&query.drive_id)
        .fetch_all(&pool).await.map_err(database_unavailable)?;
    response.series = inventory
        .iter()
        .map(|row| {
            Ok(Series {
                drive_id: row.try_get("drive_id")?,
                barcode: row.try_get("barcode")?,
                last_at: row.try_get("last_at")?,
                transfer_id: None,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(database_unavailable)?;
    if aggregate {
        // Each series keeps its own average per bucket, then the series add up.
        // SUM over only NULLs is NULL: a bucket nobody measured stays a gap.
        let sums=sqlx::query("SELECT bucket,SUM(write_rate) AS write_rate,SUM(local_growth) AS local_growth FROM (SELECT MIN((at_ms-?)/?,?) AS bucket,AVG(write_rate_bps) AS write_rate,AVG(local_growth_bps) AS local_growth FROM telemetry_samples WHERE vtl_id=? AND (? IS NULL OR drive_id=?) AND at_ms>=? AND at_ms<=? GROUP BY bucket,drive_id,barcode) GROUP BY bucket ORDER BY bucket")
            .bind(start).bind(resolution).bind((buckets-1) as i64).bind(&response.vtl_id).bind(&query.drive_id).bind(&query.drive_id).bind(start.max(cutoff)).bind(end)
            .fetch_all(&pool).await.map_err(database_unavailable)?;
        for row in sums {
            let index = row
                .try_get::<i64, _>("bucket")
                .map_err(database_unavailable)? as usize;
            if let Some(point) = response.points.get_mut(index) {
                point.write_rate = row.try_get("write_rate").map_err(database_unavailable)?;
                point.local_growth = row.try_get("local_growth").map_err(database_unavailable)?;
                if point.write_rate.is_some() || point.local_growth.is_some() {
                    response.status = "available";
                }
            }
        }
        return Ok(response);
    }
    let Some(barcode) = query.barcode.as_deref() else {
        if !response.series.is_empty() {
            response.status = "available";
        }
        return Ok(response);
    };
    let drive = if let Some(drive) = query.drive_id {
        Some(drive)
    } else {
        sqlx::query_scalar::<_,String>("SELECT drive_id FROM telemetry_samples WHERE vtl_id=? AND barcode=? AND at_ms>=? AND at_ms<=? ORDER BY at_ms DESC,drive_id LIMIT 1")
            .bind(&response.vtl_id).bind(barcode).bind(cutoff).bind(end).fetch_optional(&pool).await.map_err(database_unavailable)?
    };
    let Some(drive) = drive else {
        return Ok(response);
    };
    // SQLite performs the averages over indexed rows; only <=600 aggregate rows
    // cross the async boundary. Empty buckets remain explicit nulls.
    let aggregates=sqlx::query("SELECT MIN((at_ms-?)/?,?) AS bucket,AVG(write_rate_bps) AS write_rate,AVG(local_growth_bps) AS local_growth FROM telemetry_samples WHERE vtl_id=? AND drive_id=? AND barcode=? AND at_ms>=? AND at_ms<=? GROUP BY bucket ORDER BY bucket")
        .bind(start).bind(resolution).bind((buckets-1) as i64).bind(&response.vtl_id).bind(drive).bind(barcode).bind(start.max(cutoff)).bind(end)
        .fetch_all(&pool).await.map_err(database_unavailable)?;
    if !aggregates.is_empty() {
        response.status = "available";
    }
    for row in aggregates {
        let index = row
            .try_get::<i64, _>("bucket")
            .map_err(database_unavailable)? as usize;
        if let Some(point) = response.points.get_mut(index) {
            point.write_rate = row.try_get("write_rate").map_err(database_unavailable)?;
            point.local_growth = row.try_get("local_growth").map_err(database_unavailable)?;
        }
    }
    Ok(response)
}

async fn read_s3_history(
    catalog: &Catalog,
    query: HistoryQuery,
    mut response: History,
    now: i64,
) -> Result<History, ApiError> {
    let pool = catalog.pool();
    let cutoff = now - i64::from(response.retention_days) * DAY_MS;
    let download = query.kind.as_deref() == Some("s3_restore");
    let inventory = sqlx::query("SELECT barcode,transfer_id,MAX(at_ms) AS last_at FROM s3_transfer_samples WHERE vtl_id=? AND at_ms>=? AND at_ms<=? AND write_rate_bps IS NOT NULL AND (? IS NULL OR barcode=?) AND (? IS NULL OR transfer_id=?) AND EXISTS(SELECT 1 FROM jobs j WHERE j.id=s3_transfer_samples.transfer_id AND j.vtl_id=s3_transfer_samples.vtl_id AND ((? AND j.kind IN ('restore','recovery')) OR (NOT ? AND j.kind='export'))) GROUP BY barcode,transfer_id ORDER BY last_at DESC,transfer_id LIMIT 4096")
        .bind(&response.vtl_id).bind(cutoff).bind(response.end_ms).bind(&query.barcode).bind(&query.barcode).bind(&query.transfer_id).bind(&query.transfer_id).bind(download).bind(download)
        .fetch_all(&pool).await.map_err(database_unavailable)?;
    response.series = inventory
        .iter()
        .map(|row| {
            Ok(Series {
                drive_id: response.source.into(),
                barcode: row.try_get("barcode")?,
                last_at: row.try_get("last_at")?,
                transfer_id: Some(row.try_get("transfer_id")?),
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(database_unavailable)?;
    let rows = if query.aggregate == Some(true) {
        // Each transfer keeps its elapsed-weighted rate per bucket, then the
        // concurrent transfers of this kind add up.
        sqlx::query("SELECT bucket,SUM(rate) AS rate FROM (SELECT MIN((at_ms-?)/?,?) AS bucket,SUM(write_rate_bps*elapsed_ms)/SUM(CASE WHEN write_rate_bps IS NOT NULL THEN elapsed_ms END) AS rate FROM s3_transfer_samples WHERE vtl_id=? AND at_ms>=? AND at_ms<=? AND EXISTS(SELECT 1 FROM jobs j WHERE j.id=s3_transfer_samples.transfer_id AND j.vtl_id=s3_transfer_samples.vtl_id AND ((? AND j.kind IN ('restore','recovery')) OR (NOT ? AND j.kind='export'))) GROUP BY bucket,transfer_id) GROUP BY bucket ORDER BY bucket")
            .bind(response.start_ms).bind(response.resolution_ms).bind((response.points.len()-1) as i64).bind(&response.vtl_id).bind(response.start_ms.max(cutoff)).bind(response.end_ms).bind(download).bind(download)
            .fetch_all(&pool).await.map_err(database_unavailable)?
    } else {
        let Some(barcode) = query.barcode else {
            if !response.series.is_empty() {
                response.status = "available";
            }
            return Ok(response);
        };
        // A barcode's latest recorded export is the default; never blend export jobs.
        let transfer = query.transfer_id.or_else(|| {
            response
                .series
                .first()
                .and_then(|series| series.transfer_id.clone())
        });
        let Some(transfer) = transfer else {
            return Ok(response);
        };
        sqlx::query("SELECT MIN((at_ms-?)/?,?) AS bucket,SUM(write_rate_bps*elapsed_ms)/SUM(CASE WHEN write_rate_bps IS NOT NULL THEN elapsed_ms END) AS rate FROM s3_transfer_samples WHERE vtl_id=? AND barcode=? AND transfer_id=? AND at_ms>=? AND at_ms<=? AND EXISTS(SELECT 1 FROM jobs j WHERE j.id=s3_transfer_samples.transfer_id AND j.vtl_id=s3_transfer_samples.vtl_id AND ((? AND j.kind IN ('restore','recovery')) OR (NOT ? AND j.kind='export'))) GROUP BY bucket ORDER BY bucket")
        .bind(response.start_ms).bind(response.resolution_ms).bind((response.points.len()-1) as i64).bind(&response.vtl_id).bind(barcode).bind(transfer).bind(response.start_ms.max(cutoff)).bind(response.end_ms).bind(download).bind(download)
        .fetch_all(&pool).await.map_err(database_unavailable)?
    };
    for row in rows {
        let index = row
            .try_get::<i64, _>("bucket")
            .map_err(database_unavailable)? as usize;
        let rate = row.try_get("rate").map_err(database_unavailable)?;
        if let Some(point) = response.points.get_mut(index) {
            point.write_rate = rate;
            if rate.is_some() {
                response.status = "available";
            }
        }
    }
    Ok(response)
}

/// Launched once at API startup, never by a history read or browser connection.
pub fn monitor(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(5));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) = collect_once(
                &state.catalog,
                state.config.telemetry_dir.as_deref(),
                state.config.telemetry_retention_days,
                now_ms(),
            )
            .await
            {
                eprintln!("Telemetry history collection failed: {error}");
            }
        }
    })
}

#[cfg(test)]
mod tests;
