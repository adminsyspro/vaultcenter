CREATE TABLE auth_users (
    id TEXT PRIMARY KEY NOT NULL,
    username TEXT NOT NULL,
    password_hash TEXT,
    issuer TEXT,
    subject TEXT,
    role TEXT NOT NULL CHECK (role IN ('admin', 'viewer')),
    revision INTEGER NOT NULL DEFAULT 0,
    disabled INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT (unixepoch()),
    CHECK ((issuer IS NULL AND subject IS NULL AND password_hash IS NOT NULL)
        OR (issuer IS NOT NULL AND subject IS NOT NULL AND password_hash IS NULL))
);
CREATE UNIQUE INDEX auth_local_username ON auth_users(username) WHERE issuer IS NULL;
CREATE UNIQUE INDEX auth_oidc_subject ON auth_users(issuer, subject) WHERE issuer IS NOT NULL;

CREATE TABLE auth_credentials (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    secret_hash TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('session', 'token')),
    name TEXT NOT NULL,
    csrf TEXT,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX auth_credentials_user ON auth_credentials(user_id);

CREATE TABLE auth_login_limits (
    key TEXT PRIMARY KEY NOT NULL,
    attempts INTEGER NOT NULL,
    reset_at INTEGER NOT NULL
);

CREATE TABLE auth_oidc_pending (
    state_hash TEXT PRIMARY KEY NOT NULL,
    browser_hash TEXT NOT NULL,
    nonce TEXT NOT NULL,
    verifier TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);
