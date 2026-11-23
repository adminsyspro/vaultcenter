use crate::{Barcode, Manifest};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RemoteGeneration {
    pub barcode: Barcode,
    pub generation: u64,
    pub format_version: u16,
    pub manifest_key: String,
    pub manifest_sha256: String,
    pub logical_size_bytes: u64,
    pub file_count: usize,
    pub segment_count: usize,
    pub local_match: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReconcileOutput {
    pub generations: Vec<RemoteGeneration>,
    pub local_size_bytes: u64,
}

pub fn safe_manifest_path(raw: &str) -> bool {
    !raw.is_empty()
        && !raw.contains('\\')
        && !raw.contains('\0')
        && raw
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

pub fn valid_sha256(raw: &str) -> bool {
    raw.len() == 64
        && raw
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

impl Manifest {
    /// Validate before creating any restore files, including legacy segment-only manifests.
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.format_version, 1..=3) || self.generation == 0 {
            return Err("unsupported manifest format or generation".into());
        }
        let mut files = HashMap::new();
        for file in &self.files {
            if !safe_manifest_path(&file.path)
                || !valid_sha256(&file.checksum_sha256)
                || files.insert(file.path.as_str(), file.size_bytes).is_some()
            {
                return Err("invalid or duplicate file in manifest".into());
            }
        }
        let mut offsets = HashMap::<&str, u64>::new();
        let mut keys = HashSet::new();
        let mut total = 0u64;
        for (index, segment) in self.segments.iter().enumerate() {
            if segment.sequence != index as u64
                || segment.size_bytes == 0
                || !safe_manifest_path(&segment.source_path)
                || !valid_sha256(&segment.checksum_sha256)
                || (self.format_version == 3
                    && !segment.s3_checksum.as_ref().is_some_and(|c| c.validate()))
                || (self.format_version < 3 && segment.s3_checksum.is_some())
                || !keys.insert(&segment.object_key)
            {
                return Err("invalid segment in manifest".into());
            }
            if self.format_version >= 2 && !files.contains_key(segment.source_path.as_str()) {
                return Err("segment references an undeclared file".into());
            }
            let offset = offsets.entry(&segment.source_path).or_default();
            if *offset != segment.source_offset_bytes {
                return Err("non-contiguous segment offsets".into());
            }
            *offset = offset
                .checked_add(segment.size_bytes)
                .ok_or("file size overflow")?;
            total = total
                .checked_add(segment.size_bytes)
                .ok_or("logical size overflow")?;
        }
        if total != self.logical_size_bytes {
            return Err("manifest size mismatch".into());
        }
        if self.format_version >= 2 {
            for (path, size) in &files {
                if offsets.get(path).copied().unwrap_or(0) != *size {
                    return Err("file size mismatch".into());
                }
            }
        }
        let paths = if self.format_version >= 2 {
            files.keys().copied().collect::<Vec<_>>()
        } else {
            offsets.keys().copied().collect()
        };
        let path_set: HashSet<_> = paths.iter().copied().collect();
        for path in paths {
            let mut parent = path;
            while let Some((prefix, _)) = parent.rsplit_once('/') {
                if path_set.contains(prefix) {
                    return Err("file/directory collision in manifest".into());
                }
                parent = prefix;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CartridgeFile, Segment};
    fn example() -> Manifest {
        Manifest {
            format_version: 2,
            barcode: Barcode::parse("TAPE01").unwrap(),
            generation: 1,
            logical_size_bytes: 4,
            files: vec![CartridgeFile {
                path: "data".into(),
                size_bytes: 4,
                checksum_sha256: "a".repeat(64),
            }],
            segments: vec![Segment {
                sequence: 0,
                source_path: "data".into(),
                source_offset_bytes: 0,
                size_bytes: 4,
                checksum_sha256: "a".repeat(64),
                object_key: "segment".into(),
                s3_checksum: None,
            }],
        }
    }
    #[test]
    fn v3_requires_native_checksums_without_weakening_legacy_validation() {
        let mut value = serde_json::to_value(example()).unwrap();
        value["format_version"] = 3.into();
        assert!(
            serde_json::from_value::<Manifest>(value.clone())
                .unwrap()
                .validate()
                .is_err()
        );
        value["segments"][0]["s3_checksum"] = serde_json::json!({"kind":"COMPOSITE", "value":"ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=-2"});
        assert!(
            serde_json::from_value::<Manifest>(value.clone())
                .unwrap()
                .validate()
                .is_ok()
        );
        value["segments"][0]["s3_checksum"]["value"] = "garbage".into();
        assert!(
            serde_json::from_value::<Manifest>(value)
                .unwrap()
                .validate()
                .is_err()
        );
        assert!(example().validate().is_ok());
    }
    #[test]
    fn rejects_corrupt_layouts_before_restore() {
        assert!(example().validate().is_ok());
        let mut m = example();
        m.segments[0].source_offset_bytes = 1;
        assert!(m.validate().is_err());
        let mut m = example();
        m.files[0].path = "../data".into();
        assert!(m.validate().is_err());
        let mut m = example();
        m.files.push(m.files[0].clone());
        assert!(m.validate().is_err());
        let mut m = example();
        m.segments[0].source_path = "unexpected".into();
        assert!(m.validate().is_err());
        let mut m = example();
        m.files[0].size_bytes = 5;
        assert!(m.validate().is_err());
        let mut m = example();
        m.files.push(CartridgeFile {
            path: "data/child".into(),
            size_bytes: 0,
            checksum_sha256: "a".repeat(64),
        });
        assert!(m.validate().is_err());
    }
    #[test]
    fn accepts_legacy_segments_and_empty_files() {
        let mut m = example();
        m.format_version = 1;
        m.files.clear();
        assert!(m.validate().is_ok());
        let mut m = example();
        m.files.push(CartridgeFile {
            path: "empty".into(),
            size_bytes: 0,
            checksum_sha256: "a".repeat(64),
        });
        assert!(m.validate().is_ok());
    }
}

/// Durable result exposed in the export job; UI placement is intentionally separate.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IntegrityValidation {
    pub status: String,
    pub method: String,
    pub verified_at: u64,
    pub verified_segments: usize,
    pub verified_bytes: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExportReceipt {
    pub scope: ExportScope,
    pub generation: RemoteGeneration,
    pub integrity: IntegrityValidation,
}
impl ExportReceipt {
    pub fn validate(&self, barcode: &Barcode, generation: u64) -> Result<(), String> {
        let copy = &self.generation;
        let proof = &self.integrity;
        let suffix = format!("cartridges/{barcode}/manifests/{generation}.json");
        if &copy.barcode != barcode
            || copy.generation != generation
            || copy.format_version != 3
            || !copy.local_match
            || !valid_sha256(&copy.manifest_sha256)
            || !(copy.manifest_key == suffix || copy.manifest_key.ends_with(&format!("/{suffix}")))
            || !safe_manifest_path(&copy.manifest_key)
            || proof.status != "verified"
            || proof.method != "s3_sha256"
            || proof.verified_at == 0
            || proof.verified_segments != copy.segment_count
            || proof.verified_bytes != copy.logical_size_bytes
        {
            return Err("invalid export integrity receipt".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ExportScope {
    pub job_id: Option<String>,
    pub vtl_id: Option<String>,
    pub destination_id: Option<String>,
    pub configuration_revision: Option<i64>,
    pub source: String,
}
impl ExportReceipt {
    pub fn validate_scope(&self, expected: &ExportScope, manifest_key: &str) -> Result<(), String> {
        if &self.scope != expected || self.generation.manifest_key != manifest_key {
            return Err(
                "export integrity receipt belongs to another operation or destination".into(),
            );
        }
        Ok(())
    }
}
