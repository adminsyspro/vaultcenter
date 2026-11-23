# S3 segments and uploads

## Sending a cartridge without stopping PBS

In the library, select the finished, unloaded cartridges, then
**Send to S3**. The confirmation prepares the cartridges and starts sending
them directly: no manual sealing or switch to maintenance is needed with the
compatible engine. PBS can keep backing up to the other cartridges of the same
VTL. Disk and network throughput remain shared.

The cartridge being sent cannot be loaded during the preparation, the upload
and the checks. A cartridge that is already loaded is refused: unload it in
PBS, then start the send again. The application does not unload a drive and
does not interrupt a backup to perform an export.

The protection relies on the same directory lock: exclusive in mhVTL before
any read/write of the MAM until the unload, shared in the API and,
independently, in each worker until the end of its checks. The death of the
API therefore does not release the protection held by a surviving worker.
Fingerprint checks and multipart resume remain active.

The controller checks for the `VAULTCENTER_MEDIA_LOCK_V1` capability in
`/usr/lib/libvtlscsi.so` and the identity of that library as actually loaded
by each drive (`/proc/<pid>/maps`). An old process with a new library on disk
is not enough. Deploying the fix requires restarting the drives once, after
the PBS operations in progress have finished. Installations without this
capability keep their path under maintenance, with no way around the check.

**S3 storage → Copies and buckets** finds the remote copies, including
without a local catalogue. This lookup does not send any file.
The upload to S3 and the cold archiving of the bucket remain two separate
operations. The online export does not allow writing to the cartridge being
sent at the same time; no snapshot of a loaded cartridge is created.

A cartridge defines two independent sizes:

| Setting | Initial value | Range |
| --- | --- | --- |
| Segment, object visible in the bucket | 1 TiB | 16 MiB to 1 TiB |
| Multipart part, assembled into that object | 256 MiB | 16 to 1024 MiB |

A full 1 TiB segment uses 4096 parts of 256 MiB. VaultCenter refuses
combinations that exceed 10,000 parts per segment. Older cartridges keep
their segment size; the migration gives them 256 MiB parts.
The settings are defined at creation, without changing how PBS splits its
writes. TiB and MiB are binary units; 1 TiB is about 1.10 TB.

Each mhVTL file is split separately. Its last segment may be smaller and the
small metadata files remain separate objects. VaultCenter does not group
these files to fill a 1 TiB object.

## Export and verification

New exports start after the file inventory and the acquisition of the
cartridge lock, with no full prior read. The worker reads a part, computes its
SHA-256 and sends it with the native S3 checksum. It requires the same
checksum in the response. Segments use a COMPOSITE SHA-256 multipart upload,
including a segment made of a single part; the final manifest uses a validated
PUT. An endpoint that does not confirm checksums causes an explicit failure.

Only one part is buffered per export: 256 MiB by default, up to 1 GiB depending
on the setting. Reading/computing and sending are sequential to limit memory. Several
cartridges always share the configured throughput of their VTL. A 1 TiB
segment is never loaded into memory.

After assembly, HEAD with ChecksumMode checks the size, the COMPOSITE type and
the composite fingerprint of the binary SHA-256 values of the parts, in order.
This is not the classic SHA-256 of the segment, which is kept separately for
restores. The export does not download any segment again. The manifest,
published last, is read back. If the assembly response is lost or unreadable
while the object exists, that same HEAD settles the matter without failing the
export; if there is no object, the full S3 error (code and cause) is kept in
the job and the export remains resumable.
The worker also checks that the source files have not changed (identity,
size, mtime and ctime), under the shared lock respected by mhVTL.

v3 manifests contain the native S3 checksums and the classic fingerprints per
file/segment. v1/v2 manifests remain readable; an old engine cannot read v3.
The explicit deep verification and the restore keep their integrity reads;
archive preparation re-reads only v1/v2 copies. Upload validation attests to
the data received by S3, not to a later physical audit.

The job result includes `receipt.integrity`: status `verified`, method
`s3_sha256`, Unix date `verified_at`, `verified_segments` and `verified_bytes`.
The receipt contains the generation and the fingerprint of the manifest, and is
tied to the job, the VTL, the destination/revision and the source path. The
API checks this scope, keeps the other known generations and finishes without
a deep reconciliation.
Under the library, the S3 storage panel links each cartridge to the job's
destination and shows the volume transferred, its decimal percentage and its
throughput. At the end, “Fingerprint verified” appears only for a finished job
whose receipt matches the cartridge, its generation, the VTL and the
destination/revision. An upload that ends without this receipt is shown as
“Fingerprint unconfirmed”.

## Resuming an interrupted export

**Cartridges → Resume** restarts a failed export with the same generation and
the same destination. The catalogue checks that the last job of this cartridge
is indeed this failed export, and that the S3 configuration has not changed.
Eligibility does not depend on the 100 jobs visible in the history. A
verification or restore error does not allow an export to be resumed.

The resume uses the same per-cartridge protection as the first send. On an
old engine, it still requires managed maintenance.

The private log keeps the multipart identifier, the ETag/size/SHA-256 receipts,
the identity of the files and the SHA-256 states at each confirmed part. It is
stored in `uploads/`, next to `COLD_VTL_DATABASE`. The CLI lets you choose
another folder with `--upload-state-dir`, which must be outside the cartridge
files. Keep this folder with the catalogue when backing up the installation.
The state format is explicitly tied to sha2 0.11; an incompatible change must
plan their migration or refuse the resume.

The resume compares the identity of the source, the destination and the
settings, checks the parts already confirmed on the S3 side, then restores the
computation states without re-reading the prefix already sent. A lost part
response leads to sending that part again with the same number. A lost
assembly response is resolved with a native HEAD. A response lost after the
manifest was published is resolved by an exact comparison of that small
document. The finished log is kept to allow this resume.

Historical v1 logs still use the earlier protocol, with its full reads and its
`--skip-deep-verify` parameter. This parameter never disables the native
validation of new exports. If an old export had published its manifest and
then deleted its log before losing its response, use an explicit
synchronisation to find its copy: the new engine refuses to adopt an existing
generation without a local receipt proving its source.

A resume may require operator action:

- **Ambiguous multipart creation**: if S3 created the upload but VaultCenter
  did not receive its identifier, VaultCenter blocks new attempts. S3 does not
  make it possible to identify its owner with certainty from the list of
  incomplete uploads alone. Inspect the log and the S3 traces to establish the
  exact identifier before any recovery; do not delete the log or abort an
  upload based only on a match of its object name.
- **Modified source, destination or parts**, or upload deleted by an expiry
  rule on the bucket: the resume is refused and the data is kept for
  inspection. It does not silently recreate an export.

The interface offers a manual restart, not an automatic resume after a
restart. An export transfers the objects to S3; archiving the bucket remains a
separate operation.

## Local validation

The tests use a simulated S3 server on loopback: interruption of a part, lost
responses at creation and assembly, resume, corruption and restore. A sparse
file checks the offsets at 1 TiB without transferring that volume. These
tests do not measure the throughput of a 1 TiB export to a real provider.
