//! Optional, bounded history of acknowledged payload bytes. Never export evidence.
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, serde::Serialize)]
struct ExportProgress {
    phase: &'static str,
    completed_bytes: u64,
    total_bytes: Option<u64>,
}
impl Default for ExportProgress {
    fn default() -> Self {
        Self {
            phase: "preparing",
            completed_bytes: 0,
            total_bytes: None,
        }
    }
}
#[derive(Default)]
struct Counters {
    bytes: AtomicU64,
    active: AtomicBool,
    progress: Mutex<ExportProgress>,
}
pub struct Recorder {
    counters: Arc<Counters>,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
impl Recorder {
    pub async fn from_env(barcode: &str, generation: u64) -> Option<Self> {
        Self::from_environment(barcode, generation, false).await
    }
    pub async fn restore_from_env(barcode: &str, generation: u64) -> Option<Self> {
        Self::from_environment(barcode, generation, true).await
    }
    async fn from_environment(barcode: &str, generation: u64, download: bool) -> Option<Self> {
        let job = std::env::var(if download {
            "COLD_VTL_RESTORE_JOB_ID"
        } else {
            "COLD_VTL_EXPORT_JOB_ID"
        })
        .ok()?;
        let vtl = std::env::var("COLD_VTL_VTL_ID").ok()?;
        let path = PathBuf::from(std::env::var_os("COLD_VTL_DATABASE")?);
        match Self::open_direction(&path, &vtl, &job, barcode, generation, download).await {
            Ok(recorder) => Some(recorder),
            Err(_) => {
                eprintln!(
                    "S3 throughput history unavailable; integrity verification remains required"
                );
                None
            }
        }
    }
    #[cfg(test)]
    pub(crate) async fn open(
        path: &Path,
        vtl: &str,
        job: &str,
        barcode: &str,
        generation: u64,
    ) -> Result<Self, sqlx::Error> {
        Self::open_direction(path, vtl, job, barcode, generation, false).await
    }
    async fn open_direction(
        path: &Path,
        vtl: &str,
        job: &str,
        barcode: &str,
        generation: u64,
        download: bool,
    ) -> Result<Self, sqlx::Error> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(false)
                    .busy_timeout(Duration::from_millis(250)),
            )
            .await?;
        let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=? AND vtl_id=? AND subject=? AND generation=? AND ((? AND kind IN ('restore','recovery')) OR (NOT ? AND kind='export')) AND status='running')")
            .bind(job).bind(vtl).bind(barcode).bind(generation as i64).bind(download).bind(download).fetch_one(&pool).await?;
        if !valid {
            return Err(sqlx::Error::RowNotFound);
        }
        let start = now_ms();
        record(&pool, vtl, job, barcode, start, None, 5000.0).await?;
        let counters = Arc::new(Counters::default());
        publish_progress(&pool, vtl, job, barcode, &counters, download).await?;
        let shared = counters.clone();
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let (vtl, job, barcode) = (vtl.to_owned(), job.to_owned(), barcode.to_owned());
        let task = tokio::spawn(async move {
            let mut previous = Instant::now();
            let mut last_at = start;
            let mut was_active = false;
            let mut timer = tokio::time::interval_at(
                tokio::time::Instant::now() + Duration::from_secs(5),
                Duration::from_secs(5),
            );
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let finished = tokio::select! { _=timer.tick()=>false, _=&mut stopped=>true };
                let elapsed = previous.elapsed().as_secs_f64();
                previous = Instant::now();
                let bytes = shared.bytes.swap(0, Ordering::SeqCst);
                let active = shared.active.load(Ordering::SeqCst);
                let at = now_ms().max(last_at + 1);
                if bytes > 0 || active || was_active {
                    // No completion in an active interval means zero acknowledged
                    // bytes, not a claim about socket traffic or buffered data.
                    let rate = if bytes > 0 || active {
                        Some(bytes as f64 / elapsed.max(0.001))
                    } else {
                        None
                    };
                    if record(&pool, &vtl, &job, &barcode, at, rate, elapsed * 1000.0)
                        .await
                        .is_err()
                    {
                        eprintln!("S3 throughput history recording stopped; transfer continues");
                        break;
                    }
                    last_at = at;
                }
                if publish_progress(&pool, &vtl, &job, &barcode, &shared, download)
                    .await
                    .is_err()
                {
                    eprintln!("S3 progress recording stopped; transfer continues");
                    break;
                }
                was_active = active;
                if finished {
                    let _ = record(
                        &pool,
                        &vtl,
                        &job,
                        &barcode,
                        now_ms().max(last_at + 1),
                        None,
                        5000.0,
                    )
                    .await;
                    break;
                }
            }
            pool.close().await;
        });
        Ok(Self {
            counters,
            stop,
            task,
        })
    }
    pub fn set_active(&self, active: bool) {
        self.counters.active.store(active, Ordering::SeqCst);
    }
    pub fn set_total(&self, bytes: u64) {
        self.counters.progress.lock().unwrap().total_bytes = Some(bytes);
    }
    pub fn phase(&self, phase: &'static str) {
        self.counters.progress.lock().unwrap().phase = phase;
    }
    // Previously confirmed parts advance payload progress without claiming new traffic.
    pub fn reuse(&self, bytes: u64) {
        let mut progress = self.counters.progress.lock().unwrap();
        progress.completed_bytes = progress.completed_bytes.saturating_add(bytes);
    }
    pub fn acknowledge(&self, bytes: u64) {
        self.reuse(bytes);
        self.counters.bytes.fetch_add(bytes, Ordering::SeqCst);
    }
    pub async fn close(self) {
        let _ = self.stop.send(());
        let _ = self.task.await;
    }
}
async fn publish_progress(
    pool: &SqlitePool,
    vtl: &str,
    job: &str,
    barcode: &str,
    counters: &Counters,
    download: bool,
) -> Result<(), sqlx::Error> {
    let progress = counters.progress.lock().unwrap().clone();
    let percent = match progress.total_bytes {
        Some(total) if total > 0 => {
            ((progress.completed_bytes as u128 * 100) / total as u128).min(99) as i64
        }
        _ => 0,
    };
    let key = if download {
        "restore_progress"
    } else {
        "export_progress"
    };
    let message = serde_json::json!({key: progress}).to_string();
    // Never overwrite a terminal job or make verification/completion claims.
    sqlx::query("UPDATE jobs SET progress=?,message=?,updated_at=CURRENT_TIMESTAMP WHERE id=? AND vtl_id=? AND subject=? AND ((? AND kind IN ('restore','recovery')) OR (NOT ? AND kind='export')) AND status='running'")
        .bind(percent).bind(message).bind(job).bind(vtl).bind(barcode).bind(download).bind(download).execute(pool).await?;
    Ok(())
}
async fn record(
    pool: &SqlitePool,
    vtl: &str,
    job: &str,
    barcode: &str,
    at: i64,
    rate: Option<f64>,
    elapsed_ms: f64,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO s3_transfer_samples(vtl_id,transfer_id,barcode,at_ms,write_rate_bps,elapsed_ms) VALUES(?,?,?,?,?,?)")
        .bind(vtl).bind(job).bind(barcode).bind(at).bind(rate).bind(elapsed_ms.max(1.0)).execute(pool).await?;
    Ok(())
}
#[cfg(test)]
mod tests;
