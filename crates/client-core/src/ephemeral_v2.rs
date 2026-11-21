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
use std::collections::HashMap;

use super::{
    ephemeral_v2_control::{ControlBudget, IngressBudget, ReplayBudget},
    ephemeral_v2_decode::{
        AuthenticatedPlaintext, DecodeDrop, decode_authenticated_frame_with_reason,
    },
    ephemeral_v2_policy::{
        ClockState, LiveClock as PolicyClock, PolicyError, SenderBudget, TypingSlot,
    },
    ephemeral_v2_session::{
        AuthenticatedAck, AuthenticatedSequence, AuthenticatedTyping, ChallengeNonce,
        EphemeralV2SessionState, MonoTime, Participant, RoutedTyping, SessionDrop, SessionId,
        SessionTypingState,
    },
};

const MAX_ROSTER: usize = 100;
const TTL_MS: u64 = 15_000;
const MAX_ACTIVE_STARTS: usize = 50;
const MAX_FOLLOW_UPS_PER_ACK: usize = 2;

#[derive(Clone, Debug)]
struct ActiveStart {
    conversation_id: ConversationId,
    gateway_device_id: DeviceId,
    origin: TypingOriginV2,
    participant: Participant,
    expires_at_wall_ms: u64,
    sealed_mono_ms: u64,
    last_replay_mono_ms: Option<u64>,
}

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
    pub follow_ups: Vec<EphemeralSignalV2>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerSessionUpdate {
    pub peer_device_id: DeviceId,
    pub replaced_session_token: Option<u64>,
    pub current_session_token: u64,
    pub validation_revision: u64,
}

/// `participant` is the canonical E.164 roster member for gateway-attributed group typing and
/// `None` for 1:1 typing. Typing state is keyed by (peer, conversation, participant).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveAction {
    Display {
        peer_device_id: DeviceId,
        session_token: u64,
        sequence: u64,
        conversation_id: ConversationId,
        participant: Option<String>,
        state: TypingStateV2,
        deadline_mono: u64,
    },
    GatewayTypingIntent {
        peer_device_id: DeviceId,
        session_token: u64,
        sequence: u64,
        conversation_id: ConversationId,
        participant: Option<String>,
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

macro_rules! live_typing_causes {
    ($( $variant:ident = $code:literal => $name:literal ),+ $(,)?) => {
        /// Stable, append-only, content-free diagnostic cause codes.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[repr(u16)]
        pub enum LiveTypingCause { $( $variant = $code ),+ }

        impl LiveTypingCause {
            /// All defined causes in code order. Reserved codes are omitted.
            pub const ALL: &'static [Self] = &[$( Self::$variant ),+];

            /// Stable snake_case name for diagnostic headers.
            pub const fn name(self) -> &'static str {
                match self { $( Self::$variant => $name ),+ }
            }
        }
    };
}

live_typing_causes! {
    SealStarted = 1 => "seal_started",
    SealStopped = 2 => "seal_stopped",
    SealSharingOff = 3 => "seal_sharing_off",
    SealParticipantNotMember = 4 => "seal_participant_not_member",
    SealNotEligible = 5 => "seal_not_eligible",
    SealBudget = 6 => "seal_budget",
    FrameReceived = 10 => "frame_received",
    IngressRefused = 11 => "ingress_refused",
    DecodeMalformed = 12 => "decode_malformed",
    DecodeExpired = 13 => "decode_expired",
    DecodeFuture = 14 => "decode_future",
    DecodeUnknownSender = 15 => "decode_unknown_sender",
    DecodeKeyUnavailable = 16 => "decode_key_unavailable",
    DecodeCrypto = 17 => "decode_crypto",
    DecodeNotAddressed = 18 => "decode_not_addressed",
    OriginIgnored = 19 => "origin_ignored",
    NotEligible = 20 => "not_eligible",
    SessionChallenged = 21 => "session_challenged",
    SessionChallengeSuppressed = 22 => "session_challenge_suppressed",
    SequenceStale = 23 => "sequence_stale",
    StartSuppressed = 24 => "start_suppressed",
    StopWithoutActive = 25 => "stop_without_active",
    Display = 26 => "display",
    GatewayIntent = 27 => "gateway_intent",
    ChallengeAnswered = 28 => "challenge_answered",
    ChallengeAnswerRefused = 29 => "challenge_answer_refused",
    AckAccepted = 30 => "ack_accepted",
    AckIgnored = 31 => "ack_ignored",
    ResendSent = 32 => "resend_sent",
    ResendSkipped = 33 => "resend_skipped",
    RenewalChallenged = 34 => "renewal_challenged",
    SealExpired = 35 => "seal_expired",
}

