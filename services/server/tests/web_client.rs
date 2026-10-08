use std::fs;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    routing::get,
};
use http_body_util::BodyExt;
use peppy_server::web_client::{WebClientConfig, wrap};
use tempfile::TempDir;
use tower::ServiceExt;
use url::Url;

fn app() -> (TempDir, Router) {
    app_for_host("App.Example.Test.")
}

fn app_for_host(host: &str) -> (TempDir, Router) {
    app_for_host_and_account_url(host, None)
}

fn assets() -> TempDir {
    let assets = TempDir::new().unwrap();
    fs::write(assets.path().join("index.html"), "<main>Peppy</main>").unwrap();
    fs::create_dir(assets.path().join("assets")).unwrap();
    fs::write(
        assets.path().join("assets/app-1234abcd.js"),
        "console.log(1)",
    )
    .unwrap();
    fs::write(
        assets.path().join("worker.js"),
        "self.onconnect = () => {};",
    )
    .unwrap();
    fs::create_dir(assets.path().join("core")).unwrap();
    fs::write(
        assets.path().join("core/peppy_browser_core.wasm"),
        [0, 97, 115, 109],
    )
    .unwrap();
    fs::write(
        assets.path().join("core/peppy-browser-core.js"),
        "export default {}",
    )
    .unwrap();
    fs::write(assets.path().join("assets/peppy.ttf"), []).unwrap();
    assets
}

fn app_for_config(config: WebClientConfig) -> Router {
    let inner = Router::new()
        .route("/v1/ping", get(|| async { "api" }))
        .route("/healthz", get(|| async { "healthy" }))
        .route("/readyz", get(|| async { "ready" }))
        .route("/__release", get(|| async { "release" }))
        .route("/file/x", get(|| async { "file" }))
        .fallback(|| async { StatusCode::IM_A_TEAPOT });
    wrap(inner, config)
}

fn root_app() -> (TempDir, Router) {
    let assets = assets();
    let config = WebClientConfig::for_all_hosts(
        assets.path().into(),
        Url::parse("http://localhost:7000/v1").unwrap(),
    )
    .unwrap();
    (assets, app_for_config(config))
}

fn app_for_host_and_account_url(host: &str, account_url: Option<&str>) -> (TempDir, Router) {
    let assets = assets();
    let config = WebClientConfig::new(
        host.into(),
        assets.path().into(),
        Url::parse("https://api.example.test/v1").unwrap(),
    )
    .unwrap();
    let config = match account_url {
        Some(account_url) => config
            .with_account_url(Url::parse(account_url).unwrap())
            .unwrap(),
        None => config,
    };
    (assets, app_for_config(config))
}

