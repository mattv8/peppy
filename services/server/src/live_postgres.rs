use crate::live::{LiveBackend, LiveBackendError, LiveHub};
use peppy_domain::{DeviceId, VaultId};
use peppy_protocol::EphemeralSignalV2;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, postgres::PgListener};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::task::JoinHandle;
use uuid::Uuid;

const LIVE_NOTIFY_CHANNEL: &str = "peppy_live_v2";
const MAX_NOTIFY_PAYLOAD_BYTES: usize = 8_000;
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);
const LISTEN_SETUP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Notify {
    instance_id: Uuid,
    vault_id: Uuid,
    frame: EphemeralSignalV2,
}

struct Inner {
    pool: PgPool,
    listener_pool: PgPool,
    instance_id: Uuid,
    hub: LiveHub,
    ready: AtomicBool,
    readiness: tokio::sync::watch::Sender<bool>,
    listen_setup_timeout: Duration,
}

/// PostgreSQL `LISTEN`/`NOTIFY` relay for best-effort live frames. Notifications
/// are intentionally not durable and therefore are never replayed after a reconnect.
pub(crate) struct PgLiveBackend {
    inner: Arc<Inner>,
    listener_task: JoinHandle<()>,
}

impl PgLiveBackend {
    pub(crate) fn new(pool: PgPool, hub: LiveHub) -> Self {
        Self::new_with_setup_timeout(pool, hub, LISTEN_SETUP_TIMEOUT)
    }

    fn new_with_setup_timeout(pool: PgPool, hub: LiveHub, listen_setup_timeout: Duration) -> Self {
        let (readiness, _) = tokio::sync::watch::channel(false);
        let listener_pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .min_connections(0)
            .acquire_timeout(LISTEN_SETUP_TIMEOUT)
            .connect_lazy_with((*pool.connect_options()).clone());
        let inner = Arc::new(Inner {
            pool,
            listener_pool,
            instance_id: Uuid::new_v4(),
            hub,
            ready: AtomicBool::new(false),
            readiness,
            listen_setup_timeout,
        });
        let listener_task = tokio::spawn(listener_loop(Arc::downgrade(&inner)));
        Self {
            inner,
            listener_task,
        }
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.inner.ready.load(Ordering::Acquire)
    }

    /// A receiver changes whenever the LISTEN registration becomes available or
    /// unavailable. Socket sessions use it to close instead of continuing to
    /// claim cross-replica delivery after the listener is lost.
    #[allow(dead_code)]
    pub(crate) fn subscribe_readiness(&self) -> tokio::sync::watch::Receiver<bool> {
        self.inner.readiness.subscribe()
    }

    async fn publish_inner(
        &self,
        vault_id: VaultId,
        frame: EphemeralSignalV2,
    ) -> Result<(), LiveBackendError> {
        if !self.inner.ready.load(Ordering::Acquire) {
            return Err(LiveBackendError);
        }
        let payload = serde_json::to_string(&Notify {
            instance_id: self.inner.instance_id,
            vault_id: vault_id.0,
            frame,
        })
        .map_err(|_| LiveBackendError)?;
        if payload.len() >= MAX_NOTIFY_PAYLOAD_BYTES {
            return Err(LiveBackendError);
        }
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(LIVE_NOTIFY_CHANNEL)
            .bind(payload)
            .execute(&self.inner.pool)
            .await
            .map_err(|_| LiveBackendError)?;
        Ok(())
    }
}

impl Drop for PgLiveBackend {
    fn drop(&mut self) {
        set_ready(&self.inner, false);
        self.listener_task.abort();
    }
}

impl LiveBackend for PgLiveBackend {
    fn publish(
        &self,
        vault_id: VaultId,
        frame: EphemeralSignalV2,
    ) -> Pin<Box<dyn Future<Output = Result<(), LiveBackendError>> + Send + '_>> {
        Box::pin(self.publish_inner(vault_id, frame))
    }
}

