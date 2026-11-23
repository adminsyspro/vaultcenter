# Architecture

## Scope of the data plane

VaultCenter does not implement SCSI Sequential Commands (SSC) or SCSI Media
Changer (SMC) commands from scratch.

The data plane is an isolated Debian appliance running mhVTL, with its drives
and changers exposed through `tgt` SCSI passthrough over iSCSI. The Rust
services own cartridge metadata, export jobs, cloud state, integrity checks,
and restore orchestration.

## Cartridge object layout

```text
cartridges/<barcode>/
  manifests/<generation>.json
  segments/<generation>/<sequence>.bin
  indexes/<generation>/filemarks.bin
```

Segments are immutable. A manifest is uploaded last and acts as the commit for
a cartridge generation. New cartridges default to 1 TiB segments, configurable
from 16 MiB to 1 TiB; existing cartridge settings remain unchanged. Multipart
transport uses independent 256 MiB parts by default (16–1024 MiB), at most
10,000 parts per segment and four concurrent streamed uploads. Each segment records its source path and offset, sequence, byte
length, SHA-256 checksum, and object key. Manifest format 2 also records a
checksum for each reconstructed source file.

## OVHcloud archive modes

The provider must distinguish the two OVHcloud products from the endpoint:

- `s3.rbx-archive.io.cloud.ovh.net` is legacy Cold Archive v1. Archiving and
  restoring apply to the whole bucket through OVH-specific S3 Intelligent
  Tiering values. A bucket therefore represents an archive batch, not one
  independently restorable cartridge.
- `s3.eu-west-par.io.cloud.ovh.net` can use Cold Archive v2. Objects can be
  moved independently to the `DEEP_ARCHIVE`-mapped storage class.

The worker refuses a v1 archive request unless the operator repeats the exact
bucket name, acknowledges the 1 TiB minimum billing, and no incomplete
multipart upload is present. It intentionally provides no archive deletion
operation.

References:

- <https://docs.ovhcloud.com/en/guides/storage-and-backup/object-storage/cold-archive-faq>
- <https://docs.ovhcloud.com/en/guides/storage-and-backup/object-storage/cold-archive-getting-started>
- <https://docs.ovhcloud.com/en/guides/storage-and-backup/object-storage/s3-restoring-objects>

## Cartridge lifecycle

```text
Writable -> Sealed -> Uploading -> Verified -> Archiving -> Archived
                                                       -> Failed
Archived -> Restoring -> Hydrating -> Online
                               \-> Failed
```

Only a sealed cartridge may be uploaded. Only a verified remote copy may be
archived. Archived media must be restored and hydrated into the local spool
before it can be mounted for reading.

## Components

### `cold-vtl-core`

Provider-independent domain objects and state-transition validation. This
crate must remain usable by the API, workers, CLI, and future SCSI adapter.

### `cold-vtl-api`

HTTP control API and job supervisor. The current catalogue uses SQLite in WAL
mode, persists cartridges and jobs across daemon restarts, and discovers mhVTL
media at startup. PostgreSQL remains an option if multi-node control-plane
deployment becomes necessary.

The daemon embeds a dependency-free web console so the appliance does not need
a separate Node.js runtime or frontend deployment.

### `cold-vtl-cli`

Operator CLI backed by the same HTTP API as the web console. It lists and seals
media, starts exports, and follows persistent asynchronous jobs.

### `cold-vtl-worker`

Segmentation, SHA-256 checksums, S3 upload, deep verification, reconstruction,
and guarded legacy OVHcloud bucket archive/restore operations.

### `cold-vtl-provider-s3`

Generic S3-compatible data plane used by the worker.

### Possible future components

- `cold-vtl-provider-ovh`: OVH account and archive lifecycle operations.
- `cold-vtl-scsi-adapter`: bridge to another SCSI VTL implementation.

## Safety invariants

- Never archive a cartridge that is writable or has an incomplete upload.
- Upload the manifest only after every referenced segment is verified.
- Never delete the final local copy before the configured remote quorum is met.
- A restore request is asynchronous and cannot make a cartridge mountable by
  itself; hydration and integrity verification must complete first.
- Destructive credentials must be separate from upload-only credentials.

## Web console

The `ui/` package adapts Studio Admin (Next.js, TypeScript, Tailwind v4,
shadcn Radix Nova). Its static export is served by `cold-vtld` when
`COLD_VTL_UI_DIR` is set. All runtime data are loaded from same-origin `/v1/`
endpoints. No Node.js service or credentials in the frontend are required.

The static handler only handles unmatched paths, preserving API routing.
Missing assets and unknown endpoints return 404, rather than console HTML.
HTML and assets revalidate (`Cache-Control: no-cache`) on deployment.
The existing embedded console remains a fallback when the directory is unset.
