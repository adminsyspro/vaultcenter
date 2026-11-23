use super::{Catalog, CatalogError};

#[derive(Clone, Debug)]
pub struct Destination {
    pub id: String,
    pub revision: Option<i64>,
    pub active: bool,
}
impl Catalog {
    pub async fn for_destination(&self, id: String) -> Result<Self, CatalogError> {
        let mut catalog = self.clone();
        catalog.destination_id = Some(id);
        catalog.destination().await?;
        Ok(catalog)
    }
    pub async fn destination(&self) -> Result<Destination, CatalogError> {
        let vtl = self.vtl_id().await?.to_string();
        let row: Option<(String, Option<i64>, bool)> = sqlx::query_as("SELECT d.id,MAX(r.revision),d.active FROM storage_destinations d LEFT JOIN storage_destination_revisions r ON r.destination_id=d.id WHERE d.vtl_id=? AND ((? IS NULL AND d.active=1) OR d.id=?) GROUP BY d.id")
            .bind(vtl).bind(&self.destination_id).bind(&self.destination_id).fetch_optional(&self.pool).await?;
        let (id, revision, active) = row.ok_or(CatalogError::Conflict)?;
        Ok(Destination {
            id,
            revision,
            active,
        })
    }
    pub async fn worker_command(
        &self,
        path: &std::path::Path,
        job: Option<&str>,
    ) -> Result<tokio::process::Command, CatalogError> {
        let pin = if let Some(id) = job {
            let job = self.job(id).await?.ok_or(CatalogError::MissingJob)?;
            Destination {
                id: job.destination_id.ok_or(CatalogError::Conflict)?,
                revision: job.configuration_revision,
                active: false,
            }
        } else {
            self.destination().await?
        };
        self.worker_for_destination(path, pin).await
    }
    pub async fn worker_for_destination(
        &self,
        path: &std::path::Path,
        pin: Destination,
    ) -> Result<tokio::process::Command, CatalogError> {
        let runtime = if self.state_dir().join("storage-isolated").exists() {
            Some(
                crate::vtl_runtime::runtime_for_catalog(self, self.vtl_id().await?)
                    .await
                    .map_err(|_| CatalogError::Conflict)?,
            )
        } else {
            None
        };
        let mut command = tokio::process::Command::new(if runtime.is_some() {
            std::path::Path::new("/usr/local/libexec/cold-vtl-worker-run")
        } else {
            path
        });
        if let Some(runtime) = runtime {
            command.env("COLD_VTL_RUNTIME_REVISION", runtime.revision.to_string());
        }
        command
            .env_remove("AWS_ACCESS_KEY_ID")
            .env_remove("AWS_SECRET_ACCESS_KEY")
            .env_remove("AWS_SESSION_TOKEN")
            .env("COLD_VTL_DATABASE", &self.database_path)
            .env("COLD_VTL_VTL_ID", self.vtl_id().await?.to_string())
            .env("COLD_VTL_DESTINATION_ID", pin.id)
            .env(
                "COLD_VTL_CONFIGURATION_REVISION",
                pin.revision.unwrap_or(0).to_string(),
            );
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cold_vtl_core::{Barcode, Cartridge, CartridgeState, RemoteGeneration, VtlId};
    #[tokio::test]
    async fn job_pin_survives_rotation_and_historical_reconcile_is_isolated() {
        let root = std::env::temp_dir().join(format!("vtl-pin-{}", uuid::Uuid::new_v4()));
        let catalog = Catalog::open(&root.join("catalog.db")).await.unwrap();
        let vtl = catalog.vtl_id().await.unwrap().to_string();
        let original = catalog.destination().await.unwrap().id;
        sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,1,'{}')").bind(&original).execute(&catalog.pool()).await.unwrap();
        let cartridge = Cartridge::new(Barcode::parse("TAPE01").unwrap(), 1024).unwrap();
        catalog.create_cartridge(&cartridge).await.unwrap();
        let job = catalog
            .reserve_job("pinned", "reconcile", &cartridge, None, None)
            .await
            .unwrap();
        assert_eq!(job.destination_id.as_deref(), Some(original.as_str()));
        assert_eq!(job.configuration_revision, Some(1));
        sqlx::query("UPDATE storage_destinations SET active=0 WHERE id=?")
            .bind(&original)
            .execute(&catalog.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO storage_destinations(id,vtl_id,active) VALUES('next',?,1)")
            .bind(&vtl)
            .execute(&catalog.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES('next',2,'{}')").execute(&catalog.pool()).await.unwrap();
        let command = catalog
            .worker_command(std::path::Path::new("worker"), Some("pinned"))
            .await
            .unwrap();
        let env: std::collections::HashMap<_, _> = command
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| {
                v.map(|v| {
                    (
                        k.to_string_lossy().to_string(),
                        v.to_string_lossy().to_string(),
                    )
                })
            })
            .collect();
        assert_eq!(env["COLD_VTL_DESTINATION_ID"], original);
        assert_eq!(env["COLD_VTL_CONFIGURATION_REVISION"], "1");
        assert_eq!(env["COLD_VTL_VTL_ID"], vtl);
        assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY"));
        let mut historical = cartridge.clone();
        historical.state = CartridgeState::Verified;
        historical.generation = 9;
        let generation = RemoteGeneration {
            barcode: cartridge.barcode.clone(),
            generation: 9,
            format_version: 2,
            manifest_key: "historical/manifest".into(),
            manifest_sha256: "a".repeat(64),
            logical_size_bytes: 0,
            file_count: 0,
            segment_count: 0,
            local_match: true,
        };
        catalog
            .finish_job(
                "pinned",
                "completed",
                "done",
                Some(&historical),
                Some(&[generation]),
            )
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
        assert!(
            catalog
                .generations(&cartridge.barcode)
                .await
                .unwrap()
                .is_empty()
        );
        let history = catalog.for_destination(original.clone()).await.unwrap();
        assert_eq!(
            history.generations(&cartridge.barcode).await.unwrap().len(),
            1
        );
        assert!(
            history
                .reserve_job("wrong-export", "export", &cartridge, None, None)
                .await
                .is_err()
        );
        let foreign = VtlId::new();
        sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'Foreign','ready')")
            .bind(foreign.to_string())
            .execute(&catalog.pool())
            .await
            .unwrap();
        let foreign = catalog.for_vtl(foreign).await.unwrap();
        assert!(foreign.for_destination(original).await.is_err());
        catalog.pool().close().await;
        std::fs::remove_dir_all(root).unwrap();
    }
}
