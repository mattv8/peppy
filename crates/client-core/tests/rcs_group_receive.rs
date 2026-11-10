//! Group receive, history recovery, provider-row ownership, and typing roster (M4 C5).
#[path = "common/mod.rs"]
mod common;

use common::*;
use peppy_protocol::{TypingOriginV2, TypingStateV2};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const GOOGLE: &str = "g-weekend";
// Valid numbers: provider-row routing canonicalises with libphonenumber, which rejects `555`.
const SELF: &str = "+14155550100";
const ALEX: &str = "+14155552671";
const SAM: &str = "+14155552672";
const LEE: &str = "+14155552673";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn sqlcipher(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
}

fn publish_current_gateway(client: &Client, config: &ClientConfig) {
    let now = now_ms();
    client
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: config.device_id,
                state: RcsCapabilityState::Linked,
                availability: true,
                health: true,
                phone_responsive: true,
                observed_at_ms: now,
                capability_revision: 1,
                feature_bits: 1,
                linked_since_ms: Some(now),
            },
            &[],
            now,
        )
        .unwrap();
}

/// Mirrors the 1:1 receive fixture: a linked lifecycle and a mapping context for this gateway.
fn authorize_local_link_gateway(client: &Client, config: &ClientConfig) {
    let now = now_ms();
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, now, now)
        .unwrap();
    sqlcipher(&config.database_path)
        .execute(
            "INSERT INTO metadata(k,v) VALUES('rcs_mapping_context_v1',?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
            [json!({
                "link_generation": 1,
                "session_generation": 1,
                "context_revision": 1,
                "link_state": 0,
            })
            .to_string()],
        )
        .unwrap();
}

fn participant(number: &str, is_self: bool) -> Value {
    json!({
        "participant_id": format!("p-{number}"),
        "identifier_type": 1,
        "raw_number": number,
        "formatted_number": number,
        "participant_id_present": true,
        "number_present": true,
        "visible": true,
        "self": is_self,
        "canonical_address": number,
        "canonical_address_present": true,
        "display_name": "",
    })
}

/// Google's view of the group `GOOGLE`: this gateway plus the given visible non-self numbers.
fn group(others: &[&str], last_message_ms: i64) -> Value {
    let mut participants = vec![participant(SELF, true)];
    participants.extend(others.iter().map(|number| participant(number, false)));
    json!({
        "google_conversation_id": GOOGLE,
        "conversation_type": 2,
        "conversation_status": 1,
        "classification": "eligible",
        "group": true,
        "visible_non_self_count": others.len(),
        "last_message_ms": last_message_ms,
        "participants": participants,
        "name": "Weekend",
        "is_group_chat": true,
        "read_only": false,
        "invisible_participant_count": 0,
        "member_digest": format!("digest-{}", others.len()),
    })
}

fn snapshot(revision: u64, conversations: Vec<Value>) -> String {
    json!({
        "code": "conversation_candidate_snapshot",
        "snapshot_token": format!("1:2:{revision}"),
        "generation": 1,
        "stream_epoch": 2,
        "revision": revision,
        "run_incarnation": "run",
        "pairing_evidence_id": "pairing",
        "complete": true,
        "conversations": conversations,
    })
    .to_string()
}

/// A linked gateway whose snapshot proves the group `GOOGLE` with `others`.
fn linked_group(others: &[&str]) -> (tempfile::TempDir, Client, ClientConfig, RcsReceiveContext) {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-group-receive", &vault);
    let client = unlocked(&config, &vault);
    publish_current_gateway(&client, &config);
    authorize_local_link_gateway(&client, &config);
    let now = now_ms();
    client
        .ingest_group_candidates(&snapshot(1, vec![group(others, now)]), now)
        .unwrap();
    let context = client.rcs_receive_context(GOOGLE).unwrap().unwrap();
    (directory, client, config, context)
}

fn incoming(google_message_id: &str, body: &str) -> RcsProjectedRecord {
    RcsProjectedRecord {
        google_message_id: google_message_id.into(),
        google_conversation_id: GOOGLE.into(),
        direction: Direction::Incoming,
        body: body.into(),
        sent_at_ms: Some(now_ms()),
        tmp_id: None,
        rcs_reported: true,
        e2ee_reported: None,
    }
}

