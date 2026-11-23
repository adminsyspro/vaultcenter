use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Barcode(String);

impl Barcode {
    pub fn parse(value: impl Into<String>) -> Result<Self, BarcodeError> {
        let value = value.into();

        if !(6..=16).contains(&value.len()) {
            return Err(BarcodeError::InvalidLength);
        }

        if !value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(BarcodeError::InvalidCharacter);
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Barcode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for Barcode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CartridgeState {
    Writable,
    Sealed,
    Uploading,
    Verified,
    Archiving,
    Archived,
    Restoring,
    Hydrating,
    Online,
    Failed,
}

impl CartridgeState {
    pub fn can_transition_to(self, next: Self) -> bool {
        use CartridgeState::*;

        matches!(
            (self, next),
            (Writable, Sealed)
                | (Sealed, Uploading)
                | (Uploading, Verified | Failed)
                | (Verified, Archiving | Online)
                | (Archiving, Archived | Failed)
                | (Archived, Restoring)
                | (Restoring, Hydrating | Failed)
                | (Hydrating, Online | Failed)
                | (Online, Archiving)
                | (Failed, Sealed | Verified | Archived)
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Cartridge {
    pub barcode: Barcode,
    pub capacity_bytes: u64,
    #[serde(default)]
    pub media_type: Option<String>,
    #[serde(default = "default_segment_size_mib")]
    pub segment_size_mib: u32,
    #[serde(default = "default_multipart_part_size_mib")]
    pub multipart_part_size_mib: u32,
    pub bytes_written: u64,
    pub generation: u64,
    pub state: CartridgeState,
}

fn default_segment_size_mib() -> u32 {
    256
}
fn default_multipart_part_size_mib() -> u32 {
    256
}

impl Cartridge {
    pub fn new(barcode: Barcode, capacity_bytes: u64) -> Result<Self, CartridgeError> {
        if capacity_bytes == 0 {
            return Err(CartridgeError::ZeroCapacity);
        }

        Ok(Self {
            barcode,
            capacity_bytes,
            media_type: None,
            segment_size_mib: 1_048_576,
            multipart_part_size_mib: default_multipart_part_size_mib(),
            bytes_written: 0,
            generation: 1,
            state: CartridgeState::Writable,
        })
    }

    pub fn record_write(&mut self, bytes: u64) -> Result<(), CartridgeError> {
        if self.state != CartridgeState::Writable {
            return Err(CartridgeError::NotWritable(self.state));
        }

        let next_size = self
            .bytes_written
            .checked_add(bytes)
            .ok_or(CartridgeError::CapacityExceeded)?;

        if next_size > self.capacity_bytes {
            return Err(CartridgeError::CapacityExceeded);
        }

        self.bytes_written = next_size;
        Ok(())
    }

    pub fn transition_to(&mut self, next: CartridgeState) -> Result<(), CartridgeError> {
        if !self.state.can_transition_to(next) {
            return Err(CartridgeError::InvalidTransition {
                from: self.state,
                to: next,
            });
        }

        self.state = next;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Segment {
    pub sequence: u64,
    pub source_path: String,
    pub source_offset_bytes: u64,
    pub size_bytes: u64,
    pub checksum_sha256: String,
    pub object_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s3_checksum: Option<NativeChecksum>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Manifest {
    pub format_version: u16,
    pub barcode: Barcode,
    pub generation: u64,
    pub logical_size_bytes: u64,
    #[serde(default)]
    pub files: Vec<CartridgeFile>,
    pub segments: Vec<Segment>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CartridgeFile {
    pub path: String,
    pub size_bytes: u64,
    pub checksum_sha256: String,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum BarcodeError {
    #[error("barcode length must be between 6 and 16 ASCII characters")]
    InvalidLength,
    #[error("barcode may only contain ASCII uppercase letters, digits, and '-'")]
    InvalidCharacter,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum CartridgeError {
    #[error("cartridge capacity must be greater than zero")]
    ZeroCapacity,
    #[error("cartridge capacity exceeded")]
    CapacityExceeded,
    #[error("cartridge is not writable in state {0:?}")]
    NotWritable(CartridgeState),
    #[error("invalid cartridge transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: CartridgeState,
        to: CartridgeState,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_cartridge_json_has_unknown_media_and_default_segments() {
        let cartridge: Cartridge = serde_json::from_str(r#"{"barcode":"TAPE01","capacity_bytes":1048576,"bytes_written":0,"generation":1,"state":"writable"}"#).unwrap();
        let encoded = serde_json::to_value(cartridge).unwrap();
        assert_eq!(encoded["segment_size_mib"], 256);
        assert_eq!(encoded["multipart_part_size_mib"], 256);
        assert!(encoded["media_type"].is_null());
    }

    #[test]
    fn barcode_validation_rejects_lowercase_and_bad_lengths() {
        assert_eq!(Barcode::parse("TAPE01").unwrap().as_str(), "TAPE01");
        assert_eq!(
            Barcode::parse("tape01"),
            Err(BarcodeError::InvalidCharacter)
        );
        assert_eq!(Barcode::parse("SHORT"), Err(BarcodeError::InvalidLength));
    }

    #[test]
    fn a_cartridge_cannot_exceed_its_capacity() {
        let mut cartridge = Cartridge::new(Barcode::parse("TAPE0001").unwrap(), 10).unwrap();
        cartridge.record_write(8).unwrap();

        assert_eq!(
            cartridge.record_write(3),
            Err(CartridgeError::CapacityExceeded)
        );
        assert_eq!(cartridge.bytes_written, 8);
    }

    #[test]
    fn archive_requires_a_verified_upload() {
        let mut cartridge = Cartridge::new(Barcode::parse("TAPE0001").unwrap(), 10).unwrap();

        assert!(matches!(
            cartridge.transition_to(CartridgeState::Archiving),
            Err(CartridgeError::InvalidTransition { .. })
        ));

        cartridge.transition_to(CartridgeState::Sealed).unwrap();
        cartridge.transition_to(CartridgeState::Uploading).unwrap();
        cartridge.transition_to(CartridgeState::Verified).unwrap();
        cartridge.transition_to(CartridgeState::Archiving).unwrap();
        cartridge.transition_to(CartridgeState::Archived).unwrap();
    }

    #[test]
    fn archived_media_must_be_restored_and_hydrated_before_it_is_online() {
        let mut state = CartridgeState::Archived;

        for next in [
            CartridgeState::Restoring,
            CartridgeState::Hydrating,
            CartridgeState::Online,
        ] {
            assert!(state.can_transition_to(next));
            state = next;
        }
    }
}

/// Server-validated SHA-256, distinct from the ordinary whole-segment digest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativeChecksum {
    pub value: String,
    pub kind: String,
}
impl NativeChecksum {
    pub fn validate(&self) -> bool {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let digest = match self.kind.as_str() {
            "FULL_OBJECT" => self.value.as_str(),
            "COMPOSITE" => match self.value.rsplit_once('-') {
                Some((digest, count))
                    if count
                        .parse::<u32>()
                        .is_ok_and(|n| (1..=10000).contains(&n) && n.to_string() == count) =>
                {
                    digest
                }
                _ => return false,
            },
            _ => return false,
        };
        STANDARD
            .decode(digest)
            .is_ok_and(|bytes| bytes.len() == 32 && STANDARD.encode(bytes) == digest)
    }
}