async fn listener_loop(inner: Weak<Inner>) {
    let mut delay = Duration::from_secs(1);
    loop {
        let Some(state) = inner.upgrade() else {
            return;
        };
        set_ready(&state, false);
        let result = listen_once(&state).await;
        set_ready(&state, false);
        drop(state);
        if result.is_ok() {
            delay = Duration::from_secs(1);
        }
        tokio::time::sleep(delay).await;
        delay = delay.saturating_mul(2).min(MAX_RECONNECT_DELAY);
    }
}

async fn listen_once(state: &Inner) -> Result<(), sqlx::Error> {
    let mut listener = setup_listener(&state.listener_pool, state.listen_setup_timeout).await?;
    set_ready(state, true);
    loop {
        let Some(notification) = listener.try_recv().await? else {
            return Ok(());
        };
        let Ok(payload) = serde_json::from_str::<Notify>(notification.payload()) else {
            continue;
        };
        if payload.instance_id == state.instance_id {
            continue;
        }
        let _ = state.hub.forward(
            VaultId(payload.vault_id),
            DeviceId(payload.frame.sender_device_id.0),
            payload.frame,
            live_now_ms(),
        );
    }
}

async fn setup_listener(pool: &PgPool, timeout: Duration) -> Result<PgListener, sqlx::Error> {
    within_setup_deadline(timeout, async {
        let mut listener = PgListener::connect_with(pool).await?;
        listener.discard_on_drop();
        listener.eager_reconnect(false);
        listener.listen(LIVE_NOTIFY_CHANNEL).await?;
        Ok(listener)
    })
    .await
}

async fn within_setup_deadline<T>(
    timeout: Duration,
    setup: impl Future<Output = Result<T, sqlx::Error>>,
) -> Result<T, sqlx::Error> {
    tokio::time::timeout(timeout, setup)
        .await
        .map_err(|_| sqlx::Error::PoolTimedOut)?
}

fn set_ready(state: &Inner, ready: bool) {
    state.ready.store(ready, Ordering::Release);
    state.readiness.send_replace(ready);
}

