use crate::{
    ConversationId, Error, RCS_CANONICALIZER_REVISION, RcsCapabilityState, RcsContextTuple,
    rcs_native::{
        NativeCarrierRouteBinding, NativeRcsEvidence, NativeRcsOutgoing, NativeSettingsCurrentness,
        NativeSettingsProvenance,
    },
};
use phonenumber::{Mode, parse};
use rusqlite::{Connection, OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const RCS_MAPPING_PROOF_VERSION: u32 = 2;
pub const RCS_MAPPING_FRESHNESS_MS: i64 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsMappingClass {
    OneToOne,
    SelfChat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RcsLinkState {
    Linked,
    Unlinked,
    NeedsRelink,
    SignedOut,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RcsConversationType {
    Rcs,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RcsSendMode {
    Automatic,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RcsParticipantObservation {
    pub identity: String,
    pub google_participant_id: String,
    pub identifier_type: i32,
    pub is_visible: bool,
    pub is_me: bool,
}

/// Host facts required to construct an exact, local-only mapping proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RcsMappingObservation {
    pub google_conversation_id: String,
    /// Number of eligible Google candidates from a completed, target- and session-scoped lookup.
    /// Partial indexes and targeted fetches cannot attest uniqueness.
    pub google_candidate_count: u32,
    pub participants: Vec<RcsParticipantObservation>,
    pub conversation_type: RcsConversationType,
    pub status: String,
    pub send_mode: RcsSendMode,
    pub read_only: bool,
    pub default_outgoing: bool,
    pub selected_sim_id: String,
    pub selected_sim_own_identity: String,
    pub carrier_binding: NativeCarrierRouteBinding,
    pub outgoing: NativeRcsOutgoing,
    pub native_evidence: NativeRcsEvidence,
    pub identity_region: String,
    pub observed_at_ms: i64,
    pub link_generation: u64,
    pub session_generation: u64,
    /// Monotonic gateway-wide context revision (region, SIM, own identity, canonicalizer, link).
    pub context_revision: u64,
    pub canonicalizer_revision: u32,
    pub mapping_revision: u64,
    pub link_state: RcsLinkState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsMappingRefusal {
    StaleObservation,
    GoogleConversationAmbiguous,
    LinkUnavailable,
    IneligibleConversation,
    InvalidIdentity,
    ParticipantAmbiguous,
    SelfChatMismatch,
    PeppyConversationMissing,
    PeppyConversationAmbiguous,
    MappingConflict,
    NonMonotonicRevision,
    RetiredContext,
    PeppyConversationUncertain,
    SelfChatUnsupported,
}

impl RcsMappingRefusal {
    pub const fn code(self) -> &'static str {
        match self {
            Self::StaleObservation => "stale_observation",
            Self::GoogleConversationAmbiguous => "google_conversation_ambiguous",
            Self::LinkUnavailable => "link_unavailable",
            Self::IneligibleConversation => "ineligible_conversation",
            Self::InvalidIdentity => "invalid_identity",
            Self::ParticipantAmbiguous => "participant_ambiguous",
            Self::SelfChatMismatch => "self_chat_mismatch",
            Self::PeppyConversationMissing => "peppy_conversation_missing",
            Self::PeppyConversationAmbiguous => "peppy_conversation_ambiguous",
            Self::MappingConflict => "mapping_conflict",
            Self::NonMonotonicRevision => "nonmonotonic_revision",
            Self::RetiredContext => "retired_context",
            Self::PeppyConversationUncertain => "peppy_conversation_uncertain",
            Self::SelfChatUnsupported => "self_chat_unsupported",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RcsMappingOutcome {
    Mapped(RcsMapping),
    Refused(RcsMappingRefusal),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsMappingProof {
    pub canonical_participant: String,
    pub identity_region: String,
    pub own_identity: String,
    pub digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsMapping {
    pub conversation_id: ConversationId,
    pub google_conversation_id: String,
    pub class: RcsMappingClass,
    pub proof_version: u32,
    pub proof: RcsMappingProof,
    pub mapping_revision: u64,
    pub verified_at_ms: i64,
}

pub(crate) fn verify_and_store(
    conn: &Connection,
    observation: &RcsMappingObservation,
    now_ms: i64,
) -> Result<RcsMappingOutcome, Error> {
    if !is_fresh(now_ms, observation.observed_at_ms) {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::StaleObservation,
        ));
    }
    if observation.canonicalizer_revision != RCS_CANONICALIZER_REVISION {
        update_context(conn, observation)?;
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::IneligibleConversation,
        ));
    }
    match update_context(conn, observation)? {
        ContextUpdate::Retired => {
            return Ok(RcsMappingOutcome::Refused(
                RcsMappingRefusal::RetiredContext,
            ));
        }
        ContextUpdate::Changed => invalidate_all_mappings(conn, "context_changed")?,
        ContextUpdate::Unchanged | ContextUpdate::Advanced => {}
    }
    if observation.link_state == RcsLinkState::Linked {
        record_prior_link(conn)?;
    }
    record_accepted_carrier_route(conn, observation, now_ms)?;
    let outcome = verify(conn, observation, now_ms)?;
    let RcsMappingOutcome::Mapped(mapping) = &outcome else {
        if !matches!(
            outcome,
            RcsMappingOutcome::Refused(RcsMappingRefusal::NonMonotonicRevision)
        ) {
            invalidate_for_refusal(conn, observation, refusal_code(&outcome))?;
        }
        return Ok(outcome);
    };
    put(conn, mapping)?;
    Ok(outcome)
}

fn refusal_code(outcome: &RcsMappingOutcome) -> &'static str {
    match outcome {
        RcsMappingOutcome::Refused(reason) => reason.code(),
        RcsMappingOutcome::Mapped(_) => unreachable!(),
    }
}

fn verify(
    conn: &Connection,
    observation: &RcsMappingObservation,
    now_ms: i64,
) -> Result<RcsMappingOutcome, Error> {
    if !is_fresh(now_ms, observation.observed_at_ms) {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::StaleObservation,
        ));
    }
    if observation.google_candidate_count != 1
        || !observation.native_evidence.lookup_complete
        || observation.native_evidence.google_candidate_count != 1
    {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::GoogleConversationAmbiguous,
        ));
    }
    if observation.link_state != RcsLinkState::Linked {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::LinkUnavailable,
        ));
    }
    if observation.google_conversation_id.is_empty()
        || observation.conversation_type != RcsConversationType::Rcs
        || observation.status != "eligible"
        || observation.send_mode != RcsSendMode::Automatic
        || observation.read_only
        || observation.default_outgoing != !observation.outgoing.outgoing_id.is_empty()
        || observation.selected_sim_id.is_empty()
        || observation.identity_region.is_empty()
    {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::IneligibleConversation,
        ));
    }

    let own_identity = match canonical_e164(&observation.selected_sim_own_identity) {
        Some(identity) => identity,
        None => {
            return Ok(RcsMappingOutcome::Refused(
                RcsMappingRefusal::InvalidIdentity,
            ));
        }
    };
    let visible = match validated_visible_participants(observation) {
        Ok(visible) => visible,
        Err(reason) => return Ok(RcsMappingOutcome::Refused(reason)),
    };

    if !native_observation_is_current(observation, now_ms, &own_identity) {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::IneligibleConversation,
        ));
    }

    if visible
        .iter()
        .filter(|participant| participant.is_me)
        .any(|participant| {
            canonical_identity(&participant.identity).as_deref() != Some(own_identity.as_str())
        })
    {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::SelfChatMismatch,
        ));
    }

    let non_self = visible
        .iter()
        .filter(|participant| !participant.is_me)
        .collect::<Vec<_>>();
    let (class, participant) = if non_self.is_empty() {
        if visible.iter().all(|participant| {
            participant.is_me
                && canonical_identity(&participant.identity).as_deref()
                    == Some(own_identity.as_str())
        }) {
            return Ok(RcsMappingOutcome::Refused(
                RcsMappingRefusal::SelfChatUnsupported,
            ));
        } else {
            return Ok(RcsMappingOutcome::Refused(
                RcsMappingRefusal::SelfChatMismatch,
            ));
        }
    } else if non_self.len() == 1 {
        let Some(participant) = canonical_identity(&non_self[0].identity) else {
            return Ok(RcsMappingOutcome::Refused(
                RcsMappingRefusal::InvalidIdentity,
            ));
        };
        if participant == own_identity {
            return Ok(RcsMappingOutcome::Refused(
                RcsMappingRefusal::SelfChatMismatch,
            ));
        }
        (RcsMappingClass::OneToOne, participant)
    } else {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::ParticipantAmbiguous,
        ));
    };

    let (conversations, uncertain) = peppy_conversations_for_identity(conn, &participant)?;
    if uncertain {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::PeppyConversationUncertain,
        ));
    }
    let [conversation_id] = conversations.as_slice() else {
        return Ok(RcsMappingOutcome::Refused(if conversations.is_empty() {
            RcsMappingRefusal::PeppyConversationMissing
        } else {
            RcsMappingRefusal::PeppyConversationAmbiguous
        }));
    };
    if has_other_active_mapping(conn, *conversation_id, &observation.google_conversation_id)? {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::MappingConflict,
        ));
    }
    if has_google_mapping_for_other_conversation(
        conn,
        *conversation_id,
        &observation.google_conversation_id,
    )? {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::MappingConflict,
        ));
    }
    if let Some(current_revision) = highest_revision(conn, *conversation_id)?
        && observation.mapping_revision <= current_revision
    {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::NonMonotonicRevision,
        ));
    }

    let digest = proof_digest(observation, class, &own_identity, &visible);
    let proof = RcsMappingProof {
        canonical_participant: participant,
        identity_region: observation.identity_region.clone(),
        own_identity,
        digest,
    };
    Ok(RcsMappingOutcome::Mapped(RcsMapping {
        conversation_id: *conversation_id,
        google_conversation_id: observation.google_conversation_id.clone(),
        class,
        proof_version: RCS_MAPPING_PROOF_VERSION,
        proof,
        mapping_revision: observation.mapping_revision,
        verified_at_ms: observation.observed_at_ms,
    }))
}

