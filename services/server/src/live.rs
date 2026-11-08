use peppy_domain::{DeviceId, VaultId};
use peppy_protocol::EphemeralSignalV2;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

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

    fn publish_control(
        &self,
        vault_id: VaultId,
        device_id: DeviceId,
        ticket: LiveTicketIdentity,
        kind: LiveControlKind,
    ) -> Pin<Box<dyn Future<Output = Result<(), LiveBackendError>> + Send + '_>> {
        let _ = (vault_id, device_id, ticket, kind);
        Box::pin(async { Err(LiveBackendError) })
    }
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
    state: Mutex<LiveHubState>,
}

#[derive(Default)]
struct LiveHubState {
    sinks: HashMap<(VaultId, DeviceId), LiveSink>,
    mutation_epochs: HashMap<(VaultId, DeviceId), u64>,
}

struct LiveSink {
    generation: u64,
    #[cfg(test)]
    session_id: Option<[u8; 16]>,
    ticket_identity: Option<LiveTicketIdentity>,
    sender: mpsc::Sender<EphemeralSignalV2>,
    cancellation: watch::Sender<bool>,
}

pub(crate) struct LiveRegistration {
    pub(crate) generation: u64,
    pub(crate) receiver: mpsc::Receiver<EphemeralSignalV2>,
    pub(crate) cancellation: watch::Receiver<bool>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct LiveTicketIdentity {
    session_id: [u8; 16],
    session_incarnation: i64,
    connection_id: i64,
}

impl LiveTicketIdentity {
    pub(crate) fn new(session_id: [u8; 16], session_incarnation: i64, connection_id: i64) -> Self {
        assert!(
            session_incarnation > 0,
            "live ticket incarnation must be positive"
        );
        assert!(
            connection_id > 0,
            "live ticket connection ID must be positive"
        );
        Self {
            session_id,
            session_incarnation,
            connection_id,
        }
    }

    pub(crate) fn session_incarnation(self) -> i64 {
        self.session_incarnation
    }

    pub(crate) fn connection_id(self) -> i64 {
        self.connection_id
    }

