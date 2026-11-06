use peppy_domain::{DeviceId, VaultId};
use peppy_protocol::EphemeralSignalV2;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::{mpsc, watch};

const LIVE_CHANNEL_CAPACITY: usize = 32;
const FUTURE_TOLERANCE_MS: u64 = 30_000;

/// A relay is required before the live route accepts upgrades. An in-process
/// hub alone cannot provide the cross-replica delivery promise.
pub(crate) trait LiveBackend: Send + Sync {
    fn publish(
        &self,
        vault_id: VaultId,
        frame: EphemeralSignalV2,
    ) -> Pin<Box<dyn Future<Output = Result<(), LiveBackendError>> + Send + '_>>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LiveBackendError;

#[derive(Clone, Default)]
pub(crate) struct LiveHub {
    inner: Arc<LiveHubInner>,
}

#[derive(Default)]
struct LiveHubInner {
    generation: AtomicU64,
    sinks: Mutex<HashMap<(VaultId, DeviceId), LiveSink>>,
}

struct LiveSink {
    generation: u64,
    sender: mpsc::Sender<EphemeralSignalV2>,
    cancellation: watch::Sender<bool>,
}

pub(crate) struct LiveRegistration {
    pub(crate) generation: u64,
    pub(crate) receiver: mpsc::Receiver<EphemeralSignalV2>,
    pub(crate) cancellation: watch::Receiver<bool>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LiveReject {
    InvalidFrame,
    SenderMismatch,
    Expired,
    FutureTimestamp,
}

/// Per-device and per-resolved-IP limits bound relay work. Entries are pruned
/// after one minute and the cache is capped to prevent unbounded key growth.
pub(crate) const LIVE_RATE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
pub(crate) const LIVE_RATE_LIMIT: usize = 120;
const LIVE_RATE_BUCKET_CAPACITY: usize = 4_096;

#[derive(Default)]
pub(crate) struct LiveRateLimiter {
    device: HashMap<DeviceId, LiveRateBucket>,
    address: HashMap<std::net::IpAddr, LiveRateBucket>,
}

struct LiveRateBucket {
    started: std::time::Instant,
    count: usize,
}

impl LiveRateLimiter {
    pub(crate) fn allow(&mut self, device: DeviceId, address: Option<std::net::IpAddr>) -> bool {
        let now = std::time::Instant::now();
        Self::prune(&mut self.device, now);
        Self::prune(&mut self.address, now);
        if !Self::can_reserve(&self.device, device)
            || !address.is_none_or(|address| Self::can_reserve(&self.address, address))
        {
            return false;
        }
        Self::reserve(&mut self.device, device, now);
        if let Some(address) = address {
            Self::reserve(&mut self.address, address, now);
        }
        true
    }

    fn prune<K: std::cmp::Eq + std::hash::Hash>(
        buckets: &mut HashMap<K, LiveRateBucket>,
        now: std::time::Instant,
    ) {
        buckets.retain(|_, bucket| now.duration_since(bucket.started) < LIVE_RATE_WINDOW);
    }

    fn reserve<K: std::cmp::Eq + std::hash::Hash + Copy>(
        buckets: &mut HashMap<K, LiveRateBucket>,
        key: K,
        now: std::time::Instant,
    ) {
        let bucket = buckets.entry(key).or_insert(LiveRateBucket {
            started: now,
            count: 0,
        });
        bucket.count += 1;
    }

