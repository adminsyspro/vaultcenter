use crate::{ApiError, AppState, catalog::Job};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use cold_vtl_core::{Barcode, Cartridge, CartridgeState, ReconcileOutput, RemoteGeneration};
use serde::Deserialize;
use std::{
    path::{Path as FsPath, PathBuf},
    time::Duration,
};
use tokio::process::Command;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreRequest {
    generation: u64,
    /// A new directory name within the appliance's configured restore root.
    destination: Option<String>,
}

pub async fn ensure_quiescent(state: &AppState) -> Result<(), ApiError> {
    crate::deletion::no_pending(state).await?;
    crate::tape_storage::ensure_available(state).await?;
    if state.config.maintenance_helper.is_some() {
        return crate::maintenance::check(state).await;
    }
    let path = state
        .config
        .quiesce_check
        .as_ref()
        .ok_or_else(|| ApiError::conflict("maintenance check is not configured"))?;
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(path).kill_on_drop(true).output(),
    )
    .await
    .map_err(|_| ApiError::conflict("maintenance check timed out"))?
    .map_err(ApiError::conflict)?;
    if !output.status.success() {
        return Err(ApiError::conflict(
            "VTL maintenance required: stop tgt, the tape drive and changer before sealing, reconciling or exporting",
        ));
    }
    Ok(())
}

pub async fn available_cartridge(state: &AppState, raw: String) -> Result<Cartridge, ApiError> {
    crate::deletion::no_pending(state).await?;
    crate::tape_storage::ensure_available(state).await?;
    crate::archive::media_idle(state).await?;
    let barcode = Barcode::parse(raw).map_err(ApiError::bad_request)?;
    let cartridge = state
        .catalog
        .cartridge(&barcode)
        .await?
        .ok_or_else(|| ApiError::not_found("cartridge not found"))?;
    if state.catalog.has_active_job(&barcode).await? {
        return Err(ApiError::conflict(
            "another job is active for this cartridge",
        ));
    }
    Ok(cartridge)
}

pub async fn generations(
    State(state): State<AppState>,
    Path(raw): Path<String>,
) -> Result<Json<Vec<RemoteGeneration>>, ApiError> {
    let barcode = Barcode::parse(raw).map_err(ApiError::bad_request)?;
    if state.catalog.cartridge(&barcode).await?.is_none() {
        return Err(ApiError::not_found("cartridge not found"));
    }
    Ok(Json(state.catalog.generations(&barcode).await?))
}

pub async fn reconcile(
    State(state): State<AppState>,
    Path(raw): Path<String>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    Ok((
        StatusCode::ACCEPTED,
        Json(start_reconcile(&state, raw).await?),
    ))
}

/// Synchronisations the background pass runs at once per VTL, so that every
/// other cartridge stays available for exports.
pub(crate) const MAX_SYNCS_PER_VTL: i64 = 2;
/// A failed synchronisation waits this long before the background pass
/// retries it, so a lasting failure does not fill Activité every minute.
const RETRY_FAILED_AFTER: &str = "-30 minutes";

pub(crate) async fn start_reconcile(state: &AppState, raw: String) -> Result<Job, ApiError> {
    let _lock = state.operations.lock().await;
    start_reconcile_locked(state, raw, false).await
}

/// Start a synchronisation; the caller holds the operations lock. `capped`
/// (the background pass) refuses once `MAX_SYNCS_PER_VTL` run, checked here
/// under the lock so that a stale candidate list never adds one more.
pub(crate) async fn start_reconcile_locked(
    state: &AppState,
    raw: String,
    capped: bool,
) -> Result<Job, ApiError> {
    let cartridge = available_cartridge(state, raw).await?;
    if capped && running_syncs(state).await? >= MAX_SYNCS_PER_VTL {
        return Err(ApiError::conflict(
            "The maximum number of simultaneous synchronisations has been reached.",
        ));
    }
    let guard = if state
        .catalog
        .local_deleted(cartridge.barcode.as_str())
        .await?
    {
        None
    } else {
        Some(crate::media_access::protect(state, cartridge.barcode.as_str()).await?)
    };
    crate::storage::require_access(state, false).await?;
    let job = state
        .catalog
        .reserve_job(
            &Uuid::new_v4().to_string(),
            "reconcile",
            &cartridge,
            None,
            None,
        )
        .await?;
    spawn_reconcile(state.clone(), cartridge, job.id.clone(), guard);
    Ok(job)
}

/// Message of a synchronisation stopped because the active destination changed.
pub(crate) const SYNC_CANCELLED: &str = "Synchronisation cancelled: the destination has changed.";
/// `SYNC_CANCELLED` as written by versions before English messages.
const SYNC_CANCELLED_FRENCH: &str = "Synchronisation annulée : la destination a changé.";

/// Run the reserved synchronisation `id` in the background.
pub(crate) fn spawn_reconcile(
    state: AppState,
    cartridge: Cartridge,
    id: String,
    guard: Option<crate::media_access::Guard>,
) {
    let (stop, stopped) = tokio::sync::watch::channel(false);
    STOPS
        .lock()
        .expect("synchronisation registry poisoned")
        .insert(id.clone(), stop);
    let registered = Registered(id.clone());
    tokio::spawn(async move {
        let _registered = registered;
        run_reconcile(state, cartridge, id, guard, stopped).await;
    });
}

/// Removes a synchronisation's stop signal when its task ends, even by a panic.
struct Registered(String);
impl Drop for Registered {
    fn drop(&mut self) {
        match STOPS.lock() {
            Ok(mut stops) => {
                stops.remove(&self.0);
            }
            Err(_) => eprintln!("Synchronisation registry poisoned: {} not removed", self.0),
        }
    }
}

