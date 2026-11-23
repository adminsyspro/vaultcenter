use crate::{ApiError, AppState};
use cold_vtl_core::MediaReadGuard;

pub struct Guard {
    _media: Option<MediaReadGuard>,
}

async fn online_ready(state: &AppState) -> Result<(), ApiError> {
    let value = crate::cartridges::invoke(state, "export-readiness", serde_json::json!({})).await?;
    if value["media_lock_version"] != 1
        || value["vtl_id"] != state.catalog.vtl_id().await?.to_string()
    {
        return Err(ApiError::conflict(
            "Cartridge protection was not confirmed by the mhVTL engine.",
        ));
    }
    Ok(())
}

pub async fn protect(state: &AppState, barcode: &str) -> Result<Guard, ApiError> {
    cold_vtl_core::Barcode::parse(barcode).map_err(ApiError::bad_request)?;
    crate::deletion::no_pending(state).await?;
    crate::tape_storage::ensure_available(state).await?;
    match online_ready(state).await {
        Ok(()) => Ok(Guard {
            _media: Some(
                MediaReadGuard::acquire(&state.config.media_root.join(barcode))
                    .map_err(ApiError::conflict)?,
            ),
        }),
        Err(online_error) => {
            // An old engine is safe only under the original managed maintenance contract.
            crate::operations::ensure_quiescent(state)
                .await
                .map_err(|_| online_error)?;
            Ok(Guard { _media: None })
        }
    }
}

impl Guard {
    pub async fn check(&self, state: &AppState) -> Result<(), ApiError> {
        crate::tape_storage::ensure_available(state).await?;
        if self._media.is_some() {
            online_ready(state).await
        } else {
            crate::operations::ensure_quiescent(state).await
        }
    }
}

#[cfg(test)]
mod tests;
