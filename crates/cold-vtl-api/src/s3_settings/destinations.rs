use super::*;
use sqlx::{Sqlite, Transaction};

pub const LEGACY: &str = "00000000-0000-4000-8000-000000000001";
const LEGACY_DESTINATION: &str = "00000000-0000-4000-8000-000000000002";

#[derive(Serialize, Deserialize, PartialEq)]
pub(super) struct Identity {
    provider: S3Provider,
    endpoint: String,
    region: String,
    bucket: String,
    prefix: String,
}
impl Identity {
    pub(super) fn from_config(config: &Configuration) -> Result<Self, ApiError> {
        Ok(Self {
            provider: config.provider(),
            endpoint: url::Url::parse(&config.endpoint)
                .map_err(|_| ApiError::conflict("Invalid saved endpoint."))?
                .origin()
                .ascii_serialization(),
            region: config.region.clone(),
            bucket: config.bucket.clone(),
            prefix: config.prefix.trim_matches('/').into(),
        })
    }
    pub(super) fn overlaps(&self, other: &Self, other_vtl: bool) -> bool {
        self.endpoint == other.endpoint
            && self.bucket == other.bucket
            && ((other_vtl
                && (self.provider == S3Provider::OvhColdArchiveV1
                    || other.provider == S3Provider::OvhColdArchiveV1))
                || self.prefix.is_empty()
                || other.prefix.is_empty()
                || self.prefix == other.prefix
                || self.prefix.starts_with(&format!("{}/", other.prefix))
                || other.prefix.starts_with(&format!("{}/", self.prefix)))
    }
}

/// Attach legacy credentials to their existing destination; preserve object identities.
pub async fn initialize_legacy(state: &AppState) -> Result<(), ApiError> {
    let unresolved: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM storage_destinations WHERE id=? AND identity IS NULL AND active=1)",
    )
    .bind(LEGACY_DESTINATION)
    .fetch_one(&state.catalog.pool())
    .await
    .map_err(ApiError::conflict)?;
    if !unresolved {
        return Ok(());
    }
    let (revision, config) = super::legacy_configuration(state).await?;
    let Some(config) = config else {
        let copies: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM remote_generations WHERE destination_id=?")
                .bind(LEGACY_DESTINATION)
                .fetch_one(&state.catalog.pool())
                .await
                .map_err(ApiError::conflict)?;
        if copies > 0 {
            return Err(ApiError::conflict(
                "Fill in the legacy S3 configuration before attaching new storage.",
            ));
        }
        return Ok(());
    };
    super::private_catalog(state).await?;
    let identity =
        serde_json::to_string(&Identity::from_config(&config)?).map_err(ApiError::conflict)?;
    let configuration =
        serde_json::to_string(&config).map_err(|_| ApiError::conflict("Invalid configuration."))?;
    let mut tx = state
        .catalog
        .pool()
        .begin()
        .await
        .map_err(ApiError::conflict)?;
    let changed =
        sqlx::query("UPDATE storage_destinations SET identity=? WHERE id=? AND identity IS NULL")
            .bind(identity)
            .bind(LEGACY_DESTINATION)
            .execute(&mut *tx)
            .await
            .map_err(ApiError::conflict)?;
    if changed.rows_affected() == 1 {
        sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,?,?) ON CONFLICT(destination_id,revision) DO NOTHING")
            .bind(LEGACY_DESTINATION).bind(revision.max(1)).bind(&configuration).execute(&mut *tx).await.map_err(ApiError::conflict)?;
        if revision == 0 {
            sqlx::query("INSERT INTO s3_settings(id,revision,configuration,actor) VALUES(1,1,?,'migration')")
                .bind(configuration).execute(&mut *tx).await.map_err(ApiError::conflict)?;
        }
    }
    tx.commit().await.map_err(ApiError::conflict)
}

pub(super) async fn current(state: &AppState) -> Result<(i64, Option<Configuration>), ApiError> {
    initialize_legacy(state).await?;
    let vtl = state.catalog.vtl_id().await?.to_string();
    let mut tx = state
        .catalog
        .pool()
        .begin()
        .await
        .map_err(ApiError::conflict)?;
    let revision: i64 = sqlx::query_scalar("SELECT MAX(COALESCE((SELECT MAX(r.revision) FROM storage_destination_revisions r JOIN storage_destinations d ON d.id=r.destination_id WHERE d.vtl_id=?1),0),COALESCE((SELECT revision FROM s3_connections WHERE vtl_id=?1),0))")
        .bind(&vtl).fetch_one(&mut *tx).await.map_err(ApiError::conflict)?;
    let config: Option<String> = sqlx::query_scalar("SELECT r.configuration FROM storage_destination_revisions r JOIN storage_destinations d ON d.id=r.destination_id WHERE d.vtl_id=? AND d.active=1 ORDER BY r.revision DESC LIMIT 1")
        .bind(vtl).fetch_optional(&mut *tx).await.map_err(ApiError::conflict)?;
    Ok((
        revision,
        config
            .map(|value| {
                serde_json::from_str(&value)
                    .map_err(|_| ApiError::conflict("Invalid saved S3 configuration."))
            })
            .transpose()?,
    ))
}