#[tokio::test]
async fn config_emits_an_optional_account_url() {
    let (_assets, app) = app_for_host_and_account_url(
        "app.example.test",
        Some("https://account.example.test/account"),
    );

    let response = app
        .oneshot(request("/web/config.json", Some("app.example.test"), "GET"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes().as_ref(),
        br#"{"accountUrl":"https://account.example.test/account","apiOrigin":"https://api.example.test/","version":1}"#,
    );
}

#[test]
fn account_url_must_be_a_safe_distinct_https_hostname() {
    let assets = TempDir::new().unwrap();
    let config = WebClientConfig::new(
        "app.example.test".into(),
        assets.path().into(),
        Url::parse("https://api.example.test").unwrap(),
    )
    .unwrap();
    let valid = config
        .clone()
        .with_account_url(Url::parse("https://account.example.test/account").unwrap())
        .unwrap();
    assert_eq!(
        valid.account_url.unwrap().as_str(),
        "https://account.example.test/account"
    );

    for account_url in [
        "http://account.example.test/account",
        "https://user@account.example.test/account",
        "https://account.example.test/account?next=/",
        "https://account.example.test/account#billing",
        "https://account.example.test/%0A",
        "https://app.example.test/account",
        "https://invalid_host.example.test/account",
    ] {
        assert!(
            config
                .clone()
                .with_account_url(Url::parse(account_url).unwrap())
                .is_err(),
            "{account_url}"
        );
    }

    let error = config
        .with_account_url(Url::parse("https://invalid_host.example.test/account").unwrap())
        .unwrap_err();
    assert!(error.contains("PEPPY_WEB_CLIENT_ACCOUNT_URL"));
    assert!(!error.contains("PEPPY_WEB_CLIENT_HOST must"));
}

fn request(path: &str, host: Option<&str>, method: &str) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(host) = host {
        builder = builder.header(header::HOST, host);
    }
    builder.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn serves_app_assets_config_and_api_only_on_the_configured_host() {
    let (_assets, app) = app();
    let response = app
        .clone()
        .oneshot(request("/", Some("APP.EXAMPLE.TEST."), "GET"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        response.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap(),
        "default-src 'self'; base-uri 'none'; frame-ancestors 'none'; object-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; worker-src 'self'; connect-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; font-src 'self'; manifest-src 'self'"
    );
    let worker = app
        .clone()
        .oneshot(request("/worker.js", Some("app.example.test"), "GET"))
        .await
        .unwrap();
    assert_eq!(worker.headers()[header::CACHE_CONTROL], "no-store");
    assert!(
        worker.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("'wasm-unsafe-eval'")
    );

    let config = app
        .clone()
        .oneshot(request("/web/config.json", Some("app.example.test"), "GET"))
        .await
        .unwrap();
    assert_eq!(config.status(), StatusCode::OK);
    assert_eq!(config.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        config
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        br#"{"apiOrigin":"https://api.example.test/","version":1}"#,
    );

    let api = app
        .clone()
        .oneshot(request("/v1/ping", Some("app.example.test"), "GET"))
        .await
        .unwrap();
    assert_eq!(api.status(), StatusCode::OK);
    let other_host = app
        .oneshot(request("/", Some("api.example.test"), "GET"))
        .await
        .unwrap();
    assert_eq!(other_host.status(), StatusCode::IM_A_TEAPOT);
}

#[tokio::test]
async fn root_mode_serves_all_hosts_without_parsing_host_and_keeps_api_errors() {
    let (_assets, app) = root_app();
    for host in [
        None,
        Some("community.example.test"),
        Some("bad host"),
        Some("app.example.test:99999"),
    ] {
        let response = app
            .clone()
            .oneshot(request("/", host, "GET"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{host:?}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(
            response
                .headers()
                .contains_key(header::CONTENT_SECURITY_POLICY)
        );
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
    }

    let config = app
        .clone()
        .oneshot(request("/web/config.json", None, "GET"))
        .await
        .unwrap();
    assert_eq!(config.status(), StatusCode::OK);
    assert_eq!(
        config
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        br#"{"apiOrigin":"http://localhost:7000/","version":1}"#,
    );

    let api_error = app
        .clone()
        .oneshot(request("/v1/missing", None, "GET"))
        .await
        .unwrap();
    assert_eq!(api_error.status(), StatusCode::IM_A_TEAPOT);
    assert_ne!(
        api_error.headers().get(header::CONTENT_TYPE),
        Some(&axum::http::HeaderValue::from_static(
            "text/html; charset=utf-8"
        ))
    );

    for (path, method, status) in [
        ("/v1/x", "POST", StatusCode::IM_A_TEAPOT),
        ("/healthz", "GET", StatusCode::OK),
        ("/readyz", "GET", StatusCode::OK),
        ("/__release", "GET", StatusCode::OK),
        ("/file/x", "GET", StatusCode::OK),
        ("/account", "GET", StatusCode::NOT_FOUND),
        ("/hosted", "GET", StatusCode::NOT_FOUND),
        ("/signin", "GET", StatusCode::NOT_FOUND),
    ] {
        let response = app
            .clone()
            .oneshot(request(path, None, method))
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{method} {path}");
    }

    let head = app
        .clone()
        .oneshot(request("/assets/app-1234abcd.js", None, "HEAD"))
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(
        head.headers()[header::CONTENT_TYPE],
        "text/javascript; charset=utf-8"
    );
    assert_eq!(
        head.headers()[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    assert!(head.headers().contains_key(header::CONTENT_SECURITY_POLICY));
    assert!(
        head.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );

    for (path, method, status) in [
        (
            "/assets/app-1234abcd.js",
            "POST",
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        ("/%2e%2e/secret", "GET", StatusCode::NOT_FOUND),
    ] {
        let response = app
            .clone()
            .oneshot(request(path, None, method))
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{method} {path}");
    }
}

#[tokio::test]
async fn rejects_reserved_paths_invalid_assets_and_static_methods() {
    let (assets, app) = app();
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc/passwd", assets.path().join("escape.txt")).unwrap();
    for path in [
        "/hosted",
        "/account/settings",
        "/signin",
        "/missing.js",
        "/assets/missing",
        "/core/missing",
        "/web/missing",
        "/../Cargo.toml",
        "/%2fetc/passwd",
        "/%2e%2e/secret",
    ] {
        let response = app
            .clone()
            .oneshot(request(path, Some("app.example.test"), "GET"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    #[cfg(unix)]
    {
        let response = app
            .clone()
            .oneshot(request("/escape.txt", Some("app.example.test"), "GET"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    let response = app
        .oneshot(request(
            "/assets/app-1234abcd.js",
            Some("app.example.test"),
            "POST",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn preserves_head_mime_cache_and_isolates_explicit_port_hosts() {
    let (_assets, app) = app();
    let response = app
        .clone()
        .oneshot(request(
            "/core/peppy_browser_core.wasm",
            Some("app.example.test"),
            "HEAD",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/wasm");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    let port_host = app
        .clone()
        .oneshot(request("/", Some("app.example.test:443"), "GET"))
        .await
        .unwrap();
    assert_eq!(port_host.status(), StatusCode::OK);
    let (_assets, localhost_app) = app_for_host("localhost");
    let localhost_port = localhost_app
        .oneshot(request("/", Some("localhost:5443"), "GET"))
        .await
        .unwrap();
    assert_eq!(localhost_port.status(), StatusCode::OK);
    let malformed_port = app
        .clone()
        .oneshot(request("/", Some("app.example.test:99999"), "GET"))
        .await
        .unwrap();
    assert_eq!(malformed_port.status(), StatusCode::NOT_FOUND);
    let missing_host = app.oneshot(request("/", None, "GET")).await.unwrap();
    assert_eq!(missing_host.status(), StatusCode::IM_A_TEAPOT);
}

#[tokio::test]
async fn caches_generated_assets_immutably_and_serves_ttf_fonts() {
    let (_assets, app) = app();
    let asset = app
        .clone()
        .oneshot(request(
            "/assets/app-1234abcd.js",
            Some("app.example.test"),
            "GET",
        ))
        .await
        .unwrap();
    assert_eq!(
        asset.headers()[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(
        asset
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        b"console.log(1)"
    );
    let font = app
        .oneshot(request(
            "/assets/peppy.ttf",
            Some("app.example.test"),
            "HEAD",
        ))
        .await
        .unwrap();
    assert_eq!(font.headers()[header::CONTENT_TYPE], "font/ttf");
}
