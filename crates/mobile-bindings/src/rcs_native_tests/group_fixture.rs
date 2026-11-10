//! Shared group fixture for the native group tests. Setup goes through public APIs only: a
//! region-bearing contact book, a real draft sent to the group (so the roster and command sync to
//! the gateway), and Google's snapshot ingested by the gateway. Test-only.

use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use peppy_client_core::{
    ComposeDraftUpdate, ConversationId, GatewayRoute, GroupDraftSend, GroupIngestReport,
    RcsCapabilityState,
};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const GROUP_SELF: &str = "+12025550101";
pub(crate) const GROUP_MEMBERS: [&str; 2] = ["+12025550102", "+12025550103"];
pub(crate) const GROUP_GOOGLE_ID: &str = "synthetic-conversation";

pub(crate) struct GroupFixture {
    pub(crate) gateway: Arc<NativeClient>,
    pub(crate) conversation_id: ConversationId,
    pub(crate) command_id: String,
    _temp_dir: TempDir,
}

/// Sender drafts a group text to `members`; the gateway receives the roster and command.
/// The gateway's region is set; its own identity and proof come later, from the observation.
pub(crate) fn group_fixture(members: &[&str]) -> GroupFixture {
    let temp_dir = TempDir::new();
    let vault_id = Uuid::new_v4().to_string();
    let sender = native_client(&temp_dir, &vault_id, Uuid::new_v4().to_string());
    let gateway_device_id = Uuid::new_v4();
    let gateway = native_client(&temp_dir, &vault_id, gateway_device_id.to_string());
    let material = create_vault_material(vault_id, PASSPHRASE.into()).unwrap();
    for client in [&sender, &gateway] {
        client
            .unlock(
                material.profile_json.clone(),
                material.header_json.clone(),
                PASSPHRASE.into(),
            )
            .unwrap();
    }
    // The gateway is a linked, current RCS gateway, as the receive and send paths require.
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    with_client(&gateway.client, |core| {
        core.record_rcs_link_lifecycle(
            RcsCapabilityState::Linked,
            true,
            true,
            true,
            now_ms,
            now_ms,
        )?;
        let book = json!({
            "schema_version": 1,
            "book": {
                "id": "group-region", "owner_device_id": gateway_device_id.to_string(),
                "generation": "1", "state": "active", "region": "US"
            },
            "contacts": []
        });
        // Contact books are retained control: the gateway must first record the server's
        // compaction state and finish its (empty) backfill.
        core.set_server_compaction_state(true, true)?;
        core.capture_contact_book(&book.to_string())?;
        Ok(())
    })
    .unwrap();

    let draft = with_client(&sender.client, |core| {
        core.save_new_compose_draft(ComposeDraftUpdate {
            group_name: None,
            text: "hello group".into(),
            recipients: members.iter().map(|m| (*m).to_owned()).collect(),
            attachment_ids: Vec::new(),
            // Drafts carry the carrier route; the gateway's RCS permit decides the transport.
            route: Some(GatewayRoute::Carrier {
                gateway_device_id: gateway_device_id.to_string().parse().unwrap(),
                subscription_id: "android-subscription-1".into(),
            }),
        })
    })
    .unwrap();
    let queued = with_client(&sender.client, |core| {
        core.send_compose_draft_for_group(
            draft.draft_id,
            draft.revision,
            GroupDraftSend {
                group_name: Some("Weekend".into()),
                expected_member_revision: None,
                group_create_confirm_duplicate: false,
            },
        )
    })
    .unwrap();
    deliver(&sender, &gateway);

    GroupFixture {
        gateway,
        conversation_id: draft.conversation_id,
        command_id: queued.command_id.to_string(),
        _temp_dir: temp_dir,
    }
}

impl GroupFixture {
    /// The native route step for a group: advances the link context from a route observation, as
    /// the production coordinator does before it proves the group. No 1:1 verification runs.
    pub(crate) fn advance_route(&self, route: &RcsMappingObservation, now_ms: i64) -> bool {
        with_client(&self.gateway.client, |core| {
            core.verify_rcs_group_route(route, now_ms)
        })
        .unwrap()
    }

