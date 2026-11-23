mod cartridge;
mod generation;
mod media_lock;
pub use generation::{
    ExportReceipt, ExportScope, IntegrityValidation, ReconcileOutput, RemoteGeneration,
    safe_manifest_path,
};
pub use media_lock::MediaReadGuard;

pub use cartridge::{
    Barcode, BarcodeError, Cartridge, CartridgeError, CartridgeFile, CartridgeState, Manifest,
    NativeChecksum, Segment,
};

mod s3_provider;
pub use s3_provider::{
    MANAGED_BUCKET_PREFIX, OVH_ARCHIVE_ENDPOINT, S3Provider, S3ProviderDefinition,
    suggested_managed_bucket_name, valid_managed_bucket_name,
};

mod vtl;
pub use vtl::{DestinationId, VtlId};

mod rotation;
pub use rotation::{CivilDate, RotationSchedule};
