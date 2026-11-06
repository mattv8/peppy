use crate::{ConversationId, DeviceId, RcsEligibility, Store, rcs_capability};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use peppy_crypto::encrypt;
use peppy_protocol::{
    EPHEMERAL_V2_PROTOCOL_VERSION, EphemeralSignalV2, EphemeralSignalV2Kind, TypingOriginV2,
    TypingPlaintextV2, TypingStateV2,
};

use super::ephemeral_v2_policy::{ClockState, LiveClock as PolicyClock, PolicyError, SenderBudget};

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
        }
    }
}

impl EphemeralV2State {
    pub(crate) fn disable(&mut self) {
        self.enabled = false;
        self.last_sequence = 0;
        self.clock = None;
        self.budget = SenderBudget::new();
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
        || context.local_executor != LiveExecutor::CurrentGateway
        || context.local_gateway_device_id != store.config.device_id
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
    store.ephemeral_v2 = EphemeralV2State {
        enabled: true,
        generation,
        session_id: *uuid::Uuid::new_v4().as_bytes(),
        last_sequence: 0,
        clock: Some(clock),
        budget: SenderBudget::new(),
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
    if gateway_device_id != store.config.device_id {
        return Err(LiveError::InvalidContext);
    }
    let eligible = matches!(rcs_capability::projection(&store.conn, conversation_id, gateway_device_id, clock.wall_ms).map_err(|_| LiveError::InvalidContext)?, Some(projection) if projection.eligibility == RcsEligibility::Eligible);
    if !eligible {
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
    let mut budget_candidate = store.ephemeral_v2.budget.clone();
    let admitted = match state {
        TypingStateV2::Started => budget_candidate.admit_start(conversation_id, clock.mono_ms),
        TypingStateV2::Stopped => budget_candidate.admit_stop(conversation_id, clock.mono_ms),
    }
    .map_err(policy_error)?;
    if !admitted {
        store.ephemeral_v2.clock = Some(clock_candidate);
        store.ephemeral_v2.budget = budget_candidate;
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

fn policy_error(error: PolicyError) -> LiveError {
    match error {
        PolicyError::Clock => LiveError::Clock,
        PolicyError::InvalidContext | PolicyError::Capacity => LiveError::InvalidContext,
    }
}
