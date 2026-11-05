use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
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

#[derive(Clone)]
struct FakeLiveBackend {
    started: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
    release: Arc<Notify>,
    stall: bool,
}

impl FakeLiveBackend {
    fn immediate() -> Self {
        Self {
            started: Arc::new(AtomicUsize::new(0)),
            completed: Arc::new(AtomicUsize::new(0)),
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
            self.started.fetch_add(1, Ordering::SeqCst);
            if self.stall {
                self.release.notified().await;
            }
            self.completed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

struct LiveTestServer {
    pool: PgPool,
    admin: PgPool,
    schema: String,
    ws_url: String,
    task: JoinHandle<()>,
    _readiness_sender: watch::Sender<bool>,
    owner_token: String,
    owner_device: Uuid,
}

impl LiveTestServer {
    async fn start(backend: FakeLiveBackend, ready: bool) -> Self {
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
        let owner = create_owner(
            &pool,
            profile,
            vec![7, 8, 9],
            hex::encode(digest.finalize()),
            1,
        )
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
            None,
            None,
            TransportOptions::default(),
            None,
            PushConfigAdvertisement::default(),
            authorization,
            Some((Arc::new(backend), readiness)),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            pool,
            admin,
            schema,
            ws_url: format!("ws://{address}/v1/live"),
            task,
            _readiness_sender: readiness_sender,
            owner_token: owner.device_token,
            owner_device: owner.device_id,
        }
    }

    async fn connect(
        &self,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let mut request = self.ws_url.clone().into_client_request().unwrap();
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", self.owner_token).parse().unwrap(),
        );
        connect_async(request).await.unwrap().0
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
    let first = socket.next().await.unwrap().unwrap();
    let tungstenite::Message::Text(text) = first else {
        panic!("hello was not a text frame")
    };
    assert_eq!(
        serde_json::from_str::<LiveHelloV1>(&text).unwrap(),
        LiveHelloV1::new()
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        serde_json::json!({
            "type": "hello", "protocol_version": 1, "max_ttl_ms": 15000, "max_ciphertext_bytes": 5700
        })
    );
    server.shutdown().await;
}

#[tokio::test]
async fn sender_mismatch_closes_with_fixed_policy_reason() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let mut socket = server.connect().await;
    let _ = socket.next().await;
    socket
        .send(tungstenite::Message::Text(signal(Uuid::new_v4()).into()))
        .await
        .unwrap();
    let close = next_close(&mut socket).await;
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "invalid_live_frame");
    server.shutdown().await;
}

#[tokio::test]
async fn replaced_socket_with_inflight_output_cannot_finish_publish_after_takeover() {
    let backend = FakeLiveBackend::stalled();
    let server = LiveTestServer::start(backend.clone(), true).await;
    let mut replaced = server.connect().await;
    let _ = replaced.next().await;
    replaced
        .send(tungstenite::Message::Text(
            signal(server.owner_device).into(),
        ))
        .await
        .unwrap();
    wait_for_count(&backend.started, 1).await;

    let mut replacement = server.connect().await;
    let _ = replacement.next().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while replaced.next().await.is_some() {}
    })
    .await
    .expect("replaced socket remained active");
    backend.release.notify_waiters();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(backend.completed.load(Ordering::SeqCst), 0);
    server.shutdown().await;
}

#[tokio::test]
async fn stalled_backend_publish_is_cancelled_within_a_bounded_takeover_window() {
    let backend = FakeLiveBackend::stalled();
    let server = LiveTestServer::start(backend.clone(), true).await;
    let mut first = server.connect().await;
    let _ = first.next().await;
    first
        .send(tungstenite::Message::Text(
            signal(server.owner_device).into(),
        ))
        .await
        .unwrap();
    wait_for_count(&backend.started, 1).await;
    let started = tokio::time::Instant::now();
    let mut replacement = server.connect().await;
    let _ = replacement.next().await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while first.next().await.is_some() {}
    })
    .await
    .expect("stalled publish ignored cancellation");
    assert!(started.elapsed() < Duration::from_secs(2));
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
async fn oversized_application_frame_closes_with_fixed_policy_reason() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let mut socket = server.connect().await;
    let _ = socket.next().await;
    socket
        .send(tungstenite::Message::Text("x".repeat(8_193).into()))
        .await
        .unwrap();
    let close = next_close(&mut socket).await;
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "invalid_live_frame");
    server.shutdown().await;
}

#[tokio::test]
async fn invalid_utf8_application_frame_closes_with_fixed_policy_reason() {
    use tokio::io::AsyncWriteExt;

    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let mut socket = server.connect().await;
    let _ = socket.next().await;
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
    let _ = socket.next().await;
    server._readiness_sender.send(false).unwrap();
    let close = next_close(&mut socket).await;
    assert_eq!(close.code, CloseCode::Error);
    assert_eq!(close.reason, "live_unavailable");
    server.shutdown().await;
}
