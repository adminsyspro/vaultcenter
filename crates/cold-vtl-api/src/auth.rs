mod oidc;
#[cfg(test)]
mod tests;

use crate::ApiError;
use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use axum::{
    Extension, Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::{
    env,
    net::SocketAddr,
    path::Path as FsPath,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;
use uuid::Uuid;

pub type AuthResult<T> = Result<T, ApiError>;

#[derive(Clone)]
pub struct Auth {
    pool: SqlitePool,
    config: Arc<Config>,
    hashing: Arc<Semaphore>,
    dummy_hash: Arc<String>,
}

pub struct Config {
    pub public_url: String,
    secure: bool,
    oidc: Option<oidc::Config>,
}

#[derive(Clone, Debug, Serialize, sqlx::FromRow)]
pub struct User {
    pub id: String,
    pub username: String,
    pub role: String,
    pub issuer: Option<String>,
    pub disabled: bool,
    #[serde(skip)]
    revision: i64,
}

#[derive(Clone)]
pub struct Principal {
    pub user: User,
    credential_id: String,
    kind: String,
    csrf: Option<String>,
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn random_secret() -> String {
    openidconnect::CsrfToken::new_random().secret().clone()
}
pub fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
fn error(status: StatusCode, message: &str) -> ApiError {
    ApiError {
        status,
        message: message.into(),
    }
}
fn internal(_: impl std::fmt::Display) -> ApiError {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "authentication operation failed",
    )
}
fn unauthorized() -> ApiError {
    error(StatusCode::UNAUTHORIZED, "authentication required")
}
fn forbidden() -> ApiError {
    error(StatusCode::FORBIDDEN, "permission denied")
}

impl Config {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let raw =
            env::var("COLD_VTL_PUBLIC_URL").unwrap_or_else(|_| "https://localhost:8443".into());
        let url = url::Url::parse(&raw)?;
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err("COLD_VTL_PUBLIC_URL must be an origin without path, credentials, query or fragment".into());
        }
        let secure = url.scheme() == "https";
        if !(secure
            || (url.scheme() == "http"
                && env::var("COLD_VTL_ALLOW_INSECURE_HTTP").as_deref() == Ok("true")))
        {
            return Err(
                "HTTPS required; COLD_VTL_ALLOW_INSECURE_HTTP=true is only for isolated tests"
                    .into(),
            );
        }
        Ok(Self {
            public_url: url.origin().ascii_serialization(),
            secure,
            oidc: oidc::Config::from_env()?,
        })
    }
}

