use crate::{MobileBindingsError, NativeClient, with_client};
use peppy_client_core::{
    NativeRcsCommitProjection, NativeRcsPreparation, NativeRcsTargetClass, RcsMappingClass,
    RcsMappingObservation,
};
use serde::Serialize;
use std::sync::{Arc, Mutex};

const RCS_NATIVE_JSON_CAP: usize = 65_536;

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
    let mapping = preparation.mapping();
    let class = match mapping.class {
        RcsMappingClass::OneToOne => NativeRcsTargetClass::OneToOne,
        RcsMappingClass::SelfChat => NativeRcsTargetClass::SelfChat,
    };
    serde_json::to_string(&NativeRcsFacts {
        request: preparation.request(),
        stored_mapping: NativeRcsStoredMapping {
            conversation_id: mapping.conversation_id.to_string(),
            google_conversation_id: &mapping.google_conversation_id,
            class,
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
    Ok(Arc::new(NativeRcsCommittedSend { commit_json }))
}

#[uniffi::export]
impl NativeRcsCommittedSend {
    pub fn commit_json(&self) -> String {
        self.commit_json.clone()
    }
}

#[cfg(test)]
#[path = "rcs_native_tests.rs"]
mod rcs_native_tests;
