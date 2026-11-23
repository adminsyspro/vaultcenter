# Deployment and operations reference

Manual Docker deployment, bucket rotation, internals and development notes. For a normal installation, use the installer described in [install.md](install.md).

VaultCenter is an experimental virtual tape library control plane. Its goal is
to present tape drives and a media changer to existing backup software, keep a
fast local spool, and export sealed virtual cartridges to S3-compatible object
storage before transitioning them to archive storage.

The first compatibility target is Proxmox Backup Server (PBS); Veeam can also
be bound to a VTL. The data plane exposes a mhVTL changer and LTO drive over
iSCSI with `tgt`. The Rust control plane provides a persistent catalogue,
asynchronous export jobs, a CLI, and a Next.js / shadcn web console adapted
from Studio Admin.

The distribution license has not been chosen yet. VaultCenter builds on mhVTL,
which is GPL software, and the choice depends on how the two are combined.

## Project status

The console supports VTL selection, isolated inventories and per-library S3
configuration. Additional VTLs are created in the console, then prepared and
activated for PBS as described in [tape storage](tape-storage.md) and
[PBS connection](pbs-connection.md).

Executables, Rust crates, environment variables, service names and storage
paths keep the technical prefix `cold-vtl`.

## Docker deployment

The Docker appliance includes the API, web console, CLI, worker, systemd,
mhVTL userspace and the `tgt` iSCSI target. GitHub Actions tests the backend,
UI and Docker API before publishing the tested image to
`ghcr.io/adminsyspro/vaultcenter`, a public package. The CI publishes only on a
release tag `vX.Y.Z`: `X.Y.Z`, `X.Y`, `X` and `latest`; a push to `main` is built and
tested but publishes nothing. Always pin an **image digest** for deployment, found from a version tag.
See [Releasing](release.md).

### Prepare the host and configuration

The supported way to install and update an appliance on a dedicated Debian 12 or 13
VM is the installer script described in [docs/install.md](install.md). The
manual steps below remain as a reference of what it does.

Use a **dedicated Linux amd64 VM** with Docker Engine, Compose and cgroup v2.
The appliance runs privileged with host networking and device access. Docker
Desktop/macOS and rootless Docker are not supported. Install and load the
`mhvtl` and `sg` modules on the host, and install the supplied udev rules,
following the [Docker deployment guide](docker.md). The container does
not build or load kernel modules. Reserve ports **8443** (HTTPS) and **3260**
(iSCSI), and restrict iSCSI access to the authorized PBS host.

From this repository, create the operator configuration:

```bash
cd deploy/docker || exit
mkdir -p config/cold-vtl/tls config/cold-vtl/maintenance
chmod 700 config config/cold-vtl config/cold-vtl/tls config/cold-vtl/maintenance
cp auth.env.example config/cold-vtl/auth.env
cp targets.conf.example config/targets.conf
chmod 600 config/cold-vtl/auth.env config/targets.conf
```

Before starting:

- Set `COLD_VTL_PUBLIC_URL` in `config/cold-vtl/auth.env` to the appliance's
  public HTTPS URL, keeping the listener on port 8443.
- Install a certificate trusted by your clients as `config/cold-vtl/tls/fullchain.pem`
  and its private key as `config/cold-vtl/tls/privkey.pem` (mode `0600`).
- Set the target IQN and authorized PBS IP in `config/targets.conf`.
- Configure the [PBS maintenance guard](maintenance.md), its SSH identity
  and `config/cold-vtl/maintenance.json` as described in the
  [Docker guide](docker.md) before maintenance, export or reintegration.

### Start the appliance

Pin the digest of a released image (find it with
`docker inspect --format '{{index .RepoDigests 0}}'` after pulling a version tag).
Run these commands from `deploy/docker`. The GHCR package is public: no
`docker login` is needed, see [Releasing](release.md).

