# Storage status and preliminary checks

The **S3 storage** page and `cold-vtl storage` read the status of the configured
Cold Archive v1 bucket, its incomplete multipart uploads and the list of its
current objects. No archiving, recall or content download is triggered.
Reading is allowed for administrators and viewers.

## Usage

- Console: open **S3 storage**, then **Refresh storage** for a new
  observation. There is no automatic S3 polling in the background.
- Authenticated CLI: `cold-vtl storage`.
- Authenticated API: `GET /v1/storage`.
- Worker: `cold-vtl-worker storage-status`, with the protected S3 environment.

The response contains the bucket, the VTL prefix, the region, the raw OVH
status, the lock expiry if any, the inventory, the checks and the time of
reading. It contains no access key and no S3 secret. The cartridge catalogue
data and the jobs are not changed by the diagnostic.

## Reading the states

| OVH state | Display | Access according to the storage cycle |
| --- | --- | --- |
| `None` | Available | Read and write |
| `Archiving` | Archiving | List only |
| `Archived` | Archived | List only |
| `Restoring` | Recall in progress | List only |
| `Restored` | Recall complete | Read only |
| `Deleting` | Deletion in progress | List only |
| `Flushed` | Archive emptied | List only |
| missing or unknown | Unknown state | Not confirmed |

These states describe the bucket cycle, not the IAM permissions of the account.
Mapping checked against the [OVHcloud Cold Archive v1 documentation](https://docs.ovhcloud.com/en/guides/storage-and-backup/object-storage/cold-archive-getting-started).
The specific diagnostic is enabled only for the exact HTTPS endpoint
`s3.rbx-archive.io.cloud.ovh.net`, with the OVH Cold Archive v1 provider.
For the S3-compatible provider, the object inventory remains available;
the archive state is not interpreted and no archiving/recall is offered.
The provider is chosen in **S3 storage → Configuration** (see [s3-settings.md](s3-settings.md)).

## Scope and limits of the checks

Archiving in v1 would apply to **the entire bucket**, beyond the VTL prefix.
The page therefore shows the objects outside the prefix and those whose name
is not recognised. Cartridges and generations are identified by object names,
then compared with the manifests already recorded in SQLite.

- S3 listings are paginated; the inventory stops at 100,000 objects and is
  then explicitly marked as partial. A multipart pagination limit leaves the
  number of uploads unconfirmed.
- Historical versions and delete markers are not inventoried.
- No GET of a segment or manifest is performed; fingerprints are not
  re-checked. The reconciliation information in SQLite is historical.
- The local `verified` state is not SCSI write protection. A reconciliation
  under maintenance is still required before any future action.
- The diagnostic also records the active VTL jobs, maintenance and the
  announced locks. These successive reads are not an atomic snapshot.
- An error on tags, listing or permissions leaves the check concerned
  **Not confirmed**. A complete failure clears the previous display; the
  worker read timeout is limited to 25 seconds on the API side.

The levels **Checked**, **To resolve** and **Not confirmed** describe the
findings. They do not allow any action: `actions_available` is always
`false`. The diagnostic section triggers no mutation. Preparation and
confirmation are available separately in the [archiving workflow](archive.md).
