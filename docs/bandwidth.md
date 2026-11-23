# VaultCenter bandwidth limits

In **Virtual libraries → Details → Bandwidth**, administrators set two independent caps for each library:

- **PBS → tapes**: drive writes. Restore reads are not limited. This version supports one drive per VTL for this cap.
- **S3**: budget shared between all uploads, downloads and checks of this VTL, including concurrent transfers and retries.

Values are integers in **MB/s** (1,000,000 bytes/s), from 1 to 100,000. Cartridge charts show **MiB/s** (1,048,576 bytes/s): 100 MB/s is about 95.37 MiB/s. Unticking a limit restores unrestricted throughput. Both limits are disabled by default.

Saving applies to transfers in progress, without iSCSI reconnection or restart. The processes re-read the settings about every second; a command already sent and the network buffers can cause a short variation. A cap is measured over a stabilised period, not on an instantaneous peak. The messages distinguish the saved setting from its confirmation by a running drive; a stopped drive will pick up the setting at its next start.

The configuration is persistent and isolated in `/var/lib/cold-vtl/vtls/<UUID>/bandwidth.json`. The `cold-vtl` paths and services keep this name, which is the component's name. Do not edit the files manually: the revision protects concurrent changes and the drive's file is a projection of this configuration. A read error keeps the last valid limit in the active processes; a new process refuses a missing or invalid configuration. The interface reports an unconfirmed or degraded state, never an implicit deactivation.

The administrator API uses `GET` and `PUT /v1/vtls/<UUID>/bandwidth`. The PUT requires a session and its CSRF token, with all fields:

```json
{"revision": 1, "tape_write_bps": 50000000, "s3_bps": 100000000}
```

`null` means no limit. On a revision conflict, read the configuration again before retrying. S3 status checks and metadata are not limited; the budget applies to the transferred content, not to the network headers. The physical throughput can therefore be slightly above the content cap.

Deployment requires the complete image (API, worker, controller and instrumented mhVTL). An older image does not confirm the drive limits. Update between two jobs, under maintenance, then check the confirmation states before resuming backups.

Independent PBS limits also require the host module with the [mhVTL parallel queue](docker.md). The adapter queue must accept 128 commands; each drive keeps a single command in flight. Without this prerequisite, saving a PBS limit is refused, to prevent one VTL from slowing down all the others.

## Expected behaviour

A cap is an upper bound, measured on the transferred content. With 256 KiB SCSI writes sustained for at least 30 seconds, caps of 1, 50 and 100 MB/s gave about 0.995, 43.8 and 78.8 MB/s, while another VTL without a limit on the same host kept writing independently. These figures exclude filemarks and rewinds; they are not a complete PBS backup throughput. Two concurrent S3 workers share the S3 cap: under a 1 MB/s cap, they transferred 0.996 MB/s in total. Changing a limit while transfers are running does not restart any service.
