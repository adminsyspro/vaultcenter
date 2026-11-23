# Tape activity telemetry

This optional collector observes mhVTL and can query PBS's read-only changer inventory.
It never reserves a maintenance lease, reads backup payloads or modifies media. The local demo is simulated; physical observations
require commissioning the opt-in collector described below.

## Opt-in integration

The Docker image includes `vaultcenter-telemetry.service`, skipped unless
`/etc/cold-vtl/telemetry.json` exists. Use `deploy/telemetry/config.example.json`
as a template; replace the UUID with the actual VTL ID and all configured paths.
The configuration must be a root-owned regular file, not writable by group or
others. Cache files are root-only, atomically replaced in `/run/vaultcenter/telemetry`.
The API reads this location through `COLD_VTL_TELEMETRY_DIR`; absent cache is safe.
There is no tape-service dependency on the collector.

An operator must separately enable `fifo:` in mhVTL device.conf, pointing the
library and its drives at the same per-VTL FIFO. Keep the `fifo:` line
inside each device stanza: a blank line terminates that stanza. Create its parent directory
with ownership allowing only the configured mhVTL user and collector access.
The image creates `/run/vaultcenter-mhvtl-status` privately via tmpfiles;
`/run/vaultcenter-mhvtl-status/library-10.fifo` is suitable for the primary library.
Use a distinct FIFO and media root for each VTL. Never share queues across a
FIFO. No FIFO is enabled by the default appliance configuration.

For isolated VTLs, the generated services pass the FIFO as `-f/path/to/status.fifo`
(with no space). The pinned mhVTL parser treats `-f` as an optional-argument
option and silently ignores a separate path argument. Changing this requires
a guarded restart after stopping PBS work and unloading the drive; restarting
the collector alone cannot enable a missing producer stream.

Only enable this with the patched mhVTL producer. Upstream revision
`59f32ee50b3269c118965bffa29cf38b1553268f` uses blocking stdio; the included
`deploy/mhvtl/nonblocking-status-fifo.patch` uses nonblocking, atomic writes.
The additional `telemetry-heartbeat.patch` emits a bounded JSON heartbeat from
the command loop approximately once per second. A full pipe drops observations
instead of delaying tape commands. The image's
source archive includes the patch. Run `deploy/tests/check_mhvtl_fifo.py` with
that upstream source on Linux to reproduce the old stall and check the fix.

`pid_file` must identify the actual vtltape daemon (not a shell/wrapper). The
collector checks `/proc/PID/comm`, process start time and open spool descriptors.
Missing or ambiguous identity leaves the cartridge unknown. Robot messages
express movement intent, not proof of successful position; no slot map is
invented from static library_contents. Positions come from the PBS inventory
when the VTL has a PBS binding, otherwise from the library's own slot map
(`deploy/mhvtl/slot-map.patch`): `vtllibrary` writes its storage and
import/export slots to the status FIFO on every change and at least every five
seconds, as JSON records below `PIPE_BUF` split into parts sharing a sequence.
The collector accepts them only from the library PID in `pid-<library queue>`
next to the FIFO, applies a map once all parts arrived, and numbers storage
slots first, then the import/export cart, as a changer does. No SCSI command is
sent to the changer, so a backup application's unit attentions are never
consumed. Without either source, the UI displays an unpositioned inventory. The supplied systemd drop-in publishes `$MAINPID` to
`/run/vaultcenter-mhvtl-<queue>/pid` after daemon startup and removes it at shutdown.
These paths are inside the appliance PID namespace, not the Docker host.
The example sets `require_heartbeat: true`: legacy drive text is ignored because
the pinned upstream label table has shifted indexes. Heartbeats map the enum
explicitly; robot movement messages remain usable as hints.

### PBS slot positions and import/export cart

The collector uses the VTL’s existing `pbs` SSH binding. For a single-VTL
configuration without an explicit binding, it reads the adjacent `runtime.json`,
requiring the same VTL ID and a regular, owned file without group/world write
permissions. This keeps the isolation-qualified runtime generator unchanged.
Update `deploy/pbs/maintenance-guard.py` on PBS first:
the forced command accepts `--vtl UUID inventory` and only queries the changer
bound to that UUID, after checking its configured device path. It runs outside
the maintenance daemon, so a delayed inventory does not block lease handling.
The command uses `proxmox-tape changer status --cache false`; it also includes
ordinary slots reserved for export in PBS (which mhVTL alone cannot identify).