#[derive(Clone, Debug)]
pub(crate) struct EphemeralV2State {
    pub(crate) counters: [u64; 36],
    enabled: bool,
    generation: u64,
    session_id: [u8; 16],
    last_sequence: u64,
    clock: Option<ClockState>,
    budget: SenderBudget,
    control_budget: ControlBudget,
    ingress_budget: IngressBudget,
    replay_budget: ReplayBudget,
    receiver: Option<EphemeralV2SessionState>,
    sharing_enabled: bool,
    context: Option<LiveEnableContext>,
    active_starts: HashMap<TypingSlot, ActiveStart>,
}

impl Default for EphemeralV2State {
    fn default() -> Self {
        Self {
            counters: [0; 36],
            enabled: false,
            generation: 0,
            session_id: [0; 16],
            last_sequence: 0,
            clock: None,
            budget: SenderBudget::new(),
            control_budget: ControlBudget::default(),
            ingress_budget: IngressBudget::default(),
            replay_budget: ReplayBudget::default(),
            receiver: None,
            sharing_enabled: true,
            context: None,
            active_starts: HashMap::new(),
        }
    }
}

impl EphemeralV2State {
    fn count(&mut self, cause: LiveTypingCause) {
        let counter = &mut self.counters[cause as usize];
        *counter = counter.saturating_add(1);
    }

    pub(crate) fn disable(&mut self) {
        self.enabled = false;
        self.last_sequence = 0;
        self.clock = None;
        self.budget = SenderBudget::new();
        self.control_budget = ControlBudget::default();
        self.ingress_budget = IngressBudget::default();
        self.replay_budget = ReplayBudget::default();
        self.receiver = None;
        self.context = None;
        self.active_starts.clear();
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
        counters: store.ephemeral_v2.counters,
        enabled: true,
        generation,
        session_id,
        last_sequence: 0,
        clock: Some(clock),
        budget: SenderBudget::new(),
        control_budget: ControlBudget::default(),
        ingress_budget: IngressBudget::default(),
        replay_budget: ReplayBudget::default(),
        receiver: Some(EphemeralV2SessionState::new(SessionId(session_id))),
        sharing_enabled: true,
        context: Some(context),
        active_starts: HashMap::new(),
    };
    Ok(LiveEnableGeneration(generation))
}

/// Attribution seam for group typing (I7). Returns true only when `participant` is a current
/// roster member of a Live group mapping for the conversation. A participant the seam does not
/// confirm is never sealed.
pub(crate) type GroupTypingRoster<'a> = &'a dyn Fn(ConversationId, &str) -> bool;

/// The identity and content of one outbound typing frame.
pub(crate) struct TypingSealRequest {
    pub(crate) conversation_id: ConversationId,
    pub(crate) gateway_device_id: DeviceId,
    pub(crate) origin: TypingOriginV2,
    pub(crate) participant: Participant,
    pub(crate) state: TypingStateV2,
}

pub(crate) fn seal(
    store: &mut Store,
    request: TypingSealRequest,
    clock: LiveClock,
    roster: GroupTypingRoster<'_>,
) -> Result<Option<LiveOutbound>, LiveError> {
    if request.state == TypingStateV2::Stopped {
        store.ephemeral_v2.active_starts.remove(&typing_slot(
            request.conversation_id,
            request.participant.as_deref(),
        ));
    }
    seal_typing(store, request, clock, roster, None, false)
}

