use crate::{
    CommandId, ConversationId, Error, MessagePayload,
    rcs_mapping::{self, RCS_MAPPING_FRESHNESS_MS, RcsConversationType, RcsMapping,
        RcsMappingClass, RcsMappingObservation, RcsSendMode},
    rcs_native::{LocalRcsTargetSnapshot, NativeRcsParticipant, NativeRcsTargetClass},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const CARRIER_ONLY_PROOF_PREFIX: &str = "rcs_carrier_only_proof_v1/";
const INTENTIONAL_UNLINK_KEY: &str = "rcs_intentional_unlink_context_v1";
const RCS_TARGET_IDENTITY_PREFIX: &str = "rcs_target_identity_v1/";

/// A completed, target-scoped local lookup which found no eligible RCS target.
/// This is intentionally a typed host result: unknown, partial, and failed lookups have no
/// representation here and therefore cannot be persisted as carrier authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarrierOnlyProof {
    pub conversation_id: ConversationId,
    pub recipients: Vec<String>,
    pub subscription_id: String,
    pub observed_at_ms: i64,
    pub link_generation: u64,
    pub session_generation: u64,
    pub context_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IntentionalUnlinkContext {
    link_generation: u64,
    session_generation: u64,
    context_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredCarrierOnlyProof {
    recipients: Vec<String>,
    subscription_id: String,
    observed_at_ms: i64,
    expires_at_ms: i64,
    link_generation: u64,
    session_generation: u64,
    context_revision: u64,
}

pub(crate) enum CarrierAdmission {
    Permitted,
    Blocked,
    InvalidateProof,
}

pub(crate) fn record_carrier_only_proof(
    conn: &Connection,
    proof: &CarrierOnlyProof,
    now_ms: i64,
) -> Result<(), Error> {
    now_ms
        .checked_sub(proof.observed_at_ms)
        .filter(|age| (0..=RCS_MAPPING_FRESHNESS_MS).contains(age))
        .ok_or(Error::InvalidRequest("carrier-only proof is not current"))?;
    let expires_at_ms = proof
        .observed_at_ms
        .checked_add(RCS_MAPPING_FRESHNESS_MS)
        .ok_or(Error::InvalidRequest("carrier-only proof is not current"))?;
    if proof.subscription_id.is_empty()
        || !rcs_mapping::context_is_current(
            conn,
            proof.link_generation,
            proof.session_generation,
            proof.context_revision,
            true,
        )?
    {
        return Err(Error::InvalidRequest("carrier-only proof is not current"));
    }
    let recipients = canonical_recipients(&proof.recipients)?;
    let stored = StoredCarrierOnlyProof {
        recipients,
        subscription_id: proof.subscription_id.clone(),
        observed_at_ms: proof.observed_at_ms,
        expires_at_ms,
        link_generation: proof.link_generation,
        session_generation: proof.session_generation,
        context_revision: proof.context_revision,
    };
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![
            proof_key(proof.conversation_id),
            serde_json::to_string(&stored).map_err(|_| Error::Database)?
        ],
    )?;
    Ok(())
}

pub(crate) fn carrier_admission(
    conn: &Connection,
    conversation_id: ConversationId,
    recipients: &[String],
    subscription_id: &str,
    now_ms: i64,
) -> Result<CarrierAdmission, Error> {
    if rcs_mapping::has_proven_mapping(conn, conversation_id)? {
        return Ok(CarrierAdmission::Blocked);
    }
    let Some(linked) = rcs_mapping::current_link_state(conn)? else {
        return Ok(CarrierAdmission::Permitted);
    };
    if !linked {
        return Ok(if recorded_link_state_is_unlinked(conn)? {
            CarrierAdmission::Permitted
        } else {
            CarrierAdmission::Blocked
        });
    }
    let Some(value) = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [proof_key(conversation_id)],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(CarrierAdmission::Blocked);
    };
    let proof: StoredCarrierOnlyProof =
        serde_json::from_str(&value).map_err(|_| Error::Database)?;
    let Some(current_age) = now_ms.checked_sub(proof.observed_at_ms) else {
        return Ok(CarrierAdmission::InvalidateProof);
    };
    if current_age < 0 {
        return Ok(CarrierAdmission::InvalidateProof);
    }
    Ok(if proof.expires_at_ms >= now_ms
        && (0..=RCS_MAPPING_FRESHNESS_MS).contains(&current_age)
        && proof.subscription_id == subscription_id
        && proof.recipients == canonical_recipients(recipients)?
        && rcs_mapping::context_is_current(
            conn,
            proof.link_generation,
            proof.session_generation,
            proof.context_revision,
            true,
        )? {
        CarrierAdmission::Permitted
    } else {
        CarrierAdmission::Blocked
    })
}

