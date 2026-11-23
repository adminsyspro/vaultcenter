use cold_vtl_core::Barcode;
use cold_vtl_provider_s3::S3Store;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::error::Error;

pub async fn access(store: &S3Store) -> Result<Value, Box<dyn Error>> {
    let supported = crate::require_legacy_archive_endpoint(store.config()).is_ok();
    let raw = if supported {
        Some(store.bucket_status().await?.status)
    } else {
        None
    };
    let (readable, writable) = access_for_state(supported, raw.as_deref());
    Ok(
        json!({"supported":supported,"bucket":store.config().bucket,"raw_status":raw,"readable":readable,"writable":writable}),
    )
}

fn access_for_state(supported: bool, raw: Option<&str>) -> (bool, bool) {
    if !supported {
        return (true, true);
    }
    (
        matches!(raw, Some("None" | "Restored")),
        raw == Some("None"),
    )
}

pub async fn require_access(store: &S3Store, write: bool) -> Result<(), Box<dyn Error>> {
    let value = access(store).await?;
    if value[if write { "writable" } else { "readable" }] != true {
        return Err(if write {
            "The bucket does not accept writes. After archiving, use another available bucket for new exports. See S3 storage."
        } else {
            "S3 objects are not accessible. In S3 storage, recall the bucket and wait for Recall complete before restoring or synchronising."
        }.into());
    }
    Ok(())
}

pub async fn recall_inspect(store: &S3Store) -> Result<Value, Box<dyn Error>> {
    crate::require_legacy_archive_endpoint(store.config())?;
    let status = store.bucket_status().await?;
    if status.status != "Archived" {
        return Err("A recall requires an Archived bucket. If it is already available or being recalled, do not send the request again.".into());
    }
    store.check_ovh_configuration("cold-vtl").await?;
    let (mut objects, complete) = store.bucket_inventory().await?;
    objects.sort();
    if !complete || objects.is_empty() {
        return Err("Recall inventory incomplete or empty.".into());
    }
    let scope = json!({"bucket":store.config().bucket,"prefix":store.config().prefix,"configuration_id":"cold-vtl",
        "archive_lock_until":status.archive_lock_until,"raw_status":status.status,
        "inventory":inventory(&store.config().prefix,&objects,complete)});
    // Include every object key and size, including any objects outside the VTL prefix.
    let digest = crate::sha256_hex(&serde_json::to_vec(
        &json!({"scope":scope,"objects":objects}),
    )?);
    Ok(json!({"scope":scope,"digest":digest}))
}

/// Deep read-only verification of the bucket alone: complete inventory, no
/// object outside the VTL scope, every copy intact against its manifest
/// (checksums), no multipart upload. A bucket is a snapshot of what was sent:
/// local cartridges keep living and are never read or locked here.
pub async fn inspect(store: &S3Store) -> Result<Value, Box<dyn Error>> {
    crate::require_legacy_archive_endpoint(store.config())?;
    let status = store.bucket_status().await?;
    if status.status != "None" || status.archive_lock_until.is_some() {
        return Err("The bucket must be available and unlocked for a first archive.".into());
    }
    if store.incomplete_multipart_uploads().await? != 0 {
        return Err("Finish multipart uploads before archiving.".into());
    }
    store.archive_versions_clear().await?;
    let (mut objects, complete) = store.bucket_inventory().await?;
    objects.sort();
    let scope = inventory(&store.config().prefix, &objects, complete);
    require_archivable_scope(&scope, objects.is_empty())?;
    let mut expected = BTreeMap::new();
    let mut generations = Vec::new();
    for entry in scope["manifests"].as_array().ok_or("invalid manifests")? {
        let key = entry["key"].as_str().ok_or("invalid key")?;
        let bytes = crate::read_manifest_bytes(store, key).await?;
        let manifest: cold_vtl_core::Manifest = serde_json::from_slice(&bytes)?;
        let barcode = Barcode::parse(entry["barcode"].as_str().ok_or("invalid barcode")?)?;
        let generation = entry["generation"].as_u64().ok_or("invalid generation")?;
        crate::validate_identity(&manifest, store.config(), &barcode, generation)?;
        verify_copy(store, &manifest).await?;
        expected.insert(key.to_owned(), bytes.len() as u64);
        for segment in &manifest.segments {
            if expected
                .insert(segment.object_key.clone(), segment.size_bytes)
                .is_some()
            {
                return Err("Object referenced more than once in the manifests.".into());
            }
        }
        generations.push(cold_vtl_core::RemoteGeneration {
            barcode,
            generation,
            format_version: manifest.format_version,
            manifest_key: key.into(),
            manifest_sha256: crate::sha256_hex(&bytes),
            logical_size_bytes: manifest.logical_size_bytes,
            file_count: manifest.files.len(),
            segment_count: manifest.segments.len(),
            local_match: false,
        });
    }
    if expected.into_iter().collect::<Vec<_>>() != objects {
        return Err("Orphan object, missing segment or size different from the manifest.".into());
    }
    let (mut final_objects, final_complete) = store.bucket_inventory().await?;
    final_objects.sort();
    if !final_complete || final_objects != objects {
        return Err("The bucket changed during verification.".into());
    }
    archive_receipt(
        &store.config().bucket,
        &store.config().prefix,
        scope,
        &generations,
    )
}

