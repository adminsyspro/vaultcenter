use crate::{ApiError, AppState};
use axum::{Json, extract::State, http::StatusCode};
use cold_vtl_core::{Barcode, Cartridge};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{process::Stdio, time::Duration};
use tokio::io::AsyncWriteExt;
#[cfg(test)]
use tokio::process::Command;

const MIB: u64 = 1024 * 1024;
const MAX_CAPACITY_MIB: u64 = 16_777_216;
const MEDIA_TYPES: &[&str] = &["LTO5", "LTO6", "LTO7", "LTO8", "LTO9"];

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    barcode: String,
    media_type: String,
    capacity_bytes: u64,
    segment_size_mib: u32,
    #[serde(default = "default_multipart_part_size_mib")]
    multipart_part_size_mib: u32,
}
fn default_multipart_part_size_mib() -> u32 {
    256
}
impl Create {
    fn validate(&self) -> Result<Barcode, ApiError> {
        let barcode = Barcode::parse(self.barcode.clone()).map_err(ApiError::bad_request)?;
        if !MEDIA_TYPES.contains(&self.media_type.as_str()) {
            return Err(ApiError::bad_request("Unsupported LTO media type."));
        }
        if self.capacity_bytes < MIB
            || self.capacity_bytes > MAX_CAPACITY_MIB * MIB
            || !self.capacity_bytes.is_multiple_of(MIB)
        {
            return Err(ApiError::bad_request(
                "Capacity must be a whole number of MiB between 1 and 16,777,216.",
            ));
        }
        if !(16..=1_048_576).contains(&self.segment_size_mib) {
            return Err(ApiError::bad_request(
                "Segment size must be between 16 MiB and 1 TiB.",
            ));
        }
        if !(16..=1024).contains(&self.multipart_part_size_mib)
            || self.segment_size_mib.div_ceil(self.multipart_part_size_mib) > 10_000
        {
            return Err(ApiError::bad_request(
                "Multipart parts must be 16 to 1,024 MiB, with at most 10,000 parts per segment.",
            ));
        }
        Ok(barcode)
    }
    fn matches(&self, cartridge: &Cartridge) -> bool {
        cartridge.media_type.as_deref() == Some(&self.media_type)
            && cartridge.capacity_bytes == self.capacity_bytes
            && cartridge.segment_size_mib == self.segment_size_mib
            && cartridge.multipart_part_size_mib == self.multipart_part_size_mib
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BatchCreate {
    request_id: String,
    count: u32,
    media_type: String,
    capacity_bytes: u64,
    segment_size_mib: u32,
    #[serde(default = "default_multipart_part_size_mib")]
    multipart_part_size_mib: u32,
}
impl BatchCreate {
    fn validate(&self) -> Result<(), ApiError> {
        if !(1..=64).contains(&self.count)
            || self.request_id.len() != 32
            || !self
                .request_id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(ApiError::bad_request(
                "Invalid batch: choose between 1 and 64 cartridges.",
            ));
        }
        Create {
            barcode: "CVT001L8".into(),
            media_type: self.media_type.clone(),
            capacity_bytes: self.capacity_bytes,
            segment_size_mib: self.segment_size_mib,
            multipart_part_size_mib: self.multipart_part_size_mib,
        }
        .validate()?;
        Ok(())
    }
}

pub async fn batch(
    State(state): State<AppState>,
    Json(request): Json<BatchCreate>,
) -> Result<Json<Value>, ApiError> {
    request.validate()?;
    let id = state.catalog.vtl_id().await?.to_string();
    let state = crate::vtls::installed_state(state, id).await?;
    let _lock = state
        .operations
        .try_lock()
        .map_err(|_| ApiError::conflict("Another operation is being prepared."))?;
    idle(&state).await?;
    creation_readiness(&state).await?;
    let result = invoke(
        &state,
        "batch",
        serde_json::to_value(&request).map_err(ApiError::bad_request)?,
    )
    .await?;
    Ok(Json(batch_result(&state, result, Some(&request)).await?))
}

async fn batch_result(
    state: &AppState,
    mut result: Value,
    expected: Option<&BatchCreate>,
) -> Result<Value, ApiError> {
    let fail = || {
        ApiError::conflict(
            "The batch is not confirmed in the catalogue. Reload its settings before resuming.",
        )
    };
    let id = state.catalog.vtl_id().await?.to_string();
    if result["request"]["vtl_id"] != id {
        return Err(fail());
    }
    result["request"]
        .as_object_mut()
        .ok_or_else(fail)?
        .remove("vtl_id");
    let request: BatchCreate =
        serde_json::from_value(result["request"].clone()).map_err(|_| fail())?;
    request.validate()?;
    if expected.is_some_and(|input| input != &request) {
        return Err(fail());
    }
    let barcodes: Vec<String> =
        serde_json::from_value(result["barcodes"].clone()).map_err(|_| fail())?;
    let completed = result["completed"].as_u64().ok_or_else(fail)? as usize;
    if barcodes.len() != request.count as usize
        || completed > barcodes.len()
        || barcodes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != barcodes.len()
        || result["phase"]
            != if completed == barcodes.len() {
                "completed"
            } else {
                "creating"
            }
    {
        return Err(fail());
    }
    for (index, barcode) in barcodes.iter().enumerate() {
        let code = Barcode::parse(barcode.clone()).map_err(|_| fail())?;
        if index < completed {
            let tape = state.catalog.cartridge(&code).await?.ok_or_else(fail)?;
            let expected = Create {
                barcode: barcode.clone(),
                media_type: request.media_type.clone(),
                capacity_bytes: request.capacity_bytes,
                segment_size_mib: request.segment_size_mib,
                multipart_part_size_mib: request.multipart_part_size_mib,
            };
            if !expected.matches(&tape) {
                return Err(fail());
            }
        }
    }
    Ok(result)
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Options {
    configured: bool,
    ready: bool,
    #[serde(default)]
    online_creation: bool,
    reason: Option<String>,
    media_types: Vec<String>,
    default_media_type: Option<String>,
    default_capacity_mib: u64,
    default_segment_size_mib: u32,
    min_capacity_mib: u64,
    max_capacity_mib: u64,
    min_segment_size_mib: u32,
    max_segment_size_mib: u32,
    default_multipart_part_size_mib: u32,
    min_multipart_part_size_mib: u32,
    max_multipart_part_size_mib: u32,
    free_slots: u64,
    #[serde(default)]
    pending_creation: Option<Create>,
    #[serde(default)]
    pending_batch: Option<Value>,
    #[serde(default)]
    storage: Option<Value>,
}
impl Options {
    fn unavailable(configured: bool, reason: impl Into<String>) -> Self {
        Self {
            configured,
            ready: false,
            online_creation: false,
            reason: Some(reason.into()),
            media_types: vec![],
            default_media_type: None,
            default_capacity_mib: 2048,
            default_segment_size_mib: 1_048_576,
            min_capacity_mib: 1,
            max_capacity_mib: MAX_CAPACITY_MIB,
            min_segment_size_mib: 16,
            max_segment_size_mib: 1_048_576,
            default_multipart_part_size_mib: 256,
            min_multipart_part_size_mib: 16,
            max_multipart_part_size_mib: 1024,
            free_slots: 0,
            pending_creation: None,
            pending_batch: None,
            storage: None,
        }
    }
}

pub(crate) async fn invoke(
    state: &AppState,
    action: &str,
    mut input: Value,
) -> Result<Value, ApiError> {
    let helper = state.config.cartridge_helper.as_ref().ok_or_else(|| {
        ApiError::conflict("The cartridge creation controller is not configured.")
    })?;
    input["vtl_id"] = json!(state.catalog.vtl_id().await?.to_string());
    let mut child = crate::vtl_runtime::helper_command(state, helper)
        .await?
        .arg(action)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Creation owns a durable journal and lock; it must survive a disconnected API.
        .kill_on_drop(!matches!(action, "create" | "batch" | "delete-local"))
        .spawn()
        .map_err(ApiError::conflict)?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input.to_string().as_bytes())
            .await
            .map_err(ApiError::conflict)?;
    }
    let output = tokio::time::timeout(Duration::from_secs(if matches!(action,"create"|"batch"|"delete-local") { 300 } else { 15 }), child.wait_with_output())
        .await.map_err(|_| ApiError::conflict("The controller did not respond in time. Check the library, then resubmit the request with the same settings to resume the operation."))?
        .map_err(ApiError::conflict)?;
    let body: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        ApiError::conflict(
            "The controller returned an invalid response. Check the library before trying again.",
        )
    })?;
    if !output.status.success() || body.get("error").is_some() {
        return Err(ApiError::conflict(
            body.get("error")
                .and_then(Value::as_str)
                .unwrap_or("The cartridge operation failed."),
        ));
    }
    Ok(body)
}

