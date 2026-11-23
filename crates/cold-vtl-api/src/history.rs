//! History of the S3 copies of a VTL: export, bucket archive and recall,
//! local deletion. Read from the catalogue only, never from S3.
use crate::{ApiError, AppState};
use axum::{
    Json,
    extract::{Query, State},
    http::header,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

/// Last known state of the bucket holding a copy, from its OVH requests.
#[derive(Serialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum BucketState {
    Available,
    Archiving,
    Archived,
    Recalling,
    Recalled,
    Unknown,
}

#[derive(Serialize, Debug)]
pub struct HistoryRow {
    pub barcode: String,
    pub generation: i64,
    pub bytes: Option<i64>,
    pub exported_at: String,
    pub destination_id: String,
    pub bucket: Option<String>,
    pub removed_at: Option<String>,
    /// `s3_deleted` when an S3 deletion completed, `catalogue` otherwise.
    pub removal: Option<&'static str>,
    pub bucket_state: BucketState,
    pub archived_at: Option<String>,
    pub recalled_at: Option<String>,
    pub readable_until: Option<String>,
    pub local_deleted_at: Option<String>,
}

/// The latest request that did not fail decides the state; a recall stays
/// readable for 30 days, then the bucket is archived again.
fn bucket_state(
    operation: Option<&str>,
    phase: Option<&str>,
    readable_until: Option<&str>,
    now: &str,
) -> BucketState {
    match (operation, phase) {
        (None, _) => BucketState::Available,
        (_, Some("uncertain")) => BucketState::Unknown,
        (Some("archive"), Some("queued" | "submitting" | "accepted")) => BucketState::Archiving,
        (Some("archive"), Some("archived")) => BucketState::Archived,
        (Some("recall"), Some("queued" | "submitting" | "accepted")) => BucketState::Recalling,
        (Some("recall"), Some("restored")) if readable_until.is_some_and(|until| until > now) => {
            BucketState::Recalled
        }
        (Some("recall"), Some("restored")) => BucketState::Archived,
        _ => BucketState::Unknown,
    }
}

pub async fn rows(pool: &SqlitePool, vtl_id: &str) -> Result<Vec<HistoryRow>, sqlx::Error> {
    let rows = sqlx::query(
        "WITH latest AS (SELECT destination_id,operation,phase,ROW_NUMBER() OVER (PARTITION BY destination_id ORDER BY updated_at DESC,rowid DESC) AS n
           FROM archive_requests WHERE vtl_id=?1 AND phase<>'failed'),
         archived AS (SELECT destination_id,MAX(archived_at) AS archived_at FROM archive_requests WHERE vtl_id=?1 AND operation='archive' GROUP BY destination_id),
         recalled AS (SELECT destination_id,MAX(restored_at) AS restored_at FROM archive_requests WHERE vtl_id=?1 AND operation='recall' GROUP BY destination_id)
         SELECT c.barcode,c.generation,c.bytes,c.recorded_at,c.destination_id,json_extract(d.identity,'$.bucket') AS bucket,c.removed_at,
          EXISTS(SELECT 1 FROM jobs j WHERE j.vtl_id=c.vtl_id AND j.kind='s3_delete' AND j.status='completed' AND j.destination_id=c.destination_id
           AND j.subject=c.barcode AND j.generation=c.generation) AS s3_deleted,
          l.operation,l.phase,a.archived_at,r.restored_at,datetime(r.restored_at,'+30 days') AS readable_until,x.deleted_at,CURRENT_TIMESTAMP AS now
         FROM copy_register c
         LEFT JOIN storage_destinations d ON d.vtl_id=c.vtl_id AND d.id=c.destination_id
         LEFT JOIN latest l ON l.destination_id=c.destination_id AND l.n=1
         LEFT JOIN archived a ON a.destination_id=c.destination_id
         LEFT JOIN recalled r ON r.destination_id=c.destination_id
         LEFT JOIN cartridge_deletions x ON x.vtl_id=c.vtl_id AND x.barcode=c.barcode
         WHERE c.vtl_id=?1 ORDER BY c.barcode,c.generation,c.recorded_at",
    )
    .bind(vtl_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|row| {
            let removed_at: Option<String> = row.get("removed_at");
            let readable_until: Option<String> = row.get("readable_until");
            let removal = removed_at.as_ref().map(|_| {
                if row.get::<bool, _>("s3_deleted") {
                    "s3_deleted"
                } else {
                    "catalogue"
                }
            });
            HistoryRow {
                barcode: row.get("barcode"),
                generation: row.get("generation"),
                bytes: row.get("bytes"),
                exported_at: row.get("recorded_at"),
                destination_id: row.get("destination_id"),
                bucket: row.get("bucket"),
                bucket_state: bucket_state(
                    row.get("operation"),
                    row.get("phase"),
                    readable_until.as_deref(),
                    row.get("now"),
                ),
                removed_at,
                removal,
                archived_at: row.get("archived_at"),
                recalled_at: row.get("restored_at"),
                readable_until,
                local_deleted_at: row.get("deleted_at"),
            }
        })
        .collect())
}