    pub(crate) fn session_hash(self) -> [u8; 32] {
        Sha256::digest(self.session_id).into()
    }
}

const LIVE_CONTROL_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LiveControlKind {
    Close,
    Replace,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveControl {
    version: u8,
    instance_id: Uuid,
    vault_id: Uuid,
    device_id: Uuid,
    session_hash: [u8; 32],
    session_incarnation: i64,
    connection_id: i64,
    kind: LiveControlKind,
}

impl LiveControl {
    pub(crate) fn new(
        instance_id: Uuid,
        vault_id: VaultId,
        device_id: DeviceId,
        ticket: LiveTicketIdentity,
        kind: LiveControlKind,
    ) -> Self {
        Self {
            version: LIVE_CONTROL_VERSION,
            instance_id,
            vault_id: vault_id.0,
            device_id: device_id.0,
            session_hash: ticket.session_hash(),
            session_incarnation: ticket.session_incarnation,
            connection_id: ticket.connection_id,
            kind,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.version == LIVE_CONTROL_VERSION
            && self.session_incarnation > 0
            && self.connection_id > 0
    }

    pub(crate) fn is_from(&self, instance_id: Uuid) -> bool {
        self.instance_id == instance_id
    }

    pub(crate) fn vault_id(&self) -> VaultId {
        VaultId(self.vault_id)
    }

    pub(crate) fn device_id(&self) -> DeviceId {
        DeviceId(self.device_id)
    }

    pub(crate) fn ticket_fields(&self) -> ([u8; 32], i64, i64) {
        (
            self.session_hash,
            self.session_incarnation,
            self.connection_id,
        )
    }
}

/// Opaque per-device state captured before durable ticket validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LiveTicketSnapshot(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LiveTicketInstallRefusal {
    ChangedEpoch,
    CurrentOrNewerConnection,
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
pub(crate) const LIVE_UPGRADE_RATE_LIMIT: usize = 16;
const LIVE_RATE_BUCKET_CAPACITY: usize = 4_096;

pub(crate) struct LiveRateLimiter {
    limit: usize,
    device: HashMap<DeviceId, LiveRateBucket>,
    address: HashMap<std::net::IpAddr, LiveRateBucket>,
}

impl Default for LiveRateLimiter {
    fn default() -> Self {
        Self::with_limit(LIVE_RATE_LIMIT)
    }
}

struct LiveRateBucket {
    started: std::time::Instant,
    count: usize,
}

impl LiveRateLimiter {
    pub(crate) fn with_limit(limit: usize) -> Self {
        Self {
            limit,
            device: HashMap::new(),
            address: HashMap::new(),
        }
    }

    pub(crate) fn allow(&mut self, device: DeviceId, address: Option<std::net::IpAddr>) -> bool {
        let now = std::time::Instant::now();
        Self::prune(&mut self.device, now);
        Self::prune(&mut self.address, now);
        if !self.can_reserve(&self.device, device)
            || !address.is_none_or(|address| self.can_reserve(&self.address, address))
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
        &self,
        buckets: &HashMap<K, LiveRateBucket>,
        key: K,
    ) -> bool {
        match buckets.get(&key) {
            Some(bucket) => bucket.count < self.limit,
            None => buckets.len() < LIVE_RATE_BUCKET_CAPACITY,
        }
    }
}

impl LiveHub {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register(&self, vault_id: VaultId, device_id: DeviceId) -> LiveRegistration {
        self.register_with_session(vault_id, device_id, None)
    }

    #[cfg(test)]
    pub(crate) fn register_ticket(
        &self,
        vault_id: VaultId,
        device_id: DeviceId,
        session_id: [u8; 16],
    ) -> LiveRegistration {
        self.register_with_session(vault_id, device_id, Some(session_id))
    }

    #[cfg(test)]
    pub(crate) fn register_ticket_with_identity(
        &self,
        vault_id: VaultId,
        device_id: DeviceId,
        ticket: LiveTicketIdentity,
    ) -> LiveRegistration {
        self.register_with_ticket(vault_id, device_id, Some(ticket.session_id), Some(ticket))
    }

    pub(crate) fn ticket_snapshot(
        &self,
        vault_id: VaultId,
        device_id: DeviceId,
    ) -> LiveTicketSnapshot {
        let state = self.inner.state.lock().expect("live hub lock poisoned");
        LiveTicketSnapshot(Self::mutation_epoch(&state, (vault_id, device_id)))
    }

    /// Callers validate durable state outside this mutex before attempting an install.
    pub(crate) fn register_ticket_if_current(
        &self,
        vault_id: VaultId,
        device_id: DeviceId,
        snapshot: LiveTicketSnapshot,
        ticket: LiveTicketIdentity,
    ) -> Result<LiveRegistration, LiveTicketInstallRefusal> {
        let generation = self.next_generation();
        let (sender, receiver) = mpsc::channel(LIVE_CHANNEL_CAPACITY);
        let (cancellation_sender, cancellation) = watch::channel(false);
        let mut state = self.inner.state.lock().expect("live hub lock poisoned");
        let key = (vault_id, device_id);
        if Self::mutation_epoch(&state, key) != snapshot.0 {
            return Err(LiveTicketInstallRefusal::ChangedEpoch);
        }
        if state
            .sinks
            .get(&key)
            .and_then(|sink| sink.ticket_identity)
            .is_some_and(|current| current.connection_id >= ticket.connection_id)
        {
            return Err(LiveTicketInstallRefusal::CurrentOrNewerConnection);
        }
        Self::install_sink(
            &mut state,
            key,
            LiveSink {
                generation,
                #[cfg(test)]
                session_id: Some(ticket.session_id),
                ticket_identity: Some(ticket),
                sender,
                cancellation: cancellation_sender,
            },
        );
        Ok(LiveRegistration {
            generation,
            receiver,
            cancellation,
        })
    }

    fn register_with_session(
        &self,
        vault_id: VaultId,
        device_id: DeviceId,
        session_id: Option<[u8; 16]>,
    ) -> LiveRegistration {
        self.register_with_ticket(vault_id, device_id, session_id, None)
    }

    fn register_with_ticket(
        &self,
        vault_id: VaultId,
        device_id: DeviceId,
        _session_id: Option<[u8; 16]>,
        ticket_identity: Option<LiveTicketIdentity>,
    ) -> LiveRegistration {
        let generation = self.next_generation();
        let (sender, receiver) = mpsc::channel(LIVE_CHANNEL_CAPACITY);
        let (cancellation_sender, cancellation) = watch::channel(false);
        let mut state = self.inner.state.lock().expect("live hub lock poisoned");
        Self::install_sink(
            &mut state,
            (vault_id, device_id),
            LiveSink {
                generation,
                #[cfg(test)]
                session_id: _session_id,
                ticket_identity,
                sender,
                cancellation: cancellation_sender,
            },
        );
        LiveRegistration {
            generation,
            receiver,
            cancellation,
        }
    }

    #[cfg(test)]
    pub(crate) fn cancel_exact(
        &self,
        vault_id: VaultId,
        device_id: DeviceId,
        session_id: [u8; 16],
        socket_generation: u64,
    ) {
        let mut state = self.inner.state.lock().expect("live hub lock poisoned");
        let key = (vault_id, device_id);
        if state.sinks.get(&key).is_some_and(|sink| {
            sink.session_id == Some(session_id) && sink.generation == socket_generation
        }) {
            Self::remove_sink(&mut state, key);
        }
    }

    pub(crate) fn cancel_ticket_exact(
        &self,
        vault_id: VaultId,
        device_id: DeviceId,
        ticket: LiveTicketIdentity,
    ) {
        self.cancel_ticket_hash_exact(
            vault_id,
            device_id,
            ticket.session_hash(),
            ticket.session_incarnation,
            ticket.connection_id,
        );
    }

    pub(crate) fn cancel_ticket_hash_exact(
        &self,
        vault_id: VaultId,
        device_id: DeviceId,
        session_hash: [u8; 32],
        session_incarnation: i64,
        connection_id: i64,
    ) {
        let mut state = self.inner.state.lock().expect("live hub lock poisoned");
        let key = (vault_id, device_id);
        if state
            .sinks
            .get(&key)
            .and_then(|sink| sink.ticket_identity)
            .is_some_and(|ticket| {
                ticket.session_hash() == session_hash
                    && ticket.session_incarnation == session_incarnation
                    && ticket.connection_id == connection_id
            })
        {
            Self::remove_sink(&mut state, key);
        }
    }

    #[cfg(test)]
    pub(crate) fn ticket_generation(
        &self,
        vault_id: VaultId,
        device_id: DeviceId,
        session_id: [u8; 16],
    ) -> Option<u64> {
        self.inner
            .state
            .lock()
            .expect("live hub lock poisoned")
            .sinks
            .get(&(vault_id, device_id))
            .filter(|sink| {
                sink.ticket_identity
                    .map_or(sink.session_id == Some(session_id), |ticket| {
                        ticket.session_id == session_id
                            && ticket.session_incarnation() > 0
                            && ticket.connection_id() > 0
                    })
            })
            .map(|sink| sink.generation)
    }

    pub(crate) fn unregister(&self, vault_id: VaultId, device_id: DeviceId, generation: u64) {
        let mut state = self.inner.state.lock().expect("live hub lock poisoned");
        let key = (vault_id, device_id);
        if state
            .sinks
            .get(&key)
            .is_some_and(|sink| sink.generation == generation)
        {
            Self::remove_sink(&mut state, key);
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
        let state = self.inner.state.lock().expect("live hub lock poisoned");
        Ok(Self::forward_locked(
            &state.sinks,
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
        let state = self.inner.state.lock().expect("live hub lock poisoned");
        if state
            .sinks
            .get(&(vault_id, authenticated_device_id))
            .is_none_or(|sink| sink.generation != generation)
        {
            return Ok(None);
        }
        Ok(Some(Self::forward_locked(
            &state.sinks,
            vault_id,
            authenticated_device_id,
            frame,
        )))
    }

    fn next_generation(&self) -> u64 {
        self.inner
            .generation
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .expect("live generation exhausted")
            + 1
    }

    fn mutation_epoch(state: &LiveHubState, key: (VaultId, DeviceId)) -> u64 {
        state.mutation_epochs.get(&key).copied().unwrap_or_default()
    }

    fn advance_mutation_epoch(state: &mut LiveHubState, key: (VaultId, DeviceId)) {
        let next = Self::mutation_epoch(state, key)
            .checked_add(1)
            .expect("live mutation epoch exhausted");
        state.mutation_epochs.insert(key, next);
    }

    fn install_sink(state: &mut LiveHubState, key: (VaultId, DeviceId), sink: LiveSink) {
        Self::advance_mutation_epoch(state, key);
        if let Some(replaced) = state.sinks.insert(key, sink) {
            let _ = replaced.cancellation.send(true);
        }
    }

    fn remove_sink(state: &mut LiveHubState, key: (VaultId, DeviceId)) {
        let removed = state.sinks.remove(&key).expect("live sink must exist");
        Self::advance_mutation_epoch(state, key);
        drop(removed);
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

    fn ticket(session: u8, incarnation: i64, connection: i64) -> LiveTicketIdentity {
        LiveTicketIdentity::new([session; 16], incarnation, connection)
    }

    #[test]
    fn control_wrapper_hashes_ticket_identity_and_rejects_invalid_wire_values() {
        let control = LiveControl::new(
            Uuid::from_u128(1),
            vault(2),
            device(3),
            ticket(4, 5, 6),
            LiveControlKind::Close,
        );
        let expected_hash: [u8; 32] = Sha256::digest([4; 16]).into();
        assert_eq!(control.session_hash, expected_hash);
        assert!(control.is_valid());

        let mut value = serde_json::to_value(&control).unwrap();
        value["version"] = serde_json::json!(2);
        assert!(
            !serde_json::from_value::<LiveControl>(value)
                .unwrap()
                .is_valid()
        );
        for field in ["session_incarnation", "connection_id"] {
            let mut value = serde_json::to_value(&control).unwrap();
            value[field] = serde_json::json!(0);
            assert!(
                !serde_json::from_value::<LiveControl>(value)
                    .unwrap()
                    .is_valid()
            );
        }
        for (field, value) in [
            ("session_hash", serde_json::json!([1, 2, 3])),
            ("kind", serde_json::json!("unknown")),
            ("unexpected", serde_json::json!(true)),
        ] {
            let mut invalid = serde_json::to_value(&control).unwrap();
            invalid[field] = value;
            assert!(serde_json::from_value::<LiveControl>(invalid).is_err());
        }
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
    fn upgrade_limiter_keeps_unrelated_device_and_ip_admissible() {
        let mut limiter = LiveRateLimiter::with_limit(1);
        let first = "127.0.0.1".parse().unwrap();
        let second = "127.0.0.2".parse().unwrap();
        assert!(limiter.allow(device(1), Some(first)));
        assert!(!limiter.allow(device(1), Some(first)));
        assert!(limiter.allow(device(2), Some(second)));
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

    #[test]
    fn exact_ticket_cancel_removes_matching_registration() {
        let hub = LiveHub::new();
        let mut registration = hub.register_ticket(vault(1), device(2), [1; 16]);
        hub.cancel_exact(vault(1), device(2), [1; 16], registration.generation);
        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(0));
        assert_eq!(
            registration.receiver.try_recv(),
            Err(TryRecvError::Disconnected)
        );
    }

    #[test]
    fn stale_ticket_generation_cancels_nothing() {
        let hub = LiveHub::new();
        let old = hub.register_ticket(vault(1), device(2), [1; 16]);
        let mut current = hub.register_ticket(vault(1), device(2), [1; 16]);
        hub.cancel_exact(vault(1), device(2), [1; 16], old.generation);
        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(1));
        assert_eq!(current.receiver.try_recv(), Ok(frame()));
    }

    #[test]
    fn ticket_cancel_leaves_different_session_untouched() {
        let hub = LiveHub::new();
        let old = hub.register_ticket(vault(1), device(2), [1; 16]);
        let mut current = hub.register_ticket(vault(1), device(2), [2; 16]);
        hub.cancel_exact(vault(1), device(2), [1; 16], old.generation);
        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(1));
        assert_eq!(current.receiver.try_recv(), Ok(frame()));
    }

    #[test]
    fn ticket_cancel_does_not_remove_bearer_registration() {
        let hub = LiveHub::new();
        let mut registration = hub.register(vault(1), device(2));
        hub.cancel_exact(vault(1), device(2), [1; 16], registration.generation);
        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(1));
        assert_eq!(registration.receiver.try_recv(), Ok(frame()));
    }

    #[test]
    fn delayed_ticket_snapshot_cannot_reinstall_after_successor_removal() {
        let hub = LiveHub::new();
        let delayed = hub.ticket_snapshot(vault(1), device(2));
        let successor = ticket(2, 1, 2);
        let registration = hub.register_ticket_with_identity(vault(1), device(2), successor);
        hub.unregister(vault(1), device(2), registration.generation);

        assert!(matches!(
            hub.register_ticket_if_current(vault(1), device(2), delayed, ticket(1, 1, 1)),
            Err(LiveTicketInstallRefusal::ChangedEpoch)
        ));
    }

    #[test]
    fn bearer_interposition_and_removal_invalidate_ticket_snapshot() {
        let hub = LiveHub::new();
        let delayed = hub.ticket_snapshot(vault(1), device(2));
        let bearer = hub.register(vault(1), device(2));
        hub.unregister(vault(1), device(2), bearer.generation);

        assert!(matches!(
            hub.register_ticket_if_current(vault(1), device(2), delayed, ticket(1, 1, 1)),
            Err(LiveTicketInstallRefusal::ChangedEpoch)
        ));
    }

    #[test]
    fn fresh_snapshot_refuses_equal_or_older_durable_connection() {
        let hub = LiveHub::new();
        hub.register_ticket_with_identity(vault(1), device(2), ticket(1, 1, 2));
        let snapshot = hub.ticket_snapshot(vault(1), device(2));

        assert!(matches!(
            hub.register_ticket_if_current(vault(1), device(2), snapshot, ticket(1, 1, 2)),
            Err(LiveTicketInstallRefusal::CurrentOrNewerConnection)
        ));
        assert!(matches!(
            hub.register_ticket_if_current(vault(1), device(2), snapshot, ticket(1, 1, 1)),
            Err(LiveTicketInstallRefusal::CurrentOrNewerConnection)
        ));
    }

    #[test]
    fn stale_durable_cancel_leaves_successor_and_bearer_untouched() {
        let hub = LiveHub::new();
        let old = ticket(1, 1, 1);
        let successor = ticket(1, 2, 2);
        hub.register_ticket_with_identity(vault(1), device(2), old);
        let mut current = hub.register_ticket_with_identity(vault(1), device(2), successor);
        hub.cancel_ticket_exact(vault(1), device(2), old);
        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(1));
        assert_eq!(current.receiver.try_recv(), Ok(frame()));

        let mut bearer = hub.register(vault(1), device(2));
        hub.cancel_ticket_exact(vault(1), device(2), successor);
        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(1));
        assert_eq!(bearer.receiver.try_recv(), Ok(frame()));
    }

    #[test]
    fn exact_cancel_mismatch_in_any_single_field_preserves_current_sink() {
        let current_ticket = ticket(1, 2, 2);
        let mismatches = [ticket(2, 2, 2), ticket(1, 3, 2), ticket(1, 2, 3)];
        for mismatched in mismatches {
            let hub = LiveHub::new();
            let mut current =
                hub.register_ticket_with_identity(vault(1), device(2), current_ticket);
            let snapshot = hub.ticket_snapshot(vault(1), device(2));

            hub.cancel_ticket_exact(vault(1), device(2), mismatched);

            assert_eq!(hub.ticket_snapshot(vault(1), device(2)), snapshot);
            assert!(!*current.cancellation.borrow());
            assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(1));
            assert_eq!(current.receiver.try_recv(), Ok(frame()));
        }
    }

    #[test]
    fn hash_exact_cancel_removes_only_the_matching_ticket() {
        let hub = LiveHub::new();
        let current = ticket(1, 2, 3);
        let mut registration = hub.register_ticket_with_identity(vault(1), device(2), current);
        hub.cancel_ticket_hash_exact(vault(1), device(2), ticket(2, 2, 3).session_hash(), 2, 3);
        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(1));
        assert_eq!(registration.receiver.try_recv(), Ok(frame()));

        hub.cancel_ticket_hash_exact(vault(1), device(2), current.session_hash(), 2, 3);
        assert_eq!(
            registration.receiver.try_recv(),
            Err(TryRecvError::Disconnected)
        );
    }

    #[test]
    fn successful_exact_cancel_invalidates_prior_snapshot_without_successor() {
        let hub = LiveHub::new();
        let current_ticket = ticket(1, 2, 2);
        let mut current = hub.register_ticket_with_identity(vault(1), device(2), current_ticket);
        let snapshot = hub.ticket_snapshot(vault(1), device(2));

        hub.cancel_ticket_exact(vault(1), device(2), current_ticket);

        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(0));
        assert_eq!(current.receiver.try_recv(), Err(TryRecvError::Disconnected));
        assert!(matches!(
            hub.register_ticket_if_current(vault(1), device(2), snapshot, ticket(1, 3, 3)),
            Err(LiveTicketInstallRefusal::ChangedEpoch)
        ));
    }