/// Stop signal of each synchronisation running in this process, by job id.
static STOPS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, tokio::sync::watch::Sender<bool>>>,
> = std::sync::LazyLock::new(Default::default);

/// How long a stopped synchronisation worker may take to exit before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// Stop signals of these synchronisations, taken before their jobs are
/// closed so that each is sent right after the closing commit.
pub(crate) fn reconcile_stops(ids: &[String]) -> Vec<tokio::sync::watch::Sender<bool>> {
    let stops = STOPS.lock().expect("synchronisation registry poisoned");
    ids.iter().filter_map(|id| stops.get(id).cloned()).collect()
}

/// Stop the workers of synchronisations whose jobs were just closed with
/// `SYNC_CANCELLED`: each task releases its cartridge and keeps that
/// message, then the worker units of those `barcodes` are stopped.
pub(crate) async fn stop_reconciles(
    vtl: &str,
    stops: Vec<tokio::sync::watch::Sender<bool>>,
    barcodes: Vec<String>,
) {
    for stop in stops {
        stop.send_replace(true);
    }
    // An isolated VTL runs its worker in a transient systemd unit that the
    // `systemd-run --wait` client keeps running when it is signalled.
    if !barcodes.is_empty() {
        stop_reconcile_units(vtl.to_owned(), barcodes).await;
    }
}

/// Whether a unit's `ExecStart` (as `systemctl show -p ExecStart --value`
/// prints it) runs the synchronisation of exactly `barcode`. The worker
/// command line carries no job id; one synchronisation runs per cartridge.
fn reconcile_unit_matches(exec_start: &str, barcode: &str) -> bool {
    let Some(argv) = exec_start
        .split_once("argv[]=")
        .map(|(_, rest)| rest.split_once(" ;").map_or(rest, |(argv, _)| argv))
    else {
        return false;
    };
    let args: Vec<&str> = argv.split_whitespace().collect();
    let command = args
        .windows(2)
        .any(|pair| pair == ["--execute", "reconcile"]);
    command && args.windows(2).any(|pair| pair == ["--barcode", barcode])
}

/// Total time the unit sweep of a switch may take, under the operations lock;
/// on expiry the jobs stay closed and a worker at worst runs on uselessly.
const SWEEP_BUDGET: Duration = Duration::from_secs(15);
/// Time one systemctl call may take within that budget.
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(5);

async fn systemctl(
    program: &std::path::Path,
    timeout: Duration,
    args: &[&str],
) -> Result<String, String> {
    let output = tokio::time::timeout(
        timeout,
        Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| format!("no answer within {} s", timeout.as_secs_f32()))?
    .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What a unit sweep got through before it ended or ran out of time.
#[derive(Debug, Default, PartialEq, Eq)]
struct Sweep {
    /// Units of the VTL listed but not inspected yet.
    uninspected: Vec<String>,
    /// Units synchronising a cancelled cartridge, not confirmed stopped.
    unstopped: Vec<String>,
    stopped: Vec<String>,
}

/// Stop the transient worker units of this VTL that synchronise one of
/// `barcodes`; no other unit is ever stopped. Without systemctl (tests,
/// development) there is no such unit and nothing is done. The sweep takes
/// at most `SWEEP_BUDGET` (each call `SYSTEMCTL_TIMEOUT`).
async fn stop_reconcile_units(vtl: String, barcodes: Vec<String>) {
    sweep_units(
        std::path::Path::new("systemctl"),
        &vtl,
        &barcodes,
        SWEEP_BUDGET,
        SYSTEMCTL_TIMEOUT,
    )
    .await;
}

async fn sweep_units(
    program: &std::path::Path,
    vtl: &str,
    barcodes: &[String],
    budget: Duration,
    per_call: Duration,
) -> Sweep {
    let mut sweep = Sweep::default();
    if tokio::time::timeout(
        budget,
        sweep_within(program, vtl, barcodes, per_call, &mut sweep),
    )
    .await
    .is_err()
    {
        eprintln!(
            "Synchronisation units of VTL {vtl}: sweep stopped after {} s; not confirmed stopped: {:?}; not inspected: {:?}",
            budget.as_secs_f32(),
            sweep.unstopped,
            sweep.uninspected
        );
    }
    sweep
}

async fn sweep_within(
    program: &std::path::Path,
    vtl: &str,
    barcodes: &[String],
    per_call: Duration,
    sweep: &mut Sweep,
) {
    if vtl.is_empty() || !vtl.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-') {
        eprintln!("Synchronisation units of VTL {vtl:?} not stopped: unexpected VTL id");
        return;
    }
    let pattern = format!("vaultcenter-worker-{vtl}-*");
    let listed = match systemctl(
        program,
        per_call,
        &["list-units", "--no-legend", "--plain", &pattern],
    )
    .await
    {
        Ok(listed) => listed,
        Err(error) => {
            eprintln!("Synchronisation units of VTL {vtl} not looked up (systemctl: {error})");
            return;
        }
    };
    let prefix = format!("vaultcenter-worker-{vtl}-");
    sweep.uninspected = listed
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|unit| unit.starts_with(&prefix) && unit.ends_with(".service"))
        .map(str::to_owned)
        .collect();
    while let Some(unit) = sweep.uninspected.first().cloned() {
        let exec_start = systemctl(
            program,
            per_call,
            &["show", "-p", "ExecStart", "--value", &unit],
        )
        .await;
        sweep.uninspected.remove(0);
        let exec_start = match exec_start {
            Ok(exec_start) => exec_start,
            Err(error) => {
                eprintln!("Worker unit {unit} not inspected: {error}");
                continue;
            }
        };
        let Some(barcode) = barcodes
            .iter()
            .find(|barcode| reconcile_unit_matches(&exec_start, barcode))
        else {
            continue;
        };
        sweep.unstopped.push(unit.clone());
        match systemctl(program, per_call, &["stop", &unit]).await {
            Ok(_) => {
                eprintln!("Synchronisation of {barcode} stopped: unit {unit}");
                sweep.unstopped.retain(|other| other != &unit);
                sweep.stopped.push(unit);
            }
            Err(error) => {
                eprintln!("Synchronisation unit {unit} of {barcode} not stopped: {error}")
            }
        }
    }
}

