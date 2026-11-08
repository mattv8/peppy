use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{Method, Request, StatusCode},
    routing::post,
};
use http_body_util::BodyExt;
use peppy_hosted_api::mcp::McpWorkerCallRequest;
use peppy_server::{
    ServerBuilder,
    api::{AccessPolicy, AccessPrincipal, CommunityAccessPolicy, Operation, PolicyFuture},
    config::Config,
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tempfile::NamedTempFile;
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

const MAX_BODY: usize = 4 * 1024 * 1024;

#[derive(Clone, Default)]
struct WorkerState {
    calls: Arc<Mutex<Vec<McpWorkerCallRequest>>>,
    oversized_result: Arc<std::sync::atomic::AtomicBool>,
    block: Arc<std::sync::atomic::AtomicBool>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

async fn worker_call(
    State(state): State<WorkerState>,
    Json(request): Json<McpWorkerCallRequest>,
) -> Json<serde_json::Value> {
    state.calls.lock().unwrap().push(request.clone());
    if state.block.load(std::sync::atomic::Ordering::Relaxed) {
        state.entered.notify_one();
        state.release.notified().await;
    }
    if state
        .oversized_result
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Json(serde_json::json!({"content":[{"type":"text","text":"x".repeat(MAX_BODY)}]}));
    }
    match request
        .request
        .get("method")
        .and_then(serde_json::Value::as_str)
    {
        Some("tools/list") => Json(serde_json::json!({"tools":[
            {"name":"list_conversations","description":"read","inputSchema":{"type":"object"}},
            {"name":"send_message","description":"write","inputSchema":{"type":"object"}},
            {"name":"send_image","description":"media","inputSchema":{"type":"object"}}
        ]})),
        Some("tools/call") => Json(
            serde_json::json!({"content":[{"type":"text","text":request.vault_id.to_string()}],"isError":false}),
        ),
        _ => Json(serde_json::json!({"content":[],"isError":true})),
    }
}

/// Denies the listed operations for every principal; everything else is allowed.
struct DenyingPolicy {
    denied: Vec<Operation>,
}

impl AccessPolicy for DenyingPolicy {
    fn authorize(
        &self,
        _principal: AccessPrincipal,
        operation: Operation,
    ) -> PolicyFuture<'_, bool> {
        let allowed = !self.denied.contains(&operation);
        Box::pin(async move { allowed })
    }
}

#[derive(Default)]
struct RecordingPolicy {
    operations: Arc<Mutex<Vec<Operation>>>,
}

impl AccessPolicy for RecordingPolicy {
    fn authorize(
        &self,
        _principal: AccessPrincipal,
        operation: Operation,
    ) -> PolicyFuture<'_, bool> {
        self.operations.lock().unwrap().push(operation);
        Box::pin(async { true })
    }
}

/// Allows the first `Read` (HTTP bearer authentication) and denies every later
/// one, modelling a lapse between authentication and the MCP handler's precheck.
#[derive(Default)]
struct ReadLapsesAfterAuthenticationPolicy {
    reads: std::sync::atomic::AtomicUsize,
}

impl AccessPolicy for ReadLapsesAfterAuthenticationPolicy {
    fn authorize(
        &self,
        _principal: AccessPrincipal,
        operation: Operation,
    ) -> PolicyFuture<'_, bool> {
        let allowed = operation != Operation::Read
            || self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
        Box::pin(async move { allowed })
    }
}

