# Installing and updating VaultCenter

The `install.sh` script installs a new VaultCenter appliance on a dedicated
VM, and later updates it with the same command. It has no other source than
itself and the `ghcr.io/adminsyspro/vaultcenter` image, designated by its
digest: everything the host receives comes from that image.

```bash
curl -fsSL https://vaultcenter.io/install | sudo bash
```

To pass options, put them after `-s --`: `… | sudo bash -s -- --version X.Y.Z`.

The script chooses its mode by itself:

| State of the machine | Mode |
| --- | --- |
| Neither `/etc/vaultcenter-host` nor a `vaultcenter-appliance-1` container | new installation |
| `/etc/vaultcenter-host/compose.json` present, without an `installing=1` marker | update |
| `/var/lib/vaultcenter-install/state` present without `compose.json`, or with `installing=1` | resuming an interrupted installation |
| an installation under the former product name (directory or container with the former name) | refused: this installer does not convert it |
| any other mix | refused, with what was found |

Refusals change nothing. On a terminal, the script asks for a confirmation
worded for the mode ("Install VaultCenter on this machine?", "Update the
VaultCenter appliance on this machine?", "Resume the interrupted
installation…", "Cancel the interrupted update…"); without a terminal,
`--yes` is required.

This guide ships in the image: once the host files are extracted, it is on the
machine, in `/usr/local/share/vaultcenter/<version>/host/install.md`, and that
is the path the messages of the script and of `update.py` give.

## Requirements

- A **dedicated VM**, Debian **12 or 13**, **amd64**. Other systems, other
  architectures, a container (LXC) and rootless Docker are refused.
- The script run as **root** (`sudo`).
- `curl`, `python3` and `openssl`: the script checks them and installs through
  apt those that are missing (`curl` with `ca-certificates`), once the safety
  checks have passed and before their first use. `curl` is necessarily there
  when the script arrives through `curl | bash`; it may be missing when the
  script is run from a file. An update does not need `openssl`.
- **Secure Boot disabled**: the `mhvtl` kernel module is not signed and could
  not be loaded. The script only checks this if `mokutil` is installed, and then refuses if Secure Boot is reported as enabled.
- The headers of the **running** kernel available (present in
  `/lib/modules/<kernel>/build` or in the repositories), and the headers
  metapackage of the kernel flavour (`linux-headers-amd64`,
  `linux-headers-cloud-amd64` or `linux-headers-rt-amd64`) available in the
  repositories. The script refreshes the apt lists itself (`apt-get update`)
  before checking. If the headers of the running kernel still cannot be found,
  that kernel is no longer in the repositories: update the kernel
  (`apt-get dist-upgrade`), reboot, run again.
- Outbound HTTPS access, behind a proxy if needed: run `sudo -E` to pass
  `HTTPS_PROXY` (and `HTTP_PROXY`, `NO_PROXY`) to the script and to Docker.
  `apt-get` only reads the lowercase variables: the script passes the proxy to
  it on every call (`-o Acquire::http::Proxy=…`, `-o Acquire::https::Proxy=…`,
  each taking the other's value if only one is set). The appliance itself is
  always queried without a proxy (`/health` on `127.0.0.1`, with `--noproxy`).
  A proxy whose URL carries a password is visible in the process list during
  apt calls; the script's log masks it.

Safety checks, never skipped (the script runs all of them, then refuses and
lists every cause):

- Debian 12 or 13; amd64; systemd; cgroup v2 (unified hierarchy); not a
  container; not rootless Docker;
- on a new installation: ports **8443** and **3260** free, no `tgt` or `mhvtl`
  daemon running on the host (units `tgt.service`, `mhvtl.target`, processes
  `tgtd`, `vtltape`, `vtllibrary`);
- Secure Boot disabled (checked only if `mokutil` is installed); headers of the
  running kernel available; before the module step, headers metapackage of the
  kernel flavour available; package of each missing tool (`curl`, `python3`,
  `openssl`) known to apt.

The preflight only reads, with one exception: it refreshes the apt lists
(`apt-get update`), otherwise a cloud image with empty lists would be wrongly
refused. If this refresh fails, it is a cause of refusal.

Sizing checks (skipped by `--skip-preflight`):

| Check | Threshold |
| --- | --- |
| CPU | at least 2 |
| RAM | 4 GiB (the actual threshold is 3.5 GiB, because the RAM read is a little below the nominal size) |
| Free space under `/var/lib/docker` | 20 GiB |
| Outbound HTTPS | `https://ghcr.io/v2/`, `https://deb.debian.org/`, `https://download.docker.com/` reachable |

During an update, and when resuming on an appliance that has already started,
the ports and the host daemons are not checked (the appliance legitimately
holds them). During an update, the CPU, RAM and free space thresholds are not
checked either: they apply to a new installation, not to an appliance in
service whose disk holds its data. Only the outbound HTTPS access is checked.

## Options

No option is required: the image is public, and without `--token` or
`--registry-token` the script downloads it without credentials, without
calling `vaultcenter.io` or logging in to the registry. `--registry-user`
without `--registry-token` stops the script (code 2) before any change.

| Option | Effect |
| --- | --- |
| `--token <token>` | Optional, reserved for distribution by customer token: the token is exchanged with `https://vaultcenter.io/api/v1/install/validate` for registry credentials. Not needed as long as the image is public. Cannot be used with `--registry-token`. |
| `--registry-token <token>` | Private image mode, for a version that is not public: a GHCR `read:packages` token used directly, without the site. Requires `--registry-user`. |
| `--registry-user <name>` | GitHub account that owns the GHCR token. Required with `--registry-token`. |
| `--version X.Y.Z` | Target version: `X.Y.Z`, or a pre-release `X.Y.Z-<pre-release>` (for example `0.2.0-rc.1`; a pre-release is lower than its release). By default, the `latest` tag. |
| `--yes` | Asks no question. Required outside a terminal. During an update, also asserts that no backup is running on the VTLs without a maintenance guard and that no Veeam job is running (see "Updating"). |
| `--quiet` | One `[n/N] step: result` line per step, without decorated output. This is also the output outside a terminal. `NO_COLOR` is honoured. |
| `--skip-preflight` | Skips the sizing checks. The safety checks remain. |
| `--resume-rollback` | Restores the backup of an interrupted update. Only for an installed appliance. Pass the same `--version` as the interrupted update: the script needs the image and the files of that version. |
| `--force-rollback` | With `--resume-rollback` only: restores even if the appliance runs in good health in the target version. Never after a verified update. |

## Private image mode

This mode is for installing an image that is not public (a private test
package, for example): the script is copied by hand and the GHCR token is
given directly. For a published version, it is not needed.

1. On GitHub, create a **classic** personal access token with only the
   `read:packages` scope.
2. Have the script on the VM: the `deploy/install/install.sh` file of the
   repository, copied to the VM, or the one in the image
   (`/usr/local/share/vaultcenter/host/install.sh`).
3. Run, as root:

   ```bash
   sudo bash install.sh --registry-user <GitHub account> --registry-token <token> --version X.Y.Z
   ```

The token is passed to the script as an argument: it is visible in the process
list (`ps`) while the script runs and in the shell history. **`sudo` also
records the whole command line, token included, in the system journal**
(`journalctl _COMM=sudo`, `/var/log/auth.log` depending on the configuration),
where it stays after the operation: this applies to `--token` as well as to
`--registry-token`, including in the form `curl … | sudo bash -s --
--token <TOKEN>`. Run the command from a trusted machine and revoke the token
after the operation. The script masks it in its log and writes it nowhere (an
unknown option such as `--token=<token>` is only echoed up to the `=` sign);
Docker keeps no trace of it after the download (see below).

## What the script changes on the host

All paths are those of a real machine. The script has no uninstall command and
deletes nothing on failure.

**Log and state of the script**

- `/var/log/vaultcenter-install.log` (0600): everything the script runs and
  gets, without tokens or passwords; the progress of an update is added to it.
- `/var/lib/vaultcenter-install/state`: finished steps (`done=`), chosen image
  and version, `installing=1` marker, state of the isolation receipt, public
  name. A successful summary removes the `done=` and `installing=1` lines from
  it.
- `/var/lib/vaultcenter-install/mhvtl.sha256`: fingerprint of the sources of
  the installed module.
- `/var/lib/vaultcenter-install/compose/compose.yaml`: the Compose file of the
  image with its two `./config/...` mounts made absolute.
- `/var/lib/vaultcenter-install/update.lock`: lock of the update.
- `/etc/vaultcenter-install/docker/` (0700): Docker client configuration of
  the script's own (`DOCKER_CONFIG`). The registry login is written there for
  the duration of the download, then removed by `docker logout`; no registry
  credentials stay there.

**Packages and repositories**

- `/etc/apt/keyrings/docker.asc` and `/etc/apt/sources.list.d/docker.list`:
  the official Docker repository (`bookworm` or `trixie`), only if Docker (with
  the Compose plugin) is missing. In that case: `docker-ce`, `docker-ce-cli`,
  `containerd.io`, `docker-compose-plugin`, and `systemctl enable --now docker`.
  A Docker that is already usable is kept as it is.
- `dkms`, `build-essential`, `linux-headers-<running kernel>` and the headers
  metapackage of the kernel flavour (`linux-headers-amd64`,
  `linux-headers-cloud-amd64` or `linux-headers-rt-amd64`), which brings in the
  headers of every later kernel. For a kernel of another flavour, only the
  versioned package is installed, with a warning.
- `curl` (with `ca-certificates`), `python3`, `openssl`: only those that are
  missing.
- The apt lists are refreshed (`apt-get update`) by the preflight checks, then
  again after the Docker repository is added.
- `/etc/systemd/system/docker.service.d/vaultcenter-proxy.conf`, only if
  `HTTPS_PROXY` is set at installation: the Docker daemon is then restarted.
  During an update, the script does not touch this file and never restarts
  Docker (that would cut off the appliance in service): if it differs from the
  environment, the script says so, with the command to apply it later.

**Image and host files**

- The `ghcr.io/adminsyspro/vaultcenter` image downloaded by Docker; a
  temporary `vaultcenter-install-payload` container (created, never started,
  always removed).
- `/usr/local/share/vaultcenter/<version>/host/` and
  `/usr/local/share/vaultcenter/<version>/mhvtl-source.tar.gz`: host files
  (including `update.py` and this guide, `install.md`) and module sources,
  extracted from the image.

**`mhvtl` kernel module**

- `/usr/src/mhvtl-vc-<first 12 digits of the sha256 of the sources>/` (with
  `dkms.conf`), the DKMS tree `/var/lib/dkms/mhvtl/` and the module installed
  under `/lib/modules/<kernel>/updates/dkms/`, for **every kernel** whose
  headers are installed (the running kernel first; a failure for another kernel
  is a warning).
- `mhvtl` module loaded (`modprobe mhvtl opts=0`), `sg` module loaded.
- `/etc/systemd/system/vaultcenter-mhvtl.service`, enabled: at boot, before
  Docker, it reloads the module and rebuilds it through DKMS if it is missing
  (see "DKMS and kernel updates").
- `/etc/modules-load.d/vaultcenter.conf` (`mhvtl`, `sg`),
  `/etc/modprobe.d/vaultcenter-mhvtl.conf`,
  `/etc/sysctl.d/99-vaultcenter-console.conf` (applied by `sysctl --load`),
  `/etc/udev/rules.d/99-vaultcenter.rules` (rules reloaded).

**Host storage service** (written by `install.py`, from the image)

- `/etc/vaultcenter-host/` (0700): `compose.json`, `storage.json`,
  `storage-socket.conf`, `isolated-mode`.
- `/usr/local/libexec/vaultcenter-host/`: `storage_controller.py`,
  `storage_paths.py`, `vtl_state.py`, `vtl_storage.py`, `storage_mounts.py`.
- `/etc/systemd/system/vaultcenter-storage-host.service` and
  `vaultcenter-storage-mounts.service`, enabled and started. The storage
  service is ordered **before** Docker, so that the appliance comes back after
  a reboot of the machine.
- `/run/vaultcenter-host/`, `/var/lib/vaultcenter-host/`,
  `/var/lib/vaultcenter-mounts/`; the Docker volumes `vaultcenter_state` and
  `vaultcenter_media`.
- **Isolated multi-VTL** mode: no library is created by default, VTLs are
  created in the console.

**Configuration** (under `/etc/vaultcenter-host/config/`, never overwritten if
it exists)

- `targets.conf` (0600), reduced to a comment: it is not read in isolated mode.
- `cold-vtl/vtl-runtime.json` (0600): the pool of mhVTL resources (library,
  drive, iSCSI target) that new VTLs take from, four VTLs' worth. The appliance
  does not start in isolated mode without it.
- `cold-vtl/auth.env` (0600), with `COLD_VTL_PUBLIC_URL=https://<name>:8443`;
  `cold-vtl/tls/fullchain.pem` (0644) and `privkey.pem` (0600);
  `cold-vtl/maintenance/`.
- The isolation receipt, in the state directory of the `vaultcenter_state`
  volume (`isolation-qualified.json`, 0600), if the image ships one.

**Appliance**

- Container `vaultcenter-appliance-1` started by
  `docker compose -f /etc/vaultcenter-host/compose.json up -d`, and the `admin`
  account created in its catalogue.

**Update** (see below): `/var/lib/vaultcenter-host/backups/`, the two marker
files in `/var/lib/vaultcenter-host/`, `compose.json`, the controllers and
units above, the DKMS module.

## Installation steps

1. **Preflight checks**: safety, installation of the missing tools, then
   sizing.
2. **Registry credentials**: none by default, the image is public; with
   `--token`, the site returns a registry, an account and a password; with
   `--registry-token`, the site is not called and the registry is `ghcr.io`.
3. **Docker**: installation from the official repository, unless it is already
   usable.
4. **VaultCenter image**: `docker login` (the script's own configuration),
   download of the target tag, reading of the digest, check that the
   `org.opencontainers.image.version` label is the requested version, then
   `docker logout`. Everything after that uses `…@sha256:<digest>`. The
   download takes minutes: on a terminal its progress is shown; outside a
   terminal or with `--quiet`, one line announces it and another says it has
   finished.
5. **Host files**: extraction from the image to
   `/usr/local/share/vaultcenter/<version>/`.
6. **mhvtl kernel module**: see "DKMS and kernel updates". An `mhvtl` module
   that is already loaded and does not come from this script makes the script
   **refuse**.
7. **Host storage service**: `install.py --new-appliance --isolated`.
8. **Configuration**: directories, `auth.env`, certificate, isolation receipt.
9. **Administrator account**: see below.
10. **Startup**: `docker compose up -d`, wait for the `healthy` state (5
    minutes at most), then reading of `/health` to confirm the version.
11. **Summary**: address, account, password, SHA-256 fingerprint of the
    certificate, path of the log.

### Resuming after an interruption

Run **the same command** again. The script records each finished step in
`/var/lib/vaultcenter-install/state` and resumes at the failed step; each
failure shows the cause, the step and the sentence "run the same installation
command again".

- Steps already done are skipped (`already done (resumed)`). These are
  **always run again**: the preflight checks, the credentials (not needed if
  the image is already downloaded) and the summary.
- Existing configuration files are kept: an `auth.env` or a certificate
  changed by hand is not rewritten.
- If the `vaultcenter-appliance-1` container exists or if the storage service
  is installed, the resumed run no longer checks the ports or the host daemons:
  they are its own.
- **A `--version` other than that of the installation already begun** is
  refused (code 2): a resumed installation keeps the image it began with. The
  message gives both versions. To finish, run again without `--version` or
  with the recorded version. To start over with another version, delete
  `/var/lib/vaultcenter-install/state`; if `/etc/vaultcenter-host` already
  exists, first deal with the case below.
- **`/etc/vaultcenter-host` half created.** `install.py` creates the Docker
  volumes (empty), then the directory, writes `compose.json` into it, then
  marks the volumes; it refuses to run over an existing configuration. If the
  script stopped during this step, the resumed run stops (code 4) without
  deleting anything:
  - directory present **without** `compose.json`: the volumes are still empty,
    and the message names the directory. Delete it yourself
    (`rm -r /etc/vaultcenter-host`), then run the same command again;
  - `compose.json` present although the step is not finished: the Docker
    volumes have already been marked for a new appliance, and `install.py`
    would refuse them ("The state volume is not empty. Refusing adoption.").
    The script stops with this message:

    ```text
    the installation of the host storage service stopped part-way during an earlier run: /etc/vaultcenter-host/compose.json exists although the step is not finished, and the host installer refuses to run over an existing configuration. Nothing has been deleted. Concerned: the /etc/vaultcenter-host directory and the Docker volumes vaultcenter_state and vaultcenter_media. Deleting them is your decision: do so only if this appliance has never started and they hold nothing worth keeping.
    To resume: after deleting them yourself (rm -r /etc/vaultcenter-host ; docker volume rm vaultcenter_state vaultcenter_media), run the same installation command again
    ```

    Nothing is deleted by the script, and it will not do it for you: on a
    machine where an appliance has already run, these volumes are its
    catalogue and its media. Only delete them if this installation has never
    started.
- If the `admin` account was already created during an earlier run, its
  password is not shown again (see below).

## The administrator account

The script creates the local `admin` account with a random 24-character
password, passed to `cold-vtld create-user admin --role admin` on standard
input. It is shown **only once**, in the final box, and never written to the
log or to a file.

The password only exists in the memory of the script, from the creation of the
account to the final box. Two cases make it unknown, and the final box says so
(`Password: unknown`):

- the **Startup** step fails after the account is created: the failure message
  warns that the password will not be shown when resuming;
- the account already exists when the script tries to create it (`cold-vtld`
  answers `username already exists`, after a run that stopped just after the
  creation): the step is taken as done, and the existing password is not
  changed.

If the password was not written down (box lost, or one of the two cases
above), reset it with `cold-vtld reset-password`, described in
[authentication.md](authentication.md) ("Initialisation and local recovery"):
the command re-enables the account, revokes its sessions and its tokens, and
reads the password on standard input (at least 12 characters). From a root
shell on the host, with the same call as the script:

```bash
read -rs -p 'New password: ' vaultcenter_password
printf '\n'
printf '%s\n' "$vaultcenter_password" | docker exec -i vaultcenter-appliance-1 \
  /usr/local/bin/cold-vtld reset-password admin
unset vaultcenter_password
```

The password goes through standard input (`printf` is a shell builtin: it
appears neither in the history nor in the process list). The command runs in
the running appliance, as [authentication.md](authentication.md) describes.
This procedure has not yet been verified on a running appliance.

The command prints `Local account updated.`. To create **another
administrator** instead of resetting `admin`, replace the end of the command
with `/usr/local/bin/cold-vtld create-user <name> --role admin` (names of 3 to
64 lowercase letters, digits, `.`, `_`, `-`).

## The certificate

The script generates a **self-signed** certificate (RSA 2048, valid for **825
days**) in `/etc/vaultcenter-host/config/cold-vtl/tls/fullchain.pem` and
`privkey.pem`. Its subject is `CN=<full name of the machine>`; the alternative
names are that name (`DNS:`) and every global IP address of the machine
(`IP:`). When the full name is unusable (`localhost`, a name without a
domain), the address of the default route serves as the name and in the URL,
and the summary says so. The summary gives the SHA-256 fingerprint: compare it
in the browser before accepting the certificate.

The certificate is neither renewed nor replaced by an update. To replace it
with a certificate trusted by the clients: install the certificate and its key
at the same paths (key in mode 0600), check `COLD_VTL_PUBLIC_URL` in
`config/cold-vtl/auth.env`, then restart the appliance in a maintenance window.
See [docker.md](docker.md), "Operator configuration".

## The isolation receipt

The isolation receipt (`isolation-qualified.json`) is the record that an
isolation qualification was carried out on a specific `vtl_runtime.py`: it
carries its `runtime_sha256` and `isolation_verified`. The appliance refuses to
start the workers, and therefore the **S3 exports**, without a receipt that
matches its `vtl_runtime.py`. The qualification is a separate procedure; the
script qualifies nothing.

At installation, the script installs the receipt shipped with the version
**when there is one** and it matches the `vtl_runtime.py` of that version (same
sha256, `isolation_verified` true). Otherwise it installs nothing, records
`isolation_receipt=missing`, and the summary warns that S3 exports stay
refused until a receipt is installed.

## Updating

The same command as the installation, on an installed appliance. The script
runs the safety checks, gets the credentials, downloads the target image
(`--version`, otherwise `latest`), extracts the host files from it, then hands
over to the `update.py` **of the new version**, which does the rest. It does
not reinstall Docker (it refuses if Docker is unusable). The previous state of
the script (`state`) is ignored. The sizing thresholds of a new installation do
not apply to an update.

**Ctrl-C during the update.** While `update.py` runs, the script does not stop
on SIGINT, SIGTERM or SIGHUP: `update.py` receives the same signal, undoes what
it has begun (rollback) and gives its result; the script waits for it, shows
the summary, reports that a signal was received and exits with the code of
that result (5 after a successful rollback), not with 130.

When the kernel module sources change, the appliance is **stopped** during the
update (up to a 5-minute grace period) before the module is replaced.

Order of `update.py`:

1. Checks that change nothing: same digest as the installed one (nothing to
   do, exit 0), version read from `/health` (the appliance must be started and
   healthy), target version not lower than the installed one, isolation
   receipt, restart conditions, kernel module.
2. Active VTLs put into maintenance, then backup.
3. Step 5, kernel module (if the sources have changed): `dkms build` and
   `dkms install --force` for the running kernel, check that DKMS reports the
   new version `installed` (otherwise the step fails), then the same
   installation for every other kernel that had the old version (a failure
   there is a warning, with the command to run before rebooting on that
   kernel). Step 6, host files (controllers, units, `vaultcenter-mhvtl.service`
   enabled, restart of the storage service). Step 7, container (new digest in
   `compose.json`, `up -d --force-recreate`). Step 8, verification (healthy
   container, `/health` at the target version, the same active VTL units as
   before).
4. Finalisation: removal of the old DKMS module, then a check that the new one
   is still installed and that `modinfo -k <kernel> mhvtl` finds it (otherwise
   a forced reinstallation; if that is not enough, the summary says the module
   will be missing at the next reboot and gives the command), leaving
   maintenance, deletion of the old backups.

### What refuses an update

A refusal (code 3) changes nothing and lists its causes:

- a previous update that did not finish (`update-in-progress.json` marker, see
  "Rollback");
- another update running (lock);
- target version older than the installed one: going back to an older version
  is not done by an update;
- `queued` or `running` jobs in the catalogue; active VTL workers; storage of a
  VTL in a phase other than `ready` (a draft VTL, `unconfigured`, does not
  block); catalogue unreadable in read-only mode;
- maintenance of a VTL in the `entering`, `leaving` or `failed` state;
- an installed, qualified isolation receipt that does not match the
  `vtl_runtime.py` of the new version, with no receipt shipped for it (the
  workers would no longer start). Without a qualified receipt installed, the
  update goes through with a warning: S3 exports stay refused as before;
- installed `mhvtl` module unknown to DKMS for the running kernel (it could
  not be put back in case of failure);
- less than 2 × the size of the catalogue (WAL included) + 200 MiB free under
  `/var/lib/vaultcenter-host/backups`;
- files missing from the extracted host files;
- appliance not started, or whose `/health` cannot be read;
- invalid image (not designated by its `name@sha256:<64 digits>` digest);
- script not run as root.

### The maintenance guard and the question

The script puts each active VTL into maintenance through the existing
controller, which checks the guard (the lock of the backup server). It only
takes out of maintenance the VTLs that **it put** into maintenance itself; a
VTL already in maintenance is left as it is.

- **VTL without a maintenance guard** (the controller does not answer): the
  script cannot know that no backup is running. It refuses, unless `--yes` is
  given, and neither enters nor leaves maintenance for that VTL.
- On a terminal and without `--yes`, when this is **the only** reason for the
  refusal, the script shows the causes and asks the question "No backup job is
  using these libraries right now: do you confirm?". Answering `y` runs the
  update again with `--yes`. Without a terminal, `--yes` is required from the
  start (exit 2): the script does not ask for it afterwards, so you must have
  checked before passing it.
- **Veeam VTL**: the guard is local, VaultCenter does not see a running Veeam
  job. Check in Veeam that no job is using the library, then confirm: `--yes`
  asserts that no Veeam job is running. The VTL is then put into maintenance
  with an idle confirmation.

### What is backed up, where, how many

`/var/lib/vaultcenter-host/backups/<YYYYMMDD-HHMMSS>-<installed version>/`
(0700) contains: `catalog.db` (consistent SQLite copy, integrity checked),
`etc-vaultcenter-host.tar` (all of `/etc/vaultcenter-host`), `host-files.tar`
(host controllers and units), `image` (the old digest), `version`,
`mhvtl.sha256`, `isolation-qualified.json` (if a receipt is installed) and
`module-change.json` (if the module is changed). The **three** most recent
backups are kept, the new one included.

The **media volumes are not backed up**: the update does not change them.

### Results and exits

| Result | Code | What it means |
| --- | --- | --- |
| `updated` | 0 | Update verified and finalised. If VTLs remain in maintenance, the summary names them: take them out of maintenance in the console. Also read the "Next steps and notes" section when it is there (old module not removed, module to reinstall, backups not pruned). |
| `unchanged` | 0 | The target image is already installed. |
| `refused` | 3 | Nothing has been changed; causes listed. |
| `rolled_back` | 5 | A step failed, the backup is restored, the old version runs. **The VTLs the update had put into maintenance stay there**, and the summary names them: taking them out is the operator's decision. |
| `rollback_failed` | 6 | The restoration failed at the step given; nothing else is attempted. Follow the manual rollback. |

The result of `update.py` is a JSON object (last line of its standard output,
copied to the log): `result`, `from`, `to`, `reasons`, `warnings`, `backup`,
`step`, `confirmable`, `still_in_maintenance` and `maintenance_left`.
`still_in_maintenance` is always present: it is the list of the VTLs the update
put into maintenance that are still there, each with its reason;
`maintenance_left` is true exactly when that list is empty (so also when the
update put no VTL into maintenance). The summary shows the reasons and the
warnings of every result, and the line of the "Appliance update" step only
carries the success mark for `updated` and `unchanged`.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success (installation, update, or nothing to do). |
| 2 | Usage: unknown or missing option, forbidden combination, no terminal without `--yes`, resuming with a `--version` other than that of the installation already begun. |
| 3 | Refusal: the machine is not suitable, former installation, inconsistent state, cancelled at the confirmation, update refused. Nothing has been changed. |
| 4 | A step failed (network, apt, Docker, DKMS…). The cause and the command to resume are shown. |
| 5 | Update cancelled, previous version restored. |
| 6 | Rollback failed. |
| 129 | Interrupted by SIGHUP (terminal closed). |
| 130 | Interrupted by Ctrl-C (SIGINT). |
| 143 | Interrupted by SIGTERM. |

Codes 129, 130 and 143 only apply outside the update proper: while `update.py`
runs, a signal is recorded and the script exits with the code of the result
(see "Updating").

If the result announced by `update.py` and its exit code do not agree, the
script keeps the more severe code. `update.py` itself exits with 0, 3, 5 or 6.

## Rollback

### Automatic

A failure of steps 5 to 8 restores the backup, in reverse order: stopping the
container, `/etc/vaultcenter-host` (so `compose.json` and the old digest),
`catalog.db` **together with** the old image (a migration of the new version
makes the database unreadable by the old one) and the isolation receipt, host
files and restart of the storage service, previous kernel module (for the
running kernel, and for the other kernels that had it), recreation of the
container, health and version check. A failure of the restoration stops there,
with the path of the backup.

### The two marker files

The markers are in `/var/lib/vaultcenter-host/`.

- **`update-in-progress.json`**: written before step 5. An update that has not
  passed step 8; a rollback is possible. It holds the backup, the versions, the
  target image, the step reached and what the finalisation will have to do. As
  long as it exists, every normal run is refused, except `--resume-rollback`.
- **`update-verified.json`**: the same file, **renamed** by the last act of
  step 8. Its name, and nothing it contains, says that the update is verified:
  it is **never** rolled back, and the next run completes its finalisation
  (recorded VTLs taken out of maintenance, old module removed, cleanup).

### `--resume-rollback` and `--force-rollback`

After an interruption (power cut, machine rebooted during the update), the
same command is refused and names the marker. To restore the recorded backup,
run again with **the same `--version`**:

```bash
sudo bash install.sh --token <TOKEN> --version X.Y.Z --resume-rollback
```

`--resume-rollback` first checks that the backup is complete and its catalogue
intact, before stopping anything.

- It does **nothing** if `update-verified.json` exists, with or without
  `--force-rollback`: restoring the old catalogue would lose what the
  appliance has written since.
- It also refuses to restore an appliance that **runs in good health in the
  target version** with the target image (healthy container, `/health` at the
  target version) while the marker has not been renamed: it may already have
  written data to the new catalogue.
- When `compose.json` names the target image, it **waits for the health of the
  container** (up to 5 minutes) before concluding: after a reboot of the
  machine, the container is "starting" for a while, which does not mean that
  the new version is not working. If the health remains **undetermined** (the
  container reports itself neither `healthy` nor `unhealthy`, for example
  because it is stopped, or `/health` does not answer), it refuses too, and
  says so: check the appliance (`docker ps`), then run again.
- **`--force-rollback`** lifts these two refusals: it stops the appliance,
  restores the backup (catalogue, configuration, host files, kernel module) and
  puts the previous version back, even if the appliance is healthy in the
  target version, and therefore **loses what has been written since the
  update**. It never lifts the first one (verified update).

A normal run refused because of the marker never says to delete it: it names
`--resume-rollback` (and `--force-rollback` when needed), says what it will do
and, if a previous rollback failed, at which step.

After a successful rollback, the marker is deleted and the VTLs the update had
put into maintenance stay there. After a resumed rollback, their state is read:
those you have taken out of maintenance in the meantime are not reported.

**Without network access.** `install.sh` needs the registry to find the image
and the files of the target version. If the machine no longer has network
access, the rollback can be started directly with the `update.py` already
extracted during the interrupted update:

```bash
sudo python3 /usr/local/share/vaultcenter/<target version>/host/update.py --resume-rollback
```

(add `--force-rollback` in the cases above). Its last line of output is the
result in JSON; its exit code is 0, 3, 5 or 6.

### Keeping an unrecorded update

A rare case: `update-in-progress.json` exists, but the appliance runs in good
health in the target version (the last step could not be recorded, or it
failed and the rollback failed too). Every run is then refused, and so is
`--resume-rollback` alone. To **keep** the version in place rather than go
back:

1. Read the marker (`cat /var/lib/vaultcenter-host/update-in-progress.json`):
   the step reached, the backup, the VTLs put into maintenance (`entered`), the
   old module (`old_module`), and `rollback_failed` if a rollback failed.
2. Check the appliance: `docker inspect --format '{{.State.Health.Status}}'
   vaultcenter-appliance-1` answers `healthy`, `/health` gives the target
   version, and in the console each VTL has its library, drives and target
   active. A failed "verification" step often means that a VTL unit did not
   come back: fix that first.
3. Check the module: `dkms status -m mhvtl` and `modinfo -k "$(uname -r)"
   mhvtl`. If the old version (`old_module`) is still listed and the new one is
   installed, remove the old one (`dkms remove -m mhvtl -v <old_module>
   --all`), then check `modinfo` again.
4. Take the VTLs still in maintenance out of it in the console.
5. Only then, delete `/var/lib/vaultcenter-host/update-in-progress.json`. The
   backup stays in `backups/`; it must no longer be restored.

### Manual procedure from a backup

For a `rollback_failed`, or when the backup is that of a failure that
`--resume-rollback` cannot handle. **After a verified update, do not restore:
the old catalogue is the one from before the update, and everything written
since (cartridges, jobs, histories) is lost.**

Variables, to adapt (the directory is the one given by the summary, by
`update-in-progress.json` or by `ls /var/lib/vaultcenter-host/backups/`):

```bash
BACKUP=/var/lib/vaultcenter-host/backups/<YYYYMMDD-HHMMSS>-<version>
COMPOSE=/etc/vaultcenter-host/compose.json
STATE_DIR=$(python3 -c 'import json; print(json.load(open("/etc/vaultcenter-host/storage.json"))["state_dir"])')
```

1. Check the backup (all the files listed above, and a catalogue that answers
   `ok`):

   ```bash
   ls -l "$BACKUP"
   python3 -c 'import sqlite3, sys; print(sqlite3.connect("file:" + sys.argv[1] + "?mode=ro", uri=True).execute("PRAGMA integrity_check").fetchall())' "$BACKUP/catalog.db"
   ```

2. Stop the appliance:

   ```bash
   docker compose -f "$COMPOSE" stop appliance
   ```

3. Restore the host configuration (the names in the archive are relative to
   `/`), which puts back `compose.json` and the old digest:

   ```bash
   tar -xpf "$BACKUP/etc-vaultcenter-host.tar" -C /
   ```

   The automatic rollback also deletes the files in `/etc/vaultcenter-host`
   that the archive does not contain (added by the new version): compare with
   `tar -tf "$BACKUP/etc-vaultcenter-host.tar"` and remove them.

4. Restore the catalogue: first remove the `-wal`, `-shm` and `-journal` files,
   so that no journal of the new version is applied to the restored file, then
   copy while keeping the owner and the mode of the current one:

   ```bash
   rm -f "$STATE_DIR"/catalog.db-wal "$STATE_DIR"/catalog.db-shm "$STATE_DIR"/catalog.db-journal
   cp "$BACKUP/catalog.db" "$STATE_DIR/catalog.db.restore"
   chown --reference="$STATE_DIR/catalog.db" "$STATE_DIR/catalog.db.restore"
   chmod --reference="$STATE_DIR/catalog.db" "$STATE_DIR/catalog.db.restore"
   mv "$STATE_DIR/catalog.db.restore" "$STATE_DIR/catalog.db"
   ```

   Isolation receipt: if there is a `$BACKUP/isolation-qualified.json`, copy it
   to `$STATE_DIR/` (0600); otherwise delete `$STATE_DIR/isolation-qualified.json`.

5. Restore the host files, then remove those the new version added (compare
   with `tar -tf "$BACKUP/host-files.tar"`) and restart the storage service:

   ```bash
   tar -xpf "$BACKUP/host-files.tar" -C /
   systemctl daemon-reload
   systemctl restart vaultcenter-storage-host
   ```

6. If `$BACKUP/module-change.json` exists, the kernel module was changed: it
   gives `old_version`, `new_version` and `kernel`. Put the old one back.
   Unloading (`modprobe -r`) only applies if the update had already reloaded
   the module (`"reloaded": true` in that file) and it is loaded; the
   `dkms remove` of the new version only applies if `dkms status -m mhvtl`
   lists it:

   ```bash
   dkms status -m mhvtl                    # which versions DKMS knows
   modprobe -r mhvtl                       # if loaded; the appliance is stopped
   dkms remove -m mhvtl -v <new_version> --all   # if <new_version> is listed
   # if <old_version> is not "installed" for <kernel>:
   dkms install --force -m mhvtl -v <old_version> -k <kernel>
   # likewise for each kernel in "other_kernels"
   modprobe mhvtl opts=0
   modinfo -k <kernel> mhvtl | head -3
   cp "$BACKUP/mhvtl.sha256" /var/lib/vaultcenter-install/mhvtl.sha256
   ```

7. Recreate the appliance with the old digest, wait until it is healthy and
   check the version (the one in the `$BACKUP/version` file):

   ```bash
   docker compose -f "$COMPOSE" up -d --force-recreate appliance
   docker inspect --format '{{.State.Health.Status}}' vaultcenter-appliance-1
   curl -fsSk https://127.0.0.1:8443/health
   cat "$BACKUP/version"
   ```

8. Delete the marker `/var/lib/vaultcenter-host/update-in-progress.json`. The
   VTLs are still in maintenance: take them out of it in the console, after
   checking the appliance.

If the marker file is **unreadable** (the script says so and does not change
it), examine it by hand: `update-in-progress.json` reports an update that did
not finish, `update-verified.json` a verified update whose finalisation is
still to be done (take the VTLs out of maintenance, remove the old module with
`dkms status` then `dkms remove`).

## DKMS and kernel updates

The `mhvtl` module is installed through DKMS: an `mhvtl` package of version
`vc-<first 12 digits of the sha256 of the sources>`, with `AUTOINSTALL="yes"`.
With every new kernel, DKMS rebuilds the module for it **if it finds its
headers**; `/etc/modules-load.d/vaultcenter.conf` loads it at boot, together
with `sg`. Three things make sure that a kernel update followed by a reboot
does not leave the appliance without its module:

- **The headers follow the kernel.** The script installs
  `linux-headers-<running kernel>` and the metapackage of the kernel flavour:
  `linux-headers-cloud-amd64` for a `…-cloud-amd64` kernel (cloud images),
  `linux-headers-rt-amd64` for `…-rt-amd64`, `linux-headers-amd64` for
  `…-amd64`. Every `apt upgrade` that brings a kernel then brings its headers.
  For a kernel of another flavour (non-Debian kernel), the script warns: you
  have to install `linux-headers-<new kernel>` yourself before rebooting.
- **The module is built for every installed kernel**, not only the running
  one: at installation, for every kernel that has `/lib/modules/<kernel>/build`;
  during a VaultCenter update that changes the module, for every kernel that
  had the old version, before that version is removed.
- **A boot-time guard.** `vaultcenter-mhvtl.service` runs before Docker: it
  tries `modprobe mhvtl opts=0`; if the module is missing for the running
  kernel, it runs `dkms autoinstall -k <kernel>` then reloads the module, and
  loads `sg`. If the headers are there, a missing module means a longer boot
  (one to two minutes of compilation, 15 minutes at most), not an outage. It
  logs what it did: `journalctl -u vaultcenter-mhvtl`. If the headers are
  missing, it fails without blocking the boot, and its message gives the
  commands.

The fingerprint of the sources (and so the DKMS version) only changes if the
module sources change: the archive is built reproducibly when the image is
built. A VaultCenter update whose module is unchanged does not stop the
appliance to rebuild it.

To check:

```bash
dkms status -m mhvtl     # mhvtl/<version>, <kernel>, x86_64: installed (one line per kernel)
modinfo mhvtl | head -3
modinfo -k <other installed kernel> mhvtl | head -3
lsmod | grep -E 'mhvtl|^sg'
systemctl is-enabled vaultcenter-mhvtl.service
```

If the build fails: read `/var/lib/dkms/mhvtl/<version>/build/make.log`,
install the missing headers, then `dkms autoinstall -k <kernel>` (or `dkms
build` then `dkms install -m mhvtl -v <version> -k <kernel>`), `modprobe mhvtl
opts=0`, and start the appliance
(`docker compose -f /etc/vaultcenter-host/compose.json up -d`). A VaultCenter
update whose module sources change builds and loads the new version itself,
keeps the old one for the rollback, and checks after its removal that the
module is still installed (`dkms status`, `modinfo -k`). If the summary says
that the module will be missing at the next reboot, run the command it gives
(`dkms install --force -m mhvtl -v <version> -k <kernel>`) before any reboot.

## Log

`/var/log/vaultcenter-install.log` (0600, root): each command run and its
output, the progress of the update (prefix `update:`), the decisions of the
script, the signals received during an update. The tokens, the administrator
password and the password of a proxy URL are replaced in it by `***`; the
administrator password is never written. The details of a DKMS build are in
`/var/lib/dkms/mhvtl/<version>/build/make.log`.

## Known limitations

The installer and the updater are covered by automated tests with simulated
system tools. Their behaviour on real machines (new installation, update,
broken update, reboot after a kernel update, on Debian 12 and 13) has not been
fully verified yet. Not yet observed in particular: what DKMS does with two versions
of the same module (`--force`, removal of the old one), the
`vaultcenter-mhvtl.service` guard after a kernel update with and without
headers, and the health state Docker reports for a stopped container (it
decides whether `--resume-rollback` requires `--force-rollback` after a power
cut in the middle of the container recreation).
