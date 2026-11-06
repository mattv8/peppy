use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
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

struct LiveTestServer {
    pool: PgPool,
    admin: PgPool,
    schema: String,
    base_url: String,
    ws_url: String,
    task: JoinHandle<()>,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    _readiness_sender: watch::Sender<bool>,
    live_hub: crate::live::LiveHub,
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

impl LiveTestServer {
    async fn start(backend: FakeLiveBackend, ready: bool) -> Self {
        Self::start_with_trusted_origin(backend, ready, LIVE_PUBLISH_TIMEOUT, true).await
    }

    async fn start_without_trusted_origin(backend: FakeLiveBackend, ready: bool) -> Self {
        Self::start_with_trusted_origin(backend, ready, LIVE_PUBLISH_TIMEOUT, false).await
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
        Self::start_with_trusted_origin(backend, ready, publish_timeout, true).await
    }

    async fn start_with_trusted_origin(
        backend: FakeLiveBackend,
        ready: bool,
        publish_timeout: Duration,
        trusted_origin: bool,
    ) -> Self {
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
        let mut isolated_url: url::Url = database_url.parse().unwrap();
        isolated_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let pool = PgPoolOptions::new()
            .max_connections(5)
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
        let authorization = RouterAuthorization {
            policy: Arc::new(CommunityAccessPolicy),
            enrollment: Arc::new(CommunityEnrollmentAuthorization::new(pool.clone())),
            claim_policy: crate::instance::configured_claim_policy(false).unwrap(),
            row_security: false,
            rls_enforced: false,
        };
        let mut config = test_config(&database_url);
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
            Some((
                Arc::new(backend),
                readiness,
                publish_timeout,
                Some(live_hub_observer.clone()),
            )),
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
            _readiness_sender: readiness_sender,
            live_hub: live_hub_observer
                .lock()
                .unwrap()
                .clone()
                .expect("live test router must expose its hub"),
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
        let request = format!(
            "GET /v1/live HTTP/1.1\r\nHost: {address}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nOrigin: https://web.peppy.test\r\nSec-WebSocket-Protocol: {protocol}\r\nSec-WebSocket-Protocol: {protocol}\r\n\r\n"
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
    let ticket_digest: Vec<u8> = sqlx::query_scalar("SELECT ticket_digest FROM live_tickets")
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(
        ticket_digest,
        Sha256::digest(URL_SAFE_NO_PAD.decode(ticket).unwrap()).to_vec()
    );

    let closed = client
        .delete(format!("{}/v1/live/tickets", server.base_url))
        .bearer_auth(&server.owner_token)
        .json(&serde_json::json!({"session_id": session_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(closed.status(), reqwest::StatusCode::NO_CONTENT);
    let rejected = client
        .post(format!("{}/v1/live/tickets", server.base_url))
        .bearer_auth(&server.owner_token)
        .json(&serde_json::json!({"session_id": session_id}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        rejected.json::<serde_json::Value>().await.unwrap()["code"],
        "live_ticket_session_terminal"
    );
    server.shutdown().await;
}

async fn live_ticket_request(
    server: &LiveTestServer,
    method: reqwest::Method,
    session_id: String,
) -> reqwest::Response {
    reqwest::Client::new()
        .request(method, format!("{}/v1/live/tickets", server.base_url))
        .bearer_auth(&server.owner_token)
        .json(&serde_json::json!({"session_id": session_id}))
        .send()
        .await
        .unwrap()
}

async fn issue_ticket(server: &LiveTestServer, session_id: String) -> String {
    let response = live_ticket_request(server, reqwest::Method::POST, session_id)
        .await
        .error_for_status()
        .unwrap();
    response.json::<serde_json::Value>().await.unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_owned()
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
    let ticket = issue_ticket(&server, session_id.clone()).await;
    assert_eq!(
        live_ticket_request(&server, reqwest::Method::DELETE, session_id.clone())
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
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let (mut socket, _) = server.connect_with_ticket(&session_id, &ticket).await;
    expect_strict_hello(&mut socket).await;

    assert_eq!(
        live_ticket_request(&server, reqwest::Method::DELETE, session_id)
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
async fn stale_g1_recheck_cannot_renew_a_replaced_ticket_socket_lease() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session_id = URL_SAFE_NO_PAD.encode([42_u8; 16]);
    let ticket = issue_ticket(&server, session_id.clone()).await;
    let (session, _) = ticket_parts(&session_id, &ticket);
    let g1 = server.live_hub.register_ticket(
        VaultId(server.vault),
        DeviceId(server.owner_device),
        session,
    );
    let g2 = server.live_hub.register_ticket(
        VaultId(server.vault),
        DeviceId(server.owner_device),
        session,
    );
    let principal = Principal {
        vault: server.vault,
        device: server.owner_device,
        role: "owner".into(),
        token_digest: Vec::new(),
    };
    let hash = Sha256::digest(session).to_vec();
    let before: i64 = sqlx::query_scalar("SELECT (extract(epoch FROM lease_until)*1000000)::bigint FROM live_ticket_sessions WHERE session_id_hash=$1")
        .bind(&hash)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert!(
        !renew_live_ticket_lease(
            &server.pool,
            &server.live_hub,
            &principal,
            session,
            g1.generation
        )
        .await
        .unwrap()
    );
    let stale_after: i64 = sqlx::query_scalar("SELECT (extract(epoch FROM lease_until)*1000000)::bigint FROM live_ticket_sessions WHERE session_id_hash=$1")
        .bind(&hash)
        .fetch_one(&server.pool)
        .await
        .unwrap();
    assert_eq!(stale_after, before);
    assert!(
        renew_live_ticket_lease(
            &server.pool,
            &server.live_hub,
            &principal,
            session,
            g2.generation
        )
        .await
        .unwrap()
    );
    server.shutdown().await;
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
    assert_eq!(
        live_ticket_request(&server, reqwest::Method::DELETE, recent_tombstone.clone())
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let old_tombstone = URL_SAFE_NO_PAD.encode([45_u8; 16]);
    assert_eq!(
        live_ticket_request(&server, reqwest::Method::DELETE, old_tombstone.clone())
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
    let ticket = issue_ticket(&server, session_id.clone()).await;
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

    let replacement_ticket = issue_ticket(&server, session_id.clone()).await;
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
    let ticket = issued.json::<serde_json::Value>().await.unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_owned();
    let protocol = live_ticket_protocol(&session_id, &ticket);
    assert_eq!(
        live_ticket_request(&server, reqwest::Method::DELETE, session_id)
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );

    let captured = logs.text();
    assert!(captured.contains(SAFE_MARKER));
    for secret in [&ticket, &protocol, &server.owner_token] {
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
    let replay_ticket = issue_ticket(&server, replay_session.clone()).await;
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
    let origin_ticket = issue_ticket(&server, origin_session.clone()).await;
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
    assert_eq!(principal.vault, server.vault);
    assert_eq!(principal.device, server.owner_device);
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
    let ticket = issue_ticket(&server, session_id.clone()).await;
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
    let request = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("{base_url}/v1/live/tickets"))
            .bearer_auth(owner_token)
            .json(&serde_json::json!({"session_id": blocked_session}))
            .send()
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(250)).await;
    device_lock.commit().await.unwrap();

    let rejected = request.await.unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        rejected.json::<serde_json::Value>().await.unwrap()["code"],
        "live_ticket_session_terminal"
    );
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
async fn live_ticket_delete_before_issue_is_terminal_and_tickets_are_not_stored_raw() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let closed_id = URL_SAFE_NO_PAD.encode([9_u8; 16]);
    assert_eq!(
        live_ticket_request(&server, reqwest::Method::DELETE, closed_id.clone())
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let rejected = live_ticket_request(&server, reqwest::Method::POST, closed_id).await;
    assert_eq!(rejected.status(), reqwest::StatusCode::CONFLICT);

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
async fn live_ticket_caps_bound_fresh_session_creation_and_tombstones() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let first = URL_SAFE_NO_PAD.encode([11_u8; 16]);
    assert_eq!(
        live_ticket_request(&server, reqwest::Method::POST, first.clone())
            .await
            .status(),
        reqwest::StatusCode::CREATED
    );
    assert_eq!(
        live_ticket_request(&server, reqwest::Method::POST, first.clone())
            .await
            .status(),
        reqwest::StatusCode::CREATED
    );
    assert_eq!(
        live_ticket_request(&server, reqwest::Method::POST, first.clone())
            .await
            .status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        live_ticket_request(
            &server,
            reqwest::Method::POST,
            URL_SAFE_NO_PAD.encode([12_u8; 16])
        )
        .await
        .status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        live_ticket_request(&server, reqwest::Method::DELETE, first)
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );

    for byte in 13_u8..18 {
        assert_eq!(
            live_ticket_request(
                &server,
                reqwest::Method::DELETE,
                URL_SAFE_NO_PAD.encode([byte; 16])
            )
            .await
            .status(),
            reqwest::StatusCode::NO_CONTENT
        );
    }
    assert_eq!(
        live_ticket_request(
            &server,
            reqwest::Method::DELETE,
            URL_SAFE_NO_PAD.encode([18_u8; 16])
        )
        .await
        .status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    let sessions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert!(sessions <= 6);
    assert_eq!(
        live_ticket_request(
            &server,
            reqwest::Method::DELETE,
            URL_SAFE_NO_PAD.encode([11_u8; 16])
        )
        .await
        .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    server.shutdown().await;
}

#[tokio::test]
async fn live_ticket_delete_flood_is_admission_bounded_and_reaper_preserves_recent_rows() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let existing_session = URL_SAFE_NO_PAD.encode([61_u8; 16]);
    let live_session = URL_SAFE_NO_PAD.encode([68_u8; 16]);
    assert_eq!(
        live_ticket_request(&server, reqwest::Method::POST, existing_session.clone())
            .await
            .status(),
        reqwest::StatusCode::CREATED
    );
    let absent_sessions: Vec<_> = (62_u8..67)
        .map(|byte| URL_SAFE_NO_PAD.encode([byte; 16]))
        .collect();
    for session_id in &absent_sessions {
        assert_eq!(
            live_ticket_request(&server, reqwest::Method::DELETE, session_id.clone())
                .await
                .status(),
            reqwest::StatusCode::NO_CONTENT
        );
    }
    let tombstones: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND state='closed'",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(tombstones, absent_sessions.len() as i64);

    let rows_before_rejection: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(
        live_ticket_request(
            &server,
            reqwest::Method::DELETE,
            URL_SAFE_NO_PAD.encode([67_u8; 16]),
        )
        .await
        .status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    let rows_after_rejection: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(rows_after_rejection, rows_before_rejection);

    assert_eq!(
        live_ticket_request(&server, reqwest::Method::DELETE, existing_session.clone())
            .await
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    sqlx::query("INSERT INTO live_ticket_sessions(vault_id,device_id,session_id_hash,state,lease_until) VALUES($1,$2,$3,'open',now()+interval '1 hour')")
        .bind(server.vault)
        .bind(server.owner_device)
        .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(&live_session).unwrap()).to_vec())
        .execute(&server.pool)
        .await
        .unwrap();
    for session_id in &absent_sessions {
        sqlx::query("UPDATE live_ticket_sessions SET closed_at=now()-interval '6 minutes' WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3")
            .bind(server.vault)
            .bind(server.owner_device)
            .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(session_id).unwrap()).to_vec())
            .execute(&server.pool)
            .await
            .unwrap();
    }

    assert_eq!(
        crate::live_ticket_maintenance::reap_live_tickets(&server.pool, 100)
            .await
            .unwrap(),
        absent_sessions.len() as u64
    );
    let old_tombstones: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND state='closed' AND closed_at<now()-interval '5 minutes'",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(old_tombstones, 0);
    for session_id in [&existing_session, &live_session] {
        let retained: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3",
        )
        .bind(server.vault)
        .bind(server.owner_device)
        .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(session_id).unwrap()).to_vec())
        .fetch_one(&server.pool)
        .await
        .unwrap();
        assert_eq!(retained, 1);
    }
    let unexpired_live_sessions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3 AND state='open' AND lease_until>now()",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .bind(Sha256::digest(URL_SAFE_NO_PAD.decode(live_session).unwrap()).to_vec())
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(unexpired_live_sessions, 1);
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
async fn live_ticket_issue_delete_race_never_leaves_an_open_session_after_delete() {
    for value in 0_u8..20 {
        let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
        let replica_one = live_ticket_replica_pool(&server.schema).await;
        let replica_two = live_ticket_replica_pool(&server.schema).await;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let session = URL_SAFE_NO_PAD.encode([value; 16]);
        let post = async {
            barrier.wait().await;
            live_ticket_request(&server, reqwest::Method::POST, session.clone())
                .await
                .status()
        };
        let delete = async {
            barrier.wait().await;
            live_ticket_request(&server, reqwest::Method::DELETE, session.clone())
                .await
                .status()
        };
        let (_post, deleted) = tokio::join!(post, delete);
        assert_eq!(deleted, reqwest::StatusCode::NO_CONTENT);
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

#[tokio::test]
async fn readiness_loss_disconnects_established_socket_as_unavailable() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let mut socket = server.connect().await;
    expect_strict_hello(&mut socket).await;
    server._readiness_sender.send(false).unwrap();
    let close = next_close(&mut socket).await;
    assert_eq!(close.code, CloseCode::Error);
    assert_eq!(close.reason, "live_unavailable");
    server.shutdown().await;
}
