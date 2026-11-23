//! Resource identity and operation locks shared by all VTL entry points.
use crate::{ApiError, AppState};
use cold_vtl_core::VtlId;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, TryLockError};

pub struct OperationLocks {
    registry: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    current: Arc<AsyncMutex<()>>,
}
impl Default for OperationLocks {
    fn default() -> Self {
        let current = Arc::new(AsyncMutex::new(()));
        let registry = HashMap::from([(
            "00000000-0000-4000-8000-000000000001".into(),
            current.clone(),
        )]);
        Self {
            registry: Arc::new(Mutex::new(registry)),
            current,
        }
    }
}
impl OperationLocks {
    pub fn for_vtl(&self, id: VtlId) -> Self {
        let current = self
            .registry
            .lock()
            .expect("operation registry poisoned")
            .entry(id.to_string())
            .or_default()
            .clone();
        Self {
            registry: self.registry.clone(),
            current,
        }
    }
    pub async fn lock(&self) -> OwnedMutexGuard<()> {
        self.current.clone().lock_owned().await
    }
    pub fn try_lock(&self) -> Result<OwnedMutexGuard<()>, TryLockError> {
        self.current.clone().try_lock_owned()
    }
}

pub fn isolated(state: &AppState) -> bool {
    state.catalog.state_dir().join("storage-isolated").exists()
}

#[derive(serde::Deserialize, Clone)]
pub struct VtlRuntime {
    pub vtl_id: String,
    pub revision: i64,
    pub media_root: std::path::PathBuf,
    pub restore_root: std::path::PathBuf,
}

pub async fn runtime_for_catalog(
    catalog: &crate::catalog::Catalog,
    id: VtlId,
) -> Result<VtlRuntime, ApiError> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::MetadataExt;
    let fail =
        || ApiError::conflict("Hardware manifest missing, modified or not activated for this VTL.");
    let root = catalog.state_dir();
    let path = root.join("vtls").join(id.to_string()).join("runtime.json");
    let info = tokio::fs::symlink_metadata(&path)
        .await
        .map_err(|_| fail())?;
    if !info.is_file()
        || info.uid() != unsafe { libc::geteuid() }
        || info.mode() & 0o022 != 0
        || info.len() > 65536
    {
        return Err(fail());
    }
    let bytes = tokio::fs::read(path).await.map_err(|_| fail())?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| fail())?;
    let runtime: VtlRuntime = serde_json::from_value(value.clone()).map_err(|_| fail())?;
    if runtime.vtl_id != id.to_string()
        || runtime.media_root
            != std::path::Path::new("/var/lib/vaultcenter-media").join(id.to_string())
        || runtime.restore_root != root.join("restores").join(id.to_string())
        || value["state_root"]
            != root
                .join("vtls")
                .join(id.to_string())
                .to_string_lossy()
                .as_ref()
        || value["config_root"]
            != root
                .join("vtls")
                .join(id.to_string())
                .join("config")
                .to_string_lossy()
                .as_ref()
    {
        return Err(fail());
    }
    let record:Option<(String,String,i64,String)>=sqlx::query_as("SELECT p.manifest_sha256,p.phase,v.provisioning_revision,v.lifecycle FROM vtl_provisioning p JOIN vtls v ON v.id=p.vtl_id WHERE p.vtl_id=?")
        .bind(id.to_string()).fetch_optional(&catalog.pool()).await.map_err(ApiError::conflict)?;
    let hash = hex::encode(Sha256::digest(
        serde_json::to_vec(&value).map_err(|_| fail())?,
    ));
    if !matches!(record,Some((sha,phase,revision,lifecycle)) if sha==hash && phase=="active" && revision==runtime.revision && lifecycle=="ready")
    {
        return Err(fail());
    }
    // The API and root controller must both accept the same qualified runtime build.
    let qualification: serde_json::Value = serde_json::from_slice(
        &tokio::fs::read(root.join("isolation-qualified.json"))
            .await
            .map_err(|_| fail())?,
    )
    .map_err(|_| fail())?;
    if !qualified_receipt(&qualification) {
        return Err(fail());
    }
    Ok(runtime)
}

fn qualified_receipt(value: &serde_json::Value) -> bool {
    use sha2::{Digest, Sha256};
    let sha = hex::encode(Sha256::digest(include_bytes!(
        "../../../deploy/cold-vtl/vtl_runtime.py"
    )));
    value["isolation_verified"] == true && value["runtime_sha256"].as_str() == Some(&sha)
}

pub async fn helper_command(
    state: &AppState,
    path: &std::path::Path,
) -> Result<tokio::process::Command, ApiError> {
    let mut command = tokio::process::Command::new(path);
    if isolated(state) {
        let runtime = runtime_for_catalog(&state.catalog, state.catalog.vtl_id().await?).await?;
        command
            .env("COLD_VTL_VTL_ID", runtime.vtl_id)
            .env("COLD_VTL_RUNTIME_REVISION", runtime.revision.to_string());
    }
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn busy_b_allows_independent_a_and_same_vtl_remains_serialized() {
        let locks = OperationLocks::default();
        let a = VtlId::new();
        let b = VtlId::new();
        let first = locks.for_vtl(a);
        let second = locks.for_vtl(b);
        let _busy = second.lock().await;
        assert!(first.try_lock().is_ok());
        assert!(locks.for_vtl(b).try_lock().is_err());
        let _a = first.lock().await;
        assert!(locks.for_vtl(a).try_lock().is_err());
    }
}

#[cfg(test)]
mod manifest_tests {
    use super::*;
    use crate::catalog::Catalog;
    #[test]
    fn qualification_is_bound_to_this_runtime_build() {
        use sha2::{Digest, Sha256};
        assert!(!qualified_receipt(
            &serde_json::json!({"isolation_verified":true})
        ));
        assert!(!qualified_receipt(
            &serde_json::json!({"isolation_verified":true,"runtime_sha256":"stale"})
        ));
        let sha = hex::encode(Sha256::digest(include_bytes!(
            "../../../deploy/cold-vtl/vtl_runtime.py"
        )));
        assert!(qualified_receipt(
            &serde_json::json!({"isolation_verified":true,"runtime_sha256":sha})
        ));
    }
    #[tokio::test]
    async fn unverified_or_changed_runtime_cannot_supply_media_roots() {
        let root = std::env::temp_dir().join(format!("runtime-{}", uuid::Uuid::new_v4()));
        let catalog = Catalog::open(&root.join("catalog.db")).await.unwrap();
        let id = catalog.vtl_id().await.unwrap();
        assert!(runtime_for_catalog(&catalog, id).await.is_err());
        let dir = root.join("vtls").join(id.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("runtime.json"), "{}").unwrap();
        assert!(runtime_for_catalog(&catalog, id).await.is_err());
        catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }
}