struct Fixture {
    admin: PgPool,
    pool: PgPool,
    schema: String,
    router: Router,
    worker: WorkerState,
    worker_task: tokio::task::JoinHandle<()>,
    vault: Uuid,
    grant: Uuid,
    token: String,
    other_vault: Uuid,
    other_device: Uuid,
    other_token: String,
    _control_token: NamedTempFile,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_policy(Arc::new(CommunityAccessPolicy)).await
    }

    async fn with_policy(policy: Arc<dyn AccessPolicy>) -> Self {
        let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("peppy_mcp_protocol_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let mut database: Url = database_url.parse().unwrap();
        database
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database.as_str())
            .await
            .unwrap();
        peppy_server::MIGRATOR.run(&pool).await.unwrap();

        let worker = WorkerState::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let worker_addr = listener.local_addr().unwrap();
        let worker_router = Router::new()
            .route("/internal/mcp/call", post(worker_call))
            .with_state(worker.clone());
        let worker_task = tokio::spawn(async move {
            axum::serve(listener, worker_router).await.unwrap();
        });
        let mut control_token = NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut control_token, b"worker-test-token\n").unwrap();

        let (vault, _device, grant, token) = seed(&pool, "first", &["conversations:read"]).await;
        let (other_vault, other_device, _other_grant, other_token) =
            seed(&pool, "second", &["conversations:read"]).await;
        let config = Config {
            bind_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            database_url,
            release_identity: "test".into(),
            s3: None,
            public_api_url: Some(Url::parse("https://peppy.test").unwrap()),
            public_attachment_url: None,
            vault_attachment_quota_bytes: 512 * 1024 * 1024,
            trusted_proxy_cidrs: Vec::new(),
            replay_retention: Duration::from_secs(60),
            relay_url: None,
            mcp_worker_url: Some(Url::parse(&format!("http://{worker_addr}/")).unwrap()),
            mcp_worker_token_file: Some(control_token.path().into()),
            web_client: None,
            web_live_trusted_origin: None,
        };
        let router = ServerBuilder::new(config, pool.clone())
            .access_policy(policy)
            .build()
            .await
            .unwrap()
            .router();
        Self {
            admin,
            pool,
            schema,
            router,
            worker,
            worker_task,
            vault,
            grant,
            token,
            other_vault,
            other_device,
            other_token,
            _control_token: control_token,
        }
    }

    async fn request(
        &self,
        token: Option<&str>,
        body: serde_json::Value,
    ) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
        let mut builder = Request::post("/mcp")
            .header("host", "peppy.test")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-03-26")
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let response = self
            .router
            .clone()
            .oneshot(
                builder
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, body, headers)
    }

    async fn request_with_method(
        &self,
        method: Method,
        token: Option<&str>,
    ) -> (StatusCode, Vec<u8>, axum::http::HeaderMap) {
        let mut builder = Request::builder()
            .method(method)
            .uri("/mcp")
            .header("host", "peppy.test")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", "2025-03-26")
            .header("content-type", "application/json");
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let response = self
            .router
            .clone()
            .oneshot(
                builder
                    .body(Body::from(serde_json::to_vec(&initialize()).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, body, headers)
    }

    async fn cleanup(self) {
        self.worker_task.abort();
        self.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

async fn request_router(
    router: Router,
    token: &str,
    body: serde_json::Value,
) -> (StatusCode, Vec<u8>) {
    let response = router
        .oneshot(
            Request::post("/mcp")
                .header("host", "peppy.test")
                .header("accept", "application/json, text/event-stream")
                .header("mcp-protocol-version", "2025-03-26")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, body)
}

async fn seed(pool: &PgPool, label: &str, scopes: &[&str]) -> (Uuid, Uuid, Uuid, String) {
    let vault = Uuid::new_v4();
    let device = Uuid::new_v4();
    let grant = Uuid::new_v4();
    let token = format!("mcp-{label}-{}", Uuid::new_v4());
    let client_id = format!("client-{label}-{}", Uuid::new_v4().simple());
    sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,'{}',$2,1,$3)")
        .bind(vault).bind(vec![1_u8]).bind("11".repeat(32)).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name,kind) VALUES($1,$2,'device','{}',$3,1,$4,'desktop')")
        .bind(vault).bind(device).bind("11".repeat(32)).bind(label).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO mcp_devices(vault_id,device_id) VALUES($1,$2)")
        .bind(vault)
        .bind(device)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO mcp_grants(id,vault_id,device_id,client_name,scopes) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(grant)
    .bind(vault)
    .bind(device)
    .bind(label)
    .bind(scopes)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO mcp_oauth_clients(client_id,client_name,redirect_uris,allowed_scopes) VALUES($1,$2,ARRAY['https://client.test/callback'],ARRAY['conversations:read','messages:send','media:send'])")
        .bind(&client_id).bind(label).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO mcp_oauth_tokens(digest,token_kind,family_id,vault_id,device_id,grant_id,client_id,resource,expires_at) VALUES($1,'access',$2,$3,$4,$5,$6,'https://peppy.test/mcp',now()+interval '1 hour')")
        .bind(Sha256::digest(token.as_bytes()).as_slice()).bind(Uuid::new_v4()).bind(vault).bind(device).bind(grant).bind(client_id).execute(pool).await.unwrap();
    (vault, device, grant, token)
}

fn initialize() -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})
}

