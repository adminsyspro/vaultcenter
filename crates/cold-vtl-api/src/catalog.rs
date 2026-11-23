mod destinations;
mod vtls;
use cold_vtl_core::{Barcode, Cartridge, CartridgeState, RemoteGeneration};
use serde::Serialize;
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::path::Path;
use thiserror::Error;

#[derive(Clone)]
pub struct Catalog {
    pool: SqlitePool,
    vtl_id: Option<cold_vtl_core::VtlId>,
    destination_id: Option<String>,
    database_path: std::path::PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub destination_id: Option<String>,
    pub configuration_revision: Option<i64>,
    pub generation: Option<i64>,
    pub destination: Option<String>,
    pub subject: String,
    pub status: String,
    pub progress: i64,
    pub message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct CartridgeView {
    #[serde(flatten)]
    cartridge: Cartridge,
    export_resumable: bool,
    local_deleted: bool,
    /// Its synchronisation with the active bucket is still to run or running.
    reconcile_pending: bool,
    /// Most recent verified S3 copy, all destinations included.
    last_copy: Option<serde_json::Value>,
}

impl Catalog {
    pub fn state_dir(&self) -> &Path {
        self.database_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
    }

    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    /// Whether this view targets one explicit destination instead of the active one.
    pub fn pinned(&self) -> bool {
        self.destination_id.is_some()
    }

    pub async fn open(path: &Path) -> Result<Self, CatalogError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        sqlx::query("PRAGMA journal_mode = WAL")
            .execute(&pool)
            .await?;
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        sqlx::query(
            "UPDATE jobs SET status = 'failed', message = 'daemon restarted while job was active', updated_at = CURRENT_TIMESTAMP WHERE status IN ('queued', 'running') AND kind NOT IN ('recovery','recovery_install')",
        )
        .execute(&pool)
        .await?;
        sqlx::query("UPDATE cartridges SET state = 'failed' WHERE state = 'uploading'")
            .execute(&pool)
            .await?;
        Ok(Self {
            pool,
            vtl_id: None,
            destination_id: None,
            database_path: path.to_owned(),
        })
    }

