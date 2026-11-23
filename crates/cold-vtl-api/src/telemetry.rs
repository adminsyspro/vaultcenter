//! Bounded, passive cache reads. No tape access or subprocess on the HTTP path.
pub mod history;
use cold_vtl_core::VtlId;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, io::Read, os::unix::fs::OpenOptionsExt, path::Path};
const LIMIT: u64 = 1_048_576;

#[derive(Debug, Deserialize, Serialize)]
pub struct Telemetry {
    version: u8,
    vtl_id: String,
    generated_at: Option<i64>,
    observed_at: Option<i64>,
    source: String,
    status: String,
    reason: Option<String>,
    drives: Vec<Drive>,
    slots: Vec<Slot>,
    media: Vec<Media>,
}
#[derive(Debug, Deserialize, Serialize)]
struct Drive {
    id: String,
    label: String,
    barcode: Option<String>,
    origin_slot: Option<u32>,
    state: String,
    observed_at: Option<i64>,
    bytes_written: Option<u64>,
    write_rate_bps: Option<f64>,
    bandwidth_revision: Option<u64>,
    tape_write_limit_bps: Option<u64>,
    bandwidth_error: Option<bool>,
}
#[derive(Debug, Deserialize, Serialize)]
struct Slot {
    number: u32,
    kind: String,
    barcode: Option<String>,
    position_known: bool,
    observed_at: Option<i64>,
}
#[derive(Debug, Deserialize, Serialize)]
struct Media {
    barcode: String,
    observed_at: Option<i64>,
    local_size_bytes: Option<u64>,
    local_growth_bps: Option<f64>,
    logical_used_bytes: Option<u64>,
    logical_capacity_bytes: Option<u64>,
}
fn unavailable(id: VtlId, reason: &str) -> Telemetry {
    Telemetry {
        version: 1,
        vtl_id: id.to_string(),
        generated_at: None,
        observed_at: None,
        source: "mhvtl".into(),
        status: "unavailable".into(),
        reason: Some(reason.into()),
        drives: vec![],
        slots: vec![],
        media: vec![],
    }
}
fn timestamp(value: Option<i64>, now: i64) -> bool {
    value.is_none_or(|v| v >= 0 && v <= now.saturating_add(5000))
}
fn fresh(value: Option<i64>, now: i64, age: i64) -> bool {
    value.is_some_and(|v| now.saturating_sub(v) <= age)
}
fn barcode(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn rate(value: Option<f64>) -> bool {
    value.is_none_or(|v| v.is_finite() && (0.0..=9_007_199_254_740_991.0).contains(&v))
}
fn text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 120 && !value.chars().any(char::is_control)
}
fn validate(value: &Telemetry, id: VtlId, now: i64) -> bool {
    let mut drives = HashSet::new();
    let mut positions = HashSet::new();
    let mut media = HashSet::new();
    value.version == 1
        && value.vtl_id == id.to_string()
        && matches!(value.source.as_str(), "mhvtl" | "demo")
        && matches!(value.status.as_str(), "available" | "stale" | "unavailable")
        && value.reason.as_deref().is_none_or(|v| {
            matches!(
                v,
                "not_configured" | "source_missing" | "stale" | "invalid_snapshot"
            )
        })
        && timestamp(value.generated_at, now)
        && timestamp(value.observed_at, now)
        && value.drives.len() <= 64
        && value.slots.len() <= 4096
        && value.media.len() <= 1024
        && value.drives.iter().all(|d| {
            text(&d.id)
                && text(&d.label)
                && drives.insert(&d.id)
                && d.barcode.as_deref().is_none_or(barcode)
                && timestamp(d.observed_at, now)
                && rate(d.write_rate_bps)
                && matches!(
                    d.state.as_str(),
                    "empty"
                        | "loading"
                        | "loaded"
                        | "reading"
                        | "writing"
                        | "unloading"
                        | "idle"
                        | "maintenance"
                        | "error"
                        | "unknown"
                )
        })
        && value.slots.iter().all(|s| {
            s.number > 0
                && positions.insert((s.kind.as_str(), s.number))
                && matches!(s.kind.as_str(), "storage" | "import_export")
                && s.barcode.as_deref().is_none_or(barcode)
                && timestamp(s.observed_at, now)
        })
        && value.media.iter().all(|m| {
            barcode(&m.barcode)
                && media.insert(&m.barcode)
                && timestamp(m.observed_at, now)
                && rate(m.local_growth_bps)
        })
}
fn read(root: &Path, id: VtlId, now: i64) -> Telemetry {
    let path = root.join(format!("{id}.json"));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path);
    let Ok(file) = file else {
        return unavailable(
            id,
            if file
                .err()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                "source_missing"
            } else {
                "invalid_snapshot"
            },
        );
    };
    if !file
        .metadata()
        .is_ok_and(|m| m.is_file() && m.len() <= LIMIT)
    {
        return unavailable(id, "invalid_snapshot");
    }
    let mut bytes = Vec::new();
    if file.take(LIMIT + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > LIMIT {
        return unavailable(id, "invalid_snapshot");
    }
    let Ok(mut value) = serde_json::from_slice::<Telemetry>(&bytes) else {
        return unavailable(id, "invalid_snapshot");
    };
    if !validate(&value, id, now) {
        return unavailable(id, "invalid_snapshot");
    }
    if value.status == "unavailable" {
        return unavailable(id, value.reason.as_deref().unwrap_or("source_missing"));
    }
    let stale = value.status == "stale" || !fresh(value.generated_at, now, 10_000);
    if stale {
        value.status = "stale".into();
        value.reason = Some("stale".into());
    }
    for drive in &mut value.drives {
        if stale || !fresh(drive.observed_at, now, 15_000) {
            drive.state = "unknown".into();
            drive.barcode = None;
            drive.origin_slot = None;
            drive.bytes_written = None;
            drive.write_rate_bps = None;
            drive.bandwidth_revision = None;
            drive.tape_write_limit_bps = None;
            drive.bandwidth_error = None;
        }
    }
    for slot in &mut value.slots {
        if stale || !fresh(slot.observed_at, now, 15_000) {
            slot.position_known = false;
            slot.barcode = None;
        }
    }
    for media in &mut value.media {
        if stale || !fresh(media.observed_at, now, 10_000) {
            media.local_growth_bps = None;
            media.logical_used_bytes = None;
            media.logical_capacity_bytes = None;
        }
    }
    value
}
pub async fn read_snapshot(root: Option<&Path>, id: VtlId, now_ms: i64) -> Telemetry {
    let Some(root) = root else {
        return unavailable(id, "not_configured");
    };
    let root = root.to_owned();
    tokio::task::spawn_blocking(move || read(&root, id, now_ms))
        .await
        .unwrap_or_else(|_| unavailable(id, "invalid_snapshot"))
}
#[cfg(test)]
mod tests;