pub(crate) fn canonical_recipient(identity: &str) -> Option<String> {
    let normalized = identity.trim();
    if normalized.is_empty() {
        return None;
    }
    if normalized.starts_with('+') {
        return parse(None, normalized)
            .ok()
            .filter(|number| number.is_valid())
            .map(|number| number.format().mode(Mode::E164).to_string());
    }
    Some(format!("raw:{normalized}"))
}

fn canonical_identity(identity: &str) -> Option<String> {
    canonical_recipient(identity)
}

fn canonical_e164(identity: &str) -> Option<String> {
    let identity = canonical_identity(identity)?;
    identity.starts_with('+').then_some(identity)
}

fn native_attestation_is_current(
    observation: &RcsMappingObservation,
    now_ms: i64,
    own_identity: &str,
) -> bool {
    let evidence = &observation.native_evidence;
    let binding = &observation.carrier_binding;
    let outgoing = &observation.outgoing;
    let fresh = |observed_at_ms| {
        now_ms
            .checked_sub(observed_at_ms)
            .is_some_and(|age| (0..=RCS_MAPPING_FRESHNESS_MS).contains(&age))
    };
    !evidence.lifetime.runtime_id.is_empty()
        && observation.link_generation > 0
        && observation.session_generation > 0
        && observation.context_revision > 0
        && evidence.lifetime.run_generation > 0
        && evidence.lifetime.link_generation == observation.link_generation
        && evidence.lifetime.session_generation == observation.session_generation
        && evidence.lifetime.context_revision == observation.context_revision
        && !evidence.lookup_id.is_empty()
        && evidence.settings_revision > 0
        && evidence.settings_stream_epoch > 0
        && evidence.settings_provenance == NativeSettingsProvenance::CurrentEvent
        && evidence.settings_observed_at_ms > 0
        && evidence.settings_observed_at_ms <= evidence.settings_validated_at_ms
        && fresh(evidence.settings_validated_at_ms)
        && fresh(evidence.conversation_observed_at_ms)
        && binding.carrier_route_id == observation.selected_sim_id
        && binding.carrier_route_id
            == format!("android-subscription-{}", binding.android_subscription_id)
        && binding.is_current_default_sms
        && binding.route_context_revision == observation.context_revision
        && fresh(binding.route_observed_at_ms)
        && !binding.same_handset_evidence_id.is_empty()
        && canonical_e164(&binding.own_number_raw).as_deref() == Some(own_identity)
        && !outgoing.outgoing_id.is_empty()
        && !outgoing.sim_participant_id.is_empty()
}

fn native_observation_is_current(
    observation: &RcsMappingObservation,
    now_ms: i64,
    own_identity: &str,
) -> bool {
    native_settings_are_current(observation, now_ms, own_identity)
        && observation.native_evidence.lookup_complete
        && observation.native_evidence.google_candidate_count == 1
        && observation.native_evidence.google_candidate_count == observation.google_candidate_count
}

/// Route authority requires verified native settings even when the target lookup found no match.
fn native_settings_are_current(
    observation: &RcsMappingObservation,
    now_ms: i64,
    own_identity: &str,
) -> bool {
    let evidence = &observation.native_evidence;
    let outgoing = &observation.outgoing;
    let joined_cards = evidence
        .current_settings_sims
        .iter()
        .filter(|sim| sim.sim_participant_id == outgoing.outgoing_id)
        .collect::<Vec<_>>();
    let [selected_card] = joined_cards.as_slice() else {
        return false;
    };
    native_attestation_is_current(observation, now_ms, own_identity)
        && evidence.settings_currentness == NativeSettingsCurrentness::ContinuousVerifiedFeed
        && outgoing.sim_participant_id == outgoing.outgoing_id
        && canonical_e164(&outgoing.own_identity_raw).as_deref() == Some(own_identity)
        && outgoing.rcs_chats_enabled
        && selected_card.outgoing_id == outgoing.outgoing_id
        && selected_card.rcs_chats_enabled
        && selected_card.sim_payload == outgoing.sim_payload
        && canonical_e164(&selected_card.own_identity_raw).as_deref() == Some(own_identity)
        && evidence
            .current_settings_sims
            .iter()
            .filter(|sim| canonical_e164(&sim.own_identity_raw).as_deref() == Some(own_identity))
            .count()
            == 1
}

