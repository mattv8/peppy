use crate::{
    CommandId, ConversationId, Error, MessagePayload, Transport,
    conversation_match::canonical_address,
    rcs_group::{self, GroupMappingState, GroupSendAuthority},
    rcs_mapping::{
        self, RCS_MAPPING_FRESHNESS_MS, RcsConversationType, RcsMapping, RcsMappingClass,
        RcsMappingObservation, RcsMappingProof, RcsSendMode,
    },
    rcs_native::{
        LocalRcsTargetSnapshot, NativeRcsCommitProjection, NativeRcsParticipant,
        NativeRcsTargetClass,
    },
};
use peppy_domain::SendRefusalReason;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const CARRIER_ONLY_PROOF_PREFIX: &str = "rcs_carrier_only_proof_v1/";
const INTENTIONAL_UNLINK_KEY: &str = "rcs_intentional_unlink_context_v1";
const RCS_TARGET_IDENTITY_PREFIX: &str = "rcs_target_identity_v1/";
const ADMISSION_DIAGNOSTIC_PREFIX: &str = "rcs_carrier_admission_diagnostic_v1/";

/// A completed, target-scoped local lookup which found no eligible RCS target.
/// This is intentionally a typed host result: unknown, partial, and failed lookups have no
/// representation here and therefore cannot be persisted as carrier authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarrierOnlyProof {
    pub conversation_id: ConversationId,
    pub recipients: Vec<String>,
    pub canonical_recipient: String,
    pub identity_region: String,
    pub canonicalizer_revision: u32,
    pub subscription_id: String,
    pub observed_at_ms: i64,
    pub link_generation: u64,
    pub session_generation: u64,
    pub context_revision: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IntentionalUnlinkContext {
    link_generation: Option<u64>,
    session_generation: Option<u64>,
    context_revision: Option<u64>,
    lifecycle_generation: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredCarrierOnlyProof {
    canonical_recipient: String,
    identity_region: String,
    canonicalizer_revision: u32,
    subscription_id: String,
    observed_at_ms: i64,
    expires_at_ms: i64,
    link_generation: u64,
    session_generation: u64,
    context_revision: u64,
}

pub(crate) enum CarrierAdmission {
    Permitted,
    ShortCodeSms,
    GroupMmsPreM4,
    Blocked,
    InvalidateProof,
    /// A group conversation whose RCS state refuses carrier sends; the reason is the
    /// desktop-facing refusal code.
    GroupRefused(SendRefusalReason),
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
        || rcs_mapping::has_live_mapping(conn, proof.conversation_id)?
        || !rcs_mapping::context_matches_carrier_proof(
            conn,
            proof.link_generation,
            proof.session_generation,
            proof.context_revision,
            &proof.identity_region,
            proof.canonicalizer_revision,
        )?
    {
        return Err(Error::InvalidRequest("carrier-only proof is not current"));
    }
    canonical_recipients(&proof.recipients)?;
    if proof.canonical_recipient.is_empty()
        || (proof.identity_region.is_empty() && !proof.canonical_recipient.starts_with('+'))
    {
        return Err(Error::InvalidRequest("invalid carrier-only recipient"));
    }
    let stored = StoredCarrierOnlyProof {
        canonical_recipient: proof.canonical_recipient.clone(),
        identity_region: proof.identity_region.clone(),
        canonicalizer_revision: proof.canonicalizer_revision,
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

/// Carrier admission for one command. `media` is true when the command carries attachments: a
/// group text is `Mms` transport too, so only attachments decide the group media rule.
#[allow(clippy::too_many_arguments)]
pub(crate) fn carrier_admission(
    conn: &Connection,
    conversation_id: ConversationId,
    recipients: &[String],
    transport: Transport,
    media: bool,
    subscription_id: &str,
    now_ms: i64,
    expected_member_revision: Option<u64>,
) -> Result<CarrierAdmission, Error> {
    if rcs_mapping::has_live_mapping(conn, conversation_id)? {
        return Ok(CarrierAdmission::Blocked);
    }
    if let Some(mapping) = rcs_group::group_mapping(conn, conversation_id)? {
        return match mapping.state {
            GroupMappingState::Withdrawn => {
                Ok(CarrierAdmission::GroupRefused(SendRefusalReason::Withdrawn))
            }
            // Proof in progress: carrier work waits until the mapping settles.
            GroupMappingState::Proving => Ok(CarrierAdmission::Blocked),
            // Group media never leaves carrier for a receiving group in M4 (media over RCS is M5),
            // matching a live 1:1 mapping.
            GroupMappingState::Live | GroupMappingState::Paused if media => {
                Ok(CarrierAdmission::Blocked)
            }
            // A stale client that did not send the revision it saw must reload.
            GroupMappingState::Live | GroupMappingState::Paused
                if expected_member_revision.is_none() =>
            {
                Ok(CarrierAdmission::GroupRefused(
                    SendRefusalReason::UpdateRequired,
                ))
            }
            // The live proved revision goes through the native group RCS permit, never carrier.
            GroupMappingState::Live
                if expected_member_revision == Some(mapping.proved_member_revision) =>
            {
                Ok(CarrierAdmission::Blocked)
            }
            GroupMappingState::Live | GroupMappingState::Paused => Ok(
                CarrierAdmission::GroupRefused(SendRefusalReason::MembersChanged),
            ),
            // An unmapped group keeps the pre-M4 carrier rules.
            GroupMappingState::Unlinked => carrier_admission_unmapped(
                conn,
                conversation_id,
                recipients,
                transport,
                subscription_id,
                now_ms,
            ),
        };
    }
    carrier_admission_unmapped(
        conn,
        conversation_id,
        recipients,
        transport,
        subscription_id,
        now_ms,
    )
}

fn carrier_admission_unmapped(
    conn: &Connection,
    conversation_id: ConversationId,
    recipients: &[String],
    transport: Transport,
    subscription_id: &str,
    now_ms: i64,
) -> Result<CarrierAdmission, Error> {
    let Some(linked) = rcs_mapping::current_link_state(conn)? else {
        return Ok(CarrierAdmission::Permitted);
    };
    if !linked {
        return Ok(
            if !rcs_mapping::has_prior_link(conn)? || intentional_unlink_is_current(conn)? {
                CarrierAdmission::Permitted
            } else {
                CarrierAdmission::Blocked
            },
        );
    }
    let region = crate::rcs_target::resolved_region(conn).map_err(|_| Error::Database)?;
    let current_route = rcs_mapping::carrier_route_is_current(conn, subscription_id, now_ms)?;
    if current_route && transport == Transport::Sms && is_short_code(recipients, region.as_deref())
    {
        record_admission_diagnostic(
            conn,
            conversation_id,
            "rcs_carrier_admission_short_code_sms",
        )?;
        return Ok(CarrierAdmission::ShortCodeSms);
    }
    if current_route
        && transport == Transport::Mms
        && group_recipients_are_provable(recipients, region.as_deref())
        && nonself_recipient_count(conn, recipients, region.as_deref())? > 1
    {
        record_admission_diagnostic(
            conn,
            conversation_id,
            "rcs_carrier_admission_group_mms_pre_m4",
        )?;
        return Ok(CarrierAdmission::GroupMmsPreM4);
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
    Ok(
        if proof.expires_at_ms >= now_ms
            && (0..=RCS_MAPPING_FRESHNESS_MS).contains(&current_age)
            && proof.subscription_id == subscription_id
            && recipients.len() == 1
            && proof.canonical_recipient
                == canonical_address(
                    &recipients[0],
                    (!proof.identity_region.is_empty()).then_some(proof.identity_region.as_str()),
                )
            && rcs_mapping::context_matches_carrier_proof(
                conn,
                proof.link_generation,
                proof.session_generation,
                proof.context_revision,
                &proof.identity_region,
                proof.canonicalizer_revision,
            )?
        {
            CarrierAdmission::Permitted
        } else {
            CarrierAdmission::Blocked
        },
    )
}

fn is_short_code(recipients: &[String], region: Option<&str>) -> bool {
    let [recipient] = recipients else {
        return false;
    };
    crate::rcs_target::is_short_code(recipient, region)
}

/// Without a singleton region, a national spelling cannot be proven equal to another recipient.
/// The group exception is then refused instead of counting raw text as distinct recipients.
fn group_recipients_are_provable(recipients: &[String], region: Option<&str>) -> bool {
    region.is_some()
        || recipients
            .iter()
            .all(|recipient| recipient.trim().starts_with('+'))
}

fn nonself_recipient_count(
    conn: &Connection,
    recipients: &[String],
    region: Option<&str>,
) -> Result<usize, Error> {
    let own_identity = rcs_mapping::confirmed_own_identity(conn)?;
    Ok(recipients
        .iter()
        .filter_map(|recipient| {
            let canonical = canonical_address(recipient, region);
            (!canonical.is_empty() && own_identity.as_deref() != Some(canonical.as_str()))
                .then_some(canonical)
        })
        .collect::<BTreeSet<_>>()
        .len())
}

fn record_admission_diagnostic(
    conn: &Connection,
    conversation_id: ConversationId,
    code: &str,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
        params![
            format!("{ADMISSION_DIAGNOSTIC_PREFIX}{conversation_id}"),
            code
        ],
    )?;
    Ok(())
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
    context: Option<(u64, u64, u64)>,
    lifecycle_generation: u64,
) -> Result<(), Error> {
    let (link_generation, session_generation, context_revision) = context
        .map(|(link, session, revision)| (Some(link), Some(session), Some(revision)))
        .unwrap_or((None, None, None));
    let context = IntentionalUnlinkContext {
        link_generation,
        session_generation,
        context_revision,
        lifecycle_generation: Some(lifecycle_generation),
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

pub(crate) fn intentional_unlink_is_current(conn: &Connection) -> Result<bool, Error> {
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
    let Some(lifecycle) = rcs_mapping::recorded_link_lifecycle_metadata(conn)? else {
        return Ok(false);
    };
    let Some(lifecycle_generation) = context.lifecycle_generation else {
        return Ok(false);
    };
    if lifecycle.state != crate::RcsCapabilityState::Unlinked
        || lifecycle.generation != lifecycle_generation
    {
        return Ok(false);
    }
    match (
        context.link_generation,
        context.session_generation,
        context.context_revision,
    ) {
        (Some(link_generation), Some(session_generation), Some(context_revision)) => {
            rcs_mapping::context_is_current(
                conn,
                link_generation,
                session_generation,
                context_revision,
                false,
            )
        }
        (None, None, None) => Ok(true),
        _ => Ok(false),
    }
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
    command_id: CommandId,
    conversation_id: ConversationId,
    tmp_id: String,
    mapping_revision: u64,
    proof_digest: [u8; 32],
    google_conversation_id: String,
    target: LocalRcsTargetSnapshot,
    message: MessagePayload,
}

impl RcsPermit {
    pub(crate) fn from_admission(
        command_id: CommandId,
        message: MessagePayload,
        tmp_id: String,
        mapping: &RcsMapping,
        observation: &RcsMappingObservation,
    ) -> Result<Self, Error> {
        Ok(Self {
            command_id,
            conversation_id: message.record.conversation_id,
            tmp_id,
            mapping_revision: mapping.mapping_revision,
            proof_digest: mapping.proof.digest,
            google_conversation_id: mapping.google_conversation_id.clone(),
            target: local_target_snapshot(mapping, observation)?,
            message,
        })
    }

    pub fn command_id(&self) -> CommandId {
        self.command_id
    }

    pub fn conversation_id(&self) -> ConversationId {
        self.conversation_id
    }

    pub fn tmp_id(&self) -> &str {
        &self.tmp_id
    }

    pub fn mapping_revision(&self) -> u64 {
        self.mapping_revision
    }

    pub fn proof_digest(&self) -> [u8; 32] {
        self.proof_digest
    }

    pub fn google_conversation_id(&self) -> &str {
        &self.google_conversation_id
    }

    pub fn target(&self) -> &LocalRcsTargetSnapshot {
        &self.target
    }

    pub fn message(&self) -> &MessagePayload {
        &self.message
    }

    /// Returns a detached projection of this immutable permit. Native commit must compare the
    /// projection with its prepared facts before admitting an external effect.
    pub fn native_commit_projection(&self) -> NativeRcsCommitProjection {
        NativeRcsCommitProjection {
            command_id: self.command_id.to_string(),
            tmp_id: self.tmp_id.clone(),
            mapping_revision: self.mapping_revision,
            proof_version: self.target.proof_version,
            proof_digest: self.proof_digest.to_vec(),
            message_body: self.message.body.clone(),
            media: None,
            target: self.target.clone(),
            expected_member_digest: None,
        }
    }
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
    /// Group targets only; absent for 1:1 so their stored identities are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<StoredGroupIdentity>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct StoredGroupIdentity {
    conversation_id: String,
    member_set_digest: String,
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

/// Stores the group target identity for `command_id` in the same form as the 1:1 identity.
pub(crate) fn record_group_target_identity(
    conn: &Connection,
    command_id: CommandId,
    authority: &GroupSendAuthority,
    observation: &RcsMappingObservation,
) -> Result<(), Error> {
    let identity = group_target_identity(authority, observation)?;
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
    if mapping.proof_version != rcs_mapping::RCS_MAPPING_PROOF_VERSION {
        return Err(Error::InvalidRequest("RCS target identity is not current"));
    }
    observed_target_identity(
        &mapping.google_conversation_id,
        &mapping.proof,
        mapping.class == RcsMappingClass::SelfChat,
        None,
        observation,
    )
}

fn group_target_identity(
    authority: &GroupSendAuthority,
    observation: &RcsMappingObservation,
) -> Result<StoredRcsTargetIdentity, Error> {
    let proof = RcsMappingProof {
        canonical_participant: authority.member_set_digest.clone(),
        identity_region: authority.identity_region.clone(),
        own_identity: authority.own_identity.clone(),
        digest: authority.proof_digest,
    };
    observed_target_identity(
        &authority.google_conversation_id,
        &proof,
        false,
        Some(StoredGroupIdentity {
            conversation_id: authority.conversation_id.to_string(),
            member_set_digest: authority.member_set_digest.clone(),
        }),
        observation,
    )
}

fn observed_target_identity(
    google_conversation_id: &str,
    proof: &RcsMappingProof,
    self_chat: bool,
    group: Option<StoredGroupIdentity>,
    observation: &RcsMappingObservation,
) -> Result<StoredRcsTargetIdentity, Error> {
    if observation.google_conversation_id != google_conversation_id
        || observation.conversation_type != RcsConversationType::Rcs
        || observation.send_mode != RcsSendMode::Automatic
        || !observation.default_outgoing
    {
        return Err(Error::InvalidRequest("RCS target identity is not current"));
    }
    Ok(StoredRcsTargetIdentity {
        version: 2,
        google_conversation_id: google_conversation_id.to_owned(),
        canonical_participant: proof.canonical_participant.clone(),
        own_identity: proof.own_identity.clone(),
        identity_region: proof.identity_region.clone(),
        selected_sim_id: observation.selected_sim_id.clone(),
        link_generation: observation.link_generation,
        session_generation: observation.session_generation,
        canonicalizer_revision: observation.canonicalizer_revision,
        self_chat,
        outgoing_id: observation.outgoing.outgoing_id.clone(),
        sim_participant_id: observation.outgoing.sim_participant_id.clone(),
        sim_payload_two: observation.outgoing.sim_payload.two,
        sim_payload_sim_number: observation.outgoing.sim_payload.sim_number,
        canonical_os_own_identity: canonical_e164(&observation.carrier_binding.own_number_raw)?,
        canonical_google_own_identity: canonical_e164(&observation.outgoing.own_identity_raw)?,
        group,
    })
}

fn canonical_e164(identity: &str) -> Result<String, Error> {
    rcs_mapping::canonical_recipient(identity)
        .filter(|identity| identity.starts_with('+'))
        .ok_or(Error::InvalidRequest("RCS own identity must be E.164"))
}

fn local_target_snapshot(
    mapping: &RcsMapping,
    observation: &RcsMappingObservation,
) -> Result<LocalRcsTargetSnapshot, Error> {
    target_identity(mapping, observation)?;
    let class = match mapping.class {
        RcsMappingClass::OneToOne => NativeRcsTargetClass::OneToOne,
        RcsMappingClass::SelfChat => NativeRcsTargetClass::SelfChat,
    };
    Ok(LocalRcsTargetSnapshot {
        proof_version: mapping.proof_version,
        peppy_conversation_id: mapping.conversation_id.to_string(),
        google_conversation_id: mapping.google_conversation_id.clone(),
        conversation_type: observation.conversation_type,
        status: observation.status.clone(),
        send_mode: observation.send_mode,
        read_only: observation.read_only,
        default_outgoing: observation.default_outgoing,
        participants: native_participants(observation),
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

fn native_participants(observation: &RcsMappingObservation) -> Vec<NativeRcsParticipant> {
    observation
        .participants
        .iter()
        .map(|participant| NativeRcsParticipant {
            identity: participant.identity.clone(),
            google_participant_id: participant.google_participant_id.clone(),
            identifier_type: participant.identifier_type,
            is_visible: participant.is_visible,
            is_me: participant.is_me,
        })
        .collect()
}

/// The commit projection for one admitted group send. Its target class is `Group`, and the target
/// identity is the proved conversation and member digest (D1 / C).
pub(crate) fn group_commit_projection(
    command_id: CommandId,
    tmp_id: String,
    authority: &GroupSendAuthority,
    observation: &RcsMappingObservation,
    message_body: String,
) -> Result<NativeRcsCommitProjection, Error> {
    group_target_identity(authority, observation)?;
    Ok(NativeRcsCommitProjection {
        command_id: command_id.to_string(),
        tmp_id,
        mapping_revision: authority.mapping_revision,
        proof_version: authority.proof_version,
        proof_digest: authority.proof_digest.to_vec(),
        message_body,
        media: None,
        target: LocalRcsTargetSnapshot {
            proof_version: authority.proof_version,
            peppy_conversation_id: authority.conversation_id.to_string(),
            google_conversation_id: authority.google_conversation_id.clone(),
            conversation_type: observation.conversation_type,
            status: observation.status.clone(),
            send_mode: observation.send_mode,
            read_only: observation.read_only,
            default_outgoing: observation.default_outgoing,
            participants: native_participants(observation),
            class: NativeRcsTargetClass::Group,
            canonical_recipient: authority.member_set_digest.clone(),
            canonical_own_identity: authority.own_identity.clone(),
            identity_region: authority.identity_region.clone(),
            canonicalizer_revision: observation.canonicalizer_revision,
            selected_sim_id: observation.selected_sim_id.clone(),
            carrier_binding: observation.carrier_binding.clone(),
            outgoing: observation.outgoing.clone(),
            evidence: observation.native_evidence.clone(),
        },
        expected_member_digest: Some(authority.member_set_digest.clone()),
    })
}

fn target_identity_key(command_id: CommandId) -> String {
    format!("{RCS_TARGET_IDENTITY_PREFIX}{command_id}")
}

#[cfg(test)]
mod tests {
    use super::{INTENTIONAL_UNLINK_KEY, IntentionalUnlinkContext, intentional_unlink_is_current};
    use rusqlite::Connection;

    #[test]
    fn legacy_intentional_unlink_context_is_unfenced() {
        let legacy = r#"{"link_generation":1,"session_generation":2,"context_revision":3}"#;
        let context: IntentionalUnlinkContext = serde_json::from_str(legacy).unwrap();
        assert_eq!(context.lifecycle_generation, None);

        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE metadata(k TEXT PRIMARY KEY, v TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO metadata(k,v) VALUES(?,?)",
            [INTENTIONAL_UNLINK_KEY, legacy],
        )
        .unwrap();
        assert!(!intentional_unlink_is_current(&conn).unwrap());
    }
}
