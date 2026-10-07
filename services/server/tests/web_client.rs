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
    let assets = TempDir::new().unwrap();
    fs::write(assets.path().join("index.html"), "<main>Peppy</main>").unwrap();
    fs::create_dir(assets.path().join("assets")).unwrap();
    fs::create_dir(assets.path().join("core")).unwrap();
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
    let config = WebClientConfig::new(
        host.into(),
        assets.path().into(),
        Url::parse("https://api.example.test/v1").unwrap(),
    )
    .unwrap();
    let inner = Router::new()
        .route("/v1/ping", get(|| async { "api" }))
        .route("/healthz", get(|| async { "healthy" }))
        .fallback(|| async { StatusCode::IM_A_TEAPOT });
    (assets, wrap(inner, config))
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