    pub(crate) fn ingest(&self, snapshot: &str, now_ms: i64) -> GroupIngestReport {
        with_client(&self.gateway.client, |core| {
            core.ingest_group_candidates(snapshot, now_ms)
        })
        .unwrap()
    }

    pub(crate) fn verify(
        &self,
        observation: &RcsMappingObservation,
        now_ms: i64,
    ) -> NativeRcsGroupVerification {
        gateway_verify_rcs_group_observation(
            self.gateway.clone(),
            serde_json::to_string(observation).unwrap(),
            now_ms,
        )
        .unwrap()
    }

    pub(crate) fn prepare(&self, now_ms: i64) -> Arc<NativeRcsSendPreparation> {
        gateway_prepare_native_rcs_send(self.gateway.clone(), self.command_id.clone(), now_ms)
            .unwrap()
    }

    pub(crate) fn proved_revision(&self) -> Option<u64> {
        with_client(&self.gateway.client, |core| {
            core.proved_group_member_revision(self.conversation_id)
        })
        .unwrap()
    }
}

/// Go's member digest over the non-self numbers: SHA-256 of the sorted, deduplicated numbers
/// joined by LF, base64. Mirrors the Go and core digest definition.
pub(crate) fn group_member_digest(members: &[&str]) -> String {
    let mut sorted = members.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    STANDARD.encode(Sha256::digest(sorted.join("\n").as_bytes()))
}

/// Google's group snapshot for `members` under `google_conversation_id`, with the same shape the
/// Go snapshot emits.
pub(crate) fn group_snapshot(
    google_conversation_id: &str,
    members: &[&str],
    last_message_ms: i64,
) -> String {
    let participant = |number: &str, is_self: bool| {
        json!({
            "participant_id": format!("p-{number}"),
            "identifier_type": 1,
            "raw_number": number,
            "formatted_number": "",
            "participant_id_present": true,
            "number_present": true,
            "visible": true,
            "self": is_self,
            "canonical_address": number,
            "canonical_address_present": true,
            "display_name": "",
        })
    };
    let mut participants = vec![participant(GROUP_SELF, true)];
    participants.extend(members.iter().map(|m| participant(m, false)));
    json!({
        "code": "conversation_candidate_snapshot",
        "snapshot_token": "1:1:1",
        "generation": 1,
        "stream_epoch": 1,
        "revision": 1,
        "run_incarnation": "runtime",
        "pairing_evidence_id": "pairing",
        "complete": true,
        "conversations": [{
            "google_conversation_id": google_conversation_id,
            "conversation_type": 2,
            "conversation_status": 1,
            "classification": "eligible",
            "group": true,
            "visible_non_self_count": members.len(),
            "last_message_ms": last_message_ms,
            "participants": participants,
            "name": "Weekend",
            "is_group_chat": true,
            "read_only": false,
            "invisible_participant_count": 0,
            "member_digest": group_member_digest(members),
        }],
    })
    .to_string()
}

/// A fresh target observation of the group, every timestamp at `at`. Self is the single `is_me`
/// participant; `members` are the visible non-self numbers.
pub(crate) fn group_observation(members: &[&str], at: i64) -> RcsMappingObservation {
    let mut observation = observation();
    observation.google_conversation_id = GROUP_GOOGLE_ID.into();
    observation.participants = std::iter::once(RcsParticipantObservation {
        identity: GROUP_SELF.into(),
        google_participant_id: "p-self".into(),
        identifier_type: 1,
        is_visible: true,
        is_me: true,
    })
    .chain(members.iter().map(|member| RcsParticipantObservation {
        identity: (*member).into(),
        google_participant_id: format!("p-{member}"),
        identifier_type: 1,
        is_visible: true,
        is_me: false,
    }))
    .collect();
    observation.observed_at_ms = at;
    observation.carrier_binding.route_observed_at_ms = at;
    observation.native_evidence.settings_observed_at_ms = at;
    observation.native_evidence.settings_validated_at_ms = at;
    observation.native_evidence.conversation_observed_at_ms = at;
    observation
}