    #[test]
    fn ticket_snapshot_is_scoped_to_its_device() {
        let hub = LiveHub::new();
        let snapshot = hub.ticket_snapshot(vault(1), device(2));
        hub.register(vault(1), device(3));

        let mut registration = hub
            .register_ticket_if_current(vault(1), device(2), snapshot, ticket(1, 1, 1))
            .expect("unrelated device must not invalidate snapshot");
        hub.cancel_ticket_exact(vault(1), device(2), ticket(1, 1, 1));
        assert_eq!(
            registration.receiver.try_recv(),
            Err(TryRecvError::Disconnected)
        );
    }

    #[test]
    fn fresh_snapshot_installs_successor_and_cancels_only_its_tuple() {
        let hub = LiveHub::new();
        let old = ticket(1, 1, 1);
        hub.register_ticket_with_identity(vault(1), device(2), old);
        let snapshot = hub.ticket_snapshot(vault(1), device(2));
        let current = ticket(1, 2, 2);
        let mut registration = hub
            .register_ticket_if_current(vault(1), device(2), snapshot, current)
            .expect("fresh snapshot and newer connection must install");

        hub.cancel_ticket_exact(vault(1), device(2), old);
        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(1));
        assert_eq!(registration.receiver.try_recv(), Ok(frame()));
        hub.cancel_ticket_exact(vault(1), device(2), current);
        assert_eq!(hub.forward(vault(1), device(1), frame(), 100000), Ok(0));
    }
}