fn restricted_text_row(row_id: i64, addresses: &[&str], observed_at_ms: i64) -> RcsProviderRow {
    RcsProviderRow {
        provider_row_id: row_id,
        provider_thread_id: 7,
        provider_type: 2,
        observed_at_ms,
        addresses: addresses.iter().map(|a| a.to_string()).collect(),
        restricted: true,
        text_only: true,
    }
}

#[test]
fn live_group_resolves_a_receive_context_and_joins_history_recovery() {
    let (_directory, client, _config, context) = linked_group(&[ALEX, SAM]);
    assert_eq!(context.google_conversation_id, GOOGLE);
    let lanes = client.rcs_link_owned_conversations().unwrap();
    assert!(
        lanes
            .iter()
            .any(|lane| lane.conversation_id == context.conversation_id
                && lane.google_conversation_id == GOOGLE),
        "group lanes are enumerated with the 1:1 lanes for recovery"
    );
    assert_eq!(
        client
            .complete_rcs_recovery(
                context.conversation_id,
                context.ownership_generation,
                now_ms()
            )
            .unwrap(),
        RcsRecoveryOutcome::Recorded,
        "recovery fences on the group lane generation exactly as 1:1"
    );
}

#[test]
fn incoming_group_message_is_attributed_to_its_roster_member_and_dedupes() {
    let (_directory, client, _config, context) = linked_group(&[ALEX, SAM]);
    let conversation = context.conversation_id;
    let observe = |sender: RcsGroupSender| {
        client
            .ingest_rcs_receive_observation(
                conversation,
                context.ownership_generation,
                incoming("m-1", "hi"),
                Some(sender),
                RcsObservationProvenance::Live,
            )
            .unwrap()
    };
    assert_eq!(
        observe(RcsGroupSender::Address(ALEX.into())),
        RcsReceiveOutcome::Inserted
    );
    assert_eq!(
        only_message(&client, conversation)
            .payload
            .sender_address
            .as_deref(),
        Some(ALEX)
    );
    assert_eq!(
        observe(RcsGroupSender::Address(ALEX.into())),
        RcsReceiveOutcome::Duplicate
    );
    assert!(
        client
            .proved_group_member_revision(conversation)
            .unwrap()
            .is_some(),
        "an attributed message keeps the mapping Live"
    );
}

