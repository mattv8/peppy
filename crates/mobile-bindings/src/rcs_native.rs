use crate::{MobileBindingsError, NativeClient, NativePermitState, with_client};
use peppy_client_core::{
    GroupCreateBegin, GroupCreateOutcome, GroupCreateState, NativeRcsCommitProjection,
    NativeRcsPreparation, NativeRcsPreparationTarget, NativeRcsTargetClass,
    RCS_CANONICALIZER_REVISION, RcsBootstrapResolution, RcsMappingClass, RcsMappingObservation,
    RcsMappingOutcome, RcsMappingRefusal, RcsTargetDecision, SendRefusalReason,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

const RCS_NATIVE_JSON_CAP: usize = 65_536;
const RCS_NATIVE_SNAPSHOT_CAP: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsContextTuple {
    pub link_generation: u64,
    pub session_generation: u64,
    pub context_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRcsMappingVerification {
    Mapped,
    GoogleConversationAmbiguous,
    Refused,
}

/// Outcome of re-proving a live group against one fresh target observation.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRcsGroupVerification {
    Proven,
    MembersChanged,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRcsBootstrapResolution {
    Resolved { conversation_id: String },
    Missing,
    Ambiguous,
    Uncertain,
    InvalidIdentity,
    AlreadyMappedElsewhere,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum NativeRcsTargetDecision {
    Found {
        google_conversation_id: String,
        snapshot_token: String,
    },
    Ambiguous,
    Unknown,
    Unsupported,
    NotFoundComplete,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeRcsEligibilityPublication {
    pub conversation_id: String,
}

#[uniffi::export]
pub fn gateway_publish_rcs_eligibility(
    client: Arc<NativeClient>,
    publications: Vec<NativeRcsEligibilityPublication>,
    now_ms: i64,
) -> Result<u32, MobileBindingsError> {
    with_client(&client.client, |core| {
        let conversation_ids = publications
            .into_iter()
            .map(|value| {
                value
                    .conversation_id
                    .parse()
                    .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid conversation"))
            })
            .collect::<Result<Vec<_>, peppy_client_core::Error>>()?;
        core.publish_rcs_eligibility_from_lifecycle(&conversation_ids, now_ms)
    })
    .and_then(|published| u32::try_from(published).map_err(|_| MobileBindingsError::InvalidRequest))
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativePendingRcsCommand {
    pub command_id: String,
    pub conversation_id: String,
    pub recipient: String,
}

#[uniffi::export]
pub fn gateway_pending_native_rcs_commands(
    client: Arc<NativeClient>,
) -> Result<Vec<NativePendingRcsCommand>, MobileBindingsError> {
    with_client(&client.client, |core| core.pending_native_rcs_commands()).map(|commands| {
        commands
            .into_iter()
            .map(|command| NativePendingRcsCommand {
                command_id: command.command_id.to_string(),
                conversation_id: command.conversation_id.to_string(),
                recipient: command.recipient,
            })
            .collect()
    })
}

/// Sanitized received RCS media commands eligible for the gateway upload coordinator. The command
/// ID is opaque; media references and plaintext paths remain core-local.
#[uniffi::export]
pub fn gateway_pending_rcs_media_commands(
    client: Arc<NativeClient>,
    now_ms: i64,
) -> Result<Vec<NativePendingRcsCommand>, MobileBindingsError> {
    with_client(&client.client, |core| {
        core.pending_rcs_media_commands(now_ms)
    })
    .map(|commands| {
        commands
            .into_iter()
            .map(|command| NativePendingRcsCommand {
                command_id: command.command_id.to_string(),
                conversation_id: command.conversation_id.to_string(),
                recipient: command.recipient,
            })
            .collect()
    })
}

#[derive(uniffi::Object)]
/// A native-only, single-use preparation. It is not a transport permit.
pub struct NativeRcsSendPreparation {
    owner: Arc<NativeClient>,
    preparation: Mutex<Option<NativeRcsPreparation>>,
}

#[derive(uniffi::Object)]
/// Core-issued bytes for one already-admitted attempt.
///
/// Reading these bytes after disposal does not authorize a current effect; a
/// trusted native consumer forwards the bytes unchanged exactly once.
pub struct NativeRcsCommittedSend {
    commit_json: String,
    command_id: String,
    tmp_id: String,
    google_conversation_id: String,
}

#[derive(uniffi::Record)]
/// Advisory attachment metadata for the native upload coordinator. No plaintext path or Google
/// reference crosses this boundary.
pub struct NativeRcsMediaAttachment {
    pub command_id: String,
    pub attachment_index: u32,
    pub attachment_id: String,
    pub mime: String,
    pub name: String,
    pub size: u64,
    pub upload_required: bool,
}

#[derive(Serialize)]
struct NativeRcsStoredProof<'a> {
    canonical_participant: &'a str,
    identity_region: &'a str,
    own_identity: &'a str,
    digest: &'a [u8; 32],
}

#[derive(Serialize)]
struct NativeRcsStoredMapping<'a> {
    conversation_id: String,
    google_conversation_id: &'a str,
    class: NativeRcsTargetClass,
    proof_version: u32,
    proof: NativeRcsStoredProof<'a>,
    mapping_revision: u64,
    verified_at_ms: i64,
}

#[derive(Serialize)]
struct NativeRcsFacts<'a> {
    request: &'a peppy_client_core::NativeRcsPrepareRequest,
    stored_mapping: NativeRcsStoredMapping<'a>,
}

fn facts_json(preparation: &NativeRcsPreparation) -> Result<String, MobileBindingsError> {
    let stored_mapping = match preparation.target() {
        NativeRcsPreparationTarget::OneToOne(mapping) => NativeRcsStoredMapping {
            conversation_id: mapping.conversation_id.to_string(),
            google_conversation_id: &mapping.google_conversation_id,
            class: match mapping.class {
                RcsMappingClass::OneToOne => NativeRcsTargetClass::OneToOne,
                RcsMappingClass::SelfChat => NativeRcsTargetClass::SelfChat,
            },
            proof_version: mapping.proof_version,
            proof: NativeRcsStoredProof {
                canonical_participant: &mapping.proof.canonical_participant,
                identity_region: &mapping.proof.identity_region,
                own_identity: &mapping.proof.own_identity,
                digest: &mapping.proof.digest,
            },
            mapping_revision: mapping.mapping_revision,
            verified_at_ms: mapping.verified_at_ms,
        },
        NativeRcsPreparationTarget::Group(authority) => NativeRcsStoredMapping {
            conversation_id: authority.conversation_id.to_string(),
            google_conversation_id: &authority.google_conversation_id,
            class: NativeRcsTargetClass::Group,
            proof_version: authority.proof_version,
            proof: NativeRcsStoredProof {
                canonical_participant: &authority.member_set_digest,
                identity_region: &authority.identity_region,
                own_identity: &authority.own_identity,
                digest: &authority.proof_digest,
            },
            mapping_revision: authority.mapping_revision,
            verified_at_ms: authority.verified_at_ms,
        },
    };
    serde_json::to_string(&NativeRcsFacts {
        request: preparation.request(),
        stored_mapping,
    })
    .map_err(|_| MobileBindingsError::InvalidRequest)
}

fn bounded_facts_json(preparation: &NativeRcsPreparation) -> Result<String, MobileBindingsError> {
    let facts = facts_json(preparation)?;
    (facts.len() <= RCS_NATIVE_JSON_CAP)
        .then_some(facts)
        .ok_or(MobileBindingsError::InvalidRequest)
}

#[uniffi::export]
/// Captures core-owned stored mapping facts for a trusted native observation.
pub fn gateway_prepare_native_rcs_send(
    client: Arc<NativeClient>,
    command_id: String,
    now_ms: i64,
) -> Result<Arc<NativeRcsSendPreparation>, MobileBindingsError> {
    let preparation = with_client(&client.client, |core| {
        let command_id = command_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid command id"))?;
        core.prepare_native_rcs_send(command_id, now_ms)
    })?;
    bounded_facts_json(&preparation)?;
    Ok(Arc::new(NativeRcsSendPreparation {
        owner: client,
        preparation: Mutex::new(Some(preparation)),
    }))
}

#[uniffi::export]
pub fn gateway_next_rcs_media_attachment(
    client: Arc<NativeClient>,
    command_id: String,
    now_ms: i64,
) -> Result<Option<NativeRcsMediaAttachment>, MobileBindingsError> {
    with_client(&client.client, |core| {
        let command_id = command_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid command id"))?;
        core.next_rcs_media_attachment(command_id, now_ms)
    })
    .map(|attachment| {
        attachment.map(|attachment| NativeRcsMediaAttachment {
            command_id: attachment.command_id.to_string(),
            attachment_index: attachment.attachment_index,
            attachment_id: attachment.attachment_id.to_string(),
            mime: attachment.mime,
            name: attachment.name,
            size: attachment.size,
            upload_required: attachment.upload_required,
        })
    })
}

#[uniffi::export]
pub fn gateway_mark_rcs_media_uploading(
    client: Arc<NativeClient>,
    command_id: String,
    attachment_index: u32,
) -> Result<(), MobileBindingsError> {
    with_client(&client.client, |core| {
        core.mark_rcs_media_uploading(
            command_id
                .parse()
                .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid command id"))?,
            attachment_index,
        )
    })
}

#[uniffi::export]
pub fn gateway_mark_rcs_media_reference_ready(
    client: Arc<NativeClient>,
    command_id: String,
    attachment_index: u32,
    media_ref: String,
    uploaded_at_ms: i64,
) -> Result<(), MobileBindingsError> {
    with_client(&client.client, |core| {
        core.mark_rcs_media_reference_ready(
            command_id
                .parse()
                .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid command id"))?,
            attachment_index,
            &media_ref,
            uploaded_at_ms,
        )
    })
}

#[uniffi::export]
pub fn gateway_issue_rcs_media_permit(
    client: Arc<NativeClient>,
    command_id: String,
    attachment_index: u32,
    now_ms: i64,
) -> Result<Arc<NativeRcsSendPreparation>, MobileBindingsError> {
    let preparation = with_client(&client.client, |core| {
        core.prepare_native_rcs_media_send(
            command_id
                .parse()
                .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid command id"))?,
            attachment_index,
            now_ms,
        )
    })?;
    bounded_facts_json(&preparation)?;
    Ok(Arc::new(NativeRcsSendPreparation {
        owner: client,
        preparation: Mutex::new(Some(preparation)),
    }))
}

#[uniffi::export]
pub fn gateway_record_rcs_media_commit(
    client: Arc<NativeClient>,
    command_id: String,
    attachment_index: u32,
) -> Result<(), MobileBindingsError> {
    with_client(&client.client, |core| {
        let command_id = command_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid command id"))?;
        core.record_rcs_media_commit(command_id, attachment_index)
    })
}

#[uniffi::export]
pub fn gateway_record_rcs_media_outcome(
    client: Arc<NativeClient>,
    command_id: String,
    attachment_index: u32,
    code: String,
    _at_ms: i64,
) -> Result<(), MobileBindingsError> {
    with_client(&client.client, |core| {
        core.record_rcs_media_outcome(
            command_id
                .parse()
                .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid command id"))?,
            attachment_index,
            &code,
        )
    })
}

#[uniffi::export]
impl NativeRcsSendPreparation {
    pub fn facts_json(&self) -> Result<String, MobileBindingsError> {
        with_client(&self.owner.client, |_| {
            let preparation = self
                .preparation
                .lock()
                .map_err(|_| peppy_client_core::Error::Database)?;
            let preparation = preparation
                .as_ref()
                .ok_or(peppy_client_core::Error::Conflict)?;
            facts_json(preparation).map_err(|_| peppy_client_core::Error::InvalidRequest("facts"))
        })
        .and_then(|facts| {
            (facts.len() <= RCS_NATIVE_JSON_CAP)
                .then_some(facts)
                .ok_or(MobileBindingsError::InvalidRequest)
        })
    }

    /// The group member digest for the Go prepare request; `None` for a 1:1 target, which must omit it.
    pub fn expected_member_digest(&self) -> Option<String> {
        self.preparation
            .lock()
            .ok()?
            .as_ref()?
            .expected_member_digest()
            .map(str::to_owned)
    }

    pub fn begin(
        &self,
        observation_json: String,
        now_ms: i64,
    ) -> Result<Arc<NativeRcsCommittedSend>, MobileBindingsError> {
        with_client(&self.owner.client, |core| {
            self.begin_admitted(core, observation_json, now_ms)
        })
    }
}

impl NativeRcsSendPreparation {
    fn begin_admitted(
        &self,
        core: &peppy_client_core::Client,
        observation_json: String,
        now_ms: i64,
    ) -> Result<Arc<NativeRcsCommittedSend>, peppy_client_core::Error> {
        let preparation = self
            .preparation
            .lock()
            .map_err(|_| peppy_client_core::Error::Database)?
            .take()
            .ok_or(peppy_client_core::Error::Conflict)?;
        if observation_json.len() > RCS_NATIVE_JSON_CAP {
            return Err(peppy_client_core::Error::InvalidRequest(
                "observation too large",
            ));
        }
        let observation: RcsMappingObservation = serde_json::from_str(&observation_json)
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid observation"))?;
        let projection = core.begin_native_rcs_send(preparation, &observation, now_ms)?;
        serialize_commit(projection)
    }
}

fn serialize_commit(
    projection: NativeRcsCommitProjection,
) -> Result<Arc<NativeRcsCommittedSend>, peppy_client_core::Error> {
    let commit_json =
        serde_json::to_string(&projection).map_err(|_| peppy_client_core::Error::Database)?;
    Ok(Arc::new(NativeRcsCommittedSend {
        commit_json,
        command_id: projection.command_id,
        tmp_id: projection.tmp_id,
        google_conversation_id: projection.target.google_conversation_id,
    }))
}

#[uniffi::export]
impl NativeRcsCommittedSend {
    pub fn commit_json(&self) -> String {
        self.commit_json.clone()
    }

    pub fn command_id(&self) -> String {
        self.command_id.clone()
    }

    pub fn tmp_id(&self) -> String {
        self.tmp_id.clone()
    }

    pub fn google_conversation_id(&self) -> String {
        self.google_conversation_id.clone()
    }
}

#[uniffi::export]
pub fn rcs_canonicalizer_revision() -> u32 {
    RCS_CANONICALIZER_REVISION
}

#[uniffi::export]
pub fn gateway_next_rcs_context_tuple(
    client: Arc<NativeClient>,
    runtime_id: String,
    run_generation: u64,
    stream_epoch: u64,
    pairing_evidence_id: String,
) -> Result<NativeRcsContextTuple, MobileBindingsError> {
    with_client(&client.client, |core| {
        core.next_rcs_context_tuple(
            &runtime_id,
            run_generation,
            stream_epoch,
            &pairing_evidence_id,
        )
    })
    .map(|tuple| NativeRcsContextTuple {
        link_generation: tuple.link_generation,
        session_generation: tuple.session_generation,
        context_revision: tuple.context_revision,
    })
}

#[uniffi::export]
pub fn gateway_next_rcs_mapping_revision(
    client: Arc<NativeClient>,
    conversation_id: String,
) -> Result<u64, MobileBindingsError> {
    with_client(&client.client, |core| {
        let conversation_id = conversation_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid conversation id"))?;
        core.next_rcs_mapping_revision(conversation_id)
    })
}

#[uniffi::export]
pub fn gateway_verify_rcs_mapping(
    client: Arc<NativeClient>,
    observation_json: String,
    now_ms: i64,
) -> Result<NativeRcsMappingVerification, MobileBindingsError> {
    with_client(&client.client, |core| {
        if observation_json.len() > RCS_NATIVE_JSON_CAP {
            return Err(peppy_client_core::Error::InvalidRequest(
                "observation too large",
            ));
        }
        let observation = serde_json::from_str::<RcsMappingObservation>(&observation_json)
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid observation"))?;
        core.verify_rcs_mapping(&observation, now_ms)
    })
    .map(|outcome| match outcome {
        RcsMappingOutcome::Mapped(_) => NativeRcsMappingVerification::Mapped,
        RcsMappingOutcome::Refused(RcsMappingRefusal::GoogleConversationAmbiguous) => {
            NativeRcsMappingVerification::GoogleConversationAmbiguous
        }
        RcsMappingOutcome::Refused(_) => NativeRcsMappingVerification::Refused,
    })
}

#[uniffi::export]
/// Re-proves a live group against the fresh target observation the send will use. Call it where
/// the 1:1 target verification runs for a group ticket, before preparing the send.
pub fn gateway_verify_rcs_group_observation(
    client: Arc<NativeClient>,
    observation_json: String,
    now_ms: i64,
) -> Result<NativeRcsGroupVerification, MobileBindingsError> {
    with_client(&client.client, |core| {
        if observation_json.len() > RCS_NATIVE_JSON_CAP {
            return Err(peppy_client_core::Error::InvalidRequest(
                "observation too large",
            ));
        }
        let observation = serde_json::from_str::<RcsMappingObservation>(&observation_json)
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid observation"))?;
        core.verify_rcs_group_observation(&observation, now_ms)
    })
    .map(|outcome| match outcome {
        peppy_client_core::GroupProofOutcome::Proven => NativeRcsGroupVerification::Proven,
        peppy_client_core::GroupProofOutcome::MembersChanged => {
            NativeRcsGroupVerification::MembersChanged
        }
        peppy_client_core::GroupProofOutcome::Refused => NativeRcsGroupVerification::Refused,
    })
}

#[uniffi::export]
/// Advances the link context from a group's route observation, as the 1:1 route step does. `false`
/// means the context is not current; the group must not be proved or sent on that route.
pub fn gateway_verify_rcs_group_route(
    client: Arc<NativeClient>,
    observation_json: String,
    now_ms: i64,
) -> Result<bool, MobileBindingsError> {
    with_client(&client.client, |core| {
        if observation_json.len() > RCS_NATIVE_JSON_CAP {
            return Err(peppy_client_core::Error::InvalidRequest(
                "observation too large",
            ));
        }
        let observation = serde_json::from_str::<RcsMappingObservation>(&observation_json)
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid observation"))?;
        core.verify_rcs_group_route(&observation, now_ms)
    })
}

#[uniffi::export]
/// The stored member digest of a Live group for the advisory Go prepare request; `None` otherwise.
/// It carries no freshness and authorizes nothing by itself.
pub fn gateway_proved_group_member_digest(
    client: Arc<NativeClient>,
    conversation_id: String,
) -> Result<Option<String>, MobileBindingsError> {
    with_client(&client.client, |core| {
        let conversation_id = conversation_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid conversation id"))?;
        core.proved_group_member_digest(conversation_id)
    })
}

/// Native permit state for a group refusal. Only `CreateNotRcs` leaves the message sent.
pub(crate) fn group_refusal_state(reason: SendRefusalReason) -> NativePermitState {
    match reason {
        SendRefusalReason::MembersChanged => NativePermitState::GroupMembersChanged,
        SendRefusalReason::UpdateRequired => NativePermitState::GroupUpdateRequired,
        SendRefusalReason::Withdrawn => NativePermitState::GroupWithdrawn,
        SendRefusalReason::CreateUnconfirmed => NativePermitState::GroupCreateUnconfirmed,
        SendRefusalReason::CreateNotRcs => NativePermitState::GroupCreateNotRcs,
    }
}

/// Outcome of asking for the one-use create permit of a group send that waits on its create.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum NativeGroupCreateBegin {
    /// Run exactly one create with these canonical other members and this name, then commit it.
    Permit {
        command_id: String,
        numbers: Vec<String>,
        name: Option<String>,
    },
    /// No create was issued. `state` is the send's definite refusal.
    Refused { state: NativePermitState },
}

/// Google's create answer, recorded once per permit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeGroupCreateOutcome {
    Pending,
    NotRcs,
    Unknown,
}