/// S3 confirmed every native checksum at export, so a HEAD per segment proves
/// the object is still the one sent. Only copies without native checksums
/// (manifests v1/v2) are downloaded and hashed again.
pub(crate) async fn verify_copy(
    store: &S3Store,
    manifest: &cold_vtl_core::Manifest,
) -> Result<(), Box<dyn Error>> {
    crate::verify_manifest(store, manifest, false).await?;
    if manifest.segments.iter().all(|s| s.s3_checksum.is_some()) {
        return Ok(());
    }
    let mut file_hashes = BTreeMap::<String, Sha256>::new();
    for segment in &manifest.segments {
        let hash = file_hashes.entry(segment.source_path.clone()).or_default();
        crate::stream_segment(
            store,
            &segment.object_key,
            segment.size_bytes,
            &segment.checksum_sha256,
            |chunk| {
                hash.update(chunk);
                Ok(())
            },
        )
        .await?;
    }
    verify_files(&manifest.files, file_hashes)
}

fn require_archivable_scope(scope: &Value, empty: bool) -> Result<(), Box<dyn Error>> {
    if scope["complete"] != true
        || empty
        || scope["outside_prefix"] != 0
        || scope["unrecognized_objects"] != 0
    {
        return Err(
            "Empty bucket, partial inventory or objects outside the recognised VTL scope.".into(),
        );
    }
    Ok(())
}

/// The digest binds the verified bucket content only.
fn archive_receipt(
    bucket: &str,
    prefix: &str,
    scope: Value,
    generations: &[cold_vtl_core::RemoteGeneration],
) -> Result<Value, Box<dyn Error>> {
    let payload =
        json!({"bucket":bucket,"prefix":prefix,"inventory":scope,"generations":generations});
    let digest = crate::sha256_hex(&serde_json::to_vec(&payload)?);
    Ok(json!({"digest":digest,"scope":payload}))
}

fn verify_files(
    files: &[cold_vtl_core::CartridgeFile],
    mut hashes: BTreeMap<String, Sha256>,
) -> Result<(), Box<dyn Error>> {
    for file in files {
        let hash = hashes.remove(&file.path).unwrap_or_default();
        if hex::encode(hash.finalize()) != file.checksum_sha256 {
            return Err(
                "The S3 segments do not reproduce the file checksum declared by the manifest."
                    .into(),
            );
        }
    }
    Ok(())
}

pub async fn status(store: &S3Store) -> Value {
    let config = store.config();
    let supported = config.provider().definition().managed_archive;
    let mut result = json!({"supported":supported,"provider":config.provider(),"provider_label":config.provider().definition().label,
        "bucket":config.bucket,"prefix":config.prefix,"region":config.region,
        "raw_status":null,"archive_lock_until":null,"multipart_uploads":null,"inventory":null});
    if supported && let Ok(status) = store.bucket_status().await {
        result["raw_status"] = json!(status.status);
        result["archive_lock_until"] = json!(status.archive_lock_until);
    }
    if let Ok(count) = store.incomplete_multipart_uploads().await {
        result["multipart_uploads"] = json!(count);
    }
    if let Ok((objects, complete)) = store.bucket_inventory().await {
        result["inventory"] = inventory(&config.prefix, &objects, complete);
    }
    result
}

fn generation(raw: &str) -> Option<u64> {
    raw.parse::<u64>()
        .ok()
        .filter(|n| *n > 0 && n.to_string() == raw)
}