pub(crate) async fn idle(state: &AppState) -> Result<(), ApiError> {
    crate::archive::media_idle(state).await?;
    if state
        .catalog
        .list_jobs()
        .await?
        .iter()
        .any(|job| matches!(job.status.as_str(), "queued" | "running"))
    {
        return Err(ApiError::conflict(
            "Wait for all active VTL tasks to finish before creating a cartridge.",
        ));
    }
    Ok(())
}

async fn managed_maintenance(state: &AppState) -> Result<(), ApiError> {
    if state.config.maintenance_helper.is_none() {
        return Err(ApiError::conflict(
            "Managed maintenance is not configured. Configure it before creating a cartridge.",
        ));
    }
    crate::maintenance::check(state).await.map_err(|error| {
        if error.message == "enter managed maintenance before changing local media" {
            ApiError::conflict("Enable managed maintenance before creating a cartridge.")
        } else {
            error
        }
    })
}

// Old helpers retain their maintenance contract. A live-capable controller
// rechecks its own runtime and locks on every create/batch request.
async fn creation_readiness(state: &AppState) -> Result<(), ApiError> {
    match managed_maintenance(state).await {
        Ok(()) => Ok(()),
        Err(maintenance_error) => {
            let value = invoke(state, "options", json!({})).await;
            if let Ok(value) = value
                && let Ok(options) = serde_json::from_value::<Options>(value)
            {
                if options.configured && options.ready && options.online_creation {
                    return Ok(());
                }
                if options.online_creation {
                    return Err(ApiError::conflict(options.reason.unwrap_or_else(|| {
                        "Online creation is unavailable for this library.".into()
                    })));
                }
            }
            Err(maintenance_error)
        }
    }
}