/// Create state of a conversation's group, for native status.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum NativeGroupCreateState {
    NotRequested,
    Begun,
    Pending,
    Unknown,
    NotRcs,
    Refused { state: NativePermitState },
    Linked,
}

/// Counts from one group candidate snapshot. Google identifiers never cross this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct NativeGroupClassification {
    pub linked: u64,
    pub created: u64,
    pub live: u64,
    pub members_changed: u64,
    pub paused: u64,
    pub withdrawn: u64,
    pub not_provable: u64,
    pub dormant: u64,
    pub conflicts: u64,
}

/// Applies one complete Google group candidate snapshot and reports counts only.
#[uniffi::export]
pub fn gateway_ingest_group_candidates(
    client: Arc<NativeClient>,
    snapshot_json: String,
    now_ms: i64,
) -> Result<NativeGroupClassification, MobileBindingsError> {
    with_client(&client.client, |core| {
        if snapshot_json.len() > RCS_NATIVE_SNAPSHOT_CAP {
            return Err(peppy_client_core::Error::InvalidRequest(
                "group snapshot too large",
            ));
        }
        core.ingest_group_candidates(&snapshot_json, now_ms)
    })
    .map(|report| NativeGroupClassification {
        linked: report.linked as u64,
        created: report.created as u64,
        live: report.live as u64,
        members_changed: report.members_changed as u64,
        paused: report.paused as u64,
        withdrawn: report.withdrawn as u64,
        not_provable: report.not_provable as u64,
        dormant: report.dormant as u64,
        conflicts: report.conflicts as u64,
    })
}

