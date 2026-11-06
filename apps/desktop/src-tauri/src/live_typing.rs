use futures_util::{SinkExt, StreamExt};
use peppy_client_core::{
    Client, ConversationId, DeviceId, LiveAction, LiveClock, LiveEnableContext,
    LiveEnableGeneration, LiveError, LiveExecutor, LiveOutcome, PeerSessionUpdate,
};
use peppy_desktop_api::rcs_status::{
    snapshot_at, RcsEligibilityState, RcsGatewayState, RcsStatusSnapshot, SnapshotAccess,
};
use peppy_protocol::LiveHelloV2;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::mpsc,
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
    outbound: Mutex<Option<mpsc::Sender<String>>>,
    sharing: Mutex<bool>,
    retirement_requested: AtomicBool,
    started: Instant,
}

impl Default for LiveTypingHost {
    fn default() -> Self {
        Self {
            outbound: Mutex::new(None),
            sharing: Mutex::new(true),
            retirement_requested: AtomicBool::new(false),
            started: Instant::now(),
        }
    }
}

impl LiveTypingHost {
    fn attach(&self) -> mpsc::Receiver<String> {
        let (sender, receiver) = mpsc::channel(1);
        *self
            .outbound
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(sender);
        receiver
    }

    fn detach(&self) {
        self.outbound
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
    }

    fn send_with(&self, frame: impl FnOnce() -> Option<String>) -> bool {
        let outbound = self
            .outbound
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(sender) = outbound.as_ref() else {
            return false;
        };
        frame().is_some_and(|frame| sender.try_send(frame).is_ok())
    }

