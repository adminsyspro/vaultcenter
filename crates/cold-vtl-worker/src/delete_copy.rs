//! Deletes only the explicitly pinned generation; never touches local media.
use cold_vtl_core::Barcode;
use cold_vtl_provider_s3::S3Store;
use serde_json::Value;
use std::error::Error;
pub async fn run(
    store: &S3Store,
    barcode: &Barcode,
    generation: u64,
    hash: &str,
    execute: bool,
) -> Result<Value, Box<dyn Error>> {
    crate::storage::require_access(store, true).await?;
    store.require_unversioned_deletion().await?;
    let key = crate::manifest_key(store.config(), barcode, generation);
    let bytes = crate::read_manifest_bytes(store, &key).await?;
    if hash.len() != 64 || crate::sha256_hex(&bytes) != hash {
        return Err("The S3 copy changed. Refresh storage before deleting it.".into());
    }
    let manifest: cold_vtl_core::Manifest = serde_json::from_slice(&bytes)?;
    crate::validate_identity(&manifest, store.config(), barcode, generation)?;
    let mut keys = std::collections::BTreeSet::new();
    let prefix = store.config().object_key(&format!(
        "cartridges/{barcode}/generations/{generation}/segments/"
    ));
    for segment in &manifest.segments {
        let name = segment
            .object_key
            .strip_prefix(&prefix)
            .ok_or("Segment outside the generation.")?;
        if !name.ends_with(".bin")
            || name.contains('/')
            || name.contains('\\')
            || name.contains('%')
            || name.contains('?')
            || name.contains('#')
        {
            return Err("Invalid S3 segment key.".into());
        }
        // No prefix deletion: only validated, generation-owned manifest entries.
        if !keys.insert(segment.object_key.clone()) {
            return Err("A segment is referenced more than once in the manifest.".into());
        }
    }
    if execute {
        for segment in &keys {
            store.delete_object(segment).await?;
        }
        // Keep the manifest on partial failure so the exact same deletion can retry.
        // Recheck access and identity before removing the final inventory entry.
        crate::storage::require_access(store, true).await?;
        if crate::sha256_hex(&crate::read_manifest_bytes(store, &key).await?) != hash {
            return Err("The manifest changed during deletion. Check the bucket.".into());
        }
        store.delete_object(&key).await?;
    }
    Ok(
        serde_json::json!({"status":if execute {"deleted"} else {"checked"},"barcode":barcode,"generation":generation,"bucket":store.config().bucket,"manifest_sha256":hash,"objects":keys.len()+1,"logical_size_bytes":manifest.logical_size_bytes}),
    )
}
#[cfg(test)]
mod tests;
