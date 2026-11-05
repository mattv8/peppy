use crate::live::{LiveBackend, LiveBackendError, LiveHub};
use peppy_domain::{DeviceId, VaultId};
use peppy_protocol::EphemeralSignalV1;
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

const LIVE_NOTIFY_CHANNEL: &str = "peppy_live_v1";
const MAX_NOTIFY_PAYLOAD_BYTES: usize = 8_000;
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Notify {
    instance_id: Uuid,
    vault_id: Uuid,
    frame: EphemeralSignalV1,
}

struct Inner {
    pool: PgPool,
    instance_id: Uuid,
    hub: LiveHub,
    ready: AtomicBool,
    readiness: tokio::sync::watch::Sender<bool>,
}

/// PostgreSQL `LISTEN`/`NOTIFY` relay for best-effort live frames. Notifications
/// are intentionally not durable and therefore are never replayed after a reconnect.
pub(crate) struct PgLiveBackend {
    inner: Arc<Inner>,
    listener_task: JoinHandle<()>,
}

impl PgLiveBackend {
    pub(crate) fn new(pool: PgPool, hub: LiveHub) -> Self {
        let (readiness, _) = tokio::sync::watch::channel(false);
        let inner = Arc::new(Inner {
            pool,
            instance_id: Uuid::new_v4(),
            hub,
            ready: AtomicBool::new(false),
            readiness,
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
        frame: EphemeralSignalV1,
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
        frame: EphemeralSignalV1,
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
    let mut listener = PgListener::connect_with(&state.pool).await?;
    listener.listen(LIVE_NOTIFY_CHANNEL).await?;
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
    use tokio::sync::mpsc::error::TryRecvError;

    fn frame(sender_device_id: DeviceId, expires_at_ms: u64) -> EphemeralSignalV1 {
        EphemeralSignalV1 {
            protocol_version: 1,
            kind: "typing".into(),
            sender_device_id,
            key_epoch: 1,
            sent_at_ms: live_now_ms().to_string(),
            expires_at_ms: expires_at_ms.to_string(),
            sequence: "1".into(),
            ciphertext_b64: STANDARD.encode([0_u8; 32]),
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

    #[tokio::test]
    async fn postgres_notification_delivers_once_to_another_replica_without_replay() {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL is required for PostgreSQL live relay tests");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect(&database_url)
            .await
            .unwrap();
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

        let expired = frame(sender_device_id, live_now_ms().saturating_sub(1));
        local_backend.publish(vault_id, expired).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), remote_receiver.recv())
                .await
                .is_err()
        );

        let mut oversized = frame(sender_device_id, live_now_ms() + 10_000);
        oversized.ciphertext_b64 = STANDARD.encode(vec![0_u8; MAX_NOTIFY_PAYLOAD_BYTES]);
        assert_eq!(
            local_backend.publish(vault_id, oversized).await,
            Err(LiveBackendError)
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), remote_receiver.recv())
                .await
                .is_err()
        );

        drop(remote_backend);
        let mut no_replay = remote_hub.register(vault_id, receiver_device_id);
        assert_eq!(no_replay.receiver.try_recv(), Err(TryRecvError::Empty));
    }

    #[tokio::test]
    async fn listener_loss_and_backend_drop_mark_readiness_unavailable() {
        let database_url = std::env::var("TEST_DATABASE_URL")
            .expect("TEST_DATABASE_URL is required for PostgreSQL live relay tests");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .unwrap();
        let backend = PgLiveBackend::new(pool.clone(), LiveHub::new());
        wait_ready(&backend).await;
        let mut readiness = backend.subscribe_readiness();

        tokio::time::timeout(Duration::from_secs(2), pool.close())
            .await
            .expect("pool closes listener connection");
        tokio::time::timeout(Duration::from_secs(2), async {
            while *readiness.borrow_and_update() {
                readiness
                    .changed()
                    .await
                    .expect("backend should remain alive");
            }
        })
        .await
        .expect("listener loss marks backend unavailable");
        assert!(!backend.is_ready());

        drop(backend);
        assert!(!*readiness.borrow_and_update());
    }
}
