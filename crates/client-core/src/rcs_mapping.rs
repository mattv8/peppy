use crate::{ConversationId, Error};
use phonenumber::{Mode, parse};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const RCS_MAPPING_PROOF_VERSION: u32 = 1;
pub const RCS_MAPPING_FRESHNESS_MS: i64 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsMappingClass {
    OneToOne,
    SelfChat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsLinkState {
    Linked,
    Unlinked,
    NeedsRelink,
    SignedOut,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsConversationType {
    Rcs,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsSendMode {
    Automatic,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsParticipantObservation {
    pub identity: String,
    pub is_visible: bool,
    pub is_me: bool,
}

/// Host facts required to construct an exact, local-only mapping proof.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    ContextMismatch,
    PeppyConversationUncertain,
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
            Self::ContextMismatch => "context_mismatch",
            Self::PeppyConversationUncertain => "peppy_conversation_uncertain",
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
    if now_ms.saturating_sub(observation.observed_at_ms).abs() > RCS_MAPPING_FRESHNESS_MS {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::StaleObservation,
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
    let outcome = verify(conn, observation, now_ms)?;
    let RcsMappingOutcome::Mapped(mapping) = &outcome else {
        invalidate_for_refusal(conn, observation, refusal_code(&outcome))?;
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
    if now_ms.saturating_sub(observation.observed_at_ms).abs() > RCS_MAPPING_FRESHNESS_MS {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::StaleObservation,
        ));
    }
    if observation.google_candidate_count != 1 {
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
        || !observation.default_outgoing
        || observation.selected_sim_id.is_empty()
        || observation.identity_region.is_empty()
    {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::IneligibleConversation,
        ));
    }

    let own_identity = match canonical_identity(&observation.selected_sim_own_identity) {
        Some(identity) => identity,
        None => {
            return Ok(RcsMappingOutcome::Refused(
                RcsMappingRefusal::InvalidIdentity,
            ));
        }
    };
    let visible = observation
        .participants
        .iter()
        .filter(|participant| participant.is_visible)
        .collect::<Vec<_>>();
    if visible.is_empty() {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::ParticipantAmbiguous,
        ));
    }
    if visible.iter().any(|participant| {
        !participant.identity.trim().is_empty()
            && canonical_identity(&participant.identity).is_none()
    }) {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::InvalidIdentity,
        ));
    }
    let visible = visible
        .into_iter()
        .filter(|participant| !participant.identity.trim().is_empty())
        .collect::<Vec<_>>();
    if visible.is_empty() {
        return Ok(RcsMappingOutcome::Refused(
            RcsMappingRefusal::ParticipantAmbiguous,
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
            (RcsMappingClass::SelfChat, own_identity.clone())
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

    let digest = proof_digest(observation, class, &own_identity);
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

fn canonical_identity(identity: &str) -> Option<String> {
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
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"peppy/rcs-mapping-proof/v1\0");
    for value in [
        &observation.google_conversation_id,
        &observation.status,
        &observation.selected_sim_id,
        &observation.identity_region,
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
    digest.update(observation.canonicalizer_revision.to_be_bytes());
    digest.update(observation.mapping_revision.to_be_bytes());
    let mut participants = observation
        .participants
        .iter()
        .filter(|participant| participant.is_visible)
        .filter_map(|participant| {
            canonical_identity(&participant.identity).map(|identity| (participant.is_me, identity))
        })
        .collect::<Vec<_>>();
    participants.sort_unstable();
    for (is_me, identity) in participants {
        digest.update([u8::from(is_me)]);
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
        let current: RcsContext = serde_json::from_str(&current).map_err(|_| Error::Database)?;
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

#[allow(dead_code)] // R2 consumes this validator while its public permit wrapper is being integrated.
pub(crate) fn mapping_matches_observation(
    conn: &Connection,
    mapping: &RcsMapping,
    observation: &RcsMappingObservation,
    now_ms: i64,
) -> Result<bool, Error> {
    if now_ms.saturating_sub(mapping.verified_at_ms).abs() > RCS_MAPPING_FRESHNESS_MS {
        return Ok(false);
    }
    let Some(context) = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [RCS_CONTEXT_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    if observation.google_candidate_count != 1
        || observation.google_conversation_id != mapping.google_conversation_id
        || observation.conversation_type != RcsConversationType::Rcs
        || observation.status != "eligible"
        || observation.send_mode != RcsSendMode::Automatic
        || observation.read_only
        || !observation.default_outgoing
        || context != context_value(observation)
        || observation.link_state != RcsLinkState::Linked
        || mapping.mapping_revision != observation.mapping_revision
        || mapping.verified_at_ms != observation.observed_at_ms
    {
        return Ok(false);
    }
    let own_identity = match canonical_identity(&observation.selected_sim_own_identity) {
        Some(identity) => identity,
        None => return Ok(false),
    };
    if mapping.proof.digest != proof_digest(observation, mapping.class, &own_identity) {
        return Ok(false);
    }
    let (conversations, uncertain) =
        peppy_conversations_for_identity(conn, &mapping.proof.canonical_participant)?;
    Ok(!uncertain && conversations.as_slice() == [mapping.conversation_id])
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct RcsContext {
    link_generation: u64,
    session_generation: u64,
    context_revision: u64,
    identity_region: String,
    own_identity: String,
    sim_id: String,
    canonicalizer_revision: u32,
    link_state: u8,
}

impl RcsContext {
    fn same_content(&self, other: &Self) -> bool {
        self.identity_region == other.identity_region
            && self.own_identity == other.own_identity
            && self.sim_id == other.sim_id
            && self.canonicalizer_revision == other.canonicalizer_revision
            && self.link_state == other.link_state
    }
}

fn context_value(observation: &RcsMappingObservation) -> String {
    serde_json::to_string(&RcsContext {
        link_generation: observation.link_generation,
        session_generation: observation.session_generation,
        context_revision: observation.context_revision,
        identity_region: observation.identity_region.clone(),
        own_identity: canonical_identity(&observation.selected_sim_own_identity)
            .unwrap_or_default(),
        sim_id: observation.selected_sim_id.clone(),
        canonicalizer_revision: observation.canonicalizer_revision,
        link_state: observation.link_state as u8,
    })
    .expect("RCS context is serializable")
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
            .find(|identity| own_identity.as_deref() != Some(identity.as_str()));
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

fn invalidate_all_mappings(conn: &Connection, code: &str) -> Result<(), Error> {
    conn.execute(
        "UPDATE rcs_conversation_mappings SET invalidated_at_ms=verified_at_ms, invalidation_code=? WHERE invalidated_at_ms IS NULL",
        [code],
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
