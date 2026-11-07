use std::{
    fs, io,
    io::Write,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_hosted_api::mcp::{
    McpDeviceState, McpWorkerActivateRequest, McpWorkerCallRequest, McpWorkerCancelRequest,
    McpWorkerCompleteRequest, McpWorkerPrepareRequest, McpWorkerRevokeRequest,
    McpWorkerStatusRequest, McpWorkerStatusResponse, McpWorkerWebhookOp, McpWorkerWebhookRequest,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::{
    config::WorkerConfig, custody::read_private, enrollment::EnrollmentManager,
    registry::RuntimeRegistry,
};

/// How often abandoned claimants are swept even without enrollment traffic.
const CLAIM_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
const CALL_WORK_LOCK_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Clone)]
pub struct WorkerHttp {
    token: Arc<str>,
    enrollments: Arc<Mutex<EnrollmentManager>>,
    runtimes: Arc<RuntimeRegistry>,
}

impl WorkerHttp {
    pub fn new(config: WorkerConfig) -> Result<Self, std::io::Error> {
        let token = load_or_create_control_token(&config.worker_token_file)?;
        let runtimes = RuntimeRegistry::open(config.clone()).map_err(std::io::Error::other)?;
        Ok(Self {
            token: Arc::from(token.trim()),
            enrollments: Arc::new(Mutex::new(EnrollmentManager::new(config))),
            runtimes: Arc::new(runtimes),
        })
    }

    pub async fn start_sync(&self) {
        self.runtimes.start_sync().await;
        let enrollments = self.enrollments.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(CLAIM_SWEEP_INTERVAL);
            loop {
                interval.tick().await;
                enrollments.lock().await.sweep_expired(Instant::now());
            }
        });
    }

    pub async fn shutdown(&self) {
        self.runtimes.shutdown().await;
    }

    pub fn router(self) -> Router {
        let control = Router::new()
            .route("/healthz", get(healthz))
            .route("/internal/mcp/prepare", post(prepare))
            .route("/internal/mcp/activate", post(activate))
            .route("/internal/mcp/complete", post(complete))
            .route("/internal/mcp/cancel", post(cancel))
            .route("/internal/mcp/status", post(status))
            .route("/internal/mcp/revoke", post(revoke))
            .route("/internal/mcp/webhooks", post(webhooks))
            .layer(DefaultBodyLimit::max(64 * 1024));
        let calls = Router::new()
            .route("/internal/mcp/call", post(call))
            .layer(DefaultBodyLimit::max(4 * 1024 * 1024));
        control.merge(calls).with_state(self)
    }
}

fn load_or_create_control_token(path: &Path) -> io::Result<String> {
    match read_control_token(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => create_control_token(path),
        result => result,
    }
}

fn read_control_token(path: &Path) -> io::Result<String> {
    let token = String::from_utf8(read_private(path)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid worker token"))?;
    if token.trim().len() < 32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid worker token",
        ));
    }
    Ok(token)
}

fn create_control_token(path: &Path) -> io::Result<String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "worker token requires a private directory",
            )
        })?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    libsodium_rs::ensure_init().map_err(io::Error::other)?;
    let mut bytes = [0_u8; 32];
    libsodium_rs::random::fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return read_control_token(path);
        }
        Err(error) => return Err(error),
    };
    file.write_all(token.as_bytes())?;
    file.sync_all()?;
    #[cfg(unix)]
    crate::custody::validate_private_file(&file)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(token)
}

async fn healthz() -> &'static str {
    "ok"
}

fn authorize(headers: &HeaderMap, state: &WorkerHttp) -> Result<(), StatusCode> {
    let value = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let supplied = value
        .strip_prefix("Bearer ")
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if subtle_eq(supplied.as_bytes(), state.token.as_bytes()) {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn subtle_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

async fn prepare(
    State(state): State<WorkerHttp>,
    headers: HeaderMap,
    Json(request): Json<McpWorkerPrepareRequest>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&headers, &state)?;
    state
        .enrollments
        .lock()
        .await
        .prepare(request)
        .map(|value| Json(serde_json::to_value(value).expect("worker DTO serializes")))
        .map_err(|_| StatusCode::BAD_REQUEST)
}

async fn activate(
    State(state): State<WorkerHttp>,
    headers: HeaderMap,
    Json(request): Json<McpWorkerActivateRequest>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&headers, &state)?;
    state
        .enrollments
        .lock()
        .await
        .activate(request)
        .map(|value| Json(serde_json::to_value(value).expect("worker DTO serializes")))
        .map_err(|_| StatusCode::BAD_REQUEST)
}