pub(super) async fn save(
    state: &AppState,
    config: &Configuration,
    revision: i64,
    changed: bool,
    actor: &str,
) -> Result<String, ApiError> {
    let vtl = state.catalog.vtl_id().await?.to_string();
    let identity = Identity::from_config(config)?;
    let mut tx = state
        .catalog
        .pool()
        .begin()
        .await
        .map_err(ApiError::conflict)?;
    sqlx::query("UPDATE vtls SET provisioning_revision=provisioning_revision WHERE id=?")
        .bind(&vtl)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    let actual: i64 = sqlx::query_scalar("SELECT MAX(COALESCE((SELECT MAX(r.revision) FROM storage_destination_revisions r JOIN storage_destinations d ON d.id=r.destination_id WHERE d.vtl_id=?1),0),COALESCE((SELECT revision FROM s3_connections WHERE vtl_id=?1),0))")
        .bind(&vtl).fetch_one(&mut *tx).await.map_err(ApiError::conflict)?;
    if actual != revision {
        return Err(ApiError::conflict(
            "The S3 configuration has changed. Reload it.",
        ));
    }
    let pending: i64 = sqlx::query_scalar("SELECT (SELECT COUNT(*) FROM jobs WHERE vtl_id=? AND status IN ('queued','running'))+(SELECT COUNT(*) FROM archive_requests WHERE vtl_id=? AND ? AND phase IN ('queued','submitting','uncertain','accepted'))+(SELECT COUNT(*) FROM bucket_rotations WHERE vtl_id=? AND phase IN ('reserved','creating','ready','uncertain'))+(SELECT COUNT(*) FROM bucket_creations WHERE vtl_id=? AND phase IN ('reserved','creating','uncertain'))")
        .bind(&vtl).bind(&vtl).bind(changed).bind(&vtl).bind(&vtl).fetch_one(&mut *tx).await.map_err(ApiError::conflict)?;
    if pending > 0 {
        return Err(ApiError::conflict(
            "An operation is still using this VTL's storage.",
        ));
    }

    let rows: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT id,vtl_id,identity FROM storage_destinations")
            .fetch_all(&mut *tx)
            .await
            .map_err(ApiError::conflict)?;
    let mut selected = None;
    for (id, owner, stored) in rows {
        if let Some(stored) = stored {
            let other: Identity = serde_json::from_str(&stored)
                .map_err(|_| ApiError::conflict("Invalid saved destination identity."))?;
            if owner == vtl && identity == other {
                selected = Some(id);
            } else if identity.overlaps(&other, owner != vtl) {
                return Err(ApiError::conflict(
                    "This bucket or prefix overlaps a destination already assigned, including in the history. Use separate storage.",
                ));
            }
        } else if owner == vtl {
            selected = Some(id);
        }
    }
    let id = selected.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    // A manual change of bucket: the replaced one stops being active now
    // (automatic archiving and the rotation period count from this time).
    sqlx::query("UPDATE storage_destinations SET deactivated_at=CURRENT_TIMESTAMP WHERE vtl_id=? AND active=1 AND id<>?")
        .bind(&vtl)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    sqlx::query("UPDATE storage_destinations SET active=0 WHERE vtl_id=?")
        .bind(&vtl)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::conflict)?;
    sqlx::query("INSERT INTO storage_destinations(id,vtl_id,identity,active) VALUES(?,?,?,1) ON CONFLICT(id) DO UPDATE SET active=1,identity=COALESCE(storage_destinations.identity,excluded.identity)")
        .bind(&id).bind(&vtl).bind(serde_json::to_string(&identity).map_err(ApiError::conflict)?).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    let configuration =
        serde_json::to_string(config).map_err(|_| ApiError::conflict("Invalid configuration."))?;
    sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,?,?)")
        .bind(&id).bind(revision+1).bind(&configuration).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    // Existing workers still use the single-library compatibility row only.
    if vtl == LEGACY {
        sqlx::query("INSERT INTO s3_settings(id,revision,configuration,actor) VALUES(1,?,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,configuration=excluded.configuration,actor=excluded.actor,updated_at=CURRENT_TIMESTAMP")
            .bind(revision+1).bind(configuration).bind(actor).execute(&mut *tx).await.map_err(ApiError::conflict)?;
    }
    if changed {
        mark_reconcile(&mut tx, &vtl).await?;
    }
    tx.commit().await.map_err(ApiError::conflict)?;
    Ok(id)
}
async fn mark_reconcile(tx: &mut Transaction<'_, Sqlite>, vtl: &str) -> Result<(), ApiError> {
    sqlx::query("INSERT OR IGNORE INTO s3_reconcile_required(vtl_id,barcode) SELECT vtl_id,barcode FROM cartridges WHERE vtl_id=?")
        .bind(vtl).execute(&mut **tx).await.map_err(ApiError::conflict)?;
    Ok(())
}

