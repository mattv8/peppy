use axum::{
    Router,
    body::Body,
    extract::{Request as AxumRequest, State},
    http::{Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use http_body_util::BodyExt;
use peppy_server::api::router;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use tempfile::NamedTempFile;
use tokio::{
    net::TcpListener,
    sync::{Barrier, Mutex, MutexGuard, Notify},
    task::JoinHandle,
};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

type WorkerRequests = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

const ADVISORY_LOCK_SQL: &str =
    "SELECT pg_advisory_xact_lock(hashtextextended('peppy:mcp-enable:' || $1::text, 0))";
// Counts sessions queued on this vault's MCP enable lock. Filtering by the lock key keeps the
// count independent of other tests sharing the database.
const SQL_ADVISORY_WAITERS: &str = "SELECT count(*) FROM pg_locks l, LATERAL (SELECT hashtextextended('peppy:mcp-enable:' || $1::text, 0) AS key) k WHERE l.locktype='advisory' AND NOT l.granted AND l.classid::bigint=((k.key >> 32) & 4294967295) AND l.objid::bigint=(k.key & 4294967295)";
const SQL_EXPIRED: &str =
    "SELECT count(*) FROM mcp_operations WHERE id=$1 AND expires_at <= clock_timestamp()";
const SQL_CLEANUP_PENDING: &str =
    "SELECT count(*) FROM mcp_operations WHERE id=$1 AND cleanup_pending";

fn worker_config_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct Fixture {
    admin: PgPool,
    pool: PgPool,
    schema: String,
    vault: Uuid,
    owner: Uuid,
    device: Uuid,
    owner_token: String,
    device_token: String,
    session_token: String,
    grant: Uuid,
}

impl Fixture {
    async fn new() -> Self {
        let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("peppy_mcp_lifecycle_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let mut url: Url = database_url.parse().unwrap();
        url.query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(url.as_str())
            .await
            .unwrap();
        peppy_server::MIGRATOR.run(&pool).await.unwrap();
        let vault = Uuid::new_v4();
        let owner = Uuid::new_v4();
        let device = Uuid::new_v4();
        let grant = Uuid::new_v4();
        let owner_token = "a".repeat(96);
        let device_token = "b".repeat(96);
        let session_token = "session-token".to_string();
        sqlx::query("INSERT INTO vaults(vault_id,public_key_profile,encrypted_vault_check_header,key_epoch,profile_fingerprint) VALUES($1,'{}',$2,1,$3)").bind(vault).bind(vec![1u8]).bind("11".repeat(32)).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name,kind) VALUES($1,$2,'owner','{}',$4,1,'owner','desktop'),($1,$3,'device','{}',$4,1,'device','desktop')").bind(vault).bind(owner).bind(device).bind("11".repeat(32)).execute(&pool).await.unwrap();
        for (token, id) in [(&owner_token, owner), (&device_token, device)] {
            sqlx::query(
                "INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)",
            )
            .bind(Sha256::digest(token.as_bytes()).as_slice())
            .bind(vault)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO vault_owners(vault_id,username,password_hash,is_admin,operation_id,request_digest) VALUES($1,'owner','hash',TRUE,$2,$3)").bind(vault).bind(Uuid::new_v4()).bind(vec![1u8; 32]).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO instance_sessions(token_digest,vault_id,expires_at) VALUES($1,$2,now()+interval '1 hour')").bind(Sha256::digest(session_token.as_bytes()).as_slice()).bind(vault).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO mcp_devices(vault_id,device_id) VALUES($1,$2)")
            .bind(vault)
            .bind(device)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO mcp_grants(id,vault_id,device_id,client_name,scopes) VALUES($1,$2,$3,'test',ARRAY['conversations:read'])").bind(grant).bind(vault).bind(device).execute(&pool).await.unwrap();
        Self {
            admin,
            pool,
            schema,
            vault,
            owner,
            device,
            owner_token,
            device_token,
            session_token,
            grant,
        }
    }
    async fn cleanup(self) {
        self.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

async fn request(pool: PgPool, token: &str, method: Method, path: String) -> (StatusCode, String) {
    // The router reads the process-wide worker environment while it is built.
    let router = {
        let _worker_config_lock = worker_config_lock().lock().await;
        router(pool)
    };
    let response = router
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into())
}

#[derive(Clone)]
struct Gate {
    path: &'static str,
    arrived: Arc<Notify>,
    release: Arc<Notify>,
}

#[derive(Clone)]
struct WorkerState {
    requests: WorkerRequests,
    gate: Option<Gate>,
    revoke_status: Arc<Mutex<StatusCode>>,
}

struct FakeWorker {
    url: String,
    token: NamedTempFile,
    state: WorkerState,
}

impl FakeWorker {
    fn gate(&self) -> &Gate {
        self.state.gate.as_ref().expect("worker was not gated")
    }

    async fn bodies(&self, path: &str) -> Vec<serde_json::Value> {
        self.state
            .requests
            .lock()
            .await
            .iter()
            .filter(|(recorded, _)| recorded.as_str() == path)
            .map(|(_, body)| body.clone())
            .collect()
    }
}

async fn fake_worker() -> FakeWorker {
    spawn_fake_worker(None).await
}

async fn gated_fake_worker(path: &'static str) -> FakeWorker {
    spawn_fake_worker(Some(Gate {
        path,
        arrived: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    }))
    .await
}

async fn spawn_fake_worker(gate: Option<Gate>) -> FakeWorker {
    let state = WorkerState {
        requests: Arc::new(Mutex::new(Vec::new())),
        gate,
        revoke_status: Arc::new(Mutex::new(StatusCode::NO_CONTENT)),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .fallback(any(handle_worker_request))
        .with_state(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let token = NamedTempFile::new().unwrap();
    std::fs::write(token.path(), "worker-token").unwrap();
    FakeWorker {
        url: format!("http://{address}/"),
        token,
        state,
    }
}

async fn handle_worker_request(State(state): State<WorkerState>, request: AxumRequest) -> Response {
    let path = request.uri().path().to_string();
    let bytes = request.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    state.requests.lock().await.push((path.clone(), body));
    if let Some(gate) = state
        .gate
        .as_ref()
        .filter(|gate| gate.path == path.as_str())
    {
        gate.arrived.notify_one();
        gate.release.notified().await;
    }
    match path.as_str() {
        "/internal/mcp/prepare" => {
            let operation_id = Uuid::new_v4();
            axum::Json(serde_json::json!({
                "operation_id": operation_id,
                "pairing": {
                    "version": 1,
                    "purpose": "connect_computer",
                    "origin": "https://peppy.test",
                    "invitation_id": operation_id,
                    "invitation_secret": "secret",
                    "recipient": {
                        "device": {
                            "device_id": Uuid::new_v4(),
                            "kind": "desktop",
                            "name": "Peppy MCP",
                            "signing_public_key": "key"
                        },
                        "credential_digest": "digest",
                        "requested_role": "device"
                    },
                    "recipient_transfer_public_key": "key",
                    "recipient_commitment": "commitment"
                }
            }))
            .into_response()
        }
        "/internal/mcp/activate" => axum::Json(serde_json::json!({"confirm":{"claim_token":"claim","device_signature":"signature","admission_signature":"admission"}})).into_response(),
        "/internal/mcp/revoke" => (*state.revoke_status.lock().await).into_response(),
        "/internal/mcp/cancel" => StatusCode::NO_CONTENT.into_response(),
        _ => axum::Json(serde_json::json!({})).into_response(),
    }
}

/// The caller must hold `worker_config_lock` for as long as the router is in use.
fn router_for_worker(pool: PgPool, worker: &FakeWorker, _lock: &MutexGuard<'_, ()>) -> Router {
    unsafe {
        std::env::set_var("PEPPY_MCP_WORKER_URL", &worker.url);
        std::env::set_var("PEPPY_MCP_WORKER_TOKEN_FILE", worker.token.path());
    }
    let router = router(pool);
    unsafe {
        std::env::remove_var("PEPPY_MCP_WORKER_URL");
        std::env::remove_var("PEPPY_MCP_WORKER_TOKEN_FILE");
    }
    router
}

async fn mcp_request(
    router: Router,
    fixture: &Fixture,
    path: &str,
    body: serde_json::Value,
) -> StatusCode {
    router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .header("authorization", format!("Bearer {}", fixture.owner_token))
                .header("peppy-instance-session", &fixture.session_token)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

fn spawn_post(
    router: Router,
    token: String,
    session_token: String,
    path: impl Into<String>,
    body: serde_json::Value,
) -> JoinHandle<(StatusCode, String)> {
    let path = path.into();
    tokio::spawn(async move {
        let response = router
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .header("authorization", format!("Bearer {token}"))
                    .header("peppy-instance-session", session_token)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    })
}

fn spawn_owner_post(
    router: Router,
    fixture: &Fixture,
    path: impl Into<String>,
    body: serde_json::Value,
) -> JoinHandle<(StatusCode, String)> {
    spawn_post(
        router,
        fixture.owner_token.clone(),
        fixture.session_token.clone(),
        path,
        body,
    )
}

/// A second active owner, so the requesting owner can be revoked through the real route.
async fn insert_second_owner(fixture: &Fixture) -> String {
    let device = Uuid::new_v4();
    let token = "c".repeat(96);
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name,kind) VALUES($1,$2,'owner','{}',$3,1,'second-owner','desktop')")
        .bind(fixture.vault)
        .bind(device)
        .bind("11".repeat(32))
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)")
        .bind(Sha256::digest(token.as_bytes()).as_slice())
        .bind(fixture.vault)
        .bind(device)
        .execute(&fixture.pool)
        .await
        .unwrap();
    token
}

async fn prepare_request(router: Router, owner_token: String, session_token: String) -> StatusCode {
    router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/mcp/device/prepare")
                .header("authorization", format!("Bearer {owner_token}"))
                .header("peppy-instance-session", session_token)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn wait_for_count(
    pool: &PgPool,
    sql: &str,
    id: Uuid,
    description: &str,
    expected: fn(i64) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let count: i64 = sqlx::query_scalar(sql)
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap();
            if expected(count) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {description}"));
}

/// Inserts an activated operation that expires two seconds from now, with the worker device
/// enrolled, so that `complete` reaches the worker call and only the expiry can reject it.
async fn insert_activated_operation(fixture: &Fixture) -> (Uuid, Uuid) {
    let operation = Uuid::new_v4();
    let worker_device = Uuid::new_v4();
    sqlx::query("INSERT INTO mcp_operations(id,vault_id,owner_device_id,worker_device_id,state,expires_at) VALUES($1,$2,$3,$4,'activated',clock_timestamp()+interval '2 seconds')")
        .bind(operation)
        .bind(fixture.vault)
        .bind(fixture.owner)
        .bind(worker_device)
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name,kind) VALUES($1,$2,'device','{}',$3,1,'worker','desktop')")
        .bind(fixture.vault)
        .bind(worker_device)
        .bind("11".repeat(32))
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO computer_enrollments(id,vault_id,owner_device_id,origin,claim_token_digest,server_nonce,recipient_device_id,recipient_kind,recipient_name,recipient_signing_public_key,recipient_transfer_public_key,recipient_credential_digest,recipient_commitment,profile_fingerprint,key_epoch,state,expires_at) VALUES($1,$2,(SELECT device_id FROM devices WHERE vault_id=$2 AND role='owner'),'https://peppy.test',$3,$3,$4,'desktop','worker',$3,$3,$3,$3,$5,1,'consumed',now()+interval '1 hour')")
        .bind(Uuid::new_v4())
        .bind(fixture.vault)
        .bind(vec![2u8; 32])
        .bind(worker_device)
        .bind("11".repeat(32))
        .execute(&fixture.pool)
        .await
        .unwrap();
    (operation, worker_device)
}

fn completion_request(operation: Uuid, vault: Uuid, device: Uuid) -> serde_json::Value {
    json!({"operation_id":operation,"admission":{"vault_id":vault,"device_id":device,"role":"device","material":{"profile":{},"check_header":"check","fingerprint":"11".repeat(32),"key_epoch":1,"admission_public_key":"key"}}})
}

#[tokio::test]
async fn grants_are_owner_only_and_device_revoke_fences_them_with_cleanup() {
    let fixture = Fixture::new().await;
    let (status, body) = request(
        fixture.pool.clone(),
        &fixture.owner_token,
        Method::GET,
        "/v1/mcp/grants".into(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(&fixture.grant.to_string()));
    let (status, _) = request(
        fixture.pool.clone(),
        &fixture.device_token,
        Method::GET,
        "/v1/mcp/grants".into(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = request(
        fixture.pool.clone(),
        &fixture.owner_token,
        Method::POST,
        format!("/v1/devices/{}/revoke", fixture.device),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let revoked: bool =
        sqlx::query_scalar("SELECT revoked_at IS NOT NULL FROM mcp_grants WHERE id=$1")
            .bind(fixture.grant)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let cleanup: bool = sqlx::query_scalar(
        "SELECT cleanup_pending FROM mcp_devices WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(fixture.vault)
    .bind(fixture.device)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert!(revoked && cleanup);
    fixture.cleanup().await;
}

#[tokio::test]
async fn retrying_abandoned_prepare_supersedes_the_stale_operation() {
    let fixture = Fixture::new().await;
    let worker = fake_worker().await;
    let _worker_config_lock = worker_config_lock().lock().await;
    unsafe {
        std::env::set_var("PEPPY_MCP_WORKER_URL", &worker.url);
        std::env::set_var("PEPPY_MCP_WORKER_TOKEN_FILE", worker.token.path());
    }
    let router = router(fixture.pool.clone());
    unsafe {
        std::env::remove_var("PEPPY_MCP_WORKER_URL");
        std::env::remove_var("PEPPY_MCP_WORKER_TOKEN_FILE");
    }

    assert_eq!(
        mcp_request(
            router.clone(),
            &fixture,
            "/v1/mcp/device/prepare",
            serde_json::json!({}),
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        mcp_request(
            router,
            &fixture,
            "/v1/mcp/device/prepare",
            serde_json::json!({}),
        )
        .await,
        StatusCode::OK
    );

    let prepared_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mcp_operations WHERE vault_id=$1 AND state IN ('prepared','activated')",
    )
    .bind(fixture.vault)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(
        prepared_count, 1,
        "a retry must resume one operation or supersede the abandoned preparation"
    );
    let revoked_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mcp_operations WHERE vault_id=$1 AND state='revoked'",
    )
    .bind(fixture.vault)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(revoked_count, 1);
    let revoked_operation: Uuid =
        sqlx::query_scalar("SELECT id FROM mcp_operations WHERE vault_id=$1 AND state='revoked'")
            .bind(fixture.vault)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let cancellations = worker.bodies("/internal/mcp/cancel").await;
    assert_eq!(cancellations.len(), 1);
    assert_eq!(cancellations[0]["vault_id"], fixture.vault.to_string());
    assert_eq!(
        cancellations[0]["owner_device_id"],
        fixture.owner.to_string()
    );
    assert_eq!(
        cancellations[0]["invitation_id"],
        revoked_operation.to_string()
    );

    fixture.cleanup().await;
}

#[tokio::test]
async fn concurrent_prepares_leave_one_live_operation() {
    let fixture = Fixture::new().await;
    let worker = fake_worker().await;
    let _worker_config_lock = worker_config_lock().lock().await;
    unsafe {
        std::env::set_var("PEPPY_MCP_WORKER_URL", &worker.url);
        std::env::set_var("PEPPY_MCP_WORKER_TOKEN_FILE", worker.token.path());
    }
    let router = router(fixture.pool.clone());
    unsafe {
        std::env::remove_var("PEPPY_MCP_WORKER_URL");
        std::env::remove_var("PEPPY_MCP_WORKER_TOKEN_FILE");
    }
    let barrier = Arc::new(Barrier::new(3));
    let first = {
        let barrier = barrier.clone();
        let router = router.clone();
        let owner_token = fixture.owner_token.clone();
        let session_token = fixture.session_token.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            prepare_request(router, owner_token, session_token).await
        })
    };
    let second = {
        let barrier = barrier.clone();
        let owner_token = fixture.owner_token.clone();
        let session_token = fixture.session_token.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            prepare_request(router, owner_token, session_token).await
        })
    };
    barrier.wait().await;
    let statuses = [first.await.unwrap(), second.await.unwrap()];
    assert!(statuses.contains(&StatusCode::OK));
    assert!(
        statuses
            .iter()
            .all(|status| matches!(*status, StatusCode::OK | StatusCode::CONFLICT))
    );
    let live_operations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mcp_operations WHERE vault_id=$1 AND state IN ('prepared','activated')",
    )
    .bind(fixture.vault)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(live_operations, 1);
    fixture.cleanup().await;
}

#[tokio::test]
async fn prepare_waits_for_an_in_flight_activation_then_conflicts() {
    let fixture = Fixture::new().await;
    let worker = gated_fake_worker("/internal/mcp/activate").await;
    let worker_config_lock = worker_config_lock().lock().await;
    let router = router_for_worker(fixture.pool.clone(), &worker, &worker_config_lock);
    let operation = Uuid::new_v4();
    sqlx::query("INSERT INTO mcp_operations(id,vault_id,owner_device_id,worker_device_id,state) VALUES($1,$2,$3,$4,'prepared')")
        .bind(operation)
        .bind(fixture.vault)
        .bind(fixture.owner)
        .bind(Uuid::new_v4())
        .execute(&fixture.pool)
        .await
        .unwrap();
    let activation = json!({"operation_id":operation,"status":{"state":"approved","expires_in_seconds":60,"transcript":null,"encrypted_key_package":null,"transfer_nonce":null,"owner_proof":null}});

    let activate = spawn_owner_post(
        router.clone(),
        &fixture,
        "/v1/mcp/device/activate",
        activation,
    );
    worker.gate().arrived.notified().await;
    let prepare = spawn_owner_post(router, &fixture, "/v1/mcp/device/prepare", json!({}));
    wait_for_count(
        &fixture.pool,
        SQL_ADVISORY_WAITERS,
        fixture.vault,
        "prepare to queue behind the in-flight activation",
        |waiters| waiters > 0,
    )
    .await;
    assert!(
        !prepare.is_finished(),
        "prepare must wait while activation holds the enable lock"
    );

    worker.gate().release.notify_one();
    assert_eq!(activate.await.unwrap().0, StatusCode::OK);
    let (status, body) = prepare.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("mcp_enable_in_progress"), "{body}");
    let state: String = sqlx::query_scalar("SELECT state FROM mcp_operations WHERE id=$1")
        .bind(operation)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(state, "activated");
    fixture.cleanup().await;
}

#[tokio::test]
async fn complete_that_outlives_its_operation_is_revoked_and_cleared_only_after_worker_ack() {
    let fixture = Fixture::new().await;
    let worker = gated_fake_worker("/internal/mcp/complete").await;
    *worker.state.revoke_status.lock().await = StatusCode::SERVICE_UNAVAILABLE;
    let worker_config_lock = worker_config_lock().lock().await;
    let router = router_for_worker(fixture.pool.clone(), &worker, &worker_config_lock);
    let (operation, worker_device) = insert_activated_operation(&fixture).await;

    let complete = spawn_owner_post(
        router,
        &fixture,
        "/v1/mcp/device/complete",
        completion_request(operation, fixture.vault, worker_device),
    );
    worker.gate().arrived.notified().await;
    wait_for_count(
        &fixture.pool,
        SQL_EXPIRED,
        operation,
        "the operation to expire during the worker call",
        |expired| expired == 1,
    )
    .await;
    worker.gate().release.notify_one();
    let (status, body) = complete.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("mcp_operation_invalid"), "{body}");

    let registered: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mcp_devices WHERE vault_id=$1 AND device_id=$2")
            .bind(fixture.vault)
            .bind(worker_device)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(
        registered, 0,
        "an expired completion must not register a device"
    );
    let revoke =
        json!({"vault_id":fixture.vault.to_string(),"device_id":worker_device.to_string()});
    assert_eq!(
        worker.bodies("/internal/mcp/revoke").await,
        vec![revoke.clone()]
    );
    let (state, pending): (String, bool) =
        sqlx::query_as("SELECT state,cleanup_pending FROM mcp_operations WHERE id=$1")
            .bind(operation)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(
        (state.as_str(), pending),
        ("revoked", true),
        "a failed worker revoke must leave cleanup pending"
    );

    *worker.state.revoke_status.lock().await = StatusCode::NO_CONTENT;
    let _sweeping_router = router_for_worker(fixture.pool.clone(), &worker, &worker_config_lock);
    wait_for_count(
        &fixture.pool,
        SQL_CLEANUP_PENDING,
        operation,
        "the sweep to clear cleanup after a 204 revoke",
        |pending| pending == 0,
    )
    .await;
    assert_eq!(
        worker.bodies("/internal/mcp/revoke").await,
        vec![revoke.clone(), revoke]
    );
    assert_eq!(
        worker.bodies("/internal/mcp/cancel").await,
        vec![
            json!({"vault_id":fixture.vault.to_string(),"owner_device_id":fixture.owner.to_string(),"invitation_id":operation.to_string()})
        ]
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn maintenance_waits_for_an_in_flight_complete_and_revokes_only_once() {
    let fixture = Fixture::new().await;
    let worker = gated_fake_worker("/internal/mcp/complete").await;
    let worker_config_lock = worker_config_lock().lock().await;
    let router = router_for_worker(fixture.pool.clone(), &worker, &worker_config_lock);
    let (operation, worker_device) = insert_activated_operation(&fixture).await;

    let complete = spawn_owner_post(
        router,
        &fixture,
        "/v1/mcp/device/complete",
        completion_request(operation, fixture.vault, worker_device),
    );
    worker.gate().arrived.notified().await;
    wait_for_count(
        &fixture.pool,
        SQL_EXPIRED,
        operation,
        "the operation to expire during the worker call",
        |expired| expired == 1,
    )
    .await;
    // Starting the sweep now: its candidate query sees the expired operation, then it queues on
    // the lock that the in-flight complete still holds.
    let _sweeping_router = router_for_worker(fixture.pool.clone(), &worker, &worker_config_lock);
    wait_for_count(
        &fixture.pool,
        SQL_ADVISORY_WAITERS,
        fixture.vault,
        "the sweep to queue behind the in-flight complete",
        |waiters| waiters > 0,
    )
    .await;

    worker.gate().release.notify_one();
    assert_eq!(complete.await.unwrap().0, StatusCode::CONFLICT);
    wait_for_count(
        &fixture.pool,
        SQL_ADVISORY_WAITERS,
        fixture.vault,
        "the sweep to acquire the lock",
        |waiters| waiters == 0,
    )
    .await;
    // Blocks until the sweep's transaction commits, so the checks below see its final state.
    let mut tx = fixture.pool.begin().await.unwrap();
    sqlx::query(ADVISORY_LOCK_SQL)
        .bind(fixture.vault)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();

    let (state, pending): (String, bool) =
        sqlx::query_as("SELECT state,cleanup_pending FROM mcp_operations WHERE id=$1")
            .bind(operation)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!((state.as_str(), pending), ("revoked", false));
    assert_eq!(
        worker.bodies("/internal/mcp/revoke").await.len(),
        1,
        "the complete's cleanup must be the only device revoke"
    );
    assert!(worker.bodies("/internal/mcp/cancel").await.is_empty());
    fixture.cleanup().await;
}

#[tokio::test]
async fn prepare_rejects_an_unexpired_activated_operation() {
    let fixture = Fixture::new().await;
    let worker = fake_worker().await;
    let _worker_config_lock = worker_config_lock().lock().await;
    unsafe {
        std::env::set_var("PEPPY_MCP_WORKER_URL", &worker.url);
        std::env::set_var("PEPPY_MCP_WORKER_TOKEN_FILE", worker.token.path());
    }
    let router = router(fixture.pool.clone());
    unsafe {
        std::env::remove_var("PEPPY_MCP_WORKER_URL");
        std::env::remove_var("PEPPY_MCP_WORKER_TOKEN_FILE");
    }
    sqlx::query("INSERT INTO mcp_operations(id,vault_id,owner_device_id,worker_device_id,state) VALUES($1,$2,$3,$4,'activated')")
        .bind(Uuid::new_v4())
        .bind(fixture.vault)
        .bind(fixture.owner)
        .bind(Uuid::new_v4())
        .execute(&fixture.pool)
        .await
        .unwrap();

    assert_eq!(
        mcp_request(
            router,
            &fixture,
            "/v1/mcp/device/prepare",
            serde_json::json!({})
        )
        .await,
        StatusCode::CONFLICT
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn activate_rejects_an_expired_operation() {
    let fixture = Fixture::new().await;
    let worker = fake_worker().await;
    let _worker_config_lock = worker_config_lock().lock().await;
    unsafe {
        std::env::set_var("PEPPY_MCP_WORKER_URL", &worker.url);
        std::env::set_var("PEPPY_MCP_WORKER_TOKEN_FILE", worker.token.path());
    }
    let router = router(fixture.pool.clone());
    unsafe {
        std::env::remove_var("PEPPY_MCP_WORKER_URL");
        std::env::remove_var("PEPPY_MCP_WORKER_TOKEN_FILE");
    }
    let operation = Uuid::new_v4();
    sqlx::query("INSERT INTO mcp_operations(id,vault_id,owner_device_id,worker_device_id,state,expires_at) VALUES($1,$2,$3,$4,'prepared',now()-interval '1 second')")
        .bind(operation)
        .bind(fixture.vault)
        .bind(fixture.owner)
        .bind(Uuid::new_v4())
        .execute(&fixture.pool)
        .await
        .unwrap();
    let request = serde_json::json!({"operation_id":operation,"status":{"state":"approved","expires_in_seconds":60,"transcript":null,"encrypted_key_package":null,"transfer_nonce":null,"owner_proof":null}});

    assert_eq!(
        mcp_request(router, &fixture, "/v1/mcp/device/activate", request).await,
        StatusCode::CONFLICT
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn maintenance_revokes_expired_operations_and_cancels_their_claimants() {
    let fixture = Fixture::new().await;
    let operation = Uuid::new_v4();
    sqlx::query("INSERT INTO mcp_operations(id,vault_id,owner_device_id,worker_device_id,state,expires_at) VALUES($1,$2,$3,$4,'prepared',now()-interval '1 second')")
        .bind(operation)
        .bind(fixture.vault)
        .bind(fixture.owner)
        .bind(Uuid::new_v4())
        .execute(&fixture.pool)
        .await
        .unwrap();
    let worker = fake_worker().await;
    let _worker_config_lock = worker_config_lock().lock().await;
    unsafe {
        std::env::set_var("PEPPY_MCP_WORKER_URL", &worker.url);
        std::env::set_var("PEPPY_MCP_WORKER_TOKEN_FILE", worker.token.path());
    }
    let _router = router(fixture.pool.clone());
    unsafe {
        std::env::remove_var("PEPPY_MCP_WORKER_URL");
        std::env::remove_var("PEPPY_MCP_WORKER_TOKEN_FILE");
    }

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let state: String = sqlx::query_scalar("SELECT state FROM mcp_operations WHERE id=$1")
                .bind(operation)
                .fetch_one(&fixture.pool)
                .await
                .unwrap();
            if state == "revoked" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("maintenance must revoke the expired operation");
    // The sweep commits `revoked` before it sends the cancel, so wait for the cancel itself.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while worker.bodies("/internal/mcp/cancel").await.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("maintenance must cancel the expired claimant");
    let cancellations = worker.bodies("/internal/mcp/cancel").await;
    assert_eq!(cancellations.len(), 1);
    assert_eq!(cancellations[0]["invitation_id"], operation.to_string());
    fixture.cleanup().await;
}

#[tokio::test]
async fn activate_replay_is_idempotent_and_rejects_changed_input() {
    let fixture = Fixture::new().await;
    let worker = fake_worker().await;
    let _worker_config_lock = worker_config_lock().lock().await;
    unsafe {
        std::env::set_var("PEPPY_MCP_WORKER_URL", &worker.url);
        std::env::set_var("PEPPY_MCP_WORKER_TOKEN_FILE", worker.token.path());
    }
    let router = router(fixture.pool.clone());
    unsafe {
        std::env::remove_var("PEPPY_MCP_WORKER_URL");
        std::env::remove_var("PEPPY_MCP_WORKER_TOKEN_FILE");
    }
    let operation = Uuid::new_v4();
    let worker_device = Uuid::new_v4();
    sqlx::query("INSERT INTO mcp_operations(id,vault_id,owner_device_id,worker_device_id,state) VALUES($1,$2,(SELECT device_id FROM devices WHERE vault_id=$2 AND role='owner'),$3,'prepared')").bind(operation).bind(fixture.vault).bind(worker_device).execute(&fixture.pool).await.unwrap();
    let request = serde_json::json!({"operation_id":operation,"status":{"state":"approved","expires_in_seconds":60,"transcript":null,"encrypted_key_package":null,"transfer_nonce":null,"owner_proof":null}});
    assert_eq!(
        mcp_request(
            router.clone(),
            &fixture,
            "/v1/mcp/device/activate",
            request.clone()
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        mcp_request(router.clone(), &fixture, "/v1/mcp/device/activate", request).await,
        StatusCode::OK
    );
    let changed = serde_json::json!({"operation_id":operation,"status":{"state":"approved","expires_in_seconds":61,"transcript":null,"encrypted_key_package":null,"transfer_nonce":null,"owner_proof":null}});
    assert_eq!(
        mcp_request(router.clone(), &fixture, "/v1/mcp/device/activate", changed).await,
        StatusCode::CONFLICT
    );
    assert_eq!(worker.bodies("/internal/mcp/activate").await.len(), 2);

    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name,kind) VALUES($1,$2,'device','{}',$3,1,'worker','desktop')")
        .bind(fixture.vault)
        .bind(worker_device)
        .bind("11".repeat(32))
        .execute(&fixture.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO computer_enrollments(id,vault_id,owner_device_id,origin,claim_token_digest,server_nonce,recipient_device_id,recipient_kind,recipient_name,recipient_signing_public_key,recipient_transfer_public_key,recipient_credential_digest,recipient_commitment,profile_fingerprint,key_epoch,state,expires_at) VALUES($1,$2,(SELECT device_id FROM devices WHERE vault_id=$2 AND role='owner'),'https://peppy.test',$3,$3,$4,'desktop','worker',$3,$3,$3,$3,$5,1,'consumed',now()+interval '1 hour')")
        .bind(Uuid::new_v4())
        .bind(fixture.vault)
        .bind(vec![2u8; 32])
        .bind(worker_device)
        .bind("11".repeat(32))
        .execute(&fixture.pool)
        .await
        .unwrap();
    let completion = completion_request(operation, fixture.vault, worker_device);
    assert_eq!(
        mcp_request(
            router.clone(),
            &fixture,
            "/v1/mcp/device/complete",
            completion.clone()
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        mcp_request(router, &fixture, "/v1/mcp/device/complete", completion).await,
        StatusCode::OK
    );
    assert_eq!(worker.bodies("/internal/mcp/complete").await.len(), 1);
    fixture.cleanup().await;
}

#[tokio::test]
async fn complete_rejected_after_install_revokes_the_runtime_when_its_device_is_revoked_mid_rpc() {
    let fixture = Fixture::new().await;
    let worker = gated_fake_worker("/internal/mcp/complete").await;
    *worker.state.revoke_status.lock().await = StatusCode::SERVICE_UNAVAILABLE;
    let worker_config_lock = worker_config_lock().lock().await;
    let router = router_for_worker(fixture.pool.clone(), &worker, &worker_config_lock);
    let (operation, worker_device) = insert_activated_operation(&fixture).await;

    let complete = spawn_owner_post(
        router.clone(),
        &fixture,
        "/v1/mcp/device/complete",
        completion_request(operation, fixture.vault, worker_device),
    );
    worker.gate().arrived.notified().await;
    let (status, body) = spawn_owner_post(
        router.clone(),
        &fixture,
        format!("/v1/devices/{worker_device}/revoke"),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    worker.gate().release.notify_one();
    let (status, body) = complete.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("mcp_device_not_enrolled"), "{body}");

    let registered: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mcp_devices WHERE vault_id=$1 AND device_id=$2")
            .bind(fixture.vault)
            .bind(worker_device)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(registered, 0);
    let revoke =
        json!({"vault_id":fixture.vault.to_string(),"device_id":worker_device.to_string()});
    assert_eq!(
        worker.bodies("/internal/mcp/revoke").await,
        vec![revoke.clone()]
    );
    let (state, pending): (String, bool) =
        sqlx::query_as("SELECT state,cleanup_pending FROM mcp_operations WHERE id=$1")
            .bind(operation)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!((state.as_str(), pending), ("revoked", true));

    *worker.state.revoke_status.lock().await = StatusCode::NO_CONTENT;
    let _sweeping_router = router_for_worker(fixture.pool.clone(), &worker, &worker_config_lock);
    wait_for_count(
        &fixture.pool,
        SQL_CLEANUP_PENDING,
        operation,
        "the sweep to clear cleanup after a 204 revoke",
        |pending| pending == 0,
    )
    .await;
    assert_eq!(
        worker.bodies("/internal/mcp/revoke").await,
        vec![revoke.clone(), revoke]
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn complete_rejected_after_install_when_its_owner_is_revoked_mid_rpc() {
    let fixture = Fixture::new().await;
    let worker = gated_fake_worker("/internal/mcp/complete").await;
    let worker_config_lock = worker_config_lock().lock().await;
    let router = router_for_worker(fixture.pool.clone(), &worker, &worker_config_lock);
    let (operation, worker_device) = insert_activated_operation(&fixture).await;
    let second_owner = insert_second_owner(&fixture).await;

    let complete = spawn_owner_post(
        router.clone(),
        &fixture,
        "/v1/mcp/device/complete",
        completion_request(operation, fixture.vault, worker_device),
    );
    worker.gate().arrived.notified().await;
    let (status, body) = spawn_post(
        router,
        second_owner,
        fixture.session_token.clone(),
        format!("/v1/devices/{}/revoke", fixture.owner),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    worker.gate().release.notify_one();
    let (status, body) = complete.await.unwrap();
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert!(body.contains("invalid_bearer"), "{body}");

    let registered: i64 =
        sqlx::query_scalar("SELECT count(*) FROM mcp_devices WHERE vault_id=$1 AND device_id=$2")
            .bind(fixture.vault)
            .bind(worker_device)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(registered, 0, "a revoked owner must not register a device");
    assert_eq!(
        worker.bodies("/internal/mcp/revoke").await,
        vec![json!({"vault_id":fixture.vault.to_string(),"device_id":worker_device.to_string()})]
    );
    let (state, pending): (String, bool) =
        sqlx::query_as("SELECT state,cleanup_pending FROM mcp_operations WHERE id=$1")
            .bind(operation)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(
        (state.as_str(), pending),
        ("revoked", false),
        "a 204 revoke clears cleanup in the rejecting transaction"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn activate_rejected_when_its_owner_is_revoked_mid_rpc_leaves_the_operation_prepared() {
    let fixture = Fixture::new().await;
    let worker = gated_fake_worker("/internal/mcp/activate").await;
    let worker_config_lock = worker_config_lock().lock().await;
    let router = router_for_worker(fixture.pool.clone(), &worker, &worker_config_lock);
    let operation = Uuid::new_v4();
    sqlx::query("INSERT INTO mcp_operations(id,vault_id,owner_device_id,worker_device_id,state) VALUES($1,$2,$3,$4,'prepared')")
        .bind(operation)
        .bind(fixture.vault)
        .bind(fixture.owner)
        .bind(Uuid::new_v4())
        .execute(&fixture.pool)
        .await
        .unwrap();
    let second_owner = insert_second_owner(&fixture).await;
    let activation = json!({"operation_id":operation,"status":{"state":"approved","expires_in_seconds":60,"transcript":null,"encrypted_key_package":null,"transfer_nonce":null,"owner_proof":null}});

    let activate = spawn_owner_post(
        router.clone(),
        &fixture,
        "/v1/mcp/device/activate",
        activation,
    );
    worker.gate().arrived.notified().await;
    let (status, body) = spawn_post(
        router,
        second_owner,
        fixture.session_token.clone(),
        format!("/v1/devices/{}/revoke", fixture.owner),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    worker.gate().release.notify_one();
    let (status, body) = activate.await.unwrap();
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let state: String = sqlx::query_scalar("SELECT state FROM mcp_operations WHERE id=$1")
        .bind(operation)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        state, "prepared",
        "a revoked owner must not advance the operation"
    );
    fixture.cleanup().await;
}

/// Deletes the fixture vault through the real `DELETE /v1/vault` route as its owner.
fn spawn_vault_delete(router: Router, fixture: &Fixture) -> JoinHandle<(StatusCode, String)> {
    let owner_token = fixture.owner_token.clone();
    let session_token = fixture.session_token.clone();
    let vault = fixture.vault;
    tokio::spawn(async move {
        let response = router
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/v1/vault")
                    .header("authorization", format!("Bearer {owner_token}"))
                    .header("peppy-instance-session", session_token)
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "vault_id": vault }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    })
}

/// Vault deletion must not wait on an in-flight lifecycle RPC. The timeout turns a blocked
/// deletion (the round-3 lock cycle) into a failure instead of a hung test.
async fn delete_vault_while_gated(router: Router, fixture: &Fixture) {
    let (status, body) =
        tokio::time::timeout(Duration::from_secs(5), spawn_vault_delete(router, fixture))
            .await
            .expect("vault deletion must not wait on an in-flight MCP RPC")
            .unwrap();
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

#[tokio::test]
async fn complete_gated_on_worker_returns_not_found_after_vault_deletion_and_revokes_runtime() {
    let fixture = Fixture::new().await;
    let worker = gated_fake_worker("/internal/mcp/complete").await;
    let worker_config_lock = worker_config_lock().lock().await;
    let router = router_for_worker(fixture.pool.clone(), &worker, &worker_config_lock);
    let (operation, worker_device) = insert_activated_operation(&fixture).await;

    let complete = spawn_owner_post(
        router.clone(),
        &fixture,
        "/v1/mcp/device/complete",
        completion_request(operation, fixture.vault, worker_device),
    );
    worker.gate().arrived.notified().await;
    delete_vault_while_gated(router, &fixture).await;
    worker.gate().release.notify_one();

    let (status, body) = complete.await.unwrap();
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("mcp_operation_not_found"), "{body}");
    assert!(!body.contains("deadlock"), "{body}");
    assert_eq!(
        worker.bodies("/internal/mcp/revoke").await,
        vec![json!({"vault_id":fixture.vault.to_string(),"device_id":worker_device.to_string()})],
        "the installed runtime must be revoked best-effort"
    );
    fixture.cleanup().await;
}

#[tokio::test]
async fn activate_gated_on_worker_returns_not_found_after_vault_deletion() {
    let fixture = Fixture::new().await;
    let worker = gated_fake_worker("/internal/mcp/activate").await;
    let worker_config_lock = worker_config_lock().lock().await;
    let router = router_for_worker(fixture.pool.clone(), &worker, &worker_config_lock);
    let operation = Uuid::new_v4();
    sqlx::query("INSERT INTO mcp_operations(id,vault_id,owner_device_id,worker_device_id,state) VALUES($1,$2,$3,$4,'prepared')")
        .bind(operation)
        .bind(fixture.vault)
        .bind(fixture.owner)
        .bind(Uuid::new_v4())
        .execute(&fixture.pool)
        .await
        .unwrap();
    let activation = json!({"operation_id":operation,"status":{"state":"approved","expires_in_seconds":60,"transcript":null,"encrypted_key_package":null,"transfer_nonce":null,"owner_proof":null}});

    let activate = spawn_owner_post(
        router.clone(),
        &fixture,
        "/v1/mcp/device/activate",
        activation,
    );
    worker.gate().arrived.notified().await;
    delete_vault_while_gated(router, &fixture).await;
    worker.gate().release.notify_one();

    let (status, body) = activate.await.unwrap();
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("mcp_operation_not_found"), "{body}");
    fixture.cleanup().await;
}

#[tokio::test]
async fn prepare_gated_on_worker_cancels_its_claimant_after_vault_deletion() {
    let fixture = Fixture::new().await;
    let worker = gated_fake_worker("/internal/mcp/prepare").await;
    let worker_config_lock = worker_config_lock().lock().await;
    let router = router_for_worker(fixture.pool.clone(), &worker, &worker_config_lock);

    let prepare = spawn_owner_post(
        router.clone(),
        &fixture,
        "/v1/mcp/device/prepare",
        json!({}),
    );
    worker.gate().arrived.notified().await;
    delete_vault_while_gated(router, &fixture).await;
    worker.gate().release.notify_one();

    let (status, body) = prepare.await.unwrap();
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let prepared_operation: Uuid = {
        let bodies = worker.bodies("/internal/mcp/prepare").await;
        assert_eq!(bodies.len(), 1);
        // The worker minted the claimant; the cancel must name the same invitation.
        let cancels = worker.bodies("/internal/mcp/cancel").await;
        assert_eq!(cancels.len(), 1, "the new claimant must be cancelled");
        assert_eq!(cancels[0]["vault_id"], fixture.vault.to_string());
        assert_eq!(cancels[0]["owner_device_id"], fixture.owner.to_string());
        cancels[0]["invitation_id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    };
    assert_ne!(prepared_operation, Uuid::nil());
    fixture.cleanup().await;
}
