use peppy_domain::{DeviceId, VaultId};
use peppy_protocol::EphemeralSignalV1;
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::mpsc;

const LIVE_CHANNEL_CAPACITY: usize = 32;
const FUTURE_TOLERANCE_MS: u64 = 30_000;

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
    sender: mpsc::Sender<EphemeralSignalV1>,
}

pub(crate) struct LiveRegistration {
    pub(crate) generation: u64,
    pub(crate) receiver: mpsc::Receiver<EphemeralSignalV1>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LiveReject {
    InvalidFrame,
    SenderMismatch,
    Expired,
    FutureTimestamp,
}

impl LiveHub {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register(&self, vault_id: VaultId, device_id: DeviceId) -> LiveRegistration {
        let (sender, receiver) = mpsc::channel(LIVE_CHANNEL_CAPACITY);
        let mut sinks = self.inner.sinks.lock().expect("live hub lock poisoned");
        let generation = self
            .inner
            .generation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .expect("live generation exhausted")
            + 1;
        sinks.insert((vault_id, device_id), LiveSink { generation, sender });
        LiveRegistration {
            generation,
            receiver,
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
        frame: EphemeralSignalV1,
        now_ms: u64,
    ) -> Result<usize, LiveReject> {
        if frame.sender_device_id != authenticated_device_id {
            return Err(LiveReject::SenderMismatch);
        }
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
        let sinks = self.inner.sinks.lock().expect("live hub lock poisoned");
        let mut accepted = 0;
        for (&(sink_vault, sink_device), sink) in sinks.iter() {
            if sink_vault == vault_id
                && sink_device != authenticated_device_id
                && sink.sender.try_send(frame.clone()).is_ok()
            {
                accepted += 1;
            }
        }
        Ok(accepted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use peppy_domain::{DeviceId, VaultId};
    use peppy_protocol::EphemeralSignalV1;
    use tokio::sync::mpsc::error::TryRecvError;
    use uuid::Uuid;

    fn vault(n: u128) -> VaultId {
        VaultId(Uuid::from_u128(n))
    }

    fn device(n: u128) -> DeviceId {
        DeviceId(Uuid::from_u128(n))
    }

    fn frame() -> EphemeralSignalV1 {
        EphemeralSignalV1 {
            protocol_version: 1,
            kind: "typing".into(),
            sender_device_id: device(1),
            key_epoch: 1,
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
        value.protocol_version = 2;
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
        value.ciphertext_b64 = STANDARD.encode([0_u8; 5701]);
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
}