fn list() -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})
}
fn call(name: &str) -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":{}}})
}

#[tokio::test]
async fn public_mcp_uses_sdk_transport_and_fences_each_request() {
    let fixture = Fixture::new().await;
    let (status, body, headers) = fixture.request(Some(&fixture.token), initialize()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("cache-control").unwrap(), "no-store");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["result"]["serverInfo"]["name"],
        "rmcp"
    );

    let (status, body, _) = fixture.request(Some(&fixture.token), list()).await;
    assert_eq!(status, StatusCode::OK);
    let tools = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "list_conversations");
    let (status, body, _) = fixture
        .request(Some(&fixture.token), call("send_message"))
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the SDK returns the scoped call error"
    );
    assert!(
        serde_json::from_slice::<serde_json::Value>(&body)
            .unwrap()
            .get("error")
            .is_some()
    );
    assert_eq!(
        fixture.worker.calls.lock().unwrap().len(),
        1,
        "hidden tools never reach the worker"
    );

    let (status, body, _) = fixture
        .request(Some(&fixture.other_token), call("list_conversations"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        String::from_utf8(body)
            .unwrap()
            .contains(&fixture.other_vault.to_string())
    );
    let calls = fixture.worker.calls.lock().unwrap().clone();
    assert_eq!(calls[0].vault_id, fixture.vault);
    assert_eq!(calls[1].vault_id, fixture.other_vault);
    assert_eq!(calls[1].device_id, fixture.other_device);

    let (status, _, headers) = fixture.request(None, initialize()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        headers
            .get("www-authenticate")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("resource_metadata")
    );

    let request = Request::post("/mcp")
        .header("host", "peppy.test")
        .header("origin", "https://attacker.test")
        .header("authorization", format!("Bearer {}", fixture.token))
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-03-26")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&initialize()).unwrap()))
        .unwrap();
    assert_eq!(
        fixture
            .router
            .clone()
            .oneshot(request)
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );

    let notification =
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}});
    let (status, body, _) = fixture
        .request(Some(&fixture.other_token), notification)
        .await;
    assert!(status == StatusCode::ACCEPTED || status == StatusCode::NO_CONTENT);
    assert!(body.is_empty());

    sqlx::query("UPDATE mcp_grants SET revoked_at=now() WHERE id=$1")
        .bind(fixture.grant)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        fixture.request(Some(&fixture.token), list()).await.0,
        StatusCode::UNAUTHORIZED
    );

    let oversized = serde_json::Value::String("x".repeat(MAX_BODY + 1));
    assert_eq!(
        fixture
            .request(Some(&fixture.other_token), oversized)
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    fixture
        .worker
        .oversized_result
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let (status, body, _) = fixture
        .request(Some(&fixture.other_token), call("list_conversations"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.len() < MAX_BODY,
        "oversized worker output must never reach the client"
    );
    sqlx::query("UPDATE devices SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2")
        .bind(fixture.other_vault)
        .bind(fixture.other_device)
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        fixture.request(Some(&fixture.other_token), list()).await.0,
        StatusCode::UNAUTHORIZED
    );

    fixture.cleanup().await;
}

#[tokio::test]
async fn mcp_authenticates_unsupported_methods_before_the_sdk_rejects_them() {
    let fixture = Fixture::new().await;
    let unsupported_methods = [
        Method::GET,
        Method::DELETE,
        Method::PUT,
        Method::PATCH,
        Method::OPTIONS,
        Method::HEAD,
        Method::TRACE,
    ];

    for method in &unsupported_methods {
        let (status, _, headers) = fixture
            .request_with_method(method.clone(), Some(&fixture.token))
            .await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
        assert_eq!(headers.get("allow").unwrap(), "POST", "{method}");
    }
    assert!(fixture.worker.calls.lock().unwrap().is_empty());

    for token in [None, Some("malformed")] {
        for method in &unsupported_methods {
            let (status, body, headers) = fixture.request_with_method(method.clone(), token).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {token:?}");
            assert!(
                headers.get("www-authenticate").is_some(),
                "{method} {token:?}"
            );
            if *method != Method::HEAD {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"],
                    "invalid_token",
                    "{method} {token:?}"
                );
            }
        }
    }
    assert!(fixture.worker.calls.lock().unwrap().is_empty());
    fixture.cleanup().await;
}