#[test]
fn unknown_or_unrostered_sender_is_ingested_unattributed_and_pauses_for_reprove() {
    let (_directory, client, _config, context) = linked_group(&[ALEX, SAM]);
    let conversation = context.conversation_id;
    assert_eq!(
        client
            .ingest_rcs_receive_observation(
                conversation,
                context.ownership_generation,
                incoming("m-unknown", "who?"),
                Some(RcsGroupSender::UnknownMember),
                RcsObservationProvenance::Live,
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    assert_eq!(
        only_message(&client, conversation).payload.sender_address,
        None
    );
    assert_eq!(
        client.proved_group_member_revision(conversation).unwrap(),
        None,
        "an unknown sender pauses the mapping"
    );
    assert!(
        client.rcs_receive_context(GOOGLE).unwrap().is_some(),
        "receive never stops for a paused group"
    );

    assert_eq!(
        client
            .ingest_rcs_receive_observation(
                conversation,
                context.ownership_generation,
                incoming("m-lee", "not on the roster"),
                Some(RcsGroupSender::Address(LEE.into())),
                RcsObservationProvenance::Live,
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    let senders = client
        .messages(conversation)
        .unwrap()
        .into_iter()
        .map(|message| message.payload.sender_address)
        .collect::<Vec<_>>();
    assert_eq!(
        senders,
        vec![None, None],
        "a sender outside the roster is never attributed to a member"
    );

    client
        .ingest_group_candidates(&snapshot(2, vec![group(&[ALEX, SAM], now_ms())]), now_ms())
        .unwrap();
    assert!(
        client
            .proved_group_member_revision(conversation)
            .unwrap()
            .is_some(),
        "the next snapshot re-proves the paused mapping"
    );
}

#[test]
fn self_sent_group_observation_is_outgoing_and_never_attributed() {
    let (_directory, client, _config, context) = linked_group(&[ALEX, SAM]);
    let conversation = context.conversation_id;
    let record = RcsProjectedRecord {
        direction: Direction::Outgoing,
        ..incoming("m-mine", "sent from my tablet")
    };
    assert_eq!(
        client
            .ingest_rcs_receive_observation(
                conversation,
                context.ownership_generation,
                record,
                Some(RcsGroupSender::SelfSent),
                RcsObservationProvenance::Live,
            )
            .unwrap(),
        RcsReceiveOutcome::UnmatchedOutgoing
    );
    assert_eq!(
        only_message(&client, conversation).payload.sender_address,
        None
    );
}

#[test]
fn withdrawn_group_refuses_receive_and_leaves_recovery() {
    let (_directory, client, _config, context) = linked_group(&[ALEX, SAM]);
    let report = client
        .ingest_group_candidates(&snapshot(2, vec![]), now_ms())
        .unwrap();
    assert_eq!(report.withdrawn, 1);
    assert!(client.rcs_receive_context(GOOGLE).unwrap().is_none());
    assert!(
        client
            .rcs_link_owned_conversations()
            .unwrap()
            .iter()
            .all(|lane| lane.conversation_id != context.conversation_id)
    );
    assert_eq!(
        client
            .ingest_rcs_receive_observation(
                context.conversation_id,
                context.ownership_generation,
                incoming("m-late", "after withdrawal"),
                Some(RcsGroupSender::Address(ALEX.into())),
                RcsObservationProvenance::Live,
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::ConversationUnproven)
    );
}

#[test]
fn group_provider_rows_route_by_exact_roster_and_detect_only_restricted_text() {
    let (_directory, client, _config, _context) = linked_group(&[ALEX, SAM]);
    let observed = now_ms();
    assert_eq!(
        client
            .route_rcs_provider_row(restricted_text_row(301, &[ALEX, SAM], observed))
            .unwrap(),
        RcsProviderRowRoute::DetectorRecorded,
        "an exact roster match is detector-only after the guard band"
    );
    assert_eq!(
        client
            .route_rcs_provider_row(restricted_text_row(302, &[ALEX], observed))
            .unwrap(),
        RcsProviderRowRoute::Alert,
        "a partial roster is ambiguous while a link lane exists"
    );
    let unrestricted = RcsProviderRow {
        restricted: false,
        ..restricted_text_row(303, &[ALEX, SAM], observed)
    };
    assert_eq!(
        client.route_rcs_provider_row_result(unrestricted).unwrap(),
        RcsProviderRowRouteResult {
            route: RcsProviderRowRoute::Provider,
            reason: Some(RcsProviderRouteReason::NotRestrictedText),
        }
    );
}

#[test]
fn group_typing_attributes_only_live_roster_members() {
    let (_directory, client, config, context) = linked_group(&[ALEX, SAM]);
    let conversation = context.conversation_id;
    client
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: vec![config.device_id],
                local_gateway_device_id: config.device_id,
                local_executor: LiveExecutor::CurrentGateway,
            },
            LiveClock {
                wall_ms: 1_000,
                mono_ms: 1_000,
            },
        )
        .unwrap();
    let seal = |participant: Option<&str>, origin: TypingOriginV2, mono: u64| {
        client
            .seal_typing_signal_v2(
                conversation,
                config.device_id,
                origin,
                participant.map(str::to_owned),
                TypingStateV2::Started,
                LiveClock {
                    wall_ms: mono as i64,
                    mono_ms: mono,
                },
            )
            .unwrap()
    };
    assert!(
        seal(Some(ALEX), TypingOriginV2::GoogleContact, 1_001).is_some(),
        "a roster member of a Live group is attributable"
    );
    assert!(
        seal(Some(LEE), TypingOriginV2::GoogleContact, 5_001).is_none(),
        "a non-member is never sealed"
    );
    assert!(
        seal(None, TypingOriginV2::VaultDevice, 9_001).is_some(),
        "the user's own typing flows while the group is Live"
    );

    client
        .ingest_rcs_receive_observation(
            conversation,
            context.ownership_generation,
            incoming("m-unknown", "?"),
            Some(RcsGroupSender::UnknownMember),
            RcsObservationProvenance::Live,
        )
        .unwrap();
    assert!(
        seal(None, TypingOriginV2::VaultDevice, 13_001).is_none(),
        "outbound typing into a paused group is suppressed"
    );
}
