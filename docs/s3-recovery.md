# Monthly buckets and cartridge recovery

In **S3 storage → Configuration**, save the connection (endpoint, region,
prefix, access key and secret). It can exist before any bucket.
The connection belongs to the selected library. One destination receives
new exports; historical destinations remain available for consultation.
Jobs already started keep their destination and their credentials revision.
Saving a connection does not replace the credentials of existing
destinations: attaching a bucket again with the updated connection adds a
credentials revision, without changing jobs already started. The settings
also remain available in the advanced configuration.

## Changing month

In **Copies and buckets → Manage buckets**, create the next bucket.
The application suggests an editable name, then creates and activates the
destination only after the provider confirms it. Built-in creation applies to
OVH Cold Archive v1; a generic S3 connection keeps manual attachment.
The old bucket stays in the table. Archiving it is a separate action,
available in **Bucket status and archiving** with the fee confirmations.
Archiving a historical destination does not create a third bucket.

An interrupted creation keeps an uncertain result and leaves the active
destination unchanged. After checking with the provider, the administrator can
abandon the attempt in the interface. This resolution does not delete, attach
or activate any remote bucket. A bucket that already exists never
automatically becomes the property of this attempt.

## Finding an archive after reinstalling

1. Configure the connection in an installed library.
2. Open **Manage buckets**, discover then attach the old buckets.
   The name can also be entered manually if discovery is refused.
3. Look at the table, search for the barcode or filter on the bucket.
4. If needed, recall the entire bucket and wait for read access.
5. Select **Restore to library**, choose the target library and check
   the space, the slot and any conflicts.

The inventory comes from the S3 objects and the historical manifests, without
depending on the cartridges in the local catalogue. Pagination or manifest
errors are visible as partial results. A manifest that was read is not proof
that all the segments are intact: they are checked during the download.
The selection freezes the bucket, the generation and the exact SHA-256 of the
manifest.

The barcode alone does not identify the content. If a local cartridge has the
same name, choose another library or confirm the replacement separately. The
replacement keeps the previous cartridge, with SHA-256 verification.
No implicit renaming is performed.

## Installation and resume

The download keeps PBS available. The installation then reserves the drive
through managed maintenance, checks the files, installs the cartridge, updates
its slot and checks PBS before resuming the services.
PBS tasks must be finished and the drive empty for this phase.
Closing the window does not end the job.

After an API restart, a verified copy resumes at the installation step; an
interrupted transfer starts again in a new folder, keeping the incomplete
folder. An interrupted installation is inspected through its log and, if
needed, resolved by a rollback under maintenance. The resume never guesses
that partial local changes succeeded. Safety copies and incomplete folders
are not deleted automatically.

## Source library API

All routes have the prefix `/v1/vtls/:vtl_id/storage`.

| Method | Path | Function |
| --- | --- | --- |
| GET / PUT | `/connection` | Configuration with no required bucket |
| GET | `/buckets/discover` | Remote discovery and creations in progress or to be checked |
| POST | `/buckets/attach` | Historical attachment, or activation of an existing bucket as destination |
| POST | `/buckets/create` | Durable creation then activation |
| GET / PUT | `/schedule` | Scheduled bucket change and automatic archiving |
| GET | `/recovery?destination_id=…` | Remote inventory independent of the local catalogue |
| POST | `/recovery/check` | Check of the copy and of the target library |
| POST | `/recovery` | Download, installation and PBS check |

Mutations and connection settings require an administrator.
Browser sessions must provide the CSRF token. No secret is returned.

### Space for download and installation

The pre-check verifies the temporary download folder and the storage of the
target library separately. The manifest volume is not enough: also allow for a
16 MiB reserve, and two copies if the download and the new installation use
the same file system. The previous cartridge stays in place until the
switchover, and keeping it does not free any space.

In production, mount a folder of the data disk **separate from the tape
folders** on `/var/lib/cold-vtl/restores` (subfolders isolated by VTL UUID).
Add this bind mount to the persistent Compose configuration, with
`create_host_path: false`, and exclude its source from the paths that can be
configured as tape storage in `storage.json` (`forbidden`). Do not use a
symbolic link and do not change the signed paths of the VTL hardware manifests.
Adding the mount for the first time requires a controlled recreation of the
container under PBS maintenance, after jobs have been confirmed stopped; keep
the old restore folders until their migration has been verified.

## Deleting a copy and cold access

The S3 table distinguishes buckets that are writable, archived, being
archived, being recalled and recalled read-only. Transitional states are
observed again every 30 seconds. OVH v1 recall applies to the entire bucket;
it keeps its fee confirmation and does not make the bucket writable.

`POST /v1/vtls/:vtl_id/storage/copies/delete` is restricted to administrators.
The confirmation contains the destination and its revision, the bucket, the
retyped barcode, the generation and the fingerprint of the manifest. The
server checks access and the manifest again before reserving an `s3_delete`
job. No switch to PBS maintenance and no access to local files is required.
Concurrent VTL operations prevent the deletion; the reservation remains
exclusive while it runs.

Only the validated segments of this generation are deleted, then the manifest
last. An error leaves the manifest in place to allow a new attempt and appears
in the table as an interrupted deletion. No recursive deletion of a prefix or
bucket is performed. Versioned buckets, or buckets whose rights/versioning
cannot be checked, are refused.

The catalogue assurances are invalidated before the first deletion, even if
the process is interrupted. For the active destination, a reconciliation will
be needed before a new export, and the targeted local generation is advanced
to avoid reusing an earlier upload log. Local data and the other S3
generations are kept. A success is recorded only after confirming that each
deleted object is gone.