pub async fn options(State(state): State<AppState>) -> Result<Json<Options>, ApiError> {
    let id = state.catalog.vtl_id().await?.to_string();
    let state = crate::vtls::installed_state(state, id).await?;
    if state.config.cartridge_helper.is_none() {
        return Ok(Json(Options::unavailable(
            false,
            "The cartridge creation controller is not configured.",
        )));
    }
    let mut options = match invoke(&state, "options", json!({})).await {
        Ok(value) => serde_json::from_value::<Options>(value)
            .map_err(|_| ApiError::conflict("The controller returned invalid creation options."))?,
        Err(error) => return Ok(Json(Options::unavailable(true, error.message))),
    };
    if let Some(batch) = options.pending_batch.take() {
        options.pending_batch = Some(batch_result(&state, batch, None).await?);
    }
    options
        .media_types
        .retain(|value| MEDIA_TYPES.contains(&value.as_str()));
    options.max_capacity_mib = options.max_capacity_mib.min(MAX_CAPACITY_MIB);
    options.min_capacity_mib = options.min_capacity_mib.max(1);
    options.min_segment_size_mib = options.min_segment_size_mib.max(16);
    options.max_segment_size_mib = options.max_segment_size_mib.min(1_048_576);
    options.min_multipart_part_size_mib = options.min_multipart_part_size_mib.max(16);
    options.max_multipart_part_size_mib = options.max_multipart_part_size_mib.min(1024);
    if !options
        .default_media_type
        .as_ref()
        .is_some_and(|value| options.media_types.contains(value))
    {
        options.default_media_type = options.media_types.last().cloned();
    }
    let readiness = async {
        idle(&state).await?;
        if !options.online_creation {
            managed_maintenance(&state).await?;
        }
        if let Some(pending) = &options.pending_creation {
            pending.validate()?;
            if !options.media_types.contains(&pending.media_type)
                || !(options.min_capacity_mib..=options.max_capacity_mib)
                    .contains(&(pending.capacity_bytes / MIB))
                || !(options.min_segment_size_mib..=options.max_segment_size_mib)
                    .contains(&pending.segment_size_mib)
                || !(options.min_multipart_part_size_mib..=options.max_multipart_part_size_mib)
                    .contains(&pending.multipart_part_size_mib)
            {
                return Err(ApiError::conflict(
                    "The pending creation's settings are not compatible with this library.",
                ));
            }
        }
        if options.media_types.is_empty()
            || (options.free_slots == 0
                && options.pending_creation.is_none()
                && options.pending_batch.is_none())
        {
            return Err(ApiError::conflict(
                "No compatible media type or free slot is available.",
            ));
        }
        Ok::<(), ApiError>(())
    }
    .await;
    if let Err(error) = readiness {
        options.ready = false;
        options.reason = Some(error.message);
    }
    Ok(Json(options))
}