/// Issues the one-use create permit for a group send that is waiting on a create.
#[uniffi::export]
pub fn gateway_begin_rcs_group_create(
    client: Arc<NativeClient>,
    conversation_id: String,
    command_id: String,
    now_ms: i64,
) -> Result<NativeGroupCreateBegin, MobileBindingsError> {
    with_client(&client.client, |core| {
        let conversation_id = conversation_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid conversation id"))?;
        let command_id = command_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid command id"))?;
        core.begin_rcs_group_create(conversation_id, command_id, now_ms)
    })
    .map(|begin| match begin {
        GroupCreateBegin::Permit(permit) => NativeGroupCreateBegin::Permit {
            command_id: permit.command_id.to_string(),
            numbers: permit.numbers,
            name: permit.name,
        },
        GroupCreateBegin::Refused(reason) => NativeGroupCreateBegin::Refused {
            state: group_refusal_state(reason),
        },
    })
}

/// Records Google's create answer for a begun permit. Anything unrecognised is `Unknown`.
#[uniffi::export]
pub fn gateway_commit_rcs_group_create(
    client: Arc<NativeClient>,
    command_id: String,
    result_json: String,
) -> Result<NativeGroupCreateOutcome, MobileBindingsError> {
    with_client(&client.client, |core| {
        if result_json.len() > RCS_NATIVE_JSON_CAP {
            return Err(peppy_client_core::Error::InvalidRequest(
                "create result too large",
            ));
        }
        let command_id = command_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid command id"))?;
        core.commit_rcs_group_create(command_id, &result_json)
    })
    .map(|outcome| match outcome {
        GroupCreateOutcome::Pending => NativeGroupCreateOutcome::Pending,
        GroupCreateOutcome::NotRcs => NativeGroupCreateOutcome::NotRcs,
        GroupCreateOutcome::Unknown => NativeGroupCreateOutcome::Unknown,
    })
}