The collector performs one asynchronous query at a time, waiting five seconds
between completions. SSH times out after eight seconds; reader sampling stays
independent. Responses must match the VTL, contain at most 4096 unique slots,
and carry a fresh observation timestamp. Positions expire after 15 seconds;
an unsuccessful query clears the slot observations, rather than showing stale
barcodes as current. Only the configured changer is queried; no media moves.

An explicit `pbs` object in `telemetry.json` remains supported. After updating
the collector, restart only its telemetry unit.
The UI separates `import_export` slots into the cart, including occupied and
empty positions. It reuses the existing SSH credentials; the guard update adds
only the scoped read-only inventory command.

## Meaning and limits

Local cartridge sizes are sampled independently of drive heartbeats and changer
inventory. Fresh local measurements keep the snapshot available when tape
services are stopped, including during maintenance; drive state and slot
positions remain unknown until separately observed. Local samples expire after
ten seconds and do not imply that the tape services are running.

- Heartbeats carry producer PID, millisecond wall time, monotonic time, load
  generation, barcode and a cumulative successful-write counter. The collector
  checks the PID/starttime and open spool identity before accepting a record.
  Load/restart/source boundaries and gaps exceeding five seconds reset the rate
  baseline. A missing heartbeat expires activity after 15 seconds.
- Write throughput is successful uncompressed payload bytes written by mhVTL
  divided by producer monotonic elapsed time. It excludes failed blocks, tape
  metadata, SCSI/iSCSI framing and network overhead. The counter is cumulative
  for the daemon lifetime, not tape occupancy or a PBS job completion total.
  Failed/partial SCSI commands can include blocks that were successfully written.
- The producer reports idle after three seconds without byte-counter movement
  when its last state is reading/writing. A blocked command cannot generate a
  heartbeat; the UI then becomes unknown rather than claiming continued I/O.
- File metadata is sampled each second, at most 1024 directories and 128 regular
  files per cartridge. Symlinks and ambiguous files yield unknown. **Local size**
  is physical spool length, not logical tape occupancy; growth is not PBS network
  throughput. Replacement/truncation resets the rate baseline.
- Logical tape occupancy remains unknown. Catalogue usage is shown separately
  as the last reconciliation report. Legacy text-only producers still have no
  throughput metric and their activity expires without fresh events.
- There is no PBS job completion percentage. Snapshot age and per-observation
  age are checked independently. No observation means unknown, never zero.

The authenticated endpoint `/v1/vtls/:id/telemetry` has a versioned, allowlisted
contract documented by `deploy/telemetry/snapshot-v1.example.json`. It serves a
bounded cache, not a new reader per HTTP request. Invalid/missing source does
not prevent catalogue reads or normal operations.

## Local preview and validation

Run `npm run dev:demo` from `ui`, then visit `http://localhost:3000/#media`.
The scenario selector simulates activity and source loss; these are not real
observations. Its API adapter has no upstream fallback. Cartridge mutations remain
in memory; simulated throughput samples are stored separately on local disk. Ordinary `npm run dev` still uses the existing configured API proxy.

Checks cover FIFO framing/recreation/shutdown, source scope and process identity,
nonblocking Linux producer behavior, bounded authenticated cache access,
visibility/expiry/poll concurrency, cross-VTL response isolation, and retained
media actions. Browser regression APIs are intercepted. On macOS, run the Python
suite with `TMPDIR` resolving to a real directory: the existing reintegration
suite deliberately rejects symlink ancestors such as `/var` → `/private/var`.

To enable the collector, stop tape activity first and follow the host's maintenance
procedure. Confirm that the PID published by the systemd drop-in belongs to `vtltape`. Commission the real FIFO and
identity mapping before relying on the collector as a live monitor.

Legacy text FIFO records retain their producer timestamp. Records older than 15 seconds,
more than 5 seconds in the future, or predating the collector/source/process/
cartridge boundary are rejected. Since ctime has whole-second resolution, a
transition may briefly remain unknown. Producer and collector must use the same
host timezone. A changed cartridge cannot inherit its predecessor’s activity.

## Persistent throughput history

The graph offers **Live, 1 h, 24 h, 7 days and 30 days**. Live is a
rolling two-minute view combining persisted samples with live observations. The
selected scale is remembered per VTL in the browser; the measurements themselves
are stored on the server. The transfer selector and cartridge selector stay in
the same positions for PBS and S3 views. Historical cartridges remain selectable
after unloading, even if the current collector is unavailable.

