use axum::{
    Router,
    http::{HeaderValue, header},
};
use std::{io, path::Path};
use tower_http::{services::ServeDir, set_header::SetResponseHeader};

pub async fn validate_directory(path: &Path) -> io::Result<()> {
    if !tokio::fs::metadata(path.join("index.html"))
        .await?
        .is_file()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "COLD_VTL_UI_DIR must contain a built index.html (run npm run build in ui/)",
        ));
    }
    Ok(())
}

pub fn with_static_ui(router: Router, path: impl AsRef<Path>) -> Router {
    // Keep API routes authoritative; a missing asset or unknown API path is a
    // real 404, never the console HTML. ServeDir confines paths to this directory.
    let files = SetResponseHeader::if_not_present(
        ServeDir::new(path).append_index_html_on_directories(true),
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    router.fallback_service(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, routing::get};
    use tower::ServiceExt;

    #[tokio::test]
    async fn static_ui_preserves_api_and_missing_file_responses() {
        let root = std::env::temp_dir().join(format!("cold-vtl-web-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(root.join("_next/static"))
            .await
            .unwrap();
        tokio::fs::write(
            root.join("index.html"),
            "<!doctype html><title>VaultCenter</title>",
        )
        .await
        .unwrap();
        tokio::fs::write(root.join("_next/static/app.js"), "console.log('ui');")
            .await
            .unwrap();
        validate_directory(&root).await.unwrap();
        let router = with_static_ui(
            Router::new().route("/v1/status", get(|| async { "api" })),
            &root,
        );
        for (path, status, content_type) in [
            ("/", 200, "text/html"),
            ("/_next/static/app.js", 200, "javascript"),
            ("/v1/status", 200, "text/plain"),
            ("/v1/unknown", 404, ""),
            ("/_next/static/missing.js", 404, ""),
            ("/%2e%2e/Cargo.toml", 404, ""),
        ] {
            let response = router
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status().as_u16(), status, "{path}");
            if !content_type.is_empty() {
                assert!(
                    response.headers()[header::CONTENT_TYPE]
                        .to_str()
                        .unwrap()
                        .contains(content_type),
                    "{path}"
                );
            }
        }
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn invalid_ui_directory_is_rejected() {
        let path = std::env::temp_dir().join(format!("cold-vtl-missing-{}", uuid::Uuid::new_v4()));
        assert!(validate_directory(&path).await.is_err());
    }
}
