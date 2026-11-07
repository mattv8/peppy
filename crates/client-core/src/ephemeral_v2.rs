use crate::{ConversationId, DeviceId, RcsEligibility, Store, rcs_capability};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use peppy_crypto::encrypt;
use peppy_protocol::{
    ChallengePlaintextV2, EPHEMERAL_V2_PROTOCOL_VERSION, EphemeralSignalV2, EphemeralSignalV2Kind,
    POSTGRES_NOTIFY_PAYLOAD_BYTES, SessionAckPlaintextV2, TypingOriginV2, TypingPlaintextV2,
    TypingStateV2,
};

use super::{
    ephemeral_v2_control::{ControlBudget, IngressBudget},
    ephemeral_v2_decode::{AuthenticatedPlaintext, decode_authenticated_frame},
    ephemeral_v2_policy::{ClockState, LiveClock as PolicyClock, PolicyError, SenderBudget},
    ephemeral_v2_session::{
        AuthenticatedAck, AuthenticatedSequence, AuthenticatedTyping, ChallengeNonce,
        EphemeralV2SessionState, MonoTime, RoutedTyping, SessionId, SessionTypingState,
    },
};

const MAX_ROSTER: usize = 100;
const TTL_MS: u64 = 15_000;

/// Trusted host-attested admission facts. They are not cryptographic proof of a device role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveEnableContext {
    pub trusted_roster: Vec<DeviceId>,
    pub local_gateway_device_id: DeviceId,
    pub local_executor: LiveExecutor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveExecutor {
    None,
    CurrentGateway,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveClock {
    pub wall_ms: i64,
    pub mono_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveEnableGeneration(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveOutbound {
    pub generation: LiveEnableGeneration,
    pub signal: EphemeralSignalV2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveOutcome {
    pub generation: LiveEnableGeneration,
    pub session_update: Option<PeerSessionUpdate>,
    pub action: Option<LiveAction>,
    pub reply: Option<EphemeralSignalV2>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerSessionUpdate {
    pub peer_device_id: DeviceId,
    pub replaced_session_token: Option<u64>,
    pub current_session_token: u64,
    pub validation_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveAction {
    Display {
        peer_device_id: DeviceId,
        session_token: u64,
        sequence: u64,
        conversation_id: ConversationId,
        state: TypingStateV2,
        deadline_mono: u64,
    },
    GatewayTypingIntent {
        peer_device_id: DeviceId,
        session_token: u64,
        sequence: u64,
        conversation_id: ConversationId,
        state: TypingStateV2,
        deadline_mono: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveError {
    Clock,
    Disabled,
    CounterExhausted,
    Crypto,
    InvalidContext,
}

#[derive(Clone, Debug)]
pub(crate) struct EphemeralV2State {
    enabled: bool,
    generation: u64,
    session_id: [u8; 16],
    last_sequence: u64,
    clock: Option<ClockState>,
    budget: SenderBudget,
    control_budget: ControlBudget,
    ingress_budget: IngressBudget,
    receiver: Option<EphemeralV2SessionState>,
    sharing_enabled: bool,
    context: Option<LiveEnableContext>,
}

impl Default for EphemeralV2State {
    fn default() -> Self {
        Self {
            enabled: false,
            generation: 0,
            session_id: [0; 16],
            last_sequence: 0,
            clock: None,
            budget: SenderBudget::new(),
            control_budget: ControlBudget::default(),
            ingress_budget: IngressBudget::default(),
            receiver: None,
            sharing_enabled: true,
            context: None,
        }
    }
}

impl EphemeralV2State {
    pub(crate) fn disable(&mut self) {
        self.enabled = false;
        self.last_sequence = 0;
        self.clock = None;
        self.budget = SenderBudget::new();
        self.control_budget = ControlBudget::default();
        self.ingress_budget = IngressBudget::default();
        self.receiver = None;
        self.context = None;
        self.generation = self.generation.saturating_add(1);
        self.session_id = [0; 16];
    }
}

pub(crate) fn enable(
    store: &mut Store,
    context: LiveEnableContext,
    clock: LiveClock,
) -> Result<LiveEnableGeneration, LiveError> {
    let active_epoch_available = store
        .active_epoch
        .is_some_and(|epoch| store.keys.contains_key(&epoch));
    if !active_epoch_available
        || context.trusted_roster.is_empty()
        || context.trusted_roster.len() > MAX_ROSTER
        || !context.trusted_roster.contains(&store.config.device_id)
        || context
            .trusted_roster
            .iter()
            .enumerate()
            .any(|(index, device)| context.trusted_roster[index + 1..].contains(device))
    {
        return Err(LiveError::InvalidContext);
    }
    let clock = ClockState::new(PolicyClock {
        wall_ms: clock.wall_ms,
        mono_ms: clock.mono_ms,
    })
    .map_err(policy_error)?;
    let generation = store
        .ephemeral_v2
        .generation
        .checked_add(1)
        .ok_or(LiveError::InvalidContext)?;
    let session_id = *uuid::Uuid::new_v4().as_bytes();
    store.ephemeral_v2 = EphemeralV2State {
        enabled: true,
        generation,
        session_id,
        last_sequence: 0,
        clock: Some(clock),
        budget: SenderBudget::new(),
        control_budget: ControlBudget::default(),
        ingress_budget: IngressBudget::default(),
        receiver: Some(EphemeralV2SessionState::new(SessionId(session_id))),
        sharing_enabled: true,
        context: Some(context),
    };
    Ok(LiveEnableGeneration(generation))
}

pub(crate) fn seal(
    store: &mut Store,
    conversation_id: ConversationId,
    gateway_device_id: DeviceId,
    origin: TypingOriginV2,
    state: TypingStateV2,
    clock: LiveClock,
) -> Result<Option<LiveOutbound>, LiveError> {
    if !store.ephemeral_v2.enabled {
        return Err(LiveError::Disabled);
    }
    if !store.ephemeral_v2.sharing_enabled && state == TypingStateV2::Started {
        return Ok(None);
    }
    let mut clock_candidate = store
        .ephemeral_v2
        .clock
        .clone()
        .ok_or(LiveError::Disabled)?;
    let effective_wall = clock_candidate
        .advance(PolicyClock {
            wall_ms: clock.wall_ms,
            mono_ms: clock.mono_ms,
        })
        .map_err(policy_error)?;
    let context = store
        .ephemeral_v2
        .context
        .as_ref()
        .ok_or(LiveError::Disabled)?;
    let local_gateway = context.local_gateway_device_id == store.config.device_id
        && context.local_executor == LiveExecutor::CurrentGateway;
    match origin {
        TypingOriginV2::VaultDevice if !context.trusted_roster.contains(&gateway_device_id) => {
            return Err(LiveError::InvalidContext);
        }
        TypingOriginV2::GoogleContact
            if gateway_device_id != store.config.device_id || !local_gateway =>
        {
            return Err(LiveError::InvalidContext);
        }
        _ => {}
    }
    let eligible = matches!(rcs_capability::projection(&store.conn, conversation_id, gateway_device_id, effective_wall).map_err(|_| LiveError::InvalidContext)?, Some(projection) if projection.eligibility == RcsEligibility::Eligible);
    if !eligible {
        return Ok(None);
    }
    let mut budget_candidate = store.ephemeral_v2.budget.clone();
    let admitted = match state {
        TypingStateV2::Started => budget_candidate.admit_start(conversation_id, clock.mono_ms),
        TypingStateV2::Stopped => budget_candidate.admit_stop(conversation_id, clock.mono_ms),
    }
    .map_err(policy_error)?;
    if !admitted {
        return Ok(None);
    }
    let sequence = store
        .ephemeral_v2
        .last_sequence
        .checked_add(1)
        .ok_or(LiveError::CounterExhausted)?;
    let sent_at = u64::try_from(effective_wall).map_err(|_| LiveError::Clock)?;
    let expires_at = sent_at.checked_add(TTL_MS).ok_or(LiveError::Clock)?;
    let mut signal = EphemeralSignalV2 {
        protocol_version: EPHEMERAL_V2_PROTOCOL_VERSION,
        kind: EphemeralSignalV2Kind::Typing,
        sender_device_id: store.config.device_id,
        key_epoch: store.active_epoch.ok_or(LiveError::Disabled)?,
        session_id: URL_SAFE_NO_PAD.encode(store.ephemeral_v2.session_id),
        sent_at_ms: sent_at.to_string(),
        expires_at_ms: expires_at.to_string(),
        sequence: sequence.to_string(),
        // `aad_bytes` validates the complete canonical header. A one-byte placeholder is
        // replaced before return and is never exposed outside this critical section.
        ciphertext_b64: STANDARD.encode([0_u8]),
    };
    let plaintext = serde_json::to_vec(&TypingPlaintextV2 {
        version: EPHEMERAL_V2_PROTOCOL_VERSION,
        origin,
        conversation_id: conversation_id.0,
        state,
        gateway_device_id: (origin == TypingOriginV2::VaultDevice).then_some(gateway_device_id),
    })
    .map_err(|_| LiveError::Crypto)?;
    let aad = signal
        .aad_bytes(store.config.vault_id)
        .map_err(|_| LiveError::Crypto)?;
    let epoch = store.active_epoch.ok_or(LiveError::Disabled)?;
    let encrypted = encrypt(
        &store.keys.get(&epoch).ok_or(LiveError::Disabled)?.event,
        &aad,
        &plaintext,
    )
    .map_err(|_| LiveError::Crypto)?;
    let mut frame = encrypted.nonce.to_vec();
    frame.extend_from_slice(&encrypted.ciphertext);
    signal.ciphertext_b64 = STANDARD.encode(frame);
    signal.validate().map_err(|_| LiveError::Crypto)?;
    store.ephemeral_v2.clock = Some(clock_candidate);
    store.ephemeral_v2.budget = budget_candidate;
    store.ephemeral_v2.last_sequence = sequence;
    Ok(Some(LiveOutbound {
        generation: LiveEnableGeneration(store.ephemeral_v2.generation),
        signal,
    }))
}

pub(crate) fn set_sharing(store: &mut Store, enabled: bool) -> Result<(), LiveError> {
    if !store.ephemeral_v2.enabled {
        return Err(LiveError::Disabled);
    }
    store.ephemeral_v2.sharing_enabled = enabled;
    Ok(())
}

pub(crate) fn open(
    store: &mut Store,
    frame: &EphemeralSignalV2,
    clock: LiveClock,
) -> Result<LiveOutcome, LiveError> {
    if !store.ephemeral_v2.enabled {
        return Err(LiveError::Disabled);
    }
    let mut clock_candidate = store
        .ephemeral_v2
        .clock
        .clone()
        .ok_or(LiveError::Disabled)?;
    let effective_wall = clock_candidate
        .advance(PolicyClock {
            wall_ms: clock.wall_ms,
            mono_ms: clock.mono_ms,
        })
        .map_err(policy_error)?;
    let mut ingress_candidate = store.ephemeral_v2.ingress_budget.clone();
    if !ingress_candidate.admit(frame.sender_device_id, clock.mono_ms) {
        store.ephemeral_v2.clock = Some(clock_candidate);
        store.ephemeral_v2.ingress_budget = ingress_candidate;
        return Ok(empty_outcome(store));
    }
    let context = store
        .ephemeral_v2
        .context
        .as_ref()
        .ok_or(LiveError::Disabled)?;
    let receiver = store
        .ephemeral_v2
        .receiver
        .as_ref()
        .ok_or(LiveError::Disabled)?;
    let authenticated = decode_authenticated_frame(
        u64::try_from(effective_wall).map_err(|_| LiveError::Clock)?,
        store.config.vault_id,
        store.config.device_id,
        store.ephemeral_v2.session_id,
        &context.trusted_roster,
        frame,
        &store.keys,
    );
    store.ephemeral_v2.clock = Some(clock_candidate);
    store.ephemeral_v2.ingress_budget = ingress_candidate;
    let Some(authenticated) = authenticated else {
        return Ok(empty_outcome(store));
    };

    let mut receiver_candidate = receiver.clone();
    let mut control_candidate = store.ephemeral_v2.control_budget.clone();
    let (session_update, action, reply, next_sequence) = match authenticated.plaintext {
        AuthenticatedPlaintext::Typing(typing) => {
            let local_gateway = context.local_gateway_device_id == store.config.device_id
                && context.local_executor == LiveExecutor::CurrentGateway;
            let (routing, gateway) = match typing.origin {
                TypingOriginV2::VaultDevice if local_gateway => {
                    (RoutedTyping::GatewayIntent, store.config.device_id)
                }
                TypingOriginV2::GoogleContact => {
                    (RoutedTyping::Display, authenticated.sender_device_id)
                }
                _ => return Ok(empty_outcome(store)),
            };
            let eligible = matches!(rcs_capability::projection(&store.conn, ConversationId(typing.conversation_id), gateway, effective_wall).map_err(|_| LiveError::InvalidContext)?, Some(projection) if projection.eligibility == RcsEligibility::Eligible);
            if !eligible {
                return Ok(empty_outcome(store));
            }
            let deadline = clock
                .mono_ms
                .checked_add(authenticated.remaining_ttl_ms)
                .ok_or(LiveError::Clock)?;
            let transition = receiver_candidate
                .observe_typing(AuthenticatedTyping {
                    peer: authenticated.sender_device_id,
                    session: SessionId(authenticated.session_id),
                    conversation: ConversationId(typing.conversation_id),
                    sequence: AuthenticatedSequence(authenticated.sequence),
                    state: match typing.state {
                        TypingStateV2::Started => SessionTypingState::Started,
                        TypingStateV2::Stopped => SessionTypingState::Stopped,
                    },
                    routing,
                    received_mono_ms: MonoTime(clock.mono_ms),
                    wire_deadline_mono_ms: MonoTime(deadline),
                })
                .map_err(session_error)?;
            let mut sequence = store.ephemeral_v2.last_sequence;
            let reply = if let Some(intent) = transition.challenge {
                if !control_candidate.admit_challenge(clock.mono_ms) {
                    None
                } else {
                    sequence = sequence.checked_add(1).ok_or(LiveError::CounterExhausted)?;
                    let nonce = ChallengeNonce(*uuid::Uuid::new_v4().as_bytes());
                    let reply = seal_control(
                        store,
                        EphemeralSignalV2Kind::Challenge,
                        sequence,
                        &ChallengePlaintextV2 {
                            version: EPHEMERAL_V2_PROTOCOL_VERSION,
                            challenger_device_id: store.config.device_id,
                            challenger_session_id: URL_SAFE_NO_PAD
                                .encode(store.ephemeral_v2.session_id),
                            responder_device_id: intent.peer,
                            nonce_b64: STANDARD.encode(nonce.0),
                        },
                        effective_wall,
                    )?;
                    receiver_candidate
                        .record_challenge(intent, nonce)
                        .map_err(session_error)?;
                    Some(reply)
                }
            } else {
                None
            };
            (
                transition.session_update,
                transition.action,
                reply,
                sequence,
            )
        }
        AuthenticatedPlaintext::Challenge(challenge) => {
            if !control_candidate.admit_ack(authenticated.sender_device_id, clock.mono_ms) {
                return Ok(empty_outcome(store));
            }
            let sequence = store
                .ephemeral_v2
                .last_sequence
                .checked_add(1)
                .ok_or(LiveError::CounterExhausted)?;
            let reply = seal_control(
                store,
                EphemeralSignalV2Kind::SessionAck,
                sequence,
                &SessionAckPlaintextV2 {
                    version: EPHEMERAL_V2_PROTOCOL_VERSION,
                    responder_device_id: store.config.device_id,
                    responder_session_id: URL_SAFE_NO_PAD.encode(store.ephemeral_v2.session_id),
                    challenger_device_id: challenge.challenger_device_id,
                    challenger_session_id: challenge.challenger_session_id,
                    echoed_nonce_b64: challenge.nonce_b64,
                    sequence_floor: sequence.to_string(),
                },
                effective_wall,
            )?;
            (None, None, Some(reply), sequence)
        }
        AuthenticatedPlaintext::SessionAck(ack) => {
            let nonce = STANDARD
                .decode(ack.echoed_nonce_b64)
                .map_err(|_| LiveError::Crypto)?;
            let nonce: [u8; 16] = nonce.try_into().map_err(|_| LiveError::Crypto)?;
            let floor = ack.sequence_floor.parse().map_err(|_| LiveError::Crypto)?;
            let transition = receiver_candidate
                .accept_ack(AuthenticatedAck {
                    peer: authenticated.sender_device_id,
                    session: SessionId(authenticated.session_id),
                    challenger_session: SessionId(store.ephemeral_v2.session_id),
                    nonce: ChallengeNonce(nonce),
                    sequence_floor: AuthenticatedSequence(floor),
                    received_mono_ms: MonoTime(clock.mono_ms),
                })
                .map_err(session_error)?;
            (
                transition.session_update,
                transition.action,
                None,
                store.ephemeral_v2.last_sequence,
            )
        }
    };
    store.ephemeral_v2.receiver = Some(receiver_candidate);
    store.ephemeral_v2.control_budget = control_candidate;
    store.ephemeral_v2.last_sequence = next_sequence;
    Ok(LiveOutcome {
        generation: LiveEnableGeneration(store.ephemeral_v2.generation),
        session_update: session_update.map(|update| PeerSessionUpdate {
            peer_device_id: update.peer,
            replaced_session_token: update.replaced_session_token,
            current_session_token: update.current_session_token,
            validation_revision: update.validation_revision,
        }),
        action: action.map(|action| match action.routing {
            RoutedTyping::Display => LiveAction::Display {
                peer_device_id: action.peer,
                session_token: action.session_token,
                sequence: action.sequence.0,
                conversation_id: action.conversation,
                state: match action.state {
                    SessionTypingState::Started => TypingStateV2::Started,
                    SessionTypingState::Stopped => TypingStateV2::Stopped,
                },
                deadline_mono: action.deadline_mono_ms.0,
            },
            RoutedTyping::GatewayIntent => LiveAction::GatewayTypingIntent {
                peer_device_id: action.peer,
                session_token: action.session_token,
                sequence: action.sequence.0,
                conversation_id: action.conversation,
                state: match action.state {
                    SessionTypingState::Started => TypingStateV2::Started,
                    SessionTypingState::Stopped => TypingStateV2::Stopped,
                },
                deadline_mono: action.deadline_mono_ms.0,
            },
        }),
        reply,
    })
}

pub(crate) fn open_json(
    store: &mut Store,
    frame_json: &str,
    clock: LiveClock,
) -> Result<LiveOutcome, LiveError> {
    if !store.ephemeral_v2.enabled {
        return Err(LiveError::Disabled);
    }
    let mut clock_candidate = store
        .ephemeral_v2
        .clock
        .clone()
        .ok_or(LiveError::Disabled)?;
    clock_candidate
        .advance(PolicyClock {
            wall_ms: clock.wall_ms,
            mono_ms: clock.mono_ms,
        })
        .map_err(policy_error)?;
    if frame_json.len() > POSTGRES_NOTIFY_PAYLOAD_BYTES {
        store.ephemeral_v2.clock = Some(clock_candidate);
        return Ok(empty_outcome(store));
    }
    let Ok(frame) = serde_json::from_str::<EphemeralSignalV2>(frame_json) else {
        store.ephemeral_v2.clock = Some(clock_candidate);
        return Ok(empty_outcome(store));
    };
    open(store, &frame, clock)
}

fn empty_outcome(store: &Store) -> LiveOutcome {
    LiveOutcome {
        generation: LiveEnableGeneration(store.ephemeral_v2.generation),
        session_update: None,
        action: None,
        reply: None,
    }
}

fn seal_control<T: serde::Serialize>(
    store: &Store,
    kind: EphemeralSignalV2Kind,
    sequence: u64,
    plaintext: &T,
    sent_at: i64,
) -> Result<EphemeralSignalV2, LiveError> {
    let sent_at = u64::try_from(sent_at).map_err(|_| LiveError::Clock)?;
    let expires_at = sent_at.checked_add(TTL_MS).ok_or(LiveError::Clock)?;
    let epoch = store.active_epoch.ok_or(LiveError::Disabled)?;
    let mut signal = EphemeralSignalV2 {
        protocol_version: EPHEMERAL_V2_PROTOCOL_VERSION,
        kind,
        sender_device_id: store.config.device_id,
        key_epoch: epoch,
        session_id: URL_SAFE_NO_PAD.encode(store.ephemeral_v2.session_id),
        sent_at_ms: sent_at.to_string(),
        expires_at_ms: expires_at.to_string(),
        sequence: sequence.to_string(),
        ciphertext_b64: STANDARD.encode([0_u8]),
    };
    let aad = signal
        .aad_bytes(store.config.vault_id)
        .map_err(|_| LiveError::Crypto)?;
    let plaintext = serde_json::to_vec(plaintext).map_err(|_| LiveError::Crypto)?;
    let encrypted = encrypt(
        &store.keys.get(&epoch).ok_or(LiveError::Disabled)?.event,
        &aad,
        &plaintext,
    )
    .map_err(|_| LiveError::Crypto)?;
    let mut frame = encrypted.nonce.to_vec();
    frame.extend_from_slice(&encrypted.ciphertext);
    signal.ciphertext_b64 = STANDARD.encode(frame);
    signal.validate().map_err(|_| LiveError::Crypto)?;
    Ok(signal)
}

fn session_error(error: super::ephemeral_v2_session::SessionError) -> LiveError {
    match error {
        super::ephemeral_v2_session::SessionError::Capacity => LiveError::InvalidContext,
        super::ephemeral_v2_session::SessionError::CounterExhausted => LiveError::CounterExhausted,
        super::ephemeral_v2_session::SessionError::InstallExhausted => LiveError::InvalidContext,
    }
}

fn policy_error(error: PolicyError) -> LiveError {
    match error {
        PolicyError::Clock => LiveError::Clock,
        PolicyError::InvalidContext | PolicyError::Capacity => LiveError::InvalidContext,
    }
}