fn inventory(prefix: &str, objects: &[(String, u64)], complete: bool) -> Value {
    let prefix = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}/")
    };
    let mut media = BTreeMap::<String, (u64, u64)>::new();
    let mut manifests = Vec::new();
    let (mut outside, mut unknown, mut bytes) = (0, 0, 0u64);
    for (key, size) in objects {
        bytes = bytes.saturating_add(*size);
        let Some(relative) = key.strip_prefix(&prefix) else {
            outside += 1;
            continue;
        };
        let parts: Vec<_> = relative.split('/').collect();
        if parts.len() < 4 || parts[0] != "cartridges" || Barcode::parse(parts[1]).is_err() {
            unknown += 1;
            continue;
        }
        let entry = media.entry(parts[1].into()).or_default();
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(*size);
        if parts.len() == 4
            && parts[2] == "manifests"
            && let Some(generation) = parts[3].strip_suffix(".json").and_then(generation)
        {
            manifests.push(json!({"barcode":parts[1],"generation":generation,"key":key}));
        } else if !(parts.len() == 6
            && parts[2] == "generations"
            && generation(parts[3]).is_some()
            && parts[4] == "segments"
            && parts[5].ends_with(".bin"))
        {
            unknown += 1;
        }
    }
    json!({"complete":complete,"objects":objects.len(),"bytes":bytes,"outside_prefix":outside,"unrecognized_objects":unknown,
        "cartridges":media.into_iter().map(|(barcode,(objects,bytes))|json!({"barcode":barcode,"objects":objects,"bytes":bytes})).collect::<Vec<_>>(),"manifests":manifests})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recalled_buckets_are_read_only_and_unknown_states_deny_access() {
        assert_eq!(access_for_state(true, Some("Restored")), (true, false));
        assert_eq!(access_for_state(true, Some("None")), (true, true));
        for raw in [
            None,
            Some("Archived"),
            Some("Archiving"),
            Some("Restoring"),
            Some("unexpected"),
        ] {
            assert_eq!(access_for_state(true, raw), (false, false));
        }
        assert_eq!(access_for_state(false, None), (true, true));
    }
    #[test]
    fn valid_segments_must_also_reconstruct_the_declared_file_digest() {
        let files = vec![cold_vtl_core::CartridgeFile {
            path: "data.0".into(),
            size_bytes: 3,
            checksum_sha256: crate::sha256_hex(b"abc"),
        }];
        let mut hashes = BTreeMap::new();
        hashes.insert(
            "data.0".into(),
            Sha256::new().chain_update(b"ab").chain_update(b"c"),
        );
        assert!(verify_files(&files, hashes).is_ok());
        let mut hashes = BTreeMap::new();
        hashes.insert("data.0".into(), Sha256::new().chain_update(b"xyz"));
        assert!(verify_files(&files, hashes).is_err());
    }
    #[test]
    fn scope_covers_other_prefixes_and_unknown_objects_without_claiming_verification() {
        let result = inventory(
            "vtl",
            &[
                ("vtl/cartridges/TAPE01/manifests/2.json".into(), 10),
                (
                    "vtl/cartridges/TAPE01/generations/2/segments/0000-ab.bin".into(),
                    20,
                ),
                ("vtl-other/file".into(), 30),
                ("foreign".into(), 40),
                ("vtl/cartridges/TAPE02/unknown/file".into(), 50),
                ("vtl/cartridges/tape03/manifests/1.json".into(), 60),
            ],
            false,
        );
        assert_eq!(result["objects"], 6);
        assert_eq!(result["bytes"], 210);
        assert_eq!(result["outside_prefix"], 2);
        assert_eq!(result["unrecognized_objects"], 2);
        assert_eq!(result["cartridges"].as_array().unwrap().len(), 2);
        assert_eq!(result["manifests"].as_array().unwrap().len(), 1);
        assert_eq!(result["complete"], false);
    }
    #[test]
    fn empty_prefix_and_noncanonical_generations() {
        let result = inventory(
            "",
            &[("cartridges/TAPE01/manifests/01.json".into(), 1)],
            true,
        );
        assert_eq!(result["outside_prefix"], 0);
        assert_eq!(result["unrecognized_objects"], 1);
        assert_eq!(result["manifests"], json!([]));
    }
    #[test]
    fn archive_receipt_binds_only_the_bucket_content() {
        let generation = cold_vtl_core::RemoteGeneration {
            barcode: Barcode::parse("TAPE01").unwrap(),
            generation: 2,
            format_version: 2,
            manifest_key: "vtl/cartridges/TAPE01/manifests/2.json".into(),
            manifest_sha256: "a".repeat(64),
            logical_size_bytes: 10,
            file_count: 1,
            segment_count: 1,
            local_match: false,
        };
        let scope = inventory(
            "vtl",
            &[("vtl/cartridges/TAPE01/manifests/2.json".into(), 10)],
            true,
        );
        let first = archive_receipt(
            "bucket",
            "vtl",
            scope.clone(),
            std::slice::from_ref(&generation),
        )
        .unwrap();
        let again = archive_receipt(
            "bucket",
            "vtl",
            scope.clone(),
            std::slice::from_ref(&generation),
        )
        .unwrap();
        assert_eq!(first["digest"], again["digest"]);
        assert!(first["scope"].get("locals").is_none());
        assert_eq!(first["scope"]["generations"][0]["local_match"], false);
        let other = archive_receipt("other-bucket", "vtl", scope, &[generation]).unwrap();
        assert_ne!(first["digest"], other["digest"]);
    }

    #[test]
    fn archive_scope_refuses_partial_empty_or_foreign_inventories() {
        let manifest = || vec![("vtl/cartridges/TAPE01/manifests/2.json".to_owned(), 10)];
        assert!(require_archivable_scope(&inventory("vtl", &manifest(), true), false).is_ok());
        assert!(require_archivable_scope(&inventory("vtl", &manifest(), true), true).is_err());
        assert!(require_archivable_scope(&inventory("vtl", &manifest(), false), false).is_err());
        assert!(
            require_archivable_scope(&inventory("vtl", &[("foreign".into(), 1)], true), false)
                .is_err()
        );
        assert!(
            require_archivable_scope(
                &inventory("vtl", &[("vtl/cartridges/TAPE01/other".into(), 1)], true),
                false
            )
            .is_err()
        );
    }
}