pub(crate) fn invalidate_carrier_only_proof(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM metadata WHERE k=?",
        [proof_key(conversation_id)],
    )?;
    Ok(())
}

pub(crate) fn record_intentional_unlink(
    conn: &Connection,
    link_generation: u64,
    session_generation: u64,
    context_revision: u64,
) -> Result<(), Error> {
    let context = IntentionalUnlinkContext {
        link_generation,
        session_generation,
        context_revision,
    };
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![
            INTENTIONAL_UNLINK_KEY,
            serde_json::to_string(&context).map_err(|_| Error::Database)?
        ],
    )?;
    Ok(())
}

fn recorded_link_state_is_unlinked(conn: &Connection) -> Result<bool, Error> {
    let value = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [INTENTIONAL_UNLINK_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(value) = value else {
        return Ok(false);
    };
    let context: IntentionalUnlinkContext =
        serde_json::from_str(&value).map_err(|_| Error::Database)?;
    rcs_mapping::context_is_current(
        conn,
        context.link_generation,
        context.session_generation,
        context.context_revision,
        false,
    )
}

fn proof_key(conversation_id: ConversationId) -> String {
    format!("{CARRIER_ONLY_PROOF_PREFIX}{conversation_id}")
}

fn canonical_recipients(recipients: &[String]) -> Result<Vec<String>, Error> {
    if recipients.len() != 1 {
        return Err(Error::InvalidRequest(
            "carrier-only proof requires one recipient",
        ));
    }
    let recipients = recipients
        .iter()
        .map(|recipient| rcs_mapping::canonical_recipient(recipient))
        .collect::<Option<BTreeSet<_>>>()
        .ok_or(Error::InvalidRequest("invalid carrier-only recipient"))?;
    if recipients.is_empty() {
        return Err(Error::InvalidRequest("missing carrier-only recipient"));
    }
    Ok(recipients.into_iter().collect())
}

/// Local-only authority to submit one exact RCS command to the linked transport.
/// It is deliberately not serializable: Google identifiers and proof material stay local.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsPermit {
    pub command_id: CommandId,
    pub conversation_id: ConversationId,
    pub tmp_id: String,
    pub mapping_revision: u64,
    pub proof_digest: [u8; 32],
    pub google_conversation_id: String,
    /// Restricted local-native transport projection, constructed during core admission.
    pub target: LocalRcsTargetSnapshot,
    pub message: MessagePayload,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct StoredRcsTargetIdentity {
    version: u32,
    google_conversation_id: String,
    canonical_participant: String,
    own_identity: String,
    identity_region: String,
    selected_sim_id: String,
    link_generation: u64,
    session_generation: u64,
    canonicalizer_revision: u32,
    self_chat: bool,
    outgoing_id: String,
    sim_participant_id: String,
    sim_payload_two: i32,
    sim_payload_sim_number: i32,
    canonical_os_own_identity: String,
    canonical_google_own_identity: String,
}

pub(crate) fn record_rcs_target_identity(
    conn: &Connection,
    command_id: CommandId,
    mapping: &RcsMapping,
    observation: &RcsMappingObservation,
) -> Result<(), Error> {
    let identity = target_identity(mapping, observation)?;
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO NOTHING",
        params![
            target_identity_key(command_id),
            serde_json::to_string(&identity).map_err(|_| Error::Database)?
        ],
    )?;
    Ok(())
}

