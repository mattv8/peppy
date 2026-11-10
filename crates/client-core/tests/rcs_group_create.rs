//! Create-on-first-send, unknown-outcome blocking, and create reconciliation (M4 C4c).
#[path = "common/mod.rs"]
mod common;

use common::*;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const GOOGLE: &str = "g-weekend";
const OTHER_GOOGLE: &str = "g-weekend-two";
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

/// The link is Live for this gateway: a linked lifecycle and a mapping context.
fn link_live(client: &Client, config: &ClientConfig) {
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

struct Fixture {
    _directory: tempfile::TempDir,
    client: Client,
    config: ClientConfig,
    vault: Vault,
}

fn fixture(live: bool) -> Fixture {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-group-create", &vault);
    let client = unlocked(&config, &vault);
    if live {
        publish_current_gateway(&client, &config);
        link_live(&client, &config);
    }
    Fixture {
        _directory: directory,
        client,
        config,
        vault,
    }
}

/// Sends a draft to `recipients` as a group send and returns the conversation and command.
/// Recording the create request is the gateway's receive step; the tests write it directly.
fn group_send(
    fixture: &Fixture,
    recipients: &[&str],
    confirm_duplicate: bool,
) -> (ConversationId, CommandId) {
    let draft = fixture
        .client
        .save_new_compose_draft(ComposeDraftUpdate {
            group_name: None,
            text: "hello".into(),
            recipients: recipients.iter().map(|r| (*r).to_owned()).collect(),
            attachment_ids: Vec::new(),
            route: Some(GatewayRoute::Carrier {
                gateway_device_id: fixture.config.device_id,
                subscription_id: "sub-1".into(),
            }),
        })
        .unwrap();
    let queued = fixture
        .client
        .send_compose_draft_for_group(
            draft.draft_id,
            draft.revision,
            GroupDraftSend {
                group_name: Some("Weekend".into()),
                expected_member_revision: None,
                group_create_confirm_duplicate: confirm_duplicate,
            },
        )
        .unwrap();
    record_create_request(fixture, queued.command_id, confirm_duplicate);
    (draft.conversation_id, queued.command_id)
}

fn record_create_request(fixture: &Fixture, command: CommandId, confirm_duplicate: bool) {
    let mut request = json!({"name": "Weekend"});
    if confirm_duplicate {
        request["confirm_duplicate"] = json!(true);
    }
    sqlcipher(&fixture.config.database_path)
        .execute(
            "INSERT INTO metadata(k,v) VALUES(?,?)",
            [
                format!("rcs_group_send_v1/{command}"),
                json!({"group_create": request}).to_string(),
            ],
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

/// Google's view of one group: this gateway plus the given visible non-self numbers.
fn group_at(id: &str, others: &[&str], last_message_ms: i64) -> Value {
    let mut participants = vec![participant(SELF, true)];
    participants.extend(others.iter().map(|number| participant(number, false)));
    json!({
        "google_conversation_id": id,
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
        "member_digest": format!("digest-{id}-{}", others.len()),
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

fn begin(fixture: &Fixture, conversation: ConversationId, command: CommandId) -> GroupCreateBegin {
    fixture
        .client
        .begin_rcs_group_create(conversation, command, now_ms())
        .unwrap()
}

fn permit(
    fixture: &Fixture,
    conversation: ConversationId,
    command: CommandId,
) -> GroupCreatePermit {
    match begin(fixture, conversation, command) {
        GroupCreateBegin::Permit(permit) => permit,
        other => panic!("expected a permit, got {other:?}"),
    }
}

fn refused(
    fixture: &Fixture,
    conversation: ConversationId,
    command: CommandId,
) -> SendRefusalReason {
    match begin(fixture, conversation, command) {
        GroupCreateBegin::Refused(reason) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn answer(fixture: &Fixture, command: CommandId, json: &str) -> GroupCreateOutcome {
    fixture
        .client
        .commit_rcs_group_create(command, json)
        .unwrap()
}

#[test]
fn a_create_permit_is_issued_only_while_the_link_is_live() {
    let live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    let issued = permit(&live, conversation, command);
    assert_eq!(issued.numbers, vec![ALEX.to_owned(), SAM.to_owned()]);
    assert_eq!(issued.name.as_deref(), Some("Weekend"));

    let unlinked = fixture(false);
    let (conversation, command) = group_send(&unlinked, &[ALEX, SAM], false);
    assert!(
        unlinked
            .client
            .begin_rcs_group_create(conversation, command, now_ms())
            .is_err(),
        "a group send without a Live link must not create"
    );
    assert_eq!(
        unlinked.client.group_create_state(conversation).unwrap(),
        GroupCreateState::None
    );
}

#[test]
fn a_one_to_one_send_never_creates_a_group() {
    let live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX], false);
    assert!(
        live.client
            .begin_rcs_group_create(conversation, command, now_ms())
            .is_err()
    );
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::None
    );
    assert!(live.client.group_view(conversation).unwrap().is_none());
}

#[test]
fn an_unknown_create_blocks_a_second_create_until_it_is_confirmed() {
    let live = fixture(true);
    let (conversation, first) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, first);
    assert_eq!(
        answer(&live, first, r#"{"code":"unknown"}"#),
        GroupCreateOutcome::Unknown
    );
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Unknown
    );
    let view = live.client.group_view(conversation).unwrap().unwrap();
    assert!(!view.can_create);
    assert!(view.create_needs_confirmation);

    let (_, second) = group_send(&live, &[ALEX, SAM], false);
    assert_eq!(
        refused(&live, conversation, second),
        SendRefusalReason::CreateUnconfirmed
    );
    let (_, confirmed) = group_send(&live, &[ALEX, SAM], true);
    permit(&live, conversation, confirmed);
}

#[test]
fn a_created_group_links_on_the_next_snapshot_with_the_composer_revision() {
    let live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, command);
    assert_eq!(
        answer(
            &live,
            command,
            &format!(r#"{{"code":"created_rcs_group","google_conversation_id":"{GOOGLE}"}}"#)
        ),
        GroupCreateOutcome::Pending
    );
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Pending
    );
    assert_eq!(
        live.client
            .proved_group_member_revision(conversation)
            .unwrap(),
        None
    );

    let report = live
        .client
        .ingest_group_candidates(
            &snapshot(1, vec![group_at(GOOGLE, &[ALEX, SAM], now_ms())]),
            now_ms(),
        )
        .unwrap();
    assert_eq!(report.linked, 1);
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Linked
    );
    let view = live.client.group_view(conversation).unwrap().unwrap();
    assert_eq!(view.state, GroupLinkState::Live);
    assert!(!view.can_create);
    // The revision the composer showed is the one the proof carries, so the first send can pass.
    assert_eq!(
        live.client
            .proved_group_member_revision(conversation)
            .unwrap(),
        Some(view.member_revision)
    );
}

#[test]
fn an_existing_rcs_group_links_the_same_way() {
    let live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, command);
    answer(
        &live,
        command,
        &format!(r#"{{"code":"existing_rcs_group","google_conversation_id":"{GOOGLE}"}}"#),
    );
    let report = live
        .client
        .ingest_group_candidates(
            &snapshot(1, vec![group_at(GOOGLE, &[ALEX, SAM], now_ms())]),
            now_ms(),
        )
        .unwrap();
    assert_eq!(report.linked, 1);
    assert_eq!(
        live.client.group_view(conversation).unwrap().unwrap().state,
        GroupLinkState::Live
    );
}

#[test]
fn a_group_that_is_not_rcs_goes_to_mms_and_offers_no_further_create() {
    let live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, command);
    assert_eq!(
        answer(&live, command, r#"{"code":"not_rcs"}"#),
        GroupCreateOutcome::NotRcs
    );
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::NotRcs
    );
    let view = live.client.group_view(conversation).unwrap().unwrap();
    assert!(!view.can_create);
    assert!(!view.create_needs_confirmation);
    // A later message is no create request: begin declines, and the MMS rules apply.
    let (_, later) = group_send(&live, &[ALEX, SAM], false);
    assert!(
        live.client
            .begin_rcs_group_create(conversation, later, now_ms())
            .is_err()
    );
}

#[test]
fn a_created_group_with_other_members_is_refused_and_never_linked() {
    let live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, command);
    answer(
        &live,
        command,
        &format!(r#"{{"code":"created_rcs_group","google_conversation_id":"{GOOGLE}"}}"#),
    );
    let report = live
        .client
        .ingest_group_candidates(
            &snapshot(1, vec![group_at(GOOGLE, &[ALEX, LEE], now_ms())]),
            now_ms(),
        )
        .unwrap();
    assert_eq!(report.conflicts, 1);
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Refused(SendRefusalReason::MembersChanged)
    );
    assert_eq!(
        live.client
            .proved_group_member_revision(conversation)
            .unwrap(),
        None
    );
}

