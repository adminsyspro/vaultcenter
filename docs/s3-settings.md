# S3 configuration from the console

Open **Manage VTLs → Open → Storage → S3**, with an administrator account.
Each VTL has its own S3 connection.

1. Choose the S3 provider, then enter the HTTPS endpoint, the region and the prefix.
2. Enter the access key and its secret. If the access key is unchanged, leaving
   the secret empty keeps the saved one. A new access key requires a new
   secret.
3. Click **Check and save**. The server checks the keys with the provider
   before saving; if that fails, the existing configuration stays active.
   The next S3 accesses use the saved configuration immediately, without
   restarting the API, PBS or Keycloak.
4. Create the export bucket (OVH Cold Archive v1), or choose an existing bucket
   in the advanced options.

Viewer accounts have no access to the settings or credentials.
Built-in archiving/recall remains limited to the OVH Cold Archive v1 endpoint
in Roubaix.

## Providers and capabilities

The selector is filled from the API provider registry:

| Provider | Identifier | Functions |
| --- | --- | --- |
| OVHcloud Cold Archive v1 | `ovh_cold_archive_v1` | S3, inventory, archiving and recall of the entire bucket on the Roubaix endpoint |
| S3 compatible | `s3_compatible` | S3 and inventory of current objects, without built-in Glacier archiving or recall |

The choice is stored with the configuration. It defines the capabilities and
the initial values of the form. Changing provider empties the bucket and the
credentials entered; nothing changes on the appliance until it is applied.
The common menu is called **S3 storage**. The archiving/recall sections are
offered only for a provider that has this integration.

Older configurations without `provider` remain readable: the exact Roubaix
endpoint is recognised as OVH Cold Archive v1, the others as S3 compatible.
Explicitly saving that same choice does not trigger a resynchronisation. An
unknown value or a provider/endpoint inconsistency is refused; selecting S3
compatible on the Cold Archive v1 endpoint therefore does not make it possible
to bypass the restrictions of an archived bucket.

Other Glacier providers will need an archiving, recall and access-control
adapter before announcing these capabilities. The S3-compatible mode does not
simulate these functions and does not make objects archived at another
provider accessible.

## Changing destination

The provider, the endpoint, the region, the bucket and the prefix identify the
destination. When it changes, the form asks for a confirmation and the server:

- keeps the local cartridges, the old S3 objects, the restores already
  performed and the job history;
- clears the cache of remote generations and resets the local catalogue states
  to “Writable”, without changing the cartridge files;
- synchronises the cartridges with the new bucket in the background, without
  maintenance. Only a cartridge being synchronised waits before it can be
  sealed or sent. Every application changes the configuration revision.

An old bucket is found again with **Attach an old bucket**: its copies appear
in **S3 copies** without changing the active destination.

Synchronising a cartridge re-reads its local files to compute their SHA-256.
During this read, it holds the media lock of that cartridge (the same as for
sending): at most two cartridges per VTL are synchronised at the same time in
the background, and the other cartridges remain usable.

Applying is refused during a VTL job or an unresolved reintegration.
A change of destination is also refused while an OVH request is pending,
uncertain, or accepted with no observed end. The credentials of the same
bucket can be replaced to restore the connection and OVH tracking.
Two forms open at the same time cannot silently overwrite each other's
changes: a stale revision must be reloaded.

## Secret storage

The `0007_s3_settings.sql` migration adds `s3_settings` and
`s3_reconcile_required`. Saving the settings, invalidating the cache and the
synchronisation markers form a single SQLite transaction.

The secret is kept in the local catalogue, protected by system permissions:
directory **0700**, database and auxiliary SQLite files **0600**.
It is not encrypted separately in the database; protect its backups as well.
Install the `deploy/cold-vtl/s3-settings.conf` drop-in to keep this directory
mode when the service restarts.

The secret does not appear in HTTP responses, jobs or process arguments. The
test passes it to the worker on its standard input. The interface does not
store it in `localStorage` and clears the field after it is applied.
S3 errors from the test are replaced by a generic message.

