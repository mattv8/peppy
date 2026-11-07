use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use peppy_domain::VaultId;
use peppy_protocol::{EphemeralSignalV2, LiveHelloV2};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Notify, oneshot, watch},
    task::JoinHandle,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        self,
        client::IntoClientRequest,
        protocol::{CloseFrame, frame::coding::CloseCode},
    },
};

use super::*;

const TRACE_COLLECTOR_SENTINEL: &str = "live_test_backend_publish_observed";

#[derive(Clone)]
struct FakeLiveBackend {
    started: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
    fail: Arc<AtomicBool>,
    release: Arc<Notify>,
    stall: bool,
}

impl FakeLiveBackend {
    fn immediate() -> Self {
        Self {
            started: Arc::new(AtomicUsize::new(0)),
            completed: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicUsize::new(0)),
            fail: Arc::new(AtomicBool::new(false)),
            release: Arc::new(Notify::new()),
            stall: false,
        }
    }

    fn stalled() -> Self {
        Self {
            stall: true,
            ..Self::immediate()
        }
    }
}

struct GatedPolicy {
    enabled: AtomicBool,
    passes_before_gate: AtomicUsize,
    gate: LiveTicketTestGate,
}

impl GatedPolicy {
    fn new(gate: LiveTicketTestGate) -> Self {
        Self {
            enabled: AtomicBool::new(false),
            passes_before_gate: AtomicUsize::new(1),
            gate,
        }
    }

    fn enable(&self) {
        self.enabled.store(true, Ordering::SeqCst);
    }
}

impl AccessPolicy for GatedPolicy {
    fn authorize(
        &self,
        _principal: AccessPrincipal,
        _operation: Operation,
    ) -> PolicyFuture<'_, bool> {
        Box::pin(async move {
            if self.enabled.load(Ordering::SeqCst)
                && self
                    .passes_before_gate
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                        remaining.checked_sub(1)
                    })
                    .is_err()
            {
                self.gate.enter().await;
            }
            true
        })
    }
}