    pub async fn discover(
        &self,
        media_root: &Path,
        capacity_bytes: u64,
    ) -> Result<usize, CatalogError> {
        let vtl_id = self.resolve_vtl(self.vtl_id).await?.to_string();
        let mut discovered = 0;
        if !media_root.is_dir() {
            return Ok(0);
        }

        for entry in std::fs::read_dir(media_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() || !entry.path().join("mam").is_file() {
                continue;
            }
            // The controller may have placed the media before committing its settings.
            // Any marker, including a dangling symlink, reserves it for exact recovery.
            match std::fs::symlink_metadata(entry.path().join(".vaultcenter-create-pending")) {
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            let Ok(barcode) = Barcode::parse(name) else {
                continue;
            };
            let bytes_written = directory_size(&entry.path())?;
            let result = sqlx::query(
                "INSERT INTO cartridges (vtl_id,barcode, capacity_bytes, bytes_written, generation, state) VALUES (?, ?, ?, ?, 1, 'writable') ON CONFLICT(vtl_id,barcode) DO NOTHING",
            )
            .bind(&vtl_id)
            .bind(barcode.as_str())
            .bind(as_i64(capacity_bytes)?)
            .bind(as_i64(bytes_written)?)
            .execute(&self.pool)
            .await?;
            discovered += result.rows_affected() as usize;
        }
        Ok(discovered)
    }

    pub async fn list_cartridges(&self) -> Result<Vec<Cartridge>, CatalogError> {
        let vtl_id = self.resolve_vtl(self.vtl_id).await?.to_string();
        let rows = sqlx::query(
            "SELECT barcode, capacity_bytes, bytes_written, generation, state, media_type, segment_size_mib, multipart_part_size_mib FROM cartridges WHERE vtl_id=? AND NOT EXISTS(SELECT 1 FROM cartridge_deletions x WHERE x.vtl_id=cartridges.vtl_id AND x.barcode=cartridges.barcode) ORDER BY barcode",
        )
        .bind(&vtl_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(cartridge_from_row).collect()
    }

    /// UI eligibility uses full cartridge history, independently of the bounded
    /// activity feed. Reservation repeats the check atomically before any write.
    pub async fn cartridge_views(
        &self,
        barcode: Option<&Barcode>,
    ) -> Result<Vec<CartridgeView>, CatalogError> {
        let vtl = self.vtl_id().await?.to_string();
        let rows=sqlx::query("SELECT c.*, EXISTS(SELECT 1 FROM cartridge_deletions x WHERE x.vtl_id=c.vtl_id AND x.barcode=c.barcode) AS local_deleted, COALESCE(c.state='failed' AND (SELECT j.kind='export' AND j.status='failed' AND j.generation=c.generation AND EXISTS(SELECT 1 FROM storage_destinations d WHERE d.vtl_id=c.vtl_id AND d.active=1 AND d.id=j.destination_id AND (? IS NULL OR d.id=?) AND j.configuration_revision IS (SELECT MAX(revision) FROM storage_destination_revisions WHERE destination_id=d.id)) FROM jobs j WHERE j.vtl_id=c.vtl_id AND j.cartridge_id=c.id ORDER BY j.created_at DESC,j.rowid DESC LIMIT 1),0) AS export_resumable, EXISTS(SELECT 1 FROM s3_reconcile_required r WHERE r.vtl_id=c.vtl_id AND r.barcode=c.barcode) AS reconcile_pending, (SELECT json_object('bucket',json_extract(d.identity,'$.bucket'),'generation',g.generation,'checked_at',g.checked_at) FROM remote_generations g JOIN storage_destinations d ON d.vtl_id=g.vtl_id AND d.id=g.destination_id WHERE g.vtl_id=c.vtl_id AND g.barcode=c.barcode ORDER BY g.checked_at DESC,g.generation DESC LIMIT 1) AS last_copy FROM cartridges c WHERE c.vtl_id=? AND (? IS NULL OR c.barcode=?) ORDER BY c.barcode")
            .bind(&self.destination_id).bind(&self.destination_id).bind(vtl)
            .bind(barcode.map(Barcode::as_str)).bind(barcode.map(Barcode::as_str))
            .fetch_all(&self.pool).await?;
        rows.iter()
            .map(|row| {
                Ok(CartridgeView {
                    cartridge: cartridge_from_row(row)?,
                    export_resumable: row.try_get::<bool, _>("export_resumable")?
                        && !row.try_get::<bool, _>("local_deleted")?,
                    local_deleted: row.try_get("local_deleted")?,
                    reconcile_pending: row.try_get("reconcile_pending")?,
                    last_copy: row
                        .try_get::<Option<String>, _>("last_copy")?
                        .map(|value| serde_json::from_str(&value))
                        .transpose()?,
                })
            })
            .collect()
    }

    pub async fn local_deleted(&self, barcode: &str) -> Result<bool, CatalogError> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM cartridge_deletions WHERE vtl_id=? AND barcode=?)",
        )
        .bind(self.vtl_id().await?.to_string())
        .bind(barcode)
        .fetch_one(&self.pool)
        .await?)
    }

    pub async fn cartridge(&self, barcode: &Barcode) -> Result<Option<Cartridge>, CatalogError> {
        let vtl_id = self.resolve_vtl(self.vtl_id).await?.to_string();
        let row = sqlx::query(
            "SELECT barcode, capacity_bytes, bytes_written, generation, state, media_type, segment_size_mib, multipart_part_size_mib FROM cartridges WHERE barcode = ? AND vtl_id=?",
        )
        .bind(barcode.as_str())
        .bind(&vtl_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(cartridge_from_row).transpose()
    }

    #[cfg(test)]
    pub async fn create_cartridge(&self, cartridge: &Cartridge) -> Result<(), CatalogError> {
        let vtl_id = self.resolve_vtl(self.vtl_id).await?.to_string();
        sqlx::query(
            "INSERT INTO cartridges (vtl_id,barcode, capacity_bytes, bytes_written, generation, state, media_type, segment_size_mib, multipart_part_size_mib) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&vtl_id)
        .bind(cartridge.barcode.as_str())
        .bind(as_i64(cartridge.capacity_bytes)?)
        .bind(as_i64(cartridge.bytes_written)?)
        .bind(as_i64(cartridge.generation)?)
        .bind(state_name(cartridge.state)?)
        .bind(&cartridge.media_type)
        .bind(i64::from(cartridge.segment_size_mib))
        .bind(i64::from(cartridge.multipart_part_size_mib))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_cartridge(&self, cartridge: &Cartridge) -> Result<(), CatalogError> {
        let vtl_id = self.resolve_vtl(self.vtl_id).await?.to_string();
        sqlx::query(
            "UPDATE cartridges SET capacity_bytes = ?, bytes_written = ?, generation = ?, state = ?, updated_at = CURRENT_TIMESTAMP WHERE barcode = ? AND vtl_id=?",
        )
        .bind(as_i64(cartridge.capacity_bytes)?)
        .bind(as_i64(cartridge.bytes_written)?)
        .bind(as_i64(cartridge.generation)?)
        .bind(state_name(cartridge.state)?)
        .bind(cartridge.barcode.as_str())
        .bind(&vtl_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn has_active_job(&self, barcode: &Barcode) -> Result<bool, CatalogError> {
        let vtl_id = self.resolve_vtl(self.vtl_id).await?.to_string();
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM jobs WHERE subject = ? AND vtl_id=? AND status IN ('queued','running')",
        )
        .bind(barcode.as_str())
        .bind(&vtl_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(count != 0)
    }

    pub async fn reserve_job(
        &self,
        id: &str,
        kind: &str,
        cartridge: &Cartridge,
        generation: Option<u64>,
        destination: Option<&str>,
    ) -> Result<Job, CatalogError> {
        let vtl_id = self.resolve_vtl(self.vtl_id).await?.to_string();
        let pin = self.destination().await?;
        if kind == "export" && (!pin.active || generation != Some(cartridge.generation)) {
            return Err(CatalogError::Conflict);
        }
        // Write transaction from its start: a job finishing meanwhile (another
        // connection) makes it wait instead of failing with "database is locked".
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let cartridge_id: String =
            sqlx::query_scalar("SELECT id FROM cartridges WHERE vtl_id=? AND barcode=?")
                .bind(&vtl_id)
                .bind(cartridge.barcode.as_str())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(CatalogError::Conflict)?;
        if kind == "export" {
            // Retry only the most recent operation's failed export with the same
            // generation and destination pin. Do not reinterpret failed restores
            // or archive operations as sealed media. State + reservation commit together.
            let updated=sqlx::query("UPDATE cartridges SET state='uploading', bytes_written=?, updated_at=CURRENT_TIMESTAMP WHERE barcode=? AND generation=? AND vtl_id=? AND (state IN ('writable','sealed') OR (state='failed' AND (SELECT kind='export' AND status='failed' AND generation=? AND destination_id=? AND configuration_revision IS ? FROM jobs WHERE vtl_id=? AND cartridge_id=? ORDER BY created_at DESC,rowid DESC LIMIT 1)))")
                .bind(as_i64(cartridge.bytes_written)?).bind(cartridge.barcode.as_str()).bind(as_i64(cartridge.generation)?).bind(&vtl_id)
                .bind(as_i64(cartridge.generation)?).bind(&pin.id).bind(pin.revision).bind(&vtl_id).bind(&cartridge_id)
                .execute(&mut *tx).await?;
            if updated.rows_affected() != 1 {
                return Err(CatalogError::Conflict);
            }
        }
        sqlx::query("INSERT INTO jobs (vtl_id, cartridge_id, id, kind, subject, status, progress, generation, destination, destination_id, configuration_revision) VALUES (?, ?, ?, ?, ?, 'queued', 0, ?, ?, ?, ?)")
            .bind(&vtl_id).bind(&cartridge_id).bind(id).bind(kind).bind(cartridge.barcode.as_str()).bind(generation.map(as_i64).transpose()?).bind(destination).bind(&pin.id).bind(pin.revision)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        self.job(id).await?.ok_or(CatalogError::MissingJob)
    }

    pub async fn generations(
        &self,
        barcode: &Barcode,
    ) -> Result<Vec<RemoteGeneration>, CatalogError> {
        let vtl_id = self.resolve_vtl(self.vtl_id).await?.to_string();
        let values: Vec<String> = sqlx::query_scalar(
            "SELECT metadata FROM remote_generations WHERE barcode=? AND vtl_id=? AND destination_id=? ORDER BY generation DESC",
        )
        .bind(barcode.as_str())
        .bind(&vtl_id)
        .bind(self.destination().await?.id)
        .fetch_all(&self.pool)
        .await?;
        let deleted = self.local_deleted(barcode.as_str()).await?;
        values
            .into_iter()
            .map(|value| {
                let mut generation: RemoteGeneration =
                    serde_json::from_str(&value).map_err(CatalogError::State)?;
                if deleted {
                    generation.local_match = false;
                }
                Ok(generation)
            })
            .collect()
    }

    pub async fn finish_job(
        &self,
        id: &str,
        status: &str,
        message: &str,
        cartridge: Option<&Cartridge>,
        generations: Option<&[RemoteGeneration]>,
    ) -> Result<(), CatalogError> {
        self.finish(id, status, message, cartridge, generations, false, true)
            .await
            .map(|_| ())
    }

    /// `finish_job` for a job still queued or running only: a job closed
    /// meanwhile (a synchronisation cancelled by a destination switch) is
    /// left as is, nothing is written and `false` is returned.
    pub async fn finish_open_job(
        &self,
        id: &str,
        status: &str,
        message: &str,
        cartridge: Option<&Cartridge>,
        generations: Option<&[RemoteGeneration]>,
    ) -> Result<bool, CatalogError> {
        self.finish(id, status, message, cartridge, generations, true, true)
            .await
    }

    /// A worker receipt describes one copy, not a complete remote inventory.
    pub async fn finish_verified_export(
        &self,
        id: &str,
        message: &str,
        cartridge: &Cartridge,
        generation: &RemoteGeneration,
    ) -> Result<(), CatalogError> {
        self.finish(
            id,
            "completed",
            message,
            Some(cartridge),
            Some(std::slice::from_ref(generation)),
            true,
            false,
        )
        .await
        .map(|_| ())
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish(
        &self,
        id: &str,
        status: &str,
        message: &str,
        cartridge: Option<&Cartridge>,
        generations: Option<&[RemoteGeneration]>,
        open_only: bool,
        replace_generations: bool,
    ) -> Result<bool, CatalogError> {
        let vtl_id = self.resolve_vtl(self.vtl_id).await?.to_string();
        // Like `reserve_job`: concurrent jobs end without "database is locked".
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let (subject, current): (String, String) =
            sqlx::query_as("SELECT subject,status FROM jobs WHERE id=? AND vtl_id=?")
                .bind(id)
                .bind(&vtl_id)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(CatalogError::MissingJob)?;
        if open_only && !matches!(current.as_str(), "queued" | "running") {
            return Ok(false);
        }
        let (destination_id, active): (Option<String>, bool) = sqlx::query_as("SELECT j.destination_id,COALESCE(d.active,0) FROM jobs j LEFT JOIN storage_destinations d ON d.id=j.destination_id AND d.vtl_id=j.vtl_id WHERE j.id=? AND j.vtl_id=?").bind(id).bind(&vtl_id).fetch_one(&mut *tx).await?;
        if let Some(cartridge) = cartridge {
            if subject != cartridge.barcode.as_str() {
                return Err(CatalogError::Conflict);
            }
            if active {
                sqlx::query("UPDATE cartridges SET bytes_written=?, generation=?, state=?, updated_at=CURRENT_TIMESTAMP WHERE barcode=? AND vtl_id=?")
                .bind(as_i64(cartridge.bytes_written)?).bind(as_i64(cartridge.generation)?).bind(state_name(cartridge.state)?).bind(cartridge.barcode.as_str()).bind(&vtl_id)
                .execute(&mut *tx).await?;
            }
            if let Some(generations) = generations {
                let destination_id = destination_id.as_ref().ok_or(CatalogError::Conflict)?;
                if active {
                    sqlx::query("DELETE FROM s3_reconcile_required WHERE barcode=? AND vtl_id=?")
                        .bind(cartridge.barcode.as_str())
                        .bind(&vtl_id)
                        .execute(&mut *tx)
                        .await?;
                }
                if replace_generations {
                    sqlx::query("DELETE FROM remote_generations WHERE barcode=? AND vtl_id=? AND destination_id=?")
                    .bind(cartridge.barcode.as_str())
                    .bind(&vtl_id).bind(destination_id)
                    .execute(&mut *tx)
                    .await?;
                } else {
                    // Prior copies remain restorable, but are not evidence about this source.
                    sqlx::query("UPDATE remote_generations SET metadata=json_set(metadata,'$.local_match',json('false')) WHERE barcode=? AND vtl_id=? AND destination_id=?")
                        .bind(cartridge.barcode.as_str()).bind(&vtl_id).bind(destination_id).execute(&mut *tx).await?;
                }
                for generation in generations {
                    sqlx::query("INSERT INTO remote_generations (vtl_id,destination_id,barcode,generation,metadata,cartridge_id) VALUES (?,?,?,?,?,(SELECT id FROM cartridges WHERE vtl_id=? AND barcode=?)) ON CONFLICT(cartridge_id,destination_id,generation) DO UPDATE SET metadata=excluded.metadata,cartridge_id=excluded.cartridge_id,checked_at=CURRENT_TIMESTAMP")
                        .bind(&vtl_id).bind(destination_id).bind(cartridge.barcode.as_str()).bind(as_i64(generation.generation)?).bind(serde_json::to_string(generation)?).bind(&vtl_id).bind(cartridge.barcode.as_str()).execute(&mut *tx).await?;
                }
            }
        }
        sqlx::query("UPDATE jobs SET status=?, progress=100, message=?, updated_at=CURRENT_TIMESTAMP WHERE id=? AND vtl_id=?")
            .bind(status).bind(message).bind(id).bind(&vtl_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// `update_job` for a job still queued or running only; `false` when it
    /// was closed meanwhile and is left as is.
    pub async fn update_open_job(
        &self,
        id: &str,
        status: &str,
        progress: i64,
        message: Option<&str>,
    ) -> Result<bool, CatalogError> {
        let vtl_id = self.resolve_vtl(self.vtl_id).await?.to_string();
        let updated = sqlx::query(
            "UPDATE jobs SET status = ?, progress = ?, message = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ? AND vtl_id=? AND status IN ('queued','running')",
        )
        .bind(status)
        .bind(progress)
        .bind(message)
        .bind(id)
        .bind(&vtl_id)
        .execute(&self.pool)
        .await?;
        Ok(updated.rows_affected() == 1)
    }

    pub async fn update_job(
        &self,
        id: &str,
        status: &str,
        progress: i64,
        message: Option<&str>,
    ) -> Result<(), CatalogError> {
        let vtl_id = self.resolve_vtl(self.vtl_id).await?.to_string();
        let updated = sqlx::query(
            "UPDATE jobs SET status = ?, progress = ?, message = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ? AND vtl_id=?",
        )
        .bind(status)
        .bind(progress)
        .bind(message)
        .bind(id)
        .bind(&vtl_id)
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(CatalogError::MissingJob);
        }
        Ok(())
    }

    pub async fn list_jobs(&self) -> Result<Vec<Job>, CatalogError> {
        self.filtered_jobs(None, None).await
    }

    pub async fn filtered_jobs(
        &self,
        barcode: Option<&Barcode>,
        kind: Option<&str>,
    ) -> Result<Vec<Job>, CatalogError> {
        let vtl_id = self.resolve_vtl(self.vtl_id).await?.to_string();
        let rows = sqlx::query(
            "SELECT * FROM jobs WHERE vtl_id=? AND (? IS NULL OR subject=?) AND (? IS NULL OR kind=?) ORDER BY created_at DESC,rowid DESC LIMIT 100",
        )
        .bind(&vtl_id)
        .bind(barcode.map(Barcode::as_str)).bind(barcode.map(Barcode::as_str))
        .bind(kind).bind(kind)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(job_from_row).collect()
    }

    pub async fn job(&self, id: &str) -> Result<Option<Job>, CatalogError> {
        let vtl_id = self.resolve_vtl(self.vtl_id).await?.to_string();
        let row = sqlx::query("SELECT * FROM jobs WHERE id = ? AND vtl_id=?")
            .bind(id)
            .bind(&vtl_id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(job_from_row).transpose()
    }
}

fn cartridge_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Cartridge, CatalogError> {
    let state: String = row.try_get("state")?;
    Ok(Cartridge {
        barcode: Barcode::parse(row.try_get::<String, _>("barcode")?)?,
        media_type: row.try_get("media_type")?,
        segment_size_mib: u32::try_from(row.try_get::<i64, _>("segment_size_mib")?)
            .map_err(|_| CatalogError::NumericRange)?,
        multipart_part_size_mib: u32::try_from(row.try_get::<i64, _>("multipart_part_size_mib")?)
            .map_err(|_| CatalogError::NumericRange)?,
        capacity_bytes: as_u64(row.try_get("capacity_bytes")?)?,
        bytes_written: as_u64(row.try_get("bytes_written")?)?,
        generation: as_u64(row.try_get("generation")?)?,
        state: serde_json::from_str(&format!("\"{state}\""))?,
    })
}

fn job_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Job, CatalogError> {
    Ok(Job {
        id: row.try_get("id")?,
        kind: row.try_get("kind")?,
        destination_id: row.try_get("destination_id")?,
        configuration_revision: row.try_get("configuration_revision")?,
        generation: row.try_get("generation")?,
        destination: row.try_get("destination")?,
        subject: row.try_get("subject")?,
        status: row.try_get("status")?,
        progress: row.try_get("progress")?,
        message: row.try_get("message")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn state_name(state: CartridgeState) -> Result<String, serde_json::Error> {
    serde_json::to_string(&state).map(|value| value.trim_matches('"').to_owned())
}

pub(crate) fn directory_size(path: &Path) -> Result<u64, std::io::Error> {
    let mut size = 0_u64;
    for entry in walk_directory(path)? {
        if entry.is_file() {
            size = size.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(size)
}

fn walk_directory(path: &Path) -> Result<Vec<std::path::PathBuf>, std::io::Error> {
    let mut pending = vec![path.to_owned()];
    let mut entries = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                pending.push(path.clone());
            }
            entries.push(path);
        }
    }
    Ok(entries)
}

fn as_i64(value: u64) -> Result<i64, CatalogError> {
    i64::try_from(value).map_err(|_| CatalogError::NumericRange)
}

fn as_u64(value: i64) -> Result<u64, CatalogError> {
    u64::try_from(value).map_err(|_| CatalogError::NumericRange)
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("VTL roots are invalid or differ from the recorded media association")]
    InvalidRoots,
    #[error("explicit VTL selection is required")]
    AmbiguousVtl,
    #[error("VTL does not exist")]
    MissingVtl,
    #[error("invalid VTL identifier: {0}")]
    InvalidVtlId(uuid::Error),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid barcode in database: {0}")]
    Barcode(#[from] cold_vtl_core::BarcodeError),
    #[error("invalid state in database: {0}")]
    State(#[from] serde_json::Error),
    #[error("numeric value is outside SQLite range")]
    NumericRange,
    #[error("newly created job could not be read")]
    MissingJob,
    #[error("cartridge state changed during job creation")]
    Conflict,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn local_deletion_hides_inventory_but_keeps_identity_and_s3_metadata() {
        let root = std::env::temp_dir().join(format!("deletion-{}", uuid::Uuid::new_v4()));
        let catalog = Catalog::open(&root.join("catalog.db")).await.unwrap();
        let tape = Cartridge::new(Barcode::parse("DELETE01").unwrap(), 1024).unwrap();
        catalog.create_cartridge(&tape).await.unwrap();
        let vtl = catalog.vtl_id().await.unwrap().to_string();
        let destination = catalog.destination().await.unwrap().id;
        let metadata = serde_json::json!({"barcode":"DELETE01","generation":7,"format_version":2,"logical_size_bytes":1024,"file_count":5,"segment_count":1,"local_match":true,"manifest_sha256":"checksum","manifest_key":"cartridges/DELETE01/manifests/7.json"}).to_string();
        sqlx::query("INSERT INTO remote_generations(vtl_id,destination_id,barcode,generation,metadata,cartridge_id) SELECT vtl_id,?,barcode,7,?,id FROM cartridges WHERE vtl_id=? AND barcode='DELETE01'")
            .bind(destination).bind(&metadata).bind(&vtl).execute(&catalog.pool()).await.unwrap();
        assert!(catalog.generations(&tape.barcode).await.unwrap()[0].local_match);
        sqlx::query("INSERT INTO cartridge_deletions(vtl_id,barcode,request_id) VALUES(?,'DELETE01','request')")
            .bind(&vtl).execute(&catalog.pool()).await.unwrap();
        assert!(catalog.list_cartridges().await.unwrap().is_empty());
        assert!(catalog.cartridge(&tape.barcode).await.unwrap().is_some());
        let views = serde_json::to_value(catalog.cartridge_views(None).await.unwrap()).unwrap();
        assert_eq!(views[0]["local_deleted"], true);
        assert_eq!(views[0]["export_resumable"], false);
        let copies = catalog.generations(&tape.barcode).await.unwrap();
        assert_eq!(copies.len(), 1);
        assert_eq!(copies[0].generation, 7);
        assert!(!copies[0].local_match);
        let retained: String =
            sqlx::query_scalar("SELECT metadata FROM remote_generations WHERE barcode='DELETE01'")
                .fetch_one(&catalog.pool())
                .await
                .unwrap();
        assert_eq!(retained, metadata);
        catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_export_retry_requires_latest_matching_job_and_preserves_generation() {
        for scenario in [
            "matching",
            "generation",
            "destination",
            "revision",
            "unrelated",
            "missing",
        ] {
            let root = std::env::temp_dir().join(format!("export-retry-{}", uuid::Uuid::new_v4()));
            let catalog = Catalog::open(&root.join("catalog.db")).await.unwrap();
            let vtl = catalog.vtl_id().await.unwrap().to_string();
            let destination = catalog.destination().await.unwrap().id;
            sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,1,'{}')").bind(&destination).execute(&catalog.pool()).await.unwrap();
            let mut tape = Cartridge::new(Barcode::parse("RETRY01").unwrap(), 1024).unwrap();
            tape.state = CartridgeState::Sealed;
            catalog.create_cartridge(&tape).await.unwrap();
            catalog
                .reserve_job("first", "export", &tape, Some(1), None)
                .await
                .unwrap();
            tape.state = CartridgeState::Failed;
            catalog
                .finish_job("first", "failed", "network interrupted", Some(&tape), None)
                .await
                .unwrap();
            match scenario {
                "generation" => {
                    sqlx::query("UPDATE jobs SET generation=2 WHERE id='first'")
                        .execute(&catalog.pool())
                        .await
                        .unwrap();
                }
                "destination" => {
                    sqlx::query("UPDATE storage_destinations SET active=0")
                        .execute(&catalog.pool())
                        .await
                        .unwrap();
                    sqlx::query(
                        "INSERT INTO storage_destinations(id,vtl_id,active) VALUES('next',?,1)",
                    )
                    .bind(&vtl)
                    .execute(&catalog.pool())
                    .await
                    .unwrap();
                }
                "revision" => {
                    sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,2,'{}')").bind(&destination).execute(&catalog.pool()).await.unwrap();
                }
                "unrelated" => {
                    catalog
                        .reserve_job("later", "restore", &tape, Some(1), None)
                        .await
                        .unwrap();
                    catalog
                        .finish_job("later", "failed", "restore failed", None, None)
                        .await
                        .unwrap();
                }
                "missing" => {
                    sqlx::query("DELETE FROM jobs")
                        .execute(&catalog.pool())
                        .await
                        .unwrap();
                }
                _ => {}
            }
            let view = catalog.cartridge_views(Some(&tape.barcode)).await.unwrap();
            assert_eq!(
                view[0].export_resumable,
                scenario == "matching",
                "view: {scenario}"
            );
            let retry = catalog
                .reserve_job("retry", "export", &tape, Some(1), None)
                .await;
            assert_eq!(retry.is_ok(), scenario == "matching", "{scenario}");
            let stored = catalog.cartridge(&tape.barcode).await.unwrap().unwrap();
            assert_eq!(stored.generation, 1);
            assert_eq!(
                stored.state,
                if scenario == "matching" {
                    CartridgeState::Uploading
                } else {
                    CartridgeState::Failed
                }
            );
            assert_eq!(
                catalog.job("retry").await.unwrap().is_some(),
                scenario == "matching"
            );
            catalog.pool().close().await;
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[tokio::test]
    async fn discovery_skips_pending_creation_markers_until_removed() {
        let root =
            std::env::temp_dir().join(format!("cold-vtl-discovery-{}", uuid::Uuid::new_v4()));
        let catalog = Catalog::open(&root.join("catalog.db")).await.unwrap();
        let media = root.join("media");
        for barcode in ["NEW001L8", "NEW002L8"] {
            let directory = media.join(barcode);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("mam"), b"media").unwrap();
        }
        std::fs::write(
            media.join("NEW001L8/.vaultcenter-create-pending"),
            b"pending",
        )
        .unwrap();
        // A dangling marker symlink also means discovery must leave recovery to the controller.
        std::os::unix::fs::symlink(
            "missing-journal",
            media.join("NEW002L8/.vaultcenter-create-pending"),
        )
        .unwrap();
        assert_eq!(
            catalog.discover(&media, 2048 * 1024 * 1024).await.unwrap(),
            0
        );
        assert!(catalog.list_cartridges().await.unwrap().is_empty());
        for barcode in ["NEW001L8", "NEW002L8"] {
            std::fs::remove_file(media.join(barcode).join(".vaultcenter-create-pending")).unwrap();
        }
        assert_eq!(
            catalog.discover(&media, 2048 * 1024 * 1024).await.unwrap(),
            2
        );
        assert_eq!(catalog.list_cartridges().await.unwrap().len(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn concurrent_jobs_are_exclusive_and_restore_preserves_media() {
        let root = std::env::temp_dir().join(format!("cold-vtl-catalog-{}", uuid::Uuid::new_v4()));
        let catalog = Catalog::open(&root.join("catalog.db")).await.unwrap();
        let cartridge = Cartridge {
            media_type: None,
            segment_size_mib: 256,
            multipart_part_size_mib: 256,
            barcode: Barcode::parse("TAPE01").unwrap(),
            capacity_bytes: 100,
            bytes_written: 4,
            generation: 2,
            state: CartridgeState::Verified,
        };
        catalog.create_cartridge(&cartridge).await.unwrap();
        let (a, b) = tokio::join!(
            catalog.reserve_job("job-a", "restore", &cartridge, Some(1), Some("/tmp/a")),
            catalog.reserve_job("job-b", "restore", &cartridge, Some(2), Some("/tmp/b"))
        );
        assert_ne!(a.is_ok(), b.is_ok());
        assert_eq!(catalog.list_jobs().await.unwrap().len(), 1);
        let job = a.or(b).unwrap();
        catalog
            .finish_job(&job.id, "completed", "restored", None, None)
            .await
            .unwrap();
        assert_eq!(
            catalog
                .cartridge(&cartridge.barcode)
                .await
                .unwrap()
                .unwrap(),
            cartridge
        );
        let next = catalog
            .reserve_job("job-c", "restore", &cartridge, Some(2), Some("/tmp/c"))
            .await
            .unwrap();
        assert_eq!(next.generation, Some(2));
        assert_eq!(next.destination.as_deref(), Some("/tmp/c"));
        let reopened = Catalog::open(&root.join("catalog.db")).await.unwrap();
        assert_eq!(
            reopened.job("job-c").await.unwrap().unwrap().status,
            "failed"
        );
        assert_eq!(
            reopened
                .cartridge(&cartridge.barcode)
                .await
                .unwrap()
                .unwrap(),
            cartridge
        );
        drop(reopened);
        drop(catalog);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod multi_vtl_tests;