Before the first save, the values come from the existing environment
(`/etc/cold-vtl/s3.env`, loaded by `cold-vtld.service`). After that, the SQLite configuration
takes precedence: changing only that environment file does not replace a
configuration saved in the UI.

The worker re-reads the configuration at each launch, including when it is
called by the archiving/recall helper. It uses `COLD_VTL_DATABASE`, or
`/var/lib/cold-vtl/catalog.db` by default. An invalid saved configuration
causes a refusal; it does not silently re-enable the old credentials.

## API and tests

| Method | Route | Effect |
| --- | --- | --- |
| GET | `/v1/settings/s3` | Available providers, settings without the secret, revision and number of cartridges to synchronise |
| POST | `/v1/settings/s3/test` | Test of the submitted settings, without saving |
| PUT | `/v1/settings/s3` | New test, then atomic application |

The body of the test and of the application contains `provider`, `revision`,
`endpoint`, `region`, `bucket`, `prefix`, `access_key_id`,
`secret_access_key`. The value `null` for the secret keeps the existing
secret, if the access key is unchanged.
All these routes require an administrator; mutations from a browser session
require the CSRF token.

The tests cover permissions, non-disclosure of the secret, invalid settings,
test failure without change, key rotation, stale revisions, exclusion of
operations and separation of the copy catalogues.

### Extending the registry

`cold-vtl-core/src/s3_provider.rs` centralises the identifiers, the
capabilities, the initial settings and the consistency check with the
endpoint. The API exposes these definitions to the form and keeps the
effective identity in the existing SQLite JSON (no additional schema
migration). The worker validates this identity before any access and reserves
the OVH-specific commands for the Cold Archive v1 adapter. Each job remains
tied to the destination and configuration revision frozen when it started.

## Scheduling and creating buckets

The VTL window (**Manage VTLs → Open → Storage → S3**) reads in three steps:
S3 connection (keys checked with the provider before saving), export bucket
(`vc-<VTL name>-<YYYY-MM-DD>`, UTC date, **Create** or **Change** button) and
“Ready to export”.

The suggested name, for example `vc-veeam-2026-10-13`, has no random suffix.
If it is already known (destination, creation or rotation recorded, or bucket
listed at the provider), VaultCenter suggests `-2`, then `-3`, and so on. A
name already taken by another account makes the creation fail without ever
being adopted; the name stays recorded and the next attempt, for example the
next export, suggests the next variant.

A name entered in the field is never replaced by a new suggestion. It stays
displayed during the creation; once the bucket is created and activated, a line
confirms it, and the field receives a new suggestion only if nothing has been
entered since. As long as the active bucket contains no copy, step ② does not
offer to change it and states that the next exports will go into it;
**Create** remains offered when no bucket is active.

Bucket change: manual, every week (chosen day) or every month (chosen day;
a shorter month uses its last day). The scheduled change happens at the first
export after the start of a new period: the bucket is created and activated,
then the export goes into it. A manual change serves the current period.

Uncertain creation (lost response): VaultCenter asks the provider about the
name. Missing → failure with the reason; present, empty and writable → bucket
activated; present with data → never adopted (“Use another name”); provider
unreachable → new check every 5 minutes, no other creation in the meantime.

After an existing bucket is activated as destination, the cartridges
synchronise with it in the background; only a cartridge being synchronised
waits before it can be sent.

Automatic archiving (OVH Cold Archive v1 only): “N days after a bucket stops
being active”, from 1 to 365 days. Enabling this setting means accepting the
OVH terms. Only buckets created by VaultCenter, which contain at least one copy
and which stopped being active after the setting was enabled, are archived
automatically, with at most one attempt per day; see
[supervised archiving](archive.md).

API: `GET` and `PUT /v1/vtls/{vtl}/storage/schedule` (`revision`, `rotation` =
`manual`, `weekly` or `monthly`, `weekday` from 1 to 7, `month_day` from 1 to
31, `archive_after_days`, `accept_archive_terms`).