#[derive(Deserialize, Default)]
pub struct HistoryQuery {
    pub format: Option<String>,
}

/// `GET /v1/storage/history`: JSON by default, `?format=csv` for the register.
pub async fn history(
    State(state): State<AppState>,
    Query(query): Query<HistoryQuery>,
) -> Result<Response, ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    let pool = state.catalog.pool();
    let rows = rows(&pool, &vtl).await.map_err(ApiError::conflict)?;
    match query.format.as_deref() {
        None | Some("json") => Ok(Json(rows).into_response()),
        Some("csv") => {
            let (name, today): (String, String) =
                sqlx::query_as("SELECT name,date('now') FROM vtls WHERE id=?")
                    .bind(&vtl)
                    .fetch_one(&pool)
                    .await
                    .map_err(ApiError::conflict)?;
            let disposition = format!("attachment; filename=\"{}\"", csv_filename(&name, &today));
            Ok((
                [
                    (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
                    (header::CONTENT_DISPOSITION, disposition),
                ],
                csv(&rows),
            )
                .into_response())
        }
        Some(_) => Err(ApiError::bad_request("Unknown format: json or csv.")),
    }
}

fn state_label(state: BucketState) -> &'static str {
    match state {
        BucketState::Available => "Disponible",
        BucketState::Archiving => "Archivage en cours",
        BucketState::Archived => "Archivé",
        BucketState::Recalling => "Rappel en cours",
        BucketState::Recalled => "Rappelé",
        BucketState::Unknown => "À vérifier",
    }
}

/// SQLite `CURRENT_TIMESTAMP` is UTC without a zone: written as ISO 8601 UTC.
fn iso(value: &Option<String>) -> String {
    match value {
        Some(date) if date.len() == 19 && date.as_bytes()[10] == b' ' => {
            format!("{}T{}Z", &date[..10], &date[11..])
        }
        Some(other) => other.clone(),
        None => String::new(),
    }
}

fn field(value: &str) -> String {
    if value.contains([';', '"', '\r', '\n']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

/// UTF-8 with BOM and `;` separators, so Excel opens it as is in French.
fn csv(rows: &[HistoryRow]) -> String {
    let mut out = String::from(
        "\u{feff}Bande;Génération;Bucket;Taille (octets);Exportée le;État du bucket;Archivé le;Rappelé le;Lisible jusqu’au;Retirée le;Cause du retrait;Supprimée en local le\r\n",
    );
    for row in rows {
        let removal = match row.removal {
            Some("s3_deleted") => "Supprimée de S3",
            Some(_) => "Retirée du catalogue",
            None => "",
        };
        let fields = [
            row.barcode.clone(),
            row.generation.to_string(),
            row.bucket.clone().unwrap_or_default(),
            row.bytes.map(|b| b.to_string()).unwrap_or_default(),
            iso(&Some(row.exported_at.clone())),
            state_label(row.bucket_state).to_owned(),
            iso(&row.archived_at),
            iso(&row.recalled_at),
            iso(&row.readable_until),
            iso(&row.removed_at),
            removal.to_owned(),
            iso(&row.local_deleted_at),
        ];
        out.push_str(
            &fields
                .iter()
                .map(|f| field(f))
                .collect::<Vec<_>>()
                .join(";"),
        );
        out.push_str("\r\n");
    }
    out
}

/// `historique-<vtl>-<date>.csv`, with the VTL name reduced to `[a-z0-9-]`.
fn csv_filename(vtl_name: &str, today: &str) -> String {
    let mut name = String::new();
    for c in vtl_name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            name.push(c);
        } else if !name.ends_with('-') {
            name.push('-');
        }
    }
    let name = name.trim_matches('-');
    format!(
        "historique-{}-{today}.csv",
        if name.is_empty() { "vtl" } else { name }
    )
}

#[cfg(test)]
mod tests;