/// `worker_output`, ending early with `SYNC_CANCELLED` once `stopped` is set:
/// the worker (or the client of its systemd unit) is asked to stop with
/// SIGTERM, then killed after `STOP_GRACE`.
async fn stoppable_output(
    command: &mut Command,
    stopped: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<String, String> {
    let child = command
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let pid = child.id().and_then(|pid| i32::try_from(pid).ok());
    let output = child.wait_with_output();
    tokio::pin!(output);
    let stop = async {
        // A dropped sender never asks for a stop.
        if stopped.wait_for(|stop| *stop).await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let output = tokio::select! {
        output = &mut output => output.map_err(|e| e.to_string())?,
        () = stop => {
            if let Some(pid) = pid {
                // SAFETY: plain signal to our own child, which has not been reaped yet.
                if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
                    eprintln!("Synchronisation worker {pid} not signalled: {}", std::io::Error::last_os_error());
                }
            }
            if tokio::time::timeout(STOP_GRACE, &mut output).await.is_err() {
                eprintln!("Synchronisation worker still running after {} s: killed", STOP_GRACE.as_secs());
            }
            return Err(SYNC_CANCELLED.into());
        }
    };
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

/// Synchronisations queued or running on this VTL.
pub(crate) async fn running_syncs(state: &AppState) -> Result<i64, ApiError> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE vtl_id=? AND kind='reconcile' AND status IN ('queued','running')",
    )
    .bind(state.catalog.vtl_id().await?.to_string())
    .fetch_one(&state.catalog.pool())
    .await
    .map_err(ApiError::conflict)
}

/// Cartridges still to synchronise with the active bucket, in order, skipping
/// those with an operation in progress and those whose synchronisation failed
/// less than 30 minutes ago.
pub(crate) async fn reconcile_candidates(state: &AppState) -> Result<Vec<String>, ApiError> {
    sqlx::query_scalar("SELECT r.barcode FROM s3_reconcile_required r WHERE r.vtl_id=?
          AND NOT EXISTS(SELECT 1 FROM jobs j WHERE j.vtl_id=r.vtl_id AND j.subject=r.barcode AND j.status IN ('queued','running'))
          AND NOT EXISTS(SELECT 1 FROM jobs j WHERE j.vtl_id=r.vtl_id AND j.subject=r.barcode AND j.kind='reconcile' AND j.status='failed' AND j.message IS NOT ? AND j.message IS NOT ? AND j.updated_at>datetime('now',?))
        ORDER BY r.barcode")
        .bind(state.catalog.vtl_id().await?.to_string())
        // A synchronisation cancelled by a switch did not fail: no delay.
        .bind(SYNC_CANCELLED)
        .bind(SYNC_CANCELLED_FRENCH)
        .bind(RETRY_FAILED_AFTER)
        .fetch_all(&state.catalog.pool())
        .await
        .map_err(ApiError::conflict)
}

/// Start the synchronisations still needed after a destination change, until
/// the free slots are filled. A cartridge whose start is refused (loaded in a
/// drive, being sent) is skipped for the next one and retried at the next
/// pass; every refusal is logged, the request that caused the pass being gone.
pub(crate) async fn reconcile_flagged(state: AppState) {
    let barcodes = match reconcile_candidates(&state).await {
        Ok(barcodes) => barcodes,
        Err(error) => {
            eprintln!("Cartridge synchronisation pass failed: {}", error.message);
            return;
        }
    };
    for barcode in barcodes {
        let _lock = state.operations.lock().await;
        match running_syncs(&state).await {
            Ok(running) if running >= MAX_SYNCS_PER_VTL => return,
            Ok(_) => {}
            Err(error) => {
                eprintln!("Cartridge synchronisation pass failed: {}", error.message);
                return;
            }
        }
        if let Err(error) = start_reconcile_locked(&state, barcode.clone(), true).await {
            eprintln!(
                "Synchronisation of cartridge {barcode} not started, retried at the next pass: {}",
                error.message
            );
        }
    }
}

/// One background pass for VTL `vtl`, from the unscoped `state`.
pub(crate) async fn reconcile_vtl(state: AppState, vtl: String) {
    match crate::vtls::installed_state(state, vtl.clone()).await {
        Ok(scoped) => reconcile_flagged(scoped).await,
        Err(error) => eprintln!(
            "Cartridge synchronisation not started for VTL {vtl}: {}",
            error.message
        ),
    }
}

/// Every minute, start the synchronisations still flagged on every VTL.
pub fn reconcile_monitor(state: AppState) {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(60));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            let vtls: Vec<String> =
                match sqlx::query_scalar("SELECT DISTINCT vtl_id FROM s3_reconcile_required")
                    .fetch_all(&state.catalog.pool())
                    .await
                {
                    Ok(vtls) => vtls,
                    Err(error) => {
                        eprintln!("Cartridge synchronisation pass failed: {error}");
                        continue;
                    }
                };
            for vtl in vtls {
                reconcile_vtl(state.clone(), vtl).await;
            }
        }
    });
}

