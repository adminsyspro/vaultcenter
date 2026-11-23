//! Shared fixtures for the S3 settings tests (legacy VTL, active OVH destination).
use super::*;
use destinations::Identity;

// Callers outside s3_settings may call it and ignore the private configuration.
#[allow(private_interfaces)]
pub(crate) async fn fixture() -> (AppState, std::path::PathBuf, Configuration) {
    let root = std::env::temp_dir().join(format!("bucket-operations-{}", uuid::Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
        .await
        .unwrap();
    let catalog = catalog
        .for_vtl(catalog.vtl_id().await.unwrap())
        .await
        .unwrap();
    let state = AppState {
        catalog,
        operations: std::sync::Arc::new(crate::vtl_runtime::OperationLocks::default()),
        config: std::sync::Arc::new(crate::AppConfig {
            storage_socket: None,
            media_root: root.join("media"),
            restore_root: root.join("restore"),
            worker_path: root.join("worker"),
            quiesce_check: None,
            maintenance_helper: None,
            cartridge_helper: None,
            reintegration_helper: None,
            archive_helper: None,
            telemetry_dir: None,
            telemetry_retention_days: 30,
        }),
    };
    let config: Configuration = serde_json::from_value(json!({"provider":"ovh_cold_archive_v1","endpoint":cold_vtl_core::OVH_ARCHIVE_ENDPOINT,"region":"rbx-archive","bucket":"current-bucket","prefix":"vtl","access_key_id":"key","secret_access_key":"private-secret"})).unwrap();
    let pin = state.catalog.destination().await.unwrap();
    sqlx::query("UPDATE storage_destinations SET identity=? WHERE id=?")
        .bind(serde_json::to_string(&Identity::from_config(&config).unwrap()).unwrap())
        .bind(&pin.id)
        .execute(&state.catalog.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,1,?)").bind(&pin.id).bind(serde_json::to_string(&config).unwrap()).execute(&state.catalog.pool()).await.unwrap();
    (state, root, config)
}

/// The fixture without its configuration, for tests outside `s3_settings`.
pub(crate) async fn state_fixture() -> (AppState, std::path::PathBuf) {
    let (state, root, _) = fixture().await;
    (state, root)
}

/// The shared S3 fixture (active OVH destination `current-bucket`) with an
/// archive helper script running `helper` on its input.
pub(crate) async fn archive_fixture(helper: &str) -> (AppState, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let (mut state, root) = state_fixture().await;
    let path = root.join("archive-helper");
    std::fs::write(&path, format!("#!/bin/sh\n{helper}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut config = (*state.config).clone();
    config.archive_helper = Some(path);
    state.config = std::sync::Arc::new(config);
    (state, root)
}

/// A creation interrupted after the worker was launched (`creating`).
#[allow(private_interfaces)]
pub(crate) async fn reserve(state: &AppState, config: &Configuration) {
    let vtl = state.catalog.vtl_id().await.unwrap().to_string();
    let pin = state.catalog.destination().await.unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id,destination_id,configuration_revision) VALUES('create','bucket_create',?,'running',?,?,?)").bind(&config.bucket).bind(&vtl).bind(&pin.id).bind(pin.revision).execute(&state.catalog.pool()).await.unwrap();
    sqlx::query("INSERT INTO bucket_creations(job_id,vtl_id,source_destination_id,next_bucket,configuration,revision,phase) VALUES('create',?,?,?,?,1,'creating')").bind(vtl).bind(&pin.id).bind(&config.bucket).bind(serde_json::to_string(config).unwrap()).execute(&state.catalog.pool()).await.unwrap();
}

pub(crate) fn fake_worker(root: &std::path::Path, stdout: &str, code: i32) {
    use std::os::unix::fs::PermissionsExt;
    let worker = root.join("worker");
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(
        &worker,
        format!("#!/bin/sh\ncat >/dev/null\necho '{stdout}'\nexit {code}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// Worker answering the two read-only probes; every other command loses its
/// response (exit 1). Each invocation appends its command to `root/calls`.
pub(crate) fn dispatch_worker(root: &std::path::Path, presence: &str, emptiness: &str) {
    use std::os::unix::fs::PermissionsExt;
    let worker = root.join("worker");
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(
        &worker,
        format!(
            "#!/bin/sh\ncat >/dev/null\necho \"$1\" >> '{calls}'\ncase \"$1\" in\n  bucket-presence) echo '{{\"status\":\"{presence}\"}}' ;;\n  confirm-empty-bucket) echo '{{\"status\":\"{emptiness}\"}}' ;;\n  *) echo lost; exit 1 ;;\nesac\n",
            calls = root.join("calls").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).unwrap();
}

pub(crate) fn calls(root: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(root.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// An archive of the active bucket whose next-bucket creation lost its answer:
/// the controller left the rotation `uncertain` and failed the queued request.
pub(crate) async fn uncertain_rotation(state: &AppState, next_bucket: &str) -> String {
    let vtl = state.catalog.vtl_id().await.unwrap().to_string();
    let pin = state.catalog.destination().await.unwrap();
    let pool = state.catalog.pool();
    sqlx::query("INSERT INTO jobs(id,kind,subject,status,progress,message,vtl_id,destination_id,configuration_revision) VALUES('archive','archive_submit','current-bucket','failed',100,'Création du prochain bucket non confirmée.',?,?,?)")
        .bind(&vtl).bind(&pin.id).bind(pin.revision).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,operation,vtl_id,destination_id) VALUES('archive','archive','failed','{\"confirm_bucket\":\"current-bucket\"}','archive',?,?)")
        .bind(&vtl).bind(&pin.id).execute(&pool).await.unwrap();
    let identity = json!({"provider":"ovh_cold_archive_v1","endpoint":cold_vtl_core::OVH_ARCHIVE_ENDPOINT,"region":"rbx-archive","bucket":next_bucket,"prefix":"vtl"});
    let mut config = identity.clone();
    config["access_key_id"] = json!("key");
    config["secret_access_key"] = json!("private-secret");
    sqlx::query("INSERT INTO storage_destinations(id,vtl_id,identity,active) VALUES('next-destination',?,?,0)")
        .bind(&vtl).bind(identity.to_string()).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES('next-destination',2,?)")
        .bind(config.to_string()).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO bucket_rotations(job_id,vtl_id,source_destination_id,next_destination_id,next_bucket,phase) VALUES('archive',?,?,'next-destination',?,'uncertain')")
        .bind(&vtl).bind(&pin.id).bind(next_bucket).execute(&pool).await.unwrap();
    "next-destination".into()
}

pub(crate) async fn rotation_phase(state: &AppState) -> (String, Option<String>) {
    sqlx::query_as("SELECT phase,next_destination_id FROM bucket_rotations WHERE job_id='archive'")
        .fetch_one(&state.catalog.pool())
        .await
        .unwrap()
}

pub(crate) async fn creation_phase(state: &AppState, id: &str) -> String {
    sqlx::query_scalar("SELECT phase FROM bucket_creations WHERE job_id=?")
        .bind(id)
        .fetch_one(&state.catalog.pool())
        .await
        .unwrap()
}

/// `reserve_creation` saves secrets only in a private (0700) catalogue directory.
pub(super) fn private_root(root: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// Worker that creates a bucket (or refuses it for `refusal`) after `delay`
/// seconds and logs its calls.
pub(super) fn creating_worker(root: &std::path::Path, delay: &str, refusal: Option<&str>) {
    use std::os::unix::fs::PermissionsExt;
    let reply = match refusal {
        Some(reason) => format!(
            r#"echo "{{\"status\":\"not_created\",\"reason\":\"{reason}\",\"bucket\":\"$3\"}}"; exit 3"#
        ),
        None => r#"echo "{\"status\":\"created\",\"bucket\":\"$3\"}""#.to_owned(),
    };
    std::fs::write(
        root.join("worker"),
        format!(
            "#!/bin/sh\ncat >/dev/null\necho \"$1\" >> '{calls}'\ncase \"$1\" in\n  create-bucket-from-connection) sleep {delay}; {reply} ;;\n  *) echo lost; exit 1 ;;\nesac\n",
            calls = root.join("calls").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(root.join("worker"), std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// Weekly schedule whose current period has not been served yet.
pub(super) async fn due_weekly(state: &AppState) {
    sqlx::query("INSERT INTO storage_schedules(vtl_id,revision,rotation,weekday,rotation_served_at) VALUES(?,1,'weekly',1,'2000-01-03 00:00:00')")
        .bind(state.catalog.vtl_id().await.unwrap().to_string())
        .execute(&state.catalog.pool())
        .await
        .unwrap();
}

pub(super) async fn switched_creations(state: &AppState) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM bucket_creations WHERE phase='switched'")
        .fetch_one(&state.catalog.pool())
        .await
        .unwrap()
}

/// Worker whose creation waits for `root/release`, then loses its answer;
/// the presence checks find the bucket, empty. Each command is logged.
pub(super) fn held_lost_worker(root: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(
        root.join("worker"),
        format!(
            "#!/bin/sh\ncat >/dev/null\necho \"$1\" >> '{calls}'\ncase \"$1\" in\n  create-bucket-from-connection) while [ ! -f '{release}' ]; do sleep 0.05; done; echo lost; exit 1 ;;\n  bucket-presence) echo '{{\"status\":\"exists\"}}' ;;\n  confirm-empty-bucket) echo '{{\"status\":\"empty\"}}' ;;\n  *) echo lost; exit 1 ;;\nesac\n",
            calls = root.join("calls").display(),
            release = root.join("release").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(root.join("worker"), std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// An older, inactive bucket `older` whose archive request is in `phase`; the
/// controller already failed its job (an uncertain archive keeps no job running).
pub(crate) async fn older_bucket_archive(state: &AppState, phase: &str) {
    let vtl = state.catalog.vtl_id().await.unwrap().to_string();
    let pool = state.catalog.pool();
    let identity = json!({"provider":"ovh_cold_archive_v1","endpoint":cold_vtl_core::OVH_ARCHIVE_ENDPOINT,"region":"rbx-archive","bucket":"older-bucket","prefix":"vtl"});
    sqlx::query(
        "INSERT INTO storage_destinations(id,vtl_id,identity,active) VALUES('older',?,?,0)",
    )
    .bind(&vtl)
    .bind(identity.to_string())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO jobs(id,kind,subject,status,vtl_id,destination_id) VALUES('older-archive','archive_submit','older-bucket','failed',?,'older')")
        .bind(&vtl).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request,operation,vtl_id,destination_id) VALUES('older-archive','older-archive',?,'{\"confirm_bucket\":\"older-bucket\"}','archive',?,'older')")
        .bind(phase).bind(&vtl).execute(&pool).await.unwrap();
}
