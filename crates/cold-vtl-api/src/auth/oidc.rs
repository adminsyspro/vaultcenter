use super::*;
use axum::{extract::Query, response::Redirect};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use openidconnect::{
    AccessTokenHash, AuthorizationCode, ClientId, ClientSecret, CsrfToken, HttpRequest,
    HttpResponse, IssuerUrl, Nonce, OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, TokenResponse,
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata},
};
use std::time::Duration;

pub struct Config {
    issuer: String,
    client_id: String,
    secret: String,
    pub label: String,
    admin_group: String,
    viewer_group: String,
    ca_pem: Option<Vec<u8>>,
}

impl Config {
    pub fn from_env() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let Ok(issuer) = env::var("COLD_VTL_OIDC_ISSUER") else {
            return Ok(None);
        };
        let url = url::Url::parse(&issuer)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("OIDC issuer must use HTTPS without credentials, query or fragment".into());
        }
        let secret = std::fs::read_to_string(env::var("COLD_VTL_OIDC_CLIENT_SECRET_FILE")?)?
            .trim()
            .to_owned();
        if secret.is_empty() {
            return Err("empty OIDC client secret".into());
        }
        let admin_group =
            env::var("COLD_VTL_OIDC_ADMIN_GROUP").unwrap_or_else(|_| "/cold-vtl-admins".into());
        let viewer_group =
            env::var("COLD_VTL_OIDC_VIEWER_GROUP").unwrap_or_else(|_| "/cold-vtl-viewers".into());
        if admin_group.is_empty() || viewer_group.is_empty() || admin_group == viewer_group {
            return Err("OIDC groups must be nonempty and distinct".into());
        }
        Ok(Some(Self {
            issuer,
            secret,
            client_id: env::var("COLD_VTL_OIDC_CLIENT_ID")?,
            label: env::var("COLD_VTL_OIDC_LABEL").unwrap_or_else(|_| "Keycloak".into()),
            admin_group,
            viewer_group,
            ca_pem: env::var_os("COLD_VTL_OIDC_CA_FILE")
                .map(std::fs::read)
                .transpose()?,
        }))
    }

    fn http_client(&self) -> AuthResult<reqwest::Client> {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15));
        if let Some(pem) = &self.ca_pem {
            builder = builder
                .add_root_certificate(reqwest::Certificate::from_pem(pem).map_err(internal)?);
        }
        builder.build().map_err(internal)
    }
    fn role(&self, payload: &serde_json::Value) -> AuthResult<String> {
        let groups = payload
            .get("groups")
            .and_then(|v| v.as_array())
            .ok_or_else(forbidden)?;
        if groups.iter().any(|g| g.as_str() == Some(&self.admin_group)) {
            Ok("admin".into())
        } else if groups
            .iter()
            .any(|g| g.as_str() == Some(&self.viewer_group))
        {
            Ok("viewer".into())
        } else {
            Err(forbidden())
        }
    }
}

