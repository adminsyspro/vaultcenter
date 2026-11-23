//! Application payload budget shared by every worker of one isolated VTL.
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error)]
#[error("S3 throttling unavailable: {0}")]
pub struct BandwidthError(String);
fn error(value: impl std::fmt::Display) -> BandwidthError {
    BandwidthError(value.to_string())
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema: u8,
    vtl_id: String,
    revision: u64,
    tape_write_bps: Option<u64>,
    s3_bps: Option<u64>,
}
fn valid_rate(rate: Option<u64>) -> bool {
    rate.is_none_or(|r| (1_000_000..=100_000_000_000).contains(&r) && r % 1_000_000 == 0)
}
fn protected_file(path: &Path, max_size: u64) -> Result<(), BandwidthError> {
    let info = fs::symlink_metadata(path).map_err(error)?;
    if !info.is_file()
        || info.uid() != unsafe { libc::geteuid() }
        || info.mode() & 0o022 != 0
        || info.len() > max_size
    {
        return Err(error("file not protected or too large"));
    }
    Ok(())
}
fn read_config(root: &Path, id: &str) -> Result<Config, BandwidthError> {
    let path = root.join("bandwidth.json");
    protected_file(&path, 4096)?;
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(path).map_err(error)?).map_err(error)?;
    if !["schema", "vtl_id", "revision", "tape_write_bps", "s3_bps"]
        .iter()
        .all(|key| value.get(key).is_some())
    {
        return Err(error("incomplete throughput configuration"));
    }
    let cfg: Config = serde_json::from_value(value).map_err(error)?;
    if cfg.schema != 1
        || cfg.vtl_id != id
        || cfg.revision == 0
        || cfg.revision >= 1 << 63
        || !valid_rate(cfg.s3_bps)
        || !valid_rate(cfg.tape_write_bps)
    {
        return Err(error("invalid configuration or different VTL"));
    }
    Ok(cfg)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Budget {
    schema: u8,
    vtl_id: String,
    boot: String,
    revision: u64,
    rate: Option<u64>,
    time_ns: u64,
    tokens: u64,
    fraction: u64,
}
impl Budget {
    fn new(id: &str, boot: &str, revision: u64, rate: Option<u64>, now: u64) -> Self {
        Self {
            schema: 1,
            vtl_id: id.into(),
            boot: boot.into(),
            revision,
            rate,
            time_ns: now,
            tokens: 0,
            fraction: 0,
        }
    }
    fn take(
        &mut self,
        now: u64,
        boot: &str,
        revision: u64,
        rate: Option<u64>,
        bytes: usize,
    ) -> Result<Option<Duration>, BandwidthError> {
        if !(1..=65_536).contains(&bytes) || !valid_rate(rate) {
            return Err(error("invalid amount or rate"));
        }
        if self.boot != boot || self.revision != revision || self.rate != rate {
            *self = Self::new(&self.vtl_id, boot, revision, rate, now);
        }
        let elapsed = now
            .checked_sub(self.time_ns)
            .ok_or_else(|| error("invalid budget clock"))?;
        self.time_ns = now;
        let Some(rate) = rate else {
            return Ok(None);
        };
        let capacity = (rate / 10).max(65_536);
        let earned = elapsed as u128 * rate as u128 + self.fraction as u128;
        let available = self.tokens as u128 + earned / 1_000_000_000;
        self.tokens = available.min(capacity as u128) as u64;
        self.fraction = if available >= capacity as u128 {
            0
        } else {
            (earned % 1_000_000_000) as u64
        };
        if self.tokens >= bytes as u64 {
            self.tokens -= bytes as u64;
            Ok(None)
        } else {
            let deficit =
                (bytes as u64 - self.tokens) as u128 * 1_000_000_000 - self.fraction as u128;
            Ok(Some(Duration::from_nanos(
                deficit.div_ceil(rate as u128).min(100_000_000) as u64,
            )))
        }
    }
}

struct Cached {
    config: Config,
    checked: Instant,
}
#[derive(Clone)]
pub struct VtlBandwidth {
    root: PathBuf,
    id: String,
    boot: String,
    cached: Arc<Mutex<Cached>>,
}

impl VtlBandwidth {
    pub fn open(state_root: &Path, vtl_id: &str) -> Result<Self, BandwidthError> {
        let parsed: cold_vtl_core::VtlId = vtl_id.parse().map_err(error)?;
        if parsed.to_string() != vtl_id {
            return Err(error("non-canonical VTL identifier"));
        }
        let root = state_root.join("vtls").join(vtl_id);
        for dir in [
            state_root.to_path_buf(),
            state_root.join("vtls"),
            root.clone(),
        ] {
            let info = fs::symlink_metadata(dir).map_err(error)?;
            if !info.is_dir()
                || info.uid() != unsafe { libc::geteuid() }
                || info.mode() & 0o022 != 0
            {
                return Err(error("VTL root not protected"));
            }
        }
        let config = read_config(&root, vtl_id)?;
        let boot = fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .map_err(error)?
            .trim()
            .to_owned();
        Ok(Self {
            root,
            id: vtl_id.into(),
            boot,
            cached: Arc::new(Mutex::new(Cached {
                config,
                checked: Instant::now(),
            })),
        })
    }

