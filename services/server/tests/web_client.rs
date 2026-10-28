use std::fs;

use axum::{
    Router,
    body::Body,
    extract::Path,
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    routing::{get, post},
};
use http_body_util::BodyExt;
use peppy_server::web_client::{WebClientConfig, wrap};
use tempfile::TempDir;
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

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
        .route("/hosted/v1/enrollment/config", get(|| async { "config" }))
        .route(
            "/hosted/v1/web/app-session",
            post(|| async { StatusCode::CREATED }).delete(|| async { StatusCode::NO_CONTENT }),
        )
        .route(
            "/hosted/v1/web/app-session/renew",
            post(|| async { StatusCode::ACCEPTED }),
        )
        .route(
            "/hosted/v1/web/app-account",
            get(|| async { StatusCode::UNAUTHORIZED }),
        )
        .route(
            "/hosted/v1/web/provisioning",
            post(|| async { "provisioning" }),
        )
        .route(
            "/hosted/v1/web/provisioning/complete",
            post(|| async { "complete" }),
        )
        .route(
            "/hosted/v1/web/app-authorize",
            get(|| async { "authorize" }),
        )
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
async fn app_host_forwards_only_exact_enrollment_paths_with_method_unchanged() {
    let (_assets, app) = app();
    for (path, method, status) in [
        ("/hosted/v1/enrollment/config", "GET", StatusCode::OK),
        (
            "/hosted/v1/enrollment/config?probe=1",
            "GET",
            StatusCode::OK,
        ),
        ("/hosted/v1/web/app-session", "POST", StatusCode::CREATED),
        (
            "/hosted/v1/web/app-session",
            "DELETE",
            StatusCode::NO_CONTENT,
        ),
        (
            "/hosted/v1/web/app-session/renew",
            "POST",
            StatusCode::ACCEPTED,
        ),
        (
            "/hosted/v1/web/app-account",
            "GET",
            StatusCode::UNAUTHORIZED,
        ),
        ("/hosted/v1/web/provisioning", "POST", StatusCode::OK),
        (
            "/hosted/v1/web/provisioning/complete",
            "POST",
            StatusCode::OK,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(request(path, Some("app.example.test"), method))
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{method} {path}");
    }

    // Methods the backend does not route stay the backend's decision (405), not the SPA's.
    let wrong_method = app
        .clone()
        .oneshot(request(
            "/hosted/v1/enrollment/config",
            Some("app.example.test"),
            "POST",
        ))
        .await
        .unwrap();
    assert_eq!(wrong_method.status(), StatusCode::METHOD_NOT_ALLOWED);
    let no_spa_on_post = app
        .clone()
        .oneshot(request(
            "/hosted/v1/web/app-session",
            Some("app.example.test"),
            "PUT",
        ))
        .await
        .unwrap();
    assert_eq!(no_spa_on_post.status(), StatusCode::METHOD_NOT_ALLOWED);

    for path in [
        "/hosted",
        "/hosted/",
        "/hosted/v1",
        "/hosted/v1/enrollment",
        "/hosted/v1/enrollment/config/",
        "/hosted/v1/enrollment/config/x",
        "/hosted/v1/web/app-session/",
        "/hosted/v1/web/app-session/x",
        "/hosted/v1/web/app-account/",
        "/hosted/v1/web/provisioning/complete/x",
        "/hosted/v1/web/app-authorize",
        "/hosted/v1/web/account",
        "/hosted/v1/account/billing",
        "/hosted/v1/web/app-session%2F",
    ] {
        for method in ["GET", "POST", "DELETE"] {
            let response = app
                .clone()
                .oneshot(request(path, Some("app.example.test"), method))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
        }
    }
}

#[tokio::test]
async fn app_host_forwards_computer_claims_to_the_canonical_api_router() {
    let assets = assets();
    let config = WebClientConfig::new(
        "app.example.test".into(),
        assets.path().into(),
        Url::parse("https://api.example.test/v1").unwrap(),
    )
    .unwrap();
    let canonical_id = Uuid::parse_str("0f8fad5b-d9cb-469f-a165-70867728950e").unwrap();
    let inner = Router::new()
        .route(
            "/v1/enrollment/computers/{id}/status",
            post(move |Path(id): Path<Uuid>, headers: HeaderMap| async move {
                assert_eq!(id, canonical_id);
                assert_eq!(headers[header::COOKIE], "web-session=claimant");
                StatusCode::ACCEPTED
            }),
        )
        .route(
            "/v1/enrollment/computers/{id}/confirm",
            post(move |Path(id): Path<Uuid>, headers: HeaderMap| async move {
                assert_eq!(id, canonical_id);
                assert_eq!(headers[header::COOKIE], "web-session=claimant");
                StatusCode::CREATED
            }),
        )
        // Owner creation remains a canonical API authorization decision, not an
        // anonymous app-host capability.
        .route(
            "/v1/enrollment/computers",
            post(|| async { StatusCode::UNAUTHORIZED }),
        )
        .fallback(|| async { StatusCode::NOT_FOUND });
    let app = wrap(inner, config);
    let id = canonical_id.to_string();

    for (path, status) in [
        (
            format!("/v1/enrollment/computers/{id}/status"),
            StatusCode::ACCEPTED,
        ),
        (
            format!("/v1/enrollment/computers/{id}/confirm"),
            StatusCode::CREATED,
        ),
    ] {
        let mut claim = request(&path, Some("app.example.test"), "POST");
        claim.headers_mut().insert(
            header::COOKIE,
            HeaderValue::from_static("web-session=claimant"),
        );
        let response = app.clone().oneshot(claim).await.unwrap();
        assert_eq!(response.status(), status, "POST {path}");
    }

    let owner_create = app
        .clone()
        .oneshot(request(
            "/v1/enrollment/computers",
            Some("app.example.test"),
            "POST",
        ))
        .await
        .unwrap();
    assert_eq!(owner_create.status(), StatusCode::UNAUTHORIZED);

    for (path, method, status) in [
        (
            format!("/v1/enrollment/computers/{id}/status"),
            "GET",
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            format!("/v1/enrollment/computers/{id}/approve"),
            "POST",
            StatusCode::NOT_FOUND,
        ),
        (
            format!("/v1/enrollment/computers/{id}/status/extra"),
            "POST",
            StatusCode::NOT_FOUND,
        ),
        (
            "/v1/enrollment/computers/not-a-uuid/status".into(),
            "POST",
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("/v1/enrollment/computers/{id}%2Fextra/status"),
            "POST",
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("/v1/enrollment/computers/{id}/%2e%2e/status"),
            "POST",
            StatusCode::NOT_FOUND,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(request(&path, Some("app.example.test"), method))
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{method} {path}");
    }
}

#[tokio::test]
async fn account_host_and_root_mode_do_not_forward_app_enrollment_paths() {
    let (_assets, app) = app();
    let account = app
        .clone()
        .oneshot(request(
            "/hosted/v1/web/app-authorize",
            Some("api.example.test"),
            "GET",
        ))
        .await
        .unwrap();
    assert_eq!(account.status(), StatusCode::OK);

    let (_assets, root) = root_app();
    let response = root
        .oneshot(request("/hosted/v1/web/app-session", None, "POST"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn app_host_shell_sends_no_referrer_while_assets_keep_same_origin() {
    let (_assets, app) = app();
    let shell = app
        .clone()
        .oneshot(request(
            "/?enrollment_code=abc&state=def",
            Some("app.example.test"),
            "GET",
        ))
        .await
        .unwrap();
    assert_eq!(shell.status(), StatusCode::OK);
    assert_eq!(shell.headers()[header::REFERRER_POLICY], "no-referrer");

    let asset = app
        .oneshot(request(
            "/assets/app-1234abcd.js",
            Some("app.example.test"),
            "GET",
        ))
        .await
        .unwrap();
    assert_eq!(asset.headers()[header::REFERRER_POLICY], "same-origin");

    let (_assets, root) = root_app();
    let root_shell = root.oneshot(request("/", None, "GET")).await.unwrap();
    assert_eq!(root_shell.headers()[header::REFERRER_POLICY], "same-origin");
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