#[test]
fn a_create_answer_that_is_not_recognised_is_unknown() {
    let live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, command);
    assert_eq!(
        answer(&live, command, r#"{"code":"created_rcs_group"}"#),
        GroupCreateOutcome::Unknown
    );
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Unknown
    );
}

#[test]
fn an_unknown_create_reconciles_to_one_newer_matching_group() {
    let live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, command);
    answer(&live, command, r#"{"code":"unknown"}"#);
    let newer = now_ms() + 60_000;
    let report = live
        .client
        .ingest_group_candidates(
            &snapshot(1, vec![group_at(GOOGLE, &[ALEX, SAM], newer)]),
            now_ms(),
        )
        .unwrap();
    assert_eq!(report.linked, 1);
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Linked
    );
    assert_eq!(
        live.client.group_view(conversation).unwrap().unwrap().state,
        GroupLinkState::Live
    );
}

#[test]
fn an_unknown_create_stays_unknown_when_a_complete_snapshot_has_no_match() {
    let live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, command);
    answer(&live, command, r#"{"code":"unknown"}"#);
    live.client
        .ingest_group_candidates(&snapshot(1, vec![]), now_ms())
        .unwrap();
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Unknown
    );
    let view = live.client.group_view(conversation).unwrap().unwrap();
    assert!(!view.can_create);
    assert!(view.create_needs_confirmation);
}