fn seal_typing(
    store: &mut Store,
    request: TypingSealRequest,
    clock: LiveClock,
    roster: GroupTypingRoster<'_>,
    expiry_cap: Option<u64>,
    skip_start_budget: bool,
) -> Result<Option<LiveOutbound>, LiveError> {
    let TypingSealRequest {
        conversation_id,
        gateway_device_id,
        origin,
        participant,
        state,
    } = request;
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
    store.ephemeral_v2.clock = Some(clock_candidate);
    store
        .ephemeral_v2
        .active_starts
        .retain(|_, entry| entry.expires_at_wall_ms > effective_wall as u64);
    if !store.ephemeral_v2.sharing_enabled && origin == TypingOriginV2::VaultDevice {
        store.ephemeral_v2.count(LiveTypingCause::SealSharingOff);
        return Ok(None);
    }
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
    let typing = TypingPlaintextV2 {
        version: EPHEMERAL_V2_PROTOCOL_VERSION,
        origin,
        conversation_id: conversation_id.0,
        state,
        gateway_device_id: (origin == TypingOriginV2::VaultDevice).then_some(gateway_device_id),
        participant,
    };
    // Rejects participants on vault-device frames and non-canonical E.164 values.
    typing.validate().map_err(|_| LiveError::InvalidContext)?;
    if let Some(participant) = typing.participant.as_deref()
        && !roster(conversation_id, participant)
    {
        store
            .ephemeral_v2
            .count(LiveTypingCause::SealParticipantNotMember);
        return Ok(None);
    }
    if !typing_eligible(store, conversation_id, gateway_device_id, effective_wall)? {
        store.ephemeral_v2.count(LiveTypingCause::SealNotEligible);
        return Ok(None);
    }
    let slot = typing_slot(conversation_id, typing.participant.as_deref());
    let mut budget_candidate = store.ephemeral_v2.budget.clone();
    let admitted = match state {
        TypingStateV2::Started if skip_start_budget => Ok(true),
        TypingStateV2::Started => budget_candidate.admit_start(slot, clock.mono_ms),
        TypingStateV2::Stopped => budget_candidate.admit_stop(slot, clock.mono_ms),
    }
    .map_err(policy_error)?;
    if !admitted {
        store.ephemeral_v2.count(LiveTypingCause::SealBudget);
        return Ok(None);
    }
    let sequence = store
        .ephemeral_v2
        .last_sequence
        .checked_add(1)
        .ok_or(LiveError::CounterExhausted)?;
    let sent_at = u64::try_from(effective_wall).map_err(|_| LiveError::Clock)?;
    let expires_at = sent_at.checked_add(TTL_MS).ok_or(LiveError::Clock)?;
    let expires_at = expiry_cap.map_or(expires_at, |cap| expires_at.min(cap));
    if expires_at <= sent_at {
        store.ephemeral_v2.count(LiveTypingCause::SealExpired);
        return Ok(None);
    }
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
    let plaintext = serde_json::to_vec(&typing).map_err(|_| LiveError::Crypto)?;
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
    store.ephemeral_v2.budget = budget_candidate;
    store.ephemeral_v2.last_sequence = sequence;
    if state == TypingStateV2::Started && !skip_start_budget {
        let slot = typing_slot(conversation_id, typing.participant.as_deref());
        let starts = &mut store.ephemeral_v2.active_starts;
        if !starts.contains_key(&slot)
            && starts.len() >= MAX_ACTIVE_STARTS
            && let Some(oldest) = starts
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at_wall_ms)
                .map(|(slot, _)| slot.clone())
        {
            starts.remove(&oldest);
        }
        let last_replay_mono_ms = starts
            .get(&slot)
            .and_then(|entry| entry.last_replay_mono_ms);
        starts.insert(
            slot,
            ActiveStart {
                conversation_id,
                gateway_device_id,
                origin,
                participant: typing.participant,
                expires_at_wall_ms: expires_at,
                sealed_mono_ms: clock.mono_ms,
                last_replay_mono_ms,
            },
        );
    }
    store.ephemeral_v2.count(match state {
        TypingStateV2::Started => LiveTypingCause::SealStarted,
        TypingStateV2::Stopped => LiveTypingCause::SealStopped,
    });
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
    if !enabled {
        store
            .ephemeral_v2
            .active_starts
            .retain(|_, entry| entry.origin != TypingOriginV2::VaultDevice);
    }
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
        store.ephemeral_v2.count(LiveTypingCause::FrameReceived);
        store.ephemeral_v2.count(LiveTypingCause::IngressRefused);
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
    let authenticated = decode_authenticated_frame_with_reason(
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
    let authenticated = match authenticated {
        Ok(authenticated) => authenticated,
        Err(reason) => {
            store.ephemeral_v2.count(LiveTypingCause::FrameReceived);
            store.ephemeral_v2.count(match reason {
                DecodeDrop::Malformed => LiveTypingCause::DecodeMalformed,
                DecodeDrop::Expired => LiveTypingCause::DecodeExpired,
                DecodeDrop::Future => LiveTypingCause::DecodeFuture,
                DecodeDrop::UnknownSender => LiveTypingCause::DecodeUnknownSender,
                DecodeDrop::KeyUnavailable => LiveTypingCause::DecodeKeyUnavailable,
                DecodeDrop::Crypto => LiveTypingCause::DecodeCrypto,
                DecodeDrop::NotAddressed => LiveTypingCause::DecodeNotAddressed,
            });
            return Ok(empty_outcome(store));
        }
    };

    let mut receiver_candidate = receiver.clone();
    let mut control_candidate = store.ephemeral_v2.control_budget.clone();
    let mut causes = Vec::new();
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
                _ => {
                    store.ephemeral_v2.count(LiveTypingCause::FrameReceived);
                    store.ephemeral_v2.count(LiveTypingCause::OriginIgnored);
                    return Ok(empty_outcome(store));
                }
            };
            if !typing_eligible(
                store,
                ConversationId(typing.conversation_id),
                gateway,
                effective_wall,
            )? {
                store.ephemeral_v2.count(LiveTypingCause::FrameReceived);
                store.ephemeral_v2.count(LiveTypingCause::NotEligible);
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
                    participant: typing.participant.clone(),
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
            if let Some(reason) = transition.drop {
                causes.push(match reason {
                    SessionDrop::ChallengeSuppressed => LiveTypingCause::SessionChallengeSuppressed,
                    SessionDrop::SequenceStale => LiveTypingCause::SequenceStale,
                    SessionDrop::StartSuppressed => LiveTypingCause::StartSuppressed,
                    SessionDrop::StopWithoutActive => LiveTypingCause::StopWithoutActive,
                    SessionDrop::AckIgnored => LiveTypingCause::AckIgnored,
                });
            }
            if let Some(action) = &transition.action {
                causes.push(match action.routing {
                    RoutedTyping::Display => LiveTypingCause::Display,
                    RoutedTyping::GatewayIntent => LiveTypingCause::GatewayIntent,
                });
            }
            let mut sequence = store.ephemeral_v2.last_sequence;
            let reply = if let Some(intent) = transition.challenge {
                if !control_candidate.admit_challenge(clock.mono_ms) {
                    if !transition.renewal {
                        causes.push(LiveTypingCause::SessionChallengeSuppressed);
                    }
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
                    causes.push(if transition.renewal {
                        LiveTypingCause::RenewalChallenged
                    } else {
                        LiveTypingCause::SessionChallenged
                    });
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
                store.ephemeral_v2.count(LiveTypingCause::FrameReceived);
                store
                    .ephemeral_v2
                    .count(LiveTypingCause::ChallengeAnswerRefused);
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
            causes.push(LiveTypingCause::ChallengeAnswered);
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
            causes.push(if transition.drop == Some(SessionDrop::AckIgnored) {
                LiveTypingCause::AckIgnored
            } else {
                LiveTypingCause::AckAccepted
            });
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
    store.ephemeral_v2.count(LiveTypingCause::FrameReceived);
    for cause in causes {
        store.ephemeral_v2.count(cause);
    }
    let follow_ups = if reply
        .as_ref()
        .is_some_and(|frame| frame.kind == EphemeralSignalV2Kind::SessionAck)
    {
        replay_active_starts(store, authenticated.sender_device_id, clock, effective_wall)
    } else {
        Vec::new()
    };
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
                participant: action.participant,
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
                participant: action.participant,
                state: match action.state {
                    SessionTypingState::Started => TypingStateV2::Started,
                    SessionTypingState::Stopped => TypingStateV2::Stopped,
                },
                deadline_mono: action.deadline_mono_ms.0,
            },
        }),
        reply,
        follow_ups,
    })
}

