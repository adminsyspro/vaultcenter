use super::{Catalog, CatalogError};
use cold_vtl_core::VtlId;

pub const LEGACY_VTL_ID: &str = "00000000-0000-4000-8000-000000000001";

impl Catalog {
    pub async fn vtl_id(&self) -> Result<VtlId, CatalogError> {
        self.resolve_vtl(self.vtl_id).await
    }

    /// Persist the existing appliance roots without silently moving its media on restart.
    pub async fn initialize_legacy(
        &self,
        media: &std::path::Path,
        restore: &std::path::Path,
    ) -> Result<(), CatalogError> {
        for path in [media, restore] {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(CatalogError::InvalidRoots);
            }
        }
        if media.starts_with(restore) || restore.starts_with(media) {
            return Err(CatalogError::InvalidRoots);
        }
        let media = media.to_str().ok_or(CatalogError::InvalidRoots)?;
        let restore = restore.to_str().ok_or(CatalogError::InvalidRoots)?;
        // Conditional update obtains SQLite's writer lock before inspecting the roots.
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE vtls SET media_root=?,restore_root=?,lifecycle='ready',updated_at=CURRENT_TIMESTAMP WHERE id=? AND media_root IS NULL AND restore_root IS NULL")
            .bind(media).bind(restore).bind(LEGACY_VTL_ID).execute(&mut *tx).await?;
        let saved: Option<(Option<String>, Option<String>)> =
            sqlx::query_as("SELECT media_root,restore_root FROM vtls WHERE id=?")
                .bind(LEGACY_VTL_ID)
                .fetch_optional(&mut *tx)
                .await?;
        match saved {
            Some((Some(m), Some(r))) if m == media && r == restore => {}
            _ => return Err(CatalogError::InvalidRoots),
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn for_vtl(&self, id: VtlId) -> Result<Self, CatalogError> {
        self.resolve_vtl(Some(id)).await?;
        Ok(Self {
            pool: self.pool.clone(),
            vtl_id: Some(id),
            destination_id: self.destination_id.clone(),
            database_path: self.database_path.clone(),
        })
    }

    /// Compatibility routes may select implicitly only while exactly one provisioned VTL exists. Empty drafts cannot receive media or jobs.
    pub async fn resolve_vtl(&self, requested: Option<VtlId>) -> Result<VtlId, CatalogError> {
        if let Some(id) = requested {
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM vtls WHERE id=?)")
                .bind(id.to_string())
                .fetch_one(&self.pool)
                .await?;
            return if exists {
                Ok(id)
            } else {
                Err(CatalogError::MissingVtl)
            };
        }
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM vtls WHERE id='00000000-0000-4000-8000-000000000001' OR lifecycle != 'draft' OR media_root IS NOT NULL OR restore_root IS NOT NULL LIMIT 2")
            .fetch_all(&self.pool)
            .await?;
        match ids.as_slice() {
            [id] => id.parse().map_err(CatalogError::InvalidVtlId),
            [] => Err(CatalogError::MissingVtl),
            _ => Err(CatalogError::AmbiguousVtl),
        }
    }
}
