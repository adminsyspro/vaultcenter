//! Version and revision of the running build, reported by `/health` and `/v1/status`.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const REVISION: &str = match option_env!("VAULTCENTER_REVISION") {
    Some(revision) if !revision.is_empty() => revision,
    _ => "unknown",
};

#[cfg(test)]
mod tests {
    use super::REVISION;
    use axum::extract::State;
    use std::sync::Arc;
    use uuid::Uuid;

    #[tokio::test]
    async fn health_reports_version_and_revision() {
        let body = serde_json::to_value(crate::health().await.0).unwrap();
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(body["revision"], REVISION);
        assert!(!REVISION.is_empty());
    }

    #[tokio::test]
    async fn status_reports_version_and_revision() {
        let folder = std::env::temp_dir().join(format!("cold-vtl-build-info-{}", Uuid::new_v4()));
        let catalog = crate::catalog::Catalog::open(&folder.join("catalog.db"))
            .await
            .unwrap();
        let state = crate::AppState {
            catalog,
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(crate::AppConfig {
                storage_socket: None,
                media_root: folder.join("media"),
                worker_path: folder.join("absent-worker"),
                restore_root: folder.join("restores"),
                quiesce_check: None,
                maintenance_helper: None,
                cartridge_helper: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        };
        let body =
            serde_json::to_value(crate::service_status(State(state)).await.unwrap().0).unwrap();
        assert_eq!(body["status"], "ok");
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(body["revision"], REVISION);
    }
}