fn live_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use futures_util::FutureExt;
    use peppy_protocol::EphemeralSignalV2Kind;
    use std::{str::FromStr, sync::Arc};
    use tokio::sync::mpsc::error::TryRecvError;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::{Notify as Signal, oneshot},
    };
    use url::Url;

    fn frame(sender_device_id: DeviceId, expires_at_ms: u64) -> EphemeralSignalV2 {
        frame_at(sender_device_id, live_now_ms(), expires_at_ms)
    }

    fn frame_at(
        sender_device_id: DeviceId,
        sent_at_ms: u64,
        expires_at_ms: u64,
    ) -> EphemeralSignalV2 {
        EphemeralSignalV2 {
            protocol_version: 2,
            kind: EphemeralSignalV2Kind::Typing,
            sender_device_id,
            key_epoch: 1,
            session_id: "AAAAAAAAAAAAAAAAAAAAAA".into(),
            sent_at_ms: sent_at_ms.to_string(),
            expires_at_ms: expires_at_ms.to_string(),
            sequence: "1".into(),
            ciphertext_b64: STANDARD.encode([0_u8; 32]),
        }
    }

    fn maximum_notify_payload(kind: EphemeralSignalV2Kind, ciphertext_bytes: usize) -> String {
        serde_json::to_string(&Notify {
            instance_id: Uuid::from_u128(u128::MAX),
            vault_id: Uuid::from_u128(u128::MAX),
            frame: EphemeralSignalV2 {
                protocol_version: 2,
                kind,
                sender_device_id: DeviceId(Uuid::from_u128(u128::MAX)),
                key_epoch: u32::MAX,
                session_id: "_____________________w".into(),
                sent_at_ms: u64::MAX.to_string(),
                expires_at_ms: u64::MAX.to_string(),
                sequence: u64::MAX.to_string(),
                ciphertext_b64: STANDARD.encode(vec![0_u8; ciphertext_bytes]),
            },
        })
        .unwrap()
    }

    #[test]
    fn v2_notify_payload_maxima_include_the_real_relay_wrapper() {
        use peppy_protocol::{
            MAX_EPHEMERAL_V2_CONTROL_CIPHERTEXT_BYTES, MAX_EPHEMERAL_V2_TYPING_CIPHERTEXT_BYTES,
        };

        let cases = [
            (
                EphemeralSignalV2Kind::Typing,
                MAX_EPHEMERAL_V2_TYPING_CIPHERTEXT_BYTES,
                7_599,
            ),
            (
                EphemeralSignalV2Kind::Challenge,
                MAX_EPHEMERAL_V2_CONTROL_CIPHERTEXT_BYTES,
                1_086,
            ),
            (
                EphemeralSignalV2Kind::SessionAck,
                MAX_EPHEMERAL_V2_CONTROL_CIPHERTEXT_BYTES,
                1_088,
            ),
        ];

        for (kind, ciphertext_bytes, expected_payload_bytes) in cases {
            let payload = maximum_notify_payload(kind, ciphertext_bytes);
            assert_eq!(payload.len(), expected_payload_bytes, "{kind:?}");
            assert!(payload.len() < MAX_NOTIFY_PAYLOAD_BYTES, "{kind:?}");
        }
    }

    async fn wait_ready(backend: &PgLiveBackend) {
        let mut readiness = backend.subscribe_readiness();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !*readiness.borrow_and_update() {
                readiness
                    .changed()
                    .await
                    .expect("backend should remain alive");
            }
        })
        .await
        .expect("listener should register");
    }

    async fn retained_counts(pool: &PgPool) -> (i64, i64, i64) {
        (
            sqlx::query_scalar("SELECT count(*) FROM event_log")
                .fetch_one(pool)
                .await
                .unwrap(),
            sqlx::query_scalar("SELECT count(*) FROM encrypted_records")
                .fetch_one(pool)
                .await
                .unwrap(),
            sqlx::query_scalar("SELECT count(*) FROM device_wake_jobs")
                .fetch_one(pool)
                .await
                .unwrap(),
        )
    }

    async fn stalled_listen_proxy(database_url: &str) -> (String, oneshot::Receiver<()>) {
        let upstream = Url::parse(database_url).unwrap();
        let upstream_address = format!(
            "{}:{}",
            upstream.host_str().unwrap(),
            upstream.port().unwrap_or(5432)
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = listener.local_addr().unwrap();
        let mut proxy_url = upstream;
        proxy_url.set_host(Some("127.0.0.1")).unwrap();
        proxy_url.set_port(Some(proxy_address.port())).unwrap();
        let (disposed_tx, disposed_rx) = oneshot::channel();

        tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            let upstream = TcpStream::connect(upstream_address).await.unwrap();
            let (mut client_read, mut client_write) = client.into_split();
            let (mut upstream_read, mut upstream_write) = upstream.into_split();
            let listen_sent = Arc::new(Signal::new());
            let listen_sent_for_client = listen_sent.clone();
            let client_done = tokio::spawn(async move {
                let length = client_read.read_u32().await.unwrap() as usize;
                let mut startup = vec![0; length - 4];
                client_read.read_exact(&mut startup).await.unwrap();
                upstream_write.write_u32(length as u32).await.unwrap();
                upstream_write.write_all(&startup).await.unwrap();
                loop {
                    let kind = match client_read.read_u8().await {
                        Ok(kind) => kind,
                        Err(_) => return,
                    };
                    let length = client_read.read_u32().await.unwrap() as usize;
                    let mut body = vec![0; length - 4];
                    client_read.read_exact(&mut body).await.unwrap();
                    if kind == b'Q' && body.starts_with(b"LISTEN ") {
                        listen_sent_for_client.notify_one();
                    }
                    upstream_write.write_u8(kind).await.unwrap();
                    upstream_write.write_u32(length as u32).await.unwrap();
                    upstream_write.write_all(&body).await.unwrap();
                }
            });

            loop {
                let kind = upstream_read.read_u8().await.unwrap();
                let length = upstream_read.read_u32().await.unwrap() as usize;
                let mut body = vec![0; length - 4];
                upstream_read.read_exact(&mut body).await.unwrap();
                if kind == b'Z' && listen_sent.notified().now_or_never().is_some() {
                    let _ = client_done.await;
                    let _ = disposed_tx.send(());
                    return;
                }
                client_write.write_u8(kind).await.unwrap();
                client_write.write_u32(length as u32).await.unwrap();
                client_write.write_all(&body).await.unwrap();
            }
        });

        (proxy_url.into(), disposed_rx)
    }

    #[tokio::test]
    async fn timed_out_listen_setup_discards_its_stalled_pool_connection() {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL is required for PostgreSQL live relay tests");
        let (proxy_url, disposed) = stalled_listen_proxy(&database_url).await;
        let options = sqlx::postgres::PgConnectOptions::from_str(&proxy_url)
            .unwrap()
            .ssl_mode(sqlx::postgres::PgSslMode::Disable);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .min_connections(0)
            .connect_lazy_with(options);

        assert!(matches!(
            setup_listener(&pool, Duration::from_millis(100)).await,
            Err(sqlx::Error::PoolTimedOut)
        ));
        tokio::time::timeout(Duration::from_secs(1), disposed)
            .await
            .expect("listener drop must close the stalled proxy connection")
            .expect("proxy must observe the listener connection closing");
        tokio::time::timeout(Duration::from_secs(1), async {
            while pool.size() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("discarded listener must free the only listener-pool slot");
    }

    #[tokio::test]
    async fn postgres_notification_delivers_once_to_another_replica_without_replay() {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL is required for PostgreSQL live relay tests");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect(&database_url)
            .await
            .unwrap();
        crate::MIGRATOR.run(&pool).await.unwrap();
        let retained_before = retained_counts(&pool).await;
        let vault_id = VaultId(Uuid::from_u128(1));
        let foreign_vault_id = VaultId(Uuid::from_u128(2));
        let sender_device_id = DeviceId(Uuid::from_u128(3));
        let receiver_device_id = DeviceId(Uuid::from_u128(4));
        let local_hub = LiveHub::new();
        let remote_hub = LiveHub::new();
        let local_backend = PgLiveBackend::new(pool.clone(), local_hub.clone());
        let remote_backend = PgLiveBackend::new(pool.clone(), remote_hub.clone());
        wait_ready(&local_backend).await;
        wait_ready(&remote_backend).await;

        let mut local_receiver = local_hub.register(vault_id, receiver_device_id).receiver;
        let mut remote_receiver = remote_hub.register(vault_id, receiver_device_id).receiver;
        let mut foreign_receiver = remote_hub
            .register(foreign_vault_id, receiver_device_id)
            .receiver;
        let valid = frame(sender_device_id, live_now_ms() + 10_000);

        local_backend
            .publish(vault_id, valid.clone())
            .await
            .unwrap();
        assert_eq!(
            local_hub.forward(vault_id, sender_device_id, valid.clone(), live_now_ms()),
            Ok(1)
        );
        assert_eq!(local_receiver.try_recv(), Ok(valid.clone()));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), remote_receiver.recv())
                .await
                .unwrap(),
            Some(valid)
        );
        assert_eq!(local_receiver.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(foreign_receiver.try_recv(), Err(TryRecvError::Empty));

        for (kind, ciphertext) in [
            (EphemeralSignalV2Kind::Challenge, [2_u8; 32]),
            (EphemeralSignalV2Kind::SessionAck, [3_u8; 32]),
        ] {
            let mut control = frame(sender_device_id, live_now_ms() + 10_000);
            control.kind = kind;
            control.ciphertext_b64 = STANDARD.encode(ciphertext);
            local_backend
                .publish(vault_id, control.clone())
                .await
                .unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), remote_receiver.recv())
                    .await
                    .unwrap(),
                Some(control)
            );
            assert_eq!(local_receiver.try_recv(), Err(TryRecvError::Empty));
            assert_eq!(foreign_receiver.try_recv(), Err(TryRecvError::Empty));
        }

        let now = live_now_ms();
        let expired = frame_at(sender_device_id, now - 2_000, now - 1_000);
        assert!(expired.validate().is_ok());
        local_backend.publish(vault_id, expired).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), remote_receiver.recv())
                .await
                .is_err()
        );

        let future = frame_at(sender_device_id, now + 31_000, now + 32_000);
        assert!(future.validate().is_ok());
        local_backend.publish(vault_id, future).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), remote_receiver.recv())
                .await
                .is_err()
        );

        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(LIVE_NOTIFY_CHANNEL)
            .bind("{\"unexpected\":true}")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), remote_receiver.recv())
                .await
                .is_err()
        );

        let mut oversized = frame(sender_device_id, live_now_ms() + 10_000);
        oversized.ciphertext_b64.clear();
        let wrapper_bytes = serde_json::to_string(&Notify {
            instance_id: local_backend.inner.instance_id,
            vault_id: vault_id.0,
            frame: oversized.clone(),
        })
        .unwrap()
        .len();
        oversized.ciphertext_b64 = "A".repeat(MAX_NOTIFY_PAYLOAD_BYTES - wrapper_bytes);
        for expected_bytes in [MAX_NOTIFY_PAYLOAD_BYTES, MAX_NOTIFY_PAYLOAD_BYTES + 1] {
            let serialized_bytes = serde_json::to_string(&Notify {
                instance_id: local_backend.inner.instance_id,
                vault_id: vault_id.0,
                frame: oversized.clone(),
            })
            .unwrap()
            .len();
            assert_eq!(serialized_bytes, expected_bytes);
            assert_eq!(
                local_backend.publish(vault_id, oversized.clone()).await,
                Err(LiveBackendError)
            );
            oversized.ciphertext_b64.push('A');
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), remote_receiver.recv())
                .await
                .is_err()
        );

        drop(remote_backend);
        let remote_reconnected = PgLiveBackend::new(pool.clone(), remote_hub.clone());
        wait_ready(&remote_reconnected).await;
        let mut no_replay = remote_hub.register(vault_id, receiver_device_id);
        assert_eq!(no_replay.receiver.try_recv(), Err(TryRecvError::Empty));
        let next = frame(sender_device_id, live_now_ms() + 10_000);
        local_backend.publish(vault_id, next.clone()).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), no_replay.receiver.recv())
                .await
                .unwrap(),
            Some(next)
        );
        assert_eq!(retained_counts(&pool).await, retained_before);
    }

    #[tokio::test]
    async fn listener_loss_and_backend_drop_mark_readiness_unavailable() {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL is required for PostgreSQL live relay tests");
        let application_name = format!("peppy-live-test-{}", Uuid::new_v4());
        let options = sqlx::postgres::PgConnectOptions::from_str(&database_url)
            .unwrap()
            .application_name(&application_name);
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        let backend = PgLiveBackend::new(pool.clone(), LiveHub::new());
        wait_ready(&backend).await;
        let mut readiness = backend.subscribe_readiness();
        assert_eq!(backend.inner.listener_pool.size(), 1);
        let app_pool_available: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(app_pool_available, 1);

        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .unwrap();
        sqlx::query(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name=$1",
        )
        .bind(&application_name)
        .execute(&admin)
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while *readiness.borrow_and_update() {
                readiness
                    .changed()
                    .await
                    .expect("backend should remain alive");
            }
        })
        .await
        .expect("terminated listener marks backend unavailable before reconnect");
        assert!(!backend.is_ready());
        assert!(backend.inner.listener_pool.size() <= 1);
        let app_pool_available: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(app_pool_available, 1);

        tokio::time::timeout(Duration::from_secs(2), pool.close())
            .await
            .expect("pool closes listener connection");

        drop(backend);
        assert!(!*readiness.borrow_and_update());
    }

    #[tokio::test]
    async fn stalled_subscription_setup_reaches_the_configured_deadline() {
        let result = within_setup_deadline(
            Duration::from_millis(20),
            std::future::pending::<Result<(), sqlx::Error>>(),
        )
        .await;
        assert!(matches!(result, Err(sqlx::Error::PoolTimedOut)));
    }
}
