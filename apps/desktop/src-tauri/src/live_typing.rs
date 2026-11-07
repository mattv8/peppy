use futures_util::{Sink, SinkExt, StreamExt};
use peppy_client_core::{
    Client, ConversationId, DeviceId, LiveAction, LiveClock, LiveEnableContext,
    LiveEnableGeneration, LiveError, LiveExecutor, LiveOutcome, PeerSessionUpdate,
};
use peppy_desktop_api::rcs_status::{
    snapshot_at, RcsEligibilityState, RcsGatewayState, RcsStatusSnapshot, SnapshotAccess,
};
use peppy_protocol::{LiveHelloV2, TypingOriginV2, TypingStateV2};
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{atomic::Ordering, Arc, Mutex, OnceLock},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::{sleep, timeout, Instant},
};
use tokio_tungstenite::{
    tungstenite::{self, client::IntoClientRequest, protocol::WebSocketConfig, Message},
    WebSocketStream,
};

use crate::{
    error::BridgeResult,
    session::{NetStatus, Session},
    sync::jitter,
    AppState,
};

const TYPING_EVENT: &str = "peppy://typing";
static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

pub(crate) fn install_events(app: &tauri::AppHandle) {
    let _ = APP_HANDLE.set(app.clone());
}

pub(crate) struct LiveTypingHost {
    sender: Mutex<Option<mpsc::Sender<Cmd>>>,
    started: Instant,
    #[cfg(test)]
    suspend_clock: Mutex<Option<Arc<TestSuspendClock>>>,
}

#[cfg(test)]
struct TestSuspendClock {
    base_mono: Instant,
    base_wall: SystemTime,
    elapsed: Mutex<Duration>,
    samples: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl TestSuspendClock {
    fn new() -> Self {
        Self {
            base_mono: Instant::now(),
            base_wall: SystemTime::now(),
            elapsed: Mutex::new(Duration::ZERO),
            samples: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn set(&self, elapsed: Duration) {
        *self
            .elapsed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = elapsed;
    }

    fn now(&self) -> (Instant, SystemTime) {
        self.samples.fetch_add(1, Ordering::AcqRel);
        let elapsed = *self
            .elapsed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        (self.base_mono + elapsed, self.base_wall + elapsed)
    }

    fn sample_count(&self) -> usize {
        self.samples.load(Ordering::Acquire)
    }
}

impl Default for LiveTypingHost {
    fn default() -> Self {
        Self {
            sender: Mutex::new(None),
            started: Instant::now(),
            #[cfg(test)]
            suspend_clock: Mutex::new(None),
        }
    }
}

impl LiveTypingHost {
    fn install(&self, sender: mpsc::Sender<Cmd>) {
        *self
            .sender
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(sender);
    }

    fn commands(&self) -> Option<mpsc::Sender<Cmd>> {
        self.sender
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// Gateway status changed; the actor re-derives its context. A full queue is harmless because
    /// every authority boundary re-derives the context before it admits or publishes anything.
    pub(crate) fn refresh_gateways(&self) {
        if let Some(sender) = self.commands() {
            let _ = sender.try_send(Cmd::GatewayRefresh);
        }
    }

    fn clock(&self) -> LiveClock {
        clock(self.started)
    }

    fn mono_ms(&self) -> u64 {
        self.started
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    fn suspend_now(&self) -> (Instant, SystemTime) {
        #[cfg(test)]
        if let Some(clock) = self
            .suspend_clock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
        {
            return clock.now();
        }
        (Instant::now(), SystemTime::now())
    }

    #[cfg(test)]
    fn install_suspend_clock(&self, clock: Arc<TestSuspendClock>) {
        *self
            .suspend_clock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(clock);
    }
}

pub(crate) type GatewayDeviceId = DeviceId;

pub(crate) struct RcsSnapshotInputs<'a> {
    pub client: &'a Client,
    pub now_ms: i64,
    pub unlocked: bool,
}

pub(crate) fn resolve_typing_target(
    status: &NetStatus,
    inputs: RcsSnapshotInputs<'_>,
    conversation_id: ConversationId,
) -> Option<GatewayDeviceId> {
    let snapshot = snapshot(status, inputs, Some(conversation_id))?;
    typing_target(&snapshot, conversation_id)
}

pub(crate) fn enable_context(
    status: &NetStatus,
    inputs: RcsSnapshotInputs<'_>,
) -> Option<LiveEnableContext> {
    let trusted_roster = trusted_roster(status)?;
    let snapshot = snapshot(status, inputs, None)?;
    let gateway = unique_current_linked_gateway(&snapshot)?;
    trusted_roster
        .contains(&gateway)
        .then_some(LiveEnableContext {
            trusted_roster,
            local_gateway_device_id: gateway,
            local_executor: LiveExecutor::None,
        })
}

fn snapshot(
    status: &NetStatus,
    inputs: RcsSnapshotInputs<'_>,
    active_conversation: Option<ConversationId>,
) -> Option<RcsStatusSnapshot> {
    let active_is_existing = active_conversation.is_some();
    snapshot_at(
        inputs.client,
        &status.gateways,
        &status.revoked_device_ids,
        SnapshotAccess {
            active_conversation,
            active_is_existing,
            roster_validated: status.rcs_roster_validated,
            unlocked: inputs.unlocked,
            now_ms: inputs.now_ms,
        },
    )
    .ok()
    .flatten()
}

fn trusted_roster(status: &NetStatus) -> Option<Vec<DeviceId>> {
    (!status.rcs_live_roster.is_empty() && status.rcs_roster_validated)
        .then(|| {
            status
                .rcs_live_roster
                .iter()
                .map(|device_id| device_id.parse().ok())
                .collect()
        })
        .flatten()
}

fn typing_target(
    snapshot: &RcsStatusSnapshot,
    conversation_id: ConversationId,
) -> Option<DeviceId> {
    let candidates: Vec<_> = snapshot
        .eligibilities
        .iter()
        .filter(|eligibility| {
            eligibility.conversation_id == conversation_id.to_string()
                && eligibility.eligibility == RcsEligibilityState::Eligible
                && snapshot.gateways.iter().any(|gateway| {
                    gateway.gateway_id == eligibility.gateway_id
                        && gateway.current
                        && gateway.state == RcsGatewayState::Linked
                })
        })
        .filter_map(|eligibility| eligibility.gateway_id.parse().ok())
        .collect();
    unique(candidates)
}

fn unique_current_linked_gateway(snapshot: &RcsStatusSnapshot) -> Option<DeviceId> {
    unique(
        snapshot
            .gateways
            .iter()
            .filter(|gateway| gateway.current && gateway.state == RcsGatewayState::Linked)
            .filter_map(|gateway| gateway.gateway_id.parse().ok())
            .collect(),
    )
}

fn unique(candidates: Vec<DeviceId>) -> Option<DeviceId> {
    let candidate = candidates.first()?.to_owned();
    candidates
        .iter()
        .all(|other| *other == candidate)
        .then_some(candidate)
}

#[derive(Default)]
pub(crate) struct LiveTypingState {
    context: Option<LiveEnableContext>,
    generation: Option<LiveEnableGeneration>,
}

impl LiveTypingState {
    pub(crate) fn generation(&self) -> Option<LiveEnableGeneration> {
        self.generation
    }

    pub(crate) fn apply(
        &mut self,
        client: &Client,
        context: Option<LiveEnableContext>,
        clock: LiveClock,
    ) -> Result<Option<LiveEnableGeneration>, LiveError> {
        if context == self.context {
            return Ok(self.generation);
        }
        let generation = match context.as_ref() {
            Some(context) => Some(client.enable_live_typing(context.clone(), clock)?),
            None => {
                client
                    .disable_live_typing()
                    .map_err(|_| LiveError::InvalidContext)?;
                None
            }
        };
        self.context = context;
        self.generation = generation;
        Ok(generation)
    }
}

const LIVE_MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);
#[cfg(not(test))]
const LIVE_SUSPEND_TICK: Duration = Duration::from_secs(2);
#[cfg(test)]
const LIVE_SUSPEND_TICK: Duration = Duration::from_millis(20);
const LIVE_SUSPEND_SKEW: Duration = Duration::from_secs(5);
const LIVE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const LIVE_HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const LIVE_IO_TIMEOUT: Duration = Duration::from_secs(2);
const LIVE_MAX_FRAME: usize = 64 * 1024;
const COMMAND_QUEUE_LIMIT: usize = 32;
const SEALED_QUEUE_LIMIT: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TypingDisplayState {
    pub conversation_id: ConversationId,
    pub state: TypingStateV2,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<String>,
    #[serde(skip)]
    deadline_mono: u64,
    #[serde(skip)]
    peer_device_id: DeviceId,
    #[serde(skip)]
    session_token: u64,
    #[serde(skip)]
    generation: LiveEnableGeneration,
}

#[derive(Default)]
struct InboundTyping {
    displays: HashMap<ConversationId, TypingDisplayState>,
    peers: HashMap<DeviceId, (u64, u64)>,
    sequences: HashMap<(DeviceId, ConversationId), u64>,
    events: Vec<TypingDisplayState>,
    gateway_intent_drops: u64,
}

impl InboundTyping {
    fn clear(&mut self) {
        for (_, mut display) in self.displays.drain() {
            display.state = TypingStateV2::Stopped;
            display.expires_at_ms = None;
            display.deadline_mono = 0;
            self.events.push(display);
        }
        self.peers.clear();
        self.sequences.clear();
    }

    fn next_deadline(&self) -> Option<u64> {
        self.displays
            .values()
            .map(|display| display.deadline_mono)
            .min()
    }

    fn expire(&mut self, mono: u64) {
        let expired = self
            .displays
            .iter()
            .filter_map(|(conversation_id, display)| {
                (display.deadline_mono <= mono).then_some(*conversation_id)
            })
            .collect::<Vec<_>>();
        for conversation_id in expired {
            if let Some(mut display) = self.displays.remove(&conversation_id) {
                display.state = TypingStateV2::Stopped;
                display.expires_at_ms = None;
                display.deadline_mono = 0;
                self.events.push(display);
            }
        }
    }

    fn publish(
        &mut self,
        outcome: LiveOutcome,
        generation: Option<LiveEnableGeneration>,
        clock: LiveClock,
    ) -> Option<String> {
        if Some(outcome.generation) != generation {
            return None;
        }
        if outcome
            .session_update
            .is_some_and(|update| !self.update_peer(update))
        {
            return None;
        }
        let reply = outcome
            .reply
            .and_then(|frame| serde_json::to_string(&frame).ok());
        let Some(action) = outcome.action else {
            return reply;
        };
        match action {
            LiveAction::Display {
                peer_device_id,
                session_token,
                sequence,
                conversation_id,
                state,
                deadline_mono,
            } => {
                if self
                    .peers
                    .get(&peer_device_id)
                    .is_none_or(|(token, _)| *token != session_token)
                {
                    return reply;
                }
                let last = self
                    .sequences
                    .entry((peer_device_id, conversation_id))
                    .or_insert(0);
                if sequence <= *last {
                    return reply;
                }
                *last = sequence;
                let display = TypingDisplayState {
                    conversation_id,
                    state,
                    expires_at_ms: (state == TypingStateV2::Started).then(|| {
                        u64::try_from(clock.wall_ms)
                            .unwrap_or(0)
                            .saturating_add(deadline_mono.saturating_sub(clock.mono_ms))
                            .to_string()
                    }),
                    deadline_mono,
                    peer_device_id,
                    session_token,
                    generation: outcome.generation,
                };
                if state == TypingStateV2::Started {
                    self.displays.insert(conversation_id, display.clone());
                } else {
                    self.displays.remove(&conversation_id);
                }
                self.events.push(display);
            }
            LiveAction::GatewayTypingIntent { .. } => self.gateway_intent_drops += 1,
        }
        reply
    }

    fn drain_events(&mut self) -> impl Iterator<Item = TypingDisplayState> + '_ {
        self.events.drain(..)
    }

    fn update_peer(&mut self, update: PeerSessionUpdate) -> bool {
        if self
            .peers
            .get(&update.peer_device_id)
            .is_some_and(|(_, revision)| update.validation_revision <= *revision)
        {
            return false;
        }
        let prior_token = self.peers.insert(
            update.peer_device_id,
            (update.current_session_token, update.validation_revision),
        );
        let retired_token = update.replaced_session_token.or_else(|| {
            prior_token
                .map(|(token, _)| token)
                .filter(|token| *token != update.current_session_token)
        });
        if let Some(replaced_token) = retired_token {
            let replaced = self
                .displays
                .iter()
                .filter_map(|(conversation_id, display)| {
                    (display.peer_device_id == update.peer_device_id
                        && display.session_token == replaced_token)
                        .then_some(*conversation_id)
                })
                .collect::<Vec<_>>();
            for conversation_id in replaced {
                if let Some(mut display) = self.displays.remove(&conversation_id) {
                    display.state = TypingStateV2::Stopped;
                    display.expires_at_ms = None;
                    display.deadline_mono = 0;
                    self.events.push(display);
                }
            }
            self.sequences
                .retain(|(peer, _), _| *peer != update.peer_device_id);
        }
        true
    }
}

fn typing_event_targets<'a>(
    conversation_id: ConversationId,
    binding: &crate::credentials::Binding,
    composers: impl IntoIterator<Item = (&'a str, Option<&'a crate::composer_scope::MovedScope>)>,
) -> Vec<String> {
    let mut targets = vec![crate::tray::MAIN.to_owned()];
    targets.extend(composers.into_iter().filter_map(|(label, moved)| {
        let source = crate::tray::composer_conversation(label)?;
        (crate::composer_scope::effective_conversation(source, moved, binding, conversation_id)
            == Some(conversation_id))
        .then(|| label.to_owned())
    }));
    targets
}

fn drain_publishable_events(
    inbound: &mut InboundTyping,
    generation: Option<LiveEnableGeneration>,
    cancelled: bool,
    mono_ms: u64,
) -> Vec<TypingDisplayState> {
    inbound
        .drain_events()
        .filter(|event| {
            event.state == TypingStateV2::Stopped
                || (!cancelled
                    && Some(event.generation) == generation
                    && event.deadline_mono > mono_ms)
        })
        .collect()
}

fn emit_events(
    inbound: &mut InboundTyping,
    session: &Session,
    generation: Option<LiveEnableGeneration>,
) {
    let events = drain_publishable_events(
        inbound,
        generation,
        session.cancel.is_cancelled(),
        session.live_typing.mono_ms(),
    );
    let Some(app) = APP_HANDLE.get() else {
        return;
    };
    use tauri::{Emitter as _, Manager as _};
    let state = app.state::<AppState>();
    let scopes = state
        .composer_scopes
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let windows = app.webview_windows();
    for event in events {
        let composers = windows
            .keys()
            .filter(|label| crate::tray::composer_conversation(label).is_some())
            .map(|label| (label.as_str(), scopes.get(label)));
        for target in typing_event_targets(event.conversation_id, &session.binding, composers) {
            let _ = app.emit_to(target, TYPING_EVENT, event.clone());
        }
    }
}

/// Commands are the only way into typing state. Every reply is awaited under cancellation, and the
/// actor never awaits anything the supervisor or Tauri command holds.
enum Cmd {
    SendTyping {
        conversation_id: ConversationId,
        state: TypingStateV2,
        reply: oneshot::Sender<bool>,
    },
    SetSharing {
        enabled: bool,
        reply: oneshot::Sender<Result<(), ()>>,
    },
    GatewayRefresh,
    Reevaluate,
    SocketUp,
    SocketDown,
    Retire,
    InboundFrame {
        json: String,
        reply: oneshot::Sender<InboundOutcome>,
    },
    ConfirmTransmit {
        mark: SealMark,
        reply: oneshot::Sender<Admission>,
    },
    SuspendTick {
        reply: oneshot::Sender<bool>,
    },
    Expire,
    Shutdown,
    #[cfg(test)]
    QueueSealedForTest {
        json: String,
        reply: oneshot::Sender<bool>,
    },
}

/// The authority a sealed frame was produced under, checked again at the final boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SealMark {
    seq: u64,
    sharing_epoch: u64,
    state: TypingStateV2,
}

