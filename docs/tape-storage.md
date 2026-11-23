# Tape storage per VTL

In **Tapes and drives**, select the library, then open **Configure storage**.
Enter an absolute path on the VaultCenter VM that exists and is empty, then
**Check path** and **Save**. Each VTL has its own path.
The console stays accessible; this operation does not recreate the container.

The controller accepts local ext4/XFS volumes identified by UUID. Mount the
volume on the host before choosing its directory. Identical and nested paths,
and known aliases of the same subtree, are refused between VTLs. NFS/SMB and
adopting existing tapes are not supported.

## Creating several cartridges

Enter maintenance, then choose **Tapes and drives → Create cartridges**.
Enter the number (from 1 to 64, within the limit of free slots), the type and
the capacity of each cartridge. The S3 transfer settings are shared by the
batch. The server assigns barcodes following the series of this VTL, or
starts with `CVT001L8` for a new LTO-8 library. The logs keep the assigned
numbers, even after a cartridge is deleted.

The form shows the total capacity requested and the space available for new
cartridges. The server checks the space again under lock during allocation:
physical free space, minus the capacity still to be covered for the cartridges
and batches on the same file system, across all VTLs. A margin of 16 MiB per
cartridge covers the metadata. Leftover files do not reduce the reserved
capacity. This check does not preallocate the files and does not replace
monitoring of the volume: other applications may consume space after the
check.

The batch reservations and its progress are persistent. After the window is
closed or interrupted, reopen it and choose **Resume batch**; the settings and
identifiers stay the same. Resuming the services is blocked until the batch is
finished. If the disk filled up between two steps, free some space before
resuming. A pre-existing library whose volume is inaccessible and not yet
identified prevents the budget from being confirmed: restore that volume
before a new allocation.

Once the batch is finished, leave maintenance to present the cartridges to
PBS. The older single-creation requests remain compatible.

## Permanent lock

From the first cartridge, durable creation attempt or presence of files, the
path can no longer be changed. The lock is kept after archiving, deletion,
unmounting and restart. The dialog shows:

> Path locked: this VTL contains or previously contained data. Moving it requires a dedicated migration.

**Check storage** remains available. The server also refuses changes: no API
parameter can force the unlock. Do not delete the `data-locked` markers. An
outage or a temporarily empty directory is not proof that the tapes are gone.

## Isolation and first activation

The internal paths are `/var/lib/vaultcenter-media/<UUID>`. Restores,
manifests, logs, mhVTL processes, iSCSI targets and PBS leases are separated
per VTL. A maintenance of A does not restart B or the shared iSCSI service.
The physical resources of the appliance remain shared: this isolation does not
protect against a failure of the VM, the kernel or a shared disk. The capacity
shown is that of the volume; it is not reserved for the library.

**Ready** storage does not mean that the library is activated. The operator
first installs `/etc/cold-vtl/vtl-runtime.json` from
[the example](../deploy/cold-vtl/vtl-runtime.json.example), with a hardware
pool without collisions and a PBS association by UUID. These files and the
SSH keys are reserved for root.

Two optional settings in the same file set what PBS and Veeam see of a
**new** VTL:

- `iqn_prefix` (default `iqn.2026-10.io.vaultcenter`): iSCSI name
  `<prefix>:vtl-<UUID>`, of the form `iqn.YYYY-MM.<reversed domain>` in
  lowercase letters, digits, dots and hyphens;
- `scsi_product` (default `VAULTCENTER`): SCSI product of the PBS changer,
  1 to 16 characters among letters, digits, `_`, `.` and `-`.

They are read during preparation and written into the VTL manifest. A VTL that
is already prepared keeps its iSCSI name, its SCSI product and its serial
numbers, even if these settings change. An invalid value makes the preparation
fail, never the startup.

The PBS guard uses
[one instance per VTL](../deploy/pbs/cold-vtl-guard@.service) and
[its configuration](../deploy/pbs/maintenance-guard-vtl.json.example).

In **Virtual libraries → Details**, prepare then apply the configuration.
**Enable PBS access** publishes the devices under a bootstrap lease. If PBS
does not know them yet, connect only the new target and register the
changer/drive with the planned names and paths. **Resume activation** checks
the real identities before releasing the lease. The bootstrap is permanently
closed after this check. Do not replace these steps with a manual start of
tgt or by deleting markers.

Activation requires the isolation receipt (`isolation-qualified.json`) that
matches the controller version; each version ships its own, and `install.sh`
installs it. The `acceptance_only` test mode also requires an ephemeral marker in `/run`;
it must never be shipped to production.

## Installing and migrating

For a new appliance only:

```sh
sudo python3 deploy/host/install.py --new-appliance --isolated --compose /opt/vaultcenter/compose.yaml
# Install the root runtime configuration and create the administrator.
docker compose -f /etc/vaultcenter-host/compose.json up -d
```

The `vaultcenter-storage-mounts` service prepares the shared root before
Docker; the container receives it as `rslave`. Each empty slot is a read-only
tmpfs. The `vaultcenter-storage-host` service publishes only the selected
submount and checks that it is visible in the container.

An existing appliance that is **unused and not configured** can be converted,
after stopping it, backing it up and preparing a qualified image:

```sh
python3 deploy/host/migrate_vtl_storage.py --prepare /root/vaultcenter-migration --image sha256:<digest>
python3 deploy/host/migrate_vtl_storage.py --apply /root/vaultcenter-migration/manifest.json
```

`--recover` resumes the same manifest after an interruption. The conversion
refuses any cartridge, data, source already configured or existing lock; it
does not migrate a historical installation that is in use. That requires a
dedicated procedure, with backup and PBS leases. The historical mode remains
supported as long as the isolated mode is not explicitly enabled.

## Recovery and backups

Look at `/var/lib/vaultcenter-host/vtls/<UUID>/storage.json` and the log of the
host service. After fixing the cause, recover only the VTL concerned:

```sh
python3 /usr/local/libexec/vaultcenter-host/storage_controller.py --recover --vtl <UUID>
```

The operational lock of this VTL serialises the recovery; B stays available.
A response lost during saving is replayed with its initial identifier.
Do not change the settings of an operation in progress.

A missing or replaced volume never implicitly becomes a folder of the system
disk. The storage is declared unavailable, new operations are refused and the
fallback medium stays read-only after the bind is removed.
A physical failure can interrupt the write in progress; keep the logs and
check PBS before putting the affected VTL back into service.

Back up the media, the catalogue and the configurations together, in
particular `/etc/vaultcenter-host`, `/var/lib/vaultcenter-host`,
`/etc/cold-vtl` and `/var/lib/cold-vtl`. The persistent PBS leases are part of
the recovery procedure. Do not delete the maintenance or service hold markers.