async fn complete(
    State(state): State<WorkerHttp>,
    headers: HeaderMap,
    Json(request): Json<McpWorkerCompleteRequest>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&headers, &state)?;
    let vault_id = request.vault_id;
    state
        .runtimes
        .complete(&mut *state.enrollments.lock().await, request)
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(Json(json!({"vault_id": vault_id, "state":"offline"})))
}

async fn cancel(
    State(state): State<WorkerHttp>,
    headers: HeaderMap,
    Json(request): Json<McpWorkerCancelRequest>,
) -> Result<StatusCode, StatusCode> {
    authorize(&headers, &state)?;
    state.enrollments.lock().await.cancel(request);
    Ok(StatusCode::NO_CONTENT)
}

async fn status(
    State(state): State<WorkerHttp>,
    headers: HeaderMap,
    Json(request): Json<McpWorkerStatusRequest>,
) -> Result<Json<McpWorkerStatusResponse>, StatusCode> {
    authorize(&headers, &state)?;
    let runtime = state.runtimes.get(request.vault_id).await;
    Ok(Json(
        worker_status_response(&request, runtime.as_deref()).await,
    ))
}

async fn worker_status_response(
    request: &McpWorkerStatusRequest,
    runtime: Option<&crate::runtime::Runtime>,
) -> McpWorkerStatusResponse {
    let device_id = runtime
        .map(|runtime| runtime.device_id().0)
        .unwrap_or(request.device_id);
    let (state, last_synced_at) = match runtime {
        Some(runtime) => (
            match runtime.state().await {
                crate::runtime::RuntimeState::Offline => McpDeviceState::Offline,
                crate::runtime::RuntimeState::Online => McpDeviceState::Online,
                crate::runtime::RuntimeState::NeedsApproval => McpDeviceState::NeedsApproval,
                crate::runtime::RuntimeState::Revoked => McpDeviceState::Revoked,
            },
            runtime.last_synced_at().await.and_then(sync_timestamp),
        ),
        None => (McpDeviceState::NeedsApproval, None),
    };
    McpWorkerStatusResponse {
        vault_id: request.vault_id,
        device_id,
        state,
        last_synced_at,
    }
}

#[cfg(test)]
pub(crate) async fn status_response(
    vault_id: uuid::Uuid,
    runtime: Option<&crate::runtime::Runtime>,
) -> Value {
    let device_id = runtime
        .map(|runtime| runtime.device_id().0)
        .unwrap_or(uuid::Uuid::nil());
    serde_json::to_value(
        worker_status_response(
            &McpWorkerStatusRequest {
                vault_id,
                device_id,
            },
            runtime,
        )
        .await,
    )
    .expect("worker status DTO serializes")
}

fn sync_timestamp(time: std::time::SystemTime) -> Option<String> {
    let elapsed = time.duration_since(std::time::UNIX_EPOCH).ok()?;
    let seconds: libc::time_t = elapsed.as_secs().try_into().ok()?;
    let mut utc = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: both pointers refer to valid, correctly aligned storage, and
    // gmtime_r initializes utc before returning a non-null pointer.
    let utc = unsafe {
        if libc::gmtime_r(&seconds, utc.as_mut_ptr()).is_null() {
            return None;
        }
        utc.assume_init()
    };
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        utc.tm_year + 1900,
        utc.tm_mon + 1,
        utc.tm_mday,
        utc.tm_hour,
        utc.tm_min,
        utc.tm_sec,
        elapsed.subsec_millis(),
    ))
}

