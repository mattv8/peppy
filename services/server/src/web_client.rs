use std::{path::PathBuf, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header},
    response::IntoResponse,
    routing::any,
};
use serde_json::json;
use tokio_util::io::ReaderStream;
use tower::ServiceExt;
use url::Url;

const MAX_ASSET_BYTES: u64 = 32 * 1024 * 1024;
const SECURITY_POLICY: &str = "default-src 'self'; base-uri 'none'; frame-ancestors 'none'; object-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; worker-src 'self'; connect-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; font-src 'self'; manifest-src 'self'";
const RESERVED_PATHS: [&str; 3] = ["/hosted", "/account", "/signin"];

#[derive(Clone, Debug)]
pub struct WebClientConfig {
    pub host: String,
    pub asset_dir: PathBuf,
    pub api_origin: Url,
    pub account_url: Option<Url>,
    all_hosts: bool,
}

impl WebClientConfig {
    pub fn new(host: String, asset_dir: PathBuf, api_url: Url) -> Result<Self, String> {
        let host = normalize_configured_host(&host)?;
        Self::configured(host, asset_dir, api_url, false)
    }

    /// Creates a web-client configuration that serves every request host.
    /// `host` remains the canonical API host for account-navigation checks.
    pub fn for_all_hosts(asset_dir: PathBuf, api_url: Url) -> Result<Self, String> {
        let api_origin = canonical_origin(api_url)?;
        let host = canonical_api_host(&api_origin);
        Self::configured(host, asset_dir, api_origin, true)
    }

    pub fn serves_all_hosts(&self) -> bool {
        self.all_hosts
    }

    fn configured(
        host: String,
        asset_dir: PathBuf,
        api_url: Url,
        all_hosts: bool,
    ) -> Result<Self, String> {
        let asset_dir = std::fs::canonicalize(asset_dir)
            .map_err(|error| format!("PEPPY_WEB_CLIENT_DIR cannot be resolved: {error}"))?;
        if !asset_dir.is_dir() {
            return Err("PEPPY_WEB_CLIENT_DIR must be a directory".into());
        }
        let api_origin = canonical_origin(api_url)?;
        Ok(Self {
            host,
            asset_dir,
            api_origin,
            account_url: None,
            all_hosts,
        })
    }

    pub fn with_account_url(mut self, account_url: Url) -> Result<Self, String> {
        let Some(account_host) = account_url.host_str() else {
            return Err("PEPPY_WEB_CLIENT_ACCOUNT_URL must use an HTTPS hostname".into());
        };
        if account_url.scheme() != "https"
            || !matches!(account_url.host(), Some(url::Host::Domain(_)))
            || !account_url.username().is_empty()
            || account_url.password().is_some()
            || account_url.query().is_some()
            || account_url.fragment().is_some()
            || account_url.as_str().chars().any(char::is_control)
            || contains_percent_encoded_control(account_url.as_str())
        {
            return Err("PEPPY_WEB_CLIENT_ACCOUNT_URL must be an HTTPS URL without credentials, query, fragment, or control characters".into());
        }
        let account_host = normalize_configured_host(account_host).map_err(|_| {
            "PEPPY_WEB_CLIENT_ACCOUNT_URL must use an HTTPS DNS hostname".to_owned()
        })?;
        let (configured_host, configured_host_name) = if self.serves_all_hosts() {
            (canonical_api_host(&self.api_origin), "PUBLIC_API_URL")
        } else {
            (self.host.clone(), "PEPPY_WEB_CLIENT_HOST")
        };
        if account_host == configured_host {
            return Err(format!(
                "PEPPY_WEB_CLIENT_ACCOUNT_URL host must differ from {configured_host_name}"
            ));
        }
        self.account_url = Some(account_url);
        Ok(self)
    }
}

fn canonical_api_host(api_origin: &Url) -> String {
    api_origin
        .host_str()
        .expect("canonical web client API origin has a host")
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

fn contains_percent_encoded_control(value: &str) -> bool {
    value.as_bytes().windows(3).any(|encoded| {
        encoded[0] == b'%'
            && std::str::from_utf8(&encoded[1..])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .is_some_and(|byte| byte.is_ascii_control())
    })
}

#[derive(Clone)]
struct WebClientRouter {
    inner: Router,
    config: WebClientConfig,
}

/// Dispatches requests for the dedicated browser host, or every host in
/// explicit root mode, without changing the API path dispatcher.
pub fn wrap(router: Router, config: WebClientConfig) -> Router {
    Router::new()
        .fallback(any(dispatch))
        .with_state(Arc::new(WebClientRouter {
            inner: router,
            config,
        }))
}

async fn dispatch(
    State(state): State<Arc<WebClientRouter>>,
    request: Request<Body>,
) -> Response<Body> {
    if !state.config.serves_all_hosts() {
        match request_host_target(request.headers(), &state.config.host) {
            HostTarget::App => {}
            HostTarget::AppLikeMalformed => return StatusCode::NOT_FOUND.into_response(),
            HostTarget::Other => {
                return state
                    .inner
                    .clone()
                    .oneshot(request)
                    .await
                    .unwrap_or_else(|never| match never {});
            }
        }
    }

    let path = request.uri().path();
    if is_api_path(path) {
        return state
            .inner
            .clone()
            .oneshot(request)
            .await
            .unwrap_or_else(|never| match never {});
    }
    if RESERVED_PATHS
        .iter()
        .any(|reserved| path == *reserved || path.starts_with(&format!("{reserved}/")))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    if path == "/web/config.json" {
        return configuration_response(&state.config, request.method() == Method::HEAD);
    }
    if path == "/" || (is_safe_navigation_path(path) && !is_static_namespace(path)) {
        return serve_asset(
            &state.config,
            "index.html",
            request.method() == Method::HEAD,
        )
        .await;
    }
    serve_asset(
        &state.config,
        path.strip_prefix('/').unwrap_or(path),
        request.method() == Method::HEAD,
    )
    .await
}

fn is_api_path(path: &str) -> bool {
    // Keep this coupled to the top-level API routes assembled in `lib.rs`.
    matches!(path, "/healthz" | "/readyz" | "/__release")
        || path == "/v1"
        || path.starts_with("/v1/")
        || path == "/file"
        || path.starts_with("/file/")
}

enum HostTarget {
    App,
    AppLikeMalformed,
    Other,
}

fn request_host_target(headers: &HeaderMap, configured_host: &str) -> HostTarget {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return HostTarget::Other;
    };
    if has_invalid_app_port(host, configured_host) {
        return HostTarget::AppLikeMalformed;
    }
    let Ok(authority) = host.parse::<axum::http::uri::Authority>() else {
        return if is_app_like_host(host, configured_host) {
            HostTarget::AppLikeMalformed
        } else {
            HostTarget::Other
        };
    };
    if authority
        .port()
        .is_some_and(|port| port.as_str().parse::<u16>().is_err())
    {
        return HostTarget::AppLikeMalformed;
    }
    match normalize_request_host(authority.host()) {
        Some(host) if host == configured_host => HostTarget::App,
        _ => HostTarget::Other,
    }
}