pub async fn list(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<Value>, ApiError> {
    admin(&principal)?;
    initialize_legacy(&state).await?;
    let rows: Vec<(String,String,bool,i64)> = sqlx::query_as("SELECT d.id,d.identity,d.active,COALESCE(MAX(r.revision),0) FROM storage_destinations d LEFT JOIN storage_destination_revisions r ON r.destination_id=d.id WHERE d.vtl_id=? AND d.identity IS NOT NULL GROUP BY d.id ORDER BY d.created_at,d.id")
        .bind(state.catalog.vtl_id().await?.to_string()).fetch_all(&state.catalog.pool()).await.map_err(ApiError::conflict)?;
    let mut values = Vec::new();
    for (id, identity, active, revision) in rows {
        let identity: Identity = serde_json::from_str(&identity)
            .map_err(|_| ApiError::conflict("Invalid destination identity."))?;
        values.push(json!({"id":id,"active":active,"configuration_revision":revision,"configuration":identity}));
    }
    Ok(Json(json!(values)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn identity(prefix: &str) -> Identity {
        Identity {
            provider: S3Provider::S3Compatible,
            endpoint: "https://s3.example.test".into(),
            region: "test".into(),
            bucket: "bucket".into(),
            prefix: prefix.into(),
        }
    }
    #[tokio::test]
    async fn uncertain_rotation_blocks_configuration_even_after_failed_job() {
        let root =
            std::env::temp_dir().join(format!("vtl-rotation-guard-{}", uuid::Uuid::new_v4()));
        let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
            .await
            .unwrap();
        let vtl = catalog.vtl_id().await.unwrap().to_string();
        let destination = catalog.destination().await.unwrap().id;
        sqlx::query("INSERT INTO jobs(id,kind,subject,status) VALUES('rotation','archive_submit','bucket','failed')").execute(&catalog.pool()).await.unwrap();
        sqlx::query("INSERT INTO bucket_rotations(job_id,vtl_id,source_destination_id,next_bucket,phase) VALUES('rotation',?,?,'next-bucket','uncertain')").bind(&vtl).bind(destination).execute(&catalog.pool()).await.unwrap();
        let state = AppState {
            catalog: catalog.clone(),
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
        let config:Configuration=serde_json::from_value(json!({"endpoint":"https://s3.example.test","region":"test","bucket":"replacement","prefix":"vtl","access_key_id":"key","secret_access_key":"secret"})).unwrap();
        for phase in ["reserved", "creating", "ready", "uncertain"] {
            sqlx::query("UPDATE bucket_rotations SET phase=?")
                .bind(phase)
                .execute(&catalog.pool())
                .await
                .unwrap();
            for changed in [true, false] {
                assert!(save(&state, &config, 0, changed, "admin").await.is_err());
            }
        }
        catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prefix_collisions_use_path_boundaries_and_preserve_case() {
        for (a, b, overlap) in [
            ("", "a", true),
            ("a", "a/b", true),
            ("a", "ab", false),
            ("A", "a", false),
            ("a/b", "a", true),
        ] {
            assert_eq!(
                identity(a).overlaps(&identity(b), true),
                overlap,
                "{a} / {b}"
            );
        }
        let mut other = identity("a");
        other.bucket = "different".into();
        assert!(!identity("a").overlaps(&other, true));
        other = identity("b");
        other.provider = S3Provider::OvhColdArchiveV1;
        assert!(identity("a").overlaps(&other, true));
        assert!(!identity("a").overlaps(&other, false));
    }
}