pub async fn create(
    State(state): State<AppState>,
    Json(request): Json<Create>,
) -> Result<(StatusCode, Json<Cartridge>), ApiError> {
    let barcode = request.validate()?;
    let id = state.catalog.vtl_id().await?.to_string();
    let state = crate::vtls::installed_state(state, id).await?;
    let _lock = state
        .operations
        .try_lock()
        .map_err(|_| ApiError::conflict("Another operation is being prepared."))?;
    idle(&state).await?;
    creation_readiness(&state).await?;
    let existing = state.catalog.cartridge(&barcode).await?;
    if existing
        .as_ref()
        .is_some_and(|value| !request.matches(value))
    {
        return Err(ApiError::conflict(format!(
            "Cartridge {barcode} already exists with different settings."
        )));
    }
    // The controller owns idempotency after a crash, including after its catalog commit.
    // Always reach it for an identical retry, even if the library now has no free slots.
    invoke(
        &state,
        "create",
        serde_json::to_value(&request).map_err(ApiError::bad_request)?,
    )
    .await?;
    let cartridge = state.catalog.cartridge(&barcode).await?
        .ok_or_else(|| ApiError::conflict("The controller did not confirm the creation in the catalogue. Resubmit the request with the same settings to resume the operation."))?;
    if !request.matches(&cartridge) {
        return Err(ApiError::conflict(
            "The catalogue settings do not match the requested cartridge.",
        ));
    }
    Ok((StatusCode::CREATED, Json(cartridge)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn batches_reject_invalid_count_identity_and_geometry() {
        let base = json!({"request_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","count":4,"media_type":"LTO8","capacity_bytes":2147483648_u64,"segment_size_mib":1048576,"multipart_part_size_mib":256});
        assert!(
            serde_json::from_value::<BatchCreate>(base.clone())
                .unwrap()
                .validate()
                .is_ok()
        );
        for (field, value) in [
            ("count", json!(0)),
            ("count", json!(65)),
            ("request_id", json!("../bad")),
            ("capacity_bytes", json!(1)),
            ("multipart_part_size_mib", json!(16)),
        ] {
            let mut input = base.clone();
            input[field] = value;
            assert!(
                serde_json::from_value::<BatchCreate>(input)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
        let mut input = base;
        input["barcode"] = json!("MAN001L8");
        assert!(serde_json::from_value::<BatchCreate>(input).is_err());
    }

    #[test]
    fn large_segments_validate_part_geometry_and_preserve_legacy_requests() {
        for (segment, part, valid) in [
            (1048576, 256, true),
            (160000, 16, true),
            (160001, 16, false),
            (1048577, 256, false),
            (256, 15, false),
            (256, 1025, false),
        ] {
            let value = json!({"barcode":"NEW001L8","media_type":"LTO8","capacity_bytes":2147483648_u64,"segment_size_mib":segment,"multipart_part_size_mib":part});
            let parsed = serde_json::from_value::<Create>(value);
            assert_eq!(
                parsed.is_ok_and(|input| input.validate().is_ok()),
                valid,
                "segment={segment} part={part}"
            );
        }
        let legacy: Create = serde_json::from_value(json!({"barcode":"NEW001L8","media_type":"LTO8","capacity_bytes":2147483648_u64,"segment_size_mib":64})).unwrap();
        assert_eq!(
            serde_json::to_value(legacy).unwrap()["multipart_part_size_mib"],
            256
        );
    }

    #[tokio::test]
    async fn multipart_migration_preserves_existing_values_ids_and_scoped_references() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let mut before = sqlx::migrate!("./migrations");
        before.migrations = before
            .migrations
            .iter()
            .filter(|m| m.version <= 11)
            .cloned()
            .collect::<Vec<_>>()
            .into();
        before.run(&pool).await.unwrap();
        sqlx::query("INSERT INTO cartridges(id,barcode,capacity_bytes,segment_size_mib,state) VALUES('tape-id','OLD001',1234,64,'failed')").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO vtls(id,name) VALUES('other-vtl','Other')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO cartridges(id,vtl_id,barcode,capacity_bytes,segment_size_mib,state) VALUES('other-id','other-vtl','OLD001',5678,512,'writable')").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO remote_generations(cartridge_id,barcode,generation,metadata) VALUES('tape-id','OLD001',1,'{}')").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO s3_reconcile_required(barcode) VALUES('OLD001')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO jobs(id,cartridge_id,kind,subject,status,generation) VALUES('old-job','tape-id','export','OLD001','failed',1)").execute(&pool).await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let rows: Vec<(String, u32, u32)> = sqlx::query_as(
            "SELECT id,segment_size_mib,multipart_part_size_mib FROM cartridges ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![("other-id".into(), 512, 256), ("tape-id".into(), 64, 256)]
        );
        assert!(
            sqlx::query("PRAGMA foreign_key_check")
                .fetch_all(&pool)
                .await
                .unwrap()
                .is_empty()
        );
        let reference: String = sqlx::query_scalar(
            "SELECT cartridge_id FROM remote_generations WHERE barcode='OLD001'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(reference, "tape-id");
        sqlx::query(
            "INSERT INTO cartridges(barcode,capacity_bytes,state) VALUES('NEW001',1234,'writable')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let defaults:(u32,u32) = sqlx::query_as("SELECT segment_size_mib,multipart_part_size_mib FROM cartridges WHERE barcode='NEW001'").fetch_one(&pool).await.unwrap();
        assert_eq!(defaults, (1048576, 256));
        // These constraints must survive the rewrite and protect all callers, not only HTTP.
        for statement in [
            "UPDATE cartridges SET segment_size_mib=1048577 WHERE barcode='NEW001'",
            "UPDATE cartridges SET multipart_part_size_mib=16 WHERE barcode='NEW001'",
            "UPDATE cartridges SET multipart_part_size_mib=1025 WHERE barcode='NEW001'",
            "INSERT INTO cartridges(barcode,capacity_bytes,state) VALUES('NEW001',1234,'writable')",
            "INSERT INTO jobs(id,vtl_id,cartridge_id,kind,subject,status) VALUES('wrong','other-vtl','tape-id','export','OLD001','queued')",
            "DELETE FROM cartridges WHERE id='tape-id'",
        ] {
            assert!(
                sqlx::query(statement).execute(&pool).await.is_err(),
                "{statement}"
            );
        }
        sqlx::query("UPDATE cartridges SET segment_size_mib=160000,multipart_part_size_mib=16 WHERE barcode='NEW001'").execute(&pool).await.unwrap();
        assert!(
            sqlx::query("UPDATE cartridges SET segment_size_mib=160001 WHERE barcode='NEW001'")
                .execute(&pool)
                .await
                .is_err()
        );
    }

    fn request() -> Create {
        Create {
            barcode: "NEW001L8".into(),
            media_type: "LTO8".into(),
            capacity_bytes: 2048 * MIB,
            segment_size_mib: 64,
            multipart_part_size_mib: 128,
        }
    }
    #[test]
    fn rejects_fractional_mib_capacity_unknown_media_and_segment_limits() {
        for capacity in [0, MIB - 1, MIB + 1, MAX_CAPACITY_MIB * MIB + MIB, u64::MAX] {
            let mut input = request();
            input.capacity_bytes = capacity;
            assert!(input.validate().is_err());
        }
        for segment in [0, 15, 1_048_577, u32::MAX] {
            let mut input = request();
            input.segment_size_mib = segment;
            assert!(input.validate().is_err());
        }
        for media in ["LTO4", "LTO10", "lto8", ""] {
            let mut input = request();
            input.media_type = media.into();
            assert!(input.validate().is_err());
        }
        assert!(request().validate().is_ok());
    }
    fn script(path: &std::path::Path, body: &str) {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    async fn setup() -> (AppState, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("cold-vtl-cartridge-{}", uuid::Uuid::new_v4()));
        let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
            .await
            .unwrap();
        let maintenance = root.join("maintenance");
        script(
            &maintenance,
            "#!/bin/sh\ncat >/dev/null\necho '{\"phase\":\"maintenance\",\"ready\":true}'\n",
        );
        let state = AppState {
            catalog,
            operations: std::sync::Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: std::sync::Arc::new(crate::AppConfig {
                storage_socket: None,
                media_root: root.join("media"),
                restore_root: root.join("restores"),
                worker_path: root.join("worker"),
                maintenance_helper: Some(maintenance),
                cartridge_helper: Some(root.join("cartridge")),
                quiesce_check: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        };
        (state, root)
    }
    #[tokio::test]
    async fn batch_readback_refuses_missing_media_and_returns_replayable_request() {
        let (state, root) = setup().await;
        let id = state.catalog.vtl_id().await.unwrap().to_string();
        let input = json!({"vtl_id":id,"request_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","count":2,"media_type":"LTO8","capacity_bytes":2147483648_u64,"segment_size_mib":64,"multipart_part_size_mib":128});
        let result = json!({"request":input,"barcodes":["NEW001L8","NEW002L8"],"completed":1,"phase":"creating"});
        assert!(batch_result(&state, result.clone(), None).await.is_err());
        let mut tape = Cartridge::new(Barcode::parse("NEW001L8").unwrap(), 2147483648).unwrap();
        tape.media_type = Some("LTO8".into());
        tape.segment_size_mib = 64;
        tape.multipart_part_size_mib = 128;
        state.catalog.create_cartridge(&tape).await.unwrap();
        let output = batch_result(&state, result.clone(), None).await.unwrap();
        assert!(output["request"].get("vtl_id").is_none());
        assert!(
            serde_json::from_value::<BatchCreate>(output["request"].clone())
                .unwrap()
                .validate()
                .is_ok()
        );
        for (field, value) in [
            ("completed", json!(3)),
            ("phase", json!("completed")),
            ("barcodes", json!(["NEW001L8", "NEW001L8"])),
        ] {
            let mut bad = result.clone();
            bad[field] = value;
            assert!(batch_result(&state, bad, None).await.is_err());
        }
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn controller_failure_and_missing_catalogue_never_create_a_row() {
        let (state, root) = setup().await;
        for body in [
            "#!/bin/sh\ncat >/dev/null\necho '{\"error\":\"mkmedia failed\"}'; exit 1\n",
            "#!/bin/sh\ncat >/dev/null\necho '{\"barcode\":\"NEW001L8\"}'\n",
        ] {
            script(&root.join("cartridge"), body);
            assert!(create(State(state.clone()), Json(request())).await.is_err());
            assert!(state.catalog.list_cartridges().await.unwrap().is_empty());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cartridge_response_exposes_retry_after_job_feed_has_aged_out() {
        let (state, root) = setup().await;
        let mut tape = Cartridge::new(Barcode::parse("RETRY01").unwrap(), 1024).unwrap();
        tape.state = cold_vtl_core::CartridgeState::Sealed;
        state.catalog.create_cartridge(&tape).await.unwrap();
        state
            .catalog
            .reserve_job("failed-export", "export", &tape, Some(1), None)
            .await
            .unwrap();
        tape.state = cold_vtl_core::CartridgeState::Failed;
        state
            .catalog
            .finish_job("failed-export", "failed", "interrupted", Some(&tape), None)
            .await
            .unwrap();
        for index in 0..101 {
            sqlx::query("INSERT INTO jobs(id,kind,subject,status,created_at) VALUES(?,'reconcile','OTHER1','completed','2100-01-01 00:00:00')").bind(format!("unrelated-{index}")).execute(&state.catalog.pool()).await.unwrap();
        }
        assert!(
            state
                .catalog
                .list_jobs()
                .await
                .unwrap()
                .iter()
                .all(|job| job.id != "failed-export")
        );
        let Json(view) = crate::list_cartridges(State(state.clone())).await.unwrap();
        let value = serde_json::to_value(view).unwrap();
        assert_eq!(value[0]["barcode"], "RETRY01");
        assert_eq!(value[0]["export_resumable"], true);
        assert_eq!(
            state
                .catalog
                .cartridge(&tape.barcode)
                .await
                .unwrap()
                .unwrap()
                .state,
            cold_vtl_core::CartridgeState::Failed
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn online_capability_replaces_maintenance_but_not_job_exclusion() {
        let (state, root) = setup().await;
        script(
            &root.join("maintenance"),
            "#!/bin/sh\ncat >/dev/null\necho '{\"phase\":\"online\",\"ready\":false}'\n",
        );
        let mut value = serde_json::to_value(Options::unavailable(true, "online")).unwrap();
        value["ready"] = json!(true);
        value["online_creation"] = json!(true);
        value["media_types"] = json!(["LTO8"]);
        value["free_slots"] = json!(3);
        script(
            &root.join("cartridge"),
            &format!("#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{}'\n", value),
        );
        let Json(options) = options(State(state.clone())).await.unwrap();
        assert!(
            options.ready,
            "a live-capable helper must be available outside maintenance"
        );
        assert!(options.online_creation);
        assert!(creation_readiness(&state).await.is_ok());
        sqlx::query("INSERT INTO jobs(id,kind,subject,status) VALUES('busy-online','export','OTHER1','running')")
            .execute(&state.catalog.pool()).await.unwrap();
        assert!(
            create(State(state.clone()), Json(request()))
                .await
                .unwrap_err()
                .message
                .contains("active VTL tasks")
        );
        state.catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn jobs_maintenance_and_uninstalled_vtl_block_before_controller() {
        let (state, root) = setup().await;
        script(
            &root.join("cartridge"),
            "#!/bin/sh\necho '{\"error\":\"CONTROLLER REACHED\"}'; exit 1\n",
        );
        sqlx::query(
            "INSERT INTO jobs(id,kind,subject,status) VALUES('busy','export','OTHER1','running')",
        )
        .execute(&state.catalog.pool())
        .await
        .unwrap();
        let error = create(State(state.clone()), Json(request()))
            .await
            .unwrap_err();
        assert!(error.message.contains("active VTL tasks"));
        sqlx::query("DELETE FROM jobs WHERE id='busy'")
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        script(
            &root.join("maintenance"),
            "#!/bin/sh\ncat >/dev/null\necho '{\"phase\":\"online\",\"ready\":false}'\n",
        );
        assert!(
            create(State(state.clone()), Json(request()))
                .await
                .unwrap_err()
                .message
                .contains("managed maintenance")
        );
        let id = cold_vtl_core::VtlId::new();
        sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'draft','draft')")
            .bind(id.to_string())
            .execute(&state.catalog.pool())
            .await
            .unwrap();
        let mut scoped = state.clone();
        scoped.catalog = state.catalog.for_vtl(id).await.unwrap();
        assert!(
            create(State(scoped), Json(request()))
                .await
                .unwrap_err()
                .message
                .contains("not activated yet")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn controller_commit_is_read_back_and_identical_retry_reaches_controller() {
        let (state, root) = setup().await;
        let db = serde_json::to_string(root.join("catalog.db").to_str().unwrap()).unwrap();
        script(
            &root.join("cartridge"),
            &format!(
                r#"#!/usr/bin/env python3
import json,sys,sqlite3
r=json.load(sys.stdin)
assert r['vtl_id']=='00000000-0000-4000-8000-000000000001'
with sqlite3.connect({db}) as db:
 db.execute("INSERT INTO cartridges(vtl_id,barcode,capacity_bytes,media_type,segment_size_mib,multipart_part_size_mib,state) VALUES(?,?,?,?,?,?,'writable')",(r['vtl_id'],r['barcode'],r['capacity_bytes'],r['media_type'],r['segment_size_mib'],r['multipart_part_size_mib']))
print(json.dumps({{'barcode':r['barcode']}}))
"#
            ),
        );
        let (status, Json(cartridge)) =
            create(State(state.clone()), Json(request())).await.unwrap();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(cartridge.media_type.as_deref(), Some("LTO8"));
        assert_eq!(cartridge.segment_size_mib, 64);
        assert_eq!(cartridge.multipart_part_size_mib, 128);
        script(
            &root.join("cartridge"),
            "#!/bin/sh\ncat >/dev/null\necho '{\"error\":\"retry reached controller\"}'; exit 1\n",
        );
        assert_eq!(
            create(State(state.clone()), Json(request()))
                .await
                .unwrap_err()
                .message,
            "retry reached controller"
        );
        let mut changed = request();
        changed.segment_size_mib = 128;
        assert!(
            create(State(state.clone()), Json(changed))
                .await
                .unwrap_err()
                .message
                .contains("different settings")
        );
        let mut changed_parts = request();
        changed_parts.multipart_part_size_mib = 256;
        assert!(
            create(State(state.clone()), Json(changed_parts))
                .await
                .unwrap_err()
                .message
                .contains("different settings")
        );
        // Export must carry the persisted choice, including after catalogue readback.
        let persisted = state
            .catalog
            .cartridge(&cartridge.barcode)
            .await
            .unwrap()
            .unwrap();
        let mut command = Command::new("worker");
        crate::configure_export(&mut command, &persisted, &root.join("media"));
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--segment-size-mib", "64"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--multipart-part-size-mib", "128"])
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn options_report_compatible_types_and_explain_active_jobs() {
        let (state, root) = setup().await;
        let mut advertised = Options::unavailable(true, "");
        advertised.ready = true;
        advertised.reason = None;
        advertised.media_types = vec!["LTO7".into(), "LTO8".into(), "LTO99".into()];
        advertised.default_media_type = Some("LTO8".into());
        advertised.free_slots = 4;
        script(
            &root.join("cartridge"),
            &format!(
                "#!/bin/sh\ncat >/dev/null\necho '{}'\n",
                serde_json::to_string(&advertised).unwrap()
            ),
        );
        let Json(available) = options(State(state.clone())).await.unwrap();
        assert!(available.ready);
        assert_eq!(available.media_types, ["LTO7", "LTO8"]);
        assert_eq!(available.free_slots, 4);
        sqlx::query(
            "INSERT INTO jobs(id,kind,subject,status) VALUES('busy','export','OTHER1','running')",
        )
        .execute(&state.catalog.pool())
        .await
        .unwrap();
        let Json(blocked) = options(State(state.clone())).await.unwrap();
        assert!(!blocked.ready);
        assert!(blocked.reason.unwrap().contains("active VTL tasks"));
        assert_eq!(blocked.media_types, ["LTO7", "LTO8"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn pending_creation_can_resume_after_using_the_last_slot() {
        let (state, root) = setup().await;
        let mut advertised = serde_json::to_value(Options::unavailable(
            true,
            "Reprenez la création en attente.",
        ))
        .unwrap();
        advertised["ready"] = json!(true);
        advertised["media_types"] = json!(["LTO8"]);
        advertised["default_media_type"] = json!("LTO8");
        advertised["pending_creation"] = serde_json::to_value(request()).unwrap();
        script(
            &root.join("cartridge"),
            &format!("#!/bin/sh\ncat >/dev/null\necho '{}'\n", advertised),
        );
        let Json(available) = options(State(state.clone())).await.unwrap();
        assert!(available.ready);
        let returned = serde_json::to_value(available).unwrap();
        assert_eq!(returned["free_slots"], 0);
        assert_eq!(
            returned["pending_creation"],
            serde_json::to_value(request()).unwrap()
        );
        // A pending request with invalid settings cannot bypass readiness checks.
        advertised["pending_creation"]["media_type"] = json!("LTO9");
        script(
            &root.join("cartridge"),
            &format!("#!/bin/sh\ncat >/dev/null\necho '{}'\n", advertised),
        );
        let Json(blocked) = options(State(state.clone())).await.unwrap();
        assert!(!blocked.ready);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn migration_preserves_legacy_media_with_unknown_type_and_256_mib_segments() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let mut before = sqlx::migrate!("./migrations");
        before.migrations = before
            .migrations
            .iter()
            .filter(|m| m.version <= 10)
            .cloned()
            .collect::<Vec<_>>()
            .into();
        before.run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO cartridges(barcode,capacity_bytes,state) VALUES('OLD001',1234,'writable')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let row: (u32, Option<String>, i64) = sqlx::query_as("SELECT segment_size_mib,media_type,capacity_bytes FROM cartridges WHERE barcode='OLD001'").fetch_one(&pool).await.unwrap();
        assert_eq!(row, (256, None, 1234));
    }
}