struct SealedFrame {
    json: String,
    mark: SealMark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admission {
    Transmit,
    Rejected,
    Suspended,
}

#[derive(Default)]
struct InboundOutcome {
    wire: Option<String>,
    suspended: bool,
}

type Published = (u64, Option<u64>);

struct TypingActor {
    session: Arc<Session>,
    state: LiveTypingState,
    inbound: InboundTyping,
    sharing: bool,
    sharing_epoch: u64,
    enabled_seq: u64,
    retired: bool,
    socket_up: bool,
    anchor: (Instant, SystemTime),
    sealed: mpsc::Sender<SealedFrame>,
    published: watch::Sender<Published>,
}

impl TypingActor {
    fn new(
        session: Arc<Session>,
        sealed: mpsc::Sender<SealedFrame>,
        published: watch::Sender<Published>,
    ) -> Self {
        let anchor = session.live_typing.suspend_now();
        Self {
            session,
            state: LiveTypingState::default(),
            inbound: InboundTyping::default(),
            sharing: true,
            sharing_epoch: 0,
            enabled_seq: 0,
            retired: false,
            socket_up: false,
            anchor,
            sealed,
            published,
        }
    }

    fn run(mut self, mut commands: mpsc::Receiver<Cmd>) {
        while let Some(cmd) = commands.blocking_recv() {
            if matches!(cmd, Cmd::Shutdown) {
                break;
            }
            self.handle(cmd);
        }
        self.handle(Cmd::Retire);
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::SendTyping {
                conversation_id,
                state,
                reply,
            } => {
                let _ = reply.send(self.send_typing(conversation_id, state));
            }
            Cmd::SetSharing { enabled, reply } => {
                let _ = reply.send(self.set_sharing(enabled));
            }
            Cmd::GatewayRefresh | Cmd::Reevaluate => self.reevaluate(),
            Cmd::SocketUp => {
                // A new connection starts a new suspend window; backoff time between sockets is
                // not observed by the periodic tick and must not read as a suspend gap.
                self.anchor = self.session.live_typing.suspend_now();
                self.socket_up = true;
                self.retired = false;
                self.reevaluate();
            }
            Cmd::SocketDown => {
                self.socket_up = false;
                self.retire();
            }
            Cmd::Retire => self.retire(),
            Cmd::InboundFrame { json, reply } => {
                let _ = reply.send(self.inbound_frame(&json));
            }
            Cmd::ConfirmTransmit { mark, reply } => {
                let _ = reply.send(self.confirm_transmit(mark));
            }
            Cmd::SuspendTick { reply } => {
                let _ = reply.send(self.observe_suspend());
            }
            Cmd::Expire => self.expire(),
            #[cfg(test)]
            Cmd::QueueSealedForTest { json, reply } => {
                let mark = SealMark {
                    seq: self.enabled_seq,
                    sharing_epoch: self.sharing_epoch,
                    state: TypingStateV2::Stopped,
                };
                let _ = reply.send(self.sealed.try_send(SealedFrame { json, mark }).is_ok());
            }
            Cmd::Shutdown => {}
        }
        self.publish();
    }

    fn send_typing(&mut self, conversation_id: ConversationId, state: TypingStateV2) -> bool {
        self.reevaluate();
        if state == TypingStateV2::Started && !self.sharing {
            return false;
        }
        let Some(gateway) = self.resolve_target(conversation_id) else {
            return false;
        };
        let clock = self.session.live_typing.clock();
        match self.session.client.seal_typing_signal_v2(
            conversation_id,
            gateway,
            TypingOriginV2::VaultDevice,
            state,
            clock,
        ) {
            Ok(Some(outbound)) => {
                let json = serde_json::to_string(&outbound.signal).ok();
                self.queue(outbound.generation, json, state)
            }
            Ok(None) | Err(LiveError::Disabled | LiveError::InvalidContext) => false,
            Err(LiveError::Clock) => {
                self.retire();
                false
            }
            Err(_) => false,
        }
    }

    fn queue(
        &self,
        generation: LiveEnableGeneration,
        json: Option<String>,
        state: TypingStateV2,
    ) -> bool {
        if Some(generation) != self.state.generation() {
            return false;
        }
        let Some(json) = json else {
            return false;
        };
        let mark = SealMark {
            seq: self.enabled_seq,
            sharing_epoch: self.sharing_epoch,
            state,
        };
        self.sealed.try_send(SealedFrame { json, mark }).is_ok()
    }

    fn resolve_target(&self, conversation_id: ConversationId) -> Option<DeviceId> {
        let status = self
            .session
            .status
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        resolve_typing_target(
            &status,
            snapshot_inputs(&self.session, &status, wall_ms()),
            conversation_id,
        )
    }

    fn set_sharing(&mut self, enabled: bool) -> Result<(), ()> {
        match self.session.client.set_typing_sharing(enabled) {
            Ok(()) | Err(LiveError::Disabled) => {
                if !enabled {
                    self.sharing_epoch += 1;
                }
                self.sharing = enabled;
                Ok(())
            }
            Err(_) => Err(()),
        }
    }

    fn reevaluate(&mut self) {
        if self.retired || !self.socket_up || self.session.cancel.is_cancelled() {
            if self.state.generation().is_some() {
                self.retire();
            }
            return;
        }
        let context = live_enable_context(&self.session);
        if context == self.state.context {
            return;
        }
        let previous = self.state.generation();
        if self.apply_context(context).is_err() {
            self.disable_core();
        }
        if self.state.generation() != previous {
            self.incarnation_changed();
        }
        self.emit();
    }

    fn apply_context(&mut self, context: Option<LiveEnableContext>) -> Result<(), LiveError> {
        self.state.apply(
            &self.session.client,
            context,
            self.session.live_typing.clock(),
        )?;
        if self.state.generation().is_some() {
            self.session.client.set_typing_sharing(self.sharing)?;
        }
        Ok(())
    }

    fn disable_core(&mut self) {
        let _ = self.session.client.disable_live_typing();
        self.state = LiveTypingState::default();
    }

    fn incarnation_changed(&mut self) {
        self.enabled_seq += 1;
        self.inbound.clear();
    }

    fn retire(&mut self) {
        self.retired = true;
        self.disable_core();
        self.incarnation_changed();
        self.emit();
    }

    fn observe_suspend(&mut self) -> bool {
        let now = self.session.live_typing.suspend_now();
        let suspended = suspend_detected(self.anchor, now);
        self.anchor = now;
        if suspended {
            self.retire();
        }
        suspended
    }

    fn confirm_transmit(&mut self, mark: SealMark) -> Admission {
        self.reevaluate();
        if self.observe_suspend() {
            return Admission::Suspended;
        }
        let admitted = !self.retired
            && !self.session.cancel.is_cancelled()
            && mark.seq == self.enabled_seq
            && (mark.state != TypingStateV2::Started
                || (self.sharing && mark.sharing_epoch == self.sharing_epoch));
        if admitted {
            Admission::Transmit
        } else {
            Admission::Rejected
        }
    }

    fn inbound_frame(&mut self, json: &str) -> InboundOutcome {
        self.reevaluate();
        let clock = self.session.live_typing.clock();
        match self.session.client.open_typing_signal_v2_json(json, clock) {
            Ok(outcome) => self.publish_inbound(outcome, clock),
            Err(LiveError::Clock) => {
                self.retire();
                InboundOutcome::default()
            }
            Err(_) => InboundOutcome::default(),
        }
    }

    /// Publication is decided after the core open, so the suspend recheck and retirement order
    /// are the final boundary for this frame.
    fn publish_inbound(&mut self, mut outcome: LiveOutcome, clock: LiveClock) -> InboundOutcome {
        let suspended = self.observe_suspend();
        if suspended || self.retired || self.session.cancel.is_cancelled() {
            withhold_started(&mut outcome);
        }
        let wire = self
            .inbound
            .publish(outcome, self.state.generation(), clock);
        self.inbound.expire(clock.mono_ms);
        self.emit();
        InboundOutcome { wire, suspended }
    }