pub(crate) async fn run_reconcile(
    state: AppState,
    mut cartridge: Cartridge,
    id: String,
    guard: Option<crate::media_access::Guard>,
    mut stopped: tokio::sync::watch::Receiver<bool>,
) {
    // `Ok(false)`: the job was closed meanwhile (cancelled by a destination
    // switch) and keeps its state; nothing is written for it.
    let result = async {
        if *stopped.borrow() {
            return Ok(false);
        }
        let deleted = state
            .catalog
            .local_deleted(cartridge.barcode.as_str())
            .await
            .map_err(|e| e.to_string())?;
        if !state
            .catalog
            .update_open_job(
                &id,
                "running",
                10,
                Some("remote manifest and SHA-256 verification started"),
            )
            .await
            .map_err(|e| e.to_string())?
        {
            return Ok(false);
        }
        let mut command = state
            .catalog
            .worker_command(&state.config.worker_path, Some(&id))
            .await
            .map_err(|e| e.to_string())?;
        command
            .arg("reconcile")
            .arg("--barcode")
            .arg(cartridge.barcode.as_str());
        if deleted {
            command.arg("--remote-only");
        } else {
            command
                .arg("--source")
                .arg(state.config.media_root.join(cartridge.barcode.as_str()));
        }
        let output = stoppable_output(&mut command, &mut stopped).await?;
        if *stopped.borrow() {
            return Ok(false);
        }
        let inventory: ReconcileOutput =
            serde_json::from_str(&output).map_err(|e| e.to_string())?;
        if let Some(guard) = &guard {
            guard.check(&state).await.map_err(|e| e.message)?;
        }
        let max = inventory
            .generations
            .iter()
            .map(|g| g.generation)
            .max()
            .unwrap_or(0);
        if inventory
            .generations
            .iter()
            .any(|g| g.barcode != cartridge.barcode || g.generation == 0)
        {
            return Err("invalid worker inventory".into());
        }
        if deleted && inventory.generations.iter().any(|g| g.local_match) {
            return Err("remote-only reconciliation claimed a local copy".into());
        }
        if !deleted {
            cartridge.bytes_written = inventory.local_size_bytes;
            if let Some(matching) = inventory
                .generations
                .iter()
                .filter(|g| g.local_match)
                .max_by_key(|g| g.generation)
            {
                cartridge.generation = matching.generation;
                cartridge.state = CartridgeState::Verified;
            } else {
                cartridge.generation = cartridge
                    .generation
                    .max(max.checked_add(1).ok_or("generation overflow")?);
                cartridge.state = CartridgeState::Writable;
            }
        }
        state
            .catalog
            .finish_open_job(
                &id,
                "completed",
                &output,
                Some(&cartridge),
                Some(&inventory.generations),
            )
            .await
            .map_err(|e| e.to_string())
    }
    .await;
    // A synchronisation stopped by a switch was closed with its reason.
    if let Err(error) = result
        && !*stopped.borrow()
        && let Err(failure) = state
            .catalog
            .finish_open_job(&id, "failed", &error, None, None)
            .await
    {
        eprintln!("Synchronisation job {id} failed ({error}) and could not be recorded: {failure}");
    }
}

pub fn destination_name(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 96
        && raw
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}

