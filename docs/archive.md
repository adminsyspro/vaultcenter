# Controlled archiving of the OVH bucket

An administrator archives an OVH Cold Archive v1 bucket to tape, then recalls it
when its copies need to be read again. Archiving checks only the bucket:
no local cartridge is read or locked, and maintenance is not required.

## Workflow

The window of a bucket (**S3 storage → Manage bucket**) shows its state on one line
and a single action.

1. **Archive to tape…** opens the confirmation: cartridges, volume, copies recorded
   at upload, OVH terms (minimum billing of 1 TiB for 180 days, paid recall) and,
   for the active bucket, the name of the bucket that will receive the next exports.
2. Tick **I understand the OVH terms** (and, for an attached bucket that was not
   created by VaultCenter, **This bucket is reserved for this VTL.**), then **Archive**.
3. The click checks the whole bucket (complete inventory, no object out of scope, each
   copy consistent with its manifest, no multipart upload in progress), then sends the
   request to OVH. No local cartridge is read or locked, and maintenance is not required.

Recall follows the same pattern: **Recall bucket…**, a checkbox for the recall fees,
**Recall**. The recalled bucket stays readable for 30 days; the window shows the end date.

Automatic archiving: in the VTL window (**Storage → S3**), choose
"N days after a bucket stops being active" and accept the OVH terms. Only
buckets created by VaultCenter, that contain at least one copy and that stopped being
active after the setting was enabled, are archived automatically, with at most one
attempt per day; older buckets are archived from their window. Disabling and then
re-enabling the setting starts again from the re-enabling date. A refusal appears in
Activity and in the notifications, and the attempt is retried the next day.

CLI:

    cold-vtl archive submit --confirm-bucket BUCKET_NAME --acknowledge-billing [--exclusive-bucket]
    cold-vtl recall submit --confirm-bucket BUCKET_NAME --acknowledge-retrieval-cost

API: `POST /v1/vtls/{vtl}/storage/archive?destination_id=…` with `confirm_bucket`,
`acknowledge_billing: true` and, for an attached bucket, `exclusive_bucket: true`;
`POST /v1/vtls/{vtl}/storage/recall?destination_id=…` with `confirm_bucket` and
`acknowledge_retrieval_cost: true`.

The CLI reaches only the active destination of the VTL: it does not send a
`destination_id`. An older bucket is archived or recalled from its window in the
console, or through the API with its `destination_id`. `cold-vtl archive status` and
`cold-vtl archive refresh` read and reconcile the log; `cold-vtl recall status` and
`cold-vtl recall refresh` do the same for recall.

## Active bucket and next bucket

Archiving the active bucket first creates the next bucket (`vc-<VTL name>-<YYYY-MM-DD>`,
then `-2`, `-3`… if that name is already known),
activates it for the next exports, then sends the archive request. The confirmation
shows this name and the console sends it back unchanged in the request. If its creation
is not confirmed, the active bucket is kept and VaultCenter queries the provider; no
archive request is sent. A name already taken or a bucket found with data is never
adopted: running the archive again proposes a new name.

An empty bucket is refused: archiving it protects no copy and would trigger OVH's
minimum billing.

## Recall

Recall is available for an `Archived` bucket. It reads the OVH state, checks the
`cold-vtl` configuration and inventories the entire bucket without downloading the
segments, then sends a single `OVH_RESTORE` request. The service polls the pending
requests every 30 seconds, even with the console closed: `Restoring`, then `Restored`.

After recall, OVH provides read-only access for 30 days; the bucket window
shows the end date. During this period, copies are reintegrated from
**S3 storage → S3 copies**; deleting them and exporting to this bucket are refused.
An unknown state allows neither export nor restore.