#[test]
fn an_unknown_create_is_never_permitted_again_by_its_own_command_or_empty_snapshots() {
    let live = fixture(true);
    let (conversation, consumed) = group_send(&live, &[ALEX, SAM], true);
    permit(&live, conversation, consumed);
    answer(&live, consumed, r#"{"code":"unknown"}"#);
    // Wakes: stale, empty, and unprovable snapshots, each followed by the same command again.
    for revision in 1..=3 {
        live.client
            .ingest_group_candidates(&snapshot(revision, vec![]), now_ms())
            .unwrap();
        assert_eq!(
            refused(&live, conversation, consumed),
            SendRefusalReason::CreateUnconfirmed
        );
    }
    // An exact-roster group older than the attempt is unresolved, not evidence of absence.
    let old = group_at(GOOGLE, &[ALEX, SAM], 0);
    live.client
        .ingest_group_candidates(&snapshot(4, vec![old]), now_ms())
        .unwrap();
    assert_eq!(
        refused(&live, conversation, consumed),
        SendRefusalReason::CreateUnconfirmed
    );
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Unknown
    );
}

#[test]
fn a_confirmed_duplicate_permits_one_new_create_and_the_consumed_command_never_repeats() {
    let live = fixture(true);
    let (conversation, first) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, first);
    answer(&live, first, r#"{"code":"unknown"}"#);
    let (_, confirmed) = group_send(&live, &[ALEX, SAM], true);
    permit(&live, conversation, confirmed);
    answer(&live, confirmed, r#"{"code":"unknown"}"#);
    // The second create is now the unknown one; neither consumed command may create again.
    assert_eq!(
        refused(&live, conversation, first),
        SendRefusalReason::CreateUnconfirmed
    );
    assert_eq!(
        refused(&live, conversation, confirmed),
        SendRefusalReason::CreateUnconfirmed
    );
}

#[test]
fn a_begun_create_is_never_converted_by_ordinary_admission() {
    let live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, command);
    // Carrier admission of another group text while Google is still answering.
    let (_, other) = group_send(&live, &[ALEX, SAM], false);
    assert!(
        live.client
            .begin_rcs_group_create(conversation, other, now_ms())
            .is_err(),
        "a begun create blocks a second permit while it is in progress"
    );
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Begun
    );
    assert_eq!(
        answer(
            &live,
            command,
            &format!(r#"{{"code":"created_rcs_group","google_conversation_id":"{GOOGLE}"}}"#)
        ),
        GroupCreateOutcome::Pending,
        "the in-flight create commits exactly once"
    );
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Pending
    );
}