/// Settles a create whose Google answer could not be committed: persists unknown without calling
/// Google. Only a command retained by this process is settled.
#[uniffi::export]
pub fn gateway_settle_rcs_group_create(
    client: Arc<NativeClient>,
    command_id: String,
) -> Result<(), MobileBindingsError> {
    with_client(&client.client, |core| {
        let command_id = command_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid command id"))?;
        core.settle_rcs_group_create(command_id)
    })
}

/// The create state of a conversation's group.
#[uniffi::export]
pub fn gateway_group_create_state(
    client: Arc<NativeClient>,
    conversation_id: String,
) -> Result<NativeGroupCreateState, MobileBindingsError> {
    with_client(&client.client, |core| {
        let conversation_id = conversation_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid conversation id"))?;
        core.group_create_state(conversation_id)
    })
    .map(|state| match state {
        GroupCreateState::None => NativeGroupCreateState::NotRequested,
        GroupCreateState::Begun => NativeGroupCreateState::Begun,
        GroupCreateState::Pending => NativeGroupCreateState::Pending,
        GroupCreateState::Unknown => NativeGroupCreateState::Unknown,
        GroupCreateState::NotRcs => NativeGroupCreateState::NotRcs,
        GroupCreateState::Refused(reason) => NativeGroupCreateState::Refused {
            state: group_refusal_state(reason),
        },
        GroupCreateState::Linked => NativeGroupCreateState::Linked,
    })
}