fn normalize_configured_host(host: &str) -> Result<String, String> {
    let host = host.trim().trim_end_matches('.');
    if host.is_empty()
        || host.contains(':')
        || host.contains('/')
        || host.contains('@')
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
    {
        return Err("PEPPY_WEB_CLIENT_HOST must be a DNS hostname without a port".into());
    }
    Ok(host.to_ascii_lowercase())
}

fn normalize_request_host(host: &str) -> Option<String> {
    if host.starts_with('.') || host.ends_with("..") {
        return None;
    }
    normalize_configured_host(host).ok()
}

fn is_app_like_host(host: &str, configured_host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    host == configured_host || host.starts_with(&format!("{configured_host}:"))
}

fn has_invalid_app_port(host: &str, configured_host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    host.strip_prefix(&format!("{configured_host}:"))
        .is_some_and(|port| port.parse::<u16>().is_err())
}

fn canonical_origin(mut api_url: Url) -> Result<Url, String> {
    if !matches!(api_url.scheme(), "http" | "https")
        || api_url.host_str().is_none()
        || !api_url.username().is_empty()
        || api_url.password().is_some()
    {
        return Err("PUBLIC_API_URL must be an HTTP(S) origin for the web client".into());
    }
    api_url.set_path("");
    api_url.set_query(None);
    api_url.set_fragment(None);
    Url::parse(&api_url.origin().ascii_serialization())
        .map_err(|_| "PUBLIC_API_URL must have a serializable origin".into())
}

fn is_safe_navigation_path(path: &str) -> bool {
    let trimmed = path.trim_matches('/');
    !trimmed.is_empty()
        && !trimmed.contains('.')
        && !trimmed.contains('\\')
        && !trimmed.contains("%2f")
        && !trimmed.contains("%2F")
        && !trimmed.contains("%5c")
        && !trimmed.contains("%5C")
        && !contains_encoded_traversal(trimmed)
}

fn is_static_namespace(path: &str) -> bool {
    matches!(path, "/assets" | "/core" | "/web")
        || path.starts_with("/assets/")
        || path.starts_with("/core/")
        || path.starts_with("/web/")
}

async fn serve_asset(config: &WebClientConfig, path: &str, head_only: bool) -> Response<Body> {
    let Some(path) = safe_asset_path(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let candidate = config.asset_dir.join(path);
    let Ok(metadata) = tokio::fs::metadata(&candidate).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !metadata.is_file() || metadata.len() > MAX_ASSET_BYTES {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(canonical) = tokio::fs::canonicalize(&candidate).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !canonical.starts_with(&config.asset_dir) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let body = if head_only {
        Body::empty()
    } else {
        let Ok(file) = tokio::fs::File::open(canonical).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        Body::from_stream(ReaderStream::new(file))
    };
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control(path)),
    );
    add_security_headers(response.headers_mut());
    response
}

fn safe_asset_path(path: &str) -> Option<&str> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || contains_encoded_separator(path)
    {
        return None;
    }
    Some(path)
}

fn contains_encoded_separator(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.contains("%2f") || path.contains("%5c")
}

fn contains_encoded_traversal(path: &str) -> bool {
    path.to_ascii_lowercase().contains("%2e")
}

fn configuration_response(config: &WebClientConfig, head_only: bool) -> Response<Body> {
    let body = if head_only {
        Body::empty()
    } else {
        let mut body = json!({
                "version": 1,
                "apiOrigin": config.api_origin.as_str(),
        });
        if let Some(account_url) = &config.account_url {
            body["accountUrl"] = json!(account_url.as_str());
        }
        Body::from(serde_json::to_vec(&body).expect("web client configuration is serializable"))
    };
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    add_security_headers(response.headers_mut());
    response
}

fn add_security_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(SECURITY_POLICY),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
}

fn cache_control(path: &str) -> &'static str {
    if matches!(path, "index.html" | "worker.js" | "service-worker.js") || path.starts_with("core/")
    {
        "no-store"
    } else if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600"
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "webmanifest" => "application/json; charset=utf-8",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "ogg" => "audio/ogg",
        _ => "application/octet-stream",
    }
}