```bash
export VAULTCENTER_IMAGE=ghcr.io/adminsyspro/vaultcenter@sha256:<digest>
docker compose config --quiet
docker compose pull

# Bash: create an administrator without putting the password in shell history.
read -rsp 'Administrator password (at least 12 characters): ' vaultcenter_password
printf '\n'
printf '%s\n' "$vaultcenter_password" | docker compose run --rm -T \
  --entrypoint /usr/local/bin/cold-vtld appliance create-user admin --role admin
unset vaultcenter_password

docker compose up -d
docker compose ps
docker compose exec appliance systemctl --no-pager status cold-vtld mhvtl.target tgt
```

Open the configured HTTPS URL and sign in as `admin`. Keep `VAULTCENTER_IMAGE` set
for subsequent Compose commands, or save it in `deploy/docker/.env`. The initial
library is empty; create test cartridges explicitly using the Docker guide.

### Persistence, updates and validation

The named volumes `vaultcenter_state`, `vaultcenter_media` and `vaultcenter_mhvtl-config`
store the catalogue/restores, tape spool and mhVTL configuration. Operator
configuration stays in `deploy/docker/config/`. Preserve all of them; **do not
run `docker compose down -v`** on an installation you want to keep.

To update, finish active jobs, enter verified maintenance, stop the appliance
and back up the three volumes and operator configuration. Select the new tested
digest, then run `docker compose pull` and `docker compose up -d`. Verify the
API and maintenance lock before resuming. See the [Docker guide](docker.md)
for rollback, logs, local builds and the full acceptance checklist.

CI validates TLS, authentication, the UI and catalogue access. Before production,
validate the host kernel integration, SCSI/iSCSI devices, maintenance persistence
and a complete PBS backup/restore on the dedicated VM. The API healthcheck alone
does not validate the tape data plane.

## Bucket rotation

Each VTL has one S3 backend and one active bucket for new exports. In **S3 storage**,
archiving the active OVH Cold Archive v1 bucket prepares its successor first.
Choose the next bucket name or leave it blank for a unique name containing the VTL,
year and month. The old bucket keeps its name and its catalogue entries.

The bucket selector in **S3 storage** and **Cartridges → Details** provides access
to previous buckets. Recall a cold bucket before restoring a cartridge. Jobs retain
their destination and configuration revision, so rotation does not redirect an
in-flight restore to the active bucket. Historical credentials stay in the protected
catalogue and are not returned by the bucket-list API.

Rotation records creation and archival intent durably. Ambiguous cloud responses
are observed without replaying mutations. An unconfirmed bucket creation remains
blocked for operator inspection; VaultCenter never adopts or deletes a bucket merely
because its name matches. Install the API, worker and archive helper together.

## Architecture overview

```text
PBS -> iSCSI -> SCSI VTL adapter -> local cartridge spool
                                      |
                                      v
                             segmenter + manifest
                                      |
                                      v
                               S3 provider API
                                      |
                                      v
                              OVH Cold Archive
```

The implementation deliberately separates:

- the SCSI data plane, backed by an existing VTL implementation (mhVTL);
- the Rust control plane and cartridge catalogue;
- provider adapters for S3 and archive lifecycle operations.

The `cold-vtl-worker` exports a cartridge directory as immutable segments,
validating SHA-256 through S3 while transferring each part. New exports avoid
whole-cartridge preparation scans and payload readback; native composite checksums
are checked before the v3 manifest is published. Existing v1/v2 copies remain
restorable, and explicit deep verification still reads and checks every segment.
See [transfer integrity and resume](s3-exports.md) for memory bounds and
legacy journal compatibility.

## Operator interfaces

`cold-vtld` serves the API and the web console. Its catalogue is stored in
SQLite and media are discovered from the mhVTL spool at startup.

```bash
cargo run -p cold-vtl-api
```

The service listens on `127.0.0.1:8443` by default. Configure HTTPS and create
a local administrator before using it; see [authentication](authentication.md). Set `COLD_VTL_LISTEN`,
`COLD_VTL_DATABASE`, `COLD_VTL_MEDIA_ROOT`, or `COLD_VTL_WORKER` to
override its runtime paths. The web console is served at `/`.