/// The proved member revision while the group is Live; `None` otherwise. Decimal string, as all
/// revisions cross the boundary.
#[uniffi::export]
pub fn gateway_proved_group_member_revision(
    client: Arc<NativeClient>,
    conversation_id: String,
) -> Result<Option<String>, MobileBindingsError> {
    with_client(&client.client, |core| {
        let conversation_id = conversation_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid conversation id"))?;
        core.proved_group_member_revision(conversation_id)
    })
    .map(|revision| revision.map(|revision| revision.to_string()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BootstrapCandidateJson {
    participant_identity: String,
    google_conversation_id: String,
}

/// Resolves a JSON array of `{participantIdentity, googleConversationId}` candidates in input
/// order. The core rejects more than 40 candidates.
#[uniffi::export]
pub fn gateway_resolve_rcs_bootstrap_conversations(
    client: Arc<NativeClient>,
    candidates_json: String,
) -> Result<Vec<NativeRcsBootstrapResolution>, MobileBindingsError> {
    with_client(&client.client, |core| {
        if candidates_json.len() > RCS_NATIVE_JSON_CAP {
            return Err(peppy_client_core::Error::InvalidRequest(
                "bootstrap candidates too large",
            ));
        }
        let candidates = serde_json::from_str::<Vec<BootstrapCandidateJson>>(&candidates_json)
            .map_err(|_| {
                peppy_client_core::Error::InvalidRequest("invalid bootstrap candidates")
            })?;
        let pairs = candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.participant_identity.as_str(),
                    candidate.google_conversation_id.as_str(),
                )
            })
            .collect::<Vec<_>>();
        core.resolve_rcs_bootstrap_conversations(&pairs)
    })
    .map(|resolutions| {
        resolutions
            .into_iter()
            .map(native_bootstrap_resolution)
            .collect()
    })
}