    pub async fn acquire(&self, bytes: usize) -> Result<(), BandwidthError> {
        if !(1..=65_536).contains(&bytes) {
            return Err(error("block outside 1..65536 bytes"));
        }
        // Unlimited steady state performs no file IO and schedules no blocking work.
        {
            let cache = self.cached.lock().map_err(error)?;
            if cache.checked.elapsed() < Duration::from_secs(1) && cache.config.s3_bps.is_none() {
                return Ok(());
            }
        }
        let mut contended_since = None;
        loop {
            let this = self.clone();
            let result = tokio::task::spawn_blocking(move || this.try_take(bytes))
                .await
                .map_err(error)??;
            match result {
                Attempt::Taken => return Ok(()),
                Attempt::Wait(delay) => {
                    contended_since = None;
                    tokio::time::sleep(delay).await;
                }
                Attempt::Busy => {
                    if contended_since.get_or_insert_with(Instant::now).elapsed()
                        > Duration::from_secs(5)
                    {
                        return Err(error("budget lock busy for more than five seconds"));
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        }
    }

    fn config(&self) -> Result<Config, BandwidthError> {
        let mut cache = self.cached.lock().map_err(error)?;
        if cache.checked.elapsed() >= Duration::from_secs(1) {
            let warning = self
                .root
                .join(format!("bandwidth-error-{}.json", std::process::id()));
            match read_config(&self.root, &self.id) {
                Ok(value)
                    if value.revision > cache.config.revision
                        || (value.revision == cache.config.revision
                            && value.s3_bps == cache.config.s3_bps
                            && value.tape_write_bps == cache.config.tape_write_bps) =>
                {
                    cache.config = value;
                    let _ = fs::remove_file(&warning);
                }
                result => {
                    let message = result
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "throughput revision went backwards".into());
                    let bytes = serde_json::to_vec(&serde_json::json!({"vtl_id":self.id,"pid":std::process::id(),"message":message})).map_err(error)?;
                    write_ephemeral(&warning, &bytes)?;
                    // Keep last known valid limit; report rather than silently disabling it.
                }
            }
            cache.checked = Instant::now();
        }
        Ok(cache.config.clone())
    }

    fn try_take(&self, bytes: usize) -> Result<Attempt, BandwidthError> {
        let cfg = self.config()?;
        if cfg.s3_bps.is_none() {
            return Ok(Attempt::Taken);
        }
        let lock_path = self.root.join("bandwidth-budget.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path)
            .map_err(error)?;
        protected_file(&lock_path, 4096)?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let cause = std::io::Error::last_os_error();
            return if cause.kind() == std::io::ErrorKind::WouldBlock {
                Ok(Attempt::Busy)
            } else {
                Err(error(cause))
            };
        }
        let now = monotonic_ns()?;
        let file = self.root.join("bandwidth-budget.json");
        let marker = self.root.join("bandwidth-budget-initialized");
        let mut budget = match fs::symlink_metadata(&file) {
            Ok(_) => {
                protected_file(&file, 4096)?;
                let b: Budget =
                    serde_json::from_slice(&fs::read(&file).map_err(error)?).map_err(error)?;
                if b.schema != 1
                    || b.vtl_id != self.id
                    || !valid_rate(b.rate)
                    || b.fraction >= 1_000_000_000
                {
                    return Err(error("invalid budget register"));
                }
                b
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !marker.exists() => {
                Budget::new(&self.id, &self.boot, cfg.revision, cfg.s3_bps, now)
            }
            Err(e) => return Err(error(e)),
        };
        // Never apply an older per-process cached configuration over a newer budget.
        if budget.boot == self.boot && budget.revision > cfg.revision {
            let current = read_config(&self.root, &self.id)?;
            if current.revision < budget.revision {
                return Err(error("throughput revision went backwards"));
            }
            *self.cached.lock().map_err(error)? = Cached {
                config: current,
                checked: Instant::now(),
            };
            return Ok(Attempt::Wait(Duration::from_millis(1)));
        }
        let wait = budget.take(now, &self.boot, cfg.revision, cfg.s3_bps, bytes)?;
        write_ephemeral(&file, &serde_json::to_vec(&budget).map_err(error)?)?;
        if !marker.exists() {
            write_ephemeral(&marker, b"1\n")?;
        }
        // File drop releases flock, including all error paths, before async sleep.
        drop(lock);
        Ok(wait.map_or(Attempt::Taken, Attempt::Wait))
    }
}

enum Attempt {
    Taken,
    Wait(Duration),
    Busy,
}
fn monotonic_ns() -> Result<u64, BandwidthError> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
        return Err(error(std::io::Error::last_os_error()));
    }
    (ts.tv_sec as u64)
        .checked_mul(1_000_000_000)
        .and_then(|n| n.checked_add(ts.tv_nsec as u64))
        .ok_or_else(|| error("clock out of range"))
}
fn write_ephemeral(path: &Path, bytes: &[u8]) -> Result<(), BandwidthError> {
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let result = (|| {
        let mut file: File = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)
            .map_err(error)?;
        file.write_all(bytes).map_err(error)?;
        fs::rename(&temp, path).map_err(error)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

#[cfg(test)]
pub(crate) mod tests;