    fn expire(&mut self) {
        self.inbound.expire(self.session.live_typing.mono_ms());
        self.emit();
    }

    fn emit(&mut self) {
        let generation = self.state.generation();
        emit_events(&mut self.inbound, &self.session, generation);
    }

    fn publish(&self) {
        self.published
            .send_replace((self.enabled_seq, self.inbound.next_deadline()));
    }
}

fn withhold_started(outcome: &mut LiveOutcome) {
    if matches!(
        outcome.action,
        Some(LiveAction::Display {
            state: TypingStateV2::Started,
            ..
        })
    ) {
        outcome.action = None;
    }
}

fn live_enable_context(session: &Session) -> Option<LiveEnableContext> {
    let status = session
        .status
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    enable_context(&status, snapshot_inputs(session, &status, wall_ms()))
}

fn snapshot_inputs<'a>(
    session: &'a Session,
    status: &'a NetStatus,
    now_ms: i64,
) -> RcsSnapshotInputs<'a> {
    let unlocked = session.client.key_status().ok().is_some_and(|keys| {
        matches!(
            crate::session::encryption_state(
                session.mismatch.load(Ordering::Acquire),
                status.vault.as_ref(),
                &keys,
            )
            .0,
            crate::session::EncryptionState::Unlocked { .. }
        )
    });
    RcsSnapshotInputs {
        client: &session.client,
        now_ms,
        unlocked,
    }
}

#[tauri::command]
pub(crate) async fn send_typing(
    window: tauri::WebviewWindow,
    app_state: tauri::State<'_, AppState>,
    conversation_id: String,
    state: TypingStateV2,
) -> BridgeResult<()> {
    crate::require_main_or_composer(window.label())?;
    let session = app_state.require_session().await?;
    let Ok(conversation_id) = conversation_id.parse::<ConversationId>() else {
        return Ok(());
    };
    if window.label() != crate::tray::MAIN
        && crate::effective_composer_conversation(
            &app_state,
            window.label(),
            &session,
            Some(&conversation_id.to_string()),
        )? != Some(conversation_id)
    {
        return Ok(());
    }
    request_send_typing(&session, conversation_id, state).await;
    Ok(())
}

async fn request_send_typing(
    session: &Session,
    conversation_id: ConversationId,
    state: TypingStateV2,
) -> bool {
    let Some(commands) = session.live_typing.commands() else {
        return false;
    };
    ask(session, &commands, |reply| Cmd::SendTyping {
        conversation_id,
        state,
        reply,
    })
    .await
    .unwrap_or(false)
}

async fn request_sharing(session: &Session, enabled: bool) -> bool {
    let Some(commands) = session.live_typing.commands() else {
        return false;
    };
    matches!(
        ask(session, &commands, |reply| Cmd::SetSharing {
            enabled,
            reply
        })
        .await,
        Some(Ok(()))
    )
}

#[tauri::command]
pub(crate) async fn set_typing_sharing(
    window: tauri::WebviewWindow,
    app_state: tauri::State<'_, AppState>,
    enabled: bool,
) -> BridgeResult<()> {
    crate::require_main_or_composer(window.label())?;
    let session = app_state.require_session().await?;
    if !request_sharing(&session, enabled).await {
        return Err(sharing_failed());
    }
    session.live_context_wake.notify_one();
    Ok(())
}

fn sharing_failed() -> crate::error::BridgeError {
    crate::error::BridgeError::new(
        "typing-sharing-failed",
        "Typing sharing could not be updated.",
    )
}

type LiveSocket = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Sends one command and returns its reply. `None` means the session was cancelled or the actor is
/// gone; callers treat it as a cancelled operation.
async fn ask<T>(
    session: &Session,
    commands: &mpsc::Sender<Cmd>,
    build: impl FnOnce(oneshot::Sender<T>) -> Cmd,
) -> Option<T> {
    let (reply, received) = oneshot::channel();
    if !send_cmd(session, commands, build(reply)).await {
        return None;
    }
    tokio::select! {
        _ = session.cancel.cancelled() => None,
        value = received => value.ok(),
    }
}

async fn send_cmd(session: &Session, commands: &mpsc::Sender<Cmd>, cmd: Cmd) -> bool {
    tokio::select! {
        _ = session.cancel.cancelled() => false,
        sent = commands.send(cmd) => sent.is_ok(),
    }
}

pub(crate) fn start(session: Arc<Session>) {
    let (commands, command_rx) = mpsc::channel(COMMAND_QUEUE_LIMIT);
    let (sealed_tx, sealed_rx) = mpsc::channel(SEALED_QUEUE_LIMIT);
    let (published_tx, published_rx) = watch::channel((0, None));
    let actor = TypingActor::new(session.clone(), sealed_tx, published_tx);
    let Ok(thread) = thread::Builder::new()
        .name("live-typing-actor".into())
        .spawn(move || actor.run(command_rx))
    else {
        return;
    };
    session.live_typing.install(commands.clone());
    session.track_worker(tokio::spawn(shutdown_actor(
        session.clone(),
        commands.clone(),
        thread,
    )));
    session.track_worker(tokio::spawn(supervise(
        session.clone(),
        commands,
        sealed_rx,
        published_rx,
    )));
}

/// Joins the actor thread once the session is cancelled. The join runs as a tracked worker, so
/// session drain and its deadline cover the actor exactly as they cover the other workers.
async fn shutdown_actor(
    session: Arc<Session>,
    commands: mpsc::Sender<Cmd>,
    thread: thread::JoinHandle<()>,
) {
    session.cancel.cancelled().await;
    let _ = tokio::task::spawn_blocking(move || {
        let _ = commands.blocking_send(Cmd::Shutdown);
        let _ = thread.join();
    })
    .await;
}

