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
    ephemeral_v2_policy::{
        ClockState, LiveClock as PolicyClock, PolicyError, SenderBudget, TypingSlot,
    },
    ephemeral_v2_session::{
        AuthenticatedAck, AuthenticatedSequence, AuthenticatedTyping, ChallengeNonce,
        EphemeralV2SessionState, MonoTime, Participant, RoutedTyping, SessionId,
        SessionTypingState,
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
    if !store.ephemeral_v2.sharing_enabled && origin == TypingOriginV2::VaultDevice {
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
        return Ok(None);
    }
    let eligible = match crate::rcs_group::group_mapping(&store.conn, conversation_id)
        .map_err(|_| LiveError::InvalidContext)?
    {
        // Group typing (both directions) flows only while the group mapping is Live (I7).
        Some(mapping) => mapping.state == crate::rcs_group::GroupMappingState::Live,
        None => {
            matches!(rcs_capability::projection(&store.conn, conversation_id, gateway_device_id, effective_wall).map_err(|_| LiveError::InvalidContext)?, Some(projection) if projection.eligibility == RcsEligibility::Eligible)
        }
    };
    if !eligible {
        return Ok(None);
    }
    let slot = typing_slot(conversation_id, typing.participant.as_deref());
    let mut budget_candidate = store.ephemeral_v2.budget.clone();
    let admitted = match state {
        TypingStateV2::Started => budget_candidate.admit_start(slot, clock.mono_ms),
        TypingStateV2::Stopped => budget_candidate.admit_stop(slot, clock.mono_ms),
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
}
