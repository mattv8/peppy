use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use peppy_domain::VaultId;
use peppy_protocol::{EphemeralSignalV1, LiveHelloV1};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{
    net::TcpListener,
    sync::{Notify, watch},
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
        _frame: EphemeralSignalV1,
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
    _readiness_sender: watch::Sender<bool>,
    owner_token: String,
    owner_device: Uuid,
    vault: Uuid,
    peer_token: String,
    peer_device: Uuid,
}

impl LiveTestServer {
    async fn start(backend: FakeLiveBackend, ready: bool) -> Self {
        Self::start_with_configuration(backend, ready, LIVE_PUBLISH_TIMEOUT).await
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
        let authorization = RouterAuthorization {
            policy: Arc::new(CommunityAccessPolicy),
            enrollment: Arc::new(CommunityEnrollmentAuthorization::new(pool.clone())),
            claim_policy: crate::instance::configured_claim_policy(false).unwrap(),
            row_security: false,
            rls_enforced: false,
        };
        let (router, _maintenance) = build_router_with_live_backend(
            pool.clone(),
            RouterBuildOptions {
                config: None,
                initialized_storage: None,
                transport: TransportOptions::default(),
                explicit_relay_url: None,
                push_advertisement: PushConfigAdvertisement::default(),
            },
            authorization,
            Some((Arc::new(backend), readiness, publish_timeout)),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            pool,
            admin,
            schema,
            base_url: format!("http://{address}"),
            ws_url: format!("ws://{address}/v1/live"),
            task,
            _readiness_sender: readiness_sender,
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
        self.task.abort();
        let _ = self.task.await;
        self.pool.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}

fn signal(sender: Uuid) -> String {
    signal_with_ciphertext(sender, "AQ==")
}

fn signal_with_ciphertext(sender: Uuid, ciphertext_b64: &str) -> String {
    let now = live_now_ms();
    serde_json::json!({
        "protocol_version": 1,
        "kind": "typing",
        "sender_device_id": sender,
        "key_epoch": 1,
        "sent_at_ms": now.to_string(),
        "expires_at_ms": (now + 15_000).to_string(),
        "sequence": "1",
        "ciphertext_b64": ciphertext_b64
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
        serde_json::from_str::<LiveHelloV1>(&text).unwrap(),
        LiveHelloV1::new()
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        serde_json::json!({
            "type": "hello",
            "protocol_version": 1,
            "max_ttl_ms": 15000,
            "max_ciphertext_bytes": 5700
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
    let frame = signal(server.owner_device);
    sender
        .send(tungstenite::Message::Text(frame.clone().into()))
        .await
        .unwrap();
    let received = next_text(&mut peer).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&received).unwrap(),
        serde_json::from_str::<serde_json::Value>(&frame).unwrap()
    );
    wait_for_count(&backend.completed, 1).await;

    let after = server.snapshot().await;
    // Snapshot start has no clock, connection, or request fields; every field is
    // durable vault state or a protocol capability, so exact equality is stable.
    assert_eq!(after, before);
    server.shutdown().await;
}

#[tokio::test]
async fn live_publish_receive_invalid_and_error_paths_do_not_log_sensitive_data() {
    const CIPHERTEXT_CANARY: &str = "U0lHTkFMX0NBTkFSWV9OT19MT0c=";
    const PLAINTEXT_CANARY: &str = "SIGNAL_CANARY_NO_LOG";

    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .without_time()
        .with_writer(logs.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("live tracing test must install the process collector");

    let backend = FakeLiveBackend::immediate();
    let server = LiveTestServer::start(backend.clone(), true).await;
    let mut sender = server.connect().await;
    expect_strict_hello(&mut sender).await;
    let mut peer = server.connect_peer().await;
    expect_strict_hello(&mut peer).await;

    let canary_frame = signal_with_ciphertext(server.owner_device, CIPHERTEXT_CANARY);
    sender
        .send(tungstenite::Message::Text(canary_frame.clone().into()))
        .await
        .unwrap();
    let received = next_text(&mut peer).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&received).unwrap(),
        serde_json::from_str::<serde_json::Value>(&canary_frame).unwrap()
    );

    sender
        .send(tungstenite::Message::Text(signal(Uuid::new_v4()).into()))
        .await
        .unwrap();
    let invalid_close = next_close(&mut sender).await;
    assert_eq!(invalid_close.reason, "invalid_live_frame");

    backend.fail.store(true, Ordering::SeqCst);
    peer.send(tungstenite::Message::Text(
        signal(server.peer_device).into(),
    ))
    .await
    .unwrap();
    let error_close = next_close(&mut peer).await;
    assert_eq!(error_close.reason, "live_unavailable");

    let captured = logs.text();
    assert!(captured.contains(TRACE_COLLECTOR_SENTINEL));
    let vault_id = server.vault.to_string();
    let owner_device_id = server.owner_device.to_string();
    let peer_device_id = server.peer_device.to_string();
    for secret in [
        CIPHERTEXT_CANARY,
        PLAINTEXT_CANARY,
        canary_frame.as_str(),
        vault_id.as_str(),
        owner_device_id.as_str(),
        peer_device_id.as_str(),
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
