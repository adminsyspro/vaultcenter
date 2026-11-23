# Local authentication, OIDC and HTTPS

The service requires authentication for all `/v1/*` routes.
`/health` remains public (process status only). The console offers a
local account and, if configured, OIDC sign-in. The migration `0003_auth.sql`
adds accounts, sessions, tokens and OIDC transactions to the SQLite catalogue.

## Daemon configuration

Install `deploy/cold-vtl/auth.env.example` as `/etc/cold-vtl/auth.env`
(0600), adapt the URLs and paths, then install the `auth.conf` drop-in.
The TLS key and the OIDC client secret are 0600 files under `/etc/cold-vtl`.
The public certificates can be 0644. The client secret is read from
`COLD_VTL_OIDC_CLIENT_SECRET_FILE` and is never sent to the browser.

`COLD_VTL_PUBLIC_URL` must be an exact HTTPS origin, without a path. The
callback allowed in Keycloak is exactly:

```text
https://vaultcenter.example.net:8443/auth/oidc/callback
```

The native TLS settings are `COLD_VTL_TLS_CERT` and `COLD_VTL_TLS_KEY`.
An HTTPS proxy is also possible, with an HTTP listener **on loopback**
and a public HTTPS URL. For isolated tests only, HTTP requires
`COLD_VTL_ALLOW_INSECURE_HTTP=true` and a public HTTP URL. There is no
mode that disables authentication.

Remove the `COLD_VTL_OIDC_*` variables to work with local accounts
only. Keycloak discovery is performed at each sign-in,
not at startup: a provider outage does not block the local account.
`COLD_VTL_OIDC_CA_FILE` adds a private CA to the trust store of the OIDC
client, without disabling TLS validation.

## Initialisation and local recovery

From a root shell on the VTL, the daemon reads the password on stdin:

```bash
read -rs -p 'Password: ' vtl_password
printf '%s\n' "$vtl_password" | cold-vtld create-user admin --role admin
unset vtl_password
```

Use `cold-vtld reset-password admin` in the same way to recover an
existing account. This command re-enables the account and revokes its sessions and
tokens. It uses a separate administration connection and does not change
active jobs. Do not pass the password as a process argument.

Local names have 3 to 64 lowercase letters, digits, `.`, `_`, `-`.
Passwords have 12 to 1024 bytes and are hashed with Argon2id.
The bootstrap creates no default account or password.

## Permissions and sessions

| Function | Read-only | Administrator |
| --- | --- | --- |
| Inventory, known generations, job history | Yes | Yes |
| Sealing, export, reconciliation, restore | No | Yes |
| Management of own tokens / local password | Yes | Yes |
| Account creation and deactivation | No | Yes |

OIDC groups must exactly match the configured paths. No
authorised group means **sign-in refused**. The administrator group takes
precedence if both are present. OIDC accounts are identified by
`issuer + sub`, never merged automatically with a local account by name
or email address. Their role comes from the signed ID token, after validation
of the signature, issuer, audience, expiry and nonce.

The OIDC flow uses Authorization Code, PKCE S256, a random state bound to the
browser and a single-use transaction, valid for five minutes.
HTTP redirects of the server-side client are refused. Only HTTPS
endpoints are accepted. The provider's tokens are not persisted.

Browser sessions last eight hours: opaque `HttpOnly`,
`Secure`, `SameSite=Lax` cookie, with the `__Host-` prefix over HTTPS. Mutations with a cookie
require a CSRF token; cross-origin requests are refused.
Session and token secrets are stored as SHA-256 in the
catalogue. Local sign-ins are rate-limited by IP and username;
password errors do not reveal whether the account exists.

Signing out revokes the current VaultCenter session. Changing the
password revokes all sessions and all tokens of the account. A local
change of role or a deactivation also revokes the affected access. The
console forbids changing your own permissions and deleting the
last active local administrator.

**SSO limitation:** signing out does not close the Keycloak session. Group
changes or deactivations in Keycloak take effect at the next
OIDC sign-in; there is not yet back-channel logout or continuous
checking with the provider. To cut VTL access immediately,
deactivate the account in "Users". Otherwise, existing sessions can
last eight hours and API tokens until they expire.

## CLI

```bash
cold-vtl --api https://vaultcenter.example.net:8443 --ca-cert /path/ca.crt login --username admin
cold-vtl --api https://vaultcenter.example.net:8443 --ca-cert /path/ca.crt status
cold-vtl --api https://vaultcenter.example.net:8443 --ca-cert /path/ca.crt logout
```

`login` asks for the password without echo; `--password-stdin` is available
for protected automation. The sign-in token expires after 30 days.
The credentials file is `$XDG_CONFIG_HOME/cold-vtl/credentials.json` or
`~/.config/cold-vtl/credentials.json`, created as 0600 and bound to the API origin.
`--credentials` lets you manage several appliances separately.

For an OIDC account, sign in to the console, then create a token in
"Profile → API and CLI tokens" (via the avatar), valid for 1 to 90 days. Copy it into a 0600 file
and use `--token-file /path/token`. Tokens are displayed only
once and can be revoked. An API token cannot create other tokens.

CLI variables: `COLD_VTL_API_URL`, `COLD_VTL_CA_CERT`, `COLD_VTL_TOKEN_FILE`,
`COLD_VTL_CREDENTIALS`. The CLI refuses HTTP without `--allow-insecure-http`,
secret files readable by other users, symbolic links
and HTTP redirects. `logout` revokes the token in use.

## Verification

- `cargo test --offline --workspace` and Clippy on all targets.
- `cd ui && npm run build && npm run test:restoration`: regression of the media
  flows and check of the CSRF token transmission.