// Transport adapter keeps redirects disabled, validates TLS and caps discovery/JWKS/token bodies.
async fn send(http: reqwest::Client, request: HttpRequest) -> Result<HttpResponse, std::io::Error> {
    if request.uri().scheme_str() != Some("https") {
        return Err(std::io::Error::other("OIDC endpoints must use HTTPS"));
    }
    let mut response = http
        .request(request.method().clone(), request.uri().to_string())
        .headers(request.headers().clone())
        .body(request.into_body())
        .send()
        .await
        .map_err(std::io::Error::other)?;
    let status = response.status();
    let headers = response.headers().clone();
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(std::io::Error::other)? {
        if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
            return Err(std::io::Error::other("OIDC response too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let mut output = HttpResponse::new(bytes);
    *output.status_mut() = status;
    *output.headers_mut() = headers;
    Ok(output)
}

fn unavailable(_: impl std::fmt::Display) -> ApiError {
    error(
        StatusCode::BAD_GATEWAY,
        "identity provider unavailable or invalid response; local login remains available",
    )
}

pub async fn login(
    State(auth): State<Auth>,
    connection: Option<Extension<ConnectInfo<SocketAddr>>>,
) -> AuthResult<Response> {
    let config = auth
        .config
        .oidc
        .as_ref()
        .ok_or_else(|| ApiError::not_found("OIDC is not configured"))?;
    let ip = connection
        .map(|c| c.0.0.ip().to_string())
        .unwrap_or_else(|| "unknown".into());
    auth.rate_limit(&format!("oidc:{ip}"), 40).await?;
    let http = config.http_client()?;
    let transport = move |request| send(http.clone(), request);
    let metadata = CoreProviderMetadata::discover_async(
        IssuerUrl::new(config.issuer.clone()).map_err(internal)?,
        &transport,
    )
    .await
    .map_err(unavailable)?;
    let client = CoreClient::from_provider_metadata(
        metadata,
        ClientId::new(config.client_id.clone()),
        Some(ClientSecret::new(config.secret.clone())),
    )
    .set_redirect_uri(
        RedirectUrl::new(format!("{}/auth/oidc/callback", auth.config.public_url))
            .map_err(internal)?,
    );
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, state, nonce) = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("profile".into()))
        .set_pkce_challenge(challenge)
        .url();
    if url.scheme() != "https" {
        return Err(unavailable("invalid authorization URL"));
    }
    let browser = random_secret();
    sqlx::query("INSERT INTO auth_oidc_pending (state_hash,browser_hash,nonce,verifier,expires_at) VALUES (?,?,?,?,?)")
        .bind(digest(state.secret())).bind(digest(&browser)).bind(nonce.secret()).bind(verifier.secret()).bind(now()+300).execute(&auth.pool).await.map_err(internal)?;
    let mut response = Redirect::to(url.as_str()).into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        auth.cookie("cold_vtl_oidc", &browser, 300),
    );
    Ok(response)
}

#[derive(Deserialize)]
pub struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

pub async fn callback(
    State(auth): State<Auth>,
    headers: HeaderMap,
    Query(query): Query<Callback>,
) -> Response {
    // Always return to a fixed same-origin location; don't expose provider error text or codes.
    let result = complete(&auth, &headers, query).await;
    let mut response = match result {
        Ok(mut response) => {
            *response.status_mut() = StatusCode::SEE_OTHER;
            response
                .headers_mut()
                .insert(header::LOCATION, HeaderValue::from_static("/"));
            response
        }
        Err(_) => Redirect::to("/?login_error=oidc").into_response(),
    };
    response
        .headers_mut()
        .append(header::SET_COOKIE, auth.cookie("cold_vtl_oidc", "", 0));
    response
}

