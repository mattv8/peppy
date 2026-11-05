use crate::{ConversationId, Error};
use phonenumber::{Mode, parse};
use rusqlite::{Connection, OptionalExtension, params};
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
    pub canonicalizer_revision: u32,
    pub mapping_revision: u64,
    pub link_state: RcsLinkState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsMappingRefusal {
    StaleObservation,
    LinkUnavailable,
    IneligibleConversation,
    InvalidIdentity,
    ParticipantAmbiguous,
    SelfChatMismatch,
    PeppyConversationMissing,
    PeppyConversationAmbiguous,
    MappingConflict,
    NonMonotonicRevision,
}

impl RcsMappingRefusal {
    pub const fn code(self) -> &'static str {
        match self {
            Self::StaleObservation => "stale_observation",
            Self::LinkUnavailable => "link_unavailable",
            Self::IneligibleConversation => "ineligible_conversation",
            Self::InvalidIdentity => "invalid_identity",
            Self::ParticipantAmbiguous => "participant_ambiguous",
            Self::SelfChatMismatch => "self_chat_mismatch",
            Self::PeppyConversationMissing => "peppy_conversation_missing",
            Self::PeppyConversationAmbiguous => "peppy_conversation_ambiguous",
            Self::MappingConflict => "mapping_conflict",
            Self::NonMonotonicRevision => "nonmonotonic_revision",
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
    let outcome = verify(conn, observation, now_ms)?;
    let RcsMappingOutcome::Mapped(mapping) = &outcome else {
        invalidate_google_mapping(
            conn,
            &observation.google_conversation_id,
            refusal_code(&outcome),
        )?;
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

    let conversations = peppy_conversations_for_identity(conn, &participant)?;
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
) -> Result<Vec<ConversationId>, Error> {
    let address = identity.strip_prefix("raw:").unwrap_or(identity);
    let mut statement = conn.prepare(
        "SELECT DISTINCT conversation_id FROM addresses WHERE address=? ORDER BY conversation_id",
    )?;
    statement
        .query_map([address], |row| row.get::<_, String>(0))?
        .map(|row| crate::parse(&row?).map_err(|_| Error::Database))
        .collect()
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

fn invalidate_google_mapping(
    conn: &Connection,
    google_conversation_id: &str,
    code: &str,
) -> Result<(), Error> {
    if google_conversation_id.is_empty() {
        return Ok(());
    }
    conn.execute(
        "UPDATE rcs_conversation_mappings SET invalidated_at_ms=verified_at_ms, invalidation_code=? WHERE google_conversation_id=? AND invalidated_at_ms IS NULL",
        params![code, google_conversation_id],
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
