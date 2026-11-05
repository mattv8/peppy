//! Restricted local-native RCS handoff contract.
//!
//! These records are intentionally separate from vault events and shared conversation DTOs. They
//! are the only JSON-schema-capable projection an Android transport may receive after core has
//! admitted an RCS attempt.
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NativeRcsLifetime {
    pub runtime_id: String,
    pub run_generation: u64,
    pub link_generation: u64,
    pub session_generation: u64,
    pub context_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NativeRcsParticipant {
    pub identity: String,
    pub google_participant_id: String,
    pub identifier_type: i32,
    pub is_visible: bool,
    pub is_me: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NativeOwnNumberSource {
    SubscriptionManagerPhoneNumber,
    TelephonyLine1Number,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NativeCarrierRouteBinding {
    pub carrier_route_id: String,
    pub android_subscription_id: i32,
    pub is_current_default_sms: bool,
    pub own_number_raw: String,
    pub own_number_source: NativeOwnNumberSource,
    pub route_observed_at_ms: i64,
    pub route_context_revision: u64,
    pub same_handset_evidence_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NativeRcsSimPayload {
    pub two: i32,
    pub sim_number: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NativeRcsOutgoing {
    pub outgoing_id: String,
    pub sim_participant_id: String,
    pub own_identity_raw: String,
    pub sim_payload: NativeRcsSimPayload,
    pub rcs_chats_enabled: bool,
}


#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NativeSettingsProvenance {
    CurrentEvent,
    OldEvent,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NativeSettingsCurrentness {
    ContinuousVerifiedFeed,
    Unproven,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NativeRcsEvidence {
    pub lifetime: NativeRcsLifetime,
    pub lookup_id: String,
    pub lookup_complete: bool,
    pub google_candidate_count: u32,
    pub settings_revision: u64,
    pub settings_provenance: NativeSettingsProvenance,
    pub settings_observed_at_ms: i64,
    pub settings_stream_epoch: u64,
    pub settings_currentness: NativeSettingsCurrentness,
    /// Every current Google settings SIM record; this is never a shared DTO.
    pub current_settings_sims: Vec<NativeRcsOutgoing>,
    pub settings_validated_at_ms: i64,
    pub conversation_observed_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NativeRcsTargetClass {
    OneToOne,
    SelfChat,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LocalRcsTargetSnapshot {
    pub proof_version: u32,
    pub google_conversation_id: String,
    pub participants: Vec<NativeRcsParticipant>,
    pub class: NativeRcsTargetClass,
    pub canonical_recipient: String,
    pub canonical_own_identity: String,
    pub identity_region: String,
    pub canonicalizer_revision: u32,
    pub selected_sim_id: String,
    pub carrier_binding: NativeCarrierRouteBinding,
    pub outgoing: NativeRcsOutgoing,
    pub evidence: NativeRcsEvidence,
}

/// Private commit input emitted only by core from a durable `RcsPermit`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NativeRcsCommitProjection {
    pub command_id: String,
    pub tmp_id: String,
    pub mapping_revision: u64,
    pub proof_version: u32,
    pub proof_digest: Vec<u8>,
    pub message_body: String,
    pub target: LocalRcsTargetSnapshot,
}

/// Schema for the restricted local-native handoff only; it is not an event or public relay API.
pub fn restricted_native_rcs_handoff_schema() -> serde_json::Value {
    serde_json::to_value(schema_for!(NativeRcsCommitProjection))
        .expect("native RCS handoff schema is serializable")
}