fn is_fresh(now_ms: i64, observed_at_ms: i64) -> bool {
    now_ms
        .checked_sub(observed_at_ms)
        .is_some_and(|age| (0..=RCS_MAPPING_FRESHNESS_MS).contains(&age))
}

fn validated_visible_participants(
    observation: &RcsMappingObservation,
) -> Result<Vec<&RcsParticipantObservation>, RcsMappingRefusal> {
    let visible = observation
        .participants
        .iter()
        .filter(|participant| participant.is_visible)
        .collect::<Vec<_>>();
    if visible.iter().any(|participant| {
        participant.identity.trim().is_empty()
            || participant.google_participant_id.trim().is_empty()
            || canonical_identity(&participant.identity).is_none()
    }) {
        return Err(RcsMappingRefusal::InvalidIdentity);
    }
    (!visible.is_empty())
        .then_some(visible)
        .ok_or(RcsMappingRefusal::ParticipantAmbiguous)
}

fn peppy_conversations_for_identity(
    conn: &Connection,
    identity: &str,
) -> Result<(Vec<ConversationId>, bool), Error> {
    let mut statement = conn.prepare("SELECT conversation_id,payload FROM messages")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut conversations = std::collections::BTreeSet::new();
    let mut uncertain = false;
    for row in rows {
        let (conversation_id, payload) = row?;
        let payload: crate::MessagePayload = crate::decode(payload.as_bytes())?;
        let identities = payload
            .sender_address
            .iter()
            .chain(payload.recipients.iter());
        let identities = identities
            .filter_map(|address| canonical_identity(address))
            .collect::<std::collections::BTreeSet<_>>();
        if identities.contains(identity) && payload.mms_context.is_some() {
            uncertain = true;
        } else if identities.len() == 1 && identities.contains(identity) {
            conversations.insert(crate::parse(&conversation_id).map_err(|_| Error::Database)?);
        }
    }
    Ok((conversations.into_iter().collect(), uncertain))
}

fn highest_revision(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<Option<u64>, Error> {
    conn.query_row(
        "SELECT MAX(mapping_revision) FROM rcs_mapping_proofs WHERE peppy_conversation_id=?",
        [conversation_id.to_string()],
        |row| row.get::<_, Option<u64>>(0),
    )
    .map_err(Into::into)
}

fn has_other_active_mapping(
    conn: &Connection,
    conversation_id: ConversationId,
    google_conversation_id: &str,
) -> Result<bool, Error> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM rcs_conversation_mappings WHERE peppy_conversation_id=? AND google_conversation_id<>? AND invalidated_at_ms IS NULL)",
        params![conversation_id.to_string(), google_conversation_id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn has_google_mapping_for_other_conversation(
    conn: &Connection,
    conversation_id: ConversationId,
    google_conversation_id: &str,
) -> Result<bool, Error> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM rcs_conversation_mappings WHERE google_conversation_id=? AND peppy_conversation_id<>? AND invalidated_at_ms IS NULL)",
        params![google_conversation_id, conversation_id.to_string()],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn proof_digest(
    observation: &RcsMappingObservation,
    class: RcsMappingClass,
    own_identity: &str,
    participants: &[&RcsParticipantObservation],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"peppy/rcs-mapping-proof/v2\0");
    for value in [
        &observation.google_conversation_id,
        &observation.status,
        &observation.selected_sim_id,
        &observation.identity_region,
        &observation.carrier_binding.carrier_route_id,
        &observation.carrier_binding.own_number_raw,
        &observation.carrier_binding.same_handset_evidence_id,
        &observation.outgoing.outgoing_id,
        &observation.outgoing.sim_participant_id,
        &observation.outgoing.own_identity_raw,
    ] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    digest.update((own_identity.len() as u64).to_be_bytes());
    digest.update(own_identity.as_bytes());
    digest.update([
        class as u8,
        observation.conversation_type as u8,
        observation.send_mode as u8,
    ]);
    digest.update([
        u8::from(observation.read_only),
        u8::from(observation.default_outgoing),
    ]);
    digest.update(observation.link_generation.to_be_bytes());
    digest.update(observation.session_generation.to_be_bytes());
    digest.update(observation.context_revision.to_be_bytes());
    digest.update(observation.canonicalizer_revision.to_be_bytes());
    digest.update(observation.mapping_revision.to_be_bytes());
    digest.update(
        observation
            .carrier_binding
            .android_subscription_id
            .to_be_bytes(),
    );
    digest.update(
        observation
            .carrier_binding
            .route_context_revision
            .to_be_bytes(),
    );
    digest.update([
        u8::from(observation.carrier_binding.is_current_default_sms),
        u8::from(observation.outgoing.rcs_chats_enabled),
    ]);
    digest.update(observation.outgoing.sim_payload.two.to_be_bytes());
    digest.update(observation.outgoing.sim_payload.sim_number.to_be_bytes());
    let mut participants = participants
        .iter()
        .filter_map(|participant| {
            canonical_identity(&participant.identity).map(|identity| {
                (
                    participant.is_visible,
                    participant.is_me,
                    participant.identifier_type,
                    participant.google_participant_id.as_str(),
                    identity,
                )
            })
        })
        .collect::<Vec<_>>();
    participants.sort_unstable();
    for (is_visible, is_me, identifier_type, google_participant_id, identity) in participants {
        digest.update([u8::from(is_visible), u8::from(is_me)]);
        digest.update(identifier_type.to_be_bytes());
        digest.update((google_participant_id.len() as u64).to_be_bytes());
        digest.update(google_participant_id.as_bytes());
        digest.update((identity.len() as u64).to_be_bytes());
        digest.update(identity.as_bytes());
    }
    digest.finalize().into()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextUpdate {
    Unchanged,
    Changed,
    Retired,
    Advanced,
}

const RCS_CONTEXT_KEY: &str = "rcs_mapping_context_v1";
const RCS_ACCEPTED_CARRIER_ROUTE_KEY: &str = "rcs_accepted_carrier_route_v1";
const RCS_LINK_LIFECYCLE_KEY: &str = "rcs_link_lifecycle_v1";
const RCS_LINK_LIFECYCLE_OBSERVED_AT_KEY: &str = "rcs_link_lifecycle_observed_at_ms_v1";
const RCS_LINK_LIFECYCLE_GENERATION_KEY: &str = "rcs_link_lifecycle_generation_v1";
const RCS_PRIOR_LINK_KEY: &str = "rcs_prior_link_v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecordedLinkLifecycle {
    pub state: RcsCapabilityState,
    pub observed_at_ms: Option<i64>,
    pub generation: u64,
}

pub(crate) type OptionalRcsContextTuple = Option<(u64, u64, u64)>;