fn replay_active_starts(
    store: &mut Store,
    challenger: DeviceId,
    clock: LiveClock,
    effective_wall: i64,
) -> Vec<EphemeralSignalV2> {
    let starts = &mut store.ephemeral_v2.active_starts;
    starts.retain(|_, start| start.expires_at_wall_ms > effective_wall as u64);
    let mut candidates: Vec<_> = starts
        .iter()
        .filter(|(_, start)| {
            (start.origin == TypingOriginV2::GoogleContact || start.gateway_device_id == challenger)
                && start
                    .expires_at_wall_ms
                    .saturating_sub(effective_wall as u64)
                    >= 1_000
                && start
                    .last_replay_mono_ms
                    .is_none_or(|last| clock.mono_ms.saturating_sub(last) >= 3_000)
        })
        .map(|(slot, start)| (slot.clone(), start.clone()))
        .collect();
    let skipped = starts.len().saturating_sub(candidates.len())
        + candidates.len().saturating_sub(MAX_FOLLOW_UPS_PER_ACK);
    for _ in 0..skipped {
        store.ephemeral_v2.count(LiveTypingCause::ResendSkipped);
    }
    candidates.sort_by_key(|(_, start)| std::cmp::Reverse(start.sealed_mono_ms));
    let mut follow_ups = Vec::new();
    for (slot, start) in candidates.into_iter().take(MAX_FOLLOW_UPS_PER_ACK) {
        let mut replay_budget = store.ephemeral_v2.replay_budget.clone();
        if !replay_budget.admit(clock.mono_ms) {
            store.ephemeral_v2.count(LiveTypingCause::ResendSkipped);
            break;
        }
        let participant_confirmed = match start.participant.as_deref() {
            Some(participant) => match crate::rcs_group::live_roster_contains(
                &store.conn,
                start.conversation_id,
                participant,
            ) {
                Ok(confirmed) => confirmed,
                Err(_) => break,
            },
            None => false,
        };
        let roster = |_: ConversationId, _: &str| participant_confirmed;
        let result = seal_typing(
            store,
            TypingSealRequest {
                conversation_id: start.conversation_id,
                gateway_device_id: start.gateway_device_id,
                origin: start.origin,
                participant: start.participant,
                state: TypingStateV2::Started,
            },
            clock,
            &roster,
            Some(start.expires_at_wall_ms),
            true,
        );
        let outbound = match result {
            Ok(Some(outbound)) => outbound,
            Ok(None) => {
                store.ephemeral_v2.count(LiveTypingCause::ResendSkipped);
                break;
            }
            Err(_) => break,
        };
        store.ephemeral_v2.replay_budget = replay_budget;
        if let Some(entry) = store.ephemeral_v2.active_starts.get_mut(&slot) {
            entry.last_replay_mono_ms = Some(clock.mono_ms);
        }
        follow_ups.push(outbound.signal);
        store.ephemeral_v2.count(LiveTypingCause::ResendSent);
    }
    follow_ups
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
        store.ephemeral_v2.count(LiveTypingCause::FrameReceived);
        store.ephemeral_v2.count(LiveTypingCause::DecodeMalformed);
        return Ok(empty_outcome(store));
    }
    let Ok(frame) = serde_json::from_str::<EphemeralSignalV2>(frame_json) else {
        store.ephemeral_v2.clock = Some(clock_candidate);
        store.ephemeral_v2.count(LiveTypingCause::FrameReceived);
        store.ephemeral_v2.count(LiveTypingCause::DecodeMalformed);
        return Ok(empty_outcome(store));
    };
    open(store, &frame, clock)
}