pub(crate) fn has_same_rcs_target_identity(
    conn: &Connection,
    command_id: CommandId,
    mapping: &RcsMapping,
    observation: &RcsMappingObservation,
) -> Result<bool, Error> {
    let Some(value) = conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [target_identity_key(command_id)],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    let Ok(stored) = serde_json::from_str::<StoredRcsTargetIdentity>(&value) else {
        // v1 frozen identities lack concrete outgoing/payload binding and cannot authorize retry.
        return Ok(false);
    };
    Ok(stored == target_identity(mapping, observation)?)
}

fn target_identity(
    mapping: &RcsMapping,
    observation: &RcsMappingObservation,
) -> Result<StoredRcsTargetIdentity, Error> {
    if observation.google_conversation_id != mapping.google_conversation_id
        || observation.conversation_type != RcsConversationType::Rcs
        || observation.send_mode != RcsSendMode::Automatic
        || !observation.default_outgoing
        || mapping.proof_version != rcs_mapping::RCS_MAPPING_PROOF_VERSION
    {
        return Err(Error::InvalidRequest("RCS target identity is not current"));
    }
    Ok(StoredRcsTargetIdentity {
        version: 2,
        google_conversation_id: mapping.google_conversation_id.clone(),
        canonical_participant: mapping.proof.canonical_participant.clone(),
        own_identity: mapping.proof.own_identity.clone(),
        identity_region: mapping.proof.identity_region.clone(),
        selected_sim_id: observation.selected_sim_id.clone(),
        link_generation: observation.link_generation,
        session_generation: observation.session_generation,
        canonicalizer_revision: observation.canonicalizer_revision,
        self_chat: mapping.class == RcsMappingClass::SelfChat,
        outgoing_id: observation.outgoing.outgoing_id.clone(),
        sim_participant_id: observation.outgoing.sim_participant_id.clone(),
        sim_payload_two: observation.outgoing.sim_payload.two,
        sim_payload_sim_number: observation.outgoing.sim_payload.sim_number,
        canonical_os_own_identity: canonical_e164(&observation.carrier_binding.own_number_raw)?,
        canonical_google_own_identity: canonical_e164(&observation.outgoing.own_identity_raw)?,
    })
}

fn canonical_e164(identity: &str) -> Result<String, Error> {
    rcs_mapping::canonical_recipient(identity)
        .filter(|identity| identity.starts_with('+'))
        .ok_or(Error::InvalidRequest("RCS own identity must be E.164"))
}

pub(crate) fn local_target_snapshot(
    mapping: &RcsMapping,
    observation: &RcsMappingObservation,
) -> Result<LocalRcsTargetSnapshot, Error> {
    target_identity(mapping, observation)?;
    let class = match mapping.class {
        RcsMappingClass::OneToOne => NativeRcsTargetClass::OneToOne,
        RcsMappingClass::SelfChat => NativeRcsTargetClass::SelfChat,
    };
    Ok(LocalRcsTargetSnapshot {
        google_conversation_id: mapping.google_conversation_id.clone(),
        participants: observation
            .participants
            .iter()
            .map(|participant| NativeRcsParticipant {
                identity: participant.identity.clone(),
                google_participant_id: participant.google_participant_id.clone(),
                identifier_type: participant.identifier_type,
                is_visible: participant.is_visible,
                is_me: participant.is_me,
            })
            .collect(),
        class,
        canonical_recipient: mapping.proof.canonical_participant.clone(),
        canonical_own_identity: mapping.proof.own_identity.clone(),
        identity_region: mapping.proof.identity_region.clone(),
        canonicalizer_revision: observation.canonicalizer_revision,
        selected_sim_id: observation.selected_sim_id.clone(),
        carrier_binding: observation.carrier_binding.clone(),
        outgoing: observation.outgoing.clone(),
        evidence: observation.native_evidence.clone(),
    })
}

fn target_identity_key(command_id: CommandId) -> String {
    format!("{RCS_TARGET_IDENTITY_PREFIX}{command_id}")
}