pub(crate) fn next_rcs_context_tuple(
    conn: &Connection,
    go_runtime_id: &str,
    go_run_generation: u64,
    go_stream_epoch: u64,
    go_pairing_evidence_id: &str,
) -> Result<RcsContextTuple, Error> {
    let stored = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [RCS_CONTEXT_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| serde_json::from_str::<RcsContext>(&value).map_err(|_| Error::Database))
        .transpose()?;
    let tuple = match stored {
        None => RcsContextTuple {
            link_generation: 1,
            session_generation: 1,
            context_revision: 1,
        },
        Some(stored) if stored.go_pairing_evidence_id != go_pairing_evidence_id => {
            RcsContextTuple {
                link_generation: next_context_component(stored.link_generation)?,
                session_generation: 1,
                context_revision: 1,
            }
        }
        Some(stored)
            if stored.go_runtime_id != go_runtime_id
                || stored.go_run_generation != go_run_generation
                || stored.go_stream_epoch != go_stream_epoch =>
        {
            RcsContextTuple {
                link_generation: stored.link_generation,
                session_generation: next_context_component(stored.session_generation)?,
                context_revision: 1,
            }
        }
        Some(stored) => RcsContextTuple {
            link_generation: stored.link_generation,
            session_generation: stored.session_generation,
            context_revision: next_context_component(stored.context_revision)?,
        },
    };
    ensure_sqlite_context_tuple(tuple)?;
    Ok(tuple)
}

pub(crate) fn next_rcs_mapping_revision(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<u64, Error> {
    let maximum: Option<i64> = conn.query_row(
        "SELECT MAX(mapping_revision) FROM (
            SELECT mapping_revision FROM rcs_conversation_mappings WHERE peppy_conversation_id=?
            UNION ALL
            SELECT mapping_revision FROM rcs_mapping_proofs WHERE peppy_conversation_id=?
        )",
        [conversation_id.to_string(), conversation_id.to_string()],
        |row| row.get(0),
    )?;
    maximum
        .map(|revision| {
            u64::try_from(revision)
                .ok()
                .and_then(|revision| revision.checked_add(1))
                .filter(|revision| *revision <= i64::MAX as u64)
                .ok_or(Error::InvalidRequest("RCS mapping revision exhausted"))
        })
        .transpose()
        .map(|revision| revision.unwrap_or(1))
}

fn next_context_component(value: u64) -> Result<u64, Error> {
    value
        .checked_add(1)
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or(Error::InvalidRequest("RCS context tuple exhausted"))
}

fn ensure_sqlite_context_tuple(tuple: RcsContextTuple) -> Result<(), Error> {
    [
        tuple.link_generation,
        tuple.session_generation,
        tuple.context_revision,
    ]
    .into_iter()
    .all(|value| value <= i64::MAX as u64)
    .then_some(())
    .ok_or(Error::InvalidRequest("RCS context tuple exhausted"))
}

pub(crate) fn record_link_lifecycle(
    conn: &Connection,
    state: RcsCapabilityState,
    observed_at_ms: i64,
) -> Result<bool, Error> {
    let previous = recorded_link_lifecycle_metadata(conn)?;
    if previous.is_some_and(|previous| previous.state == RcsCapabilityState::Linked)
        || state == RcsCapabilityState::Linked
    {
        record_prior_link(conn)?;
    }
    let changed = previous.is_none_or(|previous| previous.state != state);
    let generation = if changed {
        previous
            .map(|previous| previous.generation)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(Error::InvalidRequest("RCS lifecycle generation exhausted"))?
    } else {
        previous.map(|previous| previous.generation).unwrap_or(0)
    };
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![
            RCS_LINK_LIFECYCLE_KEY,
            serde_json::to_string(&state).map_err(|_| Error::Database)?
        ],
    )?;
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![
            RCS_LINK_LIFECYCLE_OBSERVED_AT_KEY,
            observed_at_ms.to_string()
        ],
    )?;
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![RCS_LINK_LIFECYCLE_GENERATION_KEY, generation.to_string()],
    )?;
    Ok(changed)
}

pub(crate) fn record_prior_link(conn: &Connection) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO NOTHING",
        [RCS_PRIOR_LINK_KEY, "1"],
    )?;
    Ok(())
}

/// Returns whether this database has ever had local linked authority.
/// Legacy databases without the marker derive and persist it from retained mapping history.
pub(crate) fn has_prior_link(conn: &Connection) -> Result<bool, Error> {
    let marked: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM metadata WHERE k=?)",
        [RCS_PRIOR_LINK_KEY],
        |row| row.get(0),
    )?;
    if marked {
        return Ok(true);
    }
    let mapped: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM rcs_conversation_mappings)",
        [],
        |row| row.get(0),
    )?;
    let linked_context = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [RCS_CONTEXT_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| serde_json::from_str::<RcsContext>(&value).map_err(|_| Error::Database))
        .transpose()?
        .is_some_and(|context| {
            context.link_state == RcsLinkState::Linked as u8 && context.has_authentic_go_binding()
        });
    let linked_route = accepted_carrier_route(conn)?.is_some_and(|route| {
        route.context.link_state == RcsLinkState::Linked as u8
            && route.context.has_authentic_go_binding()
    });
    let has_prior_link = mapped || linked_context || linked_route;
    if has_prior_link {
        record_prior_link(conn)?;
    }
    Ok(has_prior_link)
}

pub(crate) fn recorded_link_lifecycle(
    conn: &Connection,
) -> Result<Option<RcsCapabilityState>, Error> {
    conn.query_row(
        "SELECT v FROM metadata WHERE k=?",
        [RCS_LINK_LIFECYCLE_KEY],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|value| serde_json::from_str(&value).map_err(|_| Error::Database))
    .transpose()
}

pub(crate) fn recorded_link_lifecycle_metadata(
    conn: &Connection,
) -> Result<Option<RecordedLinkLifecycle>, Error> {
    let Some(state) = recorded_link_lifecycle(conn)? else {
        return Ok(None);
    };
    let observed_at_ms = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [RCS_LINK_LIFECYCLE_OBSERVED_AT_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| value.parse().map_err(|_| Error::Database))
        .transpose()?;
    let generation = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [RCS_LINK_LIFECYCLE_GENERATION_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| value.parse().map_err(|_| Error::Database))
        .transpose()?
        .unwrap_or(0);
    Ok(Some(RecordedLinkLifecycle {
        state,
        observed_at_ms,
        generation,
    }))
}