At API startup, one background task begins sampling validated mhVTL snapshots
every five seconds, independently of HTTP requests and open browser tabs. The
`telemetry_samples` SQLite table in the catalogue retains 30 days by default.
`COLD_VTL_TELEMETRY_RETENTION_DAYS` accepts 1–90 days. Expired rows are removed in
bounded batches; SQLite reuses the freed pages rather than shrinking the database
file on each deletion. Back up the catalogue to preserve these observations.
A missing/disabled collector yields no new samples. The API must be running to
collect; restarting preserves stored history but does not reconstruct the gap.

Each series belongs to its VTL, drive and barcode. Frozen snapshots are not
counted repeatedly. Stale, ambiguous and demo source observations are excluded
from physical history. Explicit `write_rate_bps` is plotted as copy throughput;
otherwise `local_growth_bps` is labelled as physical local growth. Patched producer heartbeats supply the write-rate counter independently of
spool file growth.

The authenticated read-only endpoint is
`GET /v1/vtls/:id/telemetry/history?barcode=CVT001L8&drive_id=11&window_seconds=3600`.
The UI uses windows of 120, 3600, 86400, 604800 or 2592000 seconds. `end_ms` can
anchor a historical window; omitting it uses the current time. Without `barcode`,
the response inventories retained series without returning their points. Queries
are isolated by VTL and do not trigger collection.

`aggregate=true` returns one curve for every tape instead of one cartridge:
each series is averaged per bucket as usual, then the series are summed, for
tape writes (optionally limited to one `drive_id`) as for S3 uploads or
downloads. A bucket no series measured stays null. It cannot be combined with
`barcode` or `transfer_id` (400).

Each response contains at most 600 time buckets, with a minimum width of five
seconds. Values are averages of the observed rates in that bucket. Missing
metrics/buckets remain null, not zero; the chart does not join acquisition gaps.
Live updates every five seconds (with live samples between requests), the
one-hour history every 15 seconds, and longer views every minute. No pre-existing
history is invented: recording starts once the API and a valid collector
are running.

Real S3 transfer-rate acquisition is not implemented; the S3 history endpoint
therefore returns no physical measurements. The demo separately records its
simulated PBS and S3 rates, including distinct S3 transfer IDs. It never substitutes
PBS growth for S3 throughput.

For `npm run dev:demo`, actual simulated observations are written to the
git-ignored `.local/demo-throughput-history.json` file at the repository root,
with its append journal.
Samples are appended and fsynced every five seconds, with hourly compaction;
restart recovery deduplicates records and tolerates an interrupted final append.
No synthetic past is seeded. `VAULTCENTER_DEMO_HISTORY_RETENTION_DAYS` changes the
30-day demo retention. The history survives browser and demo-server restarts;
scenario selection and the mock cartridge inventory still reset on server restart.
The transfer selector makes prior S3 history accessible without starting another
transfer. These files are local demonstration data, not real measurements.

## Producer regression checks

CI fetches the exact pinned mhVTL source, checks the old blocking FIFO regression,
applies both patches, then compiles and executes the real heartbeat formatter.
`deploy/tests/check_mhvtl_telemetry.py PATH_TO_PATCHED_MHVTL` checks atomic framing,
throttling, idle transitions and a saturated pipe. The Python suite checks
sustained activity, wrong PID/barcode, stale and reordered records, counter reset,
load generation and process/source boundaries. The image build also compiles the
full patched daemon. CI does not exercise real tape hardware: check the
collector against your own PBS installation before relying on it.

## S3 export history

New API-launched exports record acknowledged payload upload bytes in the catalog's
`s3_transfer_samples` table, scoped by VTL, barcode and export job (`transfer_id`).
The worker measures the interval with a monotonic clock and samples every five
seconds, with a final partial sample and unknown start/end boundaries. Concurrent
multipart receipts are summed. These are acknowledged-byte rates: a long part can
show zero before its acknowledgement and a higher rate when it completes. They
are not instantaneous wire rates. Retries without a successful receipt, already
confirmed parts and recovered objects are not counted as newly uploaded bytes.
Hashing and verification periods leave gaps. A stopped or killed worker cannot
extend its last sample into the future.

Recording runs in the worker without a browser and writes directly to durable
SQLite storage. Telemetry errors disable recording without changing export,
verification or reconciliation outcomes. Samples use the same configurable
retention as PBS history, with bounded cleanup batches and at most 600 points per
history response, with rates weighted by each sample’s measured duration. The latest recorded export is selected when no transfer ID is
specified; samples from separate exports are never combined. Older operations
have no measured curve and are not retrospectively estimated from job progress.

