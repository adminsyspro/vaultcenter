# VaultCenter console

Next.js static console adapted from [Studio Admin](https://github.com/arhamkhnz/next-shadcn-admin-dashboard),
upstream commit `a1c7cf365e8c926c73ec3fc5f9978f3b90136078`.
The upstream shadcn Radix Nova components, neutral theme, sidebar pattern and
Geist fonts are retained. Attribution and MIT license: `LICENSE.template`.

## Build

```sh
npm ci
npm run build
```

The complete static site is in `out/`, including locally hosted fonts. Node.js
is needed only to build the UI. No Next.js server, Google Fonts requests,
server actions, authentication demo, or fixture data are deployed.

From the Rust workspace root, serve it with the API:

```sh
COLD_VTL_UI_DIR="$PWD/ui/out" cargo run -p cold-vtl-api
```

All requests use the current origin's `/v1/` API. There are no frontend S3
credentials or endpoint overrides. The views use URL fragments so deep
links and browser history work with the static server.

## Development server

`npm run dev` starts Next with hot reload at `http://localhost:3000` and proxies
`/auth/*`, `/v1/*` and `/health` to a running `cold-vtld` backend:

```sh
COLD_VTL_DEV_API_URL=https://vaultcenter.example.net:8443 \
COLD_VTL_DEV_CA=/path/to/ca.crt \
npm run dev
```

- `COLD_VTL_DEV_API_URL` is the backend origin (scheme, host and port only).
  Without it, the proxy targets `https://127.0.0.1:8443`, the daemon's default
  listener.
- `COLD_VTL_DEV_CA` is the PEM file of a private certificate authority that
  signed the backend certificate. Without it, Node's default trust store is
  used, so a backend with a self-signed or private certificate is rejected
  (HTTP 502 and an error in the terminal). TLS validation is never disabled.

Log in with a local account of that backend; data and actions are real. SSO
opens the backend's own console because its registered OIDC callback belongs to
the appliance.

The proxy checks the browser origin before translating it to the backend
origin and preserves CSRF headers. For HTTP on localhost only, it translates
the backend's `__Host-` session cookie to a development cookie, retaining
`HttpOnly` and `SameSite`; the connection to an HTTPS backend keeps its
certificate validation.

For access from another machine, use HTTPS with `COLD_VTL_DEV_TLS_CERT` and
`COLD_VTL_DEV_TLS_KEY`, and pass `-- --hostname 0.0.0.0` to `npm run dev`.
The certificate must cover the hostname used in the browser. API requests over
HTTP are restricted to localhost; HTTPS preserves the original Secure cookies.
Production builds still export a static site served by Rust.

## Appliance installation

1. Build the UI and `cargo build --release -p cold-vtl-api`.
2. Install `cold-vtld` on the appliance.
3. Copy the contents of `out/` to `/usr/local/share/cold-vtl/ui/`.
4. Install `deploy/cold-vtl/ui.conf` as
   `/etc/systemd/system/cold-vtld.service.d/ui.conf`.
5. Reload systemd and restart `cold-vtld` when no job is active.

Keep the previous binary and UI directory for rollback. Removing the drop-in
and restarting restores the original embedded console. The daemon fails at
startup if an explicitly configured UI directory has no `index.html`.

## Navigation

The main navigation contains Library, Activity, S3 storage and Settings
(Notifications, Users and Profile for administrators, Profile for viewers).
The dashboard focuses on counts, current operations and shortcuts. Storage and
S3 copy dialogs separate their workflows using labelled section selectors.
The sidebar selector switches the current VTL and stores its ID in the fragment
query (`?vtl=…`). Switching resets dialogs and selections, aborts pending requests
and scopes all inventory, job, maintenance and storage operations to that VTL.
Storage includes an administrator-only Configuration section (`#storage/configuration`);
the old `#s3settings` address redirects there. Saving configuration refreshes storage state.
Maintenance service states are always visible; event history expands on demand.
The header avatar opens Profile (`#profile`) and sign-out. Profile contains the
current user's identity, local password change and personal API/CLI tokens.
Users (`#account`) is reserved for administrators and contains user management.

## Behaviors

- Overview metrics come only from the catalogue; virtual capacity is not
  presented as physical disk free space or S3 bucket usage.
- Media search, state filter, barcode sorting and pagination. Multiple selection
  persists across pages and filters, with page/result selection and a hidden
  selection count. Bulk seal/export confirms the eligible cartridge list and
  stops at the first error without automatically retrying uncertain requests.
- Seal/export dialogs, prevention of repeated submissions, API error display.
- Jobs list and details with automatic refresh, including terminal failures.
- Last successful snapshot remains visible during a connection failure;
  mutations are disabled until a successful refresh.
- Light/dark theme, mobile navigation, keyboard-accessible dialogs and tables.
- S3 copies dialog: reconciliation, generation selection, restoration to a new
  directory and integrity result in job details.
- HTTPS with local and Keycloak login, viewer/admin roles and API tokens.
- Managed PBS maintenance, verified cartridge reintegration and rollback.
- OVH archive/recall preparation, confirmation and persistent observations.
- Administrator S3 settings with connection test, secret retention and atomic apply;
  see [s3-settings.md](../docs/s3-settings.md).

## Validation

The build uses the TypeScript compiler API (`experimental.useTypeScriptCli: false`),
compatible with the pinned TypeScript 5 dependency. Type checking remains enabled.

```sh
npm run typecheck
npm run build
npm run test:restoration
npm run test:account
npm run test:vtl-selector
```

The restoration test requires Chromium (`npx playwright install chromium`, or
set `COLD_VTL_CHROMIUM` to an existing executable). It uses a local static server
and a simulated API. See [restoration.md](../docs/restoration.md).

Keep template components in `src/components/ui/` intact; compose product
features in `src/app/_components/`. The upstream template's full demo routes,
user accounts and fake analytics are intentionally not part of this console.

## Isolated LTO demonstration

```sh
npm run dev:demo
```

Open http://localhost:3000/#media. No certificate or backend is required.
The **Local demo** badge identifies simulated data. Choose writing,
reading, loaded, idle, empty, error, unknown positions, stale or missing source
from the scenario selector. Catalogue usage is explicitly separated from local
spool volume and activity; this is not PBS job completion.

All `/auth`, `/v1`, `/health` and `/__demo` requests are handled locally. Unknown
routes return a local error; there is no upstream fallback or OIDC redirect.
Seal/export actions mutate in-memory data, reset when the server restarts.
No real credentials are needed. Demo listening is limited to loopback.
The `.next-demo` build directory is separate from the production build/tests.

The ordinary `npm run dev` proxies to a real backend (see above).
Use `dev:demo` for isolated previews and mutation tests.

```sh
npm run test:media-telemetry
npm run test:dev-demo
```

The reader also shows a rolling throughput chart (up to 60 observations over
two minutes since opening this view). It plots explicit write-rate counters
when supplied, otherwise it labels the series as local spool growth. Missing
measurements create gaps; changing cartridge or VTL resets the series.
Run `npm run test:media-throughput` for chart regression checks.
