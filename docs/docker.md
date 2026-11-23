# VaultCenter Docker appliance

The GitHub Actions pipeline builds a **Linux amd64** image containing VaultCenter
(API, console and worker), systemd, the mhVTL daemons and the tgt iSCSI target.
It is a privileged appliance to be run on a **dedicated Linux VM**.
The mhVTL kernel module stays installed on the host; Docker Desktop/macOS,
rootless Docker and a host shared with another VTL are not supported.
The container sees the devices and uses the host's network and cgroups.
This configuration does not provide security isolation from the host.

Run the [appliance acceptance test](#appliance-acceptance-test-before-production)
on the kernel, SCSI, iSCSI and PBS before production use.
The image configures the existing library 10 and drive 11. The
multi-VTL interface keeps its drafts and its independent buckets; automated physical
provisioning of several libraries is not added by Docker.

## What the CI checks and publishes

The [ci.yml](../.github/workflows/ci.yml) workflow checks PRs and pushes:

1. Rust: formatting, Clippy without warnings, workspace tests.
2. Python: maintenance, archive and reintegration controllers, and the Docker pre-check.
3. UI: locked install, build, type checking and nine browser suites against a simulated API.
4. Image: multi-stage build, then a TLS/API/UI smoke test without privileged mode.
   The smoke test creates an ephemeral database and an administrator through stdin, refuses
   anonymous requests, signs in and reads the multi-VTL inventory.
5. On a push to main or a `v*` tag only: publication of **the same tested image**
   to `ghcr.io/adminsyspro/vaultcenter`. A push to `main` publishes `main` and
   `sha-<full SHA>`; a `vX.Y.Z` tag publishes `X.Y.Z`, `X.Y`, `X`, `latest`
   and `sha-<full SHA>`. See [release.md](release.md).

PRs and manual runs publish nothing. No workflow deploys to an appliance and
no automatic update is performed. The `GITHUB_TOKEN` is enough for GHCR if the
organisation allows Actions to create/write the package. The package is public:
pulling an image needs no credentials (see [release.md](release.md)). Always
deploy a **digest**, taken from a version tag.

The build uses Rust 1.96.1 and Node 26.2.0. Application dependencies
are locked by `Cargo.lock` and `package-lock.json`. mhVTL uses commit
`59f32ee50b3269c118965bffa29cf38b1553268f`; its corresponding
sources are provided in `/usr/share/doc/mhvtl/source.tar.gz`.
The base images have version tags, not yet locked digests.

## Preparing the Linux host

The supported way to prepare the host is the script described in
[install.md](install.md). This section remains as a reference for what the
script does on the host.

Plan for Docker Engine with Compose, cgroup v2, a durable local file system
and enough space for the tapes and the restores. Install the
module for **the kernel actually booted**, following the
[mhVTL instructions](https://github.com/markh794/mhvtl/blob/59f32ee50b3269c118965bffa29cf38b1553268f/INSTALL).
Debian/Ubuntu example, outside the container:

```bash
sudo apt-get update
sudo apt-get install build-essential git linux-headers-$(uname -r) kmod
git clone https://github.com/markh794/mhvtl.git mhvtl-host
cd mhvtl-host
git checkout 59f32ee50b3269c118965bffa29cf38b1553268f
# Adapt this path to the VaultCenter repository:
git apply /path/to/vaultcenter/deploy/mhvtl/parallel-host-queue.patch
git apply /path/to/vaultcenter/deploy/mhvtl/command-wakeup.patch
make kernel
sudo make -C kernel install
sudo modprobe mhvtl opts=0
sudo modprobe sg
```

If Secure Boot is enabled, the module must be signed according to the host's
policy. After a kernel update, rebuild and check the module
before restarting the appliance. The container neither builds nor loads a module.
Make the loading persistent on the host:

```bash
printf 'mhvtl\nsg\n' | sudo tee /etc/modules-load.d/vaultcenter.conf
# From the root of this repository:
sudo install -m 0644 deploy/host/vaultcenter-mhvtl.conf /etc/modprobe.d/vaultcenter-mhvtl.conf
sudo install -m 0644 deploy/host/99-vaultcenter-console.conf /etc/sysctl.d/99-vaultcenter-console.conf
sudo sysctl --load /etc/sysctl.d/99-vaultcenter-console.conf
sudo install -m 0644 deploy/docker/99-vaultcenter.rules /etc/udev/rules.d/99-vaultcenter.rules
sudo udevadm control --reload-rules
sudo udevadm trigger --subsystem-match=scsi_generic
```

`opts=0` disables the mhVTL driver's tracing of each SCSI command, which is enabled
by default upstream. With a serial console, emitting these traces synchronously can
slow writes down considerably. For a module that is already loaded, also apply
`printf '0\n' | sudo tee /sys/bus/mhvtl/drivers/mhvtl/opts`: this change
requires neither an iSCSI reconnection nor a drive restart. It does not disable
the driver's normal error messages. The file in `modprobe.d` keeps
the setting for future loads.

The installer `deploy/host/install.py --new-appliance` installs and applies
these two settings automatically. `kernel.printk = 4 4 1 7` limits console
output to errors and more severe levels; the other messages
remain available in the kernel log. This prevents another verbose module
from forcing synchronous writes to the serial console.

The drive's `Backoff` parameter also affects its shutdown: in this version
of mhVTL, exiting waits until the polling interval exceeds `0xf000` µs.
After activity, `Backoff: 10` can extend this wait to about
190 seconds. The drives of isolated VTLs use this value with
`TimeoutStopSec=240`; the changer keeps `Backoff: 400` and 30 seconds.
The controller waits up to 255 seconds for a drive to stop, the API
600 seconds for a complete transition and the interface 610 seconds.
Compose gives the appliance 300 seconds. For an existing installation,
also set its `stop_grace_period` to `300s` before the next recreation.
Do not apply only the `Backoff` change: the timeouts must
be updated together during maintenance, with no active PBS job.

With this setting, writes to a test cartridge were about 17% faster, with
read-back and SHA256 verification. This figure does not predict the gain for a
PBS backup in production. With these timeouts, maintenance transitions completed
in 110 to 191 seconds without forcing the drive to stop.

The targets of isolated VTLs announce `InitialR2T=No`, `ImmediateData=Yes`,
`FirstBurstLength=262144`, `MaxBurstLength=262144` and
`MaxRecvDataSegmentLength=262144` before accepting a connection. To
benefit from them, the PBS VaultCenter iSCSI node must also offer these values.
Modify only this node, identified by its IQN and portal, without
changing `/etc/iscsi/iscsid.conf` or the nodes of other appliances. The IQN of
each VTL is shown on its connection page: a VTL created before the
product was renamed keeps its own, a new VTL receives by default
`iqn.2026-10.io.vaultcenter:vtl-<UUID>`.

```bash
# On PBS, between two jobs, with the drive unloaded. Replace both values.
vaultcenter_iqn='iqn.2026-10.io.vaultcenter:vtl-UUID'
vaultcenter_portal='VAULTCENTER_IP:3260'
sudo iscsiadm -m node -T "$vaultcenter_iqn" -p "$vaultcenter_portal" --logout
sudo iscsiadm -m node -T "$vaultcenter_iqn" -p "$vaultcenter_portal" -o update -n node.session.iscsi.MaxBurstLength -v 262144
sudo iscsiadm -m node -T "$vaultcenter_iqn" -p "$vaultcenter_portal" -o update -n node.session.iscsi.FirstBurstLength -v 262144
sudo iscsiadm -m node -T "$vaultcenter_iqn" -p "$vaultcenter_portal" -o update -n node.session.iscsi.ImmediateData -v Yes
sudo iscsiadm -m node -T "$vaultcenter_iqn" -p "$vaultcenter_portal" -o update -n node.session.iscsi.InitialR2T -v No
sudo iscsiadm -m node -T "$vaultcenter_iqn" -p "$vaultcenter_portal" -o update -n 'node.conn[0].iscsi.MaxRecvDataSegmentLength' -v 262144
sudo iscsiadm -m node -T "$vaultcenter_iqn" -p "$vaultcenter_portal" --login
sudo iscsiadm -m session -P 3
```

Check the **negotiated** values of the VaultCenter session, not only the
values configured on the target. In a synthetic comparison
at a constant `Backoff=10`, throughput was 280–282 MB/s with the restrictive PBS profile
and 366 MB/s with this profile, on 256 KiB blocks read back and verified.
This is not a guaranteed gain in production.
The `timerslack_ns=1000` test did not show a reproducible gain;
keep the default value of 50000 ns.

These rules match the identities in the supplied `device.conf`. If you change
the device identifiers, adjust the udev rules, the tgt
configuration and the device wait script together. Ports 3260 and
8443 must be free; do not start competing mhVTL/tgt daemons on
the host. Restrict the iSCSI port to the authorised PBS in the host firewall.

The daemon units lift the systemd `ProtectKernelTunables` protection
present upstream: mhVTL must write the `add_lu` sysfs attribute to register
the changer and the drive. This exception applies to these two templates;
loading the modules remains a host operation.

## Operator configuration

From `deploy/docker`:

```bash
mkdir -p config/cold-vtl/tls config/cold-vtl/maintenance
chmod 700 config config/cold-vtl config/cold-vtl/tls config/cold-vtl/maintenance
cp auth.env.example config/cold-vtl/auth.env
cp targets.conf.example config/targets.conf
chmod 600 config/cold-vtl/auth.env config/targets.conf
```

Adapt the public name in `auth.env`, and the IQN and PBS IP in `targets.conf`.
Install the certificate and its key at the paths `tls/fullchain.pem` and
`tls/privkey.pem`. Use a certificate trusted by the clients and keep
the key in mode 0600. The HTTPS listener stays on 8443 for the supplied healthcheck.
For OIDC, reuse the variables described in [authentication.md](authentication.md)
and mount the secrets under `config/cold-vtl`; do not build them into the image.
The files in this directory are ignored by Git and by the Docker context.

Maintenance, controlled archiving and reintegration require the PBS guard
described in [maintenance.md](maintenance.md) and [reintegration.md](reintegration.md).
Install its configuration and its protected SSH identity before these operations:

```json
{
  "pbs": "root@pbs.example.net",
  "key_file": "/etc/cold-vtl/maintenance/id_ed25519",
  "known_hosts": "/etc/cold-vtl/maintenance/known_hosts",
  "media_root": "/opt/mhvtl",
  "restore_root": "/var/lib/cold-vtl/restores",
  "database": "/var/lib/cold-vtl/catalog.db"
}
```

Save this JSON in `config/cold-vtl/maintenance.json` (0600). Check
the PBS SSH fingerprint before filling in `known_hosts`. Without a configured guard and
identity, the controllers refuse the operations that need its lock.

## First start

The supported way to perform the first start is the script described in
[install.md](install.md), which also creates the administrator. This section remains
as a reference for what it does.

Choose the digest of a published version (procedure in [release.md](release.md)):

```bash
export VAULTCENTER_IMAGE=ghcr.io/adminsyspro/vaultcenter@sha256:REPLACE_WITH_DIGEST
docker compose pull
docker compose config --quiet
```

Create the administrator before starting, without a password in the history
or in the image. Bash example:

```bash
read -rsp 'Administrator password (12 characters minimum): ' vaultcenter_password
printf '\n'
printf '%s\n' "$vaultcenter_password" | docker compose run --rm -T \
  --entrypoint /usr/local/bin/cold-vtld appliance create-user admin --role admin
unset vaultcenter_password
docker compose up -d
docker compose logs --tail=100
docker compose exec appliance systemctl --no-pager status cold-vtld mhvtl.target tgt
```

The initial library is empty. The image neither recreates nor erases cartridges.
The volumes `vaultcenter_state`, `vaultcenter_media` and `vaultcenter_mhvtl-config` are
kept when the container is replaced. To import an installation,
stop the tasks and services cleanly, then restore the catalogue,
spool and configuration together with the same paths/identities before starting.
Never share these volumes between two active appliances.

To add a cartridge, explicitly enter maintenance, then use
**Cartridges → New cartridge**. The controller confirms the PBS lock,
that the services are stopped, that there are no jobs/workers and that a slot is free before
creating the media and its catalogue entry. Resuming the services remains
an explicit action. The installed IBM TD8 drive accepts LTO7 and LTO8; any
other drive or a different topology is refused without prior validation.
The whole virtual capacity ranges from 1 to 16,777,216 MiB, S3 segments from
16 MiB to 1 TiB (initial values: capacity of 2048 MiB, segments of 1 TiB).
Multipart parts range from 16 to 1024 MiB, with 256 MiB by default and at most
10,000 parts per segment. See [S3 exports](s3-exports.md).

The image installs `COLD_VTL_CARTRIDGE_HELPER` and uses the paths from
`maintenance.json` (`media_root`, `database`, `mhvtl_config`, the latter defaulting to
`/etc/mhvtl`). The helper never selects a path supplied by
the request. After an interruption, submit exactly the same parameters:
the `cartridge-creations/` log resumes a verified blank media without running
`mktape` again on an existing folder. A partially written media or a modified
inventory requires operator recovery; the files are kept and
`cartridge-creation-pending.json` blocks the resumption of services. Do not delete
this marker to get around an error.

The validator and the command `mktape -H … -l 10 -s … -t data -d …` follow the
[pinned mhVTL code](https://github.com/markh794/mhvtl/blob/59f32ee50b3269c118965bffa29cf38b1553268f/usr/cmd/mktape.c)
(capacities in MiB, MAM 6/4 format and blank partition files).

## Appliance acceptance test before production

The CI API smoke test does not validate kernel access. On the acceptance test VM:

- Check `systemctl is-active` for `mhvtl.target`, `vtllibrary@10.service`,
  `vtltape@11.service`, `tgt.service` and `cold-vtld.service` in the container.
- Check `lsscsi -g`, the two `/dev/tape/by-id` links, then
  `tgtadm --lld iscsi --mode target --op show`. The target must be restricted
  to the configured PBS and the LUNs must match the two devices.
- From PBS, discover the target and check the changer/drive. Run backup
  and restore jobs with test media and data.
- Enter maintenance through VaultCenter, restart the container, check that
  `maintenance-block` still exists and that tgt/drive/changer remain
  stopped. The API must stay available; resume only through the controller.
- Check reintegration and resumption after an interruption with the PBS guard.
- Recreate the container with the same volumes and check accounts, catalogue,
  tapes, bucket histories and configuration. Validate a complete restore
  of the backups on a separate VM.

The healthcheck measures the local HTTPS API; its TLS check ignores the
certificate name on the local loopback only, without credentials. It stays healthy during
normal maintenance. It does not certify that the data plane is online.
Use `docker compose exec appliance journalctl -u tgt -u vtllibrary@10
-u vtltape@11 --no-pager` for its logs.

## Update and rollback

The supported way to update, with backup and automatic rollback,
is the script described in [install.md](install.md). This section
remains as a reference for what it does.

Wait for the jobs to finish, open a verified maintenance, then stop the
container. Back up the three volumes and `config/` during this stop,
preserving owners and permissions; note the current digest. Change
`VAULTCENTER_IMAGE`, run `docker compose pull` then `docker compose up -d`.
Validate the API and the lock before resuming from maintenance.

Going back to an older image can be incompatible with an SQL migration.
In that case, restore the saved volumes and configuration **together**, with
the matching image, and check the PBS lease before any resumption. Never
delete `maintenance-block` to get around a refusal. Do not use
`docker compose down -v` on an installation you want to keep.

Local build from the root: `docker build -t vaultcenter:dev .`.
Isolated smoke test: `bash deploy/docker/smoke.sh vaultcenter:dev` (local port 18443 free).

## Optional tape telemetry

The image includes an opt-in read-only collector and a nonblocking mhVTL status
FIFO patch. It is skipped without operator configuration; the standard tape
configuration does not enable a FIFO. See [telemetry](telemetry.md) for the
protocol, permissions, metric limits and local validation. No appliance
is needed to use the isolated UI demonstration.

## Tape storage path

In **Cartridges → Storage configuration**, an administrator chooses the
path of each VTL. This path is permanently locked as soon as data
is present. Services and mounts are independent in isolated mode; a
library that is ready on the storage side still requires explicit PBS activation.

See [the storage guide](tape-storage.md) for installation, migration
(limited to unused appliances) and resumption by UUID.

The `parallel-host-queue.patch` patch raises the mhVTL adapter queue to 128 commands while keeping one command per LUN. The upstream value of 1 serialises all VTLs: slowing down one drive would slow down the others. PBS limits refuse to activate on this older module. Replacing the module requires stopping **all** mhVTL appliances on the host, outside any backup, then checking the device identities and the isolation between VTLs. A rollback must restore the saved module before resuming the services.


## Waking drives when commands arrive

The `command-wakeup.patch` patch lets the mhVTL module wake `vtltape`
when a SCSI command is available for its drive. It keeps the existing adaptive
delay as a timeout, the handling of signals and administrative
messages, and the global ioctl mutex. The change does not shorten
the shutdown procedure of drives configured with a high backoff.

The new daemon queries the capabilities on its already open descriptor:

| Host module | Userspace | Drive wait |
| --- | --- | --- |
| Old | Old or new | Timed, original behaviour |
| New | Old | Timed, compatible |
| New | New | Wake on command, with the original timeout |

A `ppoll` error disables wake-up for this process and restores the original
wait; a signal interrupting the wait immediately returns control to the
main loop. The module allows a single open per minor (`EBUSY`
for a second open), because closing the owner removes its SCSI
LU. Never open a second descriptor to probe an old module:
closing it can remove an active drive. `dup` and `fork` keep the
normal semantics of closing the last shared descriptor.

The image provides the daemon and the patched sources, without reloading the host's
module. To enable wake-up, **also** install the module built for the
booted kernel: either with the two patches above, or from the source
archive provided by the image (already patched, do not apply the patches again).
A module update requires no PBS jobs, unloaded
tapes and maintenance of **all** VTLs on the host. Back up the existing
module, stop their services and targets, replace/reload the module, then
resume and qualify the services. If the module stays busy, do not force
its removal. Plan a controlled reboot. Do not update a production
VM during a job to enable this optimisation.

In tests, wake on command improved single-drive write throughput
from 366 to 395 MB/s (+8%), with 35.9 GB read back and verified. The gain with two
drives was small and p95 latency increased; no CPU gain was
established. These measurements do not constitute a throughput guarantee in production.