fn native_bootstrap_resolution(resolution: RcsBootstrapResolution) -> NativeRcsBootstrapResolution {
    match resolution {
        RcsBootstrapResolution::Resolved { conversation_id } => {
            NativeRcsBootstrapResolution::Resolved {
                conversation_id: conversation_id.to_string(),
            }
        }
        RcsBootstrapResolution::Missing => NativeRcsBootstrapResolution::Missing,
        RcsBootstrapResolution::Ambiguous => NativeRcsBootstrapResolution::Ambiguous,
        RcsBootstrapResolution::Uncertain => NativeRcsBootstrapResolution::Uncertain,
        RcsBootstrapResolution::InvalidIdentity => NativeRcsBootstrapResolution::InvalidIdentity,
        RcsBootstrapResolution::AlreadyMappedElsewhere => {
            NativeRcsBootstrapResolution::AlreadyMappedElsewhere
        }
    }
}

#[uniffi::export]
pub fn gateway_classify_rcs_target(
    client: Arc<NativeClient>,
    conversation_id: String,
    recipient: String,
    snapshot_json: String,
    now_ms: i64,
) -> Result<NativeRcsTargetDecision, MobileBindingsError> {
    with_client(&client.client, |core| {
        if snapshot_json.len() > RCS_NATIVE_SNAPSHOT_CAP {
            return Err(peppy_client_core::Error::InvalidRequest(
                "snapshot too large",
            ));
        }
        let conversation_id = conversation_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid conversation id"))?;
        core.classify_and_commit_rcs_target(conversation_id, &recipient, &snapshot_json, now_ms)
    })
    .map(|decision| match decision {
        RcsTargetDecision::Found {
            google_conversation_id,
            snapshot_token,
        } => NativeRcsTargetDecision::Found {
            google_conversation_id,
            snapshot_token,
        },
        RcsTargetDecision::Ambiguous { .. } => NativeRcsTargetDecision::Ambiguous,
        RcsTargetDecision::Unknown { .. } => NativeRcsTargetDecision::Unknown,
        RcsTargetDecision::Unsupported { .. } => NativeRcsTargetDecision::Unsupported,
        RcsTargetDecision::NotFoundComplete { .. } => NativeRcsTargetDecision::NotFoundComplete,
    })
}

