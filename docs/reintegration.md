# Reintegrating a restore into PBS

The CLI can install a cartridge restored from S3 into mhVTL, keep its previous
version, then resume the services and check that it is present in the PBS
changer. The operation requires an administrator.

## State of the workflow in the console

The cartridge window shows the activity, the export history and the
**Send to S3** action; it has no restore or reintegration tab. Reintegration
is started with the CLI commands below and requires a local cartridge.
Recovery of an interrupted reintegration is available in Maintenance.

The PBS drive must be empty and all PBS/VTL tasks finished. The guard checks
these conditions before stopping the services. The reintegration opens and
closes its maintenance automatically; it can also use a window that is
already open and whose four services were initially active.

Restores older than this release have no SHA-256 receipt recorded per file:
run them again into a new folder before reintegrating them. Restore folders
are kept after installation.

## CLI

With an administrator session, and the CLI configured to trust the certificate
of the appliance:

```bash
cold-vtl media restore CVT001L8 --generation 1
cold-vtl jobs watch <restore-job>
cold-vtl media reintegrate CVT001L8 --restore-job <restore-job>
cold-vtl jobs watch <reintegration-job>
cold-vtl media reintegrations

# Recover from an interruption or cancel the last reintegration of this medium:
cold-vtl media rollback <reintegration-job>
cold-vtl jobs watch <rollback-job>
```

## Retention and recovery

- The worker records in the restore job the identity of the medium, the
  generation, the SHA-256 of the manifest and the size/fingerprint of each file.
- The adapter reads the finished job back from SQLite. It refuses a different
  identity, a changed manifest, an incomplete folder, symbolic/hard links and
  special files. This version accepts flat-file mhVTL cartridges containing
  `mam`, `data.0`, `indx.0`, `meta.0` and `mhvtl_data`.
- It checks all files before and after copying. The copy is prepared on the
  same file system as the spool; files and directories are synced to disk
  before the switchover.
- The previous cartridge is moved to
  `/opt/mhvtl/.cold-vtl-reintegration/<id>/previous`, then the prepared copy
  replaces the active folder. Neither version is overwritten.
- The catalogue is updated before the services resume. The PBS lease is
  released only after checking that the drive is empty and that the cartridge
  is present exactly once in PBS.
- The log `/var/lib/cold-vtl/reintegrations/<id>.json` and the marker
  `/var/lib/cold-vtl/reintegration-pending.json` make it possible to find an
  interruption, including between the two renames or before the SQL update.
  The same flock serialises maintenance, reintegration and rollback.

After a failed job, open **Maintenance → PBS reintegration** or the S3 copies
of the cartridge and use **Return to the previous cartridge**. This rollback
checks the backup, restores the files and the catalogue, then resumes the
services. The cartridge replaced during the rollback remains in
`replaced-on-rollback`. An interrupted rollback can be run again.

An unresolved reintegration blocks the ordinary commands to enter/leave
maintenance and the operations on the spool. Do not delete its markers to
force the services to resume. If the PBS lease was lost during a switchover,
the rollback refuses to change the files until the lock has been recovered by
the administrator. See [maintenance.md](maintenance.md).

Only the last non-cancelled reintegration of a medium can be cancelled
automatically. Older backups and intermediate copies are kept, with no
automatic purge. The log remains the reference if a daemon restart marks a
job as failed after a switchover that had already completed.

## API

| Method | Path | Effect |
| --- | --- | --- |
| GET | `/v1/reintegrations` | Configuration, operation to recover and the last 30 logs |
| POST | `/v1/cartridges/:barcode/reintegrate` | Asynchronous job; body `{"restore_job_id":"…"}` |
| POST | `/v1/reintegrations/:id/rollback` | Asynchronous job to return to the previous version |

Mutations require the administrator role and CSRF for browser sessions.
Viewers can read the logs. No arbitrary path supplied by the browser is used
to move the media.
