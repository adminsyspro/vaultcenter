# Local cartridge deletion

In **Cartridge library**, select the media, then **Delete (N)**.
This action is restricted to administrators and accepts up to 64 cartridges.
The dialog lists the barcodes and the exports recorded in the catalogue,
with their bucket and version, including destinations that have become inactive.
It does not check that they are actually present on S3.

Before confirming, finish the jobs, unload the drives in PBS and enter
maintenance through the **Maintenance** button next to **Power** in
**Tapes and drives**. Services are resumed in this same window. The "Idle" indication is not enough. Then confirm
the permanent erasure of the local files. The slots and disk space
are freed; the S3 objects, the restore references and the job
history are kept. No S3 delete call is made.

The PBS catalogue is not modified: cleaning it up, if needed, remains an
administrator action in PBS. The source backups in the PBS datastore are intact.

The old references remain accessible in the **Deleted
locally** filter, through **Details**, then **S3 copies and restore**. A restore
produces a new, separate folder; it does not automatically recreate a
slot or a deleted cartridge. The old barcodes remain reserved. S3 synchronisation remains available:
it then checks the remote copies without comparing local files.

## Resuming after an interruption

The request has a persistent identifier and selection. A durable log
keeps the identities of the volume and the files. The controller moves the media
on the same volume into a private area, removes their entries from the inventories
(static and online additions), records the local deletions, then erases
the files. It does not delete the cartridge rows used by the foreign keys
of exports and jobs.

After an error or if the browser is closed, reopen the library and use
**Resume deletion**. The request resumes with its original selection. The
services stay blocked until the operation is finished. A volume,
file or inventory modified during the interruption requires manual intervention;
the controller refuses to erase a different target.

## API

The routes are available under `/v1/vtls/{id}/cartridges` (and under the legacy
route when only one VTL is configured):

- `GET /deletion`: availability, blocking reason and interrupted request;
- `POST /deletion`, with `{"barcodes":["CVT001L8"]}`: preview of the S3 references;
- `POST /delete-local`, with `request_id` (32 lowercase hexadecimal characters),
  `barcodes` and `confirmation: "delete-local"`: execution or exact resumption.

POST requests require an administrator and the usual CSRF protection.
The `completed` result confirms the whole request. On a timeout,
reuse the same identifier and the same selection; do not invent a new batch.