impl Auth {
    pub async fn new(pool: SqlitePool, config: Config) -> AuthResult<Self> {
        let dummy_hash = hash_password(random_secret()).await?;
        let auth = Self {
            pool,
            config: Arc::new(config),
            hashing: Arc::new(Semaphore::new(2)),
            dummy_hash: Arc::new(dummy_hash),
        };
        auth.prune().await?;
        let cleanup = auth.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
            loop {
                interval.tick().await;
                let _ = cleanup.prune().await;
            }
        });
        Ok(auth)
    }

    async fn prune(&self) -> AuthResult<()> {
        for query in [
            "DELETE FROM auth_credentials WHERE expires_at <= ?",
            "DELETE FROM auth_oidc_pending WHERE expires_at <= ?",
        ] {
            sqlx::query(query)
                .bind(now())
                .execute(&self.pool)
                .await
                .map_err(internal)?;
        }
        sqlx::query("DELETE FROM auth_login_limits WHERE reset_at <= ?")
            .bind(now())
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        Ok(())
    }

    fn cookie_name(&self) -> &'static str {
        if self.config.secure {
            "__Host-cold_vtl"
        } else {
            "cold_vtl_dev"
        }
    }
    fn cookie(&self, name: &str, value: &str, seconds: i64) -> HeaderValue {
        HeaderValue::from_str(&format!(
            "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={seconds}{}",
            if self.config.secure { "; Secure" } else { "" }
        ))
        .expect("server-generated cookie")
    }

    fn same_origin(&self, headers: &HeaderMap) -> AuthResult<()> {
        if let Some(origin) = headers.get(header::ORIGIN)
            && origin.to_str().ok() != Some(self.config.public_url.as_str())
        {
            return Err(forbidden());
        }
        if headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) == Some("cross-site") {
            return Err(forbidden());
        }
        Ok(())
    }

    async fn authenticate(&self, headers: &HeaderMap) -> AuthResult<Principal> {
        let (secret, kind) = if let Some(value) = headers.get(header::AUTHORIZATION) {
            (
                value
                    .to_str()
                    .ok()
                    .and_then(|s| s.strip_prefix("Bearer "))
                    .filter(|s| !s.is_empty())
                    .ok_or_else(unauthorized)?
                    .to_owned(),
                "token",
            )
        } else {
            (
                cookie_value(headers, self.cookie_name()).ok_or_else(unauthorized)?,
                "session",
            )
        };
        if secret.len() > 256 {
            return Err(unauthorized());
        }
        let row = sqlx::query("SELECT u.id, u.username, u.role, u.issuer, u.disabled, u.revision, c.id AS credential_id, c.csrf FROM auth_credentials c JOIN auth_users u ON u.id=c.user_id WHERE c.secret_hash=? AND c.kind=? AND c.expires_at>? AND u.disabled=0")
            .bind(digest(&secret)).bind(kind).bind(now()).fetch_optional(&self.pool).await.map_err(internal)?.ok_or_else(unauthorized)?;
        Ok(Principal {
            user: User {
                id: row.get("id"),
                username: row.get("username"),
                role: row.get("role"),
                issuer: row.get("issuer"),
                disabled: row.get("disabled"),
                revision: row.get("revision"),
            },
            credential_id: row.get("credential_id"),
            kind: kind.into(),
            csrf: row.get("csrf"),
        })
    }

    fn csrf(&self, headers: &HeaderMap, principal: &Principal) -> AuthResult<()> {
        self.same_origin(headers)?;
        if principal.kind == "session" {
            let supplied = headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(forbidden)?;
            if Some(digest(supplied)) != principal.csrf.as_deref().map(digest) {
                return Err(forbidden());
            }
        }
        Ok(())
    }

    async fn rate_limit(&self, key: &str, max: i64) -> AuthResult<()> {
        let count: i64 = sqlx::query_scalar("INSERT INTO auth_login_limits (key,attempts,reset_at) VALUES (?,1,?) ON CONFLICT(key) DO UPDATE SET attempts=CASE WHEN reset_at<=? THEN 1 ELSE attempts+1 END, reset_at=CASE WHEN reset_at<=? THEN excluded.reset_at ELSE reset_at END RETURNING attempts")
            .bind(digest(key)).bind(now()+300).bind(now()).bind(now()).fetch_one(&self.pool).await.map_err(internal)?;
        if count > max {
            return Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "too many login attempts; retry in five minutes",
            ));
        }
        Ok(())
    }

    async fn local_login(&self, request: Login, ip: &str) -> AuthResult<User> {
        let username = request.username.to_ascii_lowercase();
        self.rate_limit(&format!("ip:{ip}"), 40).await?;
        self.rate_limit(&format!("user:{username}"), 10).await?;
        let _permit = self.hashing.clone().try_acquire_owned().map_err(|_| {
            error(
                StatusCode::TOO_MANY_REQUESTS,
                "authentication busy; retry shortly",
            )
        })?;
        let row = sqlx::query("SELECT id,username,role,issuer,disabled,revision,password_hash FROM auth_users WHERE username=? AND issuer IS NULL")
            .bind(&username).fetch_optional(&self.pool).await.map_err(internal)?;
        let hash = row
            .as_ref()
            .map(|r| r.get::<String, _>("password_hash"))
            .unwrap_or_else(|| self.dummy_hash.as_ref().clone());
        let valid = verify_password(request.password, hash).await;
        if !valid || row.as_ref().is_none_or(|r| r.get::<bool, _>("disabled")) {
            return Err(error(
                StatusCode::UNAUTHORIZED,
                "invalid username or password",
            ));
        }
        let row = row.unwrap();
        sqlx::query("DELETE FROM auth_login_limits WHERE key=?")
            .bind(digest(&format!("user:{username}")))
            .execute(&self.pool)
            .await
            .map_err(internal)?;
        Ok(User {
            id: row.get("id"),
            username: row.get("username"),
            role: row.get("role"),
            issuer: None,
            disabled: false,
            revision: row.get("revision"),
        })
    }

    async fn credential(
        &self,
        user: &User,
        kind: &str,
        name: &str,
        seconds: i64,
    ) -> AuthResult<(String, Option<String>, i64)> {
        let secret = random_secret();
        let csrf = (kind == "session").then(random_secret);
        let expires = now() + seconds;
        let inserted = sqlx::query("INSERT INTO auth_credentials (id,user_id,secret_hash,kind,name,csrf,expires_at) SELECT ?,id,?,?,?,?,? FROM auth_users WHERE id=? AND revision=? AND disabled=0")
            .bind(Uuid::new_v4().to_string()).bind(digest(&secret)).bind(kind).bind(name).bind(&csrf).bind(expires).bind(&user.id).bind(user.revision)
            .execute(&self.pool).await.map_err(internal)?;
        if inserted.rows_affected() != 1 {
            return Err(unauthorized());
        }
        Ok((secret, csrf, expires))
    }

    async fn session_response(&self, user: User, headers: &HeaderMap) -> AuthResult<Response> {
        if let Some(old) = cookie_value(headers, self.cookie_name()) {
            sqlx::query("DELETE FROM auth_credentials WHERE secret_hash=? AND kind='session'")
                .bind(digest(&old))
                .execute(&self.pool)
                .await
                .map_err(internal)?;
        }
        let (secret, csrf, _) = self
            .credential(&user, "session", "Browser", 8 * 3600)
            .await?;
        let mut response = Json(serde_json::json!({"user":user,"csrf_token":csrf})).into_response();
        response.headers_mut().append(
            header::SET_COOKIE,
            self.cookie(self.cookie_name(), &secret, 8 * 3600),
        );
        Ok(response)
    }
}

pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|s| s.split(';'))
        .filter_map(|s| s.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

pub async fn require_auth(
    State(auth): State<Auth>,
    mut request: Request,
    next: Next,
) -> AuthResult<Response> {
    let principal = auth.authenticate(request.headers()).await?;
    if !matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) {
        auth.csrf(request.headers(), &principal)?;
        // Marking one's own activity as seen changes nothing else: viewers may.
        let path = request.uri().path();
        if path.starts_with("/v1/") && path != "/v1/activity/seen" {
            admin(&principal)?;
        }
    }
    request.extensions_mut().insert(principal);
    Ok(next.run(request).await)
}

pub async fn security_headers(request: Request, next: Next) -> Response {
    let private =
        request.uri().path().starts_with("/auth/") || request.uri().path().starts_with("/v1/");
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static("frame-ancestors 'none'"),
    );
    if private {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

fn admin(principal: &Principal) -> AuthResult<()> {
    if principal.user.role == "admin" {
        Ok(())
    } else {
        Err(forbidden())
    }
}
fn valid_role(role: &str) -> bool {
    matches!(role, "admin" | "viewer")
}
fn valid_username(name: &str) -> bool {
    (3..=64).contains(&name.len())
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        })
}
fn valid_password(password: &str) -> bool {
    (12..=1024).contains(&password.len())
}

pub async fn hash_password(password: String) -> AuthResult<String> {
    if !valid_password(&password) {
        return Err(ApiError::bad_request(
            "password must contain between 12 and 1024 bytes",
        ));
    }
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes())
            .map(|h| h.to_string())
            .map_err(internal)
    })
    .await
    .map_err(internal)?
}
async fn verify_password(password: String, hash: String) -> bool {
    if password.len() > 1024 {
        return false;
    }
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).is_ok_and(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
    })
    .await
    .unwrap_or(false)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    username: String,
    password: String,
}

async fn login(
    State(auth): State<Auth>,
    connection: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    Json(request): Json<Login>,
) -> AuthResult<Response> {
    auth.same_origin(&headers)?;
    let user = auth
        .local_login(
            request,
            &connection
                .map(|c| c.0.0.ip().to_string())
                .unwrap_or_else(|| "unknown".into()),
        )
        .await?;
    auth.session_response(user, &headers).await
}
async fn token_login(
    State(auth): State<Auth>,
    connection: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    Json(request): Json<Login>,
) -> AuthResult<Json<serde_json::Value>> {
    auth.same_origin(&headers)?;
    let user = auth
        .local_login(
            request,
            &connection
                .map(|c| c.0.0.ip().to_string())
                .unwrap_or_else(|| "unknown".into()),
        )
        .await?;
    let (token, _, expires) = auth
        .credential(&user, "token", "CLI login", 30 * 86400)
        .await?;
    Ok(Json(
        serde_json::json!({"token":token,"expires_at":expires,"user":user}),
    ))
}
async fn me(Extension(principal): Extension<Principal>) -> Json<serde_json::Value> {
    Json(serde_json::json!({"user":principal.user,"csrf_token":principal.csrf}))
}
async fn config(State(auth): State<Auth>) -> Json<serde_json::Value> {
    Json(
        serde_json::json!({"local":true,"oidc":auth.config.oidc.is_some(),"oidc_label":auth.config.oidc.as_ref().map(|c| &c.label)}),
    )
}
async fn logout(
    State(auth): State<Auth>,
    Extension(principal): Extension<Principal>,
) -> AuthResult<Response> {
    sqlx::query("DELETE FROM auth_credentials WHERE id=?")
        .bind(principal.credential_id)
        .execute(&auth.pool)
        .await
        .map_err(internal)?;
    let mut response = Json(serde_json::json!({"status":"logged_out"})).into_response();
    response
        .headers_mut()
        .append(header::SET_COOKIE, auth.cookie(auth.cookie_name(), "", 0));
    Ok(response)
}