#[uniffi::export]
pub fn gateway_record_rcs_transport_ack(
    client: Arc<NativeClient>,
    command_id: String,
    ack_at_ms: i64,
) -> Result<(), MobileBindingsError> {
    with_client(&client.client, |core| {
        let command_id = command_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid command id"))?;
        core.record_rcs_transport_ack(command_id, ack_at_ms, "accepted")
    })
}

#[uniffi::export]
pub fn gateway_record_rcs_submitted(
    client: Arc<NativeClient>,
    command_id: String,
) -> Result<(), MobileBindingsError> {
    with_client(&client.client, |core| {
        let command_id = command_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid command id"))?;
        core.record_rcs_submitted(command_id)
    })
    .map(|_| ())
}

#[uniffi::export]
pub fn gateway_record_rcs_pre_effect_refusal(
    client: Arc<NativeClient>,
    command_id: String,
    tmp_id: String,
    google_conversation_id: String,
    members_changed: bool,
    now_ms: i64,
) -> Result<(), MobileBindingsError> {
    with_client(&client.client, |core| {
        let command_id = command_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid command id"))?;
        core.record_native_rcs_pre_effect_refusal(
            command_id,
            &tmp_id,
            &google_conversation_id,
            members_changed,
            now_ms,
        )
    })
    .map(|_| ())
}

