# S3 generations and restoration

The API and CLI can reconcile existing S3 exports and restore
one generation into a new directory. Restoration never replaces a cartridge
in the live mhVTL spool. A separate [managed reintegration](reintegration.md)
installs a completed restore, retains the previous cartridge and resumes PBS.

The cartridge dialog shows activity, export history and the send-to-S3 action.
The **S3 storage → Copies and buckets** page provides the complete recovery
workflow, including after a fresh installation. See [monthly buckets and
recovery](s3-recovery.md). The lower-level API and CLI operations below remain
available for restoration into a separate directory.

CLI examples require an authenticated administrator session and HTTPS. Set
`COLD_VTL_API_URL=https://vaultcenter.example.net:8443` and
`COLD_VTL_CA_CERT=/etc/cold-vtl/tls/ca.crt` on the appliance, then run
`cold-vtl login --username admin`. See [authentication.md](authentication.md).

## Maintenance for local cartridge operations

Online export uses the per-cartridge media guard described in
[S3 exports](s3-exports.md): PBS may continue using other tapes. The older
manual maintenance workflow below remains available for operations requiring
a stopped library. A recovery download leaves PBS online; installation acquires
the managed maintenance lease before changing local media.

On the appliance:

```bash
cold-vtl maintenance enter
```

The configured `COLD_VTL_MAINTENANCE_HELPER` reserves the PBS drive, persists a
service start block, and stops the data plane. The API requires that lease and
the stopped services before operating on local media. A missing or invalid
maintenance state returns HTTP 409. See [maintenance.md](maintenance.md).
The older `COLD_VTL_QUIESCE_CHECK` remains a fallback for installations without
the automated adapter; it only observes service state and does not coordinate PBS.

## Reconcile existing exports

```bash
cold-vtl media reconcile CVT001L8
cold-vtl jobs watch <returned-job-id>
cold-vtl media generations CVT001L8
```

Reconciliation lists remote manifests, validates their layout and identity,
and downloads segments to verify SHA-256. This reads S3 data and can incur
provider read/transfer charges. It does not request Cold Archive retrieval.
All generations are committed to SQLite together when the operation succeeds.
If a format-2 generation matches local files, the catalogue records that
generation as verified. Otherwise it reserves a generation above existing
exports and marks the local cartridge writable. Existing S3 exports remain intact.

In the console, use **S3 storage → Copies and buckets** to discover remote copies.

## Restore a generation

```bash
cold-vtl media restore CVT001L8 --generation 2
cold-vtl media restore CVT001L8 --generation 2 --destination cvt001-copy
cold-vtl jobs watch <returned-job-id>
```

The optional destination is a new folder name, using letters, digits, hyphens
or underscores, within `COLD_VTL_RESTORE_ROOT` (default
`/var/lib/cold-vtl/restores`). The root must be separate from the spool. Existing
directories and symlinks are refused. Omitting the name generates a unique one.
The job records its generation, absolute destination and integrity result.
`jobs watch` exits unsuccessfully when the job fails.

The console combines downloading and installation through **Restore to library**.
The separate-directory operation described here remains available through the CLI.

The worker checks that the manifest SHA-256 still matches the reconciled
catalogue, rejects invalid paths and segment layouts before writing, verifies
every downloaded segment and all format-2 file checksums. Legacy format-1
manifests have segment integrity checks but no full-file hashes or empty-file
inventory. An interrupted restore retains `.cold-vtl-incomplete`; never load
that directory into mhVTL. Retry into a different new directory.

Only one active job per cartridge is allowed, enforced by SQLite. Restoring an
older generation does not change the current cartridge state or generation.
On daemon restart, interrupted jobs become failed and uploading media become
failed; reconcile their remote state before trying another export.

## Resume the data plane

After maintenance jobs finish (managed reintegration resumes services itself):

```bash
cold-vtl maintenance leave
```

Wait for the PBS iSCSI session to recover, then verify the changer inventory.
The catalogue's sealed/verified state is not SCSI write protection. Future PBS
writes can change the spool, requiring another reconciliation before export.

## API

| Method | Path | Result |
| --- | --- | --- |
| GET | `/v1/cartridges/:barcode/generations` | Cached generation inventory |
| POST | `/v1/cartridges/:barcode/reconcile` | 202 and asynchronous job |
| POST | `/v1/cartridges/:barcode/restore` | 202 and asynchronous job |

Restore body: `{"generation":2,"destination":"cvt001-copy"}`. The destination
may be omitted or null. Reconciliation accepts an empty body. HTTP 409 reports
active jobs, required maintenance, unknown generations or existing destinations.

## Browser test

The restoration UI has a browser regression test:

```bash
cd ui
npm ci
npm run build
npx playwright install chromium
npm run test:restoration
```

Set `COLD_VTL_CHROMIUM` to use an existing Chromium executable. The test serves
`ui/out` locally and intercepts all API calls with fixtures; it never contacts a
real appliance or S3 provider. Screenshots are saved in `ui/test-results`.

PBS command reference: [tape backup and restore](https://pbs.proxmox.com/docs/tape-backup.html).