impl crate::live::LiveBackend for FakeLiveBackend {
    fn publish(
        &self,
        _vault_id: VaultId,
        _frame: EphemeralSignalV2,
    ) -> Pin<Box<dyn Future<Output = Result<(), crate::live::LiveBackendError>> + Send + '_>> {
        Box::pin(async move {
            let mut drop_acknowledgement = PublishDropAcknowledgement {
                dropped: self.dropped.clone(),
                completed: false,
            };
            self.started.fetch_add(1, Ordering::SeqCst);
            tracing::info!(
                safe_marker = TRACE_COLLECTOR_SENTINEL,
                "live_test_backend_publish"
            );
            if self.stall {
                self.release.notified().await;
            }
            drop_acknowledgement.completed = true;
            if self.fail.load(Ordering::SeqCst) {
                return Err(crate::live::LiveBackendError);
            }
            self.completed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

struct PublishDropAcknowledgement {
    dropped: Arc<AtomicUsize>,
    completed: bool,
}

impl Drop for PublishDropAcknowledgement {
    fn drop(&mut self) {
        if !self.completed {
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
}

struct DelayedCommitProxy {
    proxy_url: String,
    armed: Arc<AtomicBool>,
    commit_seen: Arc<AtomicBool>,
    commit_seen_notify: Arc<Notify>,
    release: Arc<tokio::sync::Semaphore>,
    acknowledgement_seen: Arc<AtomicBool>,
    acknowledgement_notify: Arc<Notify>,
    task: JoinHandle<()>,
}

impl DelayedCommitProxy {
    async fn start(database_url: &str) -> Self {
        Self::start_with_mode(database_url, false).await
    }

    async fn start_holding_acknowledgement(database_url: &str) -> Self {
        Self::start_with_mode(database_url, true).await
    }

    async fn start_with_mode(database_url: &str, delay_acknowledgement: bool) -> Self {
        let upstream_url = url::Url::parse(database_url).unwrap();
        let upstream_address = format!(
            "{}:{}",
            upstream_url.host_str().unwrap(),
            upstream_url.port().unwrap_or(5432)
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut proxy_url = upstream_url;
        proxy_url.set_host(Some("127.0.0.1")).unwrap();
        proxy_url.set_port(Some(address.port())).unwrap();
        proxy_url
            .query_pairs_mut()
            .append_pair("sslmode", "disable");

        let armed = Arc::new(AtomicBool::new(false));
        let commit_seen = Arc::new(AtomicBool::new(false));
        let commit_seen_notify = Arc::new(Notify::new());
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let acknowledgement_seen = Arc::new(AtomicBool::new(false));
        let acknowledgement_notify = Arc::new(Notify::new());
        let task = {
            let armed = armed.clone();
            let commit_seen = commit_seen.clone();
            let commit_seen_notify = commit_seen_notify.clone();
            let release = release.clone();
            let acknowledgement_seen = acknowledgement_seen.clone();
            let acknowledgement_notify = acknowledgement_notify.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((client, _)) = listener.accept().await else {
                        return;
                    };
                    let upstream_address = upstream_address.clone();
                    let armed = armed.clone();
                    let commit_seen = commit_seen.clone();
                    let commit_seen_notify = commit_seen_notify.clone();
                    let release = release.clone();
                    let acknowledgement_seen = acknowledgement_seen.clone();
                    let acknowledgement_notify = acknowledgement_notify.clone();
                    tokio::spawn(async move {
                        let upstream = TcpStream::connect(upstream_address).await.unwrap();
                        let (mut client_read, mut client_write) = client.into_split();
                        let (mut upstream_read, mut upstream_write) = upstream.into_split();
                        let awaiting_acknowledgement = Arc::new(AtomicBool::new(false));
                        let awaiting_acknowledgement_for_server = awaiting_acknowledgement.clone();
                        let acknowledgement_release = release.clone();

                        let client_to_server = tokio::spawn(async move {
                            let Ok(length) = client_read.read_u32().await else {
                                return;
                            };
                            let mut startup = vec![0; length as usize - 4];
                            if client_read.read_exact(&mut startup).await.is_err() {
                                return;
                            }
                            if upstream_write.write_u32(length).await.is_err()
                                || upstream_write.write_all(&startup).await.is_err()
                            {
                                return;
                            }
                            loop {
                                let Ok(kind) = client_read.read_u8().await else {
                                    return;
                                };
                                let Ok(length) = client_read.read_u32().await else {
                                    return;
                                };
                                let mut body = vec![0; length as usize - 4];
                                if client_read.read_exact(&mut body).await.is_err() {
                                    return;
                                }
                                let hold_commit = kind == b'Q'
                                    && body == b"COMMIT\0"
                                    && armed
                                        .compare_exchange(
                                            true,
                                            false,
                                            Ordering::SeqCst,
                                            Ordering::SeqCst,
                                        )
                                        .is_ok();
                                if hold_commit {
                                    commit_seen.store(true, Ordering::SeqCst);
                                    commit_seen_notify.notify_waiters();
                                    if !delay_acknowledgement {
                                        release
                                            .acquire()
                                            .await
                                            .expect("delayed commit proxy remains open")
                                            .forget();
                                    }
                                    awaiting_acknowledgement.store(true, Ordering::SeqCst);
                                }
                                if upstream_write.write_u8(kind).await.is_err()
                                    || upstream_write.write_u32(length).await.is_err()
                                    || upstream_write.write_all(&body).await.is_err()
                                {
                                    return;
                                }
                            }
                        });

                        while let Ok(kind) = upstream_read.read_u8().await {
                            let Ok(length) = upstream_read.read_u32().await else {
                                break;
                            };
                            let mut body = vec![0; length as usize - 4];
                            if upstream_read.read_exact(&mut body).await.is_err() {
                                break;
                            }
                            if delay_acknowledgement
                                && kind == b'C'
                                && body == b"COMMIT\0"
                                && awaiting_acknowledgement_for_server.load(Ordering::SeqCst)
                            {
                                // PostgreSQL has committed. Withhold CommandComplete
                                // (and subsequent ReadyForQuery) from the SQLx caller.
                                acknowledgement_seen.store(true, Ordering::SeqCst);
                                acknowledgement_notify.notify_waiters();
                                acknowledgement_release
                                    .acquire()
                                    .await
                                    .expect("acknowledgement proxy remains open")
                                    .forget();
                            }
                            if kind == b'Z'
                                && awaiting_acknowledgement_for_server.swap(false, Ordering::SeqCst)
                            {
                                acknowledgement_seen.store(true, Ordering::SeqCst);
                                acknowledgement_notify.notify_waiters();
                            }
                            if (client_write.write_u8(kind).await.is_err()
                                || client_write.write_u32(length).await.is_err()
                                || client_write.write_all(&body).await.is_err())
                                && acknowledgement_seen.load(Ordering::SeqCst)
                            {
                                break;
                            }
                        }
                        client_to_server.abort();
                    });
                }
            })
        };

        Self {
            proxy_url: proxy_url.into(),
            armed,
            commit_seen,
            commit_seen_notify,
            release,
            acknowledgement_seen,
            acknowledgement_notify,
            task,
        }
    }

    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    async fn wait_for_commit(&self) {
        loop {
            let notified = self.commit_seen_notify.notified();
            if self.commit_seen.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    fn release_commit(&self) {
        self.release.add_permits(1);
    }

    async fn wait_for_acknowledgement(&self) {
        loop {
            let notified = self.acknowledgement_notify.notified();
            if self.acknowledgement_seen.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    fn shutdown(self) {
        self.task.abort();
    }
}

mod http_budget_tests;
mod replica_tests;

struct LiveTestServer {
    pool: PgPool,
    admin: PgPool,
    schema: String,
    base_url: String,
    ws_url: String,
    task: JoinHandle<()>,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    readiness_sender: watch::Sender<bool>,
    live_hub: crate::live::LiveHub,
    state: ApiState,
    owner_token: String,
    owner_device: Uuid,
    vault: Uuid,
    peer_token: String,
    peer_device: Uuid,
}

#[derive(Debug, Eq, PartialEq)]
struct LiveTicketWriteCounts {
    presence: i64,
    tickets: i64,
    sessions: i64,
    admissions: i64,
}

struct LiveTestOptions {
    backend: FakeLiveBackend,
    ready: bool,
    publish_timeout: Duration,
    trusted_origin: bool,
    policy: Arc<dyn AccessPolicy>,
    ticket_hooks: LiveTicketTestHooks,
    max_connections: u32,
    database_proxy_url: Option<String>,
    trusted_proxy_cidrs: Vec<IpNet>,
}

impl LiveTestOptions {
    fn new(backend: FakeLiveBackend, ready: bool) -> Self {
        Self {
            backend,
            ready,
            publish_timeout: LIVE_PUBLISH_TIMEOUT,
            trusted_origin: true,
            policy: Arc::new(CommunityAccessPolicy),
            ticket_hooks: LiveTicketTestHooks::default(),
            max_connections: 5,
            database_proxy_url: None,
            trusted_proxy_cidrs: Vec::new(),
        }
    }
}

impl LiveTestServer {
    async fn start(backend: FakeLiveBackend, ready: bool) -> Self {
        Self::start_with_options(LiveTestOptions::new(backend, ready)).await
    }

    async fn start_without_trusted_origin(backend: FakeLiveBackend, ready: bool) -> Self {
        let mut options = LiveTestOptions::new(backend, ready);
        options.trusted_origin = false;
        Self::start_with_options(options).await
    }

    async fn start_with_publish_timeout(
        backend: FakeLiveBackend,
        ready: bool,
        publish_timeout: Duration,
    ) -> Self {
        Self::start_with_configuration(backend, ready, publish_timeout).await
    }

    async fn start_with_configuration(
        backend: FakeLiveBackend,
        ready: bool,
        publish_timeout: Duration,
    ) -> Self {
        let mut options = LiveTestOptions::new(backend, ready);
        options.publish_timeout = publish_timeout;
        Self::start_with_options(options).await
    }

    async fn start_with_options(options: LiveTestOptions) -> Self {
        let LiveTestOptions {
            backend,
            ready,
            publish_timeout,
            trusted_origin,
            policy,
            ticket_hooks,
            max_connections,
            database_proxy_url,
            trusted_proxy_cidrs,
        } = options;
        let database_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL is required; live socket tests never silently skip");
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let schema = format!("peppy_live_test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let mut isolated_url: url::Url = database_proxy_url
            .as_deref()
            .unwrap_or(&database_url)
            .parse()
            .unwrap();
        isolated_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(isolated_url.as_str())
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        let vault = Uuid::new_v4();
        let salt: Vec<u8> = (0..16).collect();
        let profile =
            serde_json::json!({"crypto_suite": 1, "salt": salt, "vault_id": vault, "key_epoch": 1});
        let mut digest = Sha256::new();
        digest.update(b"peppy-key-profile-v1\0");
        digest.update(1_u16.to_be_bytes());
        digest.update((0_u8..16).collect::<Vec<_>>());
        digest.update(vault.as_bytes());
        digest.update(1_u32.to_be_bytes());
        let fingerprint = hex::encode(digest.finalize());
        let owner = create_owner(&pool, profile, vec![7, 8, 9], fingerprint.clone(), 1)
            .await
            .unwrap();
        let peer_device = Uuid::new_v4();
        let peer_token = (0..3)
            .map(|_| Uuid::new_v4().simple().to_string())
            .collect::<String>();
        sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch) VALUES($1,$2,'device',$3,$4,1)")
            .bind(vault)
            .bind(peer_device)
            .bind(serde_json::json!({"fixture": true}))
            .bind(&fingerprint)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)",
        )
        .bind(Sha256::digest(peer_token.as_bytes()).as_slice())
        .bind(vault)
        .bind(peer_device)
        .execute(&pool)
        .await
        .unwrap();

        let (readiness_sender, readiness) = watch::channel(ready);
        let live_hub_observer = Arc::new(Mutex::new(None));
        let live_state_observer = Arc::new(Mutex::new(None));
        let authorization = RouterAuthorization {
            policy,
            enrollment: Arc::new(CommunityEnrollmentAuthorization::new(pool.clone())),
            claim_policy: crate::instance::configured_claim_policy(false).unwrap(),
            row_security: false,
            rls_enforced: false,
        };
        let mut config = test_config(&database_url);
        config.trusted_proxy_cidrs = trusted_proxy_cidrs;
        if !trusted_origin {
            config.web_live_trusted_origin = None;
        }
        let (router, _maintenance) = build_router_with_live_backend(
            pool.clone(),
            RouterBuildOptions {
                config: Some(config),
                initialized_storage: None,
                transport: TransportOptions::default(),
                explicit_relay_url: None,
                push_advertisement: PushConfigAdvertisement::default(),
            },
            authorization,
            Some(LiveBackendOverride {
                backend: Arc::new(backend),
                readiness,
                publish_timeout,
                hub_observer: Some(live_hub_observer.clone()),
                state_observer: Some(live_state_observer.clone()),
                ticket_hooks,
            }),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            crate::serving::serve(listener, router, async move {
                let _ = shutdown_receiver.await;
            })
            .await
            .unwrap()
        });
        Self {
            pool,
            admin,
            schema,
            base_url: format!("http://{address}"),
            ws_url: format!("ws://{address}/v1/live"),
            task,
            shutdown: Mutex::new(Some(shutdown_sender)),
            readiness_sender,
            live_hub: live_hub_observer
                .lock()
                .unwrap()
                .clone()
                .expect("live test router must expose its hub"),
            state: live_state_observer
                .lock()
                .unwrap()
                .clone()
                .expect("live test router must expose its state"),
            owner_token: owner.device_token,
            owner_device: owner.device_id,
            vault,
            peer_token,
            peer_device,
        }
    }

    async fn connect(
        &self,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        self.connect_with_token(&self.owner_token).await
    }

    async fn connect_peer(
        &self,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        self.connect_with_token(&self.peer_token).await
    }

    async fn connect_with_token(
        &self,
        token: &str,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let mut request = self.ws_url.clone().into_client_request().unwrap();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        connect_async(request).await.unwrap().0
    }

    async fn connect_with_ticket(
        &self,
        session_id: &str,
        ticket: &str,
    ) -> (
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        String,
    ) {
        let protocol = live_ticket_protocol(session_id, ticket);
        let mut request = self.ws_url.clone().into_client_request().unwrap();
        request
            .headers_mut()
            .insert("origin", "https://web.peppy.test".parse().unwrap());
        request
            .headers_mut()
            .insert("sec-websocket-protocol", protocol.parse().unwrap());
        let (socket, response) = connect_async(request).await.unwrap();
        assert_eq!(
            response.headers().get("sec-websocket-protocol").unwrap(),
            &protocol
        );
        (socket, protocol)
    }

    async fn ticket_upgrade_status(
        &self,
        session_id: &str,
        ticket: &str,
        origin: Option<&str>,
        bearer: bool,
    ) -> StatusCode {
        let mut request = self.ws_url.clone().into_client_request().unwrap();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            live_ticket_protocol(session_id, ticket).parse().unwrap(),
        );
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert("origin", origin.parse().unwrap());
        }
        if bearer {
            request.headers_mut().insert(
                "authorization",
                format!("Bearer {}", self.owner_token).parse().unwrap(),
            );
        }
        let tungstenite::Error::Http(response) = connect_async(request).await.unwrap_err() else {
            panic!("ticket upgrade unexpectedly succeeded")
        };
        response.status()
    }

    async fn raw_ticket_upgrade_status(&self, protocol: &str) -> StatusCode {
        let address = self.base_url.strip_prefix("http://").unwrap();
        let mut stream = TcpStream::connect(address).await.unwrap();
        let websocket_key = STANDARD.encode([0_u8; 16]);
        let request = format!(
            "GET /v1/live HTTP/1.1\r\nHost: {address}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {websocket_key}\r\nOrigin: https://web.peppy.test\r\nSec-WebSocket-Protocol: {protocol}\r\nSec-WebSocket-Protocol: {protocol}\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut bytes = [0_u8; 1024];
        let count = stream.read(&mut bytes).await.unwrap();
        let response = std::str::from_utf8(&bytes[..count]).unwrap();
        let status = response.lines().next().unwrap();
        StatusCode::from_u16(status.split_whitespace().nth(1).unwrap().parse().unwrap()).unwrap()
    }

    async fn snapshot(&self) -> serde_json::Value {
        reqwest::Client::new()
            .get(format!("{}/v1/snapshot", self.base_url))
            .bearer_auth(&self.owner_token)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn shutdown(self) {
        self.begin_shutdown();
        self.task.abort();
        let _ = self.task.await;
        self.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }

    fn begin_shutdown(&self) {
        if let Some(sender) = self.shutdown.lock().unwrap().take() {
            let _ = sender.send(());
        }
    }
}

fn test_config(database_url: &str) -> crate::config::Config {
    crate::config::Config {
        bind_addr: "127.0.0.1:0".parse().unwrap(),
        database_url: database_url.into(),
        release_identity: "test".into(),
        s3: None,
        public_api_url: None,
        public_attachment_url: None,
        vault_attachment_quota_bytes: 512 * 1024 * 1024,
        trusted_proxy_cidrs: Vec::new(),
        replay_retention: Duration::from_secs(30 * 86_400),
        relay_url: None,
        web_client: None,
        web_live_trusted_origin: Some(
            "https://web.peppy.test"
                .parse::<url::Url>()
                .unwrap()
                .origin(),
        ),
    }
}

fn signal(sender: Uuid) -> String {
    signal_with_ciphertext(sender, "AQ==")
}

fn signal_with_ciphertext(sender: Uuid, ciphertext_b64: &str) -> String {
    signal_with_kind_and_ciphertext(sender, "typing", ciphertext_b64)
}

fn signal_with_kind_and_ciphertext(sender: Uuid, kind: &str, ciphertext_b64: &str) -> String {
    let now = live_now_ms();
    serde_json::json!({
        "protocol_version": 2,
        "kind": kind,
        "sender_device_id": sender,
        "key_epoch": 1,
        "session_id": "AAAAAAAAAAAAAAAAAAAAAA",
        "sent_at_ms": now.to_string(),
        "expires_at_ms": (now + 15_000).to_string(),
        "sequence": "1",
        "ciphertext_b64": ciphertext_b64
    })
    .to_string()
}

fn legacy_v1_signal(sender: Uuid) -> String {
    let now = live_now_ms();
    serde_json::json!({
        "protocol_version": 1,
        "kind": "typing",
        "sender_device_id": sender,
        "key_epoch": 1,
        "sent_at_ms": now.to_string(),
        "expires_at_ms": (now + 15_000).to_string(),
        "sequence": "1",
        "ciphertext_b64": "AQ=="
    })
    .to_string()
}

fn pad_json_to(mut json: String, byte_length: usize) -> String {
    assert!(json.len() <= byte_length);
    json.extend(std::iter::repeat_n(' ', byte_length - json.len()));
    json
}

async fn expect_strict_hello(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    let first = socket.next().await.unwrap().unwrap();
    let tungstenite::Message::Text(text) = first else {
        panic!("hello was not the first text frame")
    };
    assert_eq!(
        serde_json::from_str::<LiveHelloV2>(&text).unwrap(),
        LiveHelloV2::new()
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        serde_json::json!({
            "type": "hello",
            "protocol_version": 2,
            "max_ttl_ms": 15000,
            "max_ciphertext_bytes": 5400
        })
    );
}

async fn next_close(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> CloseFrame {
    loop {
        match tokio::time::timeout(Duration::from_secs(7), socket.next())
            .await
            .expect("socket did not close")
            .expect("socket ended without close")
            .expect("websocket read failed")
        {
            tungstenite::Message::Close(Some(frame)) => return frame,
            tungstenite::Message::Ping(payload) => socket
                .send(tungstenite::Message::Pong(payload))
                .await
                .unwrap(),
            _ => {}
        }
    }
}

async fn wait_for_count(counter: &AtomicUsize, expected: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while counter.load(Ordering::SeqCst) != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("counter did not reach expected value");
}

async fn wait_for_atomic_count(counter: &AtomicUsize, expected: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if counter.load(Ordering::SeqCst) >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("test hook counter did not reach expected value");
}

async fn first_socket_message(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Option<Result<tungstenite::Message, tungstenite::Error>> {
    tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("socket did not settle after test signal")
}

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

struct CapturedLogWriter(Arc<Mutex<Vec<u8>>>);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CapturedLogWriter(self.0.clone())
    }
}

impl std::io::Write for CapturedLogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl CapturedLogs {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

async fn next_text(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> String {
    loop {
        match tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("socket did not receive live frame")
            .expect("socket ended before live frame")
            .expect("websocket read failed")
        {
            tungstenite::Message::Text(text) => return text.to_string(),
            tungstenite::Message::Ping(payload) => socket
                .send(tungstenite::Message::Pong(payload))
                .await
                .unwrap(),
            message => panic!("unexpected websocket frame: {message:?}"),
        }
    }
}

#[tokio::test]
async fn unauthenticated_live_upgrade_is_rejected_before_switching_protocols() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let error = connect_async(&server.ws_url).await.unwrap_err();
    let tungstenite::Error::Http(response) = error else {
        panic!("expected HTTP rejection")
    };
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    server.shutdown().await;
}

#[tokio::test]
async fn authenticated_live_socket_receives_strict_hello_as_first_frame() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let mut socket = server.connect().await;
    expect_strict_hello(&mut socket).await;
    server.shutdown().await;
}

#[tokio::test]
async fn legacy_v1_frame_is_rejected_without_publication() {
    let backend = FakeLiveBackend::immediate();
    let server = LiveTestServer::start(backend.clone(), true).await;
    let mut socket = server.connect().await;
    expect_strict_hello(&mut socket).await;
    socket
        .send(tungstenite::Message::Text(
            legacy_v1_signal(server.owner_device).into(),
        ))
        .await
        .unwrap();
    let close = next_close(&mut socket).await;
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "invalid_live_frame");
    assert_eq!(backend.started.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn canonical_controls_forward_exact_v2_fields() {
    let backend = FakeLiveBackend::immediate();
    let server = LiveTestServer::start(backend.clone(), true).await;
    let mut sender = server.connect().await;
    expect_strict_hello(&mut sender).await;
    let mut peer = server.connect_peer().await;
    expect_strict_hello(&mut peer).await;

    for (kind, ciphertext) in [("challenge", "Ag=="), ("session_ack", "Aw==")] {
        let frame = signal_with_kind_and_ciphertext(server.owner_device, kind, ciphertext);
        sender
            .send(tungstenite::Message::Text(frame.clone().into()))
            .await
            .unwrap();
        let received = next_text(&mut peer).await;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&received).unwrap(),
            serde_json::from_str::<serde_json::Value>(&frame).unwrap()
        );
    }
    wait_for_count(&backend.completed, 2).await;
    server.shutdown().await;
}

#[tokio::test]
async fn sender_mismatch_closes_with_fixed_policy_reason() {
    let backend = FakeLiveBackend::immediate();
    let server = LiveTestServer::start(backend.clone(), true).await;
    let mut socket = server.connect().await;
    expect_strict_hello(&mut socket).await;
    socket
        .send(tungstenite::Message::Text(signal(Uuid::new_v4()).into()))
        .await
        .unwrap();
    let close = next_close(&mut socket).await;
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "invalid_live_frame");
    assert_eq!(backend.started.load(Ordering::SeqCst), 0);
    assert_eq!(backend.completed.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn replaced_socket_with_inflight_output_cannot_finish_publish_after_takeover() {
    let backend = FakeLiveBackend::stalled();
    let server = LiveTestServer::start(backend.clone(), true).await;
    let mut replaced = server.connect().await;
    expect_strict_hello(&mut replaced).await;
    replaced
        .send(tungstenite::Message::Text(
            signal(server.owner_device).into(),
        ))
        .await
        .unwrap();
    wait_for_count(&backend.started, 1).await;

    let mut replacement = server.connect().await;
    expect_strict_hello(&mut replacement).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while replaced.next().await.is_some() {}
    })
    .await
    .expect("replaced socket remained active");
    wait_for_count(&backend.dropped, 1).await;
    backend.release.notify_waiters();
    assert_eq!(backend.completed.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn stalled_backend_publish_is_cancelled_within_a_bounded_takeover_window() {
    let backend = FakeLiveBackend::stalled();
    let server = LiveTestServer::start(backend.clone(), true).await;
    let mut first = server.connect().await;
    expect_strict_hello(&mut first).await;
    first
        .send(tungstenite::Message::Text(
            signal(server.owner_device).into(),
        ))
        .await
        .unwrap();
    wait_for_count(&backend.started, 1).await;
    let started = tokio::time::Instant::now();
    let mut replacement = server.connect().await;
    expect_strict_hello(&mut replacement).await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while first.next().await.is_some() {}
    })
    .await
    .expect("stalled publish ignored cancellation");
    assert!(started.elapsed() < Duration::from_secs(2));
    wait_for_count(&backend.dropped, 1).await;
    assert_eq!(backend.completed.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn stalled_backend_publish_times_out_without_takeover() {
    let backend = FakeLiveBackend::stalled();
    let server = LiveTestServer::start_with_publish_timeout(
        backend.clone(),
        true,
        Duration::from_millis(100),
    )
    .await;
    let mut socket = server.connect().await;
    expect_strict_hello(&mut socket).await;
    socket
        .send(tungstenite::Message::Text(
            signal(server.owner_device).into(),
        ))
        .await
        .unwrap();
    wait_for_count(&backend.started, 1).await;

    let close = next_close(&mut socket).await;
    assert_eq!(close.code, CloseCode::Error);
    assert_eq!(close.reason, "live_unavailable");
    wait_for_count(&backend.dropped, 1).await;
    assert_eq!(backend.completed.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn ticket_validation_and_stalled_publish_share_the_original_budget() {
    let backend = FakeLiveBackend::stalled();
    let validation_gate = LiveTicketTestGate::new(2);
    let hooks = LiveTicketTestHooks {
        operation_budget: Some(Duration::from_millis(350)),
        recheck_interval: Some(Duration::from_secs(5)),
        after_sample: Some(validation_gate.clone()),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(backend.clone(), true);
    options.publish_timeout = Duration::from_secs(2);
    options.ticket_hooks = hooks;
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([87_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let connect = spawn_ticket_connect(&server, session_id, ticket);

    bounded(validation_gate.wait_for_entries(1)).await;
    validation_gate.release_one();
    let mut socket = bounded(connect).await.unwrap().unwrap();
    bounded(expect_strict_hello(&mut socket)).await;

    let started = tokio::time::Instant::now();
    socket
        .send(tungstenite::Message::Text(
            signal(server.owner_device).into(),
        ))
        .await
        .unwrap();
    bounded(validation_gate.wait_for_entries(2)).await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    validation_gate.release_one();
    wait_for_count(&backend.started, 1).await;

    let close = tokio::time::timeout(Duration::from_millis(700), next_close(&mut socket))
        .await
        .expect("ticket publish received a fresh backend timeout");
    assert_eq!(close.code, CloseCode::Error);
    assert_eq!(close.reason, "live_unavailable");
    assert!(started.elapsed() < Duration::from_millis(700));
    wait_for_count(&backend.dropped, 1).await;
    assert_eq!(backend.completed.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn bearer_publish_keeps_its_backend_timeout_fallback() {
    let backend = FakeLiveBackend::stalled();
    let hooks = LiveTicketTestHooks {
        operation_budget: Some(Duration::from_millis(50)),
        recheck_interval: Some(Duration::from_secs(5)),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(backend.clone(), true);
    options.publish_timeout = Duration::from_millis(500);
    options.ticket_hooks = hooks;
    let server = LiveTestServer::start_with_options(options).await;
    let mut socket = server.connect().await;
    expect_strict_hello(&mut socket).await;

    socket
        .send(tungstenite::Message::Text(
            signal(server.owner_device).into(),
        ))
        .await
        .unwrap();
    wait_for_count(&backend.started, 1).await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(backend.dropped.load(Ordering::SeqCst), 0);
    backend.release.notify_waiters();
    wait_for_count(&backend.completed, 1).await;

    socket
        .send(tungstenite::Message::Ping(
            b"bearer-still-open".to_vec().into(),
        ))
        .await
        .unwrap();
    loop {
        match bounded(socket.next()).await.unwrap().unwrap() {
            tungstenite::Message::Pong(payload) => {
                assert_eq!(payload.as_ref(), b"bearer-still-open");
                break;
            }
            tungstenite::Message::Close(frame) => {
                panic!("bearer fallback was constrained by ticket budget: {frame:?}")
            }
            _ => {}
        }
    }
    server.shutdown().await;
}

#[tokio::test]
async fn unavailable_backend_rejects_authenticated_upgrade_with_503() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), false).await;
    let mut request = server.ws_url.clone().into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", server.owner_token).parse().unwrap(),
    );
    let error = connect_async(request).await.unwrap_err();
    let tungstenite::Error::Http(response) = error else {
        panic!("expected HTTP rejection")
    };
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    server.shutdown().await;
}

#[tokio::test]
async fn v2_per_kind_ciphertext_caps_apply_at_socket_boundary() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    for (kind, limit) in [
        ("typing", 5_400_usize),
        ("challenge", 512),
        ("session_ack", 512),
    ] {
        let backend = FakeLiveBackend::immediate();
        let server = LiveTestServer::start(backend.clone(), true).await;
        let mut socket = server.connect().await;
        expect_strict_hello(&mut socket).await;

        let at_limit = signal_with_kind_and_ciphertext(
            server.owner_device,
            kind,
            &STANDARD.encode(vec![0_u8; limit]),
        );
        socket
            .send(tungstenite::Message::Text(at_limit.into()))
            .await
            .unwrap();
        wait_for_count(&backend.completed, 1).await;

        let oversized = signal_with_kind_and_ciphertext(
            server.owner_device,
            kind,
            &STANDARD.encode(vec![0_u8; limit + 1]),
        );
        socket
            .send(tungstenite::Message::Text(oversized.into()))
            .await
            .unwrap();
        let close = next_close(&mut socket).await;
        assert_eq!(close.code, CloseCode::Policy, "{kind}");
        assert_eq!(close.reason, "invalid_live_frame", "{kind}");
        assert_eq!(backend.started.load(Ordering::SeqCst), 1, "{kind}");
        server.shutdown().await;
    }
}

#[tokio::test]
async fn native_message_limit_rejects_oversized_valid_json_before_publication() {
    let backend = FakeLiveBackend::immediate();
    let server = LiveTestServer::start(backend.clone(), true).await;
    let mut socket = server.connect().await;
    expect_strict_hello(&mut socket).await;

    let at_limit = pad_json_to(signal(server.owner_device), 8_192);
    socket
        .send(tungstenite::Message::Text(at_limit.into()))
        .await
        .unwrap();
    wait_for_count(&backend.completed, 1).await;

    let oversized = pad_json_to(signal(server.owner_device), 8_193);
    socket
        .send(tungstenite::Message::Text(oversized.into()))
        .await
        .unwrap();
    let close = next_close(&mut socket).await;
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "invalid_live_frame");
    assert_eq!(backend.started.load(Ordering::SeqCst), 1);
    assert_eq!(backend.completed.load(Ordering::SeqCst), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn live_upgrade_retains_connection_lease_until_disconnect() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let mut socket = server.connect().await;
    expect_strict_hello(&mut socket).await;

    server.begin_shutdown();
    let closed = tokio::time::timeout(Duration::from_secs(1), socket.next()).await;
    assert!(closed.is_ok());
    server.shutdown().await;
}

#[tokio::test]
async fn repeated_live_upgrades_without_frames_are_rate_limited() {
    const SETUP_TIMEOUT: Duration = Duration::from_secs(30);
    const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);
    const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

    let server = tokio::time::timeout(
        SETUP_TIMEOUT,
        LiveTestServer::start(FakeLiveBackend::immediate(), true),
    )
    .await
    .expect("live test server did not start");
    let mut sockets = Vec::new();
    for attempt in 0..crate::live::LIVE_UPGRADE_RATE_LIMIT {
        let mut socket = tokio::time::timeout(SOCKET_TIMEOUT, server.connect())
            .await
            .unwrap_or_else(|_| panic!("live upgrade {attempt} did not complete"));
        tokio::time::timeout(SOCKET_TIMEOUT, expect_strict_hello(&mut socket))
            .await
            .unwrap_or_else(|_| panic!("live upgrade {attempt} did not receive hello"));
        sockets.push(socket);
    }

    let mut request = server.ws_url.clone().into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", server.owner_token).parse().unwrap(),
    );
    let error = tokio::time::timeout(SOCKET_TIMEOUT, connect_async(request))
        .await
        .expect("rate-limited live upgrade did not complete")
        .unwrap_err();
    let tungstenite::Error::Http(response) = error else {
        panic!("expected upgrade admission rejection");
    };
    assert_eq!(response.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);

    drop(sockets);
    tokio::time::timeout(SHUTDOWN_TIMEOUT, server.shutdown())
        .await
        .expect("live test server did not shut down");
}

#[tokio::test]
async fn delivered_live_signal_does_not_change_authenticated_snapshot() {
    let backend = FakeLiveBackend::immediate();
    let server = LiveTestServer::start(backend.clone(), true).await;
    let before = server.snapshot().await;
    assert_eq!(before["snapshot_version"], 1);
    assert_eq!(before["vault_id"], serde_json::json!(server.vault));
    assert_eq!(before["record_count"], "0");
    assert_eq!(before["high_water_cursor"], "0");

    let mut sender = server.connect().await;
    expect_strict_hello(&mut sender).await;
    let mut peer = server.connect_peer().await;
    expect_strict_hello(&mut peer).await;
    for (kind, ciphertext) in [
        ("typing", "AQ=="),
        ("challenge", "Ag=="),
        ("session_ack", "Aw=="),
    ] {
        let frame = signal_with_kind_and_ciphertext(server.owner_device, kind, ciphertext);
        sender
            .send(tungstenite::Message::Text(frame.clone().into()))
            .await
            .unwrap();
        let received = next_text(&mut peer).await;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&received).unwrap(),
            serde_json::from_str::<serde_json::Value>(&frame).unwrap()
        );
    }
    wait_for_count(&backend.completed, 3).await;

    let after = server.snapshot().await;
    // Snapshot start has no clock, connection, or request fields; every field is
    // durable vault state or a protocol capability, so exact equality is stable.
    assert_eq!(after, before);
    server.shutdown().await;
}

#[tokio::test]
async fn live_publish_receive_invalid_and_error_paths_do_not_log_sensitive_data() {
    const SUCCESS_CIPHERTEXT: &str = "U1VDQ0VTU19QQVRIX0NBTkFSWQ==";
    const SUCCESS_PLAINTEXT: &str = "SUCCESS_PATH_CANARY";
    const CHALLENGE_CIPHERTEXT: &str = "Q0hBTExFTkdFX1BBVEhfQ0FOQVJZ";
    const CHALLENGE_PLAINTEXT: &str = "CHALLENGE_PATH_CANARY";
    const ACK_CIPHERTEXT: &str = "U0VTU0lPTl9BQ0tfUEFUSF9DQU5BUlk=";
    const ACK_PLAINTEXT: &str = "SESSION_ACK_PATH_CANARY";
    const INVALID_CIPHERTEXT: &str = "SU5WQUxJRF9QQVRIX0NBTkFSWQ==";
    const INVALID_PLAINTEXT: &str = "INVALID_PATH_CANARY";
    const ERROR_CIPHERTEXT: &str = "RVJST1JfUEFUSF9DQU5BUlk=";
    const ERROR_PLAINTEXT: &str = "ERROR_PATH_CANARY";
    const SAFE_SPAN_FIELD: &str = "safe_span_field_collector_control";

    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .without_time()
        .with_span_events(
            tracing_subscriber::fmt::format::FmtSpan::NEW
                | tracing_subscriber::fmt::format::FmtSpan::CLOSE,
        )
        .with_writer(logs.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("live tracing test must install the process collector");
    {
        let span =
            tracing::info_span!("live_trace_span_control", safe_span_field = SAFE_SPAN_FIELD);
        let _entered = span.enter();
        // Intentionally no event: this proves span-only fields reach the collector.
    }

    let backend = FakeLiveBackend::immediate();
    let server = LiveTestServer::start(backend.clone(), true).await;
    let mut sender = server.connect().await;
    expect_strict_hello(&mut sender).await;
    let mut peer = server.connect_peer().await;
    expect_strict_hello(&mut peer).await;

    let success_frames = [
        signal_with_kind_and_ciphertext(server.owner_device, "typing", SUCCESS_CIPHERTEXT),
        signal_with_kind_and_ciphertext(server.owner_device, "challenge", CHALLENGE_CIPHERTEXT),
        signal_with_kind_and_ciphertext(server.owner_device, "session_ack", ACK_CIPHERTEXT),
    ];
    for success_frame in &success_frames {
        sender
            .send(tungstenite::Message::Text(success_frame.clone().into()))
            .await
            .unwrap();
        let received = next_text(&mut peer).await;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&received).unwrap(),
            serde_json::from_str::<serde_json::Value>(success_frame).unwrap()
        );
    }
    wait_for_count(&backend.completed, 3).await;

    let mismatched_sender = Uuid::new_v4();
    let invalid_frame = signal_with_ciphertext(mismatched_sender, INVALID_CIPHERTEXT);
    sender
        .send(tungstenite::Message::Text(invalid_frame.clone().into()))
        .await
        .unwrap();
    let invalid_close = next_close(&mut sender).await;
    assert_eq!(invalid_close.code, CloseCode::Policy);
    assert_eq!(invalid_close.reason, "invalid_live_frame");
    assert_eq!(backend.started.load(Ordering::SeqCst), 3);

    backend.fail.store(true, Ordering::SeqCst);
    let error_frame = signal_with_ciphertext(server.peer_device, ERROR_CIPHERTEXT);
    peer.send(tungstenite::Message::Text(error_frame.clone().into()))
        .await
        .unwrap();
    let error_close = next_close(&mut peer).await;
    assert_eq!(error_close.code, CloseCode::Error);
    assert_eq!(error_close.reason, "live_unavailable");
    assert_eq!(backend.started.load(Ordering::SeqCst), 4);
    assert_eq!(backend.completed.load(Ordering::SeqCst), 3);

    let captured = logs.text();
    assert!(captured.contains(TRACE_COLLECTOR_SENTINEL));
    assert!(captured.contains(SAFE_SPAN_FIELD));
    let vault_id = server.vault.to_string();
    let owner_device_id = server.owner_device.to_string();
    let peer_device_id = server.peer_device.to_string();
    let mismatched_sender_id = mismatched_sender.to_string();
    for secret in [
        SUCCESS_CIPHERTEXT,
        SUCCESS_PLAINTEXT,
        CHALLENGE_CIPHERTEXT,
        CHALLENGE_PLAINTEXT,
        ACK_CIPHERTEXT,
        ACK_PLAINTEXT,
        success_frames[0].as_str(),
        success_frames[1].as_str(),
        success_frames[2].as_str(),
        INVALID_CIPHERTEXT,
        INVALID_PLAINTEXT,
        invalid_frame.as_str(),
        ERROR_CIPHERTEXT,
        ERROR_PLAINTEXT,
        error_frame.as_str(),
        vault_id.as_str(),
        owner_device_id.as_str(),
        peer_device_id.as_str(),
        mismatched_sender_id.as_str(),
    ] {
        assert!(
            !captured.contains(secret),
            "sensitive live data reached tracing output"
        );
    }
    server.shutdown().await;
}

#[tokio::test]
async fn invalid_utf8_application_frame_closes_with_fixed_policy_reason() {
    use tokio::io::AsyncWriteExt;

    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let mut socket = server.connect().await;
    expect_strict_hello(&mut socket).await;
    // A masked, final text frame containing invalid UTF-8. Writing the wire frame
    // directly is necessary because tungstenite refuses to construct invalid text.
    let mask = [1_u8, 2, 3, 4];
    let invalid = [0xff_u8, 0xfe];
    let wire = [
        0x81,
        0x80 | invalid.len() as u8,
        mask[0],
        mask[1],
        mask[2],
        mask[3],
        invalid[0] ^ mask[0],
        invalid[1] ^ mask[1],
    ];
    match socket.get_mut() {
        tokio_tungstenite::MaybeTlsStream::Plain(stream) => stream.write_all(&wire).await.unwrap(),
        _ => panic!("test live socket unexpectedly used TLS"),
    }
    let close = next_close(&mut socket).await;
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "invalid_live_frame");
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_issue_hashes_ticket_and_delete_terminalizes_session() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([7_u8; 16]);
    let client = reqwest::Client::new();
    let issued = client
        .post(format!("{}/v1/live/tickets", server.base_url))
        .bearer_auth(&server.owner_token)
        .json(&serde_json::json!({"session_id": session_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(issued.status(), reqwest::StatusCode::CREATED);
    let body: serde_json::Value = issued.json().await.unwrap();
    let ticket = body["ticket"].as_str().unwrap();
    let handle = body["lifetime_handle"].as_str().unwrap().to_owned();
    let ticket_digest: Vec<u8> = sqlx::query_scalar("SELECT ticket_digest FROM live_tickets")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(
        ticket_digest,
        Sha256::digest(URL_SAFE_NO_PAD.decode(ticket).unwrap()).to_vec()
    );

    let closed = live_ticket_body_request(
        &server,
        reqwest::Method::DELETE,
        serde_json::json!({"session_id": session_id, "lifetime_handle": handle}),
    )
    .await;
    assert_eq!(closed.status(), reqwest::StatusCode::NO_CONTENT);
    let rejected = live_ticket_request(&server, reqwest::Method::POST, session_id).await;
    assert_lifetime_refused(rejected, "initial POST after close").await;
    server.shutdown().await;
}

/// Step timeout for lifecycle tests: every await in those tests is bounded.
const LIFECYCLE_STEP_TIMEOUT: Duration = Duration::from_secs(10);
const LIFETIME_REFUSED: &str = "live_ticket_lifetime_refused";

async fn bounded<T>(step: impl Future<Output = T>) -> T {
    tokio::time::timeout(LIFECYCLE_STEP_TIMEOUT, step)
        .await
        .expect("live ticket lifecycle step exceeded its test timeout")
}

async fn live_ticket_body_request(
    server: &LiveTestServer,
    method: reqwest::Method,
    body: serde_json::Value,
) -> reqwest::Response {
    live_ticket_body_request_with_token(server, &server.owner_token, method, body).await
}

async fn live_ticket_body_request_with_token(
    server: &LiveTestServer,
    token: &str,
    method: reqwest::Method,
    body: serde_json::Value,
) -> reqwest::Response {
    bounded(
        reqwest::Client::new()
            .request(method, format!("{}/v1/live/tickets", server.base_url))
            .bearer_auth(token)
            .json(&body)
            .send(),
    )
    .await
    .unwrap()
}

async fn live_ticket_request(
    server: &LiveTestServer,
    method: reqwest::Method,
    session_id: String,
) -> reqwest::Response {
    live_ticket_body_request(
        server,
        method,
        serde_json::json!({"session_id": session_id}),
    )
    .await
}

async fn assert_ticket_http_lock_budget(method: reqwest::Method) {
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks.http_operation_budget = Some(Duration::from_millis(250));
    let server = LiveTestServer::start_with_options(options).await;
    let session = URL_SAFE_NO_PAD.encode([112_u8; 16]);
    let body = if method == reqwest::Method::DELETE {
        let (_, handle) = issue_ticket_with_handle(&server, session.clone()).await;
        serde_json::json!({"session_id": session, "lifetime_handle": handle})
    } else {
        serde_json::json!({"session_id": session})
    };
    let before = live_ticket_device_snapshot(&server).await;
    let mut blocker = server.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM devices WHERE vault_id=$1 AND device_id=$2 FOR UPDATE")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&mut *blocker)
        .await
        .unwrap();
    let status = {
        let request = live_ticket_body_request(&server, method, body);
        tokio::pin!(request);
        let response = tokio::time::timeout(Duration::from_secs(2), &mut request).await;
        blocker.rollback().await.unwrap();
        match response {
            Ok(response) => Some(response.status()),
            Err(_) => {
                bounded(request).await;
                None
            }
        }
    };
    let after = live_ticket_device_snapshot(&server).await;
    server.shutdown().await;
    assert_eq!(status, Some(reqwest::StatusCode::SERVICE_UNAVAILABLE));
    assert_eq!(after, before, "expired HTTP work changed durable authority");
}

#[tokio::test]
async fn ticket_http_issue_lock_wait_uses_entry_budget() {
    assert_ticket_http_lock_budget(reqwest::Method::POST).await;
}

#[tokio::test]
async fn ticket_http_close_lock_wait_uses_entry_budget() {
    assert_ticket_http_lock_budget(reqwest::Method::DELETE).await;
}

#[tokio::test]
async fn ticket_admission_events_survive_ticket_reaping_and_bound_reconnects() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session = lifecycle_session(113);
    let (_, handle) = issue_ticket_with_handle(&server, session.clone()).await;
    for _ in 1..6 {
        sqlx::query("DELETE FROM live_tickets WHERE vault_id=$1 AND device_id=$2")
            .bind(server.vault)
            .bind(server.owner_device)
            .execute(&server.pool)
            .await
            .unwrap();
        reconnect_ticket(&server, &session, &handle).await;
    }
    sqlx::query("DELETE FROM live_tickets WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();
    let before = live_ticket_device_snapshot(&server).await;
    let refused = live_ticket_body_request(
        &server,
        reqwest::Method::POST,
        lifetime_body(&session, &handle),
    )
    .await;
    assert_eq!(refused.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(live_ticket_device_snapshot(&server).await, before);
    let counts: (i64, i64) = sqlx::query_as("SELECT count(*) FILTER (WHERE event_kind='ticket_issue'),count(*) FILTER (WHERE event_kind='session_create') FROM live_ticket_admission_events WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault).bind(server.owner_device).fetch_one(&server.pool).await.unwrap();
    assert_eq!(counts, (6, 1));
    server.shutdown().await;
}

#[tokio::test]
async fn ticket_session_create_budget_survives_session_reaping_and_is_independent() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session = lifecycle_session(114);
    let (_, handle) = issue_ticket_with_handle(&server, session.clone()).await;
    sqlx::query("INSERT INTO live_ticket_admission_events(event_id,vault_id,device_id,event_kind,created_at) SELECT gen_random_uuid(),$1,$2,'session_create',clock_timestamp() FROM generate_series(1,6)")
        .bind(server.vault).bind(server.owner_device).execute(&server.pool).await.unwrap();
    reconnect_ticket(&server, &session, &handle).await;
    live_ticket_body_request(
        &server,
        reqwest::Method::DELETE,
        lifetime_body(&session, &handle),
    )
    .await
    .error_for_status()
    .unwrap();
    sqlx::query("DELETE FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();
    let refused = live_ticket_request(&server, reqwest::Method::POST, lifecycle_session(115)).await;
    assert_eq!(refused.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    sqlx::query("UPDATE live_ticket_admission_events SET created_at=clock_timestamp()-interval '61 seconds' WHERE vault_id=$1 AND device_id=$2 AND event_kind='session_create'")
        .bind(server.vault).bind(server.owner_device).execute(&server.pool).await.unwrap();
    assert_eq!(
        live_ticket_request(&server, reqwest::Method::POST, lifecycle_session(115))
            .await
            .status(),
        reqwest::StatusCode::CREATED
    );
    server.shutdown().await;
}

#[tokio::test]
async fn ticket_retention_reserves_terminal_slot_and_never_blocks_exact_close() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    for value in 1_u8..=31 {
        sqlx::query("INSERT INTO live_ticket_sessions(vault_id,device_id,session_id_hash,session_incarnation,state,created_at,closed_at) VALUES($1,$2,$3,$4,'terminal',clock_timestamp()-interval '61 seconds',clock_timestamp())")
            .bind(server.vault).bind(server.owner_device).bind(Sha256::digest([value; 16]).to_vec()).bind(i64::from(value))
            .execute(&server.pool).await.unwrap();
    }
    let session = lifecycle_session(116);
    let (_, handle) = issue_ticket_with_handle(&server, session.clone()).await;
    let before = live_ticket_device_snapshot(&server).await;
    assert_eq!(
        live_ticket_request(&server, reqwest::Method::POST, lifecycle_session(117))
            .await
            .status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        live_ticket_device_snapshot(&server).await,
        before,
        "cap refusal terminalized/replaced existing reservation"
    );
    assert_eq!(
        live_ticket_body_request(
            &server,
            reqwest::Method::DELETE,
            lifetime_body(&session, &handle)
        )
        .await
        .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let retained: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(retained, 32);
    assert_eq!(
        live_ticket_request(&server, reqwest::Method::POST, lifecycle_session(117))
            .await
            .status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    server.shutdown().await;
}

#[tokio::test]
async fn ticket_http_attempt_limiter_is_separate_and_ignores_untrusted_forwarded_peer() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let body = lifetime_body(
        &lifecycle_session(118),
        &encode_live_ticket_lifetime_handle(99),
    );
    for _ in 0..30 {
        assert_lifetime_refused(
            live_ticket_body_request(&server, reqwest::Method::DELETE, body.clone()).await,
            "attempt inside local budget",
        )
        .await;
    }
    assert_eq!(
        live_ticket_body_request(&server, reqwest::Method::DELETE, body.clone())
            .await
            .status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    let spoofed = reqwest::Client::new()
        .delete(format!("{}/v1/live/tickets", server.base_url))
        .bearer_auth(&server.peer_token)
        .header("x-forwarded-for", "192.0.2.118")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(spoofed.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    let mut bearer = server.connect().await;
    expect_strict_hello(&mut bearer).await;
    server.shutdown().await;
}

async fn issue_ticket(server: &LiveTestServer, session_id: String) -> String {
    issue_ticket_with_handle(server, session_id).await.0
}

/// Returns `(ticket, lifetime_handle)` from an initial reservation.
async fn issue_ticket_with_handle(server: &LiveTestServer, session_id: String) -> (String, String) {
    let response = live_ticket_request(server, reqwest::Method::POST, session_id)
        .await
        .error_for_status()
        .unwrap();
    let body = bounded(response.json::<serde_json::Value>()).await.unwrap();
    (
        body["ticket"].as_str().unwrap().to_owned(),
        body["lifetime_handle"].as_str().unwrap().to_owned(),
    )
}

/// Reconnects the exact lifetime named by `handle`, expecting a fresh ticket.
async fn reconnect_ticket(server: &LiveTestServer, session_id: &str, handle: &str) -> String {
    let response = live_ticket_body_request(
        server,
        reqwest::Method::POST,
        serde_json::json!({"session_id": session_id, "lifetime_handle": handle}),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let body = bounded(response.json::<serde_json::Value>()).await.unwrap();
    assert_eq!(body["lifetime_handle"], handle);
    body["ticket"].as_str().unwrap().to_owned()
}

async fn assert_lifetime_refused(response: reqwest::Response, case: &str) {
    assert_eq!(
        response.status(),
        reqwest::StatusCode::CONFLICT,
        "{case} was not refused"
    );
    let body = bounded(response.json::<serde_json::Value>()).await.unwrap();
    assert_eq!(body["code"], LIFETIME_REFUSED, "{case} code");
}

/// Every live-ticket row for the owner device, serialized for exact comparison.
async fn live_ticket_device_snapshot(server: &LiveTestServer) -> String {
    bounded(
        sqlx::query_scalar::<_, String>(
            "SELECT json_build_object(
                'sessions', (SELECT coalesce(json_agg(s ORDER BY s.session_id_hash), '[]'::json) FROM live_ticket_sessions s WHERE s.vault_id=$1 AND s.device_id=$2),
                'tickets', (SELECT coalesce(json_agg(t ORDER BY t.ticket_digest), '[]'::json) FROM live_tickets t WHERE t.vault_id=$1 AND t.device_id=$2),
                'authority', (SELECT coalesce(json_agg(a), '[]'::json) FROM live_ticket_device_authority a WHERE a.vault_id=$1 AND a.device_id=$2)
            )::text",
        )
        .bind(server.vault)
        .bind(server.owner_device)
        .fetch_one(&server.pool),
    )
    .await
    .unwrap()
}

async fn live_ticket_session_row(server: &LiveTestServer, session_id: &str) -> Option<String> {
    let hash = Sha256::digest(URL_SAFE_NO_PAD.decode(session_id).unwrap()).to_vec();
    bounded(
        sqlx::query_scalar::<_, String>(
            "SELECT row_to_json(s)::text FROM live_ticket_sessions s WHERE s.vault_id=$1 AND s.device_id=$2 AND s.session_id_hash=$3",
        )
        .bind(server.vault)
        .bind(server.owner_device)
        .bind(hash)
        .fetch_optional(&server.pool),
    )
    .await
    .unwrap()
}

async fn live_ticket_session_state(server: &LiveTestServer, session_id: &str) -> Option<String> {
    let hash = Sha256::digest(URL_SAFE_NO_PAD.decode(session_id).unwrap()).to_vec();
    bounded(
        sqlx::query_scalar::<_, String>(
            "SELECT state FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3",
        )
        .bind(server.vault)
        .bind(server.owner_device)
        .bind(hash)
        .fetch_optional(&server.pool),
    )
    .await
    .unwrap()
}

async fn live_ticket_unconsumed_count(server: &LiveTestServer, session_id: &str) -> i64 {
    let hash = Sha256::digest(URL_SAFE_NO_PAD.decode(session_id).unwrap()).to_vec();
    bounded(
        sqlx::query_scalar(
            "SELECT count(*) FROM live_tickets WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3 AND consumed_at IS NULL",
        )
        .bind(server.vault)
        .bind(server.owner_device)
        .bind(hash)
        .fetch_one(&server.pool),
    )
    .await
    .unwrap()
}

/// Invalid, absent, or foreign handles for reconnect and close.
fn invalid_lifetime_bodies(
    session_id: &str,
    handle: &str,
) -> Vec<(&'static str, serde_json::Value)> {
    let bytes = URL_SAFE_NO_PAD.decode(handle).unwrap();
    let mut long = bytes.clone();
    long.push(0);
    let mut non_canonical = handle.to_owned();
    non_canonical.pop();
    non_canonical.push('B');
    let body = |value: serde_json::Value| serde_json::json!({"session_id": session_id, "lifetime_handle": value});
    vec![
        ("missing", serde_json::json!({"session_id": session_id})),
        ("null", body(serde_json::Value::Null)),
        ("boolean", body(true.into())),
        ("number", body(1.into())),
        ("array", body(serde_json::json!([handle]))),
        ("object", body(serde_json::json!({"handle": handle}))),
        ("empty", body("".into())),
        ("malformed", body("not a handle".into())),
        ("padded", body(format!("{handle}=").into())),
        (
            "standard_alphabet",
            body(format!("+{}", &handle[1..]).into()),
        ),
        ("short", body(URL_SAFE_NO_PAD.encode(&bytes[..7]).into())),
        ("long", body(URL_SAFE_NO_PAD.encode(&long).into())),
        ("non_canonical", body(non_canonical.into())),
        ("zero", body(URL_SAFE_NO_PAD.encode([0_u8; 8]).into())),
        (
            "out_of_range",
            body(URL_SAFE_NO_PAD.encode([0xff_u8; 8]).into()),
        ),
        (
            "mismatched",
            body(URL_SAFE_NO_PAD.encode(999_999_i64.to_be_bytes()).into()),
        ),
    ]
}

/// All mutable rows whose refusal behavior is relevant to this vault, including presence.
async fn live_ticket_refusal_snapshot(server: &LiveTestServer) -> String {
    bounded(
        sqlx::query_scalar::<_, String>(
            "SELECT json_build_object(
                'sessions', (SELECT coalesce(json_agg(s ORDER BY s.device_id, s.session_id_hash), '[]'::json) FROM live_ticket_sessions s WHERE s.vault_id=$1),
                'tickets', (SELECT coalesce(json_agg(t ORDER BY t.device_id, t.ticket_digest), '[]'::json) FROM live_tickets t WHERE t.vault_id=$1),
                'authority', (SELECT coalesce(json_agg(a ORDER BY a.device_id), '[]'::json) FROM live_ticket_device_authority a WHERE a.vault_id=$1),
                'presence', (SELECT coalesce(json_agg(p ORDER BY p.device_id), '[]'::json) FROM device_presence p WHERE p.vault_id=$1)
            )::text",
        )
        .bind(server.vault)
        .fetch_one(&server.pool),
    )
    .await
    .unwrap()
}

async fn live_ticket_write_counts(server: &LiveTestServer) -> LiveTicketWriteCounts {
    LiveTicketWriteCounts {
        presence: sqlx::query_scalar(
            "SELECT count(*) FROM device_presence WHERE vault_id=$1 AND device_id=$2",
        )
        .bind(server.vault)
        .bind(server.owner_device)
        .fetch_one(&server.pool)
        .await
        .unwrap(),
        tickets: sqlx::query_scalar(
            "SELECT count(*) FROM live_tickets WHERE vault_id=$1 AND device_id=$2",
        )
        .bind(server.vault)
        .bind(server.owner_device)
        .fetch_one(&server.pool)
        .await
        .unwrap(),
        sessions: sqlx::query_scalar(
            "SELECT count(*) FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2",
        )
        .bind(server.vault)
        .bind(server.owner_device)
        .fetch_one(&server.pool)
        .await
        .unwrap(),
        admissions: sqlx::query_scalar(
            "SELECT count(*) FROM device_admissions WHERE vault_id=$1 AND device_id=$2",
        )
        .bind(server.vault)
        .bind(server.owner_device)
        .fetch_one(&server.pool)
        .await
        .unwrap(),
    }
}

fn live_ticket_protocol(session_id: &str, ticket: &str) -> String {
    format!("peppy-live-v2.{session_id}.{ticket}")
}

fn spawn_ticket_connect(
    server: &LiveTestServer,
    session_id: String,
    ticket: String,
) -> JoinHandle<
    Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tungstenite::Error,
    >,
> {
    let ws_url = server.ws_url.clone();
    tokio::spawn(async move {
        let protocol = live_ticket_protocol(&session_id, &ticket);
        let mut request = ws_url.into_client_request().unwrap();
        request
            .headers_mut()
            .insert("origin", "https://web.peppy.test".parse().unwrap());
        request
            .headers_mut()
            .insert("sec-websocket-protocol", protocol.parse().unwrap());
        connect_async(request).await.map(|(socket, _)| socket)
    })
}

#[tokio::test]
async fn changed_epoch_revalidates_sql_once_then_installs() {
    let validation_gate = LiveTicketTestGate::new(1);
    let hooks = LiveTicketTestHooks {
        after_validation: Some(validation_gate.clone()),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks = hooks.clone();
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([73_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let connect = spawn_ticket_connect(&server, session_id, ticket);

    bounded(validation_gate.wait_for_entries(1)).await;
    let bearer = server
        .live_hub
        .register(VaultId(server.vault), DeviceId(server.owner_device));
    server.live_hub.unregister(
        VaultId(server.vault),
        DeviceId(server.owner_device),
        bearer.generation,
    );
    validation_gate.release_one();

    let mut socket = bounded(connect).await.unwrap().unwrap();
    bounded(expect_strict_hello(&mut socket)).await;
    assert_eq!(
        validation_gate.entries(),
        2,
        "ChangedEpoch must revalidate once"
    );
    assert_eq!(
        hooks.credential_query_started.load(Ordering::SeqCst),
        2,
        "retry must perform a second durable SQL check"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn c2_install_and_remove_before_c1_sql_completion_forces_one_real_retry() {
    let device_gate = LiveTicketTestGate::new(1);
    let hooks = LiveTicketTestHooks {
        after_device_claim: Some(device_gate.clone()),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks = hooks.clone();
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([86_u8; 16]);
    let session_bytes: [u8; 16] = URL_SAFE_NO_PAD
        .decode(&session_id)
        .unwrap()
        .try_into()
        .unwrap();
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let connect = spawn_ticket_connect(&server, session_id, ticket);

    bounded(device_gate.wait_for_entries(1)).await;
    let durable: (i64, i64) = sqlx::query_as("SELECT session_incarnation,active_connection_id FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3")
        .bind(server.vault)
        .bind(server.owner_device)
        .bind(Sha256::digest(session_bytes).to_vec())
        .fetch_one(&server.pool)
        .await
        .unwrap();
    let c2 = crate::live::LiveTicketIdentity::new(session_bytes, durable.0, durable.1 + 1);
    let snapshot = server
        .live_hub
        .ticket_snapshot(VaultId(server.vault), DeviceId(server.owner_device));
    server
        .live_hub
        .register_ticket_if_current(
            VaultId(server.vault),
            DeviceId(server.owner_device),
            snapshot,
            c2,
        )
        .unwrap();
    server
        .live_hub
        .cancel_ticket_exact(VaultId(server.vault), DeviceId(server.owner_device), c2);
    device_gate.release_one();

    let mut socket = bounded(connect).await.unwrap().unwrap();
    bounded(expect_strict_hello(&mut socket)).await;
    assert_eq!(hooks.credential_query_started.load(Ordering::SeqCst), 2);
    server.shutdown().await;
}

#[tokio::test]
async fn second_changed_epoch_refuses_without_a_third_sql_check_or_hello() {
    let validation_gate = LiveTicketTestGate::new(2);
    let hooks = LiveTicketTestHooks {
        after_validation: Some(validation_gate.clone()),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks = hooks.clone();
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([74_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let connect = spawn_ticket_connect(&server, session_id, ticket);

    bounded(validation_gate.wait_for_entries(1)).await;
    let first = server
        .live_hub
        .register(VaultId(server.vault), DeviceId(server.owner_device));
    server.live_hub.unregister(
        VaultId(server.vault),
        DeviceId(server.owner_device),
        first.generation,
    );
    validation_gate.release_one();

    bounded(validation_gate.wait_for_entries(2)).await;
    let second = server
        .live_hub
        .register(VaultId(server.vault), DeviceId(server.owner_device));
    server.live_hub.unregister(
        VaultId(server.vault),
        DeviceId(server.owner_device),
        second.generation,
    );
    validation_gate.release_one();

    let mut socket = bounded(connect).await.unwrap().unwrap();
    let first_frame = bounded(socket.next()).await.unwrap().unwrap();
    let tungstenite::Message::Close(Some(close)) = first_frame else {
        panic!("second ChangedEpoch emitted a frame before refusal: {first_frame:?}")
    };
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "live_unauthorized");
    assert_eq!(validation_gate.entries(), 2);
    assert_eq!(hooks.credential_query_started.load(Ordering::SeqCst), 2);
    server.shutdown().await;
}

#[tokio::test]
async fn changed_epoch_retry_uses_the_original_entry_budget() {
    let device_gate = LiveTicketTestGate::new(2);
    let validation_gate = LiveTicketTestGate::new(1);
    let hooks = LiveTicketTestHooks {
        operation_budget: Some(Duration::from_millis(250)),
        after_device_claim: Some(device_gate.clone()),
        after_validation: Some(validation_gate.clone()),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks = hooks.clone();
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([75_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let connect = spawn_ticket_connect(&server, session_id, ticket);

    bounded(device_gate.wait_for_entries(1)).await;
    device_gate.release_one();
    bounded(validation_gate.wait_for_entries(1)).await;
    let bearer = server
        .live_hub
        .register(VaultId(server.vault), DeviceId(server.owner_device));
    server.live_hub.unregister(
        VaultId(server.vault),
        DeviceId(server.owner_device),
        bearer.generation,
    );
    validation_gate.release_one();
    bounded(device_gate.wait_for_entries(2)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    device_gate.release_one();

    let mut socket = bounded(connect).await.unwrap().unwrap();
    let first_frame = bounded(socket.next()).await.unwrap().unwrap();
    let tungstenite::Message::Close(Some(close)) = first_frame else {
        panic!("expired entry budget emitted a frame: {first_frame:?}")
    };
    assert_eq!(close.code, CloseCode::Error);
    assert_eq!(close.reason, "live_unavailable");
    assert_eq!(device_gate.entries(), 2);
    assert_eq!(hooks.credential_query_started.load(Ordering::SeqCst), 1);
    server.shutdown().await;
}

#[tokio::test]
async fn policy_and_sql_share_one_entry_budget() {
    let policy_gate = LiveTicketTestGate::new(1);
    let policy = Arc::new(GatedPolicy::new(policy_gate.clone()));
    let sql_gate = LiveTicketTestGate::new(1);
    let hooks = LiveTicketTestHooks {
        operation_budget: Some(Duration::from_millis(250)),
        after_device_claim: Some(sql_gate.clone()),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.policy = policy.clone();
    options.ticket_hooks = hooks.clone();
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([76_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    policy.enable();
    let connect = spawn_ticket_connect(&server, session_id, ticket);

    bounded(policy_gate.wait_for_entries(1)).await;
    tokio::time::sleep(Duration::from_millis(140)).await;
    policy_gate.release_one();
    bounded(sql_gate.wait_for_entries(1)).await;
    tokio::time::sleep(Duration::from_millis(140)).await;
    sql_gate.release_one();

    let mut socket = bounded(connect).await.unwrap().unwrap();
    let frame = first_socket_message(&mut socket).await.unwrap().unwrap();
    let tungstenite::Message::Close(Some(close)) = frame else {
        panic!("combined budget emitted a frame before refusal: {frame:?}")
    };
    assert_eq!(close.code, CloseCode::Error);
    assert_eq!(close.reason, "live_unavailable");
    assert_eq!(
        sql_gate.entries(),
        1,
        "SQL stage was never actually awaited"
    );
    assert_eq!(hooks.credential_query_started.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn readiness_interrupts_blocked_preregistration_policy_without_sql_or_hello() {
    let policy_gate = LiveTicketTestGate::new(1);
    let policy = Arc::new(GatedPolicy::new(policy_gate.clone()));
    let hooks = LiveTicketTestHooks {
        operation_budget: Some(Duration::from_secs(2)),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.policy = policy.clone();
    options.ticket_hooks = hooks.clone();
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([77_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    policy.enable();
    let connect = spawn_ticket_connect(&server, session_id, ticket);

    bounded(policy_gate.wait_for_entries(1)).await;
    let mut socket = bounded(connect).await.unwrap().unwrap();
    server.readiness_sender.send(false).unwrap();
    let frame = first_socket_message(&mut socket).await.unwrap().unwrap();
    let tungstenite::Message::Close(Some(close)) = frame else {
        panic!("readiness loss emitted a frame before refusal: {frame:?}")
    };
    assert_eq!(close.code, CloseCode::Error);
    assert_eq!(close.reason, "live_unavailable");
    assert_eq!(hooks.credential_query_started.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn shutdown_interrupts_blocked_preregistration_policy_without_sql_or_hello() {
    let policy_gate = LiveTicketTestGate::new(1);
    let policy = Arc::new(GatedPolicy::new(policy_gate.clone()));
    let hooks = LiveTicketTestHooks {
        operation_budget: Some(Duration::from_secs(2)),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.policy = policy.clone();
    options.ticket_hooks = hooks.clone();
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([78_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    policy.enable();
    let connect = spawn_ticket_connect(&server, session_id, ticket);

    bounded(policy_gate.wait_for_entries(1)).await;
    let mut socket = bounded(connect).await.unwrap().unwrap();
    server.begin_shutdown();
    match first_socket_message(&mut socket).await {
        None | Some(Err(_)) | Some(Ok(tungstenite::Message::Close(_))) => {}
        Some(Ok(frame)) => panic!("shutdown emitted a live frame: {frame:?}"),
    }
    assert_eq!(hooks.credential_query_started.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn readiness_interrupts_blocked_preregistration_sql_before_the_next_statement() {
    let sql_gate = LiveTicketTestGate::new(1);
    let hooks = LiveTicketTestHooks {
        operation_budget: Some(Duration::from_secs(2)),
        after_device_claim: Some(sql_gate.clone()),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks = hooks.clone();
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([79_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let connect = spawn_ticket_connect(&server, session_id, ticket);

    bounded(sql_gate.wait_for_entries(1)).await;
    let mut socket = bounded(connect).await.unwrap().unwrap();
    server.readiness_sender.send(false).unwrap();
    bounded(hooks.work_probe.wait_for_cancellation(0)).await;
    sql_gate.release_one();
    bounded(hooks.work_probe.wait_for_disposals(1)).await;
    let frame = first_socket_message(&mut socket).await.unwrap().unwrap();
    let tungstenite::Message::Close(Some(close)) = frame else {
        panic!("readiness loss emitted a frame before refusal: {frame:?}")
    };
    assert_eq!(close.code, CloseCode::Error);
    assert_eq!(close.reason, "live_unavailable");
    assert_eq!(hooks.credential_query_started.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn shutdown_interrupts_blocked_preregistration_sql_before_the_next_statement() {
    let sql_gate = LiveTicketTestGate::new(1);
    let hooks = LiveTicketTestHooks {
        operation_budget: Some(Duration::from_secs(2)),
        after_device_claim: Some(sql_gate.clone()),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks = hooks.clone();
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([80_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let connect = spawn_ticket_connect(&server, session_id, ticket);

    bounded(sql_gate.wait_for_entries(1)).await;
    let mut socket = bounded(connect).await.unwrap().unwrap();
    server.begin_shutdown();
    bounded(hooks.work_probe.wait_for_cancellation(0)).await;
    sql_gate.release_one();
    bounded(hooks.work_probe.wait_for_disposals(1)).await;
    match first_socket_message(&mut socket).await {
        None | Some(Err(_)) | Some(Ok(tungstenite::Message::Close(_))) => {}
        Some(Ok(frame)) => panic!("shutdown emitted a live frame: {frame:?}"),
    }
    assert_eq!(hooks.credential_query_started.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn lease_expiring_during_real_device_lock_wait_is_rejected_after_the_lock() {
    let hooks = LiveTicketTestHooks::default();
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks = hooks.clone();
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([81_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let session_hash = Sha256::digest(URL_SAFE_NO_PAD.decode(&session_id).unwrap()).to_vec();
    let (mut socket, _) = server.connect_with_ticket(&session_id, &ticket).await;
    expect_strict_hello(&mut socket).await;
    let baseline = hooks.credential_query_started.load(Ordering::SeqCst);

    sqlx::query("UPDATE live_ticket_sessions SET lease_until=clock_timestamp()+interval '250 milliseconds' WHERE session_id_hash=$1")
        .bind(&session_hash)
        .execute(&server.pool)
        .await
        .unwrap();
    let lease_before: String = sqlx::query_scalar(
        "SELECT lease_until::text FROM live_ticket_sessions WHERE session_id_hash=$1",
    )
    .bind(&session_hash)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    let mut lock = server.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM devices WHERE vault_id=$1 AND device_id=$2 FOR UPDATE")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&mut *lock)
        .await
        .unwrap();

    wait_for_atomic_count(&hooks.credential_query_started, baseline + 1).await;
    sqlx::query("SELECT pg_sleep(0.35)")
        .execute(&server.admin)
        .await
        .unwrap();
    let crossed: bool = sqlx::query_scalar("SELECT clock_timestamp() >= lease_until FROM live_ticket_sessions WHERE session_id_hash=$1")
        .bind(&session_hash)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert!(
        crossed,
        "database clock must cross the lease while the row lock is held"
    );
    lock.commit().await.unwrap();

    let close = next_close(&mut socket).await;
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "live_unauthorized");
    let lease_after: String = sqlx::query_scalar(
        "SELECT lease_until::text FROM live_ticket_sessions WHERE session_id_hash=$1",
    )
    .bind(session_hash)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        lease_after, lease_before,
        "expired lock waiter renewed the lease"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn commit_forwarded_after_scaled_lease_expiry_is_inert_and_rejected() {
    assert_eq!(LIVE_TICKET_OPERATION_BUDGET, Duration::from_secs(5));
    assert_eq!(LIVE_TICKET_LEASE_DURATION, Duration::from_secs(90));

    let database_url = std::env::var("TEST_DATABASE_URL").unwrap();
    let proxy = DelayedCommitProxy::start(&database_url).await;
    let lease_duration = Duration::from_millis(700);
    let hooks = LiveTicketTestHooks {
        recheck_operation_budget: Some(Duration::from_millis(250)),
        recheck_interval: Some(Duration::from_millis(100)),
        lease_duration: Some(lease_duration),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks = hooks.clone();
    options.database_proxy_url = Some(proxy.proxy_url.clone());
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([88_u8; 16]);
    let session_bytes: [u8; 16] = URL_SAFE_NO_PAD
        .decode(&session_id)
        .unwrap()
        .try_into()
        .unwrap();
    let session_hash = Sha256::digest(session_bytes).to_vec();
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let (mut socket, _) = server.connect_with_ticket(&session_id, &ticket).await;
    expect_strict_hello(&mut socket).await;
    let (incarnation, connection_id): (i64, i64) = sqlx::query_as(
        "SELECT session_incarnation,active_connection_id FROM live_ticket_sessions WHERE session_id_hash=$1",
    )
    .bind(&session_hash)
    .fetch_one(&server.pool)
    .await
    .unwrap();

    proxy.arm();
    bounded(proxy.wait_for_commit()).await;
    let sampled_at = hooks
        .sampled_at
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("renewal must capture its database sample before COMMIT");

    let close = tokio::time::timeout(Duration::from_secs(1), next_close(&mut socket))
        .await
        .expect("caller did not expire while the already-sent COMMIT was held");
    assert_eq!(close.code, CloseCode::Error);
    assert_eq!(close.reason, "live_unavailable");

    sqlx::query("SELECT pg_sleep(GREATEST(0::double precision, EXTRACT(EPOCH FROM ($1::timestamptz + ($2*interval '1 millisecond') - clock_timestamp())) + 0.05))")
        .bind(&sampled_at)
        .bind(i64::try_from(lease_duration.as_millis()).unwrap())
        .execute(&server.admin)
        .await
        .unwrap();
    proxy.release_commit();
    bounded(proxy.wait_for_acknowledgement()).await;

    let exact_and_expired: (bool, bool) = sqlx::query_as(&format!(
        "SELECT lease_until=$2::timestamptz+($3*interval '1 millisecond'),lease_until<=clock_timestamp() FROM {}.live_ticket_sessions WHERE session_id_hash=$1",
        server.schema
    ))
    .bind(&session_hash)
    .bind(&sampled_at)
    .bind(i64::try_from(lease_duration.as_millis()).unwrap())
    .fetch_one(&server.admin)
    .await
    .unwrap();
    assert!(
        exact_and_expired.0,
        "late COMMIT changed the sampled lease base"
    );
    assert!(
        exact_and_expired.1,
        "late COMMIT restored already-expired authority"
    );

    let role: String = sqlx::query_scalar(&format!(
        "SELECT role FROM {}.devices WHERE vault_id=$1 AND device_id=$2",
        server.schema
    ))
    .bind(server.vault)
    .bind(server.owner_device)
    .fetch_one(&server.admin)
    .await
    .unwrap();
    let principal = Principal {
        vault: server.vault,
        device: server.owner_device,
        role,
        token_digest: Sha256::digest(server.owner_token.as_bytes()).to_vec(),
    };
    let identity = crate::live::LiveTicketIdentity::new(session_bytes, incarnation, connection_id);
    let (_ready_sender, mut readiness) = watch::channel(true);
    let mut shutdown = None;
    assert!(matches!(
        live_ticket_revalidate(
            &server.state,
            &principal,
            identity,
            false,
            OperationDeadline::after(Duration::from_secs(1)),
            None,
            &mut readiness,
            &mut shutdown,
        )
        .await,
        LiveRevalidation::Unauthorized
    ));

    server.shutdown().await;
    proxy.shutdown();
}

#[tokio::test]
async fn delayed_sample_and_commit_store_exactly_sample_plus_ninety_seconds() {
    let sample_gate = LiveTicketTestGate::new(2);
    let commit_gate = LiveTicketTestGate::new(2);
    let committed_gate = LiveTicketTestGate::new(2);
    let hooks = LiveTicketTestHooks {
        operation_budget: Some(Duration::from_secs(2)),
        after_sample: Some(sample_gate.clone()),
        before_commit: Some(commit_gate.clone()),
        after_commit: Some(committed_gate.clone()),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks = hooks.clone();
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([82_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let session_hash = Sha256::digest(URL_SAFE_NO_PAD.decode(&session_id).unwrap()).to_vec();
    let connect = spawn_ticket_connect(&server, session_id, ticket);

    bounded(sample_gate.wait_for_entries(1)).await;
    sample_gate.release_one();
    bounded(commit_gate.wait_for_entries(1)).await;
    commit_gate.release_one();
    bounded(committed_gate.wait_for_entries(1)).await;
    committed_gate.release_one();
    let mut socket = bounded(connect).await.unwrap().unwrap();
    bounded(expect_strict_hello(&mut socket)).await;

    bounded(sample_gate.wait_for_entries(2)).await;
    let sampled_at = hooks
        .sampled_at
        .lock()
        .unwrap()
        .get(1)
        .cloned()
        .expect("renewal must record its database sample");
    tokio::time::sleep(Duration::from_millis(120)).await;
    sample_gate.release_one();
    bounded(commit_gate.wait_for_entries(2)).await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    commit_gate.release_one();
    bounded(committed_gate.wait_for_entries(2)).await;

    let exact: bool = sqlx::query_scalar("SELECT lease_until=$2::timestamptz+interval '90 seconds' FROM live_ticket_sessions WHERE session_id_hash=$1")
        .bind(session_hash)
        .bind(sampled_at)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert!(
        exact,
        "delayed commit recomputed rather than retaining sample + 90 seconds"
    );
    committed_gate.release_one();
    loop {
        match bounded(socket.next()).await.unwrap().unwrap() {
            tungstenite::Message::Ping(_) => break,
            tungstenite::Message::Close(frame) => {
                panic!("renewal closed before acknowledgement: {frame:?}")
            }
            _ => {}
        }
    }
    server.shutdown().await;
}

#[tokio::test]
async fn c1_waiting_on_c2s_real_device_lock_cannot_renew_the_c2_successor() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([83_u8; 16]);
    let (first_ticket, handle) = issue_ticket_with_handle(&server, session_id.clone()).await;
    let (mut stale_socket, _) = server.connect_with_ticket(&session_id, &first_ticket).await;
    expect_strict_hello(&mut stale_socket).await;
    let close_waiter = tokio::spawn(async move { next_close(&mut stale_socket).await });
    let successor_ticket = reconnect_ticket(&server, &session_id, &handle).await;
    let (session_bytes, successor_secret) = ticket_parts(&session_id, &successor_ticket);
    let session_hash = Sha256::digest(session_bytes).to_vec();

    let key_one = 18_301_i32;
    let key_two = i32::from_be_bytes(Uuid::new_v4().as_bytes()[..4].try_into().unwrap()) & i32::MAX;
    let mut advisory = server.pool.acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock($1,$2)")
        .bind(key_one)
        .bind(key_two)
        .execute(&mut *advisory)
        .await
        .unwrap();
    sqlx::query(&format!(
        "CREATE FUNCTION block_c2_live_ticket_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock({key_one},{key_two}); RETURN NEW; END $$"
    ))
    .execute(&server.pool)
    .await
    .unwrap();
    sqlx::query("CREATE TRIGGER block_c2_live_ticket_update BEFORE UPDATE OF active_connection_id ON live_ticket_sessions FOR EACH ROW EXECUTE FUNCTION block_c2_live_ticket_update()")
        .execute(&server.pool)
        .await
        .unwrap();

    let pool = server.pool.clone();
    let c2 =
        tokio::spawn(
            async move { consume_live_ticket(&pool, session_bytes, successor_secret).await },
        );
    let c2_pid: i32 = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let waiting: Option<i32> = sqlx::query_scalar("SELECT pid FROM pg_locks WHERE locktype='advisory' AND classid=$1::oid AND objid=$2::oid AND objsubid=2 AND NOT granted")
                .bind(key_one)
                .bind(key_two)
                .fetch_optional(&server.pool)
                .await
                .unwrap();
            if let Some(pid) = waiting {
                break pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("C2 did not reach the advisory trigger while holding its row locks");

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let c1_waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
            )
            .bind(c2_pid)
            .fetch_one(&server.pool)
            .await
            .unwrap();
            if c1_waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("C1 did not wait on C2's device lock");
    sqlx::query("SELECT pg_advisory_unlock($1,$2)")
        .bind(key_one)
        .bind(key_two)
        .execute(&mut *advisory)
        .await
        .unwrap();
    let successor = bounded(c2).await.unwrap().unwrap();
    let before: (i64, i64) = sqlx::query_as("SELECT active_connection_id,(extract(epoch FROM lease_until)*1000000)::bigint FROM live_ticket_sessions WHERE session_id_hash=$1")
        .bind(&session_hash)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(before.0, successor.connection_id);

    let close = bounded(close_waiter).await.unwrap();
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "live_unauthorized");
    let after: (i64, i64) = sqlx::query_as("SELECT active_connection_id,(extract(epoch FROM lease_until)*1000000)::bigint FROM live_ticket_sessions WHERE session_id_hash=$1")
        .bind(session_hash)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(
        after, before,
        "blocked C1 changed the committed C2 successor"
    );
    sqlx::query("DROP TRIGGER block_c2_live_ticket_update ON live_ticket_sessions")
        .execute(&server.pool)
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION block_c2_live_ticket_update()")
        .execute(&server.pool)
        .await
        .unwrap();
    server.shutdown().await;
}

#[tokio::test]
async fn shutdown_interrupts_an_active_check_before_its_next_sql_statement() {
    let sql_gate = LiveTicketTestGate::new(2);
    let hooks = LiveTicketTestHooks {
        operation_budget: Some(Duration::from_secs(2)),
        after_device_claim: Some(sql_gate.clone()),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks = hooks.clone();
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([84_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let connect = spawn_ticket_connect(&server, session_id, ticket);
    bounded(sql_gate.wait_for_entries(1)).await;
    sql_gate.release_one();
    let mut socket = bounded(connect).await.unwrap().unwrap();
    bounded(expect_strict_hello(&mut socket)).await;
    let statements_before = hooks.credential_query_started.load(Ordering::SeqCst);

    bounded(sql_gate.wait_for_entries(2)).await;
    server.begin_shutdown();
    bounded(hooks.work_probe.wait_for_cancellation(1)).await;
    sql_gate.release_one();
    bounded(hooks.work_probe.wait_for_disposals(2)).await;
    match first_socket_message(&mut socket).await {
        None | Some(Err(_)) | Some(Ok(tungstenite::Message::Close(_))) => {}
        Some(Ok(frame)) => panic!("shutdown emitted an active frame: {frame:?}"),
    }
    assert_eq!(
        hooks.credential_query_started.load(Ordering::SeqCst),
        statements_before,
        "shutdown allowed the blocked check to start its next statement"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn takeover_cancellation_interrupts_an_active_check_before_its_next_sql_statement() {
    let sql_gate = LiveTicketTestGate::new(2);
    let hooks = LiveTicketTestHooks {
        operation_budget: Some(Duration::from_secs(2)),
        after_device_claim: Some(sql_gate.clone()),
        ..LiveTicketTestHooks::default()
    };
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks = hooks.clone();
    let server = LiveTestServer::start_with_options(options).await;
    let session_id = URL_SAFE_NO_PAD.encode([85_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let connect = spawn_ticket_connect(&server, session_id, ticket);
    bounded(sql_gate.wait_for_entries(1)).await;
    sql_gate.release_one();
    let mut socket = bounded(connect).await.unwrap().unwrap();
    bounded(expect_strict_hello(&mut socket)).await;
    let statements_before = hooks.credential_query_started.load(Ordering::SeqCst);

    bounded(sql_gate.wait_for_entries(2)).await;
    let bearer = server
        .live_hub
        .register(VaultId(server.vault), DeviceId(server.owner_device));
    bounded(hooks.work_probe.wait_for_cancellation(1)).await;
    sql_gate.release_one();
    bounded(hooks.work_probe.wait_for_disposals(2)).await;
    match first_socket_message(&mut socket).await {
        None | Some(Err(_)) | Some(Ok(tungstenite::Message::Close(_))) => {}
        Some(Ok(frame)) => panic!("takeover emitted a stale active frame: {frame:?}"),
    }
    assert_eq!(
        hooks.credential_query_started.load(Ordering::SeqCst),
        statements_before,
        "cancellation allowed the blocked check to start its next statement"
    );
    server.live_hub.unregister(
        VaultId(server.vault),
        DeviceId(server.owner_device),
        bearer.generation,
    );
    server.shutdown().await;
}

#[tokio::test]
async fn ticket_live_upgrade_echoes_protocol_and_sends_hello() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([31_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let (mut socket, _) = server.connect_with_ticket(&session_id, &ticket).await;
    expect_strict_hello(&mut socket).await;
    server.shutdown().await;
}

#[tokio::test]
async fn ticket_upgrade_without_trusted_origin_returns_fixed_unsupported_code() {
    let session_id = URL_SAFE_NO_PAD.encode([30_u8; 16]);
    let configured = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let ticket = issue_ticket(&configured, session_id.clone()).await;
    configured.shutdown().await;

    let server =
        LiveTestServer::start_without_trusted_origin(FakeLiveBackend::immediate(), true).await;
    let protocol = live_ticket_protocol(&session_id, &ticket);
    let mut headers = HeaderMap::new();
    headers.insert("origin", "https://web.peppy.test".parse().unwrap());
    headers.insert("sec-websocket-protocol", protocol.parse().unwrap());

    let rejection = crate::live_ticket_upgrade::classify_live_upgrade(&headers, None).unwrap_err();
    assert_eq!(rejection.code(), "live_ticket_unsupported");
    assert_eq!(
        server
            .ticket_upgrade_status(&session_id, &ticket, Some("https://web.peppy.test"), false)
            .await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    server.shutdown().await;
}

#[tokio::test]
async fn ticket_issuance_rejects_unconfigured_or_unready_without_writes() {
    let unconfigured =
        LiveTestServer::start_without_trusted_origin(FakeLiveBackend::immediate(), true).await;
    let before = live_ticket_write_counts(&unconfigured).await;
    let response = live_ticket_request(
        &unconfigured,
        reqwest::Method::POST,
        URL_SAFE_NO_PAD.encode([31_u8; 16]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["code"],
        "live_ticket_unsupported"
    );
    assert_eq!(live_ticket_write_counts(&unconfigured).await, before);
    unconfigured.shutdown().await;

    let unready = LiveTestServer::start(FakeLiveBackend::immediate(), false).await;
    let before = live_ticket_write_counts(&unready).await;
    let response = live_ticket_request(
        &unready,
        reqwest::Method::POST,
        URL_SAFE_NO_PAD.encode([32_u8; 16]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["code"],
        "live_unavailable"
    );
    assert_eq!(live_ticket_write_counts(&unready).await, before);
    unready.shutdown().await;

    let ready = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let before = live_ticket_write_counts(&ready).await;
    let response = live_ticket_request(
        &ready,
        reqwest::Method::POST,
        URL_SAFE_NO_PAD.encode([33_u8; 16]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(response.json::<serde_json::Value>().await.unwrap()["ticket"].is_string());
    assert_eq!(
        live_ticket_write_counts(&ready).await,
        LiveTicketWriteCounts {
            presence: before.presence + 1,
            tickets: before.tickets + 1,
            sessions: before.sessions + 1,
            admissions: before.admissions,
        }
    );
    ready.shutdown().await;
}

#[tokio::test]
async fn duplicate_ticket_protocol_headers_are_rejected_over_raw_tcp() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([33_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let protocol = live_ticket_protocol(&session_id, &ticket);

    assert_eq!(
        server.raw_ticket_upgrade_status(&protocol).await,
        StatusCode::BAD_REQUEST
    );
    server.shutdown().await;
}

#[tokio::test]
async fn closed_ticket_session_is_rejected_at_upgrade() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([34_u8; 16]);
    let (ticket, handle) = issue_ticket_with_handle(&server, session_id.clone()).await;
    assert_eq!(
        live_ticket_body_request(
            &server,
            reqwest::Method::DELETE,
            serde_json::json!({"session_id": session_id, "lifetime_handle": handle}),
        )
        .await
        .status(),
        reqwest::StatusCode::NO_CONTENT
    );

    assert_eq!(
        server
            .ticket_upgrade_status(&session_id, &ticket, Some("https://web.peppy.test"), false)
            .await,
        StatusCode::UNAUTHORIZED
    );
    server.shutdown().await;
}

#[tokio::test]
async fn deleting_ticket_session_closes_its_open_socket() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([36_u8; 16]);
    let (ticket, handle) = issue_ticket_with_handle(&server, session_id.clone()).await;
    let (mut socket, _) = server.connect_with_ticket(&session_id, &ticket).await;
    expect_strict_hello(&mut socket).await;

    assert_eq!(
        live_ticket_body_request(
            &server,
            reqwest::Method::DELETE,
            serde_json::json!({"session_id": session_id, "lifetime_handle": handle}),
        )
        .await
        .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while socket.next().await.is_some() {}
    })
    .await
    .expect("ticket socket remained open after session deletion");
    server.shutdown().await;
}

#[tokio::test]
async fn ticket_socket_recheck_renews_its_open_lease() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([38_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let session_hash = Sha256::digest(URL_SAFE_NO_PAD.decode(&session_id).unwrap()).to_vec();
    let before: i64 = sqlx::query_scalar("SELECT (extract(epoch FROM lease_until)*1000000)::bigint FROM live_ticket_sessions WHERE session_id_hash=$1")
        .bind(&session_hash)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    let (mut socket, _) = server.connect_with_ticket(&session_id, &ticket).await;
    expect_strict_hello(&mut socket).await;
    tokio::time::sleep(Duration::from_millis(75)).await;
    let after: i64 = sqlx::query_scalar("SELECT (extract(epoch FROM lease_until)*1000000)::bigint FROM live_ticket_sessions WHERE session_id_hash=$1")
        .bind(session_hash)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert!(after > before);
    server.shutdown().await;
}

#[tokio::test]
async fn ticket_socket_recheck_rejects_the_revoked_presented_credential() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([71_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let session_hash = Sha256::digest(URL_SAFE_NO_PAD.decode(&session_id).unwrap()).to_vec();
    let (mut socket, _) = server.connect_with_ticket(&session_id, &ticket).await;
    expect_strict_hello(&mut socket).await;

    let second_digest = Sha256::digest(b"second-active-credential").to_vec();
    sqlx::query("INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)")
        .bind(&second_digest)
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();
    let old_digest = Sha256::digest(server.owner_token.as_bytes()).to_vec();
    sqlx::query("UPDATE device_credentials SET revoked_at=clock_timestamp() WHERE token_digest=$1")
        .bind(old_digest)
        .execute(&server.pool)
        .await
        .unwrap();
    let lease_before: i64 = sqlx::query_scalar("SELECT (extract(epoch FROM lease_until)*1000000)::bigint FROM live_ticket_sessions WHERE session_id_hash=$1")
        .bind(&session_hash)
        .fetch_one(&server.pool)
        .await
        .unwrap();

    let close = next_close(&mut socket).await;
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "live_unauthorized");
    let lease_after: i64 = sqlx::query_scalar("SELECT (extract(epoch FROM lease_until)*1000000)::bigint FROM live_ticket_sessions WHERE session_id_hash=$1")
        .bind(session_hash)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(
        lease_after, lease_before,
        "revoked credential renewed its lease"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn stale_ticket_socket_cannot_renew_its_durable_successor() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([72_u8; 16]);
    let (first_ticket, handle) = issue_ticket_with_handle(&server, session_id.clone()).await;
    let (mut stale_socket, _) = server.connect_with_ticket(&session_id, &first_ticket).await;
    expect_strict_hello(&mut stale_socket).await;
    let close_waiter = tokio::spawn(async move { next_close(&mut stale_socket).await });

    let successor_ticket = reconnect_ticket(&server, &session_id, &handle).await;
    let (session_bytes, successor_secret) = ticket_parts(&session_id, &successor_ticket);
    let successor = consume_live_ticket(&server.pool, session_bytes, successor_secret)
        .await
        .unwrap();
    let session_hash = Sha256::digest(session_bytes).to_vec();
    let before: (i64, i64) = sqlx::query_as("SELECT active_connection_id,(extract(epoch FROM lease_until)*1000000)::bigint FROM live_ticket_sessions WHERE session_id_hash=$1")
        .bind(&session_hash)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(before.0, successor.connection_id);

    let close = bounded(close_waiter).await.unwrap();
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "live_unauthorized");
    let after: (i64, i64) = sqlx::query_as("SELECT active_connection_id,(extract(epoch FROM lease_until)*1000000)::bigint FROM live_ticket_sessions WHERE session_id_hash=$1")
        .bind(session_hash)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(after, before, "stale socket changed its durable successor");
    server.shutdown().await;
}

#[tokio::test]
async fn ticket_socket_recheck_closes_revoked_device() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([39_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let (mut socket, _) = server.connect_with_ticket(&session_id, &ticket).await;
    expect_strict_hello(&mut socket).await;
    sqlx::query("UPDATE devices SET revoked_at=now() WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();
    let close = next_close(&mut socket).await;
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "live_unauthorized");
    server.shutdown().await;
}

#[tokio::test]
async fn ticket_socket_recheck_closes_closed_or_expired_lease() {
    for (value, update) in [
        (40_u8, "state='closed', closed_at=now(), lease_until=NULL"),
        (41_u8, "lease_until=now()-interval '1 second'"),
    ] {
        let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
        let session_id = URL_SAFE_NO_PAD.encode([value; 16]);
        let ticket = issue_ticket(&server, session_id.clone()).await;
        let (mut socket, _) = server.connect_with_ticket(&session_id, &ticket).await;
        expect_strict_hello(&mut socket).await;
        sqlx::query(&format!(
            "UPDATE live_ticket_sessions SET {update} WHERE vault_id=$1 AND device_id=$2"
        ))
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();
        let close = next_close(&mut socket).await;
        assert_eq!(close.code, CloseCode::Policy);
        assert_eq!(close.reason, "live_unauthorized");
        server.shutdown().await;
    }
}

#[tokio::test]
async fn live_ticket_reaper_terminalizes_and_reclaims_only_eligible_rows() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let expired_session = URL_SAFE_NO_PAD.encode([43_u8; 16]);
    let expired_ticket = issue_ticket(&server, expired_session.clone()).await;
    let session_hash = Sha256::digest(URL_SAFE_NO_PAD.decode(&expired_session).unwrap()).to_vec();
    sqlx::query("UPDATE live_ticket_sessions SET lease_until=now()-interval '1 second' WHERE session_id_hash=$1")
        .bind(&session_hash)
        .execute(&server.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE live_tickets SET issued_at=now()-interval '2 seconds', expires_at=now()-interval '1 second' WHERE ticket_digest=$1")
        .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(&expired_ticket).unwrap()).to_vec())
        .execute(&server.pool)
        .await
        .unwrap();
    let recent_tombstone = URL_SAFE_NO_PAD.encode([44_u8; 16]);
    let (_, recent_handle) = issue_ticket_with_handle(&server, recent_tombstone.clone()).await;
    assert_eq!(
        live_ticket_body_request(
            &server,
            reqwest::Method::DELETE,
            serde_json::json!({"session_id": recent_tombstone, "lifetime_handle": recent_handle}),
        )
        .await
        .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let old_tombstone = URL_SAFE_NO_PAD.encode([45_u8; 16]);
    let (_, old_handle) = issue_ticket_with_handle(&server, old_tombstone.clone()).await;
    assert_eq!(
        live_ticket_body_request(
            &server,
            reqwest::Method::DELETE,
            serde_json::json!({"session_id": old_tombstone, "lifetime_handle": old_handle}),
        )
        .await
        .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    sqlx::query("UPDATE live_ticket_sessions SET closed_at=now()-interval '6 minutes' WHERE session_id_hash=$1")
        .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(&old_tombstone).unwrap()).to_vec())
        .execute(&server.pool)
        .await
        .unwrap();

    crate::live_ticket_maintenance::reap_live_tickets(&server.pool, 100)
        .await
        .unwrap();
    let state: String =
        sqlx::query_scalar("SELECT state FROM live_ticket_sessions WHERE session_id_hash=$1")
            .bind(&session_hash)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(state, "terminal");
    let tickets: i64 =
        sqlx::query_scalar("SELECT count(*) FROM live_tickets WHERE ticket_digest=$1")
            .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(&expired_ticket).unwrap()).to_vec())
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(tickets, 0);
    let retained: i64 =
        sqlx::query_scalar("SELECT count(*) FROM live_ticket_sessions WHERE session_id_hash=$1")
            .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(recent_tombstone).unwrap()).to_vec())
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(retained, 1);
    let reaped: i64 =
        sqlx::query_scalar("SELECT count(*) FROM live_ticket_sessions WHERE session_id_hash=$1")
            .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(old_tombstone).unwrap()).to_vec())
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(reaped, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn delayed_ticket_cancel_for_g1_leaves_g2_open() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([37_u8; 16]);
    let (ticket, handle) = issue_ticket_with_handle(&server, session_id.clone()).await;
    let (mut g1, _) = server.connect_with_ticket(&session_id, &ticket).await;
    expect_strict_hello(&mut g1).await;
    let (session, _) = ticket_parts(&session_id, &ticket);
    let g1_generation = server
        .live_hub
        .ticket_generation(
            VaultId(server.vault),
            DeviceId(server.owner_device),
            session,
        )
        .unwrap();

    let replacement_ticket = reconnect_ticket(&server, &session_id, &handle).await;
    let (mut g2, _) = server
        .connect_with_ticket(&session_id, &replacement_ticket)
        .await;
    expect_strict_hello(&mut g2).await;
    server.live_hub.cancel_exact(
        VaultId(server.vault),
        DeviceId(server.owner_device),
        session,
        g1_generation,
    );

    match tokio::time::timeout(Duration::from_millis(200), g2.next()).await {
        Err(_) | Ok(Some(Ok(tungstenite::Message::Ping(_)))) => {}
        Ok(Some(Ok(tungstenite::Message::Close(_)))) | Ok(None) => {
            panic!("delayed G1 cancellation closed G2")
        }
        Ok(Some(Ok(message))) => panic!("unexpected G2 frame: {message:?}"),
        Ok(Some(Err(error))) => panic!("G2 read failed: {error}"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn ticket_upgrade_trace_logs_do_not_contain_ticket_or_protocol() {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .without_time()
        .with_writer(logs.clone())
        .finish();
    let _default = tracing::subscriber::set_default(subscriber);
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([35_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let (mut socket, protocol) = server.connect_with_ticket(&session_id, &ticket).await;
    expect_strict_hello(&mut socket).await;

    let captured = logs.text();
    assert!(!captured.contains(&ticket));
    assert!(!captured.contains(&protocol));
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_issue_and_delete_trace_logs_do_not_contain_request_or_response_secrets() {
    const SAFE_MARKER: &str = "live_ticket_issue_delete_trace_control";

    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .without_time()
        .with_writer(logs.clone())
        .finish();
    let _default = tracing::subscriber::set_default(subscriber);
    tracing::trace!(
        safe_marker = SAFE_MARKER,
        "live ticket trace collector control"
    );

    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([69_u8; 16]);
    let issued = live_ticket_request(&server, reqwest::Method::POST, session_id.clone())
        .await
        .error_for_status()
        .unwrap();
    let body = issued.json::<serde_json::Value>().await.unwrap();
    let ticket = body["ticket"].as_str().unwrap().to_owned();
    let handle = body["lifetime_handle"].as_str().unwrap().to_owned();
    let protocol = live_ticket_protocol(&session_id, &ticket);
    assert_eq!(
        live_ticket_body_request(
            &server,
            reqwest::Method::DELETE,
            serde_json::json!({"session_id": session_id, "lifetime_handle": handle}),
        )
        .await
        .status(),
        reqwest::StatusCode::NO_CONTENT
    );

    let captured = logs.text();
    assert!(captured.contains(SAFE_MARKER));
    for secret in [&ticket, &protocol, &handle, &server.owner_token] {
        assert!(
            !captured.contains(secret),
            "live ticket request or response secret reached tracing output"
        );
    }
    server.shutdown().await;
}

#[tokio::test]
async fn ticket_live_upgrade_rejects_replay_origin_mixed_revoked_and_closed_sessions() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;

    let replay_session = URL_SAFE_NO_PAD.encode([32_u8; 16]);
    let (replay_ticket, replay_handle) =
        issue_ticket_with_handle(&server, replay_session.clone()).await;
    let (socket, _) = server
        .connect_with_ticket(&replay_session, &replay_ticket)
        .await;
    drop(socket);
    assert_eq!(
        server
            .ticket_upgrade_status(
                &replay_session,
                &replay_ticket,
                Some("https://web.peppy.test"),
                false,
            )
            .await,
        StatusCode::UNAUTHORIZED
    );

    let origin_session = replay_session;
    let origin_ticket = reconnect_ticket(&server, &origin_session, &replay_handle).await;
    assert_eq!(
        server
            .ticket_upgrade_status(
                &origin_session,
                &origin_ticket,
                Some("https://wrong.peppy.test"),
                false,
            )
            .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        server
            .ticket_upgrade_status(&origin_session, &origin_ticket, None, false)
            .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        server
            .ticket_upgrade_status(
                &origin_session,
                &origin_ticket,
                Some("https://web.peppy.test"),
                true,
            )
            .await,
        StatusCode::BAD_REQUEST
    );

    let revoked_session = origin_session;
    let revoked_ticket = origin_ticket;
    sqlx::query(
        "UPDATE devices SET revoked_at=clock_timestamp() WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .execute(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        server
            .ticket_upgrade_status(
                &revoked_session,
                &revoked_ticket,
                Some("https://web.peppy.test"),
                false,
            )
            .await,
        StatusCode::UNAUTHORIZED
    );
    server.shutdown().await;
}

fn ticket_parts(session_id: &str, ticket: &str) -> ([u8; 16], [u8; 32]) {
    (
        URL_SAFE_NO_PAD
            .decode(session_id)
            .unwrap()
            .try_into()
            .unwrap(),
        URL_SAFE_NO_PAD.decode(ticket).unwrap().try_into().unwrap(),
    )
}

async fn live_ticket_consume_snapshot(
    server: &LiveTestServer,
    session_id: [u8; 16],
    secret: [u8; 32],
) -> (String, String, String) {
    let ticket: String = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM live_tickets t WHERE ticket_digest=$1",
    )
    .bind(Sha256::digest(secret).to_vec())
    .fetch_one(&server.pool)
    .await
    .unwrap();
    let session: String = sqlx::query_scalar("SELECT row_to_json(s)::text FROM live_ticket_sessions s WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3")
        .bind(server.vault)
        .bind(server.owner_device)
        .bind(Sha256::digest(session_id).to_vec())
        .fetch_one(&server.pool)
        .await
        .unwrap();
    let authority: String = sqlx::query_scalar("SELECT row_to_json(a)::text FROM live_ticket_device_authority a WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.owner_device)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    (ticket, session, authority)
}

#[tokio::test]
async fn live_ticket_consume_is_atomic_and_fails_closed() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session = URL_SAFE_NO_PAD.encode([23_u8; 16]);
    let ticket = issue_ticket(&server, session.clone()).await;
    let (session_id, secret) = ticket_parts(&session, &ticket);
    let resolved: Option<Uuid> =
        sqlx::query_scalar("SELECT vault_id FROM community_resolve_live_ticket_vault($1)")
            .bind(Sha256::digest(secret).to_vec())
            .fetch_optional(&server.pool)
            .await
            .unwrap();
    assert_eq!(resolved, Some(server.vault));
    let principal = consume_live_ticket(&server.pool, session_id, secret)
        .await
        .unwrap();
    assert_eq!(principal.principal.vault, server.vault);
    assert_eq!(principal.principal.device, server.owner_device);
    let durable: (i64, i64, bool) = sqlx::query_as("SELECT session_incarnation,active_connection_id,lease_until = consumed_at + interval '90 seconds' FROM live_ticket_sessions s JOIN live_tickets t USING(vault_id,device_id,session_id_hash,session_incarnation) WHERE t.ticket_digest=$1")
        .bind(Sha256::digest(secret).to_vec())
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(durable.0, principal.session_incarnation);
    assert_eq!(durable.1, principal.connection_id);
    assert!(
        durable.2,
        "lease must equal consumed_at + 90 seconds exactly"
    );
    assert_eq!(
        consume_live_ticket(&server.pool, session_id, secret)
            .await
            .err()
            .unwrap()
            .1,
        "live_ticket_invalid"
    );

    sqlx::query("UPDATE live_ticket_sessions SET state='closed', closed_at=clock_timestamp(), lease_until=NULL WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();

    let expired_session = URL_SAFE_NO_PAD.encode([24_u8; 16]);
    let expired_ticket = issue_ticket(&server, expired_session.clone()).await;
    sqlx::query("UPDATE live_tickets SET expires_at=clock_timestamp() WHERE ticket_digest=$1")
        .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(&expired_ticket).unwrap()).to_vec())
        .execute(&server.pool)
        .await
        .unwrap();
    let (session_id, secret) = ticket_parts(&expired_session, &expired_ticket);
    assert_eq!(
        consume_live_ticket(&server.pool, session_id, secret)
            .await
            .err()
            .unwrap()
            .1,
        "live_ticket_invalid"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn ticket_post_consume_database_failure_is_unavailable_not_invalid_authority() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session = lifecycle_session(119);
    let ticket = issue_ticket(&server, session.clone()).await;
    // The consume transaction can finish, but the next credential read cannot.
    // This schema mutation is confined to this test's disposable schema.
    sqlx::query("CREATE FUNCTION hide_credentials_after_consume() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN ALTER TABLE device_credentials RENAME TO unavailable_credentials; RETURN NEW; END $$")
        .execute(&server.pool).await.unwrap();
    sqlx::query("CREATE TRIGGER hide_credentials_after_consume AFTER UPDATE OF active_connection_id ON live_ticket_sessions FOR EACH ROW EXECUTE FUNCTION hide_credentials_after_consume()")
        .execute(&server.pool).await.unwrap();
    let status = server
        .ticket_upgrade_status(&session, &ticket, Some("https://web.peppy.test"), false)
        .await;
    let consumed: bool = sqlx::query_scalar("SELECT active_connection_id IS NOT NULL FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault).bind(server.owner_device).fetch_one(&server.pool).await.unwrap();
    server.shutdown().await;
    assert!(
        consumed,
        "the injected failure must happen after durable consumption"
    );
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn ticket_consume_http_budget_expires_while_device_is_locked_without_mutation() {
    let mut options = LiveTestOptions::new(FakeLiveBackend::immediate(), true);
    options.ticket_hooks.http_operation_budget = Some(Duration::from_millis(250));
    let server = LiveTestServer::start_with_options(options).await;
    let session = URL_SAFE_NO_PAD.encode([111_u8; 16]);
    let ticket = issue_ticket(&server, session.clone()).await;
    let (session_id, secret) = ticket_parts(&session, &ticket);
    let before = live_ticket_consume_snapshot(&server, session_id, secret).await;
    let mut blocker = server.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM devices WHERE vault_id=$1 AND device_id=$2 FOR UPDATE")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&mut *blocker)
        .await
        .unwrap();

    let mut connect = spawn_ticket_connect(&server, session, ticket);
    let result = tokio::time::timeout(Duration::from_secs(2), &mut connect).await;
    blocker.rollback().await.unwrap();
    let status = match result {
        Ok(Ok(Err(tungstenite::Error::Http(response)))) => Some(response.status()),
        Ok(Ok(Ok(mut socket))) => {
            let _ = socket.close(None).await;
            None
        }
        Err(_) => {
            if let Ok(Ok(mut socket)) = bounded(connect).await {
                let _ = socket.close(None).await;
            }
            None
        }
        other => panic!("unexpected upgrade result: {other:?}"),
    };
    let after = live_ticket_consume_snapshot(&server, session_id, secret).await;
    server.shutdown().await;
    assert_eq!(status, Some(StatusCode::SERVICE_UNAVAILABLE));
    assert_eq!(after, before, "expired consume must not allocate authority");
}

#[tokio::test]
async fn live_ticket_consume_exhaustion_rejects_without_mutation() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session = URL_SAFE_NO_PAD.encode([66_u8; 16]);
    let ticket = issue_ticket(&server, session.clone()).await;
    let (session_id, secret) = ticket_parts(&session, &ticket);
    sqlx::query("UPDATE live_ticket_device_authority SET next_connection_id=$3 WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.owner_device)
        .bind(i64::MAX)
        .execute(&server.pool)
        .await
        .unwrap();
    let before = live_ticket_consume_snapshot(&server, session_id, secret).await;
    assert_eq!(
        consume_live_ticket(&server.pool, session_id, secret)
            .await
            .err()
            .unwrap()
            .1,
        "live_ticket_invalid"
    );
    assert_eq!(
        live_ticket_consume_snapshot(&server, session_id, secret).await,
        before
    );
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_consume_update_failure_quarantine_preserves_ticket_session_and_counter() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session = URL_SAFE_NO_PAD.encode([67_u8; 16]);
    let ticket = issue_ticket(&server, session.clone()).await;
    let (session_id, secret) = ticket_parts(&session, &ticket);
    sqlx::query("CREATE FUNCTION fail_live_ticket_consume_session_update() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test consume failure'; END $$")
        .execute(&server.pool)
        .await
        .unwrap();
    sqlx::query("CREATE TRIGGER fail_live_ticket_consume_session_update BEFORE UPDATE OF active_connection_id ON live_ticket_sessions FOR EACH ROW EXECUTE FUNCTION fail_live_ticket_consume_session_update()")
        .execute(&server.pool)
        .await
        .unwrap();
    let before = live_ticket_consume_snapshot(&server, session_id, secret).await;
    let error = consume_live_ticket(&server.pool, session_id, secret)
        .await
        .err()
        .unwrap();
    assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error.1, "live_unavailable");
    sqlx::query("DROP TRIGGER fail_live_ticket_consume_session_update ON live_ticket_sessions")
        .execute(&server.pool)
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION fail_live_ticket_consume_session_update()")
        .execute(&server.pool)
        .await
        .unwrap();
    assert_eq!(
        live_ticket_consume_snapshot(&server, session_id, secret).await,
        before
    );
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_consume_rejects_revoked_credential_before_allocation() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session = URL_SAFE_NO_PAD.encode([68_u8; 16]);
    let ticket = issue_ticket(&server, session.clone()).await;
    let (session_id, secret) = ticket_parts(&session, &ticket);
    sqlx::query("UPDATE device_credentials SET revoked_at=clock_timestamp() WHERE vault_id=$1 AND device_id=$2")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();
    let before = live_ticket_consume_snapshot(&server, session_id, secret).await;
    assert_eq!(
        consume_live_ticket(&server.pool, session_id, secret)
            .await
            .err()
            .unwrap()
            .1,
        "live_ticket_invalid"
    );
    assert_eq!(
        live_ticket_consume_snapshot(&server, session_id, secret).await,
        before
    );
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_consume_rejects_closed_or_expired_leases_and_unknown_secrets() {
    for (value, mutation) in [
        (
            25_u8,
            "state='closed', closed_at=clock_timestamp(), lease_until=NULL",
        ),
        (26_u8, "lease_until=clock_timestamp()"),
    ] {
        let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
        let session = URL_SAFE_NO_PAD.encode([value; 16]);
        let ticket = issue_ticket(&server, session.clone()).await;
        sqlx::query(&format!(
            "UPDATE live_ticket_sessions SET {mutation} WHERE vault_id=$1 AND device_id=$2"
        ))
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();
        let (session_id, secret) = ticket_parts(&session, &ticket);
        assert_eq!(
            consume_live_ticket(&server.pool, session_id, secret)
                .await
                .err()
                .unwrap()
                .1,
            "live_ticket_invalid"
        );
        server.shutdown().await;
    }

    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    assert_eq!(
        consume_live_ticket(&server.pool, [27; 16], [28; 32])
            .await
            .err()
            .unwrap()
            .1,
        "live_ticket_invalid"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_consume_allows_exactly_one_replica_winner() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session = URL_SAFE_NO_PAD.encode([29_u8; 16]);
    let ticket = issue_ticket(&server, session.clone()).await;
    let (session_id, secret) = ticket_parts(&session, &ticket);
    let one = live_ticket_replica_pool(&server.schema).await;
    let two = live_ticket_replica_pool(&server.schema).await;
    let (left, right) = tokio::join!(
        consume_live_ticket(&one, session_id, secret),
        consume_live_ticket(&two, session_id, secret)
    );
    assert_eq!(
        [left.is_ok(), right.is_ok()]
            .into_iter()
            .filter(|winner| *winner)
            .count(),
        1
    );
    one.close().await;
    two.close().await;
    server.shutdown().await;
}

#[tokio::test]
async fn expired_provisional_ticket_lease_terminalizes_persistently_after_lock_delay() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([19_u8; 16]);
    let (ticket, handle) = issue_ticket_with_handle(&server, session_id.clone()).await;
    let session_hash = Sha256::digest(URL_SAFE_NO_PAD.decode(&session_id).unwrap()).to_vec();
    sqlx::query("UPDATE live_ticket_sessions SET lease_until=clock_timestamp()+interval '100 milliseconds' WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3")
        .bind(server.vault)
        .bind(server.owner_device)
        .bind(&session_hash)
        .execute(&server.pool)
        .await
        .unwrap();

    let mut device_lock = server.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM devices WHERE vault_id=$1 AND device_id=$2 FOR UPDATE")
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&mut *device_lock)
        .await
        .unwrap();
    let base_url = server.base_url.clone();
    let owner_token = server.owner_token.clone();
    let blocked_session = session_id.clone();
    let blocked_handle = handle.clone();
    let request = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("{base_url}/v1/live/tickets"))
            .bearer_auth(owner_token)
            .json(&serde_json::json!({"session_id": blocked_session, "lifetime_handle": blocked_handle}))
            .send()
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(250)).await;
    device_lock.commit().await.unwrap();

    let rejected = bounded(request).await.unwrap();
    assert_lifetime_refused(rejected, "delayed expired reconnect").await;
    let state: String = sqlx::query_scalar(
        "SELECT state FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .bind(&session_hash)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(state, "terminal");
    let outstanding: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_tickets WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3 AND consumed_at IS NULL",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .bind(&session_hash)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(outstanding, 0);
    assert_eq!(
        server
            .ticket_upgrade_status(&session_id, &ticket, Some("https://web.peppy.test"), false)
            .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        live_ticket_request(
            &server,
            reqwest::Method::POST,
            URL_SAFE_NO_PAD.encode([20_u8; 16]),
        )
        .await
        .status(),
        StatusCode::CREATED
    );
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_issued_tickets_are_stored_only_as_digests() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let active_id = URL_SAFE_NO_PAD.encode([10_u8; 16]);
    let issued = live_ticket_request(&server, reqwest::Method::POST, active_id).await;
    assert_eq!(issued.status(), reqwest::StatusCode::CREATED);
    let ticket = issued.json::<serde_json::Value>().await.unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_owned();
    let raw = URL_SAFE_NO_PAD.decode(ticket).unwrap();
    let stored_raw: i64 =
        sqlx::query_scalar("SELECT count(*) FROM live_tickets WHERE ticket_digest=$1")
            .bind(&raw)
            .fetch_one(&server.pool)
            .await
            .unwrap();
    assert_eq!(stored_raw, 0);
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_initial_reservations_are_capped_per_minute() {
    let server = bounded(LiveTestServer::start(FakeLiveBackend::immediate(), true)).await;
    for byte in 11_u8..17 {
        assert_eq!(
            live_ticket_request(
                &server,
                reqwest::Method::POST,
                URL_SAFE_NO_PAD.encode([byte; 16])
            )
            .await
            .status(),
            reqwest::StatusCode::CREATED
        );
    }
    let before = live_ticket_device_snapshot(&server).await;
    assert_eq!(
        live_ticket_request(
            &server,
            reqwest::Method::POST,
            URL_SAFE_NO_PAD.encode([17_u8; 16])
        )
        .await
        .status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(live_ticket_device_snapshot(&server).await, before);
    let open: i64 = bounded(
        sqlx::query_scalar(
            "SELECT count(*) FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND state='open'",
        )
        .bind(server.vault)
        .bind(server.owner_device)
        .fetch_one(&server.pool),
    )
    .await
    .unwrap();
    assert_eq!(open, 1);
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_absent_session_delete_is_refused_and_reaper_keeps_recent_rows() {
    let server = bounded(LiveTestServer::start(FakeLiveBackend::immediate(), true)).await;
    let existing_session = URL_SAFE_NO_PAD.encode([61_u8; 16]);
    let live_session = URL_SAFE_NO_PAD.encode([68_u8; 16]);
    let (_, existing_handle) = issue_ticket_with_handle(&server, existing_session.clone()).await;
    bounded(
        sqlx::query("INSERT INTO live_ticket_sessions(vault_id,device_id,session_id_hash,state,lease_until) VALUES($1,$2,$3,'open',now()+interval '1 hour')")
            .bind(server.vault)
            .bind(server.owner_device)
            .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(&live_session).unwrap()).to_vec())
            .execute(&server.pool),
    )
    .await
    .unwrap();
    let before = live_ticket_device_snapshot(&server).await;
    for byte in 62_u8..67 {
        let absent = URL_SAFE_NO_PAD.encode([byte; 16]);
        let fabricated = URL_SAFE_NO_PAD.encode(7_i64.to_be_bytes());
        assert_lifetime_refused(
            live_ticket_body_request(
                &server,
                reqwest::Method::DELETE,
                serde_json::json!({"session_id": absent, "lifetime_handle": fabricated}),
            )
            .await,
            "absent DELETE flood",
        )
        .await;
    }
    assert_eq!(live_ticket_device_snapshot(&server).await, before);

    assert_eq!(
        live_ticket_body_request(
            &server,
            reqwest::Method::DELETE,
            serde_json::json!({"session_id": existing_session, "lifetime_handle": existing_handle}),
        )
        .await
        .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    bounded(
        sqlx::query("UPDATE live_ticket_sessions SET closed_at=now()-interval '6 minutes' WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3")
            .bind(server.vault)
            .bind(server.owner_device)
            .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(&existing_session).unwrap()).to_vec())
            .execute(&server.pool),
    )
    .await
    .unwrap();
    assert_eq!(
        bounded(crate::live_ticket_maintenance::reap_live_tickets(
            &server.pool,
            100
        ))
        .await
        .unwrap(),
        1
    );
    assert!(
        live_ticket_session_state(&server, &existing_session)
            .await
            .is_none()
    );
    assert_eq!(
        live_ticket_session_state(&server, &live_session)
            .await
            .as_deref(),
        Some("open")
    );
    server.shutdown().await;
}

async fn live_ticket_replica_pool(schema: &str) -> PgPool {
    let database_url = std::env::var("TEST_DATABASE_URL").unwrap();
    let mut isolated: url::Url = database_url.parse().unwrap();
    isolated
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    PgPoolOptions::new()
        .max_connections(2)
        .connect(isolated.as_str())
        .await
        .unwrap()
}

#[tokio::test]
async fn live_ticket_concurrent_issue_on_replicas_keeps_one_open_session() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let replica_one = live_ticket_replica_pool(&server.schema).await;
    let replica_two = live_ticket_replica_pool(&server.schema).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let first = URL_SAFE_NO_PAD.encode([21_u8; 16]);
    let second = URL_SAFE_NO_PAD.encode([22_u8; 16]);
    let first_request = async {
        barrier.wait().await;
        live_ticket_request(&server, reqwest::Method::POST, first)
            .await
            .status()
    };
    let second_request = async {
        barrier.wait().await;
        live_ticket_request(&server, reqwest::Method::POST, second)
            .await
            .status()
    };
    let (first_status, second_status) = tokio::join!(first_request, second_request);
    assert!(matches!(
        first_status,
        reqwest::StatusCode::CREATED | reqwest::StatusCode::TOO_MANY_REQUESTS
    ));
    assert!(matches!(
        second_status,
        reqwest::StatusCode::CREATED | reqwest::StatusCode::TOO_MANY_REQUESTS
    ));
    let open_one: i64 =
        sqlx::query_scalar("SELECT count(*) FROM live_ticket_sessions WHERE state='open'")
            .fetch_one(&replica_one)
            .await
            .unwrap();
    let open_two: i64 =
        sqlx::query_scalar("SELECT count(*) FROM live_ticket_sessions WHERE state='open'")
            .fetch_one(&replica_two)
            .await
            .unwrap();
    assert_eq!(open_one, open_two);
    assert!(open_one <= 1);
    replica_one.close().await;
    replica_two.close().await;
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_reconnect_close_race_never_leaves_an_open_lifetime() {
    for value in 0_u8..20 {
        let server = bounded(LiveTestServer::start(FakeLiveBackend::immediate(), true)).await;
        let replica_one = live_ticket_replica_pool(&server.schema).await;
        let replica_two = live_ticket_replica_pool(&server.schema).await;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let session = URL_SAFE_NO_PAD.encode([value; 16]);
        let (_, handle) = issue_ticket_with_handle(&server, session.clone()).await;
        let reconnect = async {
            bounded(barrier.wait()).await;
            live_ticket_body_request(
                &server,
                reqwest::Method::POST,
                serde_json::json!({"session_id": session, "lifetime_handle": handle}),
            )
            .await
            .status()
        };
        let delete = async {
            bounded(barrier.wait()).await;
            live_ticket_body_request(
                &server,
                reqwest::Method::DELETE,
                serde_json::json!({"session_id": session, "lifetime_handle": handle}),
            )
            .await
            .status()
        };
        let (reconnected, deleted) = tokio::join!(reconnect, delete);
        assert_eq!(deleted, reqwest::StatusCode::NO_CONTENT);
        assert!(matches!(
            reconnected,
            reqwest::StatusCode::CREATED | reqwest::StatusCode::CONFLICT
        ));
        let open: i64 =
            sqlx::query_scalar("SELECT count(*) FROM live_ticket_sessions WHERE state='open'")
                .fetch_one(&replica_one)
                .await
                .unwrap();
        assert_eq!(open, 0);
        let open: i64 =
            sqlx::query_scalar("SELECT count(*) FROM live_ticket_sessions WHERE state='open'")
                .fetch_one(&replica_two)
                .await
                .unwrap();
        assert_eq!(open, 0);
        replica_one.close().await;
        replica_two.close().await;
        server.shutdown().await;
    }
}

fn lifecycle_session(byte: u8) -> String {
    URL_SAFE_NO_PAD.encode([byte; 16])
}

fn lifetime_body(session_id: &str, handle: &str) -> serde_json::Value {
    serde_json::json!({"session_id": session_id, "lifetime_handle": handle})
}

#[tokio::test]
async fn live_ticket_invalid_lifetime_handles_are_refused_without_writes() {
    let server = bounded(LiveTestServer::start(FakeLiveBackend::immediate(), true)).await;
    let session_id = lifecycle_session(50);
    let absent_session = lifecycle_session(51);
    let (_ticket, handle) = issue_ticket_with_handle(&server, session_id.clone()).await;
    let before = live_ticket_device_snapshot(&server).await;
    for (case, body) in invalid_lifetime_bodies(&session_id, &handle) {
        assert_lifetime_refused(
            live_ticket_body_request(&server, reqwest::Method::POST, body.clone()).await,
            case,
        )
        .await;
        assert_eq!(
            live_ticket_device_snapshot(&server).await,
            before,
            "{case} reconnect wrote"
        );
        assert_lifetime_refused(
            live_ticket_body_request(&server, reqwest::Method::DELETE, body).await,
            case,
        )
        .await;
        assert_eq!(
            live_ticket_device_snapshot(&server).await,
            before,
            "{case} close wrote"
        );
    }
    for (case, body) in invalid_lifetime_bodies(&absent_session, &handle) {
        assert_lifetime_refused(
            live_ticket_body_request(&server, reqwest::Method::DELETE, body).await,
            case,
        )
        .await;
    }
    assert_eq!(
        live_ticket_device_snapshot(&server).await,
        before,
        "absent close wrote"
    );
    assert_eq!(
        live_ticket_session_state(&server, &session_id)
            .await
            .as_deref(),
        Some("open")
    );
    reconnect_ticket(&server, &session_id, &handle).await;
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_refusals_do_not_touch_presence_for_a_fresh_bearer() {
    let server = bounded(LiveTestServer::start(FakeLiveBackend::immediate(), true)).await;
    let session_id = lifecycle_session(70);
    let terminal = lifecycle_session(71);
    let handle = URL_SAFE_NO_PAD.encode(1_i64.to_be_bytes());
    let terminal_handle = URL_SAFE_NO_PAD.encode(2_i64.to_be_bytes());
    for (id, incarnation, state, lease) in [
        (&session_id, 1_i64, "open", "now()+interval '1 hour'"),
        (&terminal, 2_i64, "terminal", "NULL"),
    ] {
        bounded(
            sqlx::query(&format!("INSERT INTO live_ticket_sessions(vault_id,device_id,session_id_hash,session_incarnation,state,lease_until,closed_at) VALUES($1,$2,$3,$4,'{state}',{lease},CASE WHEN '{state}'='terminal' THEN now() END)"))
                .bind(server.vault)
                .bind(server.peer_device)
                .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(id).unwrap()).to_vec())
                .bind(incarnation)
                .execute(&server.pool),
        )
        .await
        .unwrap();
    }

    let before = live_ticket_refusal_snapshot(&server).await;
    let stale = URL_SAFE_NO_PAD.encode(999_999_i64.to_be_bytes());
    for (case, body) in [
        ("missing", serde_json::json!({"session_id": session_id})),
        ("malformed", lifetime_body(&session_id, "not a handle")),
        ("stale", lifetime_body(&session_id, &stale)),
        ("absent", lifetime_body(&lifecycle_session(72), &handle)),
        ("terminal", lifetime_body(&terminal, &terminal_handle)),
    ] {
        for method in [reqwest::Method::POST, reqwest::Method::DELETE] {
            assert_lifetime_refused(
                live_ticket_body_request_with_token(
                    &server,
                    &server.peer_token,
                    method,
                    body.clone(),
                )
                .await,
                case,
            )
            .await;
            assert_eq!(
                live_ticket_refusal_snapshot(&server).await,
                before,
                "{case} fresh-bearer refusal wrote SQL state"
            );
        }
    }
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_exact_handle_reconnects_then_closes_only_its_lifetime() {
    let server = bounded(LiveTestServer::start(FakeLiveBackend::immediate(), true)).await;
    let session_id = lifecycle_session(58);
    let (first_ticket, handle) = issue_ticket_with_handle(&server, session_id.clone()).await;
    let reconnected = reconnect_ticket(&server, &session_id, &handle).await;
    assert_ne!(reconnected, first_ticket);
    let (mut socket, _) = bounded(server.connect_with_ticket(&session_id, &reconnected)).await;
    bounded(expect_strict_hello(&mut socket)).await;

    assert_eq!(
        live_ticket_body_request(
            &server,
            reqwest::Method::DELETE,
            lifetime_body(&session_id, &handle)
        )
        .await
        .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    assert_eq!(
        live_ticket_session_state(&server, &session_id)
            .await
            .as_deref(),
        Some("closed")
    );
    let closed = live_ticket_device_snapshot(&server).await;
    assert_lifetime_refused(
        live_ticket_body_request(
            &server,
            reqwest::Method::DELETE,
            lifetime_body(&session_id, &handle),
        )
        .await,
        "repeated close",
    )
    .await;
    assert_lifetime_refused(
        live_ticket_body_request(
            &server,
            reqwest::Method::POST,
            lifetime_body(&session_id, &handle),
        )
        .await,
        "reconnect after close",
    )
    .await;
    assert_eq!(live_ticket_device_snapshot(&server).await, closed);
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_stale_handle_after_reaping_never_touches_the_successor() {
    let server = bounded(LiveTestServer::start(FakeLiveBackend::immediate(), true)).await;
    let session_id = lifecycle_session(56);
    let (_ticket, stale) = issue_ticket_with_handle(&server, session_id.clone()).await;
    assert_eq!(
        live_ticket_body_request(
            &server,
            reqwest::Method::DELETE,
            lifetime_body(&session_id, &stale)
        )
        .await
        .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    bounded(
        sqlx::query("UPDATE live_ticket_sessions SET closed_at=now()-interval '6 minutes' WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3")
            .bind(server.vault)
            .bind(server.owner_device)
            .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(&session_id).unwrap()).to_vec())
            .execute(&server.pool),
    )
    .await
    .unwrap();
    bounded(crate::live_ticket_maintenance::reap_live_tickets(
        &server.pool,
        100,
    ))
    .await
    .unwrap();
    assert!(
        live_ticket_session_state(&server, &session_id)
            .await
            .is_none()
    );

    let (_successor_ticket, successor) =
        issue_ticket_with_handle(&server, session_id.clone()).await;
    assert_ne!(successor, stale);
    let before = live_ticket_device_snapshot(&server).await;
    assert_lifetime_refused(
        live_ticket_body_request(
            &server,
            reqwest::Method::POST,
            lifetime_body(&session_id, &stale),
        )
        .await,
        "stale reconnect",
    )
    .await;
    assert_lifetime_refused(
        live_ticket_body_request(
            &server,
            reqwest::Method::DELETE,
            lifetime_body(&session_id, &stale),
        )
        .await,
        "stale close",
    )
    .await;
    assert_eq!(live_ticket_device_snapshot(&server).await, before);
    assert_eq!(
        live_ticket_session_state(&server, &session_id)
            .await
            .as_deref(),
        Some("open")
    );
    reconnect_ticket(&server, &session_id, &successor).await;
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_invalid_handles_cannot_terminalize_an_expired_successor() {
    let server = bounded(LiveTestServer::start(FakeLiveBackend::immediate(), true)).await;
    let session_id = lifecycle_session(73);
    let (_ticket, stale) = issue_ticket_with_handle(&server, session_id.clone()).await;
    assert_eq!(
        live_ticket_body_request(
            &server,
            reqwest::Method::DELETE,
            lifetime_body(&session_id, &stale),
        )
        .await
        .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    bounded(
        sqlx::query("UPDATE live_ticket_sessions SET closed_at=now()-interval '6 minutes' WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3")
            .bind(server.vault)
            .bind(server.owner_device)
            .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(&session_id).unwrap()).to_vec())
            .execute(&server.pool),
    )
    .await
    .unwrap();
    bounded(crate::live_ticket_maintenance::reap_live_tickets(
        &server.pool,
        100,
    ))
    .await
    .unwrap();
    let (_ticket, successor) = issue_ticket_with_handle(&server, session_id.clone()).await;
    bounded(
        sqlx::query("UPDATE live_ticket_sessions SET lease_until=now()-interval '1 second' WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3")
            .bind(server.vault)
            .bind(server.owner_device)
            .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(&session_id).unwrap()).to_vec())
            .execute(&server.pool),
    )
    .await
    .unwrap();

    let before = live_ticket_refusal_snapshot(&server).await;
    for (case, body) in [
        ("missing", serde_json::json!({"session_id": session_id})),
        ("malformed", lifetime_body(&session_id, "not a handle")),
        ("stale", lifetime_body(&session_id, &stale)),
    ] {
        for method in [reqwest::Method::POST, reqwest::Method::DELETE] {
            assert_lifetime_refused(
                live_ticket_body_request(&server, method, body.clone()).await,
                case,
            )
            .await;
            assert_eq!(
                live_ticket_refusal_snapshot(&server).await,
                before,
                "{case} touched the expired successor"
            );
        }
    }
    assert_lifetime_refused(
        live_ticket_body_request(
            &server,
            reqwest::Method::POST,
            lifetime_body(&session_id, &successor),
        )
        .await,
        "exact expired successor",
    )
    .await;
    assert_eq!(
        live_ticket_session_state(&server, &session_id)
            .await
            .as_deref(),
        Some("terminal")
    );
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_exact_expired_handle_terminalizes_only_its_own_lifetime() {
    let server = bounded(LiveTestServer::start(FakeLiveBackend::immediate(), true)).await;
    let expired = lifecycle_session(57);
    let sibling = lifecycle_session(59);
    let (_ticket, expired_handle) = issue_ticket_with_handle(&server, expired.clone()).await;
    // A row created outside this route proves that exact expiry cannot touch another lifetime.
    bounded(
        sqlx::query("INSERT INTO live_ticket_sessions(vault_id,device_id,session_id_hash,state,lease_until) VALUES($1,$2,$3,'open',now()+interval '1 hour')")
            .bind(server.vault)
            .bind(server.owner_device)
            .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(&sibling).unwrap()).to_vec())
            .execute(&server.pool),
    )
    .await
    .unwrap();
    bounded(
        sqlx::query("UPDATE live_ticket_sessions SET lease_until=now()-interval '1 second' WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3")
            .bind(server.vault)
            .bind(server.owner_device)
            .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(&expired).unwrap()).to_vec())
            .execute(&server.pool),
    )
    .await
    .unwrap();
    let sibling_before = live_ticket_session_row(&server, &sibling).await;
    let before = live_ticket_refusal_snapshot(&server).await;
    for (case, body) in invalid_lifetime_bodies(&expired, &expired_handle) {
        for method in [reqwest::Method::POST, reqwest::Method::DELETE] {
            assert_lifetime_refused(
                live_ticket_body_request(&server, method, body.clone()).await,
                case,
            )
            .await;
            assert_eq!(
                live_ticket_refusal_snapshot(&server).await,
                before,
                "{case} terminalized the expired target before validation"
            );
        }
    }

    assert_lifetime_refused(
        live_ticket_body_request(
            &server,
            reqwest::Method::POST,
            lifetime_body(&expired, &expired_handle),
        )
        .await,
        "expired exact reconnect",
    )
    .await;
    assert_eq!(
        live_ticket_session_state(&server, &expired)
            .await
            .as_deref(),
        Some("terminal")
    );
    assert_eq!(live_ticket_unconsumed_count(&server, &expired).await, 0);
    assert_eq!(
        live_ticket_session_row(&server, &sibling).await,
        sibling_before
    );
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_lost_initial_response_recovers_with_a_fresh_session() {
    let server = bounded(LiveTestServer::start(FakeLiveBackend::immediate(), true)).await;
    let lost = lifecycle_session(52);
    let fresh = lifecycle_session(53);
    // The client never learns this lifetime's handle, so it forgets the SID and handle.
    let (lost_ticket, _forgotten_handle) = issue_ticket_with_handle(&server, lost.clone()).await;
    let (_fresh_ticket, fresh_handle) = issue_ticket_with_handle(&server, fresh.clone()).await;
    assert_eq!(
        live_ticket_session_state(&server, &lost).await.as_deref(),
        Some("terminal")
    );
    assert_eq!(live_ticket_unconsumed_count(&server, &lost).await, 0);

    let before = live_ticket_device_snapshot(&server).await;
    assert_lifetime_refused(
        live_ticket_request(&server, reqwest::Method::POST, lost.clone()).await,
        "forgotten initial POST",
    )
    .await;
    assert_lifetime_refused(
        live_ticket_body_request(
            &server,
            reqwest::Method::DELETE,
            serde_json::json!({"session_id": lost}),
        )
        .await,
        "forgotten close",
    )
    .await;
    assert_eq!(live_ticket_device_snapshot(&server).await, before);
    assert_eq!(
        bounded(server.ticket_upgrade_status(
            &lost,
            &lost_ticket,
            Some("https://web.peppy.test"),
            false,
        ))
        .await,
        StatusCode::UNAUTHORIZED
    );
    reconnect_ticket(&server, &fresh, &fresh_handle).await;
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_consumed_reservation_is_never_replaced() {
    let server = bounded(LiveTestServer::start(FakeLiveBackend::immediate(), true)).await;
    let consumed = lifecycle_session(54);
    let (ticket, handle) = issue_ticket_with_handle(&server, consumed.clone()).await;
    let (session_bytes, secret) = ticket_parts(&consumed, &ticket);
    bounded(consume_live_ticket(&server.pool, session_bytes, secret))
        .await
        .unwrap();
    let first_connection_id: i64 = bounded(
        sqlx::query_scalar("SELECT active_connection_id FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3")
            .bind(server.vault)
            .bind(server.owner_device)
            .bind(Sha256::digest(session_bytes).to_vec())
            .fetch_one(&server.pool),
    )
    .await
    .unwrap();
    assert_eq!(
        bounded(crate::live_ticket_maintenance::reap_live_tickets(
            &server.pool,
            1
        ))
        .await
        .unwrap(),
        1,
        "the consumed ticket must be reaped before replacement is considered"
    );

    let before = live_ticket_device_snapshot(&server).await;
    let response = live_ticket_request(&server, reqwest::Method::POST, lifecycle_session(55)).await;
    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        bounded(response.json::<serde_json::Value>()).await.unwrap()["code"],
        "live_ticket_rate_limited"
    );
    assert_eq!(live_ticket_device_snapshot(&server).await, before);
    assert_eq!(
        live_ticket_session_state(&server, &consumed)
            .await
            .as_deref(),
        Some("open")
    );
    let reconnect_ticket = reconnect_ticket(&server, &consumed, &handle).await;
    let (_, reconnect_secret) = ticket_parts(&consumed, &reconnect_ticket);
    bounded(consume_live_ticket(
        &server.pool,
        session_bytes,
        reconnect_secret,
    ))
    .await
    .unwrap();
    let reconnect_connection_id: i64 = bounded(
        sqlx::query_scalar("SELECT active_connection_id FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3")
            .bind(server.vault)
            .bind(server.owner_device)
            .bind(Sha256::digest(session_bytes).to_vec())
            .fetch_one(&server.pool),
    )
    .await
    .unwrap();
    assert!(reconnect_connection_id > first_connection_id);
    server.shutdown().await;
}

#[tokio::test]
async fn readiness_loss_disconnects_established_socket_as_unavailable() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let mut socket = server.connect().await;
    expect_strict_hello(&mut socket).await;
    server.readiness_sender.send(false).unwrap();
    let close = next_close(&mut socket).await;
    assert_eq!(close.code, CloseCode::Error);
    assert_eq!(close.reason, "live_unavailable");
    server.shutdown().await;
}