async fn revoke(
    State(state): State<WorkerHttp>,
    headers: HeaderMap,
    Json(request): Json<McpWorkerRevokeRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    authorize(&headers, &state)?;
    state
        .runtimes
        .revoke(request.vault_id, request.device_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn webhooks(
    State(state): State<WorkerHttp>,
    headers: HeaderMap,
    Json(request): Json<McpWorkerWebhookRequest>,
) -> Result<axum::response::Response, (StatusCode, Json<Value>)> {
    authorize(&headers, &state).map_err(error_status)?;
    let runtime = state
        .runtimes
        .get(request.vault_id)
        .await
        .ok_or_else(|| error(StatusCode::CONFLICT, "mcp_device_required"))?;
    if runtime.device_id().0 != request.device_id {
        return Err(error(StatusCode::CONFLICT, "mcp_device_required"));
    }
    let engine = state.runtimes.webhooks();
    engine
        .update_source_origin(request.vault_id, request.device_id, &request.source_origin)
        .await
        .map_err(webhook_error)?;
    let response: Result<Value, crate::webhooks::engine::WebhookError> = match request.op {
        McpWorkerWebhookOp::List => engine
            .list(request.vault_id, request.device_id)
            .await
            .and_then(to_value),
        McpWorkerWebhookOp::Create => match request.input {
            Some(input) => engine.create(&runtime, input).await.and_then(to_value),
            None => Err(crate::webhooks::engine::WebhookError::Code(
                "webhook_filter_invalid",
            )),
        },
        McpWorkerWebhookOp::Update => match (request.id, request.input) {
            (Some(id), Some(input)) => engine.update(&runtime, id, input).await.and_then(to_value),
            _ => Err(crate::webhooks::engine::WebhookError::Code(
                "webhook_filter_invalid",
            )),
        },
        McpWorkerWebhookOp::Delete => match request.id {
            Some(id) => {
                return engine
                    .delete(&runtime, id)
                    .await
                    .map(|_| StatusCode::NO_CONTENT.into_response())
                    .map_err(webhook_error);
            }
            None => return Err(error(StatusCode::BAD_REQUEST, "webhook_not_found")),
        },
        McpWorkerWebhookOp::RevealSecret => match request.id {
            Some(id) => engine.reveal(&runtime, id).await.and_then(to_value),
            None => Err(crate::webhooks::engine::WebhookError::Missing),
        },
        McpWorkerWebhookOp::RotateSecret => match request.id {
            Some(id) => engine.rotate(&runtime, id).await.and_then(to_value),
            None => Err(crate::webhooks::engine::WebhookError::Missing),
        },
        McpWorkerWebhookOp::Test => match request.id {
            Some(id) => engine.test(&runtime, id).await.and_then(to_value),
            None => Err(crate::webhooks::engine::WebhookError::Missing),
        },
    };
    response
        .map(Json)
        .map(IntoResponse::into_response)
        .map_err(webhook_error)
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, crate::webhooks::engine::WebhookError> {
    serde_json::to_value(value)
        .map_err(|_| crate::webhooks::engine::WebhookError::Code("mcp_unavailable"))
}

fn error(status: StatusCode, code: &'static str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"code": code})))
}
fn error_status(status: StatusCode) -> (StatusCode, Json<Value>) {
    error(status, "unauthorized")
}
fn webhook_error(
    webhook_error: crate::webhooks::engine::WebhookError,
) -> (StatusCode, Json<Value>) {
    match webhook_error {
        crate::webhooks::engine::WebhookError::Missing => {
            error(StatusCode::NOT_FOUND, "webhook_not_found")
        }
        crate::webhooks::engine::WebhookError::Code("webhook_limit") => {
            error(StatusCode::CONFLICT, "webhook_limit")
        }
        crate::webhooks::engine::WebhookError::Code("mcp_unavailable") => {
            error(StatusCode::SERVICE_UNAVAILABLE, "mcp_unavailable")
        }
        crate::webhooks::engine::WebhookError::Code(code) => error(StatusCode::BAD_REQUEST, code),
    }
}

async fn call(
    State(state): State<WorkerHttp>,
    headers: HeaderMap,
    Json(request): Json<McpWorkerCallRequest>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&headers, &state)?;
    let runtime = state
        .runtimes
        .get(request.vault_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    if runtime.device_id().0 != request.device_id {
        return Err(StatusCode::FORBIDDEN);
    }
    runtime
        .ensure_live()
        .await
        .map_err(|_| StatusCode::FORBIDDEN)?;
    let result = match request.request.get("method").and_then(Value::as_str) {
        Some("tools/list") => crate::tools::list_tools(&request.scopes),
        Some("tools/call") => {
            let _work = tokio::time::timeout(CALL_WORK_LOCK_TIMEOUT, runtime.work_guard())
                .await
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
            runtime
                .ensure_live()
                .await
                .map_err(|_| StatusCode::FORBIDDEN)?;
            crate::tools::call_tool(&runtime, &request)
                .await
                .map_err(|_| StatusCode::BAD_REQUEST)?
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    runtime
        .ensure_live()
        .await
        .map_err(|_| StatusCode::FORBIDDEN)?;
    if serde_json::to_vec(&result)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .len()
        > 4 * 1024 * 1024
    {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::{WorkerHttp, create_control_token, load_or_create_control_token, subtle_eq};
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::{fs, io, net::SocketAddr};

    use crate::config::WorkerConfig;

    #[test]
    fn generated_control_tokens_have_independent_32_byte_random_values() {
        let root = tempfile::tempdir().unwrap();
        let first = load_or_create_control_token(&root.path().join("first/token")).unwrap();
        let second = load_or_create_control_token(&root.path().join("second/token")).unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(&first).unwrap().len(), 32);
        assert_eq!(URL_SAFE_NO_PAD.decode(&second).unwrap().len(), 32);
        assert!(first != second, "independent token files reused a secret");
    }

    #[test]
    fn existing_control_token_is_preserved_byte_for_byte() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("token");
        let original = format!("  {}\n", "existing-control-token".repeat(3));
        fs::write(&path, &original).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(load_or_create_control_token(&path).unwrap() == original);
        assert!(create_control_token(&path).unwrap() == original);
        assert!(fs::read(&path).unwrap() == original.as_bytes());
    }

    #[test]
    fn invalid_existing_control_token_is_never_replaced_even_by_racing_creator() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("token");
        for original in [
            b"".as_slice(),
            b"short",
            b"                                 ",
            b"\xff\xfe",
        ] {
            fs::write(&path, original).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            assert_eq!(
                load_or_create_control_token(&path).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(
                create_control_token(&path).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert!(fs::read(&path).unwrap() == original);
        }
    }

    #[cfg(unix)]
    #[test]
    fn control_token_must_be_a_private_regular_file_without_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("token");
        fs::write(&path, "existing-control-token-value-that-is-long-enough").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_or_create_control_token(&path).is_err());

        let target = root.path().join("target");
        fs::write(&target, "existing-control-token-value-that-is-long-enough").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = root.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(load_or_create_control_token(&link).is_err());
    }

    #[test]
    fn control_token_comparison_rejects_wrong_length_and_bytes() {
        assert!(subtle_eq(b"secret", b"secret"));
        assert!(!subtle_eq(b"secret", b"secreu"));
        assert!(!subtle_eq(b"secret", b"short"));
    }

    #[tokio::test]
    async fn revoke_missing_binding_directory_returns_no_content() {
        let root = tempfile::tempdir().unwrap();
        let config = WorkerConfig {
            api_url: "http://127.0.0.1:1".into(),
            public_api_url: "http://127.0.0.1:1".into(),
            state_dir: root.path().join("state"),
            key_file: root.path().join("storage-key"),
            worker_token_file: root.path().join("control-token"),
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        };
        let worker = WorkerHttp::new(config.clone()).unwrap();
        let token = fs::read_to_string(&config.worker_token_file).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, worker.router()).await.unwrap();
        });

        let response = reqwest::Client::new()
            .post(format!("http://{address}/internal/mcp/revoke"))
            .bearer_auth(token.trim())
            .json(&serde_json::json!({
                "vault_id": uuid::Uuid::new_v4(),
                "device_id": uuid::Uuid::new_v4(),
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        server.abort();
    }

    #[tokio::test]
    async fn cancel_requires_the_control_token_and_rejects_malformed_bodies() {
        let root = tempfile::tempdir().unwrap();
        let config = WorkerConfig {
            api_url: "http://127.0.0.1:1".into(),
            public_api_url: "http://127.0.0.1:1".into(),
            state_dir: root.path().join("state"),
            key_file: root.path().join("storage-key"),
            worker_token_file: root.path().join("control-token"),
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        };
        let worker = WorkerHttp::new(config.clone()).unwrap();
        let token = fs::read_to_string(&config.worker_token_file).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/internal/mcp/cancel",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, worker.router()).await.unwrap();
        });
        let body = serde_json::json!({
            "vault_id": uuid::Uuid::new_v4(),
            "owner_device_id": uuid::Uuid::new_v4(),
            "invitation_id": uuid::Uuid::new_v4(),
        });
        let client = reqwest::Client::new();

        let missing = client.post(&url).json(&body).send().await.unwrap();
        assert_eq!(missing.status(), reqwest::StatusCode::UNAUTHORIZED);
        let wrong = client
            .post(&url)
            .bearer_auth("not-the-control-token-value-0000000000")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), reqwest::StatusCode::UNAUTHORIZED);
        let malformed = client
            .post(&url)
            .bearer_auth(token.trim())
            .header("content-type", "application/json")
            .body("{")
            .send()
            .await
            .unwrap();
        assert_eq!(malformed.status(), reqwest::StatusCode::BAD_REQUEST);
        let accepted = client
            .post(&url)
            .bearer_auth(token.trim())
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(accepted.status(), reqwest::StatusCode::NO_CONTENT);
        server.abort();
    }

    #[tokio::test]
    async fn status_response_reports_actual_runtime_identity_and_honest_missing_binding() {
        let root = tempfile::tempdir().unwrap();
        let config = WorkerConfig {
            api_url: "http://127.0.0.1:1".into(),
            public_api_url: "http://127.0.0.1:1".into(),
            state_dir: root.path().join("state"),
            key_file: root.path().join("storage-key"),
            worker_token_file: root.path().join("control-token"),
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        };
        let worker = WorkerHttp::new(config.clone()).unwrap();
        let runtime_state_dir = root.path().join("runtime");
        fs::create_dir(&runtime_state_dir).unwrap();
        let runtime = crate::runtime::Runtime::test_runtime_for_registry(
            runtime_state_dir,
            "http://127.0.0.1:1".into(),
        );
        let runtime_vault_id = runtime.vault_id().0;
        let runtime_device_id = runtime.device_id().0;
        worker.runtimes.insert(runtime).await.unwrap();
        let token = fs::read_to_string(&config.worker_token_file).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/internal/mcp/status",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, worker.router()).await.unwrap();
        });
        let requested_foreign_device_id = uuid::Uuid::new_v4();

        let response = reqwest::Client::new()
            .post(&url)
            .bearer_auth(token.trim())
            .json(&serde_json::json!({
                "vault_id": runtime_vault_id,
                "device_id": requested_foreign_device_id,
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap(),
            serde_json::json!({
                "vault_id": runtime_vault_id,
                "device_id": runtime_device_id,
                "state": "offline",
                "last_synced_at": null,
            })
        );
        let missing_vault_id = uuid::Uuid::new_v4();
        let missing_device_id = uuid::Uuid::new_v4();
        let missing = reqwest::Client::new()
            .post(url)
            .bearer_auth(token.trim())
            .json(&serde_json::json!({
                "vault_id": missing_vault_id,
                "device_id": missing_device_id,
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(missing.status(), reqwest::StatusCode::OK);
        assert_eq!(
            missing.json::<serde_json::Value>().await.unwrap(),
            serde_json::json!({
                "vault_id": missing_vault_id,
                "device_id": missing_device_id,
                "state": "needs_approval",
                "last_synced_at": null,
            })
        );
        server.abort();
    }
}