pub async fn restore(
    State(state): State<AppState>,
    Path(raw): Path<String>,
    Json(request): Json<RestoreRequest>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let _lock = state.operations.lock().await;
    if request.generation == 0 {
        return Err(ApiError::bad_request(
            "generation must be greater than zero",
        ));
    }
    let cartridge = available_cartridge(&state, raw).await?;
    let generation = state
        .catalog
        .generations(&cartridge.barcode)
        .await?
        .into_iter()
        .find(|g| g.generation == request.generation)
        .ok_or_else(|| ApiError::conflict("generation not in catalogue; reconcile S3 first"))?;
    let id = Uuid::new_v4().to_string();
    let name = request
        .destination
        .unwrap_or_else(|| format!("{}-g{}-{}", cartridge.barcode, request.generation, id));
    if !destination_name(&name) {
        return Err(ApiError::bad_request(
            "destination must be a new folder name (letters, digits, hyphen or underscore)",
        ));
    }
    let root = restore_root(&state.config.restore_root, &state.config.media_root)
        .await
        .map_err(ApiError::conflict)?;
    let destination = root.join(&name);
    if tokio::fs::symlink_metadata(&destination).await.is_ok() {
        return Err(ApiError::conflict("restore destination already exists"));
    }
    let path = destination
        .to_str()
        .ok_or_else(|| ApiError::bad_request("restore path is not UTF-8"))?;
    crate::storage::require_access(&state, false).await?;
    let job = state
        .catalog
        .reserve_job(
            &id,
            "restore",
            &cartridge,
            Some(request.generation),
            Some(path),
        )
        .await?;
    tokio::spawn(run_restore(state.clone(), generation, destination, id));
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn restore_root(root: &FsPath, media: &FsPath) -> Result<PathBuf, String> {
    tokio::fs::create_dir_all(root)
        .await
        .map_err(|e| e.to_string())?;
    let root = tokio::fs::canonicalize(root)
        .await
        .map_err(|e| e.to_string())?;
    let media = tokio::fs::canonicalize(media)
        .await
        .map_err(|e| e.to_string())?;
    if root.starts_with(&media) || media.starts_with(&root) {
        return Err("restore root must be separate from the media spool".into());
    }
    Ok(root)
}

async fn run_restore(
    state: AppState,
    generation: RemoteGeneration,
    destination: PathBuf,
    id: String,
) {
    let result = async {
        state
            .catalog
            .update_job(
                &id,
                "running",
                10,
                Some("download and integrity verification started"),
            )
            .await
            .map_err(|e| e.to_string())?;
        let mut command = state
            .catalog
            .worker_command(&state.config.worker_path, Some(&id))
            .await
            .map_err(|e| e.to_string())?;
        command
            .env("COLD_VTL_RESTORE_JOB_ID", &id)
            .arg("restore")
            .arg("--barcode")
            .arg(generation.barcode.as_str())
            .arg("--generation")
            .arg(generation.generation.to_string())
            .arg("--manifest-sha256")
            .arg(&generation.manifest_sha256)
            .arg("--destination")
            .arg(destination);
        let output = worker_output(&mut command).await?;
        let body: serde_json::Value = serde_json::from_str(&output).map_err(|e| e.to_string())?;
        if body.get("status").and_then(|v| v.as_str()) != Some("restored") {
            return Err("worker did not confirm integrity of restored files".into());
        }
        state
            .catalog
            .finish_job(&id, "completed", &output, None, None)
            .await
            .map_err(|e| e.to_string())
    }
    .await;
    if let Err(error) = result
        && let Err(failure) = state
            .catalog
            .finish_job(&id, "failed", &error, None, None)
            .await
    {
        eprintln!("Restore job {id} failed ({error}) and could not be recorded: {failure}");
    }
}

pub async fn worker_output(command: &mut Command) -> Result<String, String> {
    let output = command
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppConfig, catalog::Catalog};
    use std::sync::Arc;
    #[tokio::test]
    async fn deleted_cartridge_reconciles_without_local_source_or_state_resurrection() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("remote-reconcile-{}", Uuid::new_v4()));
        let catalog = Catalog::open(&root.join("catalog.db")).await.unwrap();
        let cartridge = Cartridge::new(Barcode::parse("TAPE01").unwrap(), 1024).unwrap();
        catalog.create_cartridge(&cartridge).await.unwrap();
        sqlx::query("INSERT INTO cartridge_deletions(vtl_id,barcode,request_id) VALUES(?,'TAPE01','deleted')")
            .bind(catalog.vtl_id().await.unwrap().to_string()).execute(&catalog.pool()).await.unwrap();
        let worker = root.join("worker");
        let quiesce = root.join("quiesce");
        std::fs::write(&worker, r#"#!/bin/sh
[ "$*" = "reconcile --barcode TAPE01 --remote-only" ] || exit 2
printf '%s' '{"generations":[{"barcode":"TAPE01","generation":7,"format_version":2,"manifest_key":"manifest","manifest_sha256":"checksum","logical_size_bytes":1024,"file_count":5,"segment_count":1,"local_match":false}],"local_size_bytes":0}'
"#).unwrap();
        std::fs::write(&quiesce, "#!/bin/sh\nexit 0\n").unwrap();
        for path in [&worker, &quiesce] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let state = AppState {
            catalog: catalog.clone(),
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(AppConfig {
                storage_socket: None,
                media_root: root.join("missing-media"),
                restore_root: root.join("restores"),
                worker_path: worker,
                quiesce_check: Some(quiesce),
                maintenance_helper: None,
                cartridge_helper: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        };
        catalog
            .reserve_job("sync", "reconcile", &cartridge, None, None)
            .await
            .unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        run_reconcile(state, cartridge.clone(), "sync".into(), None, stopped).await;
        let job = catalog.job("sync").await.unwrap().unwrap();
        assert_eq!(job.status, "completed", "{:?}", job.message);
        assert_eq!(
            catalog
                .cartridge(&cartridge.barcode)
                .await
                .unwrap()
                .unwrap(),
            cartridge
        );
        assert!(catalog.local_deleted("TAPE01").await.unwrap());
        assert_eq!(
            catalog.generations(&cartridge.barcode).await.unwrap()[0].generation,
            7
        );
        catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_the_synchronisation_unit_of_that_cartridge_matches() {
        let exec = |args: &str| {
            format!(
                "{{ path=/usr/local/libexec/cold-vtl-worker-run ; argv[]=/usr/local/libexec/cold-vtl-worker-run --execute {args} ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }}"
            )
        };
        assert!(reconcile_unit_matches(
            &exec("reconcile --barcode TAPE01 --source /opt/mhvtl/TAPE01"),
            "TAPE01"
        ));
        assert!(reconcile_unit_matches(
            &exec("reconcile --barcode TAPE01 --remote-only"),
            "TAPE01"
        ));
        for (args, barcode) in [
            (
                "reconcile --barcode TAPE012 --source /opt/mhvtl/TAPE012",
                "TAPE01",
            ),
            (
                "reconcile --barcode TAPE02 --source /opt/mhvtl/TAPE01",
                "TAPE01",
            ),
            (
                "export-directory --barcode TAPE01 --source /opt/mhvtl/TAPE01",
                "TAPE01",
            ),
            (
                "restore --barcode TAPE01 --destination /restore/reconcile",
                "TAPE01",
            ),
            ("reconcile --source /opt/mhvtl/TAPE01", "TAPE01"),
        ] {
            assert!(!reconcile_unit_matches(&exec(args), barcode), "{args}");
        }
        assert!(!reconcile_unit_matches("", "TAPE01"));
        assert!(!reconcile_unit_matches(
            "reconcile --barcode TAPE01",
            "TAPE01"
        ));
    }

    #[tokio::test]
    async fn unit_sweep_ends_within_its_budget() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("unit-sweep-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let vtl = "00000000-0000-4000-8000-000000000001";
        let program = root.join("systemctl");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  list-units) echo 'vaultcenter-worker-{vtl}-aa.service loaded active running x'; echo 'vaultcenter-worker-{vtl}-bb.service loaded active running x' ;;\n  show) echo '{{ path=/usr/local/libexec/cold-vtl-worker-run ; argv[]=/usr/local/libexec/cold-vtl-worker-run --execute reconcile --barcode TAPE01 --remote-only ; ignore_errors=no }}' ;;\n  stop) sleep 30 ;;\nesac\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let started = std::time::Instant::now();
        let sweep = sweep_units(
            &program,
            vtl,
            &["TAPE01".to_owned()],
            Duration::from_millis(500),
            Duration::from_secs(5),
        )
        .await;
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(
            sweep,
            Sweep {
                uninspected: vec![format!("vaultcenter-worker-{vtl}-bb.service")],
                unstopped: vec![format!("vaultcenter-worker-{vtl}-aa.service")],
                stopped: vec![],
            }
        );
        // A slow call is cut at its own timeout and the sweep goes on.
        let sweep = sweep_units(
            &program,
            vtl,
            &["TAPE01".to_owned()],
            Duration::from_secs(5),
            Duration::from_millis(300),
        )
        .await;
        assert_eq!(sweep.unstopped.len(), 2);
        assert!(sweep.uninspected.is_empty() && sweep.stopped.is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn destination_cannot_escape_restore_root() {
        assert!(destination_name("CVT001L8-g2-test"));
        for name in [
            "",
            "..",
            "../spool",
            "/opt/mhvtl/CVT001L8",
            "a/b",
            "a\\b",
            ".",
            "a b",
        ] {
            assert!(!destination_name(name));
        }
    }

    #[tokio::test]
    async fn rejects_unavailable_generations_existing_paths_and_active_jobs() {
        let root = std::env::temp_dir().join(format!("cold-vtl-guards-{}", Uuid::new_v4()));
        let media = root.join("media");
        tokio::fs::create_dir_all(&media).await.unwrap();
        let catalog = Catalog::open(&root.join("catalog.db")).await.unwrap();
        let cartridge = Cartridge::new(Barcode::parse("TAPE01").unwrap(), 1024).unwrap();
        catalog.create_cartridge(&cartridge).await.unwrap();
        let state = AppState {
            catalog: catalog.clone(),
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(AppConfig {
                storage_socket: None,
                media_root: media.clone(),
                restore_root: root.join("restores"),
                worker_path: PathBuf::from("/must-not-be-invoked"),
                quiesce_check: None,
                maintenance_helper: None,
                cartridge_helper: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        };
        assert_eq!(
            ensure_quiescent(&state).await.err().unwrap().status,
            StatusCode::CONFLICT
        );
        let request = |generation, destination| RestoreRequest {
            generation,
            destination,
        };
        for (generation, status) in [(0, StatusCode::BAD_REQUEST), (1, StatusCode::CONFLICT)] {
            let error = restore(
                State(state.clone()),
                Path("TAPE01".into()),
                Json(request(generation, None)),
            )
            .await
            .err()
            .unwrap();
            assert_eq!(error.status, status);
        }
        let generation = RemoteGeneration {
            barcode: cartridge.barcode.clone(),
            generation: 1,
            format_version: 2,
            manifest_key: "manifest".into(),
            manifest_sha256: "a".repeat(64),
            logical_size_bytes: 0,
            file_count: 0,
            segment_count: 0,
            local_match: false,
        };
        catalog
            .reserve_job("sync", "reconcile", &cartridge, None, None)
            .await
            .unwrap();
        catalog
            .finish_job(
                "sync",
                "completed",
                "ok",
                Some(&cartridge),
                Some(&[generation]),
            )
            .await
            .unwrap();
        let existing = state.config.restore_root.join("existing");
        tokio::fs::create_dir_all(&existing).await.unwrap();
        tokio::fs::write(existing.join("sentinel"), "preserve")
            .await
            .unwrap();
        for (destination, status) in [
            ("../media", StatusCode::BAD_REQUEST),
            ("existing", StatusCode::CONFLICT),
        ] {
            let error = restore(
                State(state.clone()),
                Path("TAPE01".into()),
                Json(request(1, Some(destination.into()))),
            )
            .await
            .err()
            .unwrap();
            assert_eq!(error.status, status);
        }
        catalog
            .reserve_job("active", "restore", &cartridge, Some(1), None)
            .await
            .unwrap();
        let error = restore(
            State(state.clone()),
            Path("TAPE01".into()),
            Json(request(1, None)),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(catalog.list_jobs().await.unwrap().len(), 2);
        assert_eq!(
            catalog
                .cartridge(&cartridge.barcode)
                .await
                .unwrap()
                .unwrap(),
            cartridge
        );
        assert_eq!(
            tokio::fs::read_to_string(existing.join("sentinel"))
                .await
                .unwrap(),
            "preserve"
        );
        assert!(restore_root(&media.join("restores"), &media).await.is_err());
        drop(state);
        drop(catalog);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}

#[cfg(test)]
mod background_tests {
    use super::*;
    use crate::s3_settings::test_support::state_fixture;

    /// Creates `barcodes`, flagged for synchronisation; `deleted` ones are
    /// locally deleted (synchronised from the bucket alone, no drive check).
    async fn flagged_cartridges(state: &AppState, barcodes: &[&str], deleted: bool) {
        let pool = state.catalog.pool();
        let vtl = state.catalog.vtl_id().await.unwrap().to_string();
        for barcode in barcodes {
            let cartridge = Cartridge::new(Barcode::parse(*barcode).unwrap(), 1024).unwrap();
            state.catalog.create_cartridge(&cartridge).await.unwrap();
            if deleted {
                sqlx::query("INSERT INTO cartridge_deletions(vtl_id,barcode,request_id) VALUES(?,?,'deleted')")
                    .bind(&vtl).bind(barcode).execute(&pool).await.unwrap();
            }
            sqlx::query("INSERT INTO s3_reconcile_required(vtl_id,barcode) VALUES(?,?)")
                .bind(&vtl)
                .bind(barcode)
                .execute(&pool)
                .await
                .unwrap();
        }
    }

    /// Worker whose synchronisation of a barcode holds until `release`.
    fn holding_worker(root: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(
            root.join("worker"),
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  storage-access) echo '{{\"readable\":true,\"writable\":true}}' ;;\n  reconcile) while [ ! -f '{release}'-\"$3\" ]; do sleep 0.02; done; echo '{{\"generations\":[],\"local_size_bytes\":0}}' ;;\n  *) exit 1 ;;\nesac\n",
                release = root.join("release").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(root.join("worker"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }

    fn release(root: &std::path::Path, barcode: &str) {
        std::fs::write(root.join(format!("release-{barcode}")), "").unwrap();
    }

    /// A reconcile job row for `barcode` in `status`, last updated `age` ago.
    async fn reconcile_job(state: &AppState, id: &str, barcode: &str, status: &str, age: &str) {
        sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id,updated_at) VALUES(?,'reconcile',?,?,?,datetime('now',?))")
            .bind(id).bind(barcode).bind(status)
            .bind(state.catalog.vtl_id().await.unwrap().to_string()).bind(age)
            .execute(&state.catalog.pool()).await.unwrap();
    }

    /// Waits (10 s at most) until the synchronisation jobs started by the
    /// code under test (not the inserted rows), by barcode, have `statuses`.
    async fn jobs_reach(state: &AppState, statuses: &[(&str, &str)]) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let expected: Vec<(String, String)> = statuses
            .iter()
            .map(|(b, s)| (b.to_string(), s.to_string()))
            .collect();
        loop {
            let jobs: Vec<(String, String)> = sqlx::query_as(
                "SELECT subject,status FROM jobs WHERE kind='reconcile' AND cartridge_id IS NOT NULL ORDER BY subject",
            )
            .fetch_all(&state.catalog.pool())
            .await
            .unwrap();
            if jobs == expected {
                return;
            }
            assert!(tokio::time::Instant::now() < deadline, "{jobs:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn flagged(state: &AppState) -> Vec<String> {
        sqlx::query_scalar("SELECT barcode FROM s3_reconcile_required ORDER BY barcode")
            .fetch_all(&state.catalog.pool())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn flagged_cartridges_synchronise_two_at_a_time_and_show_their_copy_state() {
        let (state, root) = state_fixture().await;
        let pool = state.catalog.pool();
        flagged_cartridges(&state, &["TAPE01", "TAPE02", "TAPE03"], false).await;
        let cartridge = Cartridge::new(Barcode::parse("TAPE04").unwrap(), 1024).unwrap();
        state.catalog.create_cartridge(&cartridge).await.unwrap();
        reconcile_job(&state, "sync1", "TAPE01", "running", "0 seconds").await;
        assert_eq!(
            reconcile_candidates(&state).await.unwrap(),
            vec!["TAPE02", "TAPE03"]
        );
        assert_eq!(running_syncs(&state).await.unwrap(), 1);
        sqlx::query("UPDATE jobs SET status='completed' WHERE id='sync1'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            reconcile_candidates(&state).await.unwrap(),
            vec!["TAPE01", "TAPE02", "TAPE03"]
        );
        assert_eq!(running_syncs(&state).await.unwrap(), 0);

        let destination = state.catalog.destination().await.unwrap().id;
        sqlx::query(
            "UPDATE storage_destinations SET identity='{\"bucket\":\"vc-week-41\"}' WHERE id=?",
        )
        .bind(&destination)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO remote_generations(cartridge_id,vtl_id,barcode,destination_id,generation,metadata,checked_at) SELECT id,vtl_id,barcode,?,3,'{}','2026-10-12 10:00:00' FROM cartridges WHERE barcode='TAPE04'")
            .bind(&destination).execute(&pool).await.unwrap();
        let views =
            serde_json::to_value(state.catalog.cartridge_views(None).await.unwrap()).unwrap();
        let view = |barcode: &str| {
            views
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["barcode"] == barcode)
                .unwrap()
                .clone()
        };
        assert_eq!(view("TAPE02")["reconcile_pending"], true);
        assert_eq!(view("TAPE04")["reconcile_pending"], false);
        assert_eq!(view("TAPE04")["last_copy"]["bucket"], "vc-week-41");
        assert_eq!(view("TAPE04")["last_copy"]["generation"], 3);
        assert_eq!(
            view("TAPE04")["last_copy"]["checked_at"],
            "2026-10-12 10:00:00"
        );
        assert_eq!(view("TAPE02")["last_copy"], serde_json::Value::Null);
        pool.close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn the_background_pass_synchronises_flagged_cartridges_and_clears_their_flag() {
        let (state, root) = state_fixture().await;
        flagged_cartridges(&state, &["TAPE01", "TAPE02", "TAPE03"], true).await;
        holding_worker(&root);
        crate::s3_settings::background_synchronisation(&state)
            .await
            .unwrap();
        jobs_reach(&state, &[("TAPE01", "running"), ("TAPE02", "running")]).await;
        // Two synchronisations at once: another pass starts nothing, and a
        // capped start (a stale candidate list) is refused.
        reconcile_flagged(state.clone()).await;
        assert!(
            start_reconcile_locked(&state, "TAPE03".into(), true)
                .await
                .is_err()
        );
        jobs_reach(&state, &[("TAPE01", "running"), ("TAPE02", "running")]).await;
        release(&root, "TAPE01");
        jobs_reach(&state, &[("TAPE01", "completed"), ("TAPE02", "running")]).await;
        assert_eq!(flagged(&state).await, vec!["TAPE02", "TAPE03"]);
        // The next pass (the monitor's) takes the remaining cartridge.
        reconcile_flagged(state.clone()).await;
        jobs_reach(
            &state,
            &[
                ("TAPE01", "completed"),
                ("TAPE02", "running"),
                ("TAPE03", "running"),
            ],
        )
        .await;
        // Released one at a time, so each end is observed on its own.
        release(&root, "TAPE02");
        jobs_reach(
            &state,
            &[
                ("TAPE01", "completed"),
                ("TAPE02", "completed"),
                ("TAPE03", "running"),
            ],
        )
        .await;
        release(&root, "TAPE03");
        jobs_reach(
            &state,
            &[
                ("TAPE01", "completed"),
                ("TAPE02", "completed"),
                ("TAPE03", "completed"),
            ],
        )
        .await;
        assert!(flagged(&state).await.is_empty());
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn refused_or_recently_failed_cartridges_do_not_hold_back_the_others() {
        let (state, root) = state_fixture().await;
        // TAPE01 is still local: its drive check is refused (no mhVTL here).
        flagged_cartridges(&state, &["TAPE01"], false).await;
        flagged_cartridges(&state, &["TAPE02", "TAPE03", "TAPE04"], true).await;
        // TAPE02's synchronisation failed 10 minutes ago: not retried yet.
        reconcile_job(&state, "failed-recently", "TAPE02", "failed", "-10 minutes").await;
        holding_worker(&root);
        reconcile_flagged(state.clone()).await;
        jobs_reach(&state, &[("TAPE03", "running"), ("TAPE04", "running")]).await;
        // After 30 minutes, the failed cartridge is a candidate again.
        assert_eq!(reconcile_candidates(&state).await.unwrap(), vec!["TAPE01"]);
        sqlx::query(
            "UPDATE jobs SET updated_at=datetime('now','-31 minutes') WHERE id='failed-recently'",
        )
        .execute(&state.catalog.pool())
        .await
        .unwrap();
        assert_eq!(
            reconcile_candidates(&state).await.unwrap(),
            vec!["TAPE01", "TAPE02"]
        );
        release(&root, "TAPE03");
        jobs_reach(&state, &[("TAPE03", "completed"), ("TAPE04", "running")]).await;
        release(&root, "TAPE04");
        jobs_reach(&state, &[("TAPE03", "completed"), ("TAPE04", "completed")]).await;
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn cancelled_synchronisations_are_retried_at_once_in_either_language() {
        let (state, root) = state_fixture().await;
        flagged_cartridges(&state, &["TAPE02", "TAPE03", "TAPE04"], true).await;
        for (id, barcode) in [
            ("cancelled", "TAPE02"),
            ("cancelled-fr", "TAPE03"),
            ("failed", "TAPE04"),
        ] {
            reconcile_job(&state, id, barcode, "failed", "-1 minutes").await;
        }
        for (id, message) in [
            ("cancelled", SYNC_CANCELLED),
            ("cancelled-fr", SYNC_CANCELLED_FRENCH),
            (
                "failed",
                "Recall this entire bucket in Storage S3 and wait until it is readable.",
            ),
        ] {
            sqlx::query("UPDATE jobs SET message=? WHERE id=?")
                .bind(message)
                .bind(id)
                .execute(&state.catalog.pool())
                .await
                .unwrap();
        }
        assert_eq!(
            reconcile_candidates(&state).await.unwrap(),
            vec!["TAPE02", "TAPE03"]
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn sending_a_flagged_cartridge_starts_its_synchronisation_outside_the_limit() {
        let (state, root) = state_fixture().await;
        flagged_cartridges(&state, &["TAPE01"], true).await;
        // Two background synchronisations already fill the limit.
        reconcile_job(&state, "other1", "TAPE08", "running", "0 seconds").await;
        reconcile_job(&state, "other2", "TAPE09", "running", "0 seconds").await;
        holding_worker(&root);
        let refused = crate::s3_settings::require_reconciled(&state, "TAPE01")
            .await
            .unwrap_err();
        assert_eq!(
            refused.message,
            "The synchronisation of TAPE01 has started. Start the export again when it has finished."
        );
        jobs_reach(&state, &[("TAPE01", "running")]).await;
        // While its own synchronisation runs, the send says so.
        for check in [
            crate::s3_settings::require_reconciled(&state, "TAPE01").await,
            crate::s3_settings::require_not_synchronising(&state, "TAPE01").await,
        ] {
            assert_eq!(
                check.unwrap_err().message,
                "The cartridge is synchronising with the bucket. Start the export again when the synchronisation has finished."
            );
        }
        release(&root, "TAPE01");
        jobs_reach(&state, &[("TAPE01", "completed")]).await;
        crate::s3_settings::require_reconciled(&state, "TAPE01")
            .await
            .unwrap();
        crate::s3_settings::require_not_synchronising(&state, "TAPE01")
            .await
            .unwrap();
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }
    #[tokio::test]
    async fn sealing_a_cartridge_reports_its_own_synchronisation() {
        let (state, _root) = state_fixture().await;
        flagged_cartridges(&state, &["TAPE01"], false).await;
        reconcile_job(&state, "own", "TAPE01", "running", "0 seconds").await;
        // Sealing says so, instead of "another job is active".
        let refused = crate::seal_cartridge(
            axum::extract::State(state.clone()),
            axum::extract::Path("TAPE01".to_owned()),
        )
        .await
        .map(|_| ())
        .unwrap_err();
        assert_eq!(
            refused.message,
            "The cartridge is synchronising with the bucket. Retry once the synchronisation has finished."
        );
        state.catalog.pool().close().await;
    }

    #[tokio::test]
    async fn a_started_synchronisation_asks_to_retry_the_refused_operation() {
        let (state, root) = state_fixture().await;
        flagged_cartridges(&state, &["TAPE01"], true).await;
        holding_worker(&root);
        // The wording is neutral when the refused operation is not a send.
        let refused = crate::s3_settings::require_reconciled_for(
            &state,
            "TAPE01",
            crate::s3_settings::Retry::Operation,
        )
        .await
        .unwrap_err();
        assert_eq!(
            refused.message,
            "The synchronisation of TAPE01 has started. Retry once it has finished."
        );
        jobs_reach(&state, &[("TAPE01", "running")]).await;
        release(&root, "TAPE01");
        jobs_reach(&state, &[("TAPE01", "completed")]).await;
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).ok();
    }
}