OVH bills a minimum of 1 TiB per bucket, with deletion fees before
180 days and recall fees. See the [OVH FAQ](https://docs.ovhcloud.com/fr/guides/storage-and-backup/object-storage/cold-archive-faq)
and the [Cold Archive v1 lifecycle](https://docs.ovhcloud.com/fr/guides/storage-and-backup/object-storage/cold-archive-getting-started).

## Checks and limits

- Exact HTTPS endpoint `s3.rbx-archive.io.cloud.ovh.net`, state `None`, no lock
  reported, no incomplete multipart upload, complete and non-empty inventory.
- Historical versions, versioned objects and delete markers are refused.
- Objects outside the prefix, unrecognised or orphaned are refused. Each copy must be
  consistent with its manifest. A v3 copy is not downloaded again: S3 confirmed its
  native checksums at export, and one HEAD per segment checks that the recorded size and
  checksum have not changed. Only v1/v2 copies, which have no native checksum, are read
  and their SHA-256 recomputed. This check does not detect silent corruption
  occurring at OVH after upload.
- The local cartridge files are neither read nor compared: archiving covers
  the copies present in the bucket.
- External S3 access cannot be locked. For an attached bucket,
  the administrator confirms that it is reserved for this VTL.
- Asynchronous checks, maximum worker duration of 24 hours. Only v1/v2 copies
  are read again: they alone can lengthen the check and incur
  read fees.
- Authentication, the administrator role and CSRF protection remain mandatory.
  Viewers see the state, without mutation buttons.

## Log and interruption

The SQLite migration `0005_archive.sql` adds `archive_requests`. An archive or recall
request in progress on the active bucket blocks the local operations that depend on the
active destination; archiving an older bucket blocks no local cartridge.

Before the upload command is launched, the log durably switches to
`submitting`. SDK retries are disabled for the tiering PUT. A lost response
or a stop at this boundary leaves an `uncertain` result. No new
submission is automatic, even if OVH still reports `None`.

**Check request with OVH** performs only a cloud read:

- `queued` without a surviving helper becomes `failed`: no submission could start;
- `submitting`/`uncertain` becomes `accepted` if `Archiving` is observed, or
  `archived` if `Archived` is observed;
- otherwise the uncertainty remains blocking. An administrator must examine the log
  and the state on the OVH side; do not delete the row or repeat the PUT blindly.

After a daemon restart, an old job may be marked as failed. The persistent
log is always authoritative and keeps the necessary blocks.

## Routes

- `GET /v1/vtls/{vtl}/storage/archive?destination_id=…`: configuration and local log,
  read-only.
- `POST /v1/vtls/{vtl}/storage/archive?destination_id=…`: check, then request
  (`confirm_bucket`, `acknowledge_billing`, `exclusive_bucket`, `next_bucket`). No WORM
  field is accepted.
- `POST /v1/vtls/{vtl}/storage/recall?destination_id=…`: check, then recall
  request (`confirm_bucket`, `acknowledge_retrieval_cost`).
- `POST /v1/vtls/{vtl}/storage/archive/refresh`: job that reconciles the log with OVH.

Install `deploy/cold-vtl/archive-controller.py` as
`/usr/local/libexec/cold-vtl-archive` (root, 0755), next to the updated maintenance
and reintegration helpers. Install `archive.conf` in the systemd drop-ins
`cold-vtld.service.d`, then `daemon-reload` and restart the daemon.
The existing root configuration `maintenance.json` provides the spool and
database paths; the worker is `/usr/local/bin/cold-vtl-worker` by default.

Update: install `cold-vtld`, `cold-vtl-worker`, the archive controller and the
console together. Versions must not be mixed: the new worker refuses
`--media-root`, an old controller calls the removed `prepare` step and an old
console calls removed routes. After the update, if an archive started with
the old version left the VTL in maintenance, leave it:

    cold-vtl maintenance status
    cold-vtl maintenance leave

## Copy history

**S3 storage → History** lists, for the selected VTL, each copy of a
cartridge: bucket, export date, bucket state with its date (archived on, recalled
and readable until), removal from the catalogue ("Deleted from S3" or "Removed from
the catalogue") and local deletion of the cartridge. The `copy_register` register
(migration 0025) is never purged: a deleted copy stays visible with its date.

The state shown is the last state known to the catalogue, without calling OVH; the
live state remains in the bucket window. `GET /v1/vtls/{vtl}/storage/history`
returns these rows as JSON, and `?format=csv` a UTF-8 register (BOM, `;` separator,
ISO 8601 UTC dates) named `historique-<vtl>-<date>.csv`. The archive date of a
bucket archived before this version is its last observation.
