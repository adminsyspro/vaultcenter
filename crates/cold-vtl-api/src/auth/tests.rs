use super::*;
use axum::body::{Body, to_bytes};
use tower::ServiceExt;

const PASSWORD: &str = "valid-test-password-42";
async fn setup() -> Auth {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    let auth = Auth::new(
        pool,
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    create_local_user(&auth.pool, "admin", PASSWORD.into(), "admin")
        .await
        .unwrap();
    create_local_user(&auth.pool, "reader", PASSWORD.into(), "viewer")
        .await
        .unwrap();
    auth
}
fn router(auth: &Auth) -> Router {
    Router::new()
        .route(
            "/v1/probe",
            get(|| async { Json(serde_json::json!({"ok":true})) })
                .post(|| async { Json(serde_json::json!({"ok":true})) }),
        )
        .route_layer(middleware::from_fn_with_state(auth.clone(), require_auth))
        .merge(routes(auth.clone()))
        .layer(middleware::from_fn(security_headers))
}
async fn call(
    app: &Router,
    method: &str,
    path: &str,
    headers: HeaderMap,
    body: serde_json::Value,
) -> (StatusCode, HeaderMap, serde_json::Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    req.headers_mut().extend(headers);
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}
async fn session(app: &Router, username: &str) -> (HeaderMap, serde_json::Value) {
    let (status, headers, body) = call(
        app,
        "POST",
        "/auth/login",
        HeaderMap::new(),
        serde_json::json!({"username":username,"password":PASSWORD}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let cookie = headers[header::SET_COOKIE].to_str().unwrap();
    assert!(
        cookie.contains("HttpOnly") && cookie.contains("Secure") && cookie.contains("SameSite=Lax")
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        HeaderValue::from_str(cookie.split(';').next().unwrap()).unwrap(),
    );
    headers.insert(
        "x-csrf-token",
        HeaderValue::from_str(body["csrf_token"].as_str().unwrap()).unwrap(),
    );
    (headers, body)
}

#[tokio::test]
async fn anonymous_viewer_csrf_origin_and_logout() {
    let auth = setup().await;
    let app = router(&auth);
    for method in ["GET", "POST"] {
        assert_eq!(
            call(
                &app,
                method,
                "/v1/probe",
                HeaderMap::new(),
                serde_json::json!({})
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED
        );
    }
    let (viewer, _) = session(&app, "reader").await;
    assert_eq!(
        call(
            &app,
            "GET",
            "/v1/probe",
            viewer.clone(),
            serde_json::json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    for path in ["/v1/probe", "/auth/users"] {
        assert_eq!(
            call(
                &app,
                "POST",
                path,
                viewer.clone(),
                serde_json::json!({"username":"other","password":PASSWORD,"role":"admin"})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let (admin, _) = session(&app, "admin").await;
    let mut missing = admin.clone();
    missing.remove("x-csrf-token");
    assert_eq!(
        call(&app, "POST", "/v1/probe", missing, serde_json::json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let mut wrong = admin.clone();
    wrong.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://evil.test"),
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/probe",
            wrong.clone(),
            serde_json::json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/auth/login",
            wrong,
            serde_json::json!({"username":"admin","password":PASSWORD})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, headers, _) = call(
        &app,
        "POST",
        "/v1/probe",
        admin.clone(),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        call(
            &app,
            "POST",
            "/auth/logout",
            admin.clone(),
            serde_json::json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/auth/me", admin, serde_json::json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn tokens_expiry_revocation_and_password_rotation() {
    let auth = setup().await;
    let app = router(&auth);
    let (session, _) = session(&app, "admin").await;
    let (_, _, body) = call(
        &app,
        "POST",
        "/auth/tokens",
        session.clone(),
        serde_json::json!({"name":"test","days":1}),
    )
    .await;
    let secret = body["token"].as_str().unwrap();
    let mut bearer = HeaderMap::new();
    bearer.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/probe",
            bearer.clone(),
            serde_json::json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/auth/tokens",
            bearer.clone(),
            serde_json::json!({"name":"chain","days":1})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let stored: String =
        sqlx::query_scalar("SELECT secret_hash FROM auth_credentials WHERE kind='token'")
            .fetch_one(&auth.pool)
            .await
            .unwrap();
    assert_eq!(stored, digest(secret));
    assert_ne!(stored, secret);
    let old_user = auth
        .local_login(
            Login {
                username: "admin".into(),
                password: PASSWORD.into(),
            },
            "test",
        )
        .await
        .unwrap();
    let changed = call(
        &app,
        "POST",
        "/auth/password",
        session.clone(),
        serde_json::json!({"current_password":PASSWORD,"new_password":"changed-password-456"}),
    )
    .await;
    assert_eq!(changed.0, StatusCode::OK);
    assert_eq!(
        call(&app, "GET", "/auth/me", session, serde_json::json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "GET", "/v1/probe", bearer, serde_json::json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        auth.credential(&old_user, "session", "race", 60)
            .await
            .is_err(),
        "a login verified before password reset cannot mint a credential afterward"
    );
    assert!(
        auth.local_login(
            Login {
                username: "admin".into(),
                password: PASSWORD.into()
            },
            "test"
        )
        .await
        .is_err()
    );
    let user = auth
        .local_login(
            Login {
                username: "admin".into(),
                password: "changed-password-456".into(),
            },
            "test",
        )
        .await
        .unwrap();
    let (expired, _, _) = auth
        .credential(&user, "token", "expired", -1)
        .await
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {expired}")).unwrap(),
    );
    assert!(auth.authenticate(&headers).await.is_err());
}

#[tokio::test]
async fn disable_revokes_and_last_local_administrator_is_preserved() {
    let auth = setup().await;
    let app = router(&auth);
    let (admin, admin_body) = session(&app, "admin").await;
    let (reader, reader_body) = session(&app, "reader").await;
    let reader_path = format!(
        "/auth/users/{}",
        reader_body["user"]["id"].as_str().unwrap()
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &reader_path,
            admin.clone(),
            serde_json::json!({"role":"viewer","disabled":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/v1/probe", reader, serde_json::json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let own = format!("/auth/users/{}", admin_body["user"]["id"].as_str().unwrap());
    assert_eq!(
        call(
            &app,
            "POST",
            &own,
            admin,
            serde_json::json!({"role":"viewer","disabled":true})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    sqlx::query("INSERT INTO auth_users (id,username,issuer,subject,role) VALUES ('sso','sso','https://id.test','123','admin')").execute(&auth.pool).await.unwrap();
    let sso: User = sqlx::query_as(
        "SELECT id,username,role,issuer,disabled,revision FROM auth_users WHERE id='sso'",
    )
    .fetch_one(&auth.pool)
    .await
    .unwrap();
    let (secret, _, _) = auth.credential(&sso, "token", "test", 60).await.unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &own,
            headers,
            serde_json::json!({"role":"viewer","disabled":true})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn session_rotation_and_rate_limits() {
    let auth = setup().await;
    let app = router(&auth);
    let (old, _) = session(&app, "admin").await;
    assert_eq!(
        call(
            &app,
            "POST",
            "/auth/login",
            old.clone(),
            serde_json::json!({"username":"admin","password":PASSWORD})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/auth/me", old, serde_json::json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert!(auth.rate_limit("bucket", 1).await.is_ok());
    assert_eq!(
        auth.rate_limit("bucket", 1).await.unwrap_err().status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn application_routes_all_require_authentication() {
    let folder = std::env::temp_dir().join(format!("cold-vtl-auth-routes-{}", Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&folder.join("catalog.db"))
        .await
        .unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    let state = crate::AppState {
        catalog,
        operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
        config: Arc::new(crate::AppConfig {
            storage_socket: None,
            media_root: folder.join("media"),
            worker_path: folder.join("absent-worker"),
            restore_root: folder.join("restores"),
            quiesce_check: None,
            maintenance_helper: None,
            cartridge_helper: None,
            reintegration_helper: None,
            archive_helper: None,
            telemetry_dir: None,
            telemetry_retention_days: 30,
        }),
    };
    let app = crate::app(state, None, auth.clone());
    for (method, path) in [
        ("GET", "/v1/status"),
        ("GET", "/v1/storage"),
        ("GET", "/v1/storage/access"),
        (
            "GET",
            "/v1/vtls/00000000-0000-4000-8000-000000000001/storage/connection",
        ),
        (
            "PUT",
            "/v1/vtls/00000000-0000-4000-8000-000000000001/storage/connection",
        ),
        (
            "GET",
            "/v1/vtls/00000000-0000-4000-8000-000000000001/storage/schedule",
        ),
        (
            "PUT",
            "/v1/vtls/00000000-0000-4000-8000-000000000001/storage/schedule",
        ),
        (
            "GET",
            "/v1/vtls/00000000-0000-4000-8000-000000000001/storage/buckets/discover",
        ),
        (
            "POST",
            "/v1/vtls/00000000-0000-4000-8000-000000000001/storage/buckets/attach",
        ),
        (
            "POST",
            "/v1/vtls/00000000-0000-4000-8000-000000000001/storage/buckets/create",
        ),
        (
            "GET",
            "/v1/vtls/00000000-0000-4000-8000-000000000001/storage/recovery",
        ),
        (
            "POST",
            "/v1/vtls/00000000-0000-4000-8000-000000000001/storage/recovery",
        ),
        (
            "POST",
            "/v1/vtls/00000000-0000-4000-8000-000000000001/storage/recovery/check",
        ),
        ("GET", "/v1/settings/s3"),
        ("PUT", "/v1/settings/s3"),
        ("POST", "/v1/settings/s3/test"),
        ("POST", "/v1/storage/recall"),
        ("GET", "/v1/storage/archive"),
        ("POST", "/v1/storage/archive/refresh"),
        ("POST", "/v1/storage/archive"),
        ("GET", "/v1/maintenance"),
        ("GET", "/v1/reintegrations"),
        ("POST", "/v1/reintegrations/test/rollback"),
        ("POST", "/v1/cartridges/CVT001L8/reintegrate"),
        ("POST", "/v1/maintenance/enter"),
        ("POST", "/v1/maintenance/leave"),
        ("GET", "/v1/cartridges"),
        ("POST", "/v1/cartridges"),
        ("GET", "/v1/jobs"),
        ("GET", "/v1/jobs/test"),
        ("POST", "/v1/cartridges/CVT001L8/seal"),
        ("POST", "/v1/cartridges/CVT001L8/export"),
        ("POST", "/v1/cartridges/CVT001L8/reconcile"),
        ("POST", "/v1/cartridges/CVT001L8/restore"),
        ("GET", "/v1/cartridges/CVT001L8/generations"),
        ("GET", "/v1/activity"),
        ("POST", "/v1/activity/seen"),
        (
            "POST",
            "/v1/vtls/00000000-0000-4000-8000-000000000001/storage/buckets/older/retire",
        ),
    ] {
        assert_eq!(
            call(&app, method, path, HeaderMap::new(), serde_json::json!({}))
                .await
                .0,
            StatusCode::UNAUTHORIZED,
            "{method} {path}"
        );
    }
    assert_eq!(
        call(
            &app,
            "GET",
            "/health",
            HeaderMap::new(),
            serde_json::json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    auth.pool.close().await;
    std::fs::remove_dir_all(folder).unwrap();
}

#[tokio::test]
async fn maintenance_requires_admin_and_no_active_jobs() {
    let folder = std::env::temp_dir().join(format!("cold-vtl-maintenance-api-{}", Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&folder.join("catalog.db"))
        .await
        .unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    create_local_user(&auth.pool, "admin", PASSWORD.into(), "admin")
        .await
        .unwrap();
    create_local_user(&auth.pool, "reader", PASSWORD.into(), "viewer")
        .await
        .unwrap();
    let cartridge =
        cold_vtl_core::Cartridge::new(cold_vtl_core::Barcode::parse("TAPE01").unwrap(), 1024)
            .unwrap();
    catalog.create_cartridge(&cartridge).await.unwrap();
    catalog
        .reserve_job("running-job", "restore", &cartridge, Some(1), None)
        .await
        .unwrap();
    let state = crate::AppState {
        catalog,
        operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
        config: Arc::new(crate::AppConfig {
            storage_socket: None,
            media_root: folder.join("media"),
            restore_root: folder.join("restore"),
            worker_path: folder.join("absent"),
            quiesce_check: None,
            maintenance_helper: Some(folder.join("must-not-run")),
            cartridge_helper: None,
            reintegration_helper: Some(folder.join("must-not-run")),
            archive_helper: Some(folder.join("must-not-run")),
            telemetry_dir: None,
            telemetry_retention_days: 30,
        }),
    };
    let app = crate::app(state, None, auth.clone());
    let (admin, _) = session(&app, "admin").await;
    let (viewer, _) = session(&app, "reader").await;
    for (path, payload) in [
        (
            "/v1/storage/recall",
            serde_json::json!({"confirm_bucket":"bucket","acknowledge_retrieval_cost":true}),
        ),
        ("/v1/storage/archive/refresh", serde_json::json!({})),
        (
            "/v1/storage/archive",
            serde_json::json!({"confirm_bucket":"bucket", "acknowledge_billing":true,"exclusive_bucket":true}),
        ),
        (
            "/v1/cartridges/TAPE01/reintegrate",
            serde_json::json!({"restore_job_id":"running-job"}),
        ),
        (
            "/v1/reintegrations/00000000-0000-0000-0000-000000000000/rollback",
            serde_json::json!({}),
        ),
    ] {
        assert_eq!(
            call(&app, "POST", path, viewer.clone(), payload.clone())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "POST", path, admin.clone(), payload).await.0,
            StatusCode::CONFLICT
        );
    }
    for action in ["enter", "leave"] {
        let path = format!("/v1/maintenance/{action}");
        assert_eq!(
            call(&app, "POST", &path, viewer.clone(), serde_json::json!({}))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let (status, _, body) =
            call(&app, "POST", &path, admin.clone(), serde_json::json!({})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("every active VTL job")
        );
    }
    auth.pool.close().await;
    std::fs::remove_dir_all(folder).unwrap();
}

#[tokio::test]
async fn s3_settings_auth_redaction_probe_failure_rotation_and_destination_isolation() {
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("vtl-settings-{}", Uuid::new_v4()));
    let path = root.join("catalog.db");
    let catalog = crate::catalog::Catalog::open(&path).await.unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let worker = root.join("worker");
    std::fs::write(
        &worker,
        r#"#!/usr/bin/python3
import json,sys
assert sys.argv[1:]==['config-probe']
c=json.load(sys.stdin)
if c['bucket']=='denied-bucket':
    print(c['secret_access_key'],file=sys.stderr)
    sys.exit(1)
print(json.dumps({'status':'ok','secret_access_key':c['secret_access_key']}))
"#,
    )
    .unwrap();
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    create_local_user(&auth.pool, "admin", PASSWORD.into(), "admin")
        .await
        .unwrap();
    create_local_user(&auth.pool, "reader", PASSWORD.into(), "viewer")
        .await
        .unwrap();
    let state = crate::AppState {
        catalog: catalog.clone(),
        operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
        config: Arc::new(crate::AppConfig {
            storage_socket: None,
            media_root: root.clone(),
            restore_root: root.clone(),
            worker_path: worker,
            quiesce_check: None,
            maintenance_helper: None,
            cartridge_helper: None,
            reintegration_helper: None,
            archive_helper: None,
            telemetry_dir: None,
            telemetry_retention_days: 30,
        }),
    };
    let app = crate::app(state.clone(), None, auth);
    let (admin, _) = session(&app, "admin").await;
    let (viewer, _) = session(&app, "reader").await;
    let config = json!({"endpoint":"https://s3.example.test","region":"test-region","bucket":"old-bucket","prefix":"vtl","access_key_id":"example-key","secret_access_key":"test-secret-not-for-responses"});
    sqlx::query("INSERT INTO s3_settings(id,revision,configuration,actor) VALUES(1,1,?,'test')")
        .bind(config.to_string())
        .execute(&catalog.pool())
        .await
        .unwrap();
    let mut update = config.clone();
    update["revision"] = json!(1);
    for (method, route) in [
        ("GET", "/v1/settings/s3"),
        ("POST", "/v1/settings/s3/test"),
        ("PUT", "/v1/settings/s3"),
    ] {
        assert_eq!(
            call(&app, method, route, viewer.clone(), update.clone())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let mut no_csrf = admin.clone();
    no_csrf.remove("x-csrf-token");
    assert_eq!(
        call(&app, "PUT", "/v1/settings/s3", no_csrf, update.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // Exercise the actual scoped router: none of these new entry points may
    // reach a provider or reserve a job without administrator and CSRF checks.
    let base = "/v1/vtls/00000000-0000-4000-8000-000000000001/storage";
    for (method, suffix) in [
        ("PUT", "connection"),
        ("POST", "buckets/attach"),
        ("POST", "buckets/create"),
        ("POST", "recovery/check"),
        ("POST", "recovery"),
    ] {
        let route = format!("{base}/{suffix}");
        assert_eq!(
            call(&app, method, &route, viewer.clone(), json!({}))
                .await
                .0,
            StatusCode::FORBIDDEN,
            "viewer {route}"
        );
        let mut missing_csrf = admin.clone();
        missing_csrf.remove("x-csrf-token");
        assert_eq!(
            call(&app, method, &route, missing_csrf, json!({})).await.0,
            StatusCode::FORBIDDEN,
            "CSRF {route}"
        );
    }
    // Uncertain creations are settled with the provider: no manual route.
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{base}/buckets/resolve"),
            admin.clone(),
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    for suffix in ["connection", "buckets/discover"] {
        let route = format!("{base}/{suffix}");
        assert_eq!(
            call(&app, "GET", &route, viewer.clone(), json!({})).await.0,
            StatusCode::FORBIDDEN,
            "viewer {route}"
        );
    }
    for route in ["/v1/settings/s3", "/v1/settings/s3/test"] {
        let (status, _, value) = call(
            &app,
            if route.ends_with("test") {
                "POST"
            } else {
                "GET"
            },
            route,
            admin.clone(),
            update.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(!value.to_string().contains("test-secret"));
        assert!(value.get("secret_access_key").is_none());
    }
    assert_eq!(
        state
            .catalog
            .destination()
            .await
            .unwrap()
            .revision
            .unwrap_or(0),
        1
    );
    let (_, _, settings) = call(&app, "GET", "/v1/settings/s3", admin.clone(), json!({})).await;
    assert_eq!(settings["configuration"]["provider"], "s3_compatible");
    assert_eq!(settings["providers"].as_array().unwrap().len(), 2);
    let mut invalid_provider = update.clone();
    invalid_provider["provider"] = json!("aws_glacier_not_implemented");
    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/settings/s3/test",
            admin.clone(),
            invalid_provider
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    for (provider, endpoint) in [
        ("ovh_cold_archive_v1", "https://s3.example.test"),
        ("s3_compatible", cold_vtl_core::OVH_ARCHIVE_ENDPOINT),
    ] {
        let mut invalid_provider = update.clone();
        invalid_provider["provider"] = json!(provider);
        invalid_provider["endpoint"] = json!(endpoint);
        assert_eq!(
            call(
                &app,
                "PUT",
                "/v1/settings/s3",
                admin.clone(),
                invalid_provider
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut denied = update.clone();
    denied["bucket"] = json!("denied-bucket");
    let (status, _, value) = call(&app, "PUT", "/v1/settings/s3", admin.clone(), denied).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(!value.to_string().contains("test-secret"));
    assert_eq!(
        state
            .catalog
            .destination()
            .await
            .unwrap()
            .revision
            .unwrap_or(0),
        1
    );
    let mut invalid = update.clone();
    invalid["endpoint"] = json!("https://user:password@s3.example.test");
    assert_eq!(
        call(&app, "POST", "/v1/settings/s3/test", admin.clone(), invalid)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let barcode = cold_vtl_core::Barcode::parse("TAPE01").unwrap();
    let media = cold_vtl_core::Cartridge::new(barcode.clone(), 1024).unwrap();
    catalog.create_cartridge(&media).await.unwrap();
    sqlx::query("UPDATE cartridges SET state='verified'")
        .execute(&catalog.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO remote_generations(cartridge_id,barcode,generation,metadata) SELECT id,barcode,1,'{}' FROM cartridges WHERE barcode='TAPE01'",
    )
    .execute(&catalog.pool())
    .await
    .unwrap();
    update["secret_access_key"] = json!(null);
    let (status, _, value) = call(
        &app,
        "PUT",
        "/v1/settings/s3",
        admin.clone(),
        update.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["destination_changed"], false);
    assert_eq!(
        catalog.cartridge(&barcode).await.unwrap().unwrap().state,
        cold_vtl_core::CartridgeState::Verified
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        call(
            &app,
            "PUT",
            "/v1/settings/s3",
            admin.clone(),
            update.clone()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    update["revision"] = json!(2);
    let mut rotated = update.clone();
    rotated["access_key_id"] = json!("another-key");
    assert_eq!(
        call(
            &app,
            "PUT",
            "/v1/settings/s3",
            admin.clone(),
            rotated.clone()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    rotated["secret_access_key"] = json!("rotated-secret");
    assert_eq!(
        call(
            &app,
            "PUT",
            "/v1/settings/s3",
            admin.clone(),
            rotated.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    rotated["revision"] = json!(3);
    sqlx::query("INSERT INTO jobs(id,kind,subject,status) VALUES('p','archive_prepare','bucket','completed'),('s','archive_submit','bucket','completed')").execute(&catalog.pool()).await.unwrap();
    sqlx::query("INSERT INTO archive_requests(job_id,prepare_job_id,phase,request) VALUES('s','p','accepted','{}')").execute(&catalog.pool()).await.unwrap();
    let mut destination_change = rotated.clone();
    destination_change["bucket"] = json!("new-bucket");
    assert_eq!(
        call(
            &app,
            "PUT",
            "/v1/settings/s3",
            admin.clone(),
            destination_change
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    // Credentials can be repaired for the same destination while OVH is processing.
    assert_eq!(
        call(
            &app,
            "PUT",
            "/v1/settings/s3",
            admin.clone(),
            rotated.clone()
        )
        .await
        .0,
        StatusCode::OK
    );
    rotated["revision"] = json!(4);

    sqlx::query("UPDATE archive_requests SET phase='archived'")
        .execute(&catalog.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET status='running' WHERE id='s'")
        .execute(&catalog.pool())
        .await
        .unwrap();
    assert_eq!(
        call(
            &app,
            "PUT",
            "/v1/settings/s3",
            admin.clone(),
            rotated.clone()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    sqlx::query("UPDATE jobs SET status='completed' WHERE id='s'")
        .execute(&catalog.pool())
        .await
        .unwrap();
    rotated["bucket"] = json!("new-bucket");
    let (status, _, value) = call(&app, "PUT", "/v1/settings/s3", admin.clone(), rotated).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["destination_changed"], true);
    assert!(catalog.generations(&barcode).await.unwrap().is_empty());
    assert!(
        crate::s3_settings::require_reconciled(&state, "TAPE01")
            .await
            .is_err()
    );
    assert!(catalog.job("s").await.unwrap().is_some());
    catalog
        .reserve_job("reconcile", "reconcile", &media, None, None)
        .await
        .unwrap();
    catalog
        .finish_job(
            "reconcile",
            "completed",
            "reconciled",
            Some(&media),
            Some(&[]),
        )
        .await
        .unwrap();
    assert!(
        crate::s3_settings::require_reconciled(&state, "TAPE01")
            .await
            .is_ok()
    );
    let (_, _, value) = call(&app, "GET", "/v1/settings/s3", admin, json!({})).await;
    assert_eq!(value["configuration"]["bucket"], "new-bucket");
    assert_eq!(value["configuration"]["provider"], "s3_compatible");
    assert!(!value.to_string().contains("rotated-secret"));
    catalog.pool().close().await;
    let reopened = crate::catalog::Catalog::open(&path).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT revision FROM s3_settings")
            .fetch_one(&reopened.pool())
            .await
            .unwrap(),
        5
    );
    reopened.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn multi_vtl_routes_require_context_and_preserve_roles() {
    use cold_vtl_core::{Barcode, Cartridge, VtlId};
    use serde_json::json;
    let root = std::env::temp_dir().join(format!("cold-vtl-http-{}", uuid::Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
        .await
        .unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    for (name, role) in [("admin", "admin"), ("reader", "viewer")] {
        create_local_user(&auth.pool, name, PASSWORD.into(), role)
            .await
            .unwrap();
    }
    let first = catalog.resolve_vtl(None).await.unwrap();
    let second = VtlId::new();
    sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'Second','ready')")
        .bind(second.to_string())
        .execute(&catalog.pool())
        .await
        .unwrap();
    let b = catalog.for_vtl(second).await.unwrap();
    sqlx::query(
        "INSERT INTO storage_destinations(id,vtl_id,active) VALUES('second-destination',?,1)",
    )
    .bind(second.to_string())
    .execute(&catalog.pool())
    .await
    .unwrap();
    let cartridge = Cartridge::new(Barcode::parse("TAPEB1").unwrap(), 100).unwrap();
    b.create_cartridge(&cartridge).await.unwrap();
    b.reserve_job("job-b", "reconcile", &cartridge, None, None)
        .await
        .unwrap();
    let state = crate::AppState {
        catalog: catalog.clone(),
        operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
        config: Arc::new(crate::AppConfig {
            storage_socket: None,
            media_root: root.join("media"),
            restore_root: root.join("restore"),
            worker_path: root.join("missing-worker"),
            quiesce_check: None,
            maintenance_helper: None,
            cartridge_helper: None,
            reintegration_helper: None,
            archive_helper: None,
            telemetry_dir: None,
            telemetry_retention_days: 30,
        }),
    };
    let app = crate::app(state, None, auth);
    let (viewer, _) = session(&app, "reader").await;
    for (path, expected) in [
        ("/v1/vtls".into(), StatusCode::OK),
        (format!("/v1/vtls/{second}/cartridges"), StatusCode::OK),
        (
            format!("/v1/vtls/{first}/jobs/job-b"),
            StatusCode::NOT_FOUND,
        ),
        (format!("/v1/vtls/{second}/jobs/job-b"), StatusCode::OK),
        (
            format!("/v1/vtls/{first}/cartridges/TAPEB1"),
            StatusCode::NOT_FOUND,
        ),
        (
            format!("/v1/vtls/{}/jobs", VtlId::new()),
            StatusCode::NOT_FOUND,
        ),
        ("/v1/vtls/invalid/jobs".into(), StatusCode::BAD_REQUEST),
        ("/v1/cartridges".into(), StatusCode::CONFLICT),
        ("/v1/storage".into(), StatusCode::CONFLICT),
        ("/v1/maintenance".into(), StatusCode::CONFLICT),
    ] {
        let (status, _, value) = call(&app, "GET", &path, viewer.clone(), json!({})).await;
        assert_eq!(status, expected, "{path}: {value}");
    }
    let (_, _, media) = call(
        &app,
        "GET",
        &format!("/v1/vtls/{first}/cartridges"),
        viewer.clone(),
        json!({}),
    )
    .await;
    assert_eq!(media, json!([]));
    // Cartridge history must filter before applying the global feed's 100-job cap.
    sqlx::query("INSERT INTO jobs(id,vtl_id,kind,subject,status) VALUES('old-target',?,'export','OLD001L8','completed'),('foreign-target',?,'export','OLD001L8','completed'),('target-restore',?,'restore','OLD001L8','completed')")
        .bind(first.to_string()).bind(second.to_string()).bind(first.to_string()).execute(&catalog.pool()).await.unwrap();
    sqlx::query("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<105) INSERT INTO jobs(id,vtl_id,kind,subject,status) SELECT 'new-'||x,?,'export','OTHER001','completed' FROM n")
        .bind(first.to_string()).execute(&catalog.pool()).await.unwrap();
    let (status, _, exports) = call(
        &app,
        "GET",
        &format!("/v1/vtls/{first}/jobs?barcode=OLD001L8&kind=export"),
        viewer.clone(),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        exports.as_array().unwrap().len(),
        1,
        "older target exports must survive unrelated recent jobs"
    );
    assert_eq!(exports[0]["id"], "old-target");
    let (_, _, general) = call(
        &app,
        "GET",
        &format!("/v1/vtls/{first}/jobs"),
        viewer.clone(),
        json!({}),
    )
    .await;
    assert_eq!(general.as_array().unwrap().len(), 100);
    for query in [
        "barcode=bad%2F&kind=export",
        "barcode=OLD001L8&kind=invalid",
    ] {
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("/v1/vtls/{first}/jobs?{query}"),
                viewer.clone(),
                json!({})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }

    // The copy history is readable by a viewer and scoped to its VTL.
    sqlx::query("INSERT INTO remote_generations(vtl_id,destination_id,barcode,generation,metadata,cartridge_id) SELECT vtl_id,'second-destination',barcode,1,'{}',id FROM cartridges WHERE vtl_id=? AND barcode='TAPEB1'")
        .bind(second.to_string()).execute(&catalog.pool()).await.unwrap();
    let (status, _, history) = call(
        &app,
        "GET",
        &format!("/v1/vtls/{second}/storage/history"),
        viewer.clone(),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{history}");
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0]["barcode"], "TAPEB1");
    assert_eq!(history[0]["bucket_state"], "available");
    let (_, _, first_history) = call(
        &app,
        "GET",
        &format!("/v1/vtls/{first}/storage/history"),
        viewer.clone(),
        json!({}),
    )
    .await;
    assert_eq!(first_history, json!([]));
    let (status, headers, _) = call(
        &app,
        "GET",
        &format!("/v1/vtls/{second}/storage/history?format=csv"),
        viewer.clone(),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "text/csv; charset=utf-8");
    assert!(
        headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment; filename=\"historique-second-")
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/v1/vtls/{second}/storage/history?format=xml"),
            viewer.clone(),
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );

    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/vtls",
            viewer,
            json!({"name":"Unauthorized"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", "/v1/vtls", HeaderMap::new(), json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn vtl_creation_persists_draft_without_disrupting_existing_library() {
    use serde_json::json;
    let root = std::env::temp_dir().join(format!("cold-vtl-create-{}", uuid::Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
        .await
        .unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    create_local_user(&auth.pool, "admin", PASSWORD.into(), "admin")
        .await
        .unwrap();
    let original = catalog.resolve_vtl(None).await.unwrap();
    let app = crate::app(
        crate::AppState {
            catalog: catalog.clone(),
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(crate::AppConfig {
                storage_socket: None,
                media_root: root.join("media"),
                restore_root: root.join("restore"),
                worker_path: root.join("missing-worker"),
                quiesce_check: None,
                maintenance_helper: None,
                cartridge_helper: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        },
        None,
        auth,
    );
    let (admin, _) = session(&app, "admin").await;
    for body in [
        json!({"name":" "}),
        json!({"name":"a".repeat(101)}),
        json!({"name":"test","media_root":"/etc"}),
    ] {
        let status = call(&app, "POST", "/v1/vtls", admin.clone(), body).await.0;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
            "{status}"
        );
    }
    let (status, _, created) = call(
        &app,
        "POST",
        "/v1/vtls",
        admin.clone(),
        json!({"name":"  Archives secondaires  "}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["name"], "Archives secondaires");
    assert_eq!(created["lifecycle"], "draft");
    let scoped_prefix = format!("/v1/vtls/{}", created["id"].as_str().unwrap());
    let (status, _, snapshot) = call(
        &app,
        "GET",
        &format!("{scoped_prefix}/status"),
        admin.clone(),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{snapshot}");
    assert_eq!(snapshot["cartridges"], 0);
    for (method, suffix) in [
        ("GET", "maintenance"),
        ("GET", "storage"),
        ("POST", "maintenance/enter"),
        ("POST", "cartridges/TAPE01/export"),
    ] {
        let (status, _, body) = call(
            &app,
            method,
            &format!("{scoped_prefix}/{suffix}"),
            admin.clone(),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{suffix}: {body}");
    }
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/v1/vtls/{original}/maintenance"),
            admin.clone(),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );

    assert!(uuid::Uuid::parse_str(created["id"].as_str().unwrap()).is_ok());
    let (_, _, libraries) = call(&app, "GET", "/v1/vtls", admin.clone(), json!({})).await;
    assert_eq!(libraries.as_array().unwrap().len(), 2);
    assert_eq!(catalog.resolve_vtl(None).await.unwrap(), original);
    assert_eq!(
        call(&app, "GET", "/v1/cartridges", admin.clone(), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/vtls",
            admin,
            json!({"name":"archives SECONDAIRES"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let empty: (Option<String>, Option<String>) =
        sqlx::query_as("SELECT media_root,restore_root FROM vtls WHERE id=?")
            .bind(created["id"].as_str().unwrap())
            .fetch_one(&catalog.pool())
            .await
            .unwrap();
    assert_eq!(empty, (None, None));
    catalog.pool().close().await;
    let reopened = crate::catalog::Catalog::open(&root.join("catalog.db"))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM vtls")
            .fetch_one(&reopened.pool())
            .await
            .unwrap(),
        2
    );
    reopened.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn multi_vtl_s3_configuration_is_scoped_and_preserves_destinations() {
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("cold-vtl-s3-multi-{}", uuid::Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
        .await
        .unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let worker = root.join("probe");
    std::fs::write(
        &worker,
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"status\":\"ok\"}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    for (name, role) in [("admin", "admin"), ("reader", "viewer")] {
        create_local_user(&auth.pool, name, PASSWORD.into(), role)
            .await
            .unwrap();
    }
    let app = crate::app(
        crate::AppState {
            catalog: catalog.clone(),
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(crate::AppConfig {
                storage_socket: None,
                media_root: root.join("media"),
                restore_root: root.join("restore"),
                worker_path: worker,
                quiesce_check: None,
                maintenance_helper: None,
                cartridge_helper: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        },
        None,
        auth,
    );
    let (admin, _) = session(&app, "admin").await;
    let (viewer, _) = session(&app, "reader").await;
    let mut config = json!({"provider":"s3_compatible","endpoint":"https://s3.example.test","region":"test-region","bucket":"old-bucket","prefix":"a","access_key_id":"test-key","secret_access_key":"secret-one"});
    sqlx::query("INSERT INTO s3_settings(id,revision,configuration,actor) VALUES(1,1,?,'test')")
        .bind(config.to_string())
        .execute(&catalog.pool())
        .await
        .unwrap();
    let mut ids = Vec::new();
    for name in ["Second", "Third"] {
        let (_, _, v) = call(
            &app,
            "POST",
            "/v1/vtls",
            admin.clone(),
            json!({"name":name}),
        )
        .await;
        ids.push(v["id"].as_str().unwrap().to_owned());
    }
    let b = format!("/v1/vtls/{}/settings/s3", ids[0]);
    let c = format!("/v1/vtls/{}/settings/s3", ids[1]);
    let (status, _, initial) = call(&app, "GET", &b, admin.clone(), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{initial}");
    assert_eq!(initial["configured"], false);
    assert_eq!(initial["revision"], 0);
    assert_eq!(
        call(&app, "GET", &b, viewer.clone(), json!({})).await.0,
        StatusCode::FORBIDDEN
    );
    config["revision"] = json!(0);
    config["prefix"] = json!("a/b");
    config["endpoint"] = json!("https://S3.EXAMPLE.TEST:443/");
    assert_eq!(
        call(&app, "PUT", &b, admin.clone(), config.clone()).await.0,
        StatusCode::CONFLICT
    );
    config["prefix"] = json!("ab");
    let (status, _, saved) = call(&app, "PUT", &b, admin.clone(), config.clone()).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let first = saved["destination_id"].as_str().unwrap().to_owned();
    assert!(!saved.to_string().contains("secret-one"));
    // Work on the legacy VTL must not block configuring B.
    sqlx::query("INSERT INTO jobs(id,kind,subject,status) VALUES('busy-legacy','reconcile','legacy','running')").execute(&catalog.pool()).await.unwrap();
    config["revision"] = json!(1);
    config["secret_access_key"] = json!("secret-two");
    let (status, _, rotated) = call(&app, "PUT", &b, admin.clone(), config.clone()).await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    assert_eq!(rotated["destination_id"], first);
    assert_eq!(
        call(&app, "PUT", &b, admin.clone(), config.clone()).await.0,
        StatusCode::CONFLICT
    );
    let scoped = catalog.for_vtl(ids[0].parse().unwrap()).await.unwrap();
    let tape = cold_vtl_core::Cartridge {
        media_type: None,
        segment_size_mib: 256,
        multipart_part_size_mib: 256,
        barcode: cold_vtl_core::Barcode::parse("TAPE01").unwrap(),
        capacity_bytes: 100,
        bytes_written: 10,
        generation: 1,
        state: cold_vtl_core::CartridgeState::Sealed,
    };
    scoped.create_cartridge(&tape).await.unwrap();
    sqlx::query("INSERT INTO remote_generations(vtl_id,cartridge_id,barcode,destination_id,generation,metadata) SELECT vtl_id,id,barcode,?,1,'{}' FROM cartridges WHERE vtl_id=?").bind(&first).bind(&ids[0]).execute(&catalog.pool()).await.unwrap();
    config["revision"] = json!(2);
    config["bucket"] = json!("new-bucket");
    let (status, _, changed) = call(&app, "PUT", &b, admin.clone(), config.clone()).await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_ne!(changed["destination_id"], first);
    assert_eq!(
        scoped
            .cartridge(&tape.barcode)
            .await
            .unwrap()
            .unwrap()
            .state,
        cold_vtl_core::CartridgeState::Sealed
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM remote_generations WHERE destination_id=?"
        )
        .bind(&first)
        .fetch_one(&catalog.pool())
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM storage_destination_revisions WHERE destination_id=?"
        )
        .bind(&first)
        .fetch_one(&catalog.pool())
        .await
        .unwrap(),
        2
    );
    config["revision"] = json!(0);
    config["bucket"] = json!("old-bucket");
    assert_eq!(
        call(&app, "PUT", &c, admin.clone(), config.clone()).await.0,
        StatusCode::CONFLICT
    );
    config["revision"] = json!(3);
    config["bucket"] = json!("ovh-bucket");
    config["provider"] = json!("ovh_cold_archive_v1");
    config["endpoint"] = json!(cold_vtl_core::OVH_ARCHIVE_ENDPOINT);
    config["region"] = json!("rbx-archive");
    assert_eq!(
        call(&app, "PUT", &b, admin.clone(), config.clone()).await.0,
        StatusCode::OK
    );
    config["revision"] = json!(0);
    config["prefix"] = json!("distinct");
    assert_eq!(
        call(&app, "PUT", &c, admin.clone(), config).await.0,
        StatusCode::CONFLICT
    );
    let (_, _, legacy) = call(&app, "GET", "/v1/settings/s3", admin.clone(), json!({})).await;
    assert_eq!(legacy["configuration"]["bucket"], "old-bucket");
    let (_, _, history) = call(
        &app,
        "GET",
        &format!("/v1/vtls/{}/destinations", ids[0]),
        admin,
        json!({}),
    )
    .await;
    assert_eq!(history.as_array().unwrap().len(), 3);
    assert!(!history.to_string().contains("secret-two"));
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn historical_destinations_are_viewer_safe_and_query_scoped_on_posts() {
    use cold_vtl_core::{Barcode, Cartridge, RemoteGeneration};
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("vtl-history-http-{}", uuid::Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
        .await
        .unwrap();
    let vtl = catalog.vtl_id().await.unwrap().to_string();
    let original = catalog.destination().await.unwrap().id;
    let identity = json!({"provider":"ovh_cold_archive_v1","endpoint":"https://s3.rbx-archive.io.cloud.ovh.net","region":"rbx-archive","bucket":"old-bucket","prefix":"vtl"});
    sqlx::query("UPDATE storage_destinations SET identity=?,active=0 WHERE id=?")
        .bind(identity.to_string())
        .bind(&original)
        .execute(&catalog.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO storage_destinations(id,vtl_id,identity,active) VALUES('active',?,?,1)",
    )
    .bind(&vtl)
    .bind(json!({"bucket":"new-bucket"}).to_string())
    .execute(&catalog.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO storage_destination_revisions(destination_id,revision,configuration) VALUES(?,1,?),('active',2,?)").bind(&original).bind(json!({"secret_access_key":"NEVER-DISCLOSE"}).to_string()).bind("{}").execute(&catalog.pool()).await.unwrap();
    let cartridge = Cartridge::new(Barcode::parse("TAPE01").unwrap(), 1024).unwrap();
    catalog.create_cartridge(&cartridge).await.unwrap();
    let generation = RemoteGeneration {
        barcode: cartridge.barcode.clone(),
        generation: 7,
        format_version: 2,
        manifest_key: "old/manifest".into(),
        manifest_sha256: "a".repeat(64),
        logical_size_bytes: 0,
        file_count: 0,
        segment_count: 0,
        local_match: false,
    };
    sqlx::query("INSERT INTO remote_generations(vtl_id,cartridge_id,barcode,destination_id,generation,metadata) SELECT vtl_id,id,barcode,?,7,? FROM cartridges WHERE vtl_id=? AND barcode='TAPE01'").bind(&original).bind(serde_json::to_string(&generation).unwrap()).bind(&vtl).execute(&catalog.pool()).await.unwrap();
    let worker = root.join("worker");
    std::fs::write(&worker,"#!/bin/sh\nprintf '{\"readable\":true,\"writable\":false,\"bucket\":\"old-bucket\",\"inventory\":{\"complete\":true,\"manifests\":[],\"cartridges\":[]}}'\n").unwrap();
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    for (name, role) in [("admin", "admin"), ("reader", "viewer")] {
        create_local_user(&auth.pool, name, PASSWORD.into(), role)
            .await
            .unwrap();
    }
    let app = crate::app(
        crate::AppState {
            catalog: catalog.clone(),
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(crate::AppConfig {
                storage_socket: None,
                media_root: root.join("media"),
                restore_root: root.join("restore"),
                worker_path: worker,
                quiesce_check: None,
                maintenance_helper: None,
                cartridge_helper: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        },
        None,
        auth,
    );
    let (viewer, _) = session(&app, "reader").await;
    for base in ["/v1".to_owned(), format!("/v1/vtls/{vtl}")] {
        let (status, _, buckets) = call(
            &app,
            "GET",
            &format!("{base}/storage/buckets"),
            viewer.clone(),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(buckets.as_array().unwrap().len(), 2);
        assert!(!buckets.to_string().contains("NEVER-DISCLOSE"));
        let (status, _, generations) = call(
            &app,
            "GET",
            &format!("{base}/cartridges/TAPE01/generations?destination_id={original}"),
            viewer.clone(),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(generations[0]["generation"], 7);
        assert_eq!(
            call(
                &app,
                "GET",
                &format!("{base}/cartridges/TAPE01/generations"),
                viewer.clone(),
                json!({})
            )
            .await
            .2,
            json!([])
        );
        let (status, _, probe) = call(
            &app,
            "GET",
            &format!("{base}/storage?destination_id={original}"),
            viewer.clone(),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(probe["destination_id"], original);
        assert_eq!(probe["active"], false);
        assert_eq!(probe["catalogue_generations"].as_array().unwrap().len(), 1);
    }
    let (admin, _) = session(&app, "admin").await;
    for base in ["/v1".to_owned(), format!("/v1/vtls/{vtl}")] {
        for (suffix, body) in [
            (
                "storage/archive",
                json!({"confirm_bucket":"old-bucket","acknowledge_billing":true,"exclusive_bucket":true}),
            ),
            ("storage/archive/refresh", json!({})),
            (
                "storage/recall",
                json!({"confirm_bucket":"old-bucket","acknowledge_retrieval_cost":true}),
            ),
            ("cartridges/TAPE01/reconcile", json!({})),
            ("cartridges/TAPE01/restore", json!({"generation":7})),
        ] {
            let (status, _, value) = call(
                &app,
                "POST",
                &format!("{base}/{suffix}?destination_id=foreign"),
                admin.clone(),
                body,
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{suffix}: {value}");
        }
        let (status, _, _) = call(
            &app,
            "POST",
            &format!("{base}/storage/archive?destination_id={original}"),
            admin.clone(),
            json!({"confirm_bucket":"old-bucket","acknowledge_billing":true,"exclusive_bucket":true}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        // Archive and recall are single requests: the preparation routes are gone.
        for removed in ["storage/archive/prepare", "storage/recall/prepare"] {
            let (status, _, _) = call(
                &app,
                "POST",
                &format!("{base}/{removed}?destination_id={original}"),
                admin.clone(),
                json!({}),
            )
            .await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{base}/{removed}");
        }
    }
    assert!(catalog.list_jobs().await.unwrap().is_empty());
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn telemetry_route_auth_scope_and_invalid_ids() {
    use cold_vtl_core::VtlId;
    use serde_json::json;
    let root = std::env::temp_dir().join(format!("telemetry-auth-{}", uuid::Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
        .await
        .unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    for (name, role) in [("admin", "admin"), ("reader", "viewer")] {
        create_local_user(&auth.pool, name, PASSWORD.into(), role)
            .await
            .unwrap();
    }
    let first = catalog.resolve_vtl(None).await.unwrap();
    let second = VtlId::new();
    sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'Second','ready')")
        .bind(second.to_string())
        .execute(&catalog.pool())
        .await
        .unwrap();
    let state = crate::AppState {
        catalog: catalog.clone(),
        operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
        config: Arc::new(crate::AppConfig {
            storage_socket: None,
            media_root: root.join("media"),
            restore_root: root.join("restore"),
            worker_path: root.join("missing"),
            quiesce_check: None,
            maintenance_helper: None,
            cartridge_helper: None,
            reintegration_helper: None,
            archive_helper: None,
            telemetry_dir: None,
            telemetry_retention_days: 30,
        }),
    };
    let app = crate::app(state, None, auth);
    let path = format!("/v1/vtls/{first}/telemetry");
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("{path}/history"),
            HeaderMap::new(),
            json!({})
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "GET", &path, HeaderMap::new(), json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    for user in ["admin", "reader"] {
        let (headers, _) = session(&app, user).await;
        for id in [first, second] {
            let (status, _, body) = call(
                &app,
                "GET",
                &format!("/v1/vtls/{id}/telemetry"),
                headers.clone(),
                json!({}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["vtl_id"], id.to_string());
            assert_eq!(body["status"], "unavailable");
            let (status,_,history)=call(&app,"GET",&format!("/v1/vtls/{id}/telemetry/history?barcode=SAME001L8&drive_id=11&window_seconds=120"),headers.clone(),json!({})).await;
            assert_eq!(status, StatusCode::OK, "{history}");
            assert_eq!(history["vtl_id"], id.to_string());
            assert_eq!(history["retention_days"], 30);
            assert_eq!(history["points"].as_array().unwrap().len(), 24);
            assert!(
                history["points"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|p| p["writeRate"].is_null() && p["localGrowth"].is_null())
            );
        }
        for (id, expected) in [
            (VtlId::new().to_string(), StatusCode::NOT_FOUND),
            ("invalid".into(), StatusCode::BAD_REQUEST),
        ] {
            assert_eq!(
                call(
                    &app,
                    "GET",
                    &format!("/v1/vtls/{id}/telemetry"),
                    headers.clone(),
                    json!({})
                )
                .await
                .0,
                expected
            );
            assert_eq!(
                call(
                    &app,
                    "GET",
                    &format!("/v1/vtls/{id}/telemetry/history"),
                    headers.clone(),
                    json!({})
                )
                .await
                .0,
                expected
            );
        }
        for query in [
            "window_seconds=1",
            "window_seconds=-1",
            "end_ms=-1",
            "end_ms=9223372036854775807",
            "barcode=..%2Fsecret",
            "drive_id=%0A",
            "kind=network",
            "transfer_id=unexpected",
            "extra=1",
        ] {
            assert_eq!(
                call(
                    &app,
                    "GET",
                    &format!("{path}/history?{query}"),
                    headers.clone(),
                    json!({})
                )
                .await
                .0,
                StatusCode::BAD_REQUEST,
                "{query}"
            );
        }
    }
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cartridge_creation_requires_admin_csrf_and_installed_vtl() {
    use serde_json::json;
    let folder = std::env::temp_dir().join(format!("cold-vtl-create-auth-{}", Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&folder.join("catalog.db"))
        .await
        .unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    create_local_user(&auth.pool, "admin", PASSWORD.into(), "admin")
        .await
        .unwrap();
    create_local_user(&auth.pool, "reader", PASSWORD.into(), "viewer")
        .await
        .unwrap();
    let draft = cold_vtl_core::VtlId::new();
    sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'draft','draft')")
        .bind(draft.to_string())
        .execute(&catalog.pool())
        .await
        .unwrap();
    let state = crate::AppState {
        catalog,
        operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
        config: Arc::new(crate::AppConfig {
            storage_socket: None,
            media_root: folder.join("media"),
            restore_root: folder.join("restores"),
            worker_path: folder.join("worker"),
            maintenance_helper: None,
            cartridge_helper: None,
            quiesce_check: None,
            reintegration_helper: None,
            archive_helper: None,
            telemetry_dir: None,
            telemetry_retention_days: 30,
        }),
    };
    let pool = state.catalog.pool();
    let app = crate::app(state, None, auth);
    let (admin, _) = session(&app, "admin").await;
    let (viewer, _) = session(&app, "reader").await;
    for base in [
        "/v1/cartridges",
        "/v1/vtls/00000000-0000-4000-8000-000000000001/cartridges",
    ] {
        for suffix in ["/delete-local", "/deletion"] {
            let route = format!("{base}{suffix}");
            let request = if suffix == "/deletion" {
                json!({"barcodes":["NEW001L8"]})
            } else {
                json!({"request_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","barcodes":["NEW001L8"],"confirmation":"delete-local"})
            };
            assert_eq!(
                call(&app, "POST", &route, HeaderMap::new(), request.clone())
                    .await
                    .0,
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                call(&app, "POST", &route, viewer.clone(), request.clone())
                    .await
                    .0,
                StatusCode::FORBIDDEN
            );
            let mut no_csrf = admin.clone();
            no_csrf.remove("x-csrf-token");
            assert_eq!(
                call(&app, "POST", &route, no_csrf, request).await.0,
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(call(&app,"POST",&format!("{base}/delete-local"),admin.clone(),
            json!({"request_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","barcodes":["NEW001L8"],"confirmation":"delete-s3"})).await.0, StatusCode::BAD_REQUEST);
    }
    let body = json!({"barcode":"NEW001L8","media_type":"LTO8","capacity_bytes":2147483648_u64,"segment_size_mib":64});
    for route in [
        "/v1/cartridges".to_owned(),
        "/v1/vtls/00000000-0000-4000-8000-000000000001/cartridges".to_owned(),
    ] {
        assert_eq!(
            call(&app, "POST", &route, HeaderMap::new(), body.clone())
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "POST", &route, viewer.clone(), body.clone())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        let mut no_csrf = admin.clone();
        no_csrf.remove("x-csrf-token");
        assert_eq!(
            call(&app, "POST", &route, no_csrf, body.clone()).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "POST", &route, admin.clone(), body.clone())
                .await
                .0,
            StatusCode::CONFLICT
        );
        let (status, _, options) = call(
            &app,
            "GET",
            &format!("{route}/options"),
            admin.clone(),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{options}");
        assert_eq!(options["configured"], false);
        assert_eq!(options["ready"], false);
        assert_eq!(options["default_segment_size_mib"], 1048576);
    }
    for (method, suffix, input) in [("POST", "", body), ("GET", "/options", json!({}))] {
        let (status, _, result) = call(
            &app,
            method,
            &format!("/v1/vtls/{draft}/cartridges{suffix}"),
            admin.clone(),
            input,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{result}");
    }
    sqlx::query("UPDATE vtls SET lifecycle='ready' WHERE id=?")
        .bind(draft.to_string())
        .execute(&pool)
        .await
        .unwrap();
    // A scoped deletion must reach its own extractor, even when the legacy
    // implicit VTL context is ambiguous. Invalid bodies must fail before helpers.
    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/vtls/00000000-0000-4000-8000-000000000001/cartridges/delete-local",
            admin.clone(),
            json!({})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        call(&app, "GET", "/v1/cartridges/deletion", admin, json!({}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    pool.close().await;
    std::fs::remove_dir_all(folder).unwrap();
}

#[tokio::test]
async fn notification_settings_are_admin_only_and_csrf_protected() {
    use serde_json::json;
    let folder = std::env::temp_dir().join(format!("vaultcenter-email-auth-{}", Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&folder.join("catalog.db"))
        .await
        .unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    for (name, role) in [("admin", "admin"), ("reader", "viewer")] {
        create_local_user(&auth.pool, name, PASSWORD.into(), role)
            .await
            .unwrap();
    }
    let app = crate::app(
        crate::AppState {
            catalog: catalog.clone(),
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(crate::AppConfig {
                storage_socket: None,
                media_root: folder.join("media"),
                worker_path: folder.join("absent"),
                restore_root: folder.join("restore"),
                quiesce_check: None,
                maintenance_helper: None,
                cartridge_helper: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        },
        None,
        auth,
    );
    let (admin_headers, _) = session(&app, "admin").await;
    let (viewer, _) = session(&app, "reader").await;
    for path in [
        "/v1/notifications/settings",
        "/v1/notifications/preview",
        "/v1/notifications/deliveries",
    ] {
        assert_eq!(
            call(&app, "GET", path, HeaderMap::new(), json!({})).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "GET", path, viewer.clone(), json!({})).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "GET", path, admin_headers.clone(), json!({}))
                .await
                .0,
            StatusCode::OK
        );
    }
    let (_, _, mut config) = call(
        &app,
        "GET",
        "/v1/notifications/settings",
        admin_headers.clone(),
        json!({}),
    )
    .await;
    config.as_object_mut().unwrap().remove("has_password");
    config["password"] = json!("do-not-return-secret");
    config["settings"]["enabled"] = json!(true);
    config["settings"]["host"] = json!("smtp.example.test");
    config["settings"]["sender"] = json!("from@example.test");
    config["settings"]["recipients"] = json!(["ops@example.test"]);
    let mut headers = admin_headers.clone();
    headers.insert(header::ORIGIN, HeaderValue::from_static("https://vtl.test"));
    let mut no_csrf = headers.clone();
    no_csrf.remove("x-csrf-token");
    assert_eq!(
        call(
            &app,
            "PUT",
            "/v1/notifications/settings",
            no_csrf,
            config.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let mut viewer = viewer;
    viewer.insert(header::ORIGIN, HeaderValue::from_static("https://vtl.test"));
    assert_eq!(
        call(
            &app,
            "PUT",
            "/v1/notifications/settings",
            viewer,
            config.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, _, saved) = call(
        &app,
        "PUT",
        "/v1/notifications/settings",
        headers.clone(),
        config.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(!saved.to_string().contains("do-not-return-secret"));
    assert_eq!(
        call(
            &app,
            "PUT",
            "/v1/notifications/settings",
            headers.clone(),
            config
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "POST",
            "/v1/notifications/test",
            headers.clone(),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "POST", "/v1/notifications/test", headers, json!({}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM notification_events")
            .fetch_one(&catalog.pool())
            .await
            .unwrap(),
        1
    );
    let _ = std::fs::remove_dir_all(folder);
}

#[tokio::test]
async fn tape_storage_settings_authorization_and_legacy_status() {
    use serde_json::json;
    let folder = std::env::temp_dir().join(format!("vaultcenter-storage-auth-{}", Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&folder.join("catalog.db"))
        .await
        .unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    for (name, role) in [("admin", "admin"), ("reader", "viewer")] {
        create_local_user(&auth.pool, name, PASSWORD.into(), role)
            .await
            .unwrap();
    }
    let app = crate::app(
        crate::AppState {
            catalog: catalog.clone(),
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(crate::AppConfig {
                storage_socket: None,
                media_root: folder.join("media"),
                worker_path: folder.join("absent"),
                restore_root: folder.join("restore"),
                quiesce_check: None,
                maintenance_helper: None,
                cartridge_helper: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        },
        None,
        auth,
    );
    let (mut admin, _) = session(&app, "admin").await;
    let (mut viewer, _) = session(&app, "reader").await;
    admin.insert(header::ORIGIN, HeaderValue::from_static("https://vtl.test"));
    viewer.insert(header::ORIGIN, HeaderValue::from_static("https://vtl.test"));
    let path = "/v1/settings/tape-storage";
    assert_eq!(
        call(&app, "GET", path, HeaderMap::new(), json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "GET", path, viewer.clone(), json!({})).await.0,
        StatusCode::FORBIDDEN
    );
    let (code, _, body) = call(&app, "GET", path, admin.clone(), json!({})).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["mode"], "legacy");
    for action in ["check", "apply"] {
        let url = format!("{path}/{action}");
        let body = if action == "check" {
            json!({"path":"/mnt/tapes"})
        } else {
            json!({"path":"/mnt/tapes","revision":0,"request_id":"id","check_id":"checked"})
        };
        let mut no_csrf = admin.clone();
        no_csrf.remove("x-csrf-token");
        assert_eq!(
            call(&app, "POST", &url, no_csrf, body.clone()).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "POST", &url, viewer.clone(), body.clone())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "POST", &url, admin.clone(), body).await.0,
            StatusCode::CONFLICT
        );
    }
    std::fs::remove_dir_all(folder).unwrap();
}

#[tokio::test]
async fn isolated_storage_http_scopes_drafts_locks_and_authorization() {
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let folder = std::env::temp_dir().join(format!("fst-http-{}", Uuid::new_v4()));
    let socket = std::env::temp_dir().join(format!("fst-{}", &Uuid::new_v4().to_string()[..8]));
    let catalog = crate::catalog::Catalog::open(&folder.join("catalog.db"))
        .await
        .unwrap();
    let a = catalog.vtl_id().await.unwrap().to_string();
    let b = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'B','draft')")
        .bind(&b)
        .execute(&catalog.pool())
        .await
        .unwrap();
    std::fs::write(folder.join("storage-isolated"), "{}").unwrap();
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let expected = a.clone();
    let host = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            BufReader::new(read).read_line(&mut line).await.unwrap();
            let input: serde_json::Value = serde_json::from_str(&line).unwrap();
            let id = input["vtl_id"].as_str().unwrap();
            let value = if input["action"] == "status" {
                json!({"ok":true,"result":{"vtl_id":id,"phase":"ready","mode":"managed","data_locked":id==expected,"change_allowed":id!=expected,"revision":1}})
            } else {
                json!({"ok":false,"error":{"message":"Chemin verrouillé : présence de données."}})
            };
            write
                .write_all(format!("{value}\n").as_bytes())
                .await
                .unwrap();
        }
    });
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    for (name, role) in [("admin", "admin"), ("reader", "viewer")] {
        create_local_user(&auth.pool, name, PASSWORD.into(), role)
            .await
            .unwrap();
    }
    let app = crate::app(
        crate::AppState {
            catalog: catalog.clone(),
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(crate::AppConfig {
                storage_socket: Some(socket.clone()),
                media_root: folder.join("media"),
                restore_root: folder.join("restores"),
                worker_path: folder.join("absent"),
                quiesce_check: None,
                maintenance_helper: None,
                cartridge_helper: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        },
        None,
        auth,
    );
    let (mut admin, _) = session(&app, "admin").await;
    admin.insert(header::ORIGIN, HeaderValue::from_static("https://vtl.test"));
    let (viewer, _) = session(&app, "reader").await;
    for (id, locked) in [(&a, true), (&b, false)] {
        let path = format!("/v1/vtls/{id}/settings/tape-storage");
        assert_eq!(
            call(&app, "GET", &path, HeaderMap::new(), json!({}))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "GET", &path, viewer.clone(), json!({})).await.0,
            StatusCode::FORBIDDEN
        );
        let (code, _, value) = call(&app, "GET", &path, admin.clone(), json!({})).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(value["vtl_id"], *id);
        assert_eq!(value["data_locked"], locked);
    }
    let path = format!("/v1/vtls/{a}/settings/tape-storage/apply");
    let body =
        json!({"path":"/mnt/replacement","revision":1,"request_id":"persisted","check_id":"old"});
    assert_eq!(
        call(&app, "POST", &path, admin.clone(), body.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut without_csrf = admin.clone();
    without_csrf.remove("x-csrf-token");
    assert_eq!(
        call(&app, "POST", &path, without_csrf, body).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &path,
            admin.clone(),
            json!({"path":"/mnt/a","device":"/dev/sda"})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    for suffix in ["prepare", "apply", "activate"] {
        assert_eq!(
            call(
                &app,
                "POST",
                &format!("/v1/vtls/{a}/{suffix}"),
                viewer.clone(),
                json!({})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        call(
            &app,
            "GET",
            "/v1/vtls/not-an-id/settings/tape-storage",
            admin.clone(),
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/v1/vtls/{}/settings/tape-storage", Uuid::new_v4()),
            admin.clone(),
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("UPDATE vtls SET media_root='/configured-b' WHERE id=?")
        .bind(&b)
        .execute(&catalog.pool())
        .await
        .unwrap();
    assert_eq!(
        call(&app, "GET", "/v1/settings/tape-storage", admin, json!({}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    host.abort();
    catalog.pool().close().await;
    std::fs::remove_file(socket).unwrap();
    std::fs::remove_dir_all(folder).unwrap();
}

#[tokio::test]
async fn bandwidth_and_pbs_connection_require_admin_and_csrf() {
    use serde_json::json;
    let folder =
        std::env::temp_dir().join(format!("vaultcenter-bandwidth-auth-{}", Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&folder.join("catalog.db"))
        .await
        .unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    for (name, role) in [("admin", "admin"), ("reader", "viewer")] {
        create_local_user(&auth.pool, name, PASSWORD.into(), role)
            .await
            .unwrap();
    }
    let app = crate::app(
        crate::AppState {
            catalog: catalog.clone(),
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(crate::AppConfig {
                storage_socket: None,
                media_root: folder.join("media"),
                worker_path: folder.join("absent"),
                restore_root: folder.join("restores"),
                quiesce_check: None,
                maintenance_helper: None,
                cartridge_helper: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        },
        None,
        auth,
    );
    let (viewer, _) = session(&app, "reader").await;
    let (mut admin, _) = session(&app, "admin").await;
    admin.remove("x-csrf-token");
    admin.insert(header::ORIGIN, HeaderValue::from_static("https://vtl.test"));
    for endpoint in ["bandwidth", "pbs-connection", "storage/schedule"] {
        let path = format!("/v1/vtls/00000000-0000-4000-8000-000000000001/{endpoint}");
        assert_eq!(
            call(&app, "GET", &path, HeaderMap::new(), json!({}))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&app, "GET", &path, viewer.clone(), json!({})).await.0,
            StatusCode::FORBIDDEN
        );
        // A viewer's session carries a valid CSRF token: the role alone refuses.
        assert_eq!(
            call(&app, "PUT", &path, viewer.clone(), json!({})).await.0,
            StatusCode::FORBIDDEN,
            "viewer PUT {path}"
        );
        assert_eq!(
            call(&app, "PUT", &path, admin.clone(), json!({})).await.0,
            StatusCode::FORBIDDEN
        );
    }
    catalog.pool().close().await;
    std::fs::remove_dir_all(folder).unwrap();
}

#[tokio::test]
async fn remote_copy_delete_is_confirmed_scoped_and_preserves_local_media() {
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("s3-delete-api-{}", Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
        .await
        .unwrap();
    let vtl = catalog.vtl_id().await.unwrap().to_string();
    let pin = catalog.destination().await.unwrap();
    sqlx::query("UPDATE storage_destinations SET identity='{}' WHERE id=?")
        .bind(&pin.id)
        .execute(&catalog.pool())
        .await
        .unwrap();
    let worker = root.join("worker");
    std::fs::write(&worker,r#"#!/usr/bin/env python3
import sys,json,time
assert sys.argv[1]=='delete-copy'
if '--execute' in sys.argv:time.sleep(.1)
print(json.dumps({'status':'deleted' if '--execute' in sys.argv else 'checked','bucket':'test-bucket','manifest_sha256':'a'*64,'objects':3}))
"#).unwrap();
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut cartridge =
        cold_vtl_core::Cartridge::new(cold_vtl_core::Barcode::parse("TAPE01").unwrap(), 1024)
            .unwrap();
    cartridge.state = cold_vtl_core::CartridgeState::Verified;
    catalog.create_cartridge(&cartridge).await.unwrap();
    std::fs::create_dir_all(root.join("media/TAPE01")).unwrap();
    std::fs::write(root.join("media/TAPE01/data"), b"local backup").unwrap();
    sqlx::query("INSERT INTO remote_generations(vtl_id,destination_id,barcode,generation,metadata,cartridge_id) VALUES(?,?,?,1,'{}',(SELECT id FROM cartridges WHERE barcode='TAPE01'))").bind(&vtl).bind(&pin.id).bind("TAPE01").execute(&catalog.pool()).await.unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    for (name, role) in [("admin", "admin"), ("reader", "viewer")] {
        create_local_user(&auth.pool, name, PASSWORD.into(), role)
            .await
            .unwrap();
    }
    let app = crate::app(
        crate::AppState {
            catalog: catalog.clone(),
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(crate::AppConfig {
                storage_socket: None,
                media_root: root.join("media"),
                worker_path: worker,
                restore_root: root.join("restore"),
                quiesce_check: None,
                maintenance_helper: None,
                cartridge_helper: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        },
        None,
        auth,
    );
    let path = format!("/v1/vtls/{vtl}/storage/copies/delete");
    let (admin, _) = session(&app, "admin").await;
    let (viewer, _) = session(&app, "reader").await;
    let payload = json!({"destination_id":pin.id,"configuration_revision":null,"barcode":"TAPE01","generation":1,"manifest_sha256":"a".repeat(64),"confirm_barcode":"TAPE01","confirm_bucket":"test-bucket"});
    assert_eq!(
        call(&app, "POST", &path, HeaderMap::new(), payload.clone())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "POST", &path, viewer, payload.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    let mut csrf = admin.clone();
    csrf.remove("x-csrf-token");
    assert_eq!(
        call(&app, "POST", &path, csrf, payload.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    for (field, value, expected) in [
        ("confirm_barcode", json!("wrong"), StatusCode::BAD_REQUEST),
        (
            "destination_id",
            json!("another-vtl"),
            StatusCode::NOT_FOUND,
        ),
        ("configuration_revision", json!(12), StatusCode::CONFLICT),
        (
            "confirm_bucket",
            json!("another-bucket"),
            StatusCode::CONFLICT,
        ),
    ] {
        let mut request = payload.clone();
        request[field] = value;
        assert_eq!(
            call(&app, "POST", &path, admin.clone(), request).await.0,
            expected,
            "{field}"
        );
    }
    assert!(catalog.list_jobs().await.unwrap().is_empty());
    sqlx::query("INSERT INTO jobs(id,vtl_id,kind,subject,status) VALUES('other',?,'export','OTHER1','running')").bind(&vtl).execute(&catalog.pool()).await.unwrap();
    assert_eq!(
        call(&app, "POST", &path, admin.clone(), payload.clone())
            .await
            .0,
        StatusCode::CONFLICT
    );
    sqlx::query("UPDATE jobs SET status='completed' WHERE id='other'")
        .execute(&catalog.pool())
        .await
        .unwrap();
    let (status, _, job) = call(&app, "POST", &path, admin, payload).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{job}");
    let id = job["id"].as_str().unwrap();
    for _ in 0..100 {
        if catalog.job(id).await.unwrap().unwrap().status == "completed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let job = catalog.job(id).await.unwrap().unwrap();
    assert_eq!(job.status, "completed", "{:?}", job.message);
    assert_eq!(job.destination_id, Some(pin.id.clone()));
    let local = catalog
        .cartridge(&cartridge.barcode)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(local.generation, 2);
    assert_eq!(local.state, cold_vtl_core::CartridgeState::Sealed);
    assert_eq!(
        std::fs::read(root.join("media/TAPE01/data")).unwrap(),
        b"local backup"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM remote_generations")
            .fetch_one(&catalog.pool())
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM s3_reconcile_required")
            .fetch_one(&catalog.pool())
            .await
            .unwrap(),
        1
    );
    // A remote-only copy has no local catalogue row; it still gets a scoped job.
    let (admin, _) = session(&app, "admin").await;
    let remote = json!({"destination_id":job.destination_id,"configuration_revision":null,"barcode":"REMOTE1","generation":3,"manifest_sha256":"a".repeat(64),"confirm_barcode":"REMOTE1","confirm_bucket":"test-bucket"});
    let (status, _, job) = call(&app, "POST", &path, admin, remote).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{job}");
    let id = job["id"].as_str().unwrap();
    for _ in 0..100 {
        if catalog.job(id).await.unwrap().unwrap().status == "completed" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(catalog.job(id).await.unwrap().unwrap().status, "completed");
    assert!(
        catalog
            .cartridge(&cold_vtl_core::Barcode::parse("REMOTE1").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn vtl_rename_preserves_identity_and_rejects_duplicates_stale_names_and_viewers() {
    use serde_json::json;
    let root = std::env::temp_dir().join(format!("vtl-rename-{}", Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
        .await
        .unwrap();
    let vtl = catalog.vtl_id().await.unwrap().to_string();
    let other = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO vtls(id,name,lifecycle) VALUES(?,'Archives','draft')")
        .bind(&other)
        .execute(&catalog.pool())
        .await
        .unwrap();
    let old: String = sqlx::query_scalar("SELECT name FROM vtls WHERE id=?")
        .bind(&vtl)
        .fetch_one(&catalog.pool())
        .await
        .unwrap();
    let before: (String, i64) =
        sqlx::query_as("SELECT lifecycle,provisioning_revision FROM vtls WHERE id=?")
            .bind(&vtl)
            .fetch_one(&catalog.pool())
            .await
            .unwrap();
    let destination = catalog.destination().await.unwrap().id;
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    for (name, role) in [("admin", "admin"), ("reader", "viewer")] {
        create_local_user(&auth.pool, name, PASSWORD.into(), role)
            .await
            .unwrap();
    }
    let app = crate::app(
        crate::AppState {
            catalog: catalog.clone(),
            operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
            config: Arc::new(crate::AppConfig {
                storage_socket: None,
                media_root: root.join("media"),
                worker_path: root.join("absent"),
                restore_root: root.join("restore"),
                quiesce_check: None,
                maintenance_helper: None,
                cartridge_helper: None,
                reintegration_helper: None,
                archive_helper: None,
                telemetry_dir: None,
                telemetry_retention_days: 30,
            }),
        },
        None,
        auth,
    );
    let (admin, _) = session(&app, "admin").await;
    let (viewer, _) = session(&app, "reader").await;
    let path = format!("/v1/vtls/{vtl}");
    let payload = json!({"name":"  Sauvegardes production  ","expected_name":old});
    assert_eq!(
        call(&app, "PATCH", &path, HeaderMap::new(), payload.clone())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "PATCH", &path, viewer, payload.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    let mut csrf = admin.clone();
    csrf.remove("x-csrf-token");
    assert_eq!(
        call(&app, "PATCH", &path, csrf, payload.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    for name in [
        "".to_owned(),
        "  ".into(),
        "x".repeat(101),
        "bad\nname".into(),
    ] {
        assert_eq!(
            call(
                &app,
                "PATCH",
                &path,
                admin.clone(),
                json!({"name":name,"expected_name":old})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        call(
            &app,
            "PATCH",
            &path,
            admin.clone(),
            json!({"name":"archives","expected_name":old})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            &format!("/v1/vtls/{}", Uuid::new_v4()),
            admin.clone(),
            payload.clone()
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // Renaming does not require idle workers or a provisioned hardware adapter.
    sqlx::query("INSERT INTO jobs(id,vtl_id,kind,subject,status) VALUES('running',?,'export','TAPE01','running')").bind(&vtl).execute(&catalog.pool()).await.unwrap();
    let (status, _, result) = call(&app, "PATCH", &path, admin.clone(), payload.clone()).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["name"], "Sauvegardes production");
    assert_eq!(result["id"], vtl);
    assert_eq!(
        call(&app, "PATCH", &path, admin, payload).await.0,
        StatusCode::CONFLICT
    );
    let after: (String, i64) =
        sqlx::query_as("SELECT lifecycle,provisioning_revision FROM vtls WHERE id=?")
            .bind(&vtl)
            .fetch_one(&catalog.pool())
            .await
            .unwrap();
    assert_eq!(before, after);
    assert_eq!(catalog.destination().await.unwrap().id, destination);
    assert_eq!(
        catalog.job("running").await.unwrap().unwrap().status,
        "running"
    );
    let (status, _, result) = call(
        &app,
        "GET",
        &path,
        session(&app, "reader").await.0,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["name"], "Sauvegardes production");
    catalog.pool().close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn activity_is_read_by_every_account_and_seen_separately() {
    let root = std::env::temp_dir().join(format!("cold-vtl-activity-{}", Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
        .await
        .unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    for (name, role) in [("admin", "admin"), ("reader", "viewer")] {
        create_local_user(&auth.pool, name, PASSWORD.into(), role)
            .await
            .unwrap();
    }
    let vtl = catalog.resolve_vtl(None).await.unwrap();
    sqlx::query("INSERT INTO jobs(id,vtl_id,kind,subject,status,progress) VALUES('done',?,'export','CVT001L8','completed',100)")
        .bind(vtl.to_string())
        .execute(&catalog.pool())
        .await
        .unwrap();
    let state = crate::AppState {
        catalog,
        operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
        config: Arc::new(crate::AppConfig {
            storage_socket: None,
            media_root: root.join("media"),
            restore_root: root.join("restore"),
            worker_path: root.join("missing-worker"),
            quiesce_check: None,
            maintenance_helper: None,
            cartridge_helper: None,
            reintegration_helper: None,
            archive_helper: None,
            telemetry_dir: None,
            telemetry_retention_days: 30,
        }),
    };
    let app = crate::app(state, None, auth.clone());
    let empty = serde_json::json!({});
    for (method, path) in [("GET", "/v1/activity"), ("POST", "/v1/activity/seen")] {
        let (status, _, _) = call(&app, method, path, HeaderMap::new(), empty.clone()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
    let (reader, _) = session(&app, "reader").await;
    let (admin, _) = session(&app, "admin").await;
    let (status, _, body) = call(&app, "GET", "/v1/activity", reader.clone(), empty.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["unread"], 1);
    assert_eq!(body["recent"][0]["id"], "job:done");
    assert_eq!(body["recent"][0]["outcome"], "success");
    let mut no_csrf = reader.clone();
    no_csrf.remove("x-csrf-token");
    let (status, _, _) = call(&app, "POST", "/v1/activity/seen", no_csrf, empty.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, body) = call(
        &app,
        "POST",
        "/v1/activity/seen",
        reader.clone(),
        empty.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["seen_at"].is_string(), "{body}");
    let (_, _, body) = call(&app, "GET", "/v1/activity", reader.clone(), empty.clone()).await;
    assert_eq!(body["unread"], 0);
    assert_eq!(body["recent"][0]["unread"], false);
    let (_, _, body) = call(&app, "GET", "/v1/activity", admin, empty.clone()).await;
    assert_eq!(body["unread"], 1);
    // Marking one's own activity as seen opens no other write to a viewer.
    let (status, _, _) = call(&app, "POST", "/v1/notifications/test", reader, empty).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    auth.pool.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn retiring_a_bucket_is_for_administrators() {
    let root = std::env::temp_dir().join(format!("cold-vtl-retire-{}", Uuid::new_v4()));
    let catalog = crate::catalog::Catalog::open(&root.join("catalog.db"))
        .await
        .unwrap();
    let auth = Auth::new(
        catalog.pool(),
        Config {
            public_url: "https://vtl.test".into(),
            secure: true,
            oidc: None,
        },
    )
    .await
    .unwrap();
    for (name, role) in [("admin", "admin"), ("reader", "viewer")] {
        create_local_user(&auth.pool, name, PASSWORD.into(), role)
            .await
            .unwrap();
    }
    let vtl = catalog.resolve_vtl(None).await.unwrap();
    let state = crate::AppState {
        catalog,
        operations: Arc::new(crate::vtl_runtime::OperationLocks::default()),
        config: Arc::new(crate::AppConfig {
            storage_socket: None,
            media_root: root.join("media"),
            restore_root: root.join("restore"),
            worker_path: root.join("missing-worker"),
            quiesce_check: None,
            maintenance_helper: None,
            cartridge_helper: None,
            reintegration_helper: None,
            archive_helper: None,
            telemetry_dir: None,
            telemetry_retention_days: 30,
        }),
    };
    let app = crate::app(state, None, auth.clone());
    let path = format!("/v1/vtls/{vtl}/storage/buckets/unknown/retire");
    let empty = serde_json::json!({});
    let (reader, _) = session(&app, "reader").await;
    let (admin, _) = session(&app, "admin").await;
    let (status, _, _) = call(&app, "POST", &path, reader, empty.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let mut no_csrf = admin.clone();
    no_csrf.remove("x-csrf-token");
    let (status, _, _) = call(&app, "POST", &path, no_csrf, empty.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, body) = call(&app, "POST", &path, admin, empty).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "Bucket not found for this VTL.");
    auth.pool.close().await;
    std::fs::remove_dir_all(root).unwrap();
}