#[tokio::test]
async fn protected_result_is_fenced_when_credential_changes_midflight() {
    let fixture = Fixture::new().await;
    fixture
        .worker
        .block
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let router = fixture.router.clone();
    let token = fixture.token.clone();
    let request =
        tokio::spawn(
            async move { request_router(router, &token, call("list_conversations")).await },
        );
    fixture.worker.entered.notified().await;
    sqlx::query("UPDATE mcp_grants SET revoked_at=now() WHERE id=$1")
        .bind(fixture.grant)
        .execute(&fixture.pool)
        .await
        .unwrap();
    fixture.worker.release.notify_one();
    let (status, body) = request.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    let result = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
    assert!(
        result.get("error").is_some(),
        "revoked grant must not release tool output"
    );
    assert!(
        !String::from_utf8(body)
            .unwrap()
            .contains(&fixture.vault.to_string())
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn protected_result_is_fenced_when_credential_expires_midflight() {
    let fixture = Fixture::new().await;
    fixture
        .worker
        .block
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let router = fixture.router.clone();
    let token = fixture.token.clone();
    let request =
        tokio::spawn(
            async move { request_router(router, &token, call("list_conversations")).await },
        );
    fixture.worker.entered.notified().await;
    sqlx::query("UPDATE mcp_oauth_tokens SET expires_at=now()-interval '1 second' WHERE digest=$1")
        .bind(Sha256::digest(fixture.token.as_bytes()).as_slice())
        .execute(&fixture.pool)
        .await
        .unwrap();
    fixture.worker.release.notify_one();
    let (status, body) = request.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    let result = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
    assert!(
        result.get("error").is_some(),
        "expired token must not release tool output"
    );
    assert!(
        !String::from_utf8(body)
            .unwrap()
            .contains(&fixture.vault.to_string())
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn read_only_policy_hides_send_tools_and_reports_policy_denial_as_tool_error() {
    let fixture = Fixture::with_policy(Arc::new(DenyingPolicy {
        denied: vec![Operation::Send, Operation::Upload],
    }))
    .await;
    let (_, _, _, writer_token) = seed(
        &fixture.pool,
        "writer",
        &["conversations:read", "messages:send", "media:send"],
    )
    .await;

    let (status, body, _) = fixture.request(Some(&writer_token), list()).await;
    assert_eq!(status, StatusCode::OK);
    let tools = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "list_conversations");

    let calls_before_send = fixture.worker.calls.lock().unwrap().len();
    let (status, body, _) = fixture
        .request(Some(&writer_token), call("send_message"))
        .await;
    assert_eq!(status, StatusCode::OK);
    let response = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
    assert!(
        response.get("error").is_none(),
        "policy denial is a tool result, not a JSON-RPC error"
    );
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["content"][0]["text"],
        "operation_not_permitted: this Peppy account does not currently allow this operation."
    );
    assert_eq!(
        fixture.worker.calls.lock().unwrap().len(),
        calls_before_send,
        "a policy-denied call never reaches the worker"
    );

    fixture.cleanup().await;
}

#[tokio::test]
async fn read_denied_policy_fails_tools_list_as_invalid_request() {
    let fixture =
        Fixture::with_policy(Arc::new(ReadLapsesAfterAuthenticationPolicy::default())).await;

    let (status, body, _) = fixture.request(Some(&fixture.token), list()).await;
    assert_eq!(status, StatusCode::OK);
    let error = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"].clone();
    assert_eq!(
        error["code"], -32600,
        "policy denial is not an internal error"
    );
    assert_eq!(error["message"], "operation_not_permitted");
    assert!(
        fixture.worker.calls.lock().unwrap().is_empty(),
        "a denied listing never reaches the worker"
    );

    fixture.cleanup().await;
}

#[tokio::test]
async fn scope_is_checked_before_policy_for_calls() {
    let policy = Arc::new(RecordingPolicy::default());
    let fixture = Fixture::with_policy(policy.clone()).await;

    // `fixture.token` carries only `conversations:read`, so the send is outside its grant.
    let (status, body, _) = fixture
        .request(Some(&fixture.token), call("send_message"))
        .await;
    assert_eq!(status, StatusCode::OK);
    let response = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
    assert_eq!(response["error"]["code"], -32602);
    assert_eq!(response["error"]["message"], "tool is not available");
    assert!(
        response.get("result").is_none(),
        "scope denial is a protocol error, not a tool result"
    );

    let (status, body, _) = fixture
        .request(Some(&fixture.token), call("unknown_tool"))
        .await;
    assert_eq!(status, StatusCode::OK);
    let response = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
    assert_eq!(response["error"]["code"], -32602);
    assert_eq!(response["error"]["message"], "tool is not available");

    assert!(
        fixture.worker.calls.lock().unwrap().is_empty(),
        "out-of-scope calls never reach the worker"
    );
    assert!(
        policy
            .operations
            .lock()
            .unwrap()
            .iter()
            .all(|operation| *operation == Operation::Read),
        "scope denials never invoke the denied tool's policy operation"
    );

    fixture.cleanup().await;
}

#[tokio::test]
async fn tool_catalogue_caches_each_policy_operation_for_one_request() {
    let policy = Arc::new(RecordingPolicy::default());
    let fixture = Fixture::with_policy(policy.clone()).await;
    let (_, _, _, token) = seed(
        &fixture.pool,
        "catalogue",
        &["conversations:read", "messages:send", "media:send"],
    )
    .await;

    let (status, _, _) = fixture.request(Some(&token), list()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        *policy.operations.lock().unwrap(),
        vec![
            Operation::Read,
            Operation::Read,
            Operation::Send,
            Operation::Upload,
        ],
        "after request authentication, the catalogue checks each policy operation once"
    );

    fixture.cleanup().await;
}

#[tokio::test]
async fn upload_only_denial_blocks_send_image_but_not_send_message() {
    let fixture = Fixture::with_policy(Arc::new(DenyingPolicy {
        denied: vec![Operation::Upload],
    }))
    .await;
    let (_, _, _, writer_token) = seed(
        &fixture.pool,
        "uploader",
        &["conversations:read", "messages:send", "media:send"],
    )
    .await;

    let calls_before_image = fixture.worker.calls.lock().unwrap().len();
    let (status, body, _) = fixture
        .request(Some(&writer_token), call("send_image"))
        .await;
    assert_eq!(status, StatusCode::OK);
    let response = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
    assert!(response.get("error").is_none());
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["content"][0]["text"],
        "operation_not_permitted: this Peppy account does not currently allow this operation."
    );
    assert_eq!(
        fixture.worker.calls.lock().unwrap().len(),
        calls_before_image,
        "an upload-denied image never reaches the worker"
    );

    let (status, body, _) = fixture
        .request(Some(&writer_token), call("send_message"))
        .await;
    assert_eq!(status, StatusCode::OK);
    let response = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
    assert_eq!(response["result"]["isError"], false);
    let calls = fixture.worker.calls.lock().unwrap().clone();
    assert_eq!(
        calls.len(),
        calls_before_image + 1,
        "a send-only operation still reaches the worker"
    );
    assert_eq!(
        calls.last().unwrap().request["method"],
        "tools/call",
        "the worker received the tool call"
    );

    fixture.cleanup().await;
}

#[tokio::test]
async fn media_and_text_scopes_advertise_and_execute_only_their_matching_tools() {
    let fixture = Fixture::new().await;
    let (_, _, _, text_token) = seed(&fixture.pool, "text", &["messages:send"]).await;
    let (_, _, _, media_token) = seed(&fixture.pool, "media", &["media:send"]).await;

    for (token, permitted, denied) in [
        (&text_token, "send_message", "send_image"),
        (&media_token, "send_image", "send_message"),
    ] {
        let (status, body, _) = fixture.request(Some(token), list()).await;
        assert_eq!(status, StatusCode::OK);
        let response = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
        let tools = response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(tools, vec![permitted]);

        let (status, body, _) = fixture.request(Some(token), call(permitted)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["result"]["isError"],
            false
        );

        let (status, body, _) = fixture.request(Some(token), call(denied)).await;
        assert_eq!(status, StatusCode::OK);
        let response = serde_json::from_slice::<serde_json::Value>(&body).unwrap();
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(response["error"]["message"], "tool is not available");
    }

    fixture.cleanup().await;
}
