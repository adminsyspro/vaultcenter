# Cartridge creation

In **Tapes and drives**, an administrator can open **Create cartridges**
to add a batch of 1 to 64 media. Barcodes are assigned automatically.
The LTO type, the virtual capacity, the S3 segment size and the multipart
part size are common to the batch.

The type is chosen among those the controller confirms as compatible with the
installed drives. The capacity is expressed in MiB, GiB or TiB, in whole
multiples of 1 MiB, from 1 MiB to 16 TiB. S3 segments range from 16 MiB to 1 TiB;
the initial value for new cartridges is 1 TiB. Each file is split
separately: the last segment can be smaller, and small metadata files
remain separate objects. This setting does not change the PBS blocks.

Multipart parts range from 16 to 1024 MiB, with 256 MiB by default. A full
1 TiB segment is thus sent in 4096 parts, assembled into **a single S3 object**.
The chosen combination must stay under the limit of 10,000 parts per segment.
The details of the transfer and its resumption are in [S3 exports](s3-exports.md).

## Actual creation

The library must be enabled and have the creation controller.
When a compatible engine confirms `online_creation: true`, the form
shows **Online creation available**. Creation can then take place during
a PBS write: no drive is unloaded, no service is restarted
and no iSCSI session is interrupted by this operation. Incompatible
VaultCenter operations (maintenance, restore or active local export)
remain blocking.

The daemon chooses a storage slot that is actually free, excluding
the return slot of the loaded drives. An unknown or ambiguous origin
blocks the allocation. The magazine limits and the disk capacity reservation
also apply to online creation. No additional slot is
created automatically.

With an older engine, the workflow remains that of managed maintenance: wait
for work to finish, enter maintenance, create the batch, then leave maintenance.
The first installation of the compatible engine is done in a window with no
active backup; it is not a live replacement of the executables.

Creation never replaces an existing cartridge. After a lost response,
the form offers "Resume creation" with the saved parameters,
even if the last slot has already been taken or the catalogue updated.
A strictly identical request resumes the operation thanks to the controller's log. An incomplete creation stays blocked for inspection; it is
not presented as successful and does not trigger an automatic PBS resumption.
Do not delete the logs or the media files to get around this block.

The chosen parameters are persisted per cartridge. Each export passes the
two recorded sizes to the worker. Older cartridges keep their
capacity and segment size; the migration adds a part size of
256 MiB. Their type remains unknown until it is confirmed, without deducing it
from the barcode alone.

## API

`GET /v1/vtls/:id/cartridges/options` returns the compatible types, the limits,
the default values, the free slots, `online_creation` and the
`ready` availability state. A missing flag is equivalent to false for older
controllers. The POST checks the conditions again; the options reserve nothing.

`POST /v1/vtls/:id/cartridges` requires an administrator session and the CSRF token:

```json
{
  "barcode": "NEW001L8",
  "media_type": "LTO8",
  "capacity_bytes": 2147483648,
  "segment_size_mib": 1048576,
  "multipart_part_size_mib": 256
}
```

The 201 response contains the cartridge as confirmed in the catalogue. The conditions are
checked again at creation: the result of `options` does not reserve a
slot. Unprovisioned VTLs cannot use the devices of
the installed library.

## Local demonstration

`npm run dev:demo` in `ui` simulates creation in memory. No mhVTL media,
PBS access or S3 call is made. The parameters and the new cartridges
are kept until the demonstration server restarts.

## Persistence and online resumption

The engine keeps a log of insertion intents in
`<media_root>/.vaultcenter-media-<library-id>`. It is separate from the mhVTL
inventory and works with `PERSIST: False`. The intent is synced to
disk before the media is published to SCSI traffic. After a hard stop, the
engine replays the insertions missing from its inventory before exposing itself to PBS.
It does not replace a position that is already occupied. The controller also takes this log
into account during a later creation under maintenance.

A lost response is resumed with the same batch: PBS may already have moved or
written the media. Once the insertion has been sent, resumption relies on its operation
identifier and no longer checks that the cartridge is blank. A
persistence error puts online creation in a fault state until the engine is recovered; it
does not unload the tape in use. Do not delete the logs to unblock
a creation. Manual deletions of cartridges outside VaultCenter are
not handled by this insertion log. Its limit is 4096 insertions
per library; the engine refuses new insertions once it is reached.

Going back to an older image requires an offline reconciliation of the
insertions into that version's inventory. Putting back only the old binary
would make it ignore the new log. Keep the image, catalogue,
logs and configuration together for the rollback procedure.

## Using new cartridges in PBS

The presence of cartridges in the library does not guarantee that they are selected
by a PBS pool policy. Cartridges added during a write appear in the changer
without any change to the drive, the library or the iSCSI target. However, new
blank media are not labelled in the PBS inventory, so a job that is already running
cannot switch to them: PBS reports `no usable media found`. Label the new
cartridges explicitly in PBS, then run the job again. Before relying on online
creation, check with your own PBS pool policy that new cartridges are discovered,
used when a job needs another tape, and restorable.