#[test]
fn a_begun_create_left_by_an_earlier_process_becomes_unknown_at_the_recovery_boundary() {
    let mut live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, command);
    let config = live.config.clone();
    // Reopening inside the same process is not a recovery boundary: the create is still in flight.
    let same_process = live.client;
    drop(same_process);
    live.client = open(&config);
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Begun
    );
    // A row owned by an earlier process was interrupted: it is unknown and blocks the old command.
    sqlcipher(&config.database_path)
        .execute(
            "UPDATE rcs_group_create_ledger SET owner_token='earlier-process' WHERE conversation_id=?",
            [conversation.to_string()],
        )
        .unwrap();
    let previous = live.client;
    drop(previous);
    live.client = open(&config);
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Unknown
    );
    assert_eq!(
        refused(&live, conversation, command),
        SendRefusalReason::CreateUnconfirmed
    );
}

#[test]
fn an_unknown_create_stays_unknown_when_two_newer_groups_match() {
    let live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, command);
    answer(&live, command, r#"{"code":"unknown"}"#);
    let newer = now_ms() + 60_000;
    let report = live
        .client
        .ingest_group_candidates(
            &snapshot(
                1,
                vec![
                    group_at(GOOGLE, &[ALEX, SAM], newer),
                    group_at(OTHER_GOOGLE, &[ALEX, SAM], newer),
                ],
            ),
            now_ms(),
        )
        .unwrap();
    assert_eq!(report.linked, 0);
    assert!(report.conflicts >= 1);
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Unknown
    );
    assert!(
        live.client
            .group_view(conversation)
            .unwrap()
            .unwrap()
            .create_needs_confirmation
    );
}

#[test]
fn a_failed_result_commit_stays_awaiting_settlement_until_it_is_settled_explicitly() {
    let mut live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, command);
    // Google has answered, but the durable result commit fails.
    sqlcipher(&live.config.database_path)
        .execute_batch(
            "CREATE TRIGGER fail_result_commit BEFORE UPDATE ON rcs_group_create_ledger BEGIN SELECT RAISE(ABORT,'commit failed'); END;",
        )
        .unwrap();
    assert!(
        live.client
            .commit_rcs_group_create(command, r#"{"code":"unknown"}"#)
            .is_err()
    );
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Begun
    );
    // Ordinary admission neither interrupts the create nor settles it.
    let (_, other) = group_send(&live, &[ALEX, SAM], false);
    assert!(
        live.client
            .begin_rcs_group_create(conversation, other, now_ms())
            .is_err(),
        "a begun create blocks a second permit while it awaits settlement"
    );
    assert!(live.client.settle_rcs_group_create(command).is_err());

    // A reopen in this process keeps the unfinished settlement while the database still refuses it.
    let config = live.config.clone();
    let previous = live.client;
    drop(previous);
    live.client = open(&config);
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Begun
    );

    // Once writes succeed, the explicit settlement persists unknown; Google is never called again.
    sqlcipher(&config.database_path)
        .execute_batch("DROP TRIGGER fail_result_commit;")
        .unwrap();
    live.client.settle_rcs_group_create(command).unwrap();
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Unknown
    );
    // Settled state survives a reopen, and the unfinished command stays terminal.
    let previous = live.client;
    drop(previous);
    live.client = open(&config);
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Unknown
    );
    assert_eq!(
        refused(&live, conversation, command),
        SendRefusalReason::CreateUnconfirmed
    );
    // Only a settled create can be confirmed as a duplicate by a new command.
    let (_, confirmed) = group_send(&live, &[ALEX, SAM], true);
    permit(&live, conversation, confirmed);
}