pub(crate) fn update_context(
    conn: &Connection,
    observation: &RcsMappingObservation,
) -> Result<ContextUpdate, Error> {
    if observation.context_revision == 0 {
        return Ok(ContextUpdate::Retired);
    }
    let context = context_value(observation);
    let current = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [RCS_CONTEXT_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(current) = current {
        let current: RcsContext = match serde_json::from_str(&current) {
            Ok(current) => current,
            Err(_) => {
                let legacy: LegacyRcsContext =
                    serde_json::from_str(&current).map_err(|_| Error::Database)?;
                if (
                    observation.link_generation,
                    observation.session_generation,
                    observation.context_revision,
                ) < (
                    legacy.link_generation,
                    legacy.session_generation,
                    legacy.context_revision,
                ) {
                    return Ok(ContextUpdate::Retired);
                }
                invalidate_all_mappings(conn, "legacy_context")?;
                conn.execute("INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v", params![RCS_CONTEXT_KEY, context])?;
                return Ok(ContextUpdate::Changed);
            }
        };
        if (
            observation.link_generation,
            observation.session_generation,
            observation.context_revision,
        ) < (
            current.link_generation,
            current.session_generation,
            current.context_revision,
        ) {
            return Ok(ContextUpdate::Retired);
        }
        let incoming: RcsContext = serde_json::from_str(&context).map_err(|_| Error::Database)?;
        if (
            observation.link_generation,
            observation.session_generation,
            observation.context_revision,
        ) == (
            current.link_generation,
            current.session_generation,
            current.context_revision,
        ) && incoming != current
        {
            invalidate_all_mappings(conn, "context_contradiction")?;
            conn.execute(
                "DELETE FROM metadata WHERE k=?",
                [RCS_ACCEPTED_CARRIER_ROUTE_KEY],
            )?;
            conn.execute(
                "DELETE FROM metadata WHERE k LIKE 'rcs_carrier_only_proof_v1/%'",
                [],
            )?;
            return Ok(ContextUpdate::Retired);
        }
        if incoming == current {
            return Ok(ContextUpdate::Unchanged);
        }
        if incoming.same_content(&current) {
            conn.execute(
                "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                params![RCS_CONTEXT_KEY, context],
            )?;
            return Ok(ContextUpdate::Advanced);
        }
    }
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![RCS_CONTEXT_KEY, context],
    )?;
    Ok(ContextUpdate::Changed)
}

pub(crate) fn admission_observation_targets_mapping(
    mapping: &RcsMapping,
    observation: &RcsMappingObservation,
    now_ms: i64,
) -> bool {
    is_fresh(now_ms, mapping.verified_at_ms)
        && is_fresh(now_ms, observation.observed_at_ms)
        && observation.google_conversation_id == mapping.google_conversation_id
}