#[uniffi::export]
/// Records uncertainty for the exact admitted native RCS attempt.
pub fn gateway_record_rcs_outcome_unknown(
    client: Arc<NativeClient>,
    command_id: String,
    tmp_id: String,
    google_conversation_id: String,
) -> Result<(), MobileBindingsError> {
    with_client(&client.client, |core| {
        let command_id = command_id
            .parse()
            .map_err(|_| peppy_client_core::Error::InvalidRequest("invalid command id"))?;
        core.record_native_rcs_outcome_unknown(command_id, &tmp_id, &google_conversation_id)
    })
    .map(|_| ())
}

#[uniffi::export]
/// Records a host-confirmed, durable owner unlink without accepting mapping observations.
pub fn gateway_record_rcs_explicit_unlink_completed(
    client: Arc<NativeClient>,
    observed_at_ms: i64,
    now_ms: i64,
) -> Result<(), MobileBindingsError> {
    with_client(&client.client, |core| {
        core.record_rcs_explicit_unlink_completed(observed_at_ms, now_ms)
    })
}

#[cfg(test)]
#[path = "rcs_native_tests.rs"]
mod rcs_native_tests;

#[cfg(test)]
mod group_create_binding_tests {
    use super::*;

    #[test]
    fn every_group_refusal_maps_to_its_own_native_state() {
        assert_eq!(
            group_refusal_state(SendRefusalReason::MembersChanged),
            NativePermitState::GroupMembersChanged
        );
        assert_eq!(
            group_refusal_state(SendRefusalReason::UpdateRequired),
            NativePermitState::GroupUpdateRequired
        );
        assert_eq!(
            group_refusal_state(SendRefusalReason::Withdrawn),
            NativePermitState::GroupWithdrawn
        );
        assert_eq!(
            group_refusal_state(SendRefusalReason::CreateUnconfirmed),
            NativePermitState::GroupCreateUnconfirmed
        );
        assert_eq!(
            group_refusal_state(SendRefusalReason::CreateNotRcs),
            NativePermitState::GroupCreateNotRcs
        );
    }
}