#[derive(Deserialize)]
struct NewToken {
    name: String,
    days: u32,
}
async fn new_token(
    State(auth): State<Auth>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<NewToken>,
) -> AuthResult<Json<serde_json::Value>> {
    if principal.kind != "session" {
        return Err(forbidden());
    }
    if !(1..=90).contains(&request.days)
        || request.name.trim().is_empty()
        || request.name.len() > 80
    {
        return Err(ApiError::bad_request(
            "token requires a name and an expiry between 1 and 90 days",
        ));
    }
    let (token, _, expires) = auth
        .credential(
            &principal.user,
            "token",
            request.name.trim(),
            i64::from(request.days) * 86400,
        )
        .await?;
    Ok(Json(
        serde_json::json!({"token":token,"expires_at":expires}),
    ))
}
async fn tokens(
    State(auth): State<Auth>,
    Extension(principal): Extension<Principal>,
) -> AuthResult<Json<Vec<serde_json::Value>>> {
    let rows = sqlx::query("SELECT id,name,expires_at,created_at FROM auth_credentials WHERE user_id=? AND kind='token' AND expires_at>? ORDER BY created_at DESC")
        .bind(principal.user.id).bind(now()).fetch_all(&auth.pool).await.map_err(internal)?;
    Ok(Json(rows.iter().map(|r| serde_json::json!({"id":r.get::<String,_>("id"),"name":r.get::<String,_>("name"),"expires_at":r.get::<i64,_>("expires_at"),"created_at":r.get::<i64,_>("created_at")})).collect()))
}
async fn revoke_token(
    State(auth): State<Auth>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> AuthResult<Json<serde_json::Value>> {
    sqlx::query("DELETE FROM auth_credentials WHERE id=? AND user_id=? AND kind='token'")
        .bind(id)
        .bind(principal.user.id)
        .execute(&auth.pool)
        .await
        .map_err(internal)?;
    Ok(Json(serde_json::json!({"status":"revoked"})))
}

#[derive(Deserialize)]
struct PasswordChange {
    current_password: String,
    new_password: String,
}
async fn change_password(
    State(auth): State<Auth>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<PasswordChange>,
) -> AuthResult<Json<serde_json::Value>> {
    if principal.user.issuer.is_some() {
        return Err(ApiError::bad_request(
            "manage this password with the identity provider",
        ));
    }
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM auth_users WHERE id=?")
        .bind(&principal.user.id)
        .fetch_one(&auth.pool)
        .await
        .map_err(internal)?;
    auth.rate_limit(&format!("password:{}", principal.user.id), 10)
        .await?;
    if !verify_password(request.current_password, hash).await {
        return Err(error(StatusCode::UNAUTHORIZED, "invalid current password"));
    }
    let hash = hash_password(request.new_password).await?;
    let mut tx = auth.pool.begin().await.map_err(internal)?;
    let changed = sqlx::query(
        "UPDATE auth_users SET password_hash=?,revision=revision+1 WHERE id=? AND revision=?",
    )
    .bind(hash)
    .bind(&principal.user.id)
    .bind(principal.user.revision)
    .execute(&mut *tx)
    .await
    .map_err(internal)?;
    if changed.rows_affected() != 1 {
        return Err(unauthorized());
    }
    sqlx::query("DELETE FROM auth_credentials WHERE user_id=?")
        .bind(principal.user.id)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(Json(
        serde_json::json!({"status":"password_changed","reauthenticate":true}),
    ))
}

#[derive(Deserialize)]
struct NewUser {
    username: String,
    password: String,
    role: String,
}
async fn users(
    State(auth): State<Auth>,
    Extension(principal): Extension<Principal>,
) -> AuthResult<Json<Vec<User>>> {
    admin(&principal)?;
    Ok(Json(
        sqlx::query_as(
            "SELECT id,username,role,issuer,disabled,revision FROM auth_users ORDER BY username",
        )
        .fetch_all(&auth.pool)
        .await
        .map_err(internal)?,
    ))
}
async fn new_user(
    State(auth): State<Auth>,
    Extension(principal): Extension<Principal>,
    Json(request): Json<NewUser>,
) -> AuthResult<Json<serde_json::Value>> {
    admin(&principal)?;
    create_local_user(
        &auth.pool,
        &request.username,
        request.password,
        &request.role,
    )
    .await?;
    Ok(Json(serde_json::json!({"status":"created"})))
}

#[derive(Deserialize)]
struct UserUpdate {
    role: String,
    disabled: bool,
}
async fn update_user(
    State(auth): State<Auth>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    Json(request): Json<UserUpdate>,
) -> AuthResult<Json<serde_json::Value>> {
    admin(&principal)?;
    if id == principal.user.id || !valid_role(&request.role) {
        return Err(ApiError::bad_request(
            "cannot change your own access or use an unknown role",
        ));
    }
    let mut tx = auth.pool.begin().await.map_err(internal)?;
    // Acquire SQLite's write lock before checking the last local administrator.
    sqlx::query("UPDATE auth_users SET disabled=disabled WHERE id=?")
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    let target: User = sqlx::query_as(
        "SELECT id,username,role,issuer,disabled,revision FROM auth_users WHERE id=?",
    )
    .bind(&id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(internal)?
    .ok_or_else(|| ApiError::not_found("user not found"))?;
    if target.issuer.is_some() && request.role != target.role {
        return Err(ApiError::bad_request(
            "OIDC roles are managed by provider groups",
        ));
    }
    if target.issuer.is_none()
        && target.role == "admin"
        && !target.disabled
        && (request.disabled || request.role != "admin")
    {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM auth_users WHERE issuer IS NULL AND role='admin' AND disabled=0",
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(internal)?;
        if count <= 1 {
            return Err(ApiError::conflict(
                "keep at least one enabled local administrator",
            ));
        }
    }
    sqlx::query("UPDATE auth_users SET role=?,disabled=?,revision=revision+1 WHERE id=?")
        .bind(request.role)
        .bind(request.disabled)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    sqlx::query("DELETE FROM auth_credentials WHERE user_id=?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    Ok(Json(serde_json::json!({"status":"updated"})))
}

pub async fn create_local_user(
    pool: &SqlitePool,
    username: &str,
    password: String,
    role: &str,
) -> AuthResult<()> {
    if !valid_username(username) || !valid_role(role) {
        return Err(ApiError::bad_request(
            "username: 3–64 lowercase letters, digits, dot, hyphen or underscore; role: admin or viewer",
        ));
    }
    let hash = hash_password(password).await?;
    sqlx::query("INSERT INTO auth_users (id,username,password_hash,role) VALUES (?,?,?,?)")
        .bind(Uuid::new_v4().to_string())
        .bind(username)
        .bind(hash)
        .bind(role)
        .execute(pool)
        .await
        .map_err(|e| {
            if matches!(&e,sqlx::Error::Database(db) if db.is_unique_violation()) {
                ApiError::conflict("username already exists")
            } else {
                internal(e)
            }
        })?;
    Ok(())
}

/// Administration uses a separate connection: it never marks active VTL jobs failed.
pub async fn admin_pool(path: &FsPath) -> Result<SqlitePool, Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .foreign_keys(true),
        )
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

pub fn routes(auth: Auth) -> Router {
    let private = Router::new()
        .route("/auth/me", get(me))
        .route("/auth/logout", post(logout))
        .route("/auth/password", post(change_password))
        .route("/auth/tokens", get(tokens).post(new_token))
        .route("/auth/tokens/:id", delete(revoke_token))
        .route("/auth/users", get(users).post(new_user))
        .route("/auth/users/:id", post(update_user))
        .route_layer(middleware::from_fn_with_state(auth.clone(), require_auth));
    private
        .route("/auth/config", get(config))
        .route("/auth/login", post(login))
        .route("/auth/login/token", post(token_login))
        .route("/auth/oidc/login", get(oidc::login))
        .route("/auth/oidc/callback", get(oidc::callback))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(auth)
}