Deploy the API (migration `0016`) before the new worker, and update the isolated
`worker-run.py` wrapper to forward `COLD_VTL_EXPORT_JOB_ID` through systemd. The API uses an
optional environment variable, so an older worker continues exporting but records
no history. Standalone worker commands without this API job identity do not write
history. No credentials, object names, or backup contents enter the sample table.

## Cartridge activity dialog

Opening a cartridge selects **Activity** before Restore and Synchronization.
The graph stays scoped to that cartridge and switches between PBS writes and S3
exports. It initially shows 24 hours, with previous/next windows, an end date,
and a shortcut to the latest measurements; existing longer scales remain
available within retention. Missing measurements remain gaps. The S3 operation
list uses the barcode-filtered jobs endpoint (latest 100 matching exports), so
unrelated recent jobs cannot hide this cartridge’s older exports. Operations
without measured samples remain listed with an explicit missing-curve message.

## Export progress

The worker publishes `message.export_progress` on its running export job every
five seconds: `phase` (`preparing`, `uploading`, `verifying`, `finalizing`),
`completed_bytes` and nullable `total_bytes`. The total is the prepared manifest's
payload size. Confirmed S3 writes and validated reusable parts advance payload
progress; reuse adds no new throughput samples. Upload completion does not mean
verification succeeded. Only the API can finish the job after all checks.

The cartridge cloud and operation dialog calculate the upload percentage from
these counters. Older workers' fixed job milestone is not a measured percentage;
the UI shows an indeterminate cloud until a terminal result. The compact dialog
keeps phase, payload progress and errors visible, with identifiers, versions and
timestamps under technical details. A worker executable can be replaced atomically
without restarting the API or tape services; already-running workers keep their
existing code and do not gain measured progress retroactively.

### S3 download feedback

`kind=s3_restore` selects recovery/restore throughput in the history endpoint; `kind=s3`
continues to select exports. Both use the bounded `s3_transfer_samples` history, with
job kind and VTL identity checked when reading. A transfer ID from the opposite
direction cannot expose or blend samples. Download samples count bytes written by
the worker to the staging directory, not network headers or reintegration copies.

The restore worker publishes `restore_progress` every five seconds: `preparing`,
`downloading`, then `verifying`, with measured completed/total payload bytes. The
recovery orchestrator publishes `installing` after receipt verification. Download
100% does not mean installation has finished; only the terminal job confirms that.
Older recoveries have no reconstructed curve or fabricated byte percentage.

Activity and cartridge details show these stages. The library LCD pins active jobs,
updates byte counters without adding a journal line for every sample, and records
stage changes, successes and failures independently of drive telemetry. It retains
40 session observations and shows loss of API connectivity explicitly.


### Export integrity receipt

A successful v3 export stores `message.receipt.integrity` on its completed job:
`status="verified"`, `method="s3_sha256"`, `verified_at` (Unix seconds),
`verified_segments`, `verified_bytes`. `receipt.generation` includes the SHA-256
of the published manifest, and `receipt.scope` binds it to the job, VTL,
destination/configuration revision and source directory. This is native S3
validation during transfer, distinct from an explicit later deep audit.
The API validates the receipt before marking the cartridge verified; it upserts
the new copy without removing older known generations. Legacy worker output
without a receipt still uses reconciliation. The library's S3 shelf displays
verified integrity only for a completed job with a matching receipt; legacy
completion without that proof remains explicitly unconfirmed.

### Library S3 flows

The shelf groups active exports by their pinned destination, independently of
magazine pagination, and retains the latest result for up to four other tapes.
Each lane shows acknowledged bytes, fractional progress and its own transfer-ID
scoped rate. A continuous binary ribbon moves toward S3 only while uploading with
fresh positive throughput; stale measurements, disconnection and reduced-motion
preferences stop the animation. A rotation does not move existing jobs to the new
bucket. The chart selector loads the complete transfer inventory separately from
the selected curve, so simultaneous exports remain selectable.

These rates count completed parts over approximately five-second intervals, not
socket traffic. With 256 MiB parts, one completion gives about 51.2 MiB/s and two
give 102.4 MiB/s. Simultaneous tapes can therefore show different instantaneous
values despite similar sustained throughput. The lane rates are not smoothed
with a moving average.
