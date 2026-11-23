# Operator quick start

## Web console

Open `https://vaultcenter.example.net:8443/` and sign in with a local account or
through OpenID Connect (for example Keycloak). If the appliance uses a private
certificate authority, trust it in the browser first; see
[authentication.md](authentication.md).

The dashboard shows the daemon health, all discovered virtual cartridges, their
lifecycle state, and recent jobs. The **Cartridges** view supports barcode
search, state filtering, sorting and pagination. A writable cartridge can be
sealed, then exported; the seal dialog asks the operator to confirm that PBS has
finished using it.

Use **Maintenance → Enter maintenance** to reserve the PBS drive and stop the
tape data plane. After the jobs finish, use **Resume service**. See
[maintenance.md](maintenance.md) for recovery and prerequisites.

**Cartridges → S3 copies** lists reconciled generations and restores a chosen
generation to a new directory. The job detail shows the destination and
integrity result. See [restoration.md](restoration.md) for maintenance commands,
S3 reconciliation and manual reintegration.

To put a restored generation back into PBS, use **Cartridges → S3 copies →
Reintegrate into PBS**. The operation reserves PBS, retains the current
cartridge, installs the verified restore and resumes services. Recovery and
rollback are also available from **Maintenance**. See
[reintegration](reintegration.md).

The **Activity** view supports filtering and opens job details, including worker
results and errors. Data refreshes every five seconds while the tab is visible.
The last snapshot remains visible on connection failure and mutations are
disabled until reconnection. Theme selection supports light and dark modes.

The Next.js UI is built as static files and served by Rust. Configure
`COLD_VTL_UI_DIR=/usr/local/share/cold-vtl/ui` using the supplied
`deploy/cold-vtl/ui.conf` systemd drop-in. See `ui/README.md` for installation.

## S3 configuration

Use **S3 storage → Configuration** to edit the bucket, endpoint, region,
prefix, access key and secret. Test the connection, then apply it. An empty
secret field retains the saved secret if the access key is unchanged. Switching
destination requires reconciliation before the next export. See [s3-settings.md](s3-settings.md).

## Email notifications

Administrators can configure **Notifications** for important operation outcomes.
Email is disabled by default. See [SMTP setup, events and delivery recovery](notifications.md).

## CLI

Run the CLI directly on the appliance:

```bash
export COLD_VTL_API_URL=https://vaultcenter.example.net:8443
export COLD_VTL_CA_CERT=/etc/cold-vtl/tls/ca.crt
cold-vtl login --username admin
cold-vtl status
cold-vtl media list
cold-vtl jobs list
```

With the data plane in maintenance, seal and export:

```bash
cold-vtl maintenance enter
cold-vtl media seal CVT003L8
cold-vtl media export CVT003L8
cold-vtl jobs watch <job-id>
cold-vtl maintenance leave
```

Reconcile existing S3 copies before choosing a generation to restore:

```bash
cold-vtl media reconcile CVT001L8
cold-vtl jobs watch <reconcile-job-id>
cold-vtl media generations CVT001L8
cold-vtl media restore CVT001L8 --generation 2 --destination cvt001-copy
cold-vtl jobs watch <restore-job-id>
```

Use a remote daemon with either the option or environment variable:

```bash
cold-vtl --api https://vaultcenter.example.net:8443 status
export COLD_VTL_API_URL=https://vaultcenter.example.net:8443
cold-vtl media list
```

## Service operations

```bash
systemctl status cold-vtld
journalctl -u cold-vtld
systemctl restart cold-vtld
```

Runtime configuration is defined in `cold-vtld.service`. S3 credentials initially come from `/etc/cold-vtl/s3.env` (0600). After saving
in the UI, the protected SQLite configuration takes precedence. Never print
credentials in logs or diagnostics; protect catalogue backups too.

## Safety boundary

An export uploads immutable segments, verifies every uploaded object, and only
then publishes the manifest. Export does not archive the bucket. Legacy OVH
Cold Archive v1 is a separate bucket-wide action with minimum billing and is
available in **S3 storage**, with preparation and explicit cost confirmation.
Recall is available on the same page without stopping PBS. See [archive.md](archive.md).

HTTPS and local/OIDC authentication are enabled. Viewer accounts cannot
launch mutations. The historical HTTP listener on 8080 is closed.
When an external identity provider is configured, keep a local recovery
administrator account.

## Tape storage path

In **Cartridges → Storage configuration**, an administrator chooses the path of
each VTL. This path is permanently locked as soon as it contains data. Services
and mounts are independent in isolated mode; a library whose storage is ready
still requires an explicit PBS activation.

See [the tape storage guide](tape-storage.md) for installation, migration
(limited to unused appliances) and recovery by UUID.

## Throughput and PBS connection

See [Throughput limits](bandwidth.md) and [PBS connection per VTL](pbs-connection.md).
