# VaultCenter library for Veeam Backup & Replication

Each virtual library (VTL) has a **backup client type**: PBS (the default) or Veeam. A Veeam VTL exposes an HP “MSL G3 Series” changer (revision 9.50) and an HPE “Ultrium 8-SCSI” drive, with LTO7/LTO8 cartridges. It has **only one drive**.

## Operator procedure

1. **Storage**: Manage VTLs → Configure storage, “Local storage” tab (the S3 tab is optional). If the last folder of the path does not exist but its parent does, it is created during the check.
2. **Backup client**: VTL details → “Backup client”. For Veeam, enter the IP address of the Veeam server (tape server), then “Save”. This address is frozen once the library is prepared. For PBS, the field is read-only: it comes from the appliance configuration.
3. **iSCSI connection**: enter the iSCSI address of VaultCenter reachable from the Veeam server (for example `192.168.1.10`) and the port (3260 by default). The Veeam PowerShell commands appear only **after** “Apply preparation” (step 4), once the manifest is configured: before that, the console only says that the library must be prepared. They connect the target, then check the HP MSL G3 changer and the HPE Ultrium 8 drive.
4. **Throughput limits** if needed, then **Activate library**: a single button chains the hardware reservation, the configuration and the iSCSI publication. If it is interrupted, the same button becomes “Resume activation”.
5. **Windows**: run the two PowerShell commands shown, in order. Install the HPE StoreEver driver for the drive if it does not have one.
6. **Veeam**: Tape Infrastructure → Libraries → Rescan, then Inventory. If the changer remains “Unknown Medium Changer”, enable “Use native SCSI commands instead of Windows driver” on the library.
7. **Cartridges**: created in VaultCenter (LTO7 or LTO8), then inventoried by Veeam.

## Backup client API

- `GET /v1/vtls/<id>/client`: returns `{revision, client, initiator, editable, message}`.
- `PUT /v1/vtls/<id>/client` with `{revision, client:"veeam", initiator}`: saves the Veeam client and the IP address of the Veeam server. `revision` is the revision read by the `GET` (concurrency control).
- Once the library is prepared, the association is frozen (`editable: false`) and the `PUT` is refused. PBS associations remain managed by the appliance configuration.

## Maintenance

VaultCenter cannot know whether Veeam is using the library. For a Veeam VTL, the Maintenance dialog **and** the “Stop the library” button require you to tick “No Veeam job is using this library”. This confirmation is the operator's responsibility.

A reintegration on a Veeam VTL requires maintenance to be already active.

## Limits

- A single drive; HP MSL G3 / HPE Ultrium 8 identity; LTO7/LTO8 cartridges.
- No automatic check of Veeam activity (manual confirmation).
- Veeam reintegration possible only from a maintenance that is already active.
- PBS VTLs are not changed by this client type.