    fn can_reserve<K: std::cmp::Eq + std::hash::Hash>(
        buckets: &HashMap<K, LiveRateBucket>,
        key: K,
    ) -> bool {
        match buckets.get(&key) {
            Some(bucket) => bucket.count < LIVE_RATE_LIMIT,
            None => buckets.len() < LIVE_RATE_BUCKET_CAPACITY,
        }
    }
}

impl LiveHub {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register(&self, vault_id: VaultId, device_id: DeviceId) -> LiveRegistration {
        let (sender, receiver) = mpsc::channel(LIVE_CHANNEL_CAPACITY);
        let (cancellation_sender, cancellation) = watch::channel(false);
        let mut sinks = self.inner.sinks.lock().expect("live hub lock poisoned");
        let generation = self
            .inner
            .generation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .expect("live generation exhausted")
            + 1;
        if let Some(replaced) = sinks.insert(
            (vault_id, device_id),
            LiveSink {
                generation,
                sender,
                cancellation: cancellation_sender,
            },
        ) {
            let _ = replaced.cancellation.send(true);
        }
        LiveRegistration {
            generation,
            receiver,
            cancellation,
        }
    }

    pub(crate) fn unregister(&self, vault_id: VaultId, device_id: DeviceId, generation: u64) {
        let mut sinks = self.inner.sinks.lock().expect("live hub lock poisoned");
        let key = (vault_id, device_id);
        if sinks
            .get(&key)
            .is_some_and(|sink| sink.generation == generation)
        {
            sinks.remove(&key);
        }
    }

    /// Returns the number of sinks that accepted the frame, not delivery acknowledgements.
    pub(crate) fn forward(
        &self,
        vault_id: VaultId,
        authenticated_device_id: DeviceId,
        frame: EphemeralSignalV2,
        now_ms: u64,
    ) -> Result<usize, LiveReject> {
        Self::validate_frame(authenticated_device_id, &frame, now_ms)?;
        let sinks = self.inner.sinks.lock().expect("live hub lock poisoned");
        Ok(Self::forward_locked(
            &sinks,
            vault_id,
            authenticated_device_id,
            frame,
        ))
    }

    /// Delivers only while this exact socket generation remains current. The
    /// current-generation check and local fan-out share one mutex acquisition.
    pub(crate) fn forward_current(
        &self,
        vault_id: VaultId,
        authenticated_device_id: DeviceId,
        generation: u64,
        frame: EphemeralSignalV2,
        now_ms: u64,
    ) -> Result<Option<usize>, LiveReject> {
        Self::validate_frame(authenticated_device_id, &frame, now_ms)?;
        let sinks = self.inner.sinks.lock().expect("live hub lock poisoned");
        if sinks
            .get(&(vault_id, authenticated_device_id))
            .is_none_or(|sink| sink.generation != generation)
        {
            return Ok(None);
        }
        Ok(Some(Self::forward_locked(
            &sinks,
            vault_id,
            authenticated_device_id,
            frame,
        )))
    }

    fn forward_locked(
        sinks: &HashMap<(VaultId, DeviceId), LiveSink>,
        vault_id: VaultId,
        authenticated_device_id: DeviceId,
        frame: EphemeralSignalV2,
    ) -> usize {
        let mut accepted = 0;
        for (&(sink_vault, sink_device), sink) in sinks.iter() {
            if sink_vault == vault_id
                && sink_device != authenticated_device_id
                && sink.sender.try_send(frame.clone()).is_ok()
            {
                accepted += 1;
            }
        }
        accepted
    }

    pub(crate) fn validate_frame(
        authenticated_device_id: DeviceId,
        frame: &EphemeralSignalV2,
        now_ms: u64,
    ) -> Result<(), LiveReject> {
        if frame.sender_device_id != authenticated_device_id {
            return Err(LiveReject::SenderMismatch);
        }
        Self::validate_current(frame, now_ms)
    }

