//! Remote inventory has no dependency on local media or catalogue rows.
use cold_vtl_core::{Barcode, Manifest, RemoteGeneration};
use cold_vtl_provider_s3::S3Store;
use serde_json::{Value, json};
use std::error::Error;

fn identity(prefix: &str, key: &str) -> Option<(Barcode, u64)> {
    let mut pieces = key.strip_prefix(prefix)?.split('/');
    let barcode = Barcode::parse(pieces.next()?).ok()?;
    if pieces.next()? != "manifests" {
        return None;
    }
    let generation = pieces.next()?.strip_suffix(".json")?.parse::<u64>().ok()?;
    if pieces.next().is_some() || generation == 0 {
        return None;
    }
    Some((barcode, generation))
}

pub async fn inventory(store: &S3Store) -> Result<Value, Box<dyn Error>> {
    let access = crate::storage::access(store).await?;
    let prefix = store.config().object_key("cartridges/");
    let mut copies = Vec::new();
    let mut errors = Vec::new();
    // Preserve successfully read pages on a later page failure.
    let (objects, complete) = store.recovery_inventory_objects().await?;
    for (key, _) in objects {
        let Some((barcode, generation)) = identity(&prefix, &key) else {
            continue;
        };
        if key != crate::manifest_key(store.config(), &barcode, generation) {
            continue;
        }
        if access["readable"] != true {
            copies.push(
                json!({"barcode":barcode,"generation":generation,"manifest_key":key,
                "manifest_sha256":null,"logical_size_bytes":null,"local_match":false,
                "availability":"recall_required"}),
            );
            continue;
        }
        let result: Result<RemoteGeneration, Box<dyn Error>> = async {
            let bytes = crate::read_manifest_bytes(store, &key).await?;
            let manifest: Manifest = serde_json::from_slice(&bytes)?;
            crate::validate_identity(&manifest, store.config(), &barcode, generation)?;
            Ok(RemoteGeneration {
                barcode,
                generation,
                format_version: manifest.format_version,
                manifest_key: key.clone(),
                manifest_sha256: crate::sha256_hex(&bytes),
                logical_size_bytes: manifest.logical_size_bytes,
                file_count: manifest.files.len(),
                segment_count: manifest.segments.len(),
                local_match: false,
            })
        }
        .await;
        match result {
            Ok(copy) => { let mut value = serde_json::to_value(copy)?; value["availability"] = json!("readable"); copies.push(value); }
            Err(_) => errors.push(json!({"key":key,"error":"Manifest unavailable or invalid; refresh after checking bucket access."})),
        }
    }
    if !complete {
        errors.push(
            json!({"key":null,"error":"Remote listing incomplete; some pages could not be read."}),
        );
    }
    copies.sort_by_key(|c| {
        (
            c["barcode"].as_str().unwrap_or_default().to_owned(),
            c["generation"].as_u64().unwrap_or(0),
        )
    });
    Ok(
        json!({"bucket":store.config().bucket,"copies":copies,"complete":complete && errors.is_empty(),"errors":errors,"access":access}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_manifest_keys_need_no_local_or_new_identity() {
        assert_eq!(
            identity("old/cartridges/", "old/cartridges/TAPE01/manifests/3.json")
                .unwrap()
                .1,
            3
        );
        for key in [
            "old/cartridges/TAPE01/manifests/0.json",
            "foreign/cartridges/TAPE01/manifests/3.json",
            "old/cartridges/TAPE01/manifests/3.json/extra",
            "old/cartridges/TAPE01/generations/3",
        ] {
            assert!(identity("old/cartridges/", key).is_none());
        }
    }
}