async fn supervise(
    session: Arc<Session>,
    commands: mpsc::Sender<Cmd>,
    mut sealed: mpsc::Receiver<SealedFrame>,
    published: watch::Receiver<Published>,
) {
    let mut retry = LiveRetry::default();
    let mut probed = false;
    loop {
        let mut socket = match connect(&session).await {
            Ok(socket) => socket,
            Err(()) => {
                if !probed {
                    probed = true;
                    let probe = session.api.get_json::<serde_json::Value>("/v1/live", 1024);
                    tokio::select! {
                        _ = session.cancel.cancelled() => return,
                        _ = probe => {}
                    }
                }
                if !send_cmd(&session, &commands, Cmd::SocketDown).await {
                    return;
                }
                let retry_delay = retry.after_failure();
                if !wait_or_cancel(&session, live_backoff_delay(retry_delay)).await {
                    return;
                }
                continue;
            }
        };
        if !server_hello(&mut socket, &session).await {
            if !send_cmd(&session, &commands, Cmd::SocketDown).await {
                return;
            }
            let retry_delay = retry.after_failure();
            if !wait_or_cancel(&session, live_backoff_delay(retry_delay)).await {
                return;
            }
            continue;
        }
        probed = false;
        let connected_at = Instant::now();
        if !send_cmd(&session, &commands, Cmd::SocketUp).await {
            return;
        }
        let end = run_socket(&session, &commands, &mut sealed, &published, &mut socket).await;
        drop(socket);
        if end == SocketEnd::Cancelled || !send_cmd(&session, &commands, Cmd::SocketDown).await {
            return;
        }
        let retry_delay = retry.after_connection(connected_at.elapsed());
        if !wait_or_cancel(&session, live_backoff_delay(retry_delay)).await {
            return;
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SocketEnd {
    Cancelled,
    Lost,
}

async fn run_socket(
    session: &Session,
    commands: &mpsc::Sender<Cmd>,
    sealed: &mut mpsc::Receiver<SealedFrame>,
    published: &watch::Receiver<Published>,
    socket: &mut LiveSocket,
) -> SocketEnd {
    // The periodic deadline is retained across socket traffic so a busy connection cannot defer
    // suspend detection indefinitely.
    let mut suspend_tick = tokio::time::interval(LIVE_SUSPEND_TICK);
    suspend_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let deadline = published.borrow().1;
        let expiry_delay = deadline.map(|deadline| {
            Duration::from_millis(deadline.saturating_sub(session.live_typing.mono_ms()))
        });
        tokio::select! {
            _ = session.cancel.cancelled() => return SocketEnd::Cancelled,
            _ = session.live_context_wake.notified() => {
                if !send_cmd(session, commands, Cmd::Reevaluate).await {
                    return SocketEnd::Cancelled;
                }
            }
            _ = suspend_tick.tick() => {
                match ask(session, commands, |reply| Cmd::SuspendTick { reply }).await {
                    Some(false) => {}
                    Some(true) => return SocketEnd::Lost,
                    None => return SocketEnd::Cancelled,
                }
            }
            _ = async {
                match expiry_delay {
                    Some(delay) => sleep(delay).await,
                    None => std::future::pending().await,
                }
            } => {
                if !send_cmd(session, commands, Cmd::Expire).await {
                    return SocketEnd::Cancelled;
                }
            }
            frame = sealed.recv() => {
                let Some(frame) = frame else {
                    return SocketEnd::Cancelled;
                };
                match transmit(session, commands, socket, frame).await {
                    Transmit::Sent | Transmit::Dropped => {}
                    Transmit::Lost => return SocketEnd::Lost,
                    Transmit::Cancelled => return SocketEnd::Cancelled,
                }
            }
            frame = socket.next() => match frame {
                Some(Ok(Message::Text(frame))) => {
                    let Some(outcome) = ask(session, commands, |reply| Cmd::InboundFrame {
                        json: frame.to_string(),
                        reply,
                    })
                    .await
                    else {
                        return SocketEnd::Cancelled;
                    };
                    if outcome.suspended {
                        return SocketEnd::Lost;
                    }
                    if let Some(reply) = outcome.wire {
                        if !bounded_send(socket, &session.cancel, Message::Text(reply.into())).await {
                            return SocketEnd::Lost;
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return SocketEnd::Lost,
                Some(Ok(_)) => {}
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Transmit {
    Sent,
    Dropped,
    Lost,
    Cancelled,
}

/// The final boundary: readiness is awaited first, then the actor confirms the frame's authority
/// (and the suspend clock) before the frame is handed to the socket.
async fn transmit<S>(
    session: &Session,
    commands: &mpsc::Sender<Cmd>,
    socket: &mut S,
    frame: SealedFrame,
) -> Transmit
where
    S: Sink<Message> + Unpin,
{
    let ready = tokio::select! {
        _ = session.cancel.cancelled() => return Transmit::Cancelled,
        result = timeout(LIVE_IO_TIMEOUT, futures_util::future::poll_fn(|cx| Pin::new(&mut *socket).poll_ready(cx))) => result,
    };
    if !matches!(ready, Ok(Ok(()))) {
        return Transmit::Lost;
    }
    match ask(session, commands, |reply| Cmd::ConfirmTransmit {
        mark: frame.mark,
        reply,
    })
    .await
    {
        Some(Admission::Transmit) => {}
        Some(Admission::Rejected) => return Transmit::Dropped,
        Some(Admission::Suspended) => return Transmit::Lost,
        None => return Transmit::Cancelled,
    }
    if Sink::start_send(Pin::new(&mut *socket), Message::Text(frame.json.into())).is_err() {
        return Transmit::Lost;
    }
    tokio::select! {
        _ = session.cancel.cancelled() => Transmit::Cancelled,
        result = timeout(LIVE_IO_TIMEOUT, socket.flush()) => {
            if matches!(result, Ok(Ok(()))) { Transmit::Sent } else { Transmit::Lost }
        }
    }
}

async fn bounded_send<S>(
    socket: &mut WebSocketStream<S>,
    cancel: &tokio_util::sync::CancellationToken,
    message: Message,
) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::select! {
        _ = cancel.cancelled() => false,
        result = timeout(LIVE_IO_TIMEOUT, socket.send(message)) => matches!(result, Ok(Ok(()))),
    }
}

async fn connect(session: &Session) -> Result<LiveSocket, ()> {
    let request = live_request(session.api.origin(), session.api.bearer())?;
    let connector = crate::tls::websocket_connector(session.api.origin()).map_err(|_| ())?;
    let connecting = timeout(
        LIVE_CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async_tls_with_config(
            request,
            Some(
                WebSocketConfig::default()
                    .max_message_size(Some(LIVE_MAX_FRAME))
                    .max_frame_size(Some(LIVE_MAX_FRAME)),
            ),
            true,
            connector,
        ),
    );
    tokio::select! {
        _ = session.cancel.cancelled() => Err(()),
        result = connecting => result.map_err(|_| ())?.map(|(socket, _)| socket).map_err(|_| ()),
    }
}

fn live_request(origin: &str, token: &str) -> Result<tungstenite::http::Request<()>, ()> {
    let url = format!("{origin}/v1/live")
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let mut request = url.into_client_request().map_err(|_| ())?;
    let mut bearer =
        tungstenite::http::HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| ())?;
    bearer.set_sensitive(true);
    request
        .headers_mut()
        .insert(tungstenite::http::header::AUTHORIZATION, bearer);
    Ok(request)
}

async fn server_hello(socket: &mut LiveSocket, session: &Session) -> bool {
    server_hello_with_timeout(socket, &session.cancel, LIVE_HELLO_TIMEOUT).await
}

async fn server_hello_with_timeout<S>(
    socket: &mut WebSocketStream<S>,
    cancel: &tokio_util::sync::CancellationToken,
    deadline: Duration,
) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let next = tokio::select! {
        _ = cancel.cancelled() => return false,
        frame = timeout(deadline, socket.next()) => frame.ok().flatten(),
    };
    matches!(next, Some(Ok(Message::Text(text))) if serde_json::from_str::<LiveHelloV2>(&text).ok() == Some(LiveHelloV2::new()))
}

fn clock(started: Instant) -> LiveClock {
    LiveClock {
        wall_ms: wall_ms(),
        mono_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
    }
}

fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| duration.as_millis().try_into().ok())
        .unwrap_or(0)
}

fn suspend_detected(previous: (Instant, SystemTime), now: (Instant, SystemTime)) -> bool {
    let monotonic_elapsed = now.0.duration_since(previous.0);
    let Ok(wall_elapsed) = now.1.duration_since(previous.1) else {
        return true;
    };
    wall_elapsed > monotonic_elapsed.saturating_add(LIVE_SUSPEND_SKEW)
        || monotonic_elapsed > LIVE_SUSPEND_TICK.saturating_add(LIVE_SUSPEND_SKEW)
}

async fn wait_or_cancel(session: &Session, delay: Duration) -> bool {
    tokio::select! { _ = session.cancel.cancelled() => false, _ = sleep(delay) => true }
}

fn live_backoff_delay(backoff: Duration) -> Duration {
    jitter(backoff).min(LIVE_MAX_BACKOFF)
}

struct LiveRetry {
    backoff: Duration,
}

impl Default for LiveRetry {
    fn default() -> Self {
        Self {
            backoff: Duration::from_secs(1),
        }
    }
}

impl LiveRetry {
    fn after_failure(&mut self) -> Duration {
        self.after_connection(Duration::ZERO)
    }

    fn after_connection(&mut self, connected_for: Duration) -> Duration {
        if connected_for >= crate::sync::STABLE_CONNECTION {
            self.backoff = Duration::from_secs(1);
            return self.backoff;
        }
        let retry_delay = self.backoff;
        self.backoff = next_live_backoff(self.backoff, false);
        retry_delay
    }
}

fn next_live_backoff(backoff: Duration, stable: bool) -> Duration {
    if stable {
        Duration::from_secs(1)
    } else {
        (backoff * 2).min(LIVE_MAX_BACKOFF)
    }
}

#[cfg(test)]
fn process_inbound_frame(
    client: &Client,
    inbound: &mut InboundTyping,
    frame_json: &str,
    generation: Option<LiveEnableGeneration>,
    clock: LiveClock,
) -> Result<Option<String>, LiveError> {
    let outcome = client.open_typing_signal_v2_json(frame_json, clock)?;
    let reply = inbound.publish(outcome, generation, clock);
    inbound.expire(clock.mono_ms);
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        session::Session,
        sync::apply_gateway_refresh,
        tests::{fixture_at, gateway, open, PHRASE},
    };
    use peppy_client_core::{
        RcsCapabilityState, RcsEligibility, RcsEligibilityProjection, RcsLinkStatusEvent,
    };
    use peppy_desktop_api::{
        gateways::{CapabilitiesResponse, DevicesResponse},
        RcsConversationEligibility, RcsGatewayStatus,
    };
    use tokio::net::TcpListener;

    const NOW_MS: i64 = 10_000;

    fn inputs(session: &Session) -> RcsSnapshotInputs<'_> {
        RcsSnapshotInputs {
            client: &session.client,
            now_ms: NOW_MS,
            unlocked: true,
        }
    }

    fn clock() -> LiveClock {
        LiveClock {
            wall_ms: NOW_MS,
            mono_ms: 1,
        }
    }

    fn roster_refresh(session: &Session, device_id: &str, revoked: bool) {
        let devices: DevicesResponse = serde_json::from_value(serde_json::json!({"devices": [
            {"device_id": device_id, "role": "gateway", "revoked": revoked}
        ]}))
        .unwrap();
        let capabilities: CapabilitiesResponse =
            serde_json::from_value(serde_json::json!({"capabilities": []})).unwrap();
        apply_gateway_refresh(session, &devices, &capabilities);
    }

    fn install_gateway(session: &Session, device_id: &str) {
        session.set_status(|status| {
            let mut view = gateway(device_id, true, false);
            view.simulated = false;
            status.gateways = vec![view];
        });
    }

    fn session_with_linked_eligible_gateway() -> (crate::tests::Fixture, Session, ConversationId) {
        session_with_linked_eligible_gateway_at("http://127.0.0.1:9")
    }

    fn session_with_linked_eligible_gateway_at(
        origin: &str,
    ) -> (crate::tests::Fixture, Session, ConversationId) {
        let fixture = fixture_at(origin);
        let session = open(&fixture, &fixture.binding, &[]);
        session
            .client
            .unlock(&fixture.profile, &fixture.header, PHRASE)
            .unwrap();
        let gateway_device_id: DeviceId = fixture.binding.device_id.parse().unwrap();
        let conversation_id = ConversationId::new();
        session
            .client
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    gateway_device_id,
                    state: RcsCapabilityState::Linked,
                    availability: true,
                    health: true,
                    phone_responsive: true,
                    observed_at_ms: NOW_MS,
                    capability_revision: 1,
                    feature_bits: 1,
                    linked_since_ms: Some(NOW_MS),
                },
                &[RcsEligibilityProjection {
                    conversation_id,
                    eligibility: RcsEligibility::Eligible,
                    feature_bits: 1,
                    mapping_revision: 1,
                    observed_at_ms: NOW_MS,
                    gateway_device_id,
                }],
                NOW_MS,
            )
            .unwrap();
        install_gateway(&session, &fixture.binding.device_id);
        (fixture, session, conversation_id)
    }

    fn real_supervisor_session(
        origin: &str,
    ) -> (crate::tests::Fixture, Arc<Session>, ConversationId) {
        let (fixture, session, conversation_id) = session_with_linked_eligible_gateway_at(origin);
        let gateway_device_id: DeviceId = fixture.binding.device_id.parse().unwrap();
        let now_ms = wall_ms();
        session
            .client
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    gateway_device_id,
                    state: RcsCapabilityState::Linked,
                    availability: true,
                    health: true,
                    phone_responsive: true,
                    observed_at_ms: now_ms,
                    capability_revision: 2,
                    feature_bits: 1,
                    linked_since_ms: Some(now_ms),
                },
                &[RcsEligibilityProjection {
                    conversation_id,
                    eligibility: RcsEligibility::Eligible,
                    feature_bits: 1,
                    mapping_revision: 2,
                    observed_at_ms: now_ms,
                    gateway_device_id,
                }],
                now_ms,
            )
            .unwrap();
        roster_refresh(&session, &fixture.binding.device_id, false);
        install_gateway(&session, &fixture.binding.device_id);
        (fixture, Arc::new(session), conversation_id)
    }

    /// Drives one actor synchronously: no thread, no socket, the test owns the message order.
    struct Harness {
        actor: TypingActor,
        sealed: mpsc::Receiver<SealedFrame>,
    }

    fn harness(session: &Arc<Session>) -> Harness {
        let (sealed_tx, sealed) = mpsc::channel(SEALED_QUEUE_LIMIT);
        let (published_tx, _published) = watch::channel((0, None));
        Harness {
            actor: TypingActor::new(session.clone(), sealed_tx, published_tx),
            sealed,
        }
    }

    impl Harness {
        fn send(&mut self, cmd: Cmd) {
            self.actor.handle(cmd);
        }

        fn call<T>(&mut self, build: impl FnOnce(oneshot::Sender<T>) -> Cmd) -> T {
            let (reply, mut received) = oneshot::channel();
            self.actor.handle(build(reply));
            received.try_recv().expect("actor replied to the command")
        }

        fn send_typing(&mut self, conversation_id: ConversationId, state: TypingStateV2) -> bool {
            self.call(|reply| Cmd::SendTyping {
                conversation_id,
                state,
                reply,
            })
        }

        fn sharing(&mut self, enabled: bool) -> Result<(), ()> {
            self.call(|reply| Cmd::SetSharing { enabled, reply })
        }

        fn confirm(&mut self, mark: SealMark) -> Admission {
            self.call(|reply| Cmd::ConfirmTransmit { mark, reply })
        }

        fn next_mark(&mut self) -> SealMark {
            self.sealed
                .try_recv()
                .expect("a sealed frame was queued")
                .mark
        }
    }

    #[test]
    fn no_roster_does_not_enable_live_typing() {
        let (_, session, _) = session_with_linked_eligible_gateway();
        let status = session.status.lock().unwrap();
        let mut state = LiveTypingState::default();
        assert_eq!(
            state
                .apply(
                    &session.client,
                    enable_context(&status, inputs(&session)),
                    clock()
                )
                .unwrap(),
            None
        );
        assert_eq!(
            session.client.set_typing_sharing(true),
            Err(LiveError::Disabled)
        );
    }

    #[test]
    fn resident_keys_do_not_authorize_typing_when_displayed_encryption_is_unavailable() {
        let (fixture, session, conversation) = session_with_linked_eligible_gateway();
        roster_refresh(&session, &fixture.binding.device_id, false);
        install_gateway(&session, &fixture.binding.device_id);

        session.mismatch.store(true, Ordering::Release);
        let status = session.status.lock().unwrap();
        let inputs = snapshot_inputs(&session, &status, NOW_MS);
        assert!(resolve_typing_target(&status, inputs, conversation).is_none());
        assert!(enable_context(&status, snapshot_inputs(&session, &status, NOW_MS)).is_none());
        drop(status);

        session.mismatch.store(false, Ordering::Release);
        let active_epoch = session.client.key_status().unwrap().active_epoch.unwrap();
        session.set_status(|status| {
            status.vault = Some(crate::session::VaultSummary {
                epoch: active_epoch + 1,
                fingerprint: "synthetic-fingerprint".into(),
            });
        });
        let status = session.status.lock().unwrap();
        let inputs = snapshot_inputs(&session, &status, NOW_MS);
        assert!(resolve_typing_target(&status, inputs, conversation).is_none());
        assert!(enable_context(&status, snapshot_inputs(&session, &status, NOW_MS)).is_none());
    }

    #[test]
    fn live_request_targets_v2_endpoint_with_device_bearer() {
        let request = live_request("https://example.test", "device-token").unwrap();
        assert_eq!(request.uri(), "wss://example.test/v1/live");
        assert_eq!(
            request.headers()[tungstenite::http::header::AUTHORIZATION],
            "Bearer device-token"
        );
        assert_eq!(LiveHelloV2::new().protocol_version, 2);
    }

    #[test]
    fn probe_failure_backoff_is_jittered_and_capped() {
        assert_eq!(next_live_backoff(LIVE_MAX_BACKOFF, false), LIVE_MAX_BACKOFF);
        assert_eq!(
            next_live_backoff(LIVE_MAX_BACKOFF, true),
            Duration::from_secs(1)
        );
        assert!(live_backoff_delay(LIVE_MAX_BACKOFF) <= LIVE_MAX_BACKOFF);
    }

    #[tokio::test]
    async fn silent_peer_misses_the_hello_deadline() {
        let (stream, _silent_peer) = tokio::io::duplex(64);
        let mut socket =
            WebSocketStream::from_raw_socket(stream, tungstenite::protocol::Role::Client, None)
                .await;
        let cancel = tokio_util::sync::CancellationToken::new();

        assert!(!server_hello_with_timeout(&mut socket, &cancel, Duration::from_millis(10),).await);
    }

    #[test]
    fn disable_clears_inbound_display_state() {
        let peer = DeviceId::new();
        let conversation = ConversationId::new();
        let mut inbound = InboundTyping::default();
        inbound.update_peer(PeerSessionUpdate {
            peer_device_id: peer,
            replaced_session_token: None,
            current_session_token: 1,
            validation_revision: 1,
        });
        inbound.publish(
            display_outcome(peer, conversation, 1, 1, 1, 10),
            Some(LiveEnableGeneration(1)),
            clock(),
        );
        inbound.clear();
        assert!(inbound.displays.is_empty());
        assert!(inbound.peers.is_empty());
    }

    fn display_outcome(
        peer: DeviceId,
        conversation: ConversationId,
        generation: u64,
        token: u64,
        sequence: u64,
        deadline: u64,
    ) -> LiveOutcome {
        display_outcome_with_state(
            peer,
            conversation,
            generation,
            token,
            sequence,
            deadline,
            TypingStateV2::Started,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn display_outcome_with_state(
        peer: DeviceId,
        conversation: ConversationId,
        generation: u64,
        token: u64,
        sequence: u64,
        deadline: u64,
        state: TypingStateV2,
    ) -> LiveOutcome {
        LiveOutcome {
            generation: LiveEnableGeneration(generation),
            session_update: None,
            reply: None,
            action: Some(LiveAction::Display {
                peer_device_id: peer,
                session_token: token,
                sequence,
                conversation_id: conversation,
                state,
                deadline_mono: deadline,
            }),
        }
    }

    #[test]
    fn publication_fence_drops_stale_generation_retired_tokens_and_replayed_sequences() {
        let peer = DeviceId::new();
        let conversation = ConversationId::new();
        let mut inbound = InboundTyping::default();
        inbound.update_peer(PeerSessionUpdate {
            peer_device_id: peer,
            replaced_session_token: None,
            current_session_token: 2,
            validation_revision: 1,
        });
        inbound.publish(
            display_outcome(peer, conversation, 1, 2, 2, 10),
            Some(LiveEnableGeneration(2)),
            clock(),
        );
        assert!(inbound.displays.is_empty());
        inbound.publish(
            display_outcome(peer, conversation, 2, 1, 2, 10),
            Some(LiveEnableGeneration(2)),
            clock(),
        );
        assert!(inbound.displays.is_empty());
        inbound.publish(
            display_outcome(peer, conversation, 2, 2, 2, 10),
            Some(LiveEnableGeneration(2)),
            clock(),
        );
        assert_eq!(inbound.displays.len(), 1);
        inbound.publish(
            display_outcome(peer, conversation, 2, 2, 2, 11),
            Some(LiveEnableGeneration(2)),
            clock(),
        );
        assert_eq!(inbound.displays[&conversation].deadline_mono, 10);
        assert_eq!(
            inbound.displays[&conversation].expires_at_ms.as_deref(),
            Some("10009")
        );
    }

    #[test]
    fn typing_events_follow_the_current_composer_mapping() {
        let source = ConversationId::new();
        let moved_to = ConversationId::new();
        let binding = crate::credentials::Binding {
            origin: "https://example.test".into(),
            vault_id: "vault".into(),
            device_id: "device".into(),
        };
        let moved = crate::composer_scope::MovedScope {
            binding: binding.clone(),
            conversation: moved_to,
        };
        let source_label = format!("{}{}", crate::tray::COMPOSER_PREFIX, source);
        let target_label = format!("{}{}", crate::tray::COMPOSER_PREFIX, moved_to);

        assert_eq!(
            typing_event_targets(
                moved_to,
                &binding,
                [
                    (source_label.as_str(), Some(&moved)),
                    (target_label.as_str(), None),
                ],
            ),
            vec![crate::tray::MAIN.to_owned(), source_label, target_label]
        );
    }

    #[test]
    fn clearing_an_active_display_queues_a_stopped_window_event() {
        let peer = DeviceId::new();
        let conversation = ConversationId::new();
        let mut inbound = InboundTyping::default();
        inbound.update_peer(PeerSessionUpdate {
            peer_device_id: peer,
            replaced_session_token: None,
            current_session_token: 1,
            validation_revision: 1,
        });
        inbound.publish(
            display_outcome(peer, conversation, 1, 1, 1, 10),
            Some(LiveEnableGeneration(1)),
            clock(),
        );
        inbound.events.clear();

        inbound.clear();

        let event = inbound.drain_events().next().unwrap();
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({
                "conversationId": conversation,
                "state": "stopped"
            })
        );
    }

    #[test]
    fn peer_replacement_clears_owned_displays_and_rejects_stale_revisions() {
        let peer = DeviceId::new();
        let conversation = ConversationId::new();
        let mut inbound = InboundTyping::default();
        assert!(inbound.update_peer(PeerSessionUpdate {
            peer_device_id: peer,
            replaced_session_token: None,
            current_session_token: 1,
            validation_revision: 2,
        }));
        inbound.publish(
            display_outcome(peer, conversation, 1, 1, 1, 20),
            Some(LiveEnableGeneration(1)),
            clock(),
        );
        inbound.events.clear();

        assert!(inbound.update_peer(PeerSessionUpdate {
            peer_device_id: peer,
            replaced_session_token: Some(1),
            current_session_token: 2,
            validation_revision: 3,
        }));
        assert!(!inbound.displays.contains_key(&conversation));
        let stopped = inbound.drain_events().next().unwrap();
        assert_eq!(stopped.conversation_id, conversation);
        assert_eq!(stopped.state, TypingStateV2::Stopped);

        assert!(!inbound.update_peer(PeerSessionUpdate {
            peer_device_id: peer,
            replaced_session_token: Some(2),
            current_session_token: 3,
            validation_revision: 3,
        }));
        assert_eq!(inbound.peers[&peer], (2, 3));
    }

    #[test]
    fn pruned_peer_install_without_a_replaced_token_clears_fences_for_a_lower_sequence() {
        let peer = DeviceId::new();
        let conversation = ConversationId::new();
        let mut inbound = InboundTyping::default();
        assert!(inbound.update_peer(PeerSessionUpdate {
            peer_device_id: peer,
            replaced_session_token: None,
            current_session_token: 1,
            validation_revision: 1,
        }));
        inbound.publish(
            display_outcome(peer, conversation, 1, 1, 9, 20),
            Some(LiveEnableGeneration(1)),
            clock(),
        );
        inbound.events.clear();

        assert!(inbound.update_peer(PeerSessionUpdate {
            peer_device_id: peer,
            replaced_session_token: None,
            current_session_token: 2,
            validation_revision: 2,
        }));
        inbound.publish(
            display_outcome(peer, conversation, 1, 2, 1, 20),
            Some(LiveEnableGeneration(1)),
            clock(),
        );

        assert_eq!(inbound.displays[&conversation].session_token, 2);
        assert_eq!(inbound.sequences.len(), 1);
        assert_eq!(inbound.peers.len(), 1);
    }

    #[test]
    fn repeated_pruned_peer_installs_keep_host_fences_bounded() {
        let peer = DeviceId::new();
        let mut inbound = InboundTyping::default();
        for revision in 1..=256 {
            assert!(inbound.update_peer(PeerSessionUpdate {
                peer_device_id: peer,
                replaced_session_token: None,
                current_session_token: revision,
                validation_revision: revision,
            }));
            let conversation = ConversationId::new();
            inbound.publish(
                display_outcome(peer, conversation, 1, revision, 1, 20),
                Some(LiveEnableGeneration(1)),
                clock(),
            );
        }

        assert_eq!(inbound.peers.len(), 1);
        assert_eq!(inbound.sequences.len(), 1);
        assert_eq!(inbound.displays.len(), 1);
    }

    #[test]
    fn next_display_expiry_uses_the_earliest_active_deadline() {
        let peer = DeviceId::new();
        let first = ConversationId::new();
        let second = ConversationId::new();
        let mut inbound = InboundTyping::default();
        inbound.update_peer(PeerSessionUpdate {
            peer_device_id: peer,
            replaced_session_token: None,
            current_session_token: 1,
            validation_revision: 1,
        });
        inbound.publish(
            display_outcome(peer, first, 1, 1, 1, 20),
            Some(LiveEnableGeneration(1)),
            clock(),
        );
        inbound.publish(
            display_outcome(peer, second, 1, 1, 2, 10),
            Some(LiveEnableGeneration(1)),
            clock(),
        );

        assert_eq!(inbound.next_deadline(), Some(10));
    }

    #[test]
    fn emit_fence_drops_cancelled_stale_and_expired_starts() {
        let peer = DeviceId::new();
        let conversation = ConversationId::new();
        let mut inbound = InboundTyping::default();
        inbound.update_peer(PeerSessionUpdate {
            peer_device_id: peer,
            replaced_session_token: None,
            current_session_token: 1,
            validation_revision: 1,
        });
        inbound.publish(
            display_outcome(peer, conversation, 1, 1, 1, 10),
            Some(LiveEnableGeneration(1)),
            clock(),
        );
        assert!(
            drain_publishable_events(&mut inbound, Some(LiveEnableGeneration(2)), false, 1,)
                .is_empty()
        );
        inbound.publish(
            display_outcome(peer, conversation, 1, 1, 2, 10),
            Some(LiveEnableGeneration(1)),
            clock(),
        );
        assert!(
            drain_publishable_events(&mut inbound, Some(LiveEnableGeneration(1)), true, 1,)
                .is_empty()
        );
        inbound.publish(
            display_outcome(peer, conversation, 1, 1, 3, 10),
            Some(LiveEnableGeneration(1)),
            clock(),
        );
        assert!(
            drain_publishable_events(&mut inbound, Some(LiveEnableGeneration(1)), false, 10,)
                .is_empty()
        );
    }

    #[test]
    fn expiry_and_gateway_intent_never_publish() {
        let peer = DeviceId::new();
        let conversation = ConversationId::new();
        let mut inbound = InboundTyping::default();
        inbound.update_peer(PeerSessionUpdate {
            peer_device_id: peer,
            replaced_session_token: None,
            current_session_token: 1,
            validation_revision: 1,
        });
        inbound.publish(
            display_outcome(peer, conversation, 1, 1, 1, 10),
            Some(LiveEnableGeneration(1)),
            clock(),
        );
        inbound.expire(10);
        assert!(inbound.displays.is_empty());
        let outcome = LiveOutcome {
            generation: LiveEnableGeneration(1),
            session_update: None,
            reply: None,
            action: Some(LiveAction::GatewayTypingIntent {
                peer_device_id: peer,
                session_token: 1,
                sequence: 2,
                conversation_id: conversation,
                state: TypingStateV2::Started,
                deadline_mono: 11,
            }),
        };
        inbound.publish(outcome, Some(LiveEnableGeneration(1)), clock());
        assert!(inbound.displays.is_empty());
        assert_eq!(inbound.gateway_intent_drops, 1);
    }

    fn upload(client: &Client) -> Vec<peppy_client_core::Envelope> {
        let pending = client.pending_outbox().unwrap();
        for envelope in &pending {
            client.ack_outbox(envelope.envelope_id).unwrap();
        }
        pending
    }

    fn sync(client: &Client, envelopes: &[peppy_client_core::Envelope]) {
        for (index, envelope) in envelopes.iter().enumerate() {
            let cursor = peppy_client_core::Cursor(index as u64 + 1);
            let result = client.ingest(envelope, cursor).unwrap();
            assert!(matches!(
                result,
                peppy_client_core::IngestResult::Journaled
                    | peppy_client_core::IngestResult::Duplicate
            ));
        }
        client.apply_pending(100).unwrap();
    }

    #[test]
    fn real_clients_complete_challenge_ack_then_publish_display() {
        use peppy_client_core::{ClientConfig, DatabaseKey, KeyProfile, VaultId};
        use peppy_crypto::{create_vault_check_header, derive_root_key};
        use peppy_protocol::{TypingOriginV2, TypingStateV2};
        let directory = tempfile::tempdir().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key(PHRASE, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let sender_config = ClientConfig {
            database_path: directory.path().join("a.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let receiver_config = ClientConfig {
            database_path: directory.path().join("b.db"),
            vault_id,
            device_id: DeviceId::new(),
        };
        let sender =
            Client::open(sender_config.clone(), DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
        let receiver =
            Client::open(receiver_config.clone(), DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
        for client in [&sender, &receiver] {
            client.unlock(&profile, &header, PHRASE).unwrap();
        }
        let conversation = ConversationId::new();
        sender
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    gateway_device_id: sender_config.device_id,
                    state: RcsCapabilityState::Linked,
                    availability: true,
                    health: true,
                    phone_responsive: true,
                    observed_at_ms: 1_000,
                    capability_revision: 1,
                    feature_bits: 1,
                    linked_since_ms: Some(1_000),
                },
                &[RcsEligibilityProjection {
                    conversation_id: conversation,
                    eligibility: RcsEligibility::Eligible,
                    feature_bits: 1,
                    mapping_revision: 1,
                    observed_at_ms: 1_000,
                    gateway_device_id: sender_config.device_id,
                }],
                1_000,
            )
            .unwrap();
        sync(&receiver, &upload(&sender));

        let roster = vec![sender_config.device_id, receiver_config.device_id];
        for (client, local_executor) in [
            (&sender, LiveExecutor::CurrentGateway),
            (&receiver, LiveExecutor::None),
        ] {
            client
                .enable_live_typing(
                    LiveEnableContext {
                        trusted_roster: roster.clone(),
                        local_gateway_device_id: sender_config.device_id,
                        local_executor,
                    },
                    LiveClock {
                        wall_ms: 1_000,
                        mono_ms: 1_000,
                    },
                )
                .unwrap();
        }
        let first = sender
            .seal_typing_signal_v2(
                conversation,
                sender_config.device_id,
                TypingOriginV2::GoogleContact,
                TypingStateV2::Started,
                LiveClock {
                    wall_ms: 1_001,
                    mono_ms: 1_001,
                },
            )
            .unwrap()
            .unwrap();
        let mut inbound = InboundTyping::default();
        let challenge = process_inbound_frame(
            &receiver,
            &mut inbound,
            &serde_json::to_string(&first.signal).unwrap(),
            Some(LiveEnableGeneration(1)),
            LiveClock {
                wall_ms: 1_001,
                mono_ms: 1_001,
            },
        )
        .unwrap()
        .unwrap();
        let ack = process_inbound_frame(
            &sender,
            &mut InboundTyping::default(),
            &challenge,
            Some(LiveEnableGeneration(1)),
            LiveClock {
                wall_ms: 1_002,
                mono_ms: 1_002,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            process_inbound_frame(
                &receiver,
                &mut inbound,
                &ack,
                Some(LiveEnableGeneration(1)),
                LiveClock {
                    wall_ms: 1_003,
                    mono_ms: 1_003,
                },
            ),
            Ok(None)
        );
        let second = sender
            .seal_typing_signal_v2(
                conversation,
                sender_config.device_id,
                TypingOriginV2::GoogleContact,
                TypingStateV2::Started,
                LiveClock {
                    wall_ms: 4_001,
                    mono_ms: 4_001,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            process_inbound_frame(
                &receiver,
                &mut inbound,
                &serde_json::to_string(&second.signal).unwrap(),
                Some(LiveEnableGeneration(1)),
                LiveClock {
                    wall_ms: 4_001,
                    mono_ms: 4_001,
                },
            ),
            Ok(None)
        );
        assert!(inbound.displays.contains_key(&conversation));
    }

    #[test]
    fn host_clock_uses_one_monotonic_origin() {
        let fixture = fixture_at("http://127.0.0.1:9");
        let session = open(&fixture, &fixture.binding, &[]);
        let first = session.live_typing.clock();
        std::thread::sleep(Duration::from_millis(2));
        let second = session.live_typing.clock();

        assert!(second.mono_ms > first.mono_ms);
    }

    #[test]
    fn suspend_detector_retires_on_wall_or_monotonic_gap() {
        let mono = Instant::now();
        let wall = SystemTime::now();
        assert!(suspend_detected(
            (mono, wall),
            (
                mono + LIVE_SUSPEND_TICK,
                wall + LIVE_SUSPEND_TICK + LIVE_SUSPEND_SKEW + Duration::from_millis(1)
            ),
        ));
        assert!(suspend_detected(
            (mono, wall),
            (
                mono + LIVE_SUSPEND_TICK + LIVE_SUSPEND_SKEW + Duration::from_millis(1),
                wall + LIVE_SUSPEND_TICK
            ),
        ));
        assert!(!suspend_detected(
            (mono, wall),
            (mono + LIVE_SUSPEND_TICK, wall + LIVE_SUSPEND_TICK),
        ));
    }

    #[test]
    fn socket_up_enables_and_socket_down_retires_with_a_new_generation_on_reconnect() {
        let (_, session, _) = real_supervisor_session("http://127.0.0.1:9");
        let mut h = harness(&session);
        h.send(Cmd::SocketUp);
        let first = h.actor.state.generation();
        assert!(first.is_some());
        h.send(Cmd::SocketDown);
        assert_eq!(h.actor.state.generation(), None);
        assert_eq!(
            session.client.set_typing_sharing(true),
            Err(LiveError::Disabled)
        );
        h.send(Cmd::SocketUp);
        assert_ne!(first, h.actor.state.generation());
    }

    #[test]
    fn repeated_unstable_connections_escalate_backoff_without_waiting() {
        let mut retry = LiveRetry::default();
        let observed: Vec<_> = (0..7)
            .map(|_| retry.after_connection(Duration::ZERO))
            .collect();
        assert_eq!(observed, [1, 2, 4, 8, 16, 32, 64].map(Duration::from_secs));
    }

    #[test]
    fn disconnected_time_does_not_make_an_unstable_connection_stable() {
        #[derive(Default)]
        struct FakeClock(Duration);

        impl FakeClock {
            fn now(&self) -> Duration {
                self.0
            }

            fn advance(&mut self, duration: Duration) {
                self.0 += duration;
            }
        }

        let mut retry = LiveRetry::default();
        let mut clock = FakeClock::default();
        let first_connected_at = clock.now();
        clock.advance(Duration::from_millis(1));
        assert_eq!(
            retry.after_connection(clock.now() - first_connected_at),
            Duration::from_secs(1)
        );

        clock.advance(Duration::from_secs(64));
        let second_connected_at = clock.now();
        clock.advance(Duration::from_millis(1));
        assert_eq!(
            retry.after_connection(clock.now() - second_connected_at),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn a_stable_connection_resets_the_current_retry_immediately() {
        let mut retry = LiveRetry::default();
        for _ in 0..10 {
            retry.after_connection(Duration::ZERO);
        }

        assert_eq!(
            retry.after_connection(crate::sync::STABLE_CONNECTION),
            Duration::from_secs(1)
        );
        assert_eq!(
            retry.after_connection(Duration::ZERO),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn ineligible_conversation_has_no_target() {
        let (_, session, _) = session_with_linked_eligible_gateway();
        let status = session.status.lock().unwrap();
        assert!(resolve_typing_target(&status, inputs(&session), ConversationId::new()).is_none());
    }

    #[test]
    fn ambiguous_gateways_have_no_target() {
        let conversation_id = ConversationId::new();
        let gateway_a = DeviceId::new();
        let gateway_b = DeviceId::new();
        let snapshot = RcsStatusSnapshot {
            gateways: vec![gateway_status(gateway_a), gateway_status(gateway_b)],
            eligibilities: vec![
                eligibility(conversation_id, gateway_a),
                eligibility(conversation_id, gateway_b),
            ],
        };
        assert!(typing_target(&snapshot, conversation_id).is_none());
    }

    #[test]
    fn gateway_refresh_roster_enables_live_typing() {
        let (fixture, session, _) = session_with_linked_eligible_gateway();
        roster_refresh(&session, &fixture.binding.device_id, false);
        install_gateway(&session, &fixture.binding.device_id);
        let status = session.status.lock().unwrap();
        let context = enable_context(&status, inputs(&session)).unwrap();
        assert_eq!(
            context.local_executor,
            peppy_client_core::LiveExecutor::None
        );
        assert_eq!(context.trusted_roster.len(), 1);
        let generation = LiveTypingState::default()
            .apply(&session.client, Some(context), clock())
            .unwrap();
        assert!(generation.is_some());
    }

    #[test]
    fn revocation_disables_live_typing() {
        let (fixture, session, _) = session_with_linked_eligible_gateway();
        roster_refresh(&session, &fixture.binding.device_id, false);
        install_gateway(&session, &fixture.binding.device_id);
        let mut state = LiveTypingState::default();
        let context = {
            let status = session.status.lock().unwrap();
            enable_context(&status, inputs(&session))
        };
        state.apply(&session.client, context, clock()).unwrap();
        roster_refresh(&session, &fixture.binding.device_id, true);
        let context = {
            let status = session.status.lock().unwrap();
            enable_context(&status, inputs(&session))
        };
        state.apply(&session.client, context, clock()).unwrap();
        assert_eq!(state.generation(), None);
        assert_eq!(
            session.client.set_typing_sharing(true),
            Err(LiveError::Disabled)
        );
    }

    #[test]
    fn roster_change_creates_a_new_generation() {
        let (fixture, session, _) = session_with_linked_eligible_gateway();
        roster_refresh(&session, &fixture.binding.device_id, false);
        install_gateway(&session, &fixture.binding.device_id);
        let mut state = LiveTypingState::default();
        let first = {
            let status = session.status.lock().unwrap();
            state.apply(
                &session.client,
                enable_context(&status, inputs(&session)),
                clock(),
            )
        }
        .unwrap()
        .unwrap();
        session.set_status(|status| status.rcs_live_roster.push(DeviceId::new().to_string()));
        let second = {
            let status = session.status.lock().unwrap();
            state.apply(
                &session.client,
                enable_context(&status, inputs(&session)),
                clock(),
            )
        }
        .unwrap()
        .unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn sealed_start_is_rejected_after_gateway_revocation() {
        let (fixture, session, conversation) = real_supervisor_session("http://127.0.0.1:9");
        let mut h = harness(&session);
        h.send(Cmd::SocketUp);
        assert!(h.send_typing(conversation, TypingStateV2::Started));
        let mark = h.next_mark();

        roster_refresh(&session, &fixture.binding.device_id, true);
        h.send(Cmd::GatewayRefresh);

        assert_eq!(h.confirm(mark), Admission::Rejected);
        assert_eq!(
            session.client.set_typing_sharing(true),
            Err(LiveError::Disabled)
        );
    }

    #[test]
    fn gateway_refresh_replaces_the_incarnation_and_bumps_the_sequence() {
        let (_fixture, session, _) = real_supervisor_session("http://127.0.0.1:9");
        let mut h = harness(&session);
        h.send(Cmd::SocketUp);
        let first = h.actor.state.generation();
        let first_seq = h.actor.enabled_seq;

        session.set_status(|status| status.rcs_live_roster.push(DeviceId::new().to_string()));
        h.send(Cmd::GatewayRefresh);

        assert!(h.actor.state.generation().is_some());
        assert_ne!(first, h.actor.state.generation());
        assert!(h.actor.enabled_seq > first_seq);
        let outcome = h.call(|reply| Cmd::InboundFrame {
            json: "{}".into(),
            reply,
        });
        assert!(outcome.wire.is_none());
        assert!(!outcome.suspended);
    }

    #[test]
    fn gateway_round_trip_converges_core_and_host_generations() {
        let (fixture, session, conversation) = real_supervisor_session("http://127.0.0.1:9");
        let gateway_id = fixture.binding.device_id.clone();
        let mut h = harness(&session);
        h.send(Cmd::SocketUp);
        let first = h.actor.state.generation();

        session.set_status(|status| {
            status.rcs_live_roster = vec![DeviceId::new().to_string()];
        });
        h.send(Cmd::GatewayRefresh);
        assert_eq!(h.actor.state.generation(), None);
        assert_eq!(
            session.client.set_typing_sharing(true),
            Err(LiveError::Disabled)
        );

        session.set_status(|status| status.rcs_live_roster = vec![gateway_id]);
        h.send(Cmd::GatewayRefresh);
        assert!(h.actor.state.generation().is_some());
        assert_ne!(first, h.actor.state.generation());

        assert!(h.send_typing(conversation, TypingStateV2::Started));
        let mark = h.next_mark();
        assert_eq!(mark.seq, h.actor.enabled_seq);
        assert_eq!(h.confirm(mark), Admission::Transmit);
    }

    #[test]
    fn pre_off_stop_is_admitted_and_post_off_frames_seal_nothing() {
        let (_fixture, session, conversation) = real_supervisor_session("http://127.0.0.1:9");
        let mut h = harness(&session);
        h.send(Cmd::SocketUp);
        assert!(h.send_typing(conversation, TypingStateV2::Started));
        let start = h.next_mark();
        assert!(h.send_typing(conversation, TypingStateV2::Stopped));
        let stop = h.next_mark();

        assert_eq!(h.sharing(false), Ok(()));
        assert_eq!(h.confirm(stop), Admission::Transmit);
        assert_eq!(h.confirm(start), Admission::Rejected);
        assert!(!h.send_typing(conversation, TypingStateV2::Started));
        assert!(!h.send_typing(conversation, TypingStateV2::Stopped));
        assert!(h.sealed.try_recv().is_err());
    }

    #[test]
    fn off_then_on_does_not_admit_a_start_sealed_before_the_off() {
        let (_fixture, session, conversation) = real_supervisor_session("http://127.0.0.1:9");
        let mut h = harness(&session);
        h.send(Cmd::SocketUp);
        assert!(h.send_typing(conversation, TypingStateV2::Started));
        let start = h.next_mark();

        assert_eq!(h.sharing(false), Ok(()));
        assert_eq!(h.sharing(true), Ok(()));

        assert_eq!(h.confirm(start), Admission::Rejected);
    }

    #[test]
    fn sharing_update_while_disabled_commits_the_preference() {
        let (_fixture, session, _) = real_supervisor_session("http://127.0.0.1:9");
        let mut h = harness(&session);
        assert_eq!(h.sharing(false), Ok(()));
        assert!(!h.actor.sharing);
        assert_eq!(h.actor.sharing_epoch, 1);
        assert_eq!(h.sharing(true), Ok(()));
        assert!(h.actor.sharing);
    }

    #[test]
    fn sharing_update_with_a_store_error_commits_nothing() {
        let (_fixture, session, _) = real_supervisor_session("http://127.0.0.1:9");
        let mut h = harness(&session);
        let client = session.client.clone();
        let poisoned = std::thread::spawn(move || {
            let _store = client.hold_store_lock_for_test().unwrap();
            panic!("poisons the synthetic store for this test");
        })
        .join();
        assert!(poisoned.is_err());

        assert_eq!(h.sharing(false), Err(()));
        assert!(h.actor.sharing);
        assert_eq!(h.actor.sharing_epoch, 0);
    }

    #[test]
    fn clock_gap_before_confirm_retires_and_rejects_the_queued_start() {
        let (_fixture, session, conversation) = real_supervisor_session("http://127.0.0.1:9");
        let clock = Arc::new(TestSuspendClock::new());
        session.live_typing.install_suspend_clock(clock.clone());
        let mut h = harness(&session);
        h.send(Cmd::SocketUp);
        assert!(h.send_typing(conversation, TypingStateV2::Started));
        let mark = h.next_mark();

        clock.set(LIVE_SUSPEND_TICK + LIVE_SUSPEND_SKEW + Duration::from_secs(2));

        assert_eq!(h.confirm(mark), Admission::Suspended);
        assert_eq!(
            session.client.set_typing_sharing(true),
            Err(LiveError::Disabled)
        );
    }

    #[test]
    fn clock_gap_before_inbound_withholds_started_and_retires_the_display() {
        let (_fixture, session, conversation) = real_supervisor_session("http://127.0.0.1:9");
        let suspend = Arc::new(TestSuspendClock::new());
        session.live_typing.install_suspend_clock(suspend.clone());
        let mut h = harness(&session);
        h.send(Cmd::SocketUp);
        let generation = h.actor.state.generation().unwrap().0;
        let peer = DeviceId::new();
        h.actor.inbound.update_peer(PeerSessionUpdate {
            peer_device_id: peer,
            replaced_session_token: None,
            current_session_token: 1,
            validation_revision: 1,
        });
        let first = h.actor.publish_inbound(
            display_outcome(peer, conversation, generation, 1, 1, 20_000),
            clock(),
        );
        assert!(!first.suspended);
        assert!(h.actor.inbound.displays.contains_key(&conversation));

        suspend.set(LIVE_SUSPEND_TICK + LIVE_SUSPEND_SKEW + Duration::from_secs(2));
        let started = h.actor.publish_inbound(
            display_outcome(peer, conversation, generation, 1, 2, 20_000),
            clock(),
        );
        assert!(started.suspended);
        assert!(!h.actor.inbound.displays.contains_key(&conversation));
        assert_eq!(
            session.client.set_typing_sharing(true),
            Err(LiveError::Disabled)
        );
    }

    #[test]
    fn mismatch_before_confirm_rejects_a_queued_start() {
        let (_fixture, session, conversation) = real_supervisor_session("http://127.0.0.1:9");
        let mut h = harness(&session);
        h.send(Cmd::SocketUp);
        assert!(h.send_typing(conversation, TypingStateV2::Started));
        let mark = h.next_mark();

        session.set_mismatch(true);

        assert_eq!(h.confirm(mark), Admission::Rejected);
    }

    #[test]
    fn server_ahead_status_before_confirm_rejects_a_queued_start() {
        let (_fixture, session, conversation) = real_supervisor_session("http://127.0.0.1:9");
        let mut h = harness(&session);
        h.send(Cmd::SocketUp);
        assert!(h.send_typing(conversation, TypingStateV2::Started));
        let mark = h.next_mark();
        let active_epoch = session.client.key_status().unwrap().active_epoch.unwrap();

        session.set_security_status(|status| {
            status.vault = Some(crate::session::VaultSummary {
                epoch: active_epoch + 1,
                fingerprint: "synthetic-fingerprint".into(),
            });
        });

        assert_eq!(h.confirm(mark), Admission::Rejected);
    }

    #[test]
    fn cancellation_before_confirm_rejects_queued_frames() {
        let (_fixture, session, conversation) = real_supervisor_session("http://127.0.0.1:9");
        let mut h = harness(&session);
        h.send(Cmd::SocketUp);
        assert!(h.send_typing(conversation, TypingStateV2::Started));
        let start = h.next_mark();
        assert!(h.send_typing(conversation, TypingStateV2::Stopped));
        let stop = h.next_mark();

        session.cancel.cancel();

        assert_eq!(h.confirm(start), Admission::Rejected);
        assert_eq!(h.confirm(stop), Admission::Rejected);
    }

    #[test]
    fn valid_start_and_queued_stop_are_admitted_in_order() {
        let (_fixture, session, conversation) = real_supervisor_session("http://127.0.0.1:9");
        let mut h = harness(&session);
        h.send(Cmd::SocketUp);
        assert!(h.send_typing(conversation, TypingStateV2::Started));
        let start = h.next_mark();
        assert_eq!(h.confirm(start), Admission::Transmit);
        assert!(h.send_typing(conversation, TypingStateV2::Stopped));
        let stop = h.next_mark();
        assert_eq!(h.confirm(stop), Admission::Transmit);
    }

    #[test]
    fn send_without_an_eligible_target_produces_no_frame() {
        let fixture = fixture_at("http://127.0.0.1:9");
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        session
            .client
            .unlock(&fixture.profile, &fixture.header, PHRASE)
            .unwrap();
        let mut h = harness(&session);
        h.send(Cmd::SocketUp);

        assert!(!h.send_typing(ConversationId::new(), TypingStateV2::Started));
        assert!(h.sealed.try_recv().is_err());
    }

    #[test]
    fn disconnected_send_is_dropped_without_replay() {
        let (_fixture, session, conversation) = real_supervisor_session("http://127.0.0.1:9");
        let mut h = harness(&session);

        assert!(!h.send_typing(conversation, TypingStateV2::Started));
        assert!(h.sealed.try_recv().is_err());
        h.send(Cmd::SocketUp);
        assert!(h.send_typing(conversation, TypingStateV2::Started));
        assert!(h.sealed.try_recv().is_ok());
    }

    async fn wait_for_live_enabled(session: &Session, expected: bool) {
        timeout(Duration::from_secs(1), async {
            loop {
                let enabled = session.client.set_typing_sharing(true).is_ok();
                if enabled == expected {
                    return;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for live typing enabled={expected}"));
    }

    #[tokio::test]
    async fn stalled_transport_io_yields_to_cancel_and_drains() {
        let (stream, _peer_that_stops_reading) = tokio::io::duplex(1);
        let socket =
            WebSocketStream::from_raw_socket(stream, tungstenite::protocol::Role::Client, None)
                .await;
        let cancel = tokio_util::sync::CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            let mut socket = socket;
            bounded_send(
                &mut socket,
                &task_cancel,
                Message::Text("x".repeat(LIVE_MAX_FRAME).into()),
            )
            .await
        });

        sleep(Duration::from_millis(10)).await;
        cancel.cancel();
        let sent = timeout(Duration::from_millis(200), task)
            .await
            .expect("stalled transport did not drain after cancellation")
            .unwrap();
        assert!(!sent);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn store_contention_keeps_runtime_and_cancellation_live_without_publication() {
        let (_fixture, session, conversation) = real_supervisor_session("http://127.0.0.1:9");
        start(session.clone());
        let store_client = session.client.clone();
        let (held, reached) = std::sync::mpsc::channel();
        let (release, unblock) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let store_lock = store_client.hold_store_lock_for_test().unwrap();
            held.send(()).unwrap();
            let _ = unblock.recv_timeout(Duration::from_secs(5));
            drop(store_lock);
        });
        reached.recv().unwrap();

        let sending_session = session.clone();
        let sending = tokio::spawn(async move {
            request_send_typing(&sending_session, conversation, TypingStateV2::Started).await
        });

        let heartbeat_started = Instant::now();
        timeout(Duration::from_millis(30), async {
            for _ in 0..3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Store contention blocked the current-thread runtime");
        assert!(heartbeat_started.elapsed() < Duration::from_millis(100));

        session.cancel.cancel();
        let sent = timeout(Duration::from_millis(200), sending)
            .await
            .expect("typing command ignored cancellation while the Store was held")
            .unwrap();
        assert!(!sent);
        let _ = release.send(());
        holder.join().unwrap();
        assert!(crate::sync::drain(&session, Duration::from_secs(5)).await);
    }

    async fn accept_live_peer(listener: &TcpListener) -> WebSocketStream<tokio::net::TcpStream> {
        let (stream, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(stream).await.unwrap()
    }

    async fn send_server_hello(socket: &mut WebSocketStream<tokio::net::TcpStream>) {
        socket
            .send(Message::Text(
                serde_json::to_string(&LiveHelloV2::new()).unwrap().into(),
            ))
            .await
            .unwrap();
    }

    async fn started_with_test_suspend_clock() -> (
        Arc<Session>,
        ConversationId,
        Arc<TestSuspendClock>,
        WebSocketStream<tokio::net::TcpStream>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (_fixture, session, conversation) = real_supervisor_session(&origin);
        let clock = Arc::new(TestSuspendClock::new());
        session.live_typing.install_suspend_clock(clock.clone());
        start(session.clone());
        let mut peer = accept_live_peer(&listener).await;
        send_server_hello(&mut peer).await;
        wait_for_live_enabled(&session, true).await;
        (session, conversation, clock, peer)
    }

    async fn send_frequent_traffic(
        peer: &mut WebSocketStream<tokio::net::TcpStream>,
        clock: &TestSuspendClock,
    ) {
        for second in 1..=10 {
            clock.set(Duration::from_secs(second));
            peer.send(Message::Ping(Vec::new().into())).await.unwrap();
            sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn frequent_socket_traffic_does_not_defer_or_trigger_suspend_retirement() {
        let (session, _, clock, mut peer) = started_with_test_suspend_clock().await;

        send_frequent_traffic(&mut peer, &clock).await;

        assert!(
            clock.sample_count() >= 3,
            "socket traffic deferred periodic suspend observations"
        );
        assert!(session.client.set_typing_sharing(true).is_ok());
        assert!(crate::sync::drain(&session, Duration::from_secs(5)).await);
    }

    #[tokio::test]
    async fn idle_after_frequent_traffic_does_not_false_retire_a_consistent_clock() {
        let (session, _, clock, mut peer) = started_with_test_suspend_clock().await;
        send_frequent_traffic(&mut peer, &clock).await;

        clock.set(Duration::from_secs(12));
        sleep(LIVE_SUSPEND_TICK + Duration::from_millis(5)).await;

        assert!(session.client.set_typing_sharing(true).is_ok());
        assert!(crate::sync::drain(&session, Duration::from_secs(5)).await);
    }

    #[tokio::test]
    async fn suspend_gap_retires_before_a_ready_outbound_frame_is_transmitted() {
        let (session, conversation, clock, mut peer) = started_with_test_suspend_clock().await;
        clock.set(Duration::from_secs(1));
        sleep(LIVE_SUSPEND_TICK + Duration::from_millis(2)).await;
        clock.set(LIVE_SUSPEND_TICK + LIVE_SUSPEND_SKEW + Duration::from_secs(2));

        assert!(request_send_typing(&session, conversation, TypingStateV2::Started).await);

        match timeout(Duration::from_millis(100), peer.next()).await {
            Err(_) => {}
            Ok(Some(Ok(Message::Text(_)))) => panic!("post-resume frame crossed the suspend fence"),
            Ok(_) => {}
        }
        timeout(Duration::from_millis(100), async {
            while session.client.set_typing_sharing(true).is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("suspend gap did not retire the live generation");
        assert!(crate::sync::drain(&session, Duration::from_secs(5)).await);
    }

    #[tokio::test]
    async fn supervisor_negotiation_failures_escalate_backoff_without_resetting() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (_, session, _) = real_supervisor_session(&origin);
        start(session.clone());
        let mut peer = accept_live_peer(&listener).await;
        let mut observed_delays = Vec::new();

        for base in [
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
        ] {
            peer.send(Message::Text("{}".into())).await.unwrap();
            let rejected_at = Instant::now();
            let (stream, _) = timeout(base * 2 + Duration::from_secs(1), listener.accept())
                .await
                .expect("supervisor did not retry after rejected hello")
                .unwrap();
            let delay = rejected_at.elapsed();
            assert!(
                delay >= base.saturating_sub(Duration::from_millis(50)),
                "retry {delay:?} was below the {base:?} backoff"
            );
            assert!(
                delay <= base + base / 4 + Duration::from_millis(500),
                "retry {delay:?} exceeded the 25% jitter bound for {base:?}"
            );
            observed_delays.push(delay);
            peer = tokio_tungstenite::accept_async(stream).await.unwrap();
        }
        assert!(observed_delays.windows(2).all(|pair| pair[1] > pair[0]));

        send_server_hello(&mut peer).await;
        wait_for_live_enabled(&session, true).await;
        assert!(crate::sync::drain(&session, Duration::from_secs(5)).await);
    }

    #[tokio::test]
    async fn accepted_hello_then_immediate_close_retains_reconnect_backoff() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (_, session, _) = real_supervisor_session(&origin);
        start(session.clone());
        let mut peer = accept_live_peer(&listener).await;
        send_server_hello(&mut peer).await;

        let closed_at = Instant::now();
        drop(peer);
        let (stream, _) = timeout(Duration::from_secs(4), listener.accept())
            .await
            .expect("supervisor did not reconnect after an unstable hello")
            .unwrap();
        assert!(
            closed_at.elapsed() >= Duration::from_secs(1).saturating_sub(Duration::from_millis(50)),
            "the first unstable reconnect did not use the initial backoff"
        );
        drop(stream);
        assert!(crate::sync::drain(&session, Duration::from_secs(5)).await);
    }

    #[tokio::test]
    async fn supervisor_socket_loss_reconnects_with_a_new_generation() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (fixture, session, conversation_id) = real_supervisor_session(&origin);
        let gateway: DeviceId = fixture.binding.device_id.parse().unwrap();
        start(session.clone());

        let mut first_peer = accept_live_peer(&listener).await;
        send_server_hello(&mut first_peer).await;
        wait_for_live_enabled(&session, true).await;
        let first_generation = session
            .client
            .seal_typing_signal_v2(
                conversation_id,
                gateway,
                peppy_protocol::TypingOriginV2::VaultDevice,
                peppy_protocol::TypingStateV2::Started,
                session.live_typing.clock(),
            )
            .unwrap()
            .unwrap()
            .generation;
        drop(first_peer);
        wait_for_live_enabled(&session, false).await;

        let mut second_peer = timeout(Duration::from_secs(2), accept_live_peer(&listener))
            .await
            .expect("supervisor did not reconnect after socket loss");
        send_server_hello(&mut second_peer).await;
        wait_for_live_enabled(&session, true).await;
        let second_generation = session
            .client
            .seal_typing_signal_v2(
                conversation_id,
                gateway,
                peppy_protocol::TypingOriginV2::VaultDevice,
                peppy_protocol::TypingStateV2::Started,
                session.live_typing.clock(),
            )
            .unwrap()
            .unwrap()
            .generation;
        assert_ne!(first_generation, second_generation);

        assert!(crate::sync::drain(&session, Duration::from_secs(5)).await);
    }

    async fn queue_sealed_for_test(session: &Session, json: String) -> bool {
        let Some(commands) = session.live_typing.commands() else {
            return false;
        };
        ask(session, &commands, |reply| Cmd::QueueSealedForTest {
            json,
            reply,
        })
        .await
        .unwrap_or(false)
    }

    #[tokio::test]
    async fn supervisor_session_drain_cancels_a_stalled_tcp_write() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (fixture, session, conversation_id) = real_supervisor_session(&origin);
        start(session.clone());

        let mut stalled_peer = accept_live_peer(&listener).await;
        send_server_hello(&mut stalled_peer).await;
        wait_for_live_enabled(&session, true).await;

        let outbound = session
            .client
            .seal_typing_signal_v2(
                conversation_id,
                fixture.binding.device_id.parse().unwrap(),
                peppy_protocol::TypingOriginV2::VaultDevice,
                peppy_protocol::TypingStateV2::Started,
                session.live_typing.clock(),
            )
            .unwrap()
            .unwrap();
        let mut payload = serde_json::to_string(&outbound.signal).unwrap();
        payload.push_str(&" ".repeat(LIVE_MAX_FRAME - payload.len()));
        let accepted = timeout(Duration::from_secs(5), async {
            let mut accepted = 0usize;
            loop {
                if queue_sealed_for_test(&session, payload.clone()).await {
                    accepted += 1;
                    assert!(accepted < 1_024, "TCP peer never applied backpressure");
                    tokio::task::yield_now().await;
                    continue;
                }
                sleep(Duration::from_millis(100)).await;
                if queue_sealed_for_test(&session, payload.clone()).await {
                    accepted += 1;
                    continue;
                }
                if accepted >= 4 {
                    return accepted;
                }
            }
        })
        .await
        .expect("failed to fill the stalled peer receive window");
        assert!(accepted >= 4);
        assert!(session.client.set_typing_sharing(true).is_ok());

        assert!(crate::sync::drain(&session, Duration::from_millis(300)).await);
        assert_eq!(
            session.client.set_typing_sharing(true),
            Err(LiveError::Disabled)
        );
        drop(stalled_peer);
    }

    #[tokio::test]
    async fn supervisor_retires_and_reenables_after_roster_refresh() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (fixture, session, conversation_id) = session_with_linked_eligible_gateway_at(&origin);
        let gateway_device_id: DeviceId = fixture.binding.device_id.parse().unwrap();
        let now_ms = wall_ms();
        session
            .client
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    gateway_device_id,
                    state: RcsCapabilityState::Linked,
                    availability: true,
                    health: true,
                    phone_responsive: true,
                    observed_at_ms: now_ms,
                    capability_revision: 2,
                    feature_bits: 1,
                    linked_since_ms: Some(now_ms),
                },
                &[RcsEligibilityProjection {
                    conversation_id,
                    eligibility: RcsEligibility::Eligible,
                    feature_bits: 1,
                    mapping_revision: 2,
                    observed_at_ms: now_ms,
                    gateway_device_id,
                }],
                now_ms,
            )
            .unwrap();
        roster_refresh(&session, &fixture.binding.device_id, false);
        install_gateway(&session, &fixture.binding.device_id);
        let session = Arc::new(session);

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket
                .send(Message::Text(
                    serde_json::to_string(&LiveHelloV2::new()).unwrap().into(),
                ))
                .await
                .unwrap();
            while socket.next().await.is_some() {}
        });
        start(session.clone());

        wait_for_live_enabled(&session, true).await;
        roster_refresh(&session, &fixture.binding.device_id, true);
        wait_for_live_enabled(&session, false).await;
        roster_refresh(&session, &fixture.binding.device_id, false);
        install_gateway(&session, &fixture.binding.device_id);
        session.notify();
        wait_for_live_enabled(&session, true).await;

        assert!(crate::sync::drain(&session, Duration::from_secs(5)).await);
        server.abort();
    }

    #[tokio::test]
    async fn cancellation_wakes_supervisor_wait_without_enabling() {
        let (_, session, _) = session_with_linked_eligible_gateway();
        session.cancel.cancel();
        assert!(!wait_or_cancel(&session, Duration::from_secs(1)).await);
        assert_eq!(
            session.client.set_typing_sharing(true),
            Err(LiveError::Disabled)
        );
    }

    fn gateway_status(gateway_id: DeviceId) -> RcsGatewayStatus {
        RcsGatewayStatus {
            gateway_id: gateway_id.to_string(),
            state: RcsGatewayState::Linked,
            current: true,
            available: true,
            healthy: true,
            phone_responsive: true,
            observed_at_ms: NOW_MS.to_string(),
            capability_revision: "1".into(),
            expires_at_ms: Some((NOW_MS + 1).to_string()),
        }
    }

    fn eligibility(
        conversation_id: ConversationId,
        gateway_id: DeviceId,
    ) -> RcsConversationEligibility {
        RcsConversationEligibility {
            conversation_id: conversation_id.to_string(),
            gateway_id: gateway_id.to_string(),
            eligibility: RcsEligibilityState::Eligible,
            observed_at_ms: NOW_MS.to_string(),
            mapping_revision: "1".into(),
            expires_at_ms: Some((NOW_MS + 1).to_string()),
        }
    }
}