Build the interface with `cd ui && npm ci && npm run build`, then set
`COLD_VTL_UI_DIR` to the absolute path of `ui/out`. Rust serves the static
export directly; Node.js is only needed during the build. See [ui/README.md](../ui/README.md)
for deployment and attribution. Without this setting, the original embedded
console remains available as a fallback.

Use the CLI locally or point it at a remote daemon:

```bash
cold-vtl --ca-cert /etc/cold-vtl/tls/ca.crt login --username admin
# Set COLD_VTL_CA_CERT and COLD_VTL_API_URL for the following commands.
cold-vtl status
cold-vtl media list
cold-vtl maintenance enter
cold-vtl media seal CVT002L8
cold-vtl media export CVT002L8
cold-vtl jobs list
cold-vtl jobs watch <job-id>
cold-vtl media reconcile CVT001L8
cold-vtl media generations CVT001L8
cold-vtl media restore CVT001L8 --generation 2
# After all jobs finish:
cold-vtl maintenance leave

cold-vtl --api https://vaultcenter.example.net:8443 --ca-cert /path/to/ca.crt status
```

On the appliance, the daemon runs as a systemd service. Its console is available
at its HTTPS URL (for example `https://vaultcenter.example.net:8443/`) with local
and OIDC (for example Keycloak) authentication.

```bash
systemctl status cold-vtld
journalctl -u cold-vtld
```

Local accounts, administrator/viewer roles, browser sessions and revocable
API tokens protect the control plane. See [docs/authentication.md](authentication.md)
for bootstrap, certificates, Keycloak Docker and recovery procedures.

S3 generations and restoration are available in the CLI and in
**Cartridges → S3 copies**. Restores use a separate directory and verify
integrity before reporting success. Sealing, reconciliation and export require
a managed maintenance window that reserves the PBS drive and stops the data
plane. Use **Maintenance** in the console or `cold-vtl maintenance enter/leave`.
See [docs/maintenance.md](maintenance.md) for the adapter and recovery, and
[docs/restoration.md](restoration.md) for maintenance and reintegration.

After a restore completes, **S3 copies → Reintegrate into PBS** installs it under
managed maintenance and preserves the previous cartridge. **Maintenance** also
offers recovery/rollback for an interrupted reintegration. The CLI equivalents
are `media reintegrate --restore-job`, `media reintegrations` and `media rollback`.
See [docs/reintegration.md](reintegration.md) for the complete workflow.

**S3 storage** and `cold-vtl storage` show the configured bucket status,
bucket-wide object scope and preliminary archive checks. This diagnostic is
read-only. The same page also provides separate administrator workflows for
archive and recall, with preparation, explicit confirmation and durable tracking.
See [docs/storage.md](storage.md) for state meanings and inventory limits.

## Development

```bash
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## S3 worker

Load the protected environment file before invoking the worker:

```bash
set -a
. /etc/cold-vtl/s3.env
set +a

cold-vtl-worker check
cold-vtl-worker bucket-status
cold-vtl-worker export-directory \
  --barcode CVT001L8 \
  --generation 2 \
  --source /opt/mhvtl/CVT001L8
cold-vtl-worker verify --barcode CVT001L8 --generation 2 --deep
cold-vtl-worker restore \
  --barcode CVT001L8 \
  --generation 2 \
  --destination /var/lib/cold-vtl/restore/CVT001L8
```

Bucket-level Cold Archive commands exist for the legacy OVHcloud
`rbx-archive` endpoint, but require the exact bucket name and explicit billing
acknowledgement. No delete-archive command is exposed.

See [docs/architecture.md](architecture.md) for the design and
[docs/operator.md](operator.md) for day-to-day commands.

[Guarded OVH bucket archiving](archive.md) adds a SHA-256 preparation, an
explicit confirmation and persistent tracking in the console and the CLI.
Recall is available without stopping PBS.

**S3 storage → Configuration** tests and saves the bucket, endpoint, region and
credentials without a restart. See [S3 configuration](s3-settings.md).

Create virtual cartridges with configurable LTO type, capacity, S3 segment size and multipart part size: [cartridge creation](cartridge-creation.md). Resumable streaming transport: [S3 exports](s3-exports.md).