fn typing_eligible(
    store: &Store,
    conversation: ConversationId,
    gateway: DeviceId,
    now_ms: i64,
) -> Result<bool, LiveError> {
    match crate::rcs_group::group_mapping(&store.conn, conversation)
        .map_err(|_| LiveError::InvalidContext)?
    {
        Some(mapping) => Ok(mapping.state == crate::rcs_group::GroupMappingState::Live
            && mapping.gateway_device_id == gateway),
        None => Ok(matches!(
            rcs_capability::projection(&store.conn, conversation, gateway, now_ms)
                .map_err(|_| LiveError::InvalidContext)?,
            Some(projection) if projection.eligibility == RcsEligibility::Eligible
        )),
    }
}

fn typing_slot(conversation: ConversationId, participant: Option<&str>) -> TypingSlot {
    participant.map_or_else(
        || conversation.into(),
        |participant| TypingSlot::participant(conversation, participant),
    )
}

fn empty_outcome(store: &Store) -> LiveOutcome {
    LiveOutcome {
        generation: LiveEnableGeneration(store.ephemeral_v2.generation),
        session_update: None,
        action: None,
        reply: None,
        follow_ups: Vec::new(),
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

#[cfg(test)]
mod group_seam_tests {
    use super::*;
    use crate::{
        Client, ClientConfig, DatabaseKey, KeyProfile, RcsCapabilityState,
        RcsEligibilityProjection, RcsLinkStatusEvent, VaultId,
    };
    use peppy_crypto::{create_vault_check_header, derive_root_key};

    const PASSPHRASE: &str = "correct horse battery staple";
    const ALEX: &str = "+15551230001";

    #[test]
    fn replay_seal_error_does_not_count_a_refusal() {
        let conversation = ConversationId::new();
        let (_directory, client, device) = fixture(conversation);
        seal_with(
            &client,
            conversation,
            device,
            TypingOriginV2::GoogleContact,
            None,
            1_001,
            &|_, _| true,
        )
        .unwrap()
        .unwrap();
        let mut store = client.lock().unwrap();
        store.ephemeral_v2.last_sequence = u64::MAX;
        let before = store.ephemeral_v2.counters;
        assert!(
            replay_active_starts(
                &mut store,
                device,
                LiveClock {
                    wall_ms: 1_002,
                    mono_ms: 1_002
                },
                1_002
            )
            .is_empty()
        );
        assert_eq!(store.ephemeral_v2.counters, before);
    }

    #[test]
    fn malformed_json_counts_after_clock_commit() {
        let (_directory, client, _) = fixture(ConversationId::new());
        let before = client.live_typing_counters().unwrap();
        client
            .open_typing_signal_v2_json(
                "{",
                LiveClock {
                    wall_ms: 1_001,
                    mono_ms: 1_001,
                },
            )
            .unwrap();
        let after = client.live_typing_counters().unwrap();
        for cause in [
            LiveTypingCause::FrameReceived,
            LiveTypingCause::DecodeMalformed,
        ] {
            let get = |values: &Vec<(LiveTypingCause, u64)>| {
                values.iter().find(|(c, _)| *c == cause).unwrap().1
            };
            assert_eq!(get(&after), get(&before) + 1);
        }
    }

    #[test]
    fn counters_survive_disable_and_reenable() {
        let conversation = ConversationId::new();
        let (_directory, client, device) = fixture(conversation);
        assert!(
            seal_with(
                &client,
                conversation,
                device,
                TypingOriginV2::GoogleContact,
                None,
                1_001,
                &|_, _| true,
            )
            .unwrap()
            .is_some()
        );
        let before = client.live_typing_counters().unwrap();
        assert_eq!(
            before
                .iter()
                .find(|(cause, _)| *cause == LiveTypingCause::SealStarted)
                .unwrap()
                .1,
            1
        );
        client.disable_live_typing().unwrap();
        client
            .enable_live_typing(
                LiveEnableContext {
                    trusted_roster: vec![device],
                    local_gateway_device_id: device,
                    local_executor: LiveExecutor::CurrentGateway,
                },
                LiveClock {
                    wall_ms: 1_002,
                    mono_ms: 1_002,
                },
            )
            .unwrap();
        assert_eq!(client.live_typing_counters().unwrap(), before);
    }

    #[test]
    fn seal_counters_report_each_decision() {
        let conversation = ConversationId::new();
        let (_directory, client, device) = fixture(conversation);
        let yes = |_: ConversationId, _: &str| true;
        let no = |_: ConversationId, _: &str| false;
        let count = |cause| {
            client
                .live_typing_counters()
                .unwrap()
                .into_iter()
                .find(|(candidate, _)| *candidate == cause)
                .unwrap()
                .1
        };
        client.set_typing_sharing(false).unwrap();
        assert!(
            seal_with(
                &client,
                conversation,
                device,
                TypingOriginV2::VaultDevice,
                None,
                1_001,
                &yes
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(count(LiveTypingCause::SealSharingOff), 1);
        assert!(
            seal_with(
                &client,
                conversation,
                device,
                TypingOriginV2::GoogleContact,
                Some(ALEX),
                1_002,
                &no
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(count(LiveTypingCause::SealParticipantNotMember), 1);
        assert!(
            seal_with(
                &client,
                ConversationId::new(),
                device,
                TypingOriginV2::GoogleContact,
                None,
                1_003,
                &yes
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(count(LiveTypingCause::SealNotEligible), 1);
        assert!(
            seal_with(
                &client,
                conversation,
                device,
                TypingOriginV2::GoogleContact,
                None,
                1_004,
                &yes
            )
            .unwrap()
            .is_some()
        );
        assert_eq!(count(LiveTypingCause::SealStarted), 1);
        assert!(
            seal_with(
                &client,
                conversation,
                device,
                TypingOriginV2::GoogleContact,
                None,
                1_005,
                &yes
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(count(LiveTypingCause::SealBudget), 1);
        let mut store = client.lock().unwrap();
        assert!(
            seal(
                &mut store,
                TypingSealRequest {
                    conversation_id: conversation,
                    gateway_device_id: device,
                    origin: TypingOriginV2::GoogleContact,
                    participant: None,
                    state: TypingStateV2::Stopped,
                },
                LiveClock {
                    wall_ms: 1_006,
                    mono_ms: 1_006
                },
                &yes
            )
            .unwrap()
            .is_some()
        );
        drop(store);
        assert_eq!(count(LiveTypingCause::SealStopped), 1);
    }

    fn fixture(conversation: ConversationId) -> (tempfile::TempDir, Client, DeviceId) {
        let directory = tempfile::tempdir().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key(PASSPHRASE, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let device_id = DeviceId::new();
        let config = ClientConfig {
            database_path: directory.path().join("seam.db"),
            vault_id,
            device_id,
        };
        let client = Client::open(config, DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
        client.unlock(&profile, &header, PASSPHRASE).unwrap();
        client.set_server_compaction_state(true, true).unwrap();
        client
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    gateway_device_id: device_id,
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
                    gateway_device_id: device_id,
                }],
                1_000,
            )
            .unwrap();
        client
            .enable_live_typing(
                LiveEnableContext {
                    trusted_roster: vec![device_id],
                    local_gateway_device_id: device_id,
                    local_executor: LiveExecutor::CurrentGateway,
                },
                LiveClock {
                    wall_ms: 1_000,
                    mono_ms: 1_000,
                },
            )
            .unwrap();
        (directory, client, device_id)
    }

    fn seal_with(
        client: &Client,
        conversation: ConversationId,
        gateway: DeviceId,
        origin: TypingOriginV2,
        participant: Option<&str>,
        mono_ms: u64,
        roster: GroupTypingRoster<'_>,
    ) -> Result<Option<LiveOutbound>, LiveError> {
        let mut store = client.lock().map_err(|_| LiveError::InvalidContext)?;
        seal(
            &mut store,
            TypingSealRequest {
                conversation_id: conversation,
                gateway_device_id: gateway,
                origin,
                participant: participant.map(str::to_owned),
                state: TypingStateV2::Started,
            },
            LiveClock {
                wall_ms: i64::try_from(mono_ms).unwrap(),
                mono_ms,
            },
            roster,
        )
    }

    #[test]
    fn participant_frames_seal_only_for_roster_confirmed_members() {
        let conversation = ConversationId::new();
        let (_directory, client, device) = fixture(conversation);
        let confirmed = |candidate: ConversationId, participant: &str| {
            candidate == conversation && participant == ALEX
        };
        let unknown = |_: ConversationId, _: &str| false;

        let sealed = seal_with(
            &client,
            conversation,
            device,
            TypingOriginV2::GoogleContact,
            Some(ALEX),
            1_001,
            &confirmed,
        )
        .unwrap()
        .unwrap();
        assert_eq!(sealed.signal.sequence, "1");

        assert!(
            seal_with(
                &client,
                conversation,
                device,
                TypingOriginV2::GoogleContact,
                Some("+15551230002"),
                1_002,
                &unknown,
            )
            .unwrap()
            .is_none(),
            "an unconfirmed participant is never sealed"
        );
        let next = seal_with(
            &client,
            conversation,
            device,
            TypingOriginV2::GoogleContact,
            Some(ALEX),
            4_500,
            &confirmed,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            next.signal.sequence, "2",
            "a suppressed unknown participant consumes no sequence"
        );
    }

    #[test]
    fn participant_attribution_is_rejected_for_vault_frames_and_malformed_numbers() {
        let conversation = ConversationId::new();
        let (_directory, client, device) = fixture(conversation);
        let confirmed = |_: ConversationId, _: &str| true;

        for (origin, participant) in [
            (TypingOriginV2::VaultDevice, Some(ALEX)),
            (TypingOriginV2::GoogleContact, Some("15551230001")),
            (TypingOriginV2::GoogleContact, Some("+0551230001")),
        ] {
            assert_eq!(
                seal_with(
                    &client,
                    conversation,
                    device,
                    origin,
                    participant,
                    1_001,
                    &confirmed,
                )
                .unwrap_err(),
                LiveError::InvalidContext,
                "{origin:?} with {participant:?}"
            );
        }
    }

    #[test]
    fn active_starts_refresh_stop_expire_and_clear_on_reenable() {
        let conversation = ConversationId::new();
        let (_directory, client, device) = fixture(conversation);
        let roster = |_: ConversationId, _: &str| true;
        let start = |time| {
            seal_with(
                &client,
                conversation,
                device,
                TypingOriginV2::GoogleContact,
                None,
                time,
                &roster,
            )
            .unwrap()
            .unwrap()
        };
        let first = start(1_001);
        let second = start(4_001);
        {
            let store = client.lock().unwrap();
            let entry = store.ephemeral_v2.active_starts.values().next().unwrap();
            assert_eq!(
                entry.expires_at_wall_ms.to_string(),
                second.signal.expires_at_ms
            );
            assert_ne!(first.signal.expires_at_ms, second.signal.expires_at_ms);
        }
        {
            let mut store = client.lock().unwrap();
            seal(
                &mut store,
                TypingSealRequest {
                    conversation_id: conversation,
                    gateway_device_id: device,
                    origin: TypingOriginV2::GoogleContact,
                    participant: None,
                    state: TypingStateV2::Stopped,
                },
                LiveClock {
                    wall_ms: 4_002,
                    mono_ms: 4_002,
                },
                &roster,
            )
            .unwrap();
            assert!(store.ephemeral_v2.active_starts.is_empty());
        }
        start(7_002);
        {
            let mut store = client.lock().unwrap();
            let context = store.ephemeral_v2.context.clone().unwrap();
            enable(
                &mut store,
                context,
                LiveClock {
                    wall_ms: 7_003,
                    mono_ms: 7_003,
                },
            )
            .unwrap();
            assert!(store.ephemeral_v2.active_starts.is_empty());
        }
        start(7_004);
        client.set_typing_sharing(false).unwrap();
        assert!(
            seal_with(
                &client,
                conversation,
                device,
                TypingOriginV2::VaultDevice,
                None,
                22_004,
                &roster
            )
            .unwrap()
            .is_none()
        );
        assert!(client.lock().unwrap().ephemeral_v2.active_starts.is_empty());
        start(22_005);
        assert_eq!(client.lock().unwrap().ephemeral_v2.active_starts.len(), 1);
        client.disable_live_typing().unwrap();
        assert!(client.lock().unwrap().ephemeral_v2.active_starts.is_empty());
    }

    #[test]
    fn sharing_off_clears_only_vault_device_starts() {
        let conversation = ConversationId::new();
        let (_directory, client, device) = fixture(conversation);
        let roster = |_: ConversationId, _: &str| true;
        seal_with(
            &client,
            conversation,
            device,
            TypingOriginV2::VaultDevice,
            None,
            1_001,
            &roster,
        )
        .unwrap()
        .unwrap();
        seal_with(
            &client,
            conversation,
            device,
            TypingOriginV2::GoogleContact,
            Some(ALEX),
            1_002,
            &roster,
        )
        .unwrap()
        .unwrap();
        client.set_typing_sharing(false).unwrap();
        let store = client.lock().unwrap();
        assert_eq!(store.ephemeral_v2.active_starts.len(), 1);
        assert_eq!(
            store
                .ephemeral_v2
                .active_starts
                .values()
                .next()
                .unwrap()
                .origin,
            TypingOriginV2::GoogleContact
        );
    }

    #[test]
    fn registry_evicts_nearest_expiry_and_refused_stop_removes_slot() {
        let conversation = ConversationId::new();
        let (_directory, client, device) = fixture(conversation);
        let oldest = ConversationId::new();
        let unreserved = ConversationId::new();
        {
            let mut store = client.lock().unwrap();
            for index in 0..MAX_ACTIVE_STARTS {
                let id = match index {
                    0 => oldest,
                    1 => unreserved,
                    _ => ConversationId::new(),
                };
                store.ephemeral_v2.active_starts.insert(
                    typing_slot(id, None),
                    ActiveStart {
                        conversation_id: id,
                        gateway_device_id: device,
                        origin: TypingOriginV2::GoogleContact,
                        participant: None,
                        expires_at_wall_ms: 5_000 + index as u64,
                        sealed_mono_ms: 1_000,
                        last_replay_mono_ms: None,
                    },
                );
            }
        }
        let roster = |_: ConversationId, _: &str| true;
        seal_with(
            &client,
            conversation,
            device,
            TypingOriginV2::GoogleContact,
            None,
            1_001,
            &roster,
        )
        .unwrap()
        .unwrap();
        let mut store = client.lock().unwrap();
        assert_eq!(store.ephemeral_v2.active_starts.len(), MAX_ACTIVE_STARTS);
        assert!(
            !store
                .ephemeral_v2
                .active_starts
                .contains_key(&typing_slot(oldest, None))
        );
        let result = seal(
            &mut store,
            TypingSealRequest {
                conversation_id: unreserved,
                gateway_device_id: device,
                origin: TypingOriginV2::GoogleContact,
                participant: None,
                state: TypingStateV2::Stopped,
            },
            LiveClock {
                wall_ms: 1_002,
                mono_ms: 1_002,
            },
            &roster,
        )
        .unwrap();
        assert!(result.is_none());
        assert!(
            !store
                .ephemeral_v2
                .active_starts
                .contains_key(&typing_slot(unreserved, None))
        );
    }

    #[test]
    fn replay_refuses_wrong_gateway_and_rechecks_live_roster() {
        let conversation = ConversationId::new();
        let (_directory, client, gateway) = fixture(conversation);
        let roster = |_: ConversationId, _: &str| true;
        seal_with(
            &client,
            conversation,
            gateway,
            TypingOriginV2::VaultDevice,
            None,
            1_001,
            &roster,
        )
        .unwrap()
        .unwrap();
        let mut store = client.lock().unwrap();
        let clock = LiveClock {
            wall_ms: 1_002,
            mono_ms: 1_002,
        };
        assert!(replay_active_starts(&mut store, DeviceId::new(), clock, clock.wall_ms).is_empty());
        assert_eq!(
            replay_active_starts(&mut store, gateway, clock, clock.wall_ms).len(),
            1
        );
        drop(store);

        // The seal seam accepts this participant, but there is no live membership in the DB.
        seal_with(
            &client,
            conversation,
            gateway,
            TypingOriginV2::GoogleContact,
            Some(ALEX),
            4_003,
            &roster,
        )
        .unwrap()
        .unwrap();
        let mut store = client.lock().unwrap();
        let clock = LiveClock {
            wall_ms: 4_004,
            mono_ms: 4_004,
        };
        let sequence = store.ephemeral_v2.last_sequence;
        assert!(replay_active_starts(&mut store, DeviceId::new(), clock, clock.wall_ms).is_empty());
        assert_eq!(store.ephemeral_v2.last_sequence, sequence);
    }
}