#[test]
fn a_settled_create_rejects_further_settlement_of_the_same_command() {
    let live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, command);
    sqlcipher(&live.config.database_path)
        .execute_batch(
            "CREATE TRIGGER fail_result_commit BEFORE UPDATE ON rcs_group_create_ledger BEGIN SELECT RAISE(ABORT,'commit failed'); END;",
        )
        .unwrap();
    assert!(
        live.client
            .commit_rcs_group_create(command, r#"{"code":"unknown"}"#)
            .is_err()
    );
    sqlcipher(&live.config.database_path)
        .execute_batch("DROP TRIGGER fail_result_commit;")
        .unwrap();
    live.client.settle_rcs_group_create(command).unwrap();
    // The retained entry was released: a second settlement is not a retained one and is refused.
    assert!(live.client.settle_rcs_group_create(command).is_err());
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Unknown
    );
}

#[test]
fn an_interrupted_create_whose_terminal_insert_aborts_stays_excluded_after_reconciliation() {
    let mut live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, command);
    let config = live.config.clone();
    // The earlier process dies with its create in flight. Recovery's terminal insert then fails.
    sqlcipher(&config.database_path)
        .execute(
            "UPDATE rcs_group_create_ledger SET owner_token='earlier-process' WHERE conversation_id=?",
            [conversation.to_string()],
        )
        .unwrap();
    sqlcipher(&config.database_path)
        .execute_batch(
            "CREATE TRIGGER fail_terminal_insert BEFORE INSERT ON rcs_group_create_terminal BEGIN SELECT RAISE(ABORT,'terminal insert failed'); END;",
        )
        .unwrap();
    let previous = live.client;
    drop(previous);
    // Recovery is one transaction: when its terminal row cannot commit, nothing it changed commits.
    assert!(Client::open(config.clone(), DatabaseKey::new(&DB_KEY).unwrap()).is_err());
    sqlcipher(&config.database_path)
        .execute_batch("DROP TRIGGER fail_terminal_insert;")
        .unwrap();

    live.client = unlocked(&config, &live.vault);
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Unknown
    );
    assert_eq!(
        refused(&live, conversation, command),
        SendRefusalReason::CreateUnconfirmed
    );
    // A positive reconciliation links the group, but the old command stays excluded.
    let newer = now_ms() + 60_000;
    let report = live
        .client
        .ingest_group_candidates(
            &snapshot(1, vec![group_at(GOOGLE, &[ALEX, SAM], newer)]),
            now_ms(),
        )
        .unwrap();
    assert_eq!(report.linked, 1);
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Linked
    );
    assert_eq!(
        refused(&live, conversation, command),
        SendRefusalReason::CreateUnconfirmed
    );
}

#[test]
fn a_retained_settlement_is_retried_at_the_next_gateway_boundary_on_a_live_client() {
    let live = fixture(true);
    let (conversation, command) = group_send(&live, &[ALEX, SAM], false);
    permit(&live, conversation, command);
    // An active create of another group in this process must be untouched by the retry.
    let (active, active_command) = group_send(&live, &[SAM, LEE], false);
    permit(&live, active, active_command);
    // The result commit fails, and so does its immediate settlement.
    sqlcipher(&live.config.database_path)
        .execute_batch(
            "CREATE TRIGGER fail_result_commit BEFORE UPDATE ON rcs_group_create_ledger BEGIN SELECT RAISE(ABORT,'commit failed'); END;",
        )
        .unwrap();
    assert!(
        live.client
            .commit_rcs_group_create(command, r#"{"code":"unknown"}"#)
            .is_err()
    );
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Begun
    );
    // The database recovers while this live client handle is retained.
    sqlcipher(&live.config.database_path)
        .execute_batch("DROP TRIGGER fail_result_commit;")
        .unwrap();
    // The next create pass on this handle is the retry boundary: it settles without calling Google.
    let (_, next) = group_send(&live, &[ALEX, SAM], false);
    assert_eq!(
        refused(&live, conversation, next),
        SendRefusalReason::CreateUnconfirmed
    );
    assert_eq!(
        live.client.group_create_state(conversation).unwrap(),
        GroupCreateState::Unknown
    );
    assert_eq!(
        refused(&live, conversation, command),
        SendRefusalReason::CreateUnconfirmed
    );
    assert!(
        live.client.settle_rcs_group_create(command).is_err(),
        "the retry released the retained entry"
    );
    assert_eq!(
        live.client.group_create_state(active).unwrap(),
        GroupCreateState::Begun
    );
}