    pub(crate) fn validate_current(
        frame: &EphemeralSignalV2,
        now_ms: u64,
    ) -> Result<(), LiveReject> {
        frame.validate().map_err(|_| LiveReject::InvalidFrame)?;
        let sent_at_ms: u64 = frame
            .sent_at_ms
            .parse()
            .map_err(|_| LiveReject::InvalidFrame)?;
        let expires_at_ms: u64 = frame
            .expires_at_ms
            .parse()
            .map_err(|_| LiveReject::InvalidFrame)?;
        if expires_at_ms <= now_ms {
            return Err(LiveReject::Expired);
        }
        if sent_at_ms > now_ms.saturating_add(FUTURE_TOLERANCE_MS) {
            return Err(LiveReject::FutureTimestamp);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use peppy_domain::{DeviceId, VaultId};
    use peppy_protocol::{EphemeralSignalV2, EphemeralSignalV2Kind};
    use tokio::sync::mpsc::error::TryRecvError;
    use uuid::Uuid;

    fn vault(n: u128) -> VaultId {
        VaultId(Uuid::from_u128(n))
    }

    fn device(n: u128) -> DeviceId {
        DeviceId(Uuid::from_u128(n))
    }

    fn frame() -> EphemeralSignalV2 {
        EphemeralSignalV2 {
            protocol_version: 2,
            kind: EphemeralSignalV2Kind::Typing,
            sender_device_id: device(1),
            key_epoch: 1,
            session_id: "AAAAAAAAAAAAAAAAAAAAAA".into(),
            sent_at_ms: "100000".into(),
            expires_at_ms: "115000".into(),
            sequence: "1".into(),
            ciphertext_b64: STANDARD.encode([0_u8; 32]),
        }
    }

    #[test]
    fn forwards_only_to_other_devices_in_same_vault() {
        let hub = LiveHub::new();
        let mut sender = hub.register(vault(1), device(1));
        let mut peer = hub.register(vault(1), device(2));
        let mut foreign = hub.register(vault(2), device(2));
        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(1));
        assert_eq!(peer.receiver.try_recv(), Ok(frame()));
        assert_eq!(sender.receiver.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(foreign.receiver.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn rejects_sender_mismatch_without_delivery() {
        let hub = LiveHub::new();
        let mut peer = hub.register(vault(1), device(2));
        assert_eq!(
            hub.forward(vault(1), device(3), frame(), 100000),
            Err(LiveReject::SenderMismatch)
        );
        assert_eq!(peer.receiver.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn rejects_expired_and_future_frames_at_boundaries() {
        let hub = LiveHub::new();
        let mut peer = hub.register(vault(1), device(2));
        assert_eq!(
            hub.forward(vault(1), device(1), frame(), 115000),
            Err(LiveReject::Expired)
        );
        assert_eq!(
            hub.forward(vault(1), device(1), frame(), 69999),
            Err(LiveReject::FutureTimestamp)
        );
        assert_eq!(peer.receiver.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(hub.forward(vault(1), device(1), frame(), 70000), Ok(1));
        assert_eq!(peer.receiver.try_recv(), Ok(frame()));
        assert_eq!(hub.forward(vault(1), device(1), frame(), 114999), Ok(1));
    }

    #[test]
    fn rejects_malformed_and_oversized_frames_without_delivery() {
        let hub = LiveHub::new();
        let mut peer = hub.register(vault(1), device(2));
        let mut invalid = Vec::new();
        let mut value = frame();
        value.protocol_version = 1;
        invalid.push(value);
        let mut value = frame();
        value.session_id = "not-canonical".into();
        invalid.push(value);
        let mut value = frame();
        value.sequence = "0".into();
        invalid.push(value);
        let mut value = frame();
        value.sent_at_ms = "0100000".into();
        invalid.push(value);
        let mut value = frame();
        value.expires_at_ms = "115001".into();
        invalid.push(value);
        let mut value = frame();
        value.ciphertext_b64 = "!".into();
        invalid.push(value);
        let mut value = frame();
        value.ciphertext_b64 = STANDARD.encode([0_u8; 5401]);
        invalid.push(value);
        for value in invalid {
            assert_eq!(
                hub.forward(vault(1), device(1), value, 100000),
                Err(LiveReject::InvalidFrame)
            );
        }
        assert_eq!(peer.receiver.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn second_registration_wins_and_closes_old_receiver() {
        let hub = LiveHub::new();
        let mut old = hub.register(vault(1), device(2));
        let mut current = hub.register(vault(1), device(2));
        assert!(current.generation > old.generation);
        assert_eq!(old.receiver.try_recv(), Err(TryRecvError::Disconnected));
        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(1));
        assert_eq!(current.receiver.try_recv(), Ok(frame()));
    }

    #[test]
    fn stale_unregister_preserves_replacement() {
        let hub = LiveHub::new();
        let old = hub.register(vault(1), device(2));
        let mut current = hub.register(vault(1), device(2));
        hub.unregister(vault(1), device(2), old.generation);
        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(1));
        assert_eq!(current.receiver.try_recv(), Ok(frame()));
    }

    #[test]
    fn current_unregister_removes_sink_and_does_not_replay() {
        let hub = LiveHub::new();
        let mut current = hub.register(vault(1), device(2));
        hub.unregister(vault(1), device(2), current.generation);
        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(0));
        assert_eq!(current.receiver.try_recv(), Err(TryRecvError::Disconnected));
        let mut replacement = hub.register(vault(1), device(2));
        assert_eq!(replacement.receiver.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn full_queue_drops_frame_without_blocking_other_receiver() {
        let hub = LiveHub::new();
        let mut slow = hub.register(vault(1), device(2));
        let mut fast = hub.register(vault(1), device(3));
        for _ in 0..32 {
            assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(2));
            assert_eq!(fast.receiver.try_recv(), Ok(frame()));
        }
        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(1));
        assert_eq!(fast.receiver.try_recv(), Ok(frame()));
        for _ in 0..32 {
            assert_eq!(slow.receiver.try_recv(), Ok(frame()));
        }
        assert_eq!(slow.receiver.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn cloned_hub_shares_registrations() {
        let hub = LiveHub::new();
        let mut peer = hub.register(vault(1), device(2));
        assert_eq!(
            hub.clone().forward(vault(1), device(1), frame(), 100000),
            Ok(1)
        );
        assert_eq!(peer.receiver.try_recv(), Ok(frame()));
    }

    #[test]
    fn rate_limiter_enforces_device_and_ip_buckets() {
        let mut limiter = LiveRateLimiter::default();
        let address = "127.0.0.1".parse().unwrap();
        for _ in 0..LIVE_RATE_LIMIT {
            assert!(limiter.allow(device(1), Some(address)));
        }
        assert!(!limiter.allow(device(1), Some(address)));
        assert!(!limiter.allow(device(2), Some(address)));
        assert!(limiter.allow(device(3), Some("127.0.0.2".parse().unwrap())));
    }

    #[test]
    fn rate_limiter_rejects_new_keys_at_capacity_without_resetting_existing_buckets() {
        let mut limiter = LiveRateLimiter::default();
        for value in 1..=LIVE_RATE_BUCKET_CAPACITY {
            assert!(limiter.allow(device(value as u128), None));
        }
        assert!(!limiter.allow(device((LIVE_RATE_BUCKET_CAPACITY + 1) as u128), None));
        for _ in 1..LIVE_RATE_LIMIT {
            assert!(limiter.allow(device(1), None));
        }
        assert!(!limiter.allow(device(1), None));
    }

    #[test]
    fn rate_limiter_reserves_device_and_ip_together() {
        let mut limiter = LiveRateLimiter::default();
        let address = "127.0.0.1".parse().unwrap();
        for _ in 0..LIVE_RATE_LIMIT {
            assert!(limiter.allow(device(1), Some(address)));
        }
        assert!(!limiter.allow(device(2), Some(address)));
        let alternate = "127.0.0.2".parse().unwrap();
        for _ in 0..LIVE_RATE_LIMIT {
            assert!(limiter.allow(device(2), Some(alternate)));
        }
        assert!(!limiter.allow(device(2), Some(alternate)));
    }

    #[test]
    fn replacement_cancels_old_generation() {
        let hub = LiveHub::new();
        let mut old = hub.register(vault(1), device(1));
        let current = hub.register(vault(1), device(1));
        assert!(*old.cancellation.borrow_and_update());
        hub.unregister(vault(1), device(1), old.generation);
        assert!(current.generation > old.generation);
    }
}