    fn sharing(&self) -> bool {
        *self
            .sharing
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn set_sharing(&self, client: &Client, enabled: bool) -> Result<(), LiveError> {
        let mut sharing = self
            .sharing
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *sharing = enabled;
        client.set_typing_sharing(enabled)
    }

    fn apply_sharing_with(
        &self,
        client: &Client,
        before_apply: impl FnOnce(),
    ) -> Result<(), LiveError> {
        let sharing = self
            .sharing
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        before_apply();
        client.set_typing_sharing(*sharing)
    }

    fn apply_sharing(&self, client: &Client) -> Result<(), LiveError> {
        self.apply_sharing_with(client, || {})
    }

    fn request_retirement(&self) {
        self.retirement_requested.store(true, Ordering::Release);
    }

    fn take_retirement(&self) -> bool {
        self.retirement_requested.swap(false, Ordering::AcqRel)
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
}

pub(crate) type GatewayDeviceId = DeviceId;

pub(crate) struct RcsSnapshotInputs<'a> {
    pub client: &'a Client,
    pub now_ms: i64,
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
            unlocked: true,
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
const LIVE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const LIVE_HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const LIVE_IO_TIMEOUT: Duration = Duration::from_secs(2);
const LIVE_MAX_FRAME: usize = 64 * 1024;
const INBOUND_QUEUE_LIMIT: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TypingDisplayState {
    pub conversation_id: ConversationId,
    pub state: peppy_protocol::TypingStateV2,
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
    frames: VecDeque<String>,
    queue_drops: u64,
    gateway_intent_drops: u64,
    displays: HashMap<ConversationId, TypingDisplayState>,
    peers: HashMap<DeviceId, (u64, u64)>,
    sequences: HashMap<(DeviceId, ConversationId), u64>,
    events: Vec<TypingDisplayState>,
}
impl InboundTyping {
    fn push(&mut self, frame: String) -> bool {
        if self.frames.len() == INBOUND_QUEUE_LIMIT {
            self.queue_drops += 1;
            false
        } else {
            self.frames.push_back(frame);
            true
        }
    }
    fn take(&mut self) -> Option<String> {
        self.frames.pop_front()
    }
    fn clear(&mut self) {
        self.frames.clear();
        for (_, mut display) in self.displays.drain() {
            display.state = peppy_protocol::TypingStateV2::Stopped;
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
                display.state = peppy_protocol::TypingStateV2::Stopped;
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
                    expires_at_ms: (state == peppy_protocol::TypingStateV2::Started).then(|| {
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
                if state == peppy_protocol::TypingStateV2::Started {
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
        self.peers.insert(
            update.peer_device_id,
            (update.current_session_token, update.validation_revision),
        );
        if let Some(replaced_token) = update.replaced_session_token {
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
                    display.state = peppy_protocol::TypingStateV2::Stopped;
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

fn typing_event_targets(conversation_id: ConversationId) -> [String; 2] {
    [
        crate::tray::MAIN.to_owned(),
        format!("{}{}", crate::tray::COMPOSER_PREFIX, conversation_id),
    ]
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
            event.state == peppy_protocol::TypingStateV2::Stopped
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
    use tauri::Emitter as _;
    for event in events {
        for target in typing_event_targets(event.conversation_id) {
            let _ = app.emit_to(target, TYPING_EVENT, event.clone());
        }
    }
}

fn request_retirement_on_clock(error: LiveError, session: &Session) -> bool {
    if error != LiveError::Clock {
        return false;
    }
    session.live_typing.request_retirement();
    session.live_context_wake.notify_one();
    true
}

fn send_typing_for_session(
    session: &Session,
    conversation_id: ConversationId,
    state: peppy_protocol::TypingStateV2,
    clock: LiveClock,
) -> bool {
    let gateway = {
        let status = session
            .status
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        resolve_typing_target(
            &status,
            RcsSnapshotInputs {
                client: &session.client,
                now_ms: clock.wall_ms,
            },
            conversation_id,
        )
    };
    let Some(gateway) = gateway else {
        return false;
    };
    session.live_typing.send_with(|| {
        match session.client.seal_typing_signal_v2(
            conversation_id,
            gateway,
            peppy_protocol::TypingOriginV2::VaultDevice,
            state,
            clock,
        ) {
            Ok(Some(outbound)) => serde_json::to_string(&outbound.signal).ok(),
            Err(error) if request_retirement_on_clock(error, session) => None,
            Ok(None) | Err(_) => None,
        }
    })
}

#[tauri::command]
pub(crate) async fn send_typing(
    window: tauri::WebviewWindow,
    app_state: tauri::State<'_, AppState>,
    conversation_id: String,
    state: peppy_protocol::TypingStateV2,
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
    let _ = send_typing_for_session(
        &session,
        conversation_id,
        state,
        session.live_typing.clock(),
    );
    Ok(())
}

#[tauri::command]
pub(crate) async fn set_typing_sharing(
    window: tauri::WebviewWindow,
    app_state: tauri::State<'_, AppState>,
    enabled: bool,
) -> BridgeResult<()> {
    crate::require_main_or_composer(window.label())?;
    let session = app_state.require_session().await?;
    let _ = session.live_typing.set_sharing(&session.client, enabled);
    Ok(())
}

fn apply_enable_context(
    state: &mut LiveTypingState,
    session: &Session,
    context: Option<LiveEnableContext>,
    clock: LiveClock,
) -> Result<Option<LiveEnableGeneration>, LiveError> {
    let generation = state.apply(&session.client, context, clock)?;
    if generation.is_some() {
        session.live_typing.apply_sharing(&session.client)?;
    }
    Ok(generation)
}

fn live_enable_context(session: &Session) -> Option<LiveEnableContext> {
    let status = session
        .status
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    enable_context(
        &status,
        RcsSnapshotInputs {
            client: &session.client,
            now_ms: wall_ms(),
        },
    )
}

fn apply_pending_retirement(
    state: &mut LiveTypingState,
    inbound: &mut InboundTyping,
    session: &Session,
    outbound: &mut Option<mpsc::Receiver<String>>,
) -> bool {
    if !session.live_typing.take_retirement() {
        return false;
    }
    session.live_typing.detach();
    disable(state, session);
    inbound.clear();
    emit_events(inbound, session, state.generation());
    *outbound = None;
    true
}

fn reevaluate_context(
    state: &mut LiveTypingState,
    inbound: &mut InboundTyping,
    session: &Session,
    outbound: &mut Option<mpsc::Receiver<String>>,
) {
    if apply_pending_retirement(state, inbound, session, outbound) {
        return;
    }
    let previous_generation = state.generation();
    let generation = apply_enable_context(
        state,
        session,
        live_enable_context(session),
        session.live_typing.clock(),
    )
    .unwrap_or_else(|_| {
        disable(state, session);
        None
    });
    if generation == previous_generation {
        return;
    }
    session.live_typing.detach();
    inbound.clear();
    emit_events(inbound, session, state.generation());
    *outbound = generation.map(|_| session.live_typing.attach());
}

pub(crate) fn start(session: Arc<Session>) {
    session.track_worker(tokio::spawn(supervise(session.clone())));
}

async fn supervise(session: Arc<Session>) {
    let mut state = LiveTypingState::default();
    let mut inbound = InboundTyping::default();
    let mut backoff = Duration::from_secs(1);
    let mut probed = false;
    loop {
        let result = connect(&session).await;
        let mut socket = match result {
            Ok(socket) => socket,
            Err(()) => {
                if !probed {
                    probed = true;
                    let _ = session
                        .api
                        .get_json::<serde_json::Value>("/v1/live", 1024)
                        .await;
                }
                disable(&mut state, &session);
                if !wait_or_cancel(&session, live_backoff_delay(backoff)).await {
                    return;
                }
                backoff = next_live_backoff(backoff);
                continue;
            }
        };
        if !server_hello(&mut socket, &session).await {
            disable(&mut state, &session);
            if !wait_or_cancel(&session, live_backoff_delay(backoff)).await {
                return;
            }
            backoff = next_live_backoff(backoff);
            continue;
        }
        probed = false;
        backoff = Duration::from_secs(1);
        let mut outbound = None;
        reevaluate_context(&mut state, &mut inbound, &session, &mut outbound);
        'socket: loop {
            let elapsed_mono = session.live_typing.mono_ms();
            let expiry_delay = inbound
                .next_deadline()
                .map(|deadline| Duration::from_millis(deadline.saturating_sub(elapsed_mono)));
            tokio::select! {
                _ = session.cancel.cancelled() => {
                    session.live_typing.detach();
                    disable(&mut state, &session);
                    inbound.clear();
                    emit_events(&mut inbound, &session, state.generation());
                    let _ = bounded_close(&mut socket, &session.cancel).await;
                    return;
                }
                _ = session.live_context_wake.notified() => {
                    reevaluate_context(&mut state, &mut inbound, &session, &mut outbound);
                }
                frame = async {
                    match outbound.as_mut() {
                        Some(receiver) => receiver.recv().await,
                        None => std::future::pending().await,
                    }
                } => match frame {
                    Some(frame) => {
                        if !bounded_send(
                            &mut socket,
                            &session.cancel,
                            Message::Text(frame.into()),
                        ).await {
                            break 'socket;
                        }
                    }
                    None => break,
                },
                _ = async {
                    match expiry_delay {
                        Some(delay) => sleep(delay).await,
                        None => std::future::pending().await,
                    }
                } => {
                    inbound.expire(session.live_typing.mono_ms());
                    emit_events(&mut inbound, &session, state.generation());
                }
                frame = socket.next() => match frame {
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(Message::Text(frame))) => {
                        inbound.push(frame.to_string());
                        while let Some(frame) = inbound.take() {
                            let reply = match process_inbound_frame(
                                &session.client,
                                &mut inbound,
                                &frame,
                                state.generation(),
                                session.live_typing.clock(),
                            ) {
                                Ok(reply) => reply,
                                Err(error) if request_retirement_on_clock(error, &session) => {
                                    apply_pending_retirement(
                                        &mut state,
                                        &mut inbound,
                                        &session,
                                        &mut outbound,
                                    );
                                    None
                                }
                                Err(_) => None,
                            };
                            inbound.expire(session.live_typing.mono_ms());
                            emit_events(&mut inbound, &session, state.generation());
                            if let Some(reply) = reply {
                                if !bounded_send(
                                    &mut socket,
                                    &session.cancel,
                                    Message::Text(reply.into()),
                                ).await {
                                    break 'socket;
                                }
                            }
                        }
                    }
                    Some(Ok(_)) => {}
                }
            }
        }
        session.live_typing.detach();
        disable(&mut state, &session);
        inbound.clear();
        emit_events(&mut inbound, &session, state.generation());
        if !wait_or_cancel(&session, live_backoff_delay(backoff)).await {
            return;
        }
        backoff = next_live_backoff(backoff);
    }
}

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

async fn bounded_close<S>(
    socket: &mut WebSocketStream<S>,
    cancel: &tokio_util::sync::CancellationToken,
) -> bool
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::select! {
        _ = cancel.cancelled() => false,
        result = timeout(LIVE_IO_TIMEOUT, socket.close(None)) => matches!(result, Ok(Ok(()))),
    }
}

async fn connect(
    session: &Session,
) -> Result<WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, ()> {
    let request = live_request(session.api.origin(), session.api.bearer())?;
    let connector = crate::tls::websocket_connector(session.api.origin()).map_err(|_| ())?;
    timeout(
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
    )
    .await
    .map_err(|_| ())?
    .map(|(socket, _)| socket)
    .map_err(|_| ())
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

async fn server_hello(
    socket: &mut WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    session: &Session,
) -> bool {
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

fn disable(state: &mut LiveTypingState, session: &Session) {
    let _ = state.apply(&session.client, None, session.live_typing.clock());
}

async fn wait_or_cancel(session: &Session, delay: Duration) -> bool {
    tokio::select! { _ = session.cancel.cancelled() => false, _ = sleep(delay) => true }
}

fn live_backoff_delay(backoff: Duration) -> Duration {
    jitter(backoff).min(LIVE_MAX_BACKOFF)
}

fn next_live_backoff(backoff: Duration) -> Duration {
    (backoff * 2).min(LIVE_MAX_BACKOFF)
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
        assert_eq!(next_live_backoff(LIVE_MAX_BACKOFF), LIVE_MAX_BACKOFF);
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
    fn inbound_queue_drops_new_frames_after_sixty_four() {
        let mut inbound = InboundTyping::default();
        for _ in 0..INBOUND_QUEUE_LIMIT {
            assert!(inbound.push("{}".into()));
        }
        assert!(!inbound.push("{}".into()));
        assert_eq!(inbound.frames.len(), INBOUND_QUEUE_LIMIT);
        assert_eq!(inbound.queue_drops, 1);
    }

    #[test]
    fn disable_clears_inbound_display_state() {
        let mut inbound = InboundTyping::default();
        inbound.frames.push_back("frame".into());
        inbound.clear();
        assert!(inbound.frames.is_empty());
        assert!(inbound.displays.is_empty());
    }

    fn display_outcome(
        peer: DeviceId,
        conversation: ConversationId,
        generation: u64,
        token: u64,
        sequence: u64,
        deadline: u64,
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
                state: peppy_protocol::TypingStateV2::Started,
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
    fn typing_events_target_only_main_and_the_matching_composer() {
        let conversation_id = ConversationId::new();

        assert_eq!(
            typing_event_targets(conversation_id),
            [
                crate::tray::MAIN.to_owned(),
                format!("{}{}", crate::tray::COMPOSER_PREFIX, conversation_id),
            ]
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
        assert_eq!(stopped.state, peppy_protocol::TypingStateV2::Stopped);

        assert!(!inbound.update_peer(PeerSessionUpdate {
            peer_device_id: peer,
            replaced_session_token: Some(2),
            current_session_token: 3,
            validation_revision: 3,
        }));
        assert_eq!(inbound.peers[&peer], (2, 3));
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
                state: peppy_protocol::TypingStateV2::Started,
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

    fn enable_test_session(session: &Session, fixture: &crate::tests::Fixture) {
        roster_refresh(session, &fixture.binding.device_id, false);
        install_gateway(session, &fixture.binding.device_id);
        let context = {
            let status = session.status.lock().unwrap();
            enable_context(&status, inputs(session))
        };
        apply_enable_context(&mut LiveTypingState::default(), session, context, clock()).unwrap();
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
    fn open_clock_error_is_retired_by_the_serial_supervisor_path() {
        let (fixture, session, _) = session_with_linked_eligible_gateway();
        roster_refresh(&session, &fixture.binding.device_id, false);
        install_gateway(&session, &fixture.binding.device_id);
        let mut state = LiveTypingState::default();
        let mut inbound = InboundTyping::default();
        let context = {
            let status = session.status.lock().unwrap();
            enable_context(&status, inputs(&session))
        };
        apply_enable_context(&mut state, &session, context, clock()).unwrap();
        let mut outbound = Some(session.live_typing.attach());
        assert!(state.generation().is_some());

        let error = process_inbound_frame(
            &session.client,
            &mut inbound,
            "{}",
            state.generation(),
            LiveClock {
                wall_ms: NOW_MS - 1,
                mono_ms: 0,
            },
        )
        .unwrap_err();
        assert!(request_retirement_on_clock(error, &session));
        apply_pending_retirement(&mut state, &mut inbound, &session, &mut outbound);

        assert_eq!(state.generation(), None);
        assert_eq!(
            session.client.set_typing_sharing(true),
            Err(LiveError::Disabled)
        );
    }

    #[test]
    fn sharing_toggle_serializes_after_enable_reapply() {
        let (fixture, session, _) = session_with_linked_eligible_gateway();
        enable_test_session(&session, &fixture);
        let (started_tx, started_rx) = std::sync::mpsc::channel();

        std::thread::scope(|scope| {
            let session_ref = &session;
            let toggle = scope.spawn(move || {
                started_rx.recv().unwrap();
                session_ref
                    .live_typing
                    .set_sharing(&session_ref.client, false)
                    .unwrap();
            });
            session
                .live_typing
                .apply_sharing_with(&session.client, || {
                    started_tx.send(()).unwrap();
                    std::thread::sleep(Duration::from_millis(20));
                })
                .unwrap();
            toggle.join().unwrap();
        });

        assert!(!session.live_typing.sharing());
        assert!(session
            .client
            .seal_typing_signal_v2(
                ConversationId::new(),
                fixture.binding.device_id.parse().unwrap(),
                peppy_protocol::TypingOriginV2::VaultDevice,
                peppy_protocol::TypingStateV2::Started,
                clock(),
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn sharing_off_produces_no_outbound_frame() {
        let (fixture, session, conversation) = session_with_linked_eligible_gateway();
        let _ = session.live_typing.set_sharing(&session.client, false);
        enable_test_session(&session, &fixture);
        let mut outbound = session.live_typing.attach();

        assert!(!send_typing_for_session(
            &session,
            conversation,
            peppy_protocol::TypingStateV2::Started,
            clock(),
        ));
        assert!(matches!(
            outbound.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn send_without_an_eligible_target_produces_no_frame() {
        let fixture = fixture_at("http://127.0.0.1:9");
        let session = open(&fixture, &fixture.binding, &[]);
        session
            .client
            .unlock(&fixture.profile, &fixture.header, PHRASE)
            .unwrap();
        let mut outbound = session.live_typing.attach();

        assert!(!send_typing_for_session(
            &session,
            ConversationId::new(),
            peppy_protocol::TypingStateV2::Started,
            clock(),
        ));
        assert!(matches!(
            outbound.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn disconnected_send_is_dropped_without_replay() {
        let (fixture, session, conversation) = session_with_linked_eligible_gateway();
        enable_test_session(&session, &fixture);

        assert!(!send_typing_for_session(
            &session,
            conversation,
            peppy_protocol::TypingStateV2::Started,
            clock(),
        ));
        let mut outbound = session.live_typing.attach();
        assert!(matches!(
            outbound.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert!(send_typing_for_session(
            &session,
            conversation,
            peppy_protocol::TypingStateV2::Started,
            clock(),
        ));
        assert!(outbound.try_recv().is_ok());
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
            let sent = bounded_send(
                &mut socket,
                &task_cancel,
                Message::Text("x".repeat(LIVE_MAX_FRAME).into()),
            )
            .await;
            let closed = bounded_close(&mut socket, &task_cancel).await;
            (sent, closed)
        });

        sleep(Duration::from_millis(10)).await;
        cancel.cancel();
        let (sent, closed) = timeout(Duration::from_millis(200), task)
            .await
            .expect("stalled transport did not drain after cancellation")
            .unwrap();
        assert!(!sent);
        assert!(!closed);
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
        let supervisor = tokio::spawn(supervise(session.clone()));

        wait_for_live_enabled(&session, true).await;
        roster_refresh(&session, &fixture.binding.device_id, true);
        wait_for_live_enabled(&session, false).await;
        roster_refresh(&session, &fixture.binding.device_id, false);
        install_gateway(&session, &fixture.binding.device_id);
        session.notify();
        wait_for_live_enabled(&session, true).await;

        session.cancel.cancel();
        supervisor.await.unwrap();
        server.await.unwrap();
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

    #[test]
    fn socket_drop_disables_and_reconnect_reenables_with_new_generation() {
        let (fixture, session, _) = session_with_linked_eligible_gateway();
        roster_refresh(&session, &fixture.binding.device_id, false);
        install_gateway(&session, &fixture.binding.device_id);
        let context = {
            let status = session.status.lock().unwrap();
            enable_context(&status, inputs(&session))
        };
        let mut state = LiveTypingState::default();
        let first = state
            .apply(&session.client, context.clone(), clock())
            .unwrap();
        disable(&mut state, &session);
        assert_eq!(state.generation(), None);
        let second = state.apply(&session.client, context, clock()).unwrap();
        assert_ne!(first, second);
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