async fn complete(auth: &Auth, headers: &HeaderMap, query: Callback) -> AuthResult<Response> {
    let config = auth.config.oidc.as_ref().ok_or_else(forbidden)?;
    let state = query
        .state
        .filter(|s| s.len() <= 256)
        .ok_or_else(forbidden)?;
    let browser = cookie_value(headers, "cold_vtl_oidc")
        .filter(|s| s.len() <= 256)
        .ok_or_else(forbidden)?;
    // One-time consumption is atomic and tied to the browser that started this flow.
    let pending = sqlx::query("DELETE FROM auth_oidc_pending WHERE state_hash=? AND browser_hash=? AND expires_at>? RETURNING nonce,verifier")
        .bind(digest(&state)).bind(digest(&browser)).bind(now()).fetch_optional(&auth.pool).await.map_err(internal)?.ok_or_else(forbidden)?;
    if query.error.is_some() {
        return Err(forbidden());
    }
    let code = query
        .code
        .filter(|s| !s.is_empty() && s.len() < 8192)
        .ok_or_else(forbidden)?;
    let nonce = Nonce::new(pending.get("nonce"));
    let verifier = PkceCodeVerifier::new(pending.get("verifier"));
    let http = config.http_client()?;
    let transport = move |request| send(http.clone(), request);
    // Rediscovery also refreshes signing keys after provider rotation.
    let metadata = CoreProviderMetadata::discover_async(
        IssuerUrl::new(config.issuer.clone()).map_err(internal)?,
        &transport,
    )
    .await
    .map_err(unavailable)?;
    let client = CoreClient::from_provider_metadata(
        metadata,
        ClientId::new(config.client_id.clone()),
        Some(ClientSecret::new(config.secret.clone())),
    )
    .set_redirect_uri(
        RedirectUrl::new(format!("{}/auth/oidc/callback", auth.config.public_url))
            .map_err(internal)?,
    );
    let token = client
        .exchange_code(AuthorizationCode::new(code))
        .map_err(unavailable)?
        .set_pkce_verifier(verifier)
        .request_async(&transport)
        .await
        .map_err(unavailable)?;
    let id_token = token.id_token().ok_or_else(forbidden)?;
    let validator = client.id_token_verifier();
    let claims = id_token
        .claims(&validator, &nonce)
        .map_err(|_| forbidden())?;
    if let Some(expected) = claims.access_token_hash() {
        let actual = AccessTokenHash::from_token(
            token.access_token(),
            id_token.signing_alg().map_err(|_| forbidden())?,
            id_token.signing_key(&validator).map_err(|_| forbidden())?,
        )
        .map_err(|_| forbidden())?;
        if &actual != expected {
            return Err(forbidden());
        }
    }
    // Extra group claims are read only after library verification of signature, issuer,
    // audience, expiry and nonce. They are never accepted from an unverified JWT.
    let encoded = id_token.to_string();
    let payload: serde_json::Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(encoded.split('.').nth(1).ok_or_else(forbidden)?)
            .map_err(|_| forbidden())?,
    )
    .map_err(|_| forbidden())?;
    let role = config.role(&payload)?;
    let subject = claims.subject().as_str();
    let username = claims
        .preferred_username()
        .map(|s| s.as_str())
        .unwrap_or(subject);
    let mut tx = auth.pool.begin().await.map_err(internal)?;
    sqlx::query("INSERT INTO auth_users (id,username,issuer,subject,role) VALUES (?,?,?,?,?) ON CONFLICT(issuer,subject) WHERE issuer IS NOT NULL DO UPDATE SET username=excluded.username,role=excluded.role")
        .bind(Uuid::new_v4().to_string()).bind(username).bind(&config.issuer).bind(subject).bind(role).execute(&mut *tx).await.map_err(internal)?;
    let user: User = sqlx::query_as("SELECT id,username,role,issuer,disabled,revision FROM auth_users WHERE issuer=? AND subject=?")
        .bind(&config.issuer).bind(subject).fetch_one(&mut *tx).await.map_err(internal)?;
    if user.disabled {
        return Err(forbidden());
    }
    tx.commit().await.map_err(internal)?;
    auth.session_response(user, headers).await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        Config {
            issuer: "https://id.test/realms/test".into(),
            client_id: "vtl".into(),
            secret: "secret".into(),
            label: "SSO".into(),
            admin_group: "/admins".into(),
            viewer_group: "/viewers".into(),
            ca_pem: None,
        }
    }
    #[test]
    fn groups_require_exact_membership() {
        let config = config();
        for claims in [
            serde_json::json!({}),
            serde_json::json!({"groups":"/admins"}),
            serde_json::json!({"groups":["/admins/child","admins","/unrelated"]}),
        ] {
            assert!(config.role(&claims).is_err());
        }
        assert_eq!(
            config
                .role(&serde_json::json!({"groups":["/viewers"]}))
                .unwrap(),
            "viewer"
        );
        assert_eq!(
            config
                .role(&serde_json::json!({"groups":["/viewers","/admins"]}))
                .unwrap(),
            "admin"
        );
    }
    #[tokio::test]
    async fn callback_binds_browser_and_consumes_state_on_failure() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let auth = Auth::new(
            pool,
            super::super::Config {
                public_url: "https://vtl.test".into(),
                secure: true,
                oidc: Some(config()),
            },
        )
        .await
        .unwrap();
        sqlx::query("INSERT INTO auth_oidc_pending VALUES (?,?,?,?,?)")
            .bind(digest("state"))
            .bind(digest("browser"))
            .bind("nonce")
            .bind("verifier")
            .bind(now() + 60)
            .execute(&auth.pool)
            .await
            .unwrap();
        let query = || Callback {
            state: Some("state".into()),
            code: None,
            error: Some("denied".into()),
        };
        assert!(complete(&auth, &HeaderMap::new(), query()).await.is_err());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_oidc_pending")
            .fetch_one(&auth.pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("cold_vtl_oidc=browser"),
        );
        assert!(complete(&auth, &headers, query()).await.is_err());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_oidc_pending")
            .fetch_one(&auth.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        assert!(complete(&auth, &headers, query()).await.is_err());
    }
}