pub(crate) fn admission_context_is_v2(conn: &Connection) -> Result<bool, Error> {
    let Some(value) = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [RCS_CONTEXT_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    if serde_json::from_str::<RcsContext>(&value).is_ok() {
        return Ok(true);
    }
    serde_json::from_str::<LegacyRcsContext>(&value)
        .map(|_| false)
        .map_err(|_| Error::Database)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MappingObservationStatus {
    Exact,
    CurrentWithdrawal,
    Mismatch,
}

pub(crate) fn mapping_observation_status(
    conn: &Connection,
    mapping: &RcsMapping,
    observation: &RcsMappingObservation,
    now_ms: i64,
) -> Result<MappingObservationStatus, Error> {
    if !is_fresh(now_ms, mapping.verified_at_ms) {
        return Ok(MappingObservationStatus::Mismatch);
    }
    let Some(context) = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [RCS_CONTEXT_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(MappingObservationStatus::Mismatch);
    };
    if observation.google_conversation_id != mapping.google_conversation_id
        || context != context_value(observation)
        || observation.link_state != RcsLinkState::Linked
        || mapping.mapping_revision != observation.mapping_revision
        || mapping.verified_at_ms != observation.observed_at_ms
    {
        return Ok(MappingObservationStatus::Mismatch);
    }
    let own_identity = match canonical_e164(&observation.selected_sim_own_identity) {
        Some(identity) if identity == mapping.proof.own_identity => identity,
        _ => return Ok(MappingObservationStatus::Mismatch),
    };
    if !native_attestation_is_current(observation, now_ms, &own_identity) {
        return Ok(MappingObservationStatus::Mismatch);
    }

    let visible = validated_visible_participants(observation).ok();
    let proof_matches = visible.as_ref().is_some_and(|visible| {
        mapping.proof.digest == proof_digest(observation, mapping.class, &own_identity, visible)
    });
    let (conversations, uncertain) =
        peppy_conversations_for_identity(conn, &mapping.proof.canonical_participant)?;
    let exact = observation.google_candidate_count == 1
        && observation.conversation_type == RcsConversationType::Rcs
        && observation.status == "eligible"
        && observation.send_mode == RcsSendMode::Automatic
        && !observation.read_only
        && observation.default_outgoing
        && native_observation_is_current(observation, now_ms, &own_identity)
        && proof_matches
        && !uncertain
        && conversations.as_slice() == [mapping.conversation_id];
    Ok(if exact {
        MappingObservationStatus::Exact
    } else {
        MappingObservationStatus::CurrentWithdrawal
    })
}

pub(crate) fn context_is_current(
    conn: &Connection,
    link_generation: u64,
    session_generation: u64,
    context_revision: u64,
    require_linked: bool,
) -> Result<bool, Error> {
    let Some(value) = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [RCS_CONTEXT_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    let context: RcsContext = serde_json::from_str(&value).map_err(|_| Error::Database)?;
    Ok((
        context.link_generation,
        context.session_generation,
        context.context_revision,
    ) == (link_generation, session_generation, context_revision)
        && (!require_linked || context.link_state == RcsLinkState::Linked as u8))
}

pub(crate) fn explicit_unlink_context(
    conn: &Connection,
    observed_at_ms: i64,
    now_ms: i64,
) -> Result<(OptionalRcsContextTuple, u64), Error> {
    now_ms
        .checked_sub(observed_at_ms)
        .filter(|age| (0..=RCS_MAPPING_FRESHNESS_MS).contains(age))
        .ok_or(Error::InvalidRequest("unlink context is not current"))?;
    let Some(lifecycle) = recorded_link_lifecycle_metadata(conn)? else {
        return Err(Error::InvalidRequest("explicit unlink context required"));
    };
    if lifecycle.state != RcsCapabilityState::Unlinked
        || lifecycle
            .observed_at_ms
            .is_none_or(|stored| observed_at_ms < stored || !is_fresh(now_ms, stored))
    {
        return Err(Error::InvalidRequest("explicit unlink context required"));
    }
    let Some(value) = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [RCS_CONTEXT_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok((None, lifecycle.generation));
    };
    let context: RcsContext = serde_json::from_str(&value).map_err(|_| Error::Database)?;
    if !context.has_authentic_go_binding() {
        return Err(Error::InvalidRequest("unlink context is not current"));
    }
    Ok((
        Some((
            context.link_generation,
            context.session_generation,
            context.context_revision,
        )),
        lifecycle.generation,
    ))
}

pub(crate) fn context_matches_carrier_proof(
    conn: &Connection,
    link_generation: u64,
    session_generation: u64,
    context_revision: u64,
    identity_region: &str,
    canonicalizer_revision: u32,
) -> Result<bool, Error> {
    let Some(value) = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [RCS_CONTEXT_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    let context: RcsContext = serde_json::from_str(&value).map_err(|_| Error::Database)?;
    // An empty proof region marks a region-independent (plus-prefixed) canonical recipient.
    Ok(context.link_state == RcsLinkState::Linked as u8
        && context.has_authentic_go_binding()
        && (
            context.link_generation,
            context.session_generation,
            context.context_revision,
        ) == (link_generation, session_generation, context_revision)
        && (identity_region.is_empty()
            || (context.identity_region == identity_region
                && crate::rcs_target::resolved_region(conn)
                    .map_err(|_| Error::Database)?
                    .is_some_and(|region| region == identity_region)))
        && context.canonicalizer_revision == canonicalizer_revision)
}

pub(crate) fn current_link_state(conn: &Connection) -> Result<Option<bool>, Error> {
    if let Some(state) = recorded_link_lifecycle(conn)? {
        return Ok(Some(state == RcsCapabilityState::Linked));
    }
    let value = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [RCS_CONTEXT_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    value
        .map(|value| {
            let context: RcsContext = serde_json::from_str(&value).map_err(|_| Error::Database)?;
            Ok(context.link_state == RcsLinkState::Linked as u8)
        })
        .transpose()
}

pub(crate) fn confirmed_own_identity(conn: &Connection) -> Result<Option<String>, Error> {
    let Some(value) = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [RCS_CONTEXT_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let context: RcsContext = serde_json::from_str(&value).map_err(|_| Error::Database)?;
    Ok((context.link_state == RcsLinkState::Linked as u8)
        .then(|| canonical_e164(&context.own_identity))
        .flatten())
}

pub(crate) fn has_live_mapping(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<bool, Error> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM rcs_conversation_mappings WHERE peppy_conversation_id=? AND invalidated_at_ms IS NULL)",
        [conversation_id.to_string()],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// Presence of accepted route evidence for the current context; it is not a time fence.
pub(crate) fn carrier_route_context(
    conn: &Connection,
    subscription_id: Option<&str>,
) -> Result<Option<crate::ValidatedCarrierRouteContext>, Error> {
    route_context(conn, subscription_id, None)
}

/// Accepted route evidence that is also current at `now_ms`. Classification uses this form.
pub(crate) fn current_carrier_route_context(
    conn: &Connection,
    now_ms: i64,
) -> Result<Option<crate::ValidatedCarrierRouteContext>, Error> {
    route_context(conn, None, Some(now_ms))
}

pub(crate) fn carrier_route_is_current(
    conn: &Connection,
    subscription_id: &str,
    now_ms: i64,
) -> Result<bool, Error> {
    Ok(route_context(conn, Some(subscription_id), Some(now_ms))?.is_some())
}

fn route_context(
    conn: &Connection,
    subscription_id: Option<&str>,
    now_ms: Option<i64>,
) -> Result<Option<crate::ValidatedCarrierRouteContext>, Error> {
    let Some(value) = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [RCS_CONTEXT_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let context: RcsContext = serde_json::from_str(&value).map_err(|_| Error::Database)?;
    if context.sim_id.is_empty()
        || !context.has_authentic_go_binding()
        || context.link_state != RcsLinkState::Linked as u8
        || subscription_id.is_some_and(|subscription_id| subscription_id != context.sim_id)
    {
        return Ok(None);
    }
    let Some(route) = accepted_carrier_route(conn)? else {
        return Ok(None);
    };
    if route.context != context
        || now_ms.is_some_and(|now_ms| !is_fresh(now_ms, route.accepted_at_ms))
    {
        return Ok(None);
    }
    Ok(Some(crate::ValidatedCarrierRouteContext::new(
        context.sim_id.clone(),
        context.link_generation,
        context.session_generation,
        context.context_revision,
        context.go_runtime_id,
        context.go_run_generation,
        context.go_stream_epoch,
        context.identity_region,
        context.canonicalizer_revision,
        Some(route.own_identity),
    )))
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct AcceptedCarrierRoute {
    context: RcsContext,
    own_identity: String,
    /// Records written before acceptance times existed read as stale (zero) and must be re-observed.
    #[serde(default)]
    accepted_at_ms: i64,
}

fn accepted_carrier_route(conn: &Connection) -> Result<Option<AcceptedCarrierRoute>, Error> {
    conn.query_row(
        "SELECT v FROM metadata WHERE k=?",
        [RCS_ACCEPTED_CARRIER_ROUTE_KEY],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|value| serde_json::from_str(&value).map_err(|_| Error::Database))
    .transpose()
}

fn record_accepted_carrier_route(
    conn: &Connection,
    observation: &RcsMappingObservation,
    now_ms: i64,
) -> Result<(), Error> {
    let Some(own_identity) = canonical_e164(&observation.selected_sim_own_identity) else {
        return Ok(());
    };
    if !native_settings_are_current(observation, now_ms, &own_identity) {
        return Ok(());
    }
    let context: RcsContext =
        serde_json::from_str(&context_value(observation)).map_err(|_| Error::Database)?;
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![
            RCS_ACCEPTED_CARRIER_ROUTE_KEY,
            serde_json::to_string(&AcceptedCarrierRoute {
                context,
                own_identity,
                accepted_at_ms: [
                    observation.observed_at_ms,
                    observation.carrier_binding.route_observed_at_ms,
                    observation.native_evidence.settings_validated_at_ms,
                    observation.native_evidence.conversation_observed_at_ms,
                ]
                .into_iter()
                .min()
                .unwrap_or(now_ms),
            })
            .map_err(|_| Error::Database)?
        ],
    )?;
    Ok(())
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct RcsContext {
    link_generation: u64,
    session_generation: u64,
    context_revision: u64,
    identity_region: String,
    own_identity: String,
    sim_id: String,
    outgoing_id: String,
    sim_participant_id: String,
    own_number: String,
    sim_payload_two: i32,
    sim_payload_sim_number: i32,
    canonicalizer_revision: u32,
    link_state: u8,
    #[serde(default)]
    go_runtime_id: String,
    #[serde(default)]
    go_run_generation: u64,
    #[serde(default)]
    go_stream_epoch: u64,
    #[serde(default)]
    go_pairing_evidence_id: String,
}

impl RcsContext {
    fn has_authentic_go_binding(&self) -> bool {
        !self.go_runtime_id.is_empty() && !self.go_pairing_evidence_id.is_empty()
    }

    fn same_content(&self, other: &Self) -> bool {
        self.identity_region == other.identity_region
            && self.own_identity == other.own_identity
            && self.sim_id == other.sim_id
            && self.outgoing_id == other.outgoing_id
            && self.sim_participant_id == other.sim_participant_id
            && self.own_number == other.own_number
            && self.sim_payload_two == other.sim_payload_two
            && self.sim_payload_sim_number == other.sim_payload_sim_number
            && self.canonicalizer_revision == other.canonicalizer_revision
            && self.link_state == other.link_state
            && self.go_pairing_evidence_id == other.go_pairing_evidence_id
    }
}

fn context_value(observation: &RcsMappingObservation) -> String {
    serde_json::to_string(&RcsContext {
        link_generation: observation.link_generation,
        session_generation: observation.session_generation,
        context_revision: observation.context_revision,
        identity_region: observation.identity_region.clone(),
        own_identity: canonical_e164(&observation.selected_sim_own_identity).unwrap_or_default(),
        sim_id: observation.selected_sim_id.clone(),
        outgoing_id: observation.outgoing.outgoing_id.clone(),
        sim_participant_id: observation.outgoing.sim_participant_id.clone(),
        own_number: observation.carrier_binding.own_number_raw.clone(),
        sim_payload_two: observation.outgoing.sim_payload.two,
        sim_payload_sim_number: observation.outgoing.sim_payload.sim_number,
        canonicalizer_revision: observation.canonicalizer_revision,
        link_state: observation.link_state as u8,
        go_runtime_id: observation.native_evidence.lifetime.runtime_id.clone(),
        go_run_generation: observation.native_evidence.lifetime.run_generation,
        go_stream_epoch: observation.native_evidence.settings_stream_epoch,
        go_pairing_evidence_id: observation.carrier_binding.same_handset_evidence_id.clone(),
    })
    .expect("RCS context is serializable")
}

#[derive(Deserialize)]
struct LegacyRcsContext {
    link_generation: u64,
    session_generation: u64,
    context_revision: u64,
}

fn invalidate_for_refusal(
    conn: &Connection,
    observation: &RcsMappingObservation,
    code: &str,
) -> Result<(), Error> {
    if code == RcsMappingRefusal::MappingConflict.code() {
        let own_identity = canonical_identity(&observation.selected_sim_own_identity);
        let participant = observation
            .participants
            .iter()
            .filter(|participant| participant.is_visible && !participant.is_me)
            .filter_map(|participant| canonical_identity(&participant.identity))
            .find(|identity| own_identity.as_deref() != Some(identity.as_str()))
            .or(own_identity);
        if let Some(participant) = participant {
            for conversation_id in peppy_conversations_for_identity(conn, &participant)?.0 {
                conn.execute(
                    "UPDATE rcs_conversation_mappings SET invalidated_at_ms=verified_at_ms, invalidation_code=? WHERE peppy_conversation_id=? AND invalidated_at_ms IS NULL",
                    params![code, conversation_id.to_string()],
                )?;
            }
        }
    }
    conn.execute(
        "UPDATE rcs_conversation_mappings SET invalidated_at_ms=verified_at_ms, invalidation_code=? WHERE google_conversation_id=? AND invalidated_at_ms IS NULL",
        params![code, observation.google_conversation_id],
    )?;
    Ok(())
}

pub(crate) fn invalidate_mapping(
    conn: &Connection,
    conversation_id: ConversationId,
    code: &str,
) -> Result<(), Error> {
    conn.execute(
        "UPDATE rcs_conversation_mappings SET invalidated_at_ms=verified_at_ms, invalidation_code=? WHERE peppy_conversation_id=? AND invalidated_at_ms IS NULL",
        params![code, conversation_id.to_string()],
    )?;
    Ok(())
}

pub(crate) fn invalidate_all_mappings(conn: &Connection, code: &str) -> Result<(), Error> {
    conn.execute(
        "UPDATE rcs_conversation_mappings SET invalidated_at_ms=verified_at_ms, invalidation_code=? WHERE invalidated_at_ms IS NULL",
        [code],
    )?;
    Ok(())
}

pub(crate) fn clear_accepted_carrier_route(conn: &Connection) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM metadata WHERE k=?",
        [RCS_ACCEPTED_CARRIER_ROUTE_KEY],
    )?;
    Ok(())
}

fn put(conn: &Connection, mapping: &RcsMapping) -> Result<(), Error> {
    let class = match mapping.class {
        RcsMappingClass::OneToOne => "one_to_one",
        RcsMappingClass::SelfChat => "self_chat",
    };
    conn.execute("INSERT INTO rcs_conversation_mappings(peppy_conversation_id,google_conversation_id,class,proof_version,proof_hash,mapping_revision,verified_at_ms,invalidated_at_ms,invalidation_code) VALUES(?,?,?,?,?,?,?,NULL,NULL) ON CONFLICT(peppy_conversation_id) DO UPDATE SET google_conversation_id=excluded.google_conversation_id,class=excluded.class,proof_version=excluded.proof_version,proof_hash=excluded.proof_hash,mapping_revision=excluded.mapping_revision,verified_at_ms=excluded.verified_at_ms,invalidated_at_ms=NULL,invalidation_code=NULL", params![mapping.conversation_id.to_string(), mapping.google_conversation_id, class, mapping.proof_version, mapping.proof.digest.to_vec(), mapping.mapping_revision, mapping.verified_at_ms])?;
    conn.execute("INSERT INTO rcs_mapping_proofs(peppy_conversation_id,mapping_revision,canonical_participant,identity_region,own_identity) VALUES(?,?,?,?,?)", params![mapping.conversation_id.to_string(), mapping.mapping_revision, mapping.proof.canonical_participant, mapping.proof.identity_region, mapping.proof.own_identity])?;
    Ok(())
}

pub(crate) fn get(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<Option<RcsMapping>, Error> {
    conn.query_row("SELECT m.google_conversation_id,m.class,m.proof_version,m.proof_hash,m.mapping_revision,m.verified_at_ms,p.canonical_participant,p.identity_region,p.own_identity FROM rcs_conversation_mappings m JOIN rcs_mapping_proofs p ON p.peppy_conversation_id=m.peppy_conversation_id AND p.mapping_revision=m.mapping_revision WHERE m.peppy_conversation_id=? AND m.invalidated_at_ms IS NULL", [conversation_id.to_string()], |r| {
        let digest: Vec<u8> = r.get(3)?;
        let digest: [u8; 32] = digest.try_into().map_err(|_| rusqlite::Error::InvalidColumnType(3, "proof_hash".into(), rusqlite::types::Type::Blob))?;
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, u32>(2)?, digest, r.get::<_, u64>(4)?, r.get::<_, i64>(5)?, r.get::<_, String>(6)?, r.get::<_, String>(7)?, r.get::<_, String>(8)?))
    }).optional()?.map(|(google, class, version, digest, revision, verified, participant, region, own)| Ok(RcsMapping { conversation_id, google_conversation_id: google, class: match class.as_str() { "one_to_one" => RcsMappingClass::OneToOne, "self_chat" => RcsMappingClass::SelfChat, _ => return Err(Error::Database) }, proof_version: version, proof: RcsMappingProof { canonical_participant: participant, identity_region: region, own_identity: own, digest }, mapping_revision: revision, verified_at_ms: verified })).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DeviceId, Direction, MessageId, MessagePayload, MessageRecord, SourceSequence, Transport,
    };

    fn metadata_connection() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE metadata(k TEXT PRIMARY KEY, v TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE rcs_conversation_mappings(id INTEGER PRIMARY KEY)",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn legacy_previous_linked_lifecycle_marks_prior_link_before_unlinked_overwrite() {
        let conn = metadata_connection();
        conn.execute(
            "INSERT INTO metadata(k,v) VALUES(?,?)",
            params![
                RCS_LINK_LIFECYCLE_KEY,
                serde_json::to_string(&RcsCapabilityState::Linked).unwrap()
            ],
        )
        .unwrap();
        record_link_lifecycle(&conn, RcsCapabilityState::Unlinked, 1_001).unwrap();
        assert!(has_prior_link(&conn).unwrap());
    }

    #[test]
    fn marker_absent_authentic_linked_context_derives_prior_link() {
        let conn = metadata_connection();
        let context = RcsContext {
            link_generation: 1,
            session_generation: 1,
            context_revision: 1,
            identity_region: "US".into(),
            own_identity: "+12025550100".into(),
            sim_id: "sim".into(),
            outgoing_id: "outgoing".into(),
            sim_participant_id: "participant".into(),
            own_number: "+12025550100".into(),
            sim_payload_two: 2,
            sim_payload_sim_number: 1,
            canonicalizer_revision: RCS_CANONICALIZER_REVISION,
            link_state: RcsLinkState::Linked as u8,
            go_runtime_id: "runtime".into(),
            go_run_generation: 1,
            go_stream_epoch: 1,
            go_pairing_evidence_id: "pairing".into(),
        };
        conn.execute(
            "INSERT INTO metadata(k,v) VALUES(?,?)",
            params![RCS_CONTEXT_KEY, serde_json::to_string(&context).unwrap()],
        )
        .unwrap();
        record_link_lifecycle(&conn, RcsCapabilityState::Unlinked, 1_001).unwrap();
        assert!(has_prior_link(&conn).unwrap());
    }

    fn observation() -> RcsMappingObservation {
        RcsMappingObservation {
            google_conversation_id: "google".into(),
            google_candidate_count: 1,
            participants: vec![RcsParticipantObservation {
                identity: "raw-peer".into(),
                google_participant_id: "google-peer".into(),
                identifier_type: 1,
                is_visible: true,
                is_me: false,
            }],
            conversation_type: RcsConversationType::Rcs,
            status: "eligible".into(),
            send_mode: RcsSendMode::Automatic,
            read_only: false,
            default_outgoing: true,
            selected_sim_id: "android-subscription-7".into(),
            selected_sim_own_identity: "+12025550100".into(),
            carrier_binding: NativeCarrierRouteBinding {
                carrier_route_id: "android-subscription-7".into(),
                android_subscription_id: 7,
                is_current_default_sms: true,
                own_number_raw: "+12025550100".into(),
                own_number_source: crate::NativeOwnNumberSource::SubscriptionManagerPhoneNumber,
                route_observed_at_ms: 1_000,
                route_context_revision: 1,
                same_handset_evidence_id: "verified-handset".into(),
            },
            outgoing: NativeRcsOutgoing {
                outgoing_id: "google-outgoing".into(),
                sim_participant_id: "google-outgoing".into(),
                own_identity_raw: "+12025550100".into(),
                sim_payload: crate::NativeRcsSimPayload {
                    two: 2,
                    sim_number: 7,
                },
                rcs_chats_enabled: true,
            },
            native_evidence: NativeRcsEvidence {
                lifetime: crate::NativeRcsLifetime {
                    runtime_id: "runtime".into(),
                    run_generation: 1,
                    link_generation: 1,
                    session_generation: 1,
                    context_revision: 1,
                },
                lookup_id: "lookup".into(),
                lookup_complete: true,
                google_candidate_count: 1,
                settings_revision: 1,
                settings_provenance: NativeSettingsProvenance::CurrentEvent,
                settings_observed_at_ms: 1_000,
                settings_stream_epoch: 1,
                settings_currentness: NativeSettingsCurrentness::ContinuousVerifiedFeed,
                current_settings_sims: vec![NativeRcsOutgoing {
                    outgoing_id: "google-outgoing".into(),
                    sim_participant_id: "google-outgoing".into(),
                    own_identity_raw: "+12025550100".into(),
                    sim_payload: crate::NativeRcsSimPayload {
                        two: 2,
                        sim_number: 7,
                    },
                    rcs_chats_enabled: true,
                }],
                settings_validated_at_ms: 1_000,
                conversation_observed_at_ms: 1_000,
            },
            identity_region: "ZZ".into(),
            observed_at_ms: 1_000,
            link_generation: 1,
            session_generation: 1,
            context_revision: 1,
            canonicalizer_revision: 1,
            mapping_revision: 1,
            link_state: RcsLinkState::Linked,
        }
    }

    #[test]
    fn revalidation_rejects_a_visible_malformed_participant_without_mutating_storage() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE metadata(k TEXT PRIMARY KEY,v TEXT NOT NULL); CREATE TABLE messages(conversation_id TEXT NOT NULL,payload TEXT NOT NULL); CREATE TABLE rcs_conversation_mappings(peppy_conversation_id TEXT PRIMARY KEY,google_conversation_id TEXT NOT NULL,class TEXT NOT NULL,proof_version INTEGER NOT NULL,proof_hash BLOB NOT NULL,mapping_revision INTEGER NOT NULL,verified_at_ms INTEGER NOT NULL,invalidated_at_ms INTEGER,invalidation_code TEXT); CREATE TABLE rcs_mapping_proofs(peppy_conversation_id TEXT NOT NULL,mapping_revision INTEGER NOT NULL,canonical_participant TEXT NOT NULL,identity_region TEXT NOT NULL,own_identity TEXT NOT NULL,PRIMARY KEY(peppy_conversation_id,mapping_revision));").unwrap();
        let observation = observation();
        let conversation_id = ConversationId::new();
        let payload = MessagePayload {
            record: MessageRecord {
                message_id: MessageId::new(),
                conversation_id,
                source_sequence: SourceSequence(1),
                attachments: Vec::new(),
            },
            source_device_id: DeviceId::new(),
            provider_message_id: None,
            sender_address: Some("raw-peer".into()),
            recipients: Vec::new(),
            subject: None,
            body: String::new(),
            transport: Transport::Sms,
            direction: Direction::Incoming,
            imported: true,
            sent_at_ms: None,
            ordered_at_ms: None,
            mms_context: None,
        };
        conn.execute(
            "INSERT INTO messages(conversation_id,payload) VALUES(?,?)",
            params![
                conversation_id.to_string(),
                serde_json::to_string(&payload).unwrap()
            ],
        )
        .unwrap();
        let visible = validated_visible_participants(&observation).unwrap();
        let mapping = RcsMapping {
            conversation_id,
            google_conversation_id: observation.google_conversation_id.clone(),
            class: RcsMappingClass::OneToOne,
            proof_version: RCS_MAPPING_PROOF_VERSION,
            proof: RcsMappingProof {
                canonical_participant: "raw:raw-peer".into(),
                identity_region: observation.identity_region.clone(),
                own_identity: "+12025550100".into(),
                digest: proof_digest(
                    &observation,
                    RcsMappingClass::OneToOne,
                    "+12025550100",
                    &visible,
                ),
            },
            mapping_revision: 1,
            verified_at_ms: 1_000,
        };
        put(&conn, &mapping).unwrap();
        assert!(matches!(
            update_context(&conn, &observation).unwrap(),
            ContextUpdate::Changed
        ));
        assert_eq!(
            mapping_observation_status(&conn, &mapping, &observation, 1_000).unwrap(),
            MappingObservationStatus::Exact
        );
        let mut malformed = observation;
        malformed.participants.push(RcsParticipantObservation {
            identity: "+not-an-identity".into(),
            google_participant_id: "malformed-google-peer".into(),
            identifier_type: 1,
            is_visible: true,
            is_me: false,
        });
        assert_ne!(
            mapping_observation_status(&conn, &mapping, &malformed, 1_000).unwrap(),
            MappingObservationStatus::Exact
        );
        assert_eq!(get(&conn, conversation_id).unwrap(), Some(mapping));
    }
}
