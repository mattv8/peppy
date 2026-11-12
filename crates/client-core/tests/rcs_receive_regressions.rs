#[path = "common/mod.rs"]
mod common;

use common::*;
use rusqlite::{Connection, params};
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn sql(config: &ClientConfig) -> Connection {
    let connection = Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
}

fn outbox_count(config: &ClientConfig) -> i64 {
    sql(config)
        .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
        .unwrap()
}

fn publish_gateway(client: &Client, config: &ClientConfig) {
    let observed_at_ms = now();
    client
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: config.device_id,
                state: RcsCapabilityState::Linked,
                availability: true,
                health: true,
                phone_responsive: true,
                observed_at_ms,
                capability_revision: 1,
                feature_bits: 1,
                linked_since_ms: Some(observed_at_ms),
            },
            &[],
            observed_at_ms,
        )
        .unwrap();
}

fn fixture() -> (
    tempfile::TempDir,
    Vault,
    ClientConfig,
    Client,
    ConversationId,
    RcsOwnership,
) {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-regression", &vault);
    let client = unlocked(&config, &vault);
    publish_gateway(&client, &config);
    let conversation = ConversationId::new();
    let ownership = client.begin_rcs_link_ownership(conversation, 0).unwrap();
    (directory, vault, config, client, conversation, ownership)
}

fn record(id: &str) -> RcsProjectedRecord {
    RcsProjectedRecord {
        google_message_id: id.into(),
        google_conversation_id: "google-conversation".into(),
        direction: Direction::Incoming,
        body: format!("body-{id}"),
        sent_at_ms: Some(1),
        tmp_id: None,
        rcs_reported: false,
        e2ee_reported: None,
        alternate_message_ids: Vec::new(),
        attachments: Vec::new(),
    }
}

fn status(id: &str, tmp_id: Option<&str>, value: RcsProjectedStatus) -> RcsStatusApplication {
    RcsStatusApplication {
        google_message_id: id.into(),
        google_conversation_id: "google-conversation".into(),
        tmp_id: tmp_id.map(str::to_owned),
        status: value,
    }
}

fn receive_snapshot(
    connection: &Connection,
    conversation: ConversationId,
) -> (i64, i64, i64, Option<String>, i64) {
    connection.query_row(
        "SELECT (SELECT COUNT(*) FROM messages), (SELECT COUNT(*) FROM rcs_message_identities), (SELECT COUNT(*) FROM outbox), history_token, observed_detector_high_water FROM rcs_lane_ownership WHERE conversation_id=?",
        [conversation.to_string()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    ).unwrap()
}

/// The stored local recovery record (`metadata` key `rcs_recovery:<conversation>`), parsed.
fn recovery_state(connection: &Connection, conversation: ConversationId) -> serde_json::Value {
    let json: String = connection
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [format!("rcs_recovery:{conversation}")],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&json).unwrap()
}

fn lifecycle_generation(config: &ClientConfig) -> u64 {
    sql(config)
        .query_row(
            "SELECT CAST(v AS INTEGER) FROM metadata WHERE k='rcs_link_lifecycle_generation_v1'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn record_terminal_unlink(client: &Client, config: &ClientConfig) -> u64 {
    let observed_at_ms = now();
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            observed_at_ms,
            observed_at_ms,
        )
        .unwrap();
    client
        .record_rcs_explicit_unlink_completed(observed_at_ms, observed_at_ms)
        .unwrap();
    lifecycle_generation(config)
}

#[test]
fn historical_observation_never_moves_recovery_state() {
    let (_dir, _vault, config, client, conversation, owner) = fixture();
    let connection = sql(&config);
    let proof_time: i64 = connection
        .query_row(
            "SELECT proof_time_ms FROM rcs_lane_ownership WHERE conversation_id=?",
            [conversation.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let before = recovery_state(&connection, conversation);
    let mut historical = record("historical");
    historical.sent_at_ms = Some(proof_time + 1);
    assert_eq!(
        client
            .ingest_rcs_projected_observation(
                conversation,
                owner.ownership_generation,
                historical,
                RcsObservationProvenance::Historical,
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    assert_eq!(recovery_state(&connection, conversation), before);
}

#[test]
fn fenced_unlink_cutover_rejects_stale_generation_without_mutation() {
    let (_dir, _vault, config, client, conversation, owner) = fixture();
    let connection = sql(&config);
    let before = receive_snapshot(&connection, conversation);
    assert!(matches!(
        client.begin_rcs_unlink_cutover_if_current(conversation, owner.ownership_generation + 1),
        Err(Error::InvalidRequest(_))
    ));
    assert_eq!(receive_snapshot(&connection, conversation), before);
    assert_eq!(
        client
            .begin_rcs_unlink_cutover_if_current(conversation, owner.ownership_generation)
            .unwrap()
            .owner,
        RcsReceiveOwner::Provider
    );
}

#[test]
fn owner_unlink_cutover_requires_current_durable_completion_and_is_replay_safe() {
    let (_dir, _vault, config, client, conversation, owner) = fixture();
    let connection = sql(&config);
    let observed_at_ms = now();
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            observed_at_ms,
            observed_at_ms,
        )
        .unwrap();
    let lifecycle_generation = lifecycle_generation(&config);
    let before = receive_snapshot(&connection, conversation);
    assert!(matches!(
        client.begin_rcs_owner_unlink_cutover(
            conversation,
            owner.ownership_generation,
            lifecycle_generation,
        ),
        Err(Error::InvalidRequest(_))
    ));
    assert_eq!(receive_snapshot(&connection, conversation), before);
    client
        .record_rcs_explicit_unlink_completed(observed_at_ms, observed_at_ms)
        .unwrap();
    let cutover = client
        .begin_rcs_owner_unlink_cutover(
            conversation,
            owner.ownership_generation,
            lifecycle_generation,
        )
        .unwrap();
    assert_eq!(cutover.owner, RcsReceiveOwner::Provider);
    let after = receive_snapshot(&connection, conversation);
    assert!(matches!(
        client.begin_rcs_owner_unlink_cutover(
            conversation,
            owner.ownership_generation,
            lifecycle_generation,
        ),
        Err(Error::InvalidRequest(_))
    ));
    assert_eq!(receive_snapshot(&connection, conversation), after);
}

#[test]
fn owner_unlink_cutover_is_fenced_by_lifecycle_and_preserves_outage_high_water() {
    let (_dir, _vault, config, client, conversation, owner) = fixture();
    client
        .record_rcs_provider_detector(
            conversation,
            owner.ownership_generation,
            RcsProviderDetector {
                provider_row_id: 9,
                provider_thread_id: 1,
                provider_type: 1,
                observed_at_ms: now(),
                observed_generation: 1,
            },
        )
        .unwrap();
    let lifecycle_generation = record_terminal_unlink(&client, &config);
    client
        .record_rcs_link_lifecycle(RcsCapabilityState::Linked, true, true, true, now(), now())
        .unwrap();
    let before = receive_snapshot(&sql(&config), conversation);
    assert!(matches!(
        client.begin_rcs_owner_unlink_cutover(
            conversation,
            owner.ownership_generation,
            lifecycle_generation,
        ),
        Err(Error::InvalidRequest(_))
    ));
    assert_eq!(receive_snapshot(&sql(&config), conversation), before);

    let lifecycle_generation = record_terminal_unlink(&client, &config);
    let cutover = client
        .begin_rcs_owner_unlink_cutover(
            conversation,
            owner.ownership_generation,
            lifecycle_generation,
        )
        .unwrap();
    assert_eq!(cutover.freeze_detector_floor, 9);
}

#[test]
fn terminal_lifecycle_publication_never_moves_link_owned_lanes() {
    for state in [
        RcsCapabilityState::Unlinked,
        RcsCapabilityState::SignedOut,
        RcsCapabilityState::NeedsRelink,
    ] {
        let (_dir, _vault, config, client, conversation, owner) = fixture();
        client
            .record_rcs_link_lifecycle(state, false, false, false, now(), now())
            .unwrap();
        let ownership: (String, u64) = sql(&config)
            .query_row(
                "SELECT owner,ownership_generation FROM rcs_lane_ownership WHERE conversation_id=?",
                [conversation.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(ownership, ("link".into(), owner.ownership_generation));
    }
}

fn install_echo_ledger(connection: &Connection, message_id: MessageId) {
    let command_id = CommandId::new();
    connection
        .execute(
            "INSERT INTO commands(command_id,message_id,gateway_device_id) VALUES(?,?,?)",
            params![
                command_id.to_string(),
                message_id.to_string(),
                DeviceId::new().to_string()
            ],
        )
        .unwrap();
    connection.execute("INSERT INTO rcs_send_ledger(command_id,tmp_id,mapping_revision,proof_digest,google_conversation_id,attempted_at_ms) VALUES(?,?,1,zeroblob(32),?,1)", params![command_id.to_string(), "tmp", "google-conversation"]).unwrap();
}

#[test]
fn matched_duplicate_page_never_moves_recovery_and_publication_failure_is_atomic() {
    let (_dir, vault, config, client, conversation, owner) = fixture();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("seed"))
        .unwrap();
    let message_id = only_message(&client, conversation)
        .payload
        .record
        .message_id;
    let connection = sql(&config);
    install_echo_ledger(&connection, message_id);
    let recovery_before = recovery_state(&connection, conversation);
    let mut echo = record("echo");
    echo.direction = Direction::Outgoing;
    echo.tmp_id = Some("tmp".into());
    echo.sent_at_ms = Some(now());
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, echo.clone())
            .unwrap(),
        RcsReceiveOutcome::OutgoingMatched
    );
    echo.rcs_reported = true;
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, echo)
            .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    assert_eq!(recovery_state(&connection, conversation), recovery_before);
    drop(client);
    let reopened = unlocked(&config, &vault);
    let before_unchanged_duplicate: i64 = connection
        .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
        .unwrap();
    let mut unchanged = record("echo");
    unchanged.direction = Direction::Outgoing;
    unchanged.tmp_id = Some("tmp".into());
    unchanged.sent_at_ms = Some(now());
    unchanged.rcs_reported = true;
    assert_eq!(
        reopened
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, unchanged)
            .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM outbox", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        before_unchanged_duplicate
    );
    assert_eq!(recovery_state(&connection, conversation), recovery_before);

    connection.execute_batch("CREATE TRIGGER reject_rcs_fact BEFORE INSERT ON outbox BEGIN SELECT RAISE(ABORT, 'reject'); END;").unwrap();
    let before = receive_snapshot(&connection, conversation);
    let mut failed = record("failed");
    failed.direction = Direction::Outgoing;
    failed.tmp_id = Some("tmp".into());
    failed.sent_at_ms = Some(now());
    failed.e2ee_reported = Some(true);
    assert!(
        reopened
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, failed)
            .is_err()
    );
    assert_eq!(receive_snapshot(&connection, conversation), before);
}

#[test]
fn projected_gateway_matrix_rejects_without_record_side_effects() {
    for mode in ["absent", "stale", "multiple", "other"] {
        let (_dir, _vault, config, client, conversation, owner) = fixture();
        let connection = sql(&config);
        match mode {
            "absent" => {
                connection
                    .execute("DELETE FROM rcs_link_status_projection", [])
                    .unwrap();
            }
            "stale" => {
                connection
                    .execute("UPDATE rcs_link_status_projection SET observed_at_ms=0", [])
                    .unwrap();
            }
            "multiple" => {
                connection
                    .execute(
                        "INSERT INTO rcs_link_status_projection VALUES(?,?,?,?,?,?,?,?,?)",
                        params![
                            DeviceId::new().to_string(),
                            "linked",
                            true,
                            true,
                            true,
                            now(),
                            1,
                            1,
                            1
                        ],
                    )
                    .unwrap();
            }
            "other" => {
                connection
                    .execute(
                        "UPDATE rcs_link_status_projection SET gateway_device_id=?",
                        [DeviceId::new().to_string()],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let before = receive_snapshot(&connection, conversation);
        assert_eq!(
            client
                .ingest_rcs_projected_record(conversation, owner.ownership_generation, record(mode))
                .unwrap(),
            RcsReceiveOutcome::Rejected(RcsReceiveCode::GatewayUnavailable)
        );
        let after = receive_snapshot(&connection, conversation);
        assert_eq!(after.0, before.0, "{mode}");
        assert_eq!(after.1, before.1, "{mode}");
        // The rejection deliberately publishes the content-free gateway alert,
        // so its outbox count is the only permitted change.
        assert_eq!(after.3, before.3, "{mode}");
        assert_eq!(
            client
                .rcs_receive_state(conversation)
                .unwrap()
                .unwrap()
                .alert,
            RcsReceiveAlert::GatewayUnavailable
        );
    }
    let (_dir, _vault, _config, client, conversation, owner) = fixture();
    assert_eq!(
        client
            .ingest_rcs_projected_record(
                conversation,
                owner.ownership_generation,
                record("positive")
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    // Reverting the unique_current_gateway guard in ingest fails each negative case.
}

#[test]
fn page_proof_time_rejects_equal_and_undated_pages_without_touching_live_records() {
    let (_dir, _vault, config, client, conversation, owner) = fixture();
    let connection = sql(&config);
    let proof_time: i64 = connection
        .query_row(
            "SELECT proof_time_ms FROM rcs_lane_ownership WHERE conversation_id=?",
            [conversation.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    for (id, sent_at_ms) in [("equal", Some(proof_time)), ("undated", None)] {
        let before = receive_snapshot(&connection, conversation);
        let mut page = record(id);
        page.sent_at_ms = sent_at_ms;
        assert_eq!(
            client
                .ingest_rcs_projected_observation(
                    conversation,
                    owner.ownership_generation,
                    page,
                    RcsObservationProvenance::Historical,
                )
                .unwrap(),
            RcsReceiveOutcome::Rejected(RcsReceiveCode::ProviderFloor)
        );
        assert_eq!(receive_snapshot(&connection, conversation), before);
    }
    let mut newer = record("newer");
    newer.sent_at_ms = Some(proof_time + 1);
    assert_eq!(
        client
            .ingest_rcs_projected_observation(
                conversation,
                owner.ownership_generation,
                newer,
                RcsObservationProvenance::Historical,
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    let live = record("live");
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, live)
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    assert!(
        client
            .messages(conversation)
            .unwrap()
            .iter()
            .any(|message| message.payload.body == "body-live" && !message.seen)
    );
}

#[test]
fn null_reservation_materializes_once_replays_and_resolves_immutable_status() {
    let (_dir, _vault, config, client, conversation, owner) = fixture();
    let connection = sql(&config);
    let reserved = "reserved";
    assert_eq!(
        client
            .record_rcs_link_identity(
                conversation,
                owner.ownership_generation,
                reserved,
                "google-conversation",
            )
            .unwrap(),
        RcsIdentityOutcome::Recorded
    );
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, record(reserved))
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    let reserved_id: String = connection
        .query_row(
            "SELECT message_id FROM rcs_message_identities WHERE google_message_id=?",
            [reserved],
            |r| r.get(0),
        )
        .unwrap();
    assert!(reserved_id.parse::<MessageId>().is_ok());
    assert_eq!(client.messages(conversation).unwrap().len(), 1);
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, record(reserved))
            .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    assert_eq!(
        client
            .apply_rcs_projected_status(
                conversation,
                owner.ownership_generation,
                status(reserved, None, RcsProjectedStatus::Sent)
            )
            .unwrap(),
        RcsReceiveOutcome::StatusApplied
    );
    assert_eq!(
        client
            .messages(conversation)
            .unwrap()
            .iter()
            .filter(|m| m.payload.record.message_id.to_string() == reserved_id)
            .count(),
        1
    );
    // Reverting reserved-identity materialization fails the Inserted/replay/status sequence.
}

#[test]
fn tmp_status_conflict_is_atomic_and_unchanged_status_keeps_new_association() {
    let (_dir, _vault, config, client, conversation, owner) = fixture();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("seed"))
        .unwrap();
    let message_id = only_message(&client, conversation)
        .payload
        .record
        .message_id;
    let connection = sql(&config);
    install_echo_ledger(&connection, message_id);
    let other = ConversationId::new();
    connection.execute("INSERT INTO rcs_message_identities(google_message_id,google_conversation_id,conversation_id,ownership_generation,message_id) VALUES(?,?,?,?,?)", params!["conflict", "google-conversation", other.to_string(), 1, message_id.to_string()]).unwrap();
    let before = receive_snapshot(&connection, conversation);
    assert_eq!(
        client
            .apply_rcs_projected_status(
                conversation,
                owner.ownership_generation,
                status("conflict", Some("tmp"), RcsProjectedStatus::Read)
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::IdentityConflict)
    );
    assert_eq!(receive_snapshot(&connection, conversation), before);
    assert_eq!(
        client
            .apply_rcs_projected_status(
                conversation,
                owner.ownership_generation,
                status("associated", Some("tmp"), RcsProjectedStatus::Sending)
            )
            .unwrap(),
        RcsReceiveOutcome::StatusApplied
    );
    assert_eq!(
        client
            .apply_rcs_projected_status(
                conversation,
                owner.ownership_generation,
                status("later-associated", Some("tmp"), RcsProjectedStatus::Sending)
            )
            .unwrap(),
        RcsReceiveOutcome::StatusUnchanged
    );
    drop(client);
    let reopened = open(&config);
    assert_eq!(
        reopened
            .apply_rcs_projected_status(
                conversation,
                owner.ownership_generation,
                status("later-associated", None, RcsProjectedStatus::Delivered)
            )
            .unwrap(),
        RcsReceiveOutcome::StatusApplied
    );
    // Reverting the IdentityConflict return or identity_recorded commit fails this test.
}

#[test]
fn tmp_status_rejects_an_immutable_identity_for_a_different_local_message() {
    let (_dir, _vault, config, client, conversation, owner) = fixture();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("first"))
        .unwrap();
    let first = only_message(&client, conversation)
        .payload
        .record
        .message_id;
    let mut second = record("second");
    second.google_message_id = "second-message".into();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, second)
        .unwrap();
    let second = client.messages(conversation).unwrap()[1]
        .payload
        .record
        .message_id;
    let connection = sql(&config);
    install_echo_ledger(&connection, second);
    let first = first.to_string();
    connection
        .execute(
            "INSERT INTO rcs_message_identities(google_message_id,google_conversation_id,conversation_id,ownership_generation,message_id) VALUES(?,?,?,?,?)",
            params!["immutable", "google-conversation", conversation.to_string(), owner.ownership_generation, first],
        )
        .unwrap();
    let before = fenced_snapshot(&client, &connection, conversation);
    assert_eq!(
        client
            .apply_rcs_projected_status(
                conversation,
                owner.ownership_generation,
                status("immutable", Some("tmp"), RcsProjectedStatus::Delivered),
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::IdentityConflict)
    );
    assert_eq!(fenced_snapshot(&client, &connection, conversation), before);
    drop(client);
    let reopened = open(&config);
    assert_eq!(
        fenced_snapshot(&reopened, &connection, conversation),
        before
    );
}

#[test]
fn projected_delivery_reconciles_an_unknown_attempt_once_without_resend() {
    let (_dir, vault, config, client, conversation, owner) = fixture();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("sent"))
        .unwrap();
    let message_id = only_message(&client, conversation)
        .payload
        .record
        .message_id;
    let connection = sql(&config);
    install_echo_ledger(&connection, message_id);
    let command_id: CommandId = connection
        .query_row(
            "SELECT command_id FROM rcs_send_ledger WHERE tmp_id='tmp'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
        .parse()
        .unwrap();
    connection
        .execute(
            "INSERT INTO command_ledger(command_id,key_epoch,subscription_id,payload) VALUES(?,1,'','{}')",
            [command_id.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO attempts(command_id,state,attempted_at_ms,finalized_transport) VALUES(?,'outcome_unknown',1,'rcs')",
            [command_id.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO rcs_message_identities(google_message_id,google_conversation_id,conversation_id,ownership_generation,message_id) VALUES(?,?,?,?,?)",
            params!["delivery", "google-conversation", conversation.to_string(), owner.ownership_generation, message_id.to_string()],
        )
        .unwrap();
    assert_eq!(
        client
            .apply_rcs_projected_status(
                conversation,
                owner.ownership_generation,
                status("delivery", None, RcsProjectedStatus::Delivered),
            )
            .unwrap(),
        RcsReceiveOutcome::StatusApplied
    );
    let before = outbox_count(&config);
    drop(client);
    let reopened = unlocked(&config, &vault);
    let delivered = status("delivery", Some("tmp"), RcsProjectedStatus::Delivered);
    assert_eq!(
        reopened
            .apply_rcs_projected_status(conversation, owner.ownership_generation, delivered.clone())
            .unwrap(),
        RcsReceiveOutcome::StatusUnchanged
    );
    assert_eq!(
        reopened
            .apply_rcs_projected_status(conversation, owner.ownership_generation, delivered)
            .unwrap(),
        RcsReceiveOutcome::StatusUnchanged
    );
    assert_eq!(
        reopened
            .apply_rcs_projected_status(
                conversation,
                owner.ownership_generation,
                status("delivery", Some("tmp"), RcsProjectedStatus::Sent),
            )
            .unwrap(),
        RcsReceiveOutcome::StatusUnchanged
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT state FROM attempts WHERE command_id=?",
                [command_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "delivered"
    );
    assert_eq!(outbox_count(&config), before + 1);
    assert_eq!(
        reopened.resend_rcs_outcome_unknown(command_id),
        Err(Error::AlreadyAttempted(SendState::Delivered))
    );
}

#[test]
fn reported_facts_converge_for_outgoing_match_and_immutable_duplicate() {
    for first_reported in [false, true] {
        let (directory, vault, client_config, client, conversation, owner) = fixture();
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("seed"))
            .unwrap();
        let message_id = only_message(&client, conversation)
            .payload
            .record
            .message_id;
        let connection = sql(&client_config);
        install_echo_ledger(&connection, message_id);
        let mut echo = record("immutable");
        echo.direction = Direction::Outgoing;
        echo.tmp_id = Some("tmp".into());
        echo.rcs_reported = first_reported;
        echo.e2ee_reported = Some(first_reported);
        let pre_match_send_state = only_message(&client, conversation).send_state;
        assert_eq!(
            client
                .ingest_rcs_projected_record(conversation, owner.ownership_generation, echo.clone())
                .unwrap(),
            RcsReceiveOutcome::OutgoingMatched
        );
        assert_eq!(
            client.rcs_message_reported(message_id).unwrap(),
            Some(first_reported)
        );
        assert_eq!(
            client
                .rcs_conversation_metadata(conversation)
                .unwrap()
                .unwrap()
                .rcs_encryption,
            Some(first_reported)
        );
        assert_eq!(
            only_message(&client, conversation).send_state,
            pre_match_send_state
        );
        let peer_config = config(&directory, "peer", &vault);
        let peer = unlocked(&peer_config, &vault);
        let mut server = Server::default();
        server.upload(&client);
        server.sync(&peer);
        assert_eq!(
            peer.rcs_message_reported(message_id).unwrap(),
            Some(first_reported)
        );
        assert_eq!(
            peer.rcs_conversation_metadata(conversation)
                .unwrap()
                .unwrap()
                .rcs_encryption,
            Some(first_reported)
        );
        let expected_send_state = pre_match_send_state;
        let mut tmp_duplicate = echo.clone();
        tmp_duplicate.rcs_reported = !first_reported;
        tmp_duplicate.e2ee_reported = Some(!first_reported);
        assert_eq!(
            client
                .ingest_rcs_projected_record(
                    conversation,
                    owner.ownership_generation,
                    tmp_duplicate,
                )
                .unwrap(),
            RcsReceiveOutcome::Duplicate
        );
        assert_eq!(
            client
                .rcs_conversation_metadata(conversation)
                .unwrap()
                .unwrap()
                .rcs_encryption,
            Some(!first_reported)
        );
        echo.tmp_id = None;
        echo.rcs_reported = first_reported;
        echo.e2ee_reported = Some(first_reported);
        assert_eq!(
            client
                .ingest_rcs_projected_record(conversation, owner.ownership_generation, echo)
                .unwrap(),
            RcsReceiveOutcome::Duplicate
        );
        assert_eq!(client.rcs_message_status(message_id).unwrap(), None);
        assert_eq!(
            only_message(&client, conversation).send_state,
            expected_send_state
        );
        assert_eq!(
            client
                .rcs_conversation_metadata(conversation)
                .unwrap()
                .unwrap()
                .rcs_encryption,
            Some(first_reported)
        );
        server.upload(&client);
        server.sync(&peer);
        assert_eq!(client.rcs_message_reported(message_id).unwrap(), Some(true));
        assert_eq!(peer.rcs_message_reported(message_id).unwrap(), Some(true));
        assert_eq!(
            peer.rcs_conversation_metadata(conversation)
                .unwrap()
                .unwrap()
                .rcs_encryption,
            Some(first_reported)
        );
    }
    // Reverting apply_reported_facts from either outgoing match path fails local/peer convergence.
}

#[test]
fn overflow_gap_replicates_and_e2ee_does_not_clear_it() {
    let (directory, vault, _config, client, conversation, owner) = fixture();
    for provider_row_id in 0..=1_024 {
        client
            .record_rcs_provider_detector(
                conversation,
                owner.ownership_generation,
                RcsProviderDetector {
                    provider_row_id,
                    provider_thread_id: 1,
                    provider_type: 1,
                    observed_at_ms: provider_row_id,
                    observed_generation: owner.ownership_generation,
                },
            )
            .unwrap();
    }
    assert_eq!(
        client
            .rcs_receive_state(conversation)
            .unwrap()
            .unwrap()
            .alert,
        RcsReceiveAlert::DetectorOverflow
    );
    let peer_config = config(&directory, "peer", &vault);
    let peer = unlocked(&peer_config, &vault);
    let mut server = Server::default();
    server.upload(&client);
    server.sync(&peer);
    assert!(
        peer.rcs_conversation_metadata(conversation)
            .unwrap()
            .unwrap()
            .rcs_history_gap
    );
    let mut e2ee = record("e2ee");
    e2ee.e2ee_reported = Some(true);
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, e2ee)
        .unwrap();
    server.upload(&client);
    server.sync(&peer);
    assert!(
        client
            .rcs_conversation_metadata(conversation)
            .unwrap()
            .unwrap()
            .rcs_history_gap
    );
    assert!(
        peer.rcs_conversation_metadata(conversation)
            .unwrap()
            .unwrap()
            .rcs_history_gap
    );
    // Reverting history_gap(state.alert) in apply_reported_facts fails both final assertions.
}

#[test]
fn overflow_publication_failure_rolls_back_alert_detector_and_outbox() {
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    for provider_row_id in 0..1_024 {
        client
            .record_rcs_provider_detector(
                conversation,
                owner.ownership_generation,
                RcsProviderDetector {
                    provider_row_id,
                    provider_thread_id: 1,
                    provider_type: 1,
                    observed_at_ms: provider_row_id,
                    observed_generation: owner.ownership_generation,
                },
            )
            .unwrap();
    }
    let connection = sql(&config);
    let before = receive_snapshot(&connection, conversation);
    connection
        .execute_batch(
            "CREATE TRIGGER reject_overflow_metadata BEFORE INSERT ON outbox BEGIN SELECT RAISE(ABORT, 'reject'); END;",
        )
        .unwrap();
    assert!(
        client
            .record_rcs_provider_detector(
                conversation,
                owner.ownership_generation,
                RcsProviderDetector {
                    provider_row_id: 1_024,
                    provider_thread_id: 1,
                    provider_type: 1,
                    observed_at_ms: 1_024,
                    observed_generation: owner.ownership_generation,
                },
            )
            .is_err()
    );
    assert_eq!(receive_snapshot(&connection, conversation), before);
    assert_eq!(
        client
            .rcs_receive_state(conversation)
            .unwrap()
            .unwrap()
            .alert,
        RcsReceiveAlert::None
    );
    // Reverting the overflow branch's transaction rollback leaves the alert/high-water partial.
}

#[test]
fn serialized_fact_only_payload_contains_no_provider_identity() {
    let message_id = MessageId::new();
    let opened = PrivatePayload::RcsMessageReported {
        message_id,
        rcs_reported: true,
    };
    let encoded = serde_json::to_value(&opened).unwrap();
    assert_eq!(encoded["kind"], "rcs_message_reported");
    assert_eq!(encoded["message_id"], message_id.to_string());
    assert_eq!(encoded.as_object().unwrap().len(), 3);
    assert!(encoded.get("google_message_id").is_none());
    assert!(encoded.get("google_conversation_id").is_none());
    assert!(encoded.get("tmp_id").is_none());
}

#[test]
fn known_rcs_payload_variants_apply_and_malformed_variants_do_not_deserialize() {
    let (directory, vault, _config, client, conversation, owner) = fixture();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("valid"))
        .unwrap();
    client
        .apply_rcs_projected_status(
            conversation,
            owner.ownership_generation,
            status("valid", None, RcsProjectedStatus::Sent),
        )
        .unwrap();
    let message_id = only_message(&client, conversation)
        .payload
        .record
        .message_id;
    let peer = unlocked(&config(&directory, "valid-peer", &vault), &vault);
    let mut server = Server::default();
    server.upload(&client);
    assert_eq!(server.sync(&peer).quarantined, 0);
    assert_eq!(peer.rcs_message_reported(message_id).unwrap(), Some(true));
    assert_eq!(
        peer.rcs_message_status(message_id).unwrap().unwrap().status,
        RcsMessageStatus::Sent
    );

    for payload in [
        serde_json::json!({"kind":"rcs_message_reported","message_id":7}),
        serde_json::json!({"kind":"rcs_message_status","message_id":7}),
    ] {
        assert!(serde_json::from_value::<PrivatePayload>(payload).is_err());
    }
    assert!(serde_json::from_str::<PrivatePayload>(r#"{"kind":"future_rcs_fact"}"#).is_err());
}

#[derive(Clone, Copy, Debug)]
enum Denial {
    CurrentOther,
    StaleGeneration,
    ProviderOwned,
    Unproven,
}

const DENIALS: [Denial; 4] = [
    Denial::CurrentOther,
    Denial::StaleGeneration,
    Denial::ProviderOwned,
    Denial::Unproven,
];

impl Denial {
    fn expected(self) -> RcsReceiveCode {
        match self {
            Self::CurrentOther => RcsReceiveCode::GatewayUnavailable,
            Self::StaleGeneration | Self::ProviderOwned => RcsReceiveCode::OwnershipStale,
            Self::Unproven => RcsReceiveCode::ConversationUnproven,
        }
    }
}

struct Scene {
    _directory: tempfile::TempDir,
    _vault: Vault,
    config: ClientConfig,
    client: Client,
    conversation: ConversationId,
    generation: u64,
}

fn denied_scene(denial: Denial) -> Scene {
    let (directory, vault, config, client, mut conversation, owner) = fixture();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("seed"))
        .unwrap();
    let connection = sql(&config);
    match denial {
        Denial::CurrentOther => {
            connection
                .execute(
                    "UPDATE rcs_link_status_projection SET gateway_device_id=?",
                    [DeviceId::new().to_string()],
                )
                .unwrap();
        }
        Denial::StaleGeneration => {
            client.begin_rcs_link_ownership(conversation, 0).unwrap();
        }
        Denial::ProviderOwned => {
            client.begin_rcs_unlink_cutover(conversation).unwrap();
        }
        Denial::Unproven => {
            conversation = ConversationId::new();
        }
    }
    Scene {
        _directory: directory,
        _vault: vault,
        config,
        client,
        conversation,
        generation: owner.ownership_generation,
    }
}

#[derive(Debug, PartialEq)]
struct FencedSnapshot {
    messages: i64,
    identities: i64,
    outbox: i64,
    detectors: i64,
    high_water: Option<i64>,
    recovery: Option<String>,
    alert: Option<String>,
    statuses: Vec<(String, i64, i64)>,
    send_states: Vec<Option<SendState>>,
    metadata: Option<(Option<bool>, bool)>,
}

fn fenced_snapshot(
    client: &Client,
    connection: &Connection,
    conversation: ConversationId,
) -> FencedSnapshot {
    let key = conversation.to_string();
    let (messages, identities, outbox, detectors, high_water, recovery, alert): (
        i64,
        i64,
        i64,
        i64,
        Option<i64>,
        Option<String>,
        Option<String>,
    ) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM messages), (SELECT COUNT(*) FROM rcs_message_identities), (SELECT COUNT(*) FROM outbox), (SELECT COUNT(*) FROM rcs_provider_detectors WHERE conversation_id=?1), (SELECT observed_detector_high_water FROM rcs_lane_ownership WHERE conversation_id=?1), (SELECT v FROM metadata WHERE k='rcs_recovery:' || ?1), (SELECT alert_code FROM rcs_lane_ownership WHERE conversation_id=?1)",
            [key.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .unwrap();
    let mut statement = connection
        .prepare("SELECT message_id,status_rank,rcs_reported FROM rcs_message_status ORDER BY message_id")
        .unwrap();
    let statuses = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let send_states = client
        .messages(conversation)
        .unwrap()
        .into_iter()
        .map(|message| message.send_state)
        .collect();
    FencedSnapshot {
        messages,
        identities,
        outbox,
        detectors,
        high_water,
        recovery,
        alert,
        statuses,
        send_states,
        metadata: client
            .rcs_conversation_metadata(conversation)
            .unwrap()
            .map(|metadata| (metadata.rcs_encryption, metadata.rcs_history_gap)),
    }
}

fn detector(provider_row_id: i64, generation: u64) -> RcsProviderDetector {
    RcsProviderDetector {
        provider_row_id,
        provider_thread_id: 1,
        provider_type: 1,
        observed_at_ms: provider_row_id,
        observed_generation: generation,
    }
}

fn publish_gateway_state(
    client: &Client,
    config: &ClientConfig,
    state: RcsCapabilityState,
    availability: bool,
    capability_revision: u64,
) {
    let observed_at_ms = now();
    client
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: config.device_id,
                state,
                availability,
                health: true,
                phone_responsive: true,
                observed_at_ms,
                capability_revision,
                feature_bits: 1,
                linked_since_ms: Some(observed_at_ms),
            },
            &[],
            observed_at_ms,
        )
        .unwrap();
}

fn metadata_flags(client: &Client, conversation: ConversationId) -> (Option<bool>, bool) {
    let metadata = client
        .rcs_conversation_metadata(conversation)
        .unwrap()
        .unwrap();
    (metadata.rcs_encryption, metadata.rcs_history_gap)
}

#[test]
fn fenced_detector_rejects_gateway_and_ownership_denials_without_writes() {
    for denial in DENIALS {
        let scene = denied_scene(denial);
        let connection = sql(&scene.config);
        let before = fenced_snapshot(&scene.client, &connection, scene.conversation);
        assert_eq!(
            scene
                .client
                .record_rcs_provider_detector(
                    scene.conversation,
                    scene.generation,
                    detector(77, scene.generation),
                )
                .unwrap(),
            RcsIdentityOutcome::Rejected(denial.expected()),
            "{denial:?}"
        );
        assert_eq!(
            fenced_snapshot(&scene.client, &connection, scene.conversation),
            before,
            "{denial:?}"
        );
    }
    // Positive control: the local gateway admits the detector.
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    let connection = sql(&config);
    let before = fenced_snapshot(&client, &connection, conversation);
    assert_eq!(
        client
            .record_rcs_provider_detector(
                conversation,
                owner.ownership_generation,
                detector(77, owner.ownership_generation),
            )
            .unwrap(),
        RcsIdentityOutcome::Recorded
    );
    let after = fenced_snapshot(&client, &connection, conversation);
    assert_eq!(after.detectors, before.detectors + 1);
    // Reverting the ownership-generation or local-gateway checks fails the loop above.
}

#[test]
fn outage_detectors_extend_the_cutover_floor_and_raise_the_ambiguous_window_alert() {
    let (_directory, vault, config, client, conversation, owner) = fixture();
    let states = [
        (RcsCapabilityState::SignedOut, true, true),
        (RcsCapabilityState::NeedsRelink, true, true),
        (RcsCapabilityState::Unlinked, true, true),
        (RcsCapabilityState::Unavailable, false, false),
        (RcsCapabilityState::Linked, false, true),
    ];
    for (index, (state, availability, health)) in states.into_iter().enumerate() {
        let observed_at_ms = now();
        client
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    gateway_device_id: config.device_id,
                    state,
                    availability,
                    health,
                    phone_responsive: health,
                    observed_at_ms,
                    capability_revision: u64::try_from(index + 2).unwrap(),
                    feature_bits: 0,
                    linked_since_ms: None,
                },
                &[],
                observed_at_ms,
            )
            .unwrap();
        assert_eq!(
            client
                .record_rcs_provider_detector(
                    conversation,
                    owner.ownership_generation,
                    detector(
                        i64::try_from(index + 1).unwrap(),
                        owner.ownership_generation
                    ),
                )
                .unwrap(),
            RcsIdentityOutcome::Recorded
        );
    }
    let cutover = client.begin_rcs_unlink_cutover(conversation).unwrap();
    assert_eq!(cutover.freeze_detector_floor, 5);
    assert!(cutover.ambiguous_detector_alert);
    drop(client);
    let reopened = open(&config);
    let state = reopened.rcs_receive_state(conversation).unwrap().unwrap();
    assert_eq!(state.detector_floor, 5);
    assert_eq!(state.alert, RcsReceiveAlert::AmbiguousProviderWindow);
    drop(reopened);
    drop(vault);
}

#[test]
fn fenced_status_rejects_gateway_and_ownership_denials_without_writes() {
    for denial in DENIALS {
        let scene = denied_scene(denial);
        let connection = sql(&scene.config);
        let before = fenced_snapshot(&scene.client, &connection, scene.conversation);
        assert_eq!(
            scene
                .client
                .apply_rcs_projected_status(
                    scene.conversation,
                    scene.generation,
                    status("seed", None, RcsProjectedStatus::Sent),
                )
                .unwrap(),
            RcsReceiveOutcome::Rejected(denial.expected()),
            "{denial:?}"
        );
        assert_eq!(
            fenced_snapshot(&scene.client, &connection, scene.conversation),
            before,
            "{denial:?}"
        );
    }
    // Positive control: a known message's status applies under the unique current gateway (guard: unique_current_gateway in apply_rcs_projected_status, 94109b0f).
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("seed"))
        .unwrap();
    let message_id = only_message(&client, conversation)
        .payload
        .record
        .message_id;
    let connection = sql(&config);
    let before = fenced_snapshot(&client, &connection, conversation);
    assert_eq!(
        client
            .apply_rcs_projected_status(
                conversation,
                owner.ownership_generation,
                status("seed", None, RcsProjectedStatus::Sent),
            )
            .unwrap(),
        RcsReceiveOutcome::StatusApplied
    );
    assert!(client.rcs_message_status(message_id).unwrap().is_some());
    assert_ne!(fenced_snapshot(&client, &connection, conversation), before);
    // Reverting the status gate fails the denial loop: the status row and outbox would change.
}

#[test]
fn fenced_recovery_progress_rejects_gateway_and_ownership_denials_without_writes() {
    for denial in DENIALS {
        let scene = denied_scene(denial);
        let connection = sql(&scene.config);
        let before = fenced_snapshot(&scene.client, &connection, scene.conversation);
        assert_eq!(
            scene
                .client
                .record_rcs_recovery_progress(
                    scene.conversation,
                    scene.generation,
                    10,
                    "fenced-token"
                )
                .unwrap(),
            RcsRecoveryOutcome::Rejected(denial.expected()),
            "{denial:?}"
        );
        assert_eq!(
            fenced_snapshot(&scene.client, &connection, scene.conversation),
            before,
            "{denial:?}"
        );
    }
    // Positive control: the unique current gateway records progress (guard: unique_current_gateway in update_rcs_recovery).
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    assert_eq!(
        client
            .record_rcs_recovery_progress(
                conversation,
                owner.ownership_generation,
                10,
                "fenced-token"
            )
            .unwrap(),
        RcsRecoveryOutcome::Recorded
    );
    let connection = sql(&config);
    let proof = proof_time(&connection, conversation);
    assert_eq!(
        recovery_state(&connection, conversation),
        serde_json::json!({
            "v": 1,
            "watermark_ms": proof,
            "resume_hint": {"target_ms": 10, "cursor": "fenced-token"},
            "incomplete": true
        })
    );
}

#[test]
fn gateway_withdrawal_publishes_history_gap_to_peer_and_relink_clears_it() {
    let (directory, vault, client_config, client, conversation, owner) = fixture();
    let mut encrypted = record("encrypted");
    encrypted.e2ee_reported = Some(true);
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, encrypted)
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    assert_eq!(metadata_flags(&client, conversation), (Some(true), false));
    let peer_config = config(&directory, "peer", &vault);
    let peer = unlocked(&peer_config, &vault);
    let mut server = Server::default();
    server.upload(&client);
    server.sync(&peer);
    assert_eq!(metadata_flags(&peer, conversation), (Some(true), false));

    publish_gateway_state(
        &client,
        &client_config,
        RcsCapabilityState::Unlinked,
        false,
        2,
    );
    assert_eq!(
        client
            .ingest_rcs_projected_record(
                conversation,
                owner.ownership_generation,
                record("withdrawn")
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::GatewayUnavailable)
    );
    assert_eq!(
        client
            .rcs_receive_state(conversation)
            .unwrap()
            .unwrap()
            .alert,
        RcsReceiveAlert::GatewayUnavailable
    );
    // Guard: publish_metadata(&tx, &ctx, conversation_id, None, true) in the gateway-unavailable ingest branch (94109b0f).
    assert_eq!(metadata_flags(&client, conversation), (Some(true), true));
    server.upload(&client);
    server.sync(&peer);
    assert_eq!(metadata_flags(&peer, conversation), (Some(true), true));

    publish_gateway_state(&client, &client_config, RcsCapabilityState::Linked, true, 3);
    let relinked = client.begin_rcs_link_ownership(conversation, 0).unwrap();
    assert_eq!(metadata_flags(&client, conversation), (Some(true), false));
    assert_eq!(
        client
            .ingest_rcs_projected_record(
                conversation,
                relinked.ownership_generation,
                record("after-relink")
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    server.upload(&client);
    server.sync(&peer);
    assert_eq!(metadata_flags(&peer, conversation), (Some(true), false));
}

#[test]
fn gateway_unavailable_alert_and_metadata_roll_back_together_on_publication_failure() {
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    let mut encrypted = record("encrypted");
    encrypted.e2ee_reported = Some(true);
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, encrypted)
        .unwrap();
    publish_gateway_state(&client, &config, RcsCapabilityState::Unlinked, false, 2);
    let connection = sql(&config);
    let before = fenced_snapshot(&client, &connection, conversation);
    assert_eq!(before.alert, None);
    assert_eq!(before.metadata, Some((Some(true), false)));
    connection
        .execute_batch(
            "CREATE TRIGGER reject_gateway_metadata BEFORE INSERT ON outbox BEGIN SELECT RAISE(ABORT, 'reject'); END;",
        )
        .unwrap();
    assert!(
        client
            .ingest_rcs_projected_record(
                conversation,
                owner.ownership_generation,
                record("withdrawn")
            )
            .is_err()
    );
    // Guard: the alert UPDATE and publish_metadata share one transaction in the gateway-unavailable branch (94109b0f); both must roll back.
    assert_eq!(fenced_snapshot(&client, &connection, conversation), before);
    assert_eq!(
        client
            .rcs_receive_state(conversation)
            .unwrap()
            .unwrap()
            .alert,
        RcsReceiveAlert::None
    );
    connection
        .execute_batch("DROP TRIGGER reject_gateway_metadata;")
        .unwrap();
    assert_eq!(
        client
            .ingest_rcs_projected_record(
                conversation,
                owner.ownership_generation,
                record("withdrawn")
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::GatewayUnavailable)
    );
    assert_eq!(metadata_flags(&client, conversation), (Some(true), true));
}

fn mapping_observation(observed_at_ms: i64) -> RcsMappingObservation {
    let outgoing = NativeRcsOutgoing {
        outgoing_id: "google-outgoing".into(),
        sim_participant_id: "google-outgoing".into(),
        own_identity_raw: "+12025550100".into(),
        sim_payload: NativeRcsSimPayload {
            two: 2,
            sim_number: 7,
        },
        rcs_chats_enabled: true,
    };
    RcsMappingObservation {
        google_conversation_id: "google-conversation".into(),
        google_candidate_count: 1,
        participants: vec![RcsParticipantObservation {
            identity: "raw-peer".into(),
            google_participant_id: "google-peer".into(),
            identifier_type: 1,
            is_visible: true,
            is_me: false,
        }],
        conversation_type: RcsConversationType::Rcs,
        status: "eligible".into(),
        send_mode: RcsSendMode::Automatic,
        read_only: false,
        default_outgoing: true,
        selected_sim_id: "android-subscription-7".into(),
        selected_sim_own_identity: "+12025550100".into(),
        carrier_binding: NativeCarrierRouteBinding {
            carrier_route_id: "android-subscription-7".into(),
            android_subscription_id: 7,
            is_current_default_sms: true,
            own_number_raw: "+12025550100".into(),
            own_number_source: NativeOwnNumberSource::SubscriptionManagerPhoneNumber,
            route_observed_at_ms: observed_at_ms,
            route_context_revision: 1,
            same_handset_evidence_id: "verified-handset".into(),
        },
        outgoing: outgoing.clone(),
        native_evidence: NativeRcsEvidence {
            lifetime: NativeRcsLifetime {
                runtime_id: "runtime".into(),
                run_generation: 1,
                link_generation: 4,
                session_generation: 7,
                context_revision: 1,
            },
            lookup_id: "lookup".into(),
            lookup_complete: true,
            google_candidate_count: 1,
            settings_revision: 1,
            settings_provenance: NativeSettingsProvenance::CurrentEvent,
            settings_observed_at_ms: observed_at_ms,
            settings_stream_epoch: 1,
            settings_currentness: NativeSettingsCurrentness::ContinuousVerifiedFeed,
            current_settings_sims: vec![outgoing],
            settings_validated_at_ms: observed_at_ms,
            conversation_observed_at_ms: observed_at_ms,
        },
        identity_region: "ZZ".into(),
        observed_at_ms,
        link_generation: 4,
        session_generation: 7,
        context_revision: 1,
        canonicalizer_revision: 1,
        mapping_revision: 1,
        link_state: RcsLinkState::Linked,
    }
}

fn mapped_fixture() -> (
    tempfile::TempDir,
    Vault,
    ClientConfig,
    Client,
    ConversationId,
    RcsOwnership,
    RcsContextExpectation,
) {
    let (directory, vault, config, client, conversation, ownership) = fixture();
    client
        .capture_incoming(IncomingSms {
            conversation_id: Some(conversation),
            sender_address: "raw-peer".into(),
            body: String::new(),
            provider_message_id: Some("mapping-participant".into()),
            imported: true,
        })
        .unwrap();
    let observed_at_ms = now();
    assert!(matches!(
        client
            .verify_rcs_mapping(&mapping_observation(observed_at_ms), observed_at_ms)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    let ownership_generation = ownership.ownership_generation;
    (
        directory,
        vault,
        config,
        client,
        conversation,
        ownership,
        RcsContextExpectation {
            ownership_generation,
            link_generation: 4,
            session_generation: 7,
            context_revision: 1,
        },
    )
}

/// A reply is addressed from the newest message of a thread, so a chat message must name the
/// other party the way an SMS does.
#[test]
fn a_received_chat_message_names_the_chats_participant_as_its_sender() {
    let (_dir, _vault, _config, client, conversation, ownership, _) = mapped_fixture();

    let mut received = record("in");
    received.sent_at_ms = Some(now());

    client
        .ingest_rcs_projected_record(conversation, ownership.ownership_generation, received)
        .unwrap();

    let newest = client
        .latest_message(conversation)
        .unwrap()
        .unwrap()
        .payload;
    assert_eq!(newest.transport, Transport::Rcs);
    assert_eq!(newest.sender_address.as_deref(), Some("raw-peer"));
}

#[test]
fn a_message_arriving_after_the_chats_proof_was_withdrawn_is_not_stored() {
    let (_dir, _vault, _config, client, conversation, ownership, _) = mapped_fixture();
    let observed_at_ms = now();
    let mut contradicted = mapping_observation(observed_at_ms);
    contradicted.google_candidate_count = 0;
    contradicted.native_evidence.google_candidate_count = 0;
    assert!(matches!(
        client
            .verify_rcs_mapping(&contradicted, observed_at_ms)
            .unwrap(),
        RcsMappingOutcome::Refused(_)
    ));
    let mut received = record("in");
    received.sent_at_ms = Some(now());

    let outcome = client
        .ingest_rcs_projected_record(conversation, ownership.ownership_generation, received)
        .unwrap();

    assert_eq!(
        outcome,
        RcsReceiveOutcome::Rejected(RcsReceiveCode::ConversationUnproven)
    );
    let newest = client
        .latest_message(conversation)
        .unwrap()
        .unwrap()
        .payload;
    assert_eq!(newest.transport, Transport::Sms);
}

#[test]
fn a_chat_message_sent_outside_peppy_is_addressed_to_the_chats_participant() {
    let (_dir, _vault, _config, client, conversation, ownership, _) = mapped_fixture();
    let mut sent_elsewhere = record("out");
    sent_elsewhere.direction = Direction::Outgoing;
    sent_elsewhere.sent_at_ms = Some(now());

    client
        .ingest_rcs_projected_record(conversation, ownership.ownership_generation, sent_elsewhere)
        .unwrap();

    let newest = client
        .latest_message(conversation)
        .unwrap()
        .unwrap()
        .payload;
    assert_eq!(newest.recipients, vec!["raw-peer".to_owned()]);
    assert_eq!(newest.sender_address, None);
}

#[derive(Debug, PartialEq)]
struct ContextInvalidationSnapshot {
    ownership: (String, u64, Option<String>, i64, i64, i64, Option<String>),
    context: Option<String>,
    mappings: Vec<(String, String, Option<i64>)>,
    outbox: i64,
}

fn context_invalidation_snapshot(
    connection: &Connection,
    conversation: ConversationId,
) -> ContextInvalidationSnapshot {
    let key = conversation.to_string();
    let ownership = connection
        .query_row(
            "SELECT owner,ownership_generation,history_token,detector_floor,proof_time_ms,observed_detector_high_water,alert_code FROM rcs_lane_ownership WHERE conversation_id=?",
            [key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        )
        .unwrap();
    let context = connection
        .query_row(
            "SELECT v FROM metadata WHERE k='rcs_mapping_context_v1'",
            [],
            |row| row.get(0),
        )
        .ok();
    let mut statement = connection
        .prepare("SELECT peppy_conversation_id,google_conversation_id,invalidated_at_ms FROM rcs_conversation_mappings ORDER BY peppy_conversation_id,google_conversation_id")
        .unwrap();
    let mappings = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    ContextInvalidationSnapshot {
        ownership,
        context,
        mappings,
        outbox: connection
            .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
            .unwrap(),
    }
}

fn replace_context_field(connection: &Connection, field: &str, value: u64) {
    let encoded: String = connection
        .query_row(
            "SELECT v FROM metadata WHERE k='rcs_mapping_context_v1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut context: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    context[field] = serde_json::Value::from(value);
    connection
        .execute(
            "UPDATE metadata SET v=? WHERE k='rcs_mapping_context_v1'",
            [serde_json::to_string(&context).unwrap()],
        )
        .unwrap();
}

#[test]
fn google_event_invalidation_bumps_once_and_replay_is_write_free() {
    let (_directory, _vault, config, client, conversation, _ownership, expected) = mapped_fixture();
    let observed_at_ms = now();
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Linked,
            true,
            true,
            true,
            observed_at_ms,
            observed_at_ms,
        )
        .unwrap();
    let connection = sql(&config);
    let before_outbox = outbox_count(&config);
    assert_eq!(
        client
            .invalidate_rcs_context_for_google_event(
                conversation,
                "google-conversation",
                expected,
                None
            )
            .unwrap(),
        Some(2)
    );
    let after = context_invalidation_snapshot(&connection, conversation);
    assert_eq!(after.outbox, before_outbox + 1);
    assert_eq!(
        client
            .rcs_eligibility_projection(conversation, config.device_id, now())
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::NotEligible
    );
    assert!(after.mappings.iter().all(|mapping| mapping.2.is_some()));
    let context: serde_json::Value =
        serde_json::from_str(after.context.as_deref().unwrap()).unwrap();
    assert_eq!(context["context_revision"].as_u64(), Some(2));
    assert!(matches!(
        client.invalidate_rcs_context_for_google_event(
            conversation,
            "google-conversation",
            expected,
            None,
        ),
        Err(Error::InvalidRequest(_))
    ));
    assert_eq!(
        context_invalidation_snapshot(&connection, conversation),
        after
    );
}

#[test]
fn google_event_invalidation_rejects_each_stale_context_field_without_writes() {
    for field in ["link_generation", "session_generation", "context_revision"] {
        let (_directory, _vault, config, client, conversation, _ownership, expected) =
            mapped_fixture();
        let connection = sql(&config);
        replace_context_field(&connection, field, 99);
        let before = context_invalidation_snapshot(&connection, conversation);
        assert!(matches!(
            client.invalidate_rcs_context_for_google_event(
                conversation,
                "google-conversation",
                expected,
                None,
            ),
            Err(Error::InvalidRequest(_))
        ));
        assert_eq!(
            context_invalidation_snapshot(&connection, conversation),
            before,
            "{field}"
        );
    }
}

#[test]
fn google_event_invalidation_rejects_stale_ownership_mapping_and_cutover_without_writes() {
    for scenario in ["ownership", "mapping", "deleted", "cutover"] {
        let (_directory, _vault, config, client, conversation, ownership, expected) =
            mapped_fixture();
        let connection = sql(&config);
        match scenario {
            "ownership" => {
                client.begin_rcs_link_ownership(conversation, 0).unwrap();
            }
            "mapping" => {
                connection
                    .execute(
                        "UPDATE rcs_conversation_mappings SET google_conversation_id='other-google' WHERE peppy_conversation_id=?",
                        [conversation.to_string()],
                    )
                    .unwrap();
            }
            "deleted" => {
                connection
                    .execute(
                        "DELETE FROM rcs_mapping_proofs WHERE peppy_conversation_id=?",
                        [conversation.to_string()],
                    )
                    .unwrap();
                connection
                    .execute(
                        "DELETE FROM rcs_conversation_mappings WHERE peppy_conversation_id=?",
                        [conversation.to_string()],
                    )
                    .unwrap();
            }
            "cutover" => {
                client
                    .begin_rcs_unlink_cutover_if_current(
                        conversation,
                        ownership.ownership_generation,
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let before = context_invalidation_snapshot(&connection, conversation);
        assert!(matches!(
            client.invalidate_rcs_context_for_google_event(
                conversation,
                "google-conversation",
                expected,
                None,
            ),
            Err(Error::InvalidRequest(_))
        ));
        assert_eq!(
            context_invalidation_snapshot(&connection, conversation),
            before,
            "{scenario}"
        );
    }
}

#[test]
fn google_event_invalidation_rejects_revision_overflow_without_writes() {
    let (_directory, _vault, config, client, conversation, ownership, mut expected) =
        mapped_fixture();
    let connection = sql(&config);
    replace_context_field(&connection, "context_revision", u64::MAX);
    expected.ownership_generation = ownership.ownership_generation;
    expected.context_revision = u64::MAX;
    let before = context_invalidation_snapshot(&connection, conversation);
    assert!(matches!(
        client.invalidate_rcs_context_for_google_event(
            conversation,
            "google-conversation",
            expected,
            None,
        ),
        Err(Error::InvalidRequest(_))
    ));
    assert_eq!(
        context_invalidation_snapshot(&connection, conversation),
        before
    );
}

#[test]
fn google_event_invalidation_rolls_back_context_when_mapping_update_aborts() {
    let (_directory, _vault, config, client, conversation, _ownership, expected) = mapped_fixture();
    let connection = sql(&config);
    let before = context_invalidation_snapshot(&connection, conversation);
    connection
        .execute_batch(
            "CREATE TRIGGER reject_mapping_invalidation BEFORE UPDATE ON rcs_conversation_mappings BEGIN SELECT RAISE(ABORT, 'reject'); END;",
        )
        .unwrap();
    assert!(
        client
            .invalidate_rcs_context_for_google_event(
                conversation,
                "google-conversation",
                expected,
                None
            )
            .is_err()
    );
    assert_eq!(
        context_invalidation_snapshot(&connection, conversation),
        before
    );
}

#[test]
fn google_event_invalidation_ignores_history_before_the_mapping_guard_band() {
    let (_directory, _vault, config, client, conversation, _ownership, expected) = mapped_fixture();
    let connection = sql(&config);
    let verified_at_ms: i64 = connection
        .query_row(
            "SELECT verified_at_ms FROM rcs_conversation_mappings WHERE peppy_conversation_id=?",
            [conversation.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    let before = context_invalidation_snapshot(&connection, conversation);

    assert_eq!(
        client
            .invalidate_rcs_context_for_google_event(
                conversation,
                "google-conversation",
                expected,
                Some(verified_at_ms - 120_000),
            )
            .unwrap(),
        None
    );
    assert_eq!(
        context_invalidation_snapshot(&connection, conversation),
        before
    );
}

#[test]
fn google_event_invalidation_accepts_newer_or_untrusted_history_timestamps() {
    for case in ["inside_band", "newer", "none", "zero", "negative"] {
        let (_directory, _vault, config, client, conversation, _ownership, expected) =
            mapped_fixture();
        let connection = sql(&config);
        let verified_at_ms: i64 = connection
            .query_row(
                "SELECT verified_at_ms FROM rcs_conversation_mappings WHERE peppy_conversation_id=?",
                [conversation.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let event_at_ms = match case {
            "inside_band" => Some(verified_at_ms - 120_000 + 1),
            "newer" => Some(verified_at_ms + 1),
            "none" => None,
            "zero" => Some(0),
            "negative" => Some(-1),
            _ => unreachable!(),
        };

        assert_eq!(
            client
                .invalidate_rcs_context_for_google_event(
                    conversation,
                    "google-conversation",
                    expected,
                    event_at_ms,
                )
                .unwrap(),
            Some(2),
            "{case}"
        );
    }
}

#[test]
fn google_event_invalidation_ignores_an_old_history_signal_after_a_reproof() {
    let (_directory, _vault, config, client, conversation, _ownership, expected) = mapped_fixture();
    let connection = sql(&config);
    let original_verified_at_ms: i64 = connection
        .query_row(
            "SELECT verified_at_ms FROM rcs_conversation_mappings WHERE peppy_conversation_id=?",
            [conversation.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        client
            .invalidate_rcs_context_for_google_event(
                conversation,
                "google-conversation",
                expected,
                Some(original_verified_at_ms - 120_000 + 1),
            )
            .unwrap(),
        Some(2)
    );
    let reproof_at_ms = original_verified_at_ms + 240_000;
    let mut observation = mapping_observation(reproof_at_ms);
    observation.context_revision = 2;
    observation.native_evidence.lifetime.context_revision = 2;
    observation.carrier_binding.route_context_revision = 2;
    observation.mapping_revision = 2;
    assert!(matches!(
        client
            .verify_rcs_mapping(&observation, reproof_at_ms)
            .unwrap(),
        RcsMappingOutcome::Mapped(_)
    ));
    let before = context_invalidation_snapshot(&connection, conversation);
    assert_eq!(
        client
            .invalidate_rcs_context_for_google_event(
                conversation,
                "google-conversation",
                RcsContextExpectation {
                    context_revision: 2,
                    ..expected
                },
                Some(original_verified_at_ms - 120_000 + 1),
            )
            .unwrap(),
        None
    );
    assert_eq!(
        context_invalidation_snapshot(&connection, conversation),
        before
    );
}

#[test]
fn google_event_invalidation_rejects_a_lane_without_a_one_to_one_mapping_at_any_time() {
    for event_at_ms in [None, Some(1)] {
        let (_directory, _vault, config, client, _conversation, _ownership) = fixture();
        let observed_at_ms = now();
        client
            .record_rcs_link_lifecycle(
                RcsCapabilityState::Linked,
                true,
                true,
                true,
                observed_at_ms,
                observed_at_ms,
            )
            .unwrap();
        sql(&config)
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
        let group_google_id = "group-google-conversation";
        client
            .ingest_group_candidates(
                &json!({
                    "code": "conversation_candidate_snapshot",
                    "snapshot_token": "1:2:1",
                    "generation": 1,
                    "stream_epoch": 2,
                    "revision": 1,
                    "run_incarnation": "run",
                    "pairing_evidence_id": "pairing",
                    "complete": true,
                    "conversations": [{
                        "google_conversation_id": group_google_id,
                        "conversation_type": 2,
                        "conversation_status": 1,
                        "classification": "eligible",
                        "group": true,
                        "visible_non_self_count": 2,
                        "last_message_ms": observed_at_ms,
                        "participants": [
                            {"participant_id":"self","identifier_type":1,"raw_number":"+14155550100","formatted_number":"+14155550100","participant_id_present":true,"number_present":true,"visible":true,"self":true,"canonical_address":"+14155550100","canonical_address_present":true,"display_name":""},
                            {"participant_id":"alex","identifier_type":1,"raw_number":"+14155552671","formatted_number":"+14155552671","participant_id_present":true,"number_present":true,"visible":true,"self":false,"canonical_address":"+14155552671","canonical_address_present":true,"display_name":""},
                            {"participant_id":"sam","identifier_type":1,"raw_number":"+14155552672","formatted_number":"+14155552672","participant_id_present":true,"number_present":true,"visible":true,"self":false,"canonical_address":"+14155552672","canonical_address_present":true,"display_name":""}
                        ],
                        "name": "Weekend",
                        "is_group_chat": true,
                        "read_only": false,
                        "invisible_participant_count": 0,
                        "member_digest": "digest-2"
                    }]
                })
                .to_string(),
                observed_at_ms,
            )
            .unwrap();
        let context = client
            .rcs_receive_context(group_google_id)
            .unwrap()
            .unwrap();
        let connection = sql(&config);
        let before_context: String = connection
            .query_row(
                "SELECT v FROM metadata WHERE k='rcs_mapping_context_v1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let before_group_state: String = connection
            .query_row(
                "SELECT state FROM rcs_group_mapping WHERE conversation_id=?",
                [context.conversation_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(matches!(
            client.invalidate_rcs_context_for_google_event(
                context.conversation_id,
                group_google_id,
                context.expectation,
                event_at_ms,
            ),
            Err(Error::InvalidRequest(_))
        ));
        assert_eq!(
            connection
                .query_row(
                    "SELECT v FROM metadata WHERE k='rcs_mapping_context_v1'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            before_context
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT state FROM rcs_group_mapping WHERE conversation_id=?",
                    [context.conversation_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            before_group_state
        );
    }
}

/// Ingest never touches recovery: no resume hint and no open gap.
fn assert_untouched_recovery(config: &ClientConfig, conversation: ConversationId) {
    let state = recovery_state(&sql(config), conversation);
    assert!(state["resume_hint"].is_null(), "{state}");
    assert_eq!(state["incomplete"], false, "{state}");
}

fn proof_time(connection: &Connection, conversation: ConversationId) -> i64 {
    connection
        .query_row(
            "SELECT proof_time_ms FROM rcs_lane_ownership WHERE conversation_id=?",
            [conversation.to_string()],
            |row| row.get(0),
        )
        .unwrap()
}

fn observe(
    client: &Client,
    conversation: ConversationId,
    generation: u64,
    id: &str,
    sent_at_ms: Option<i64>,
    provenance: RcsObservationProvenance,
) -> Result<RcsReceiveOutcome, Error> {
    let mut observation = record(id);
    observation.sent_at_ms = sent_at_ms;
    client.ingest_rcs_projected_observation(conversation, generation, observation, provenance)
}

#[test]
fn explicit_historical_provenance_enforces_strict_floor_and_imported_semantics() {
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    let connection = sql(&config);
    let floor = proof_time(&connection, conversation);
    let before = receive_snapshot(&connection, conversation);
    for (id, sent_at_ms) in [
        ("equal", Some(floor)),
        ("older", Some(floor - 1)),
        ("missing", None),
    ] {
        assert!(matches!(
            observe(
                &client,
                conversation,
                owner.ownership_generation,
                id,
                sent_at_ms,
                RcsObservationProvenance::Historical,
            ),
            Ok(RcsReceiveOutcome::Rejected(RcsReceiveCode::ProviderFloor))
        ));
        assert_eq!(receive_snapshot(&connection, conversation), before);
    }
    assert_eq!(
        observe(
            &client,
            conversation,
            owner.ownership_generation,
            "historical-floor-plus-one",
            Some(floor + 1),
            RcsObservationProvenance::Historical,
        )
        .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    assert!(only_message(&client, conversation).seen);
    assert_untouched_recovery(&config, conversation);

    let (_directory, _vault, config, client, conversation, owner) = fixture();
    let floor = proof_time(&sql(&config), conversation);
    assert_eq!(
        observe(
            &client,
            conversation,
            owner.ownership_generation,
            "live-at-old-time",
            Some(floor - 1),
            RcsObservationProvenance::Live,
        )
        .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    assert!(!only_message(&client, conversation).seen);
}

#[test]
fn live_then_historical_duplicate_preserves_first_created_payload() {
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    let floor = proof_time(&sql(&config), conversation);
    assert_eq!(
        observe(
            &client,
            conversation,
            owner.ownership_generation,
            "same",
            Some(floor + 1),
            RcsObservationProvenance::Live,
        )
        .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    let before = only_message(&client, conversation);
    assert!(!before.seen);
    assert_eq!(
        observe(
            &client,
            conversation,
            owner.ownership_generation,
            "same",
            Some(floor + 2),
            RcsObservationProvenance::Historical,
        )
        .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    let after = only_message(&client, conversation);
    assert_eq!(after.payload.body, before.payload.body);
    assert!(!after.seen);
    assert_untouched_recovery(&config, conversation);
}

#[test]
fn historical_then_live_duplicate_preserves_first_created_payload() {
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    let floor = proof_time(&sql(&config), conversation);
    assert_eq!(
        observe(
            &client,
            conversation,
            owner.ownership_generation,
            "same",
            Some(floor + 1),
            RcsObservationProvenance::Historical,
        )
        .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    let before = only_message(&client, conversation);
    assert!(before.seen);
    assert_eq!(
        observe(
            &client,
            conversation,
            owner.ownership_generation,
            "same",
            Some(floor + 2),
            RcsObservationProvenance::Live,
        )
        .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    let after = only_message(&client, conversation);
    assert_eq!(after.payload.body, before.payload.body);
    assert!(after.seen);
    assert_untouched_recovery(&config, conversation);
}

#[test]
fn historical_outgoing_match_preserves_outgoing_message_payload_and_checkpoint() {
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("seed"))
        .unwrap();
    let message = only_message(&client, conversation);
    let message_id = message.payload.record.message_id;
    let original_body = message.payload.body.clone();
    let connection = sql(&config);
    install_echo_ledger(&connection, message_id);
    let floor = proof_time(&connection, conversation);
    let mut echo = record("echo-observation");
    echo.direction = Direction::Outgoing;
    echo.tmp_id = Some("tmp".into());
    echo.body = "must-not-replace-outgoing".into();
    echo.sent_at_ms = Some(floor + 1);
    assert_eq!(
        client
            .ingest_rcs_projected_observation(
                conversation,
                owner.ownership_generation,
                echo,
                RcsObservationProvenance::Historical,
            )
            .unwrap(),
        RcsReceiveOutcome::OutgoingMatched
    );
    assert_eq!(
        only_message(&client, conversation).payload.body,
        original_body
    );
    assert_untouched_recovery(&config, conversation);
}

#[derive(Debug, PartialEq)]
struct CutoverSnapshot {
    ownership: (String, u64, Option<String>, i64, i64, i64, Option<String>),
    outbox: i64,
    metadata: Option<(Option<i64>, bool, i64, String, i64)>,
}

fn cutover_snapshot(connection: &Connection, conversation: ConversationId) -> CutoverSnapshot {
    CutoverSnapshot {
        ownership: connection
            .query_row(
                "SELECT owner,ownership_generation,history_token,detector_floor,proof_time_ms,observed_detector_high_water,alert_code FROM rcs_lane_ownership WHERE conversation_id=?",
                [conversation.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
            )
            .unwrap(),
        outbox: connection
            .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
            .unwrap(),
        metadata: connection
            .query_row(
                "SELECT rcs_encryption,rcs_history_gap,logical_revision,writer_device_id,source_sequence FROM rcs_conversation_metadata WHERE conversation_id=?",
                [conversation.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .ok(),
    }
}

#[test]
fn owner_cutover_lifecycle_generation_fence_alone_rejects_without_writes() {
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    let lifecycle_generation = record_terminal_unlink(&client, &config);
    let connection = sql(&config);
    let before = cutover_snapshot(&connection, conversation);
    assert!(matches!(
        client.begin_rcs_owner_unlink_cutover(
            conversation,
            owner.ownership_generation,
            lifecycle_generation + 1,
        ),
        Err(Error::InvalidRequest(_))
    ));
    assert_eq!(cutover_snapshot(&connection, conversation), before);
}

#[test]
fn owner_cutover_completion_record_fence_alone_rejects_without_writes() {
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    let observed_at_ms = now();
    client
        .record_rcs_link_lifecycle(
            RcsCapabilityState::Unlinked,
            false,
            false,
            false,
            observed_at_ms,
            observed_at_ms,
        )
        .unwrap();
    let lifecycle_generation = lifecycle_generation(&config);
    let connection = sql(&config);
    let before = cutover_snapshot(&connection, conversation);
    assert!(matches!(
        client.begin_rcs_owner_unlink_cutover(
            conversation,
            owner.ownership_generation,
            lifecycle_generation,
        ),
        Err(Error::InvalidRequest(_))
    ));
    assert_eq!(cutover_snapshot(&connection, conversation), before);
}

#[test]
fn linked_cutover_gateway_fence_alone_rejects_each_gateway_denial_without_writes() {
    for denial in ["unavailable", "nonlocal", "nonunique", "stale"] {
        let (_directory, _vault, config, client, conversation, owner) = fixture();
        let connection = sql(&config);
        match denial {
            "unavailable" => {
                connection
                    .execute("UPDATE rcs_link_status_projection SET availability=0", [])
                    .unwrap();
            }
            "nonlocal" => {
                connection
                    .execute(
                        "UPDATE rcs_link_status_projection SET gateway_device_id=?",
                        [DeviceId::new().to_string()],
                    )
                    .unwrap();
            }
            "nonunique" => {
                connection.execute(
                    "INSERT INTO rcs_link_status_projection(gateway_device_id,state,availability,health,phone_responsive,observed_at_ms,capability_revision,feature_bits,producer_sequence) SELECT ?,state,availability,health,phone_responsive,observed_at_ms,capability_revision,feature_bits,producer_sequence FROM rcs_link_status_projection LIMIT 1",
                    [DeviceId::new().to_string()],
                ).unwrap();
            }
            "stale" => {
                connection
                    .execute("UPDATE rcs_link_status_projection SET observed_at_ms=0", [])
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let before = cutover_snapshot(&connection, conversation);
        assert!(matches!(
            client.begin_rcs_unlink_cutover_if_current(conversation, owner.ownership_generation,),
            Err(Error::InvalidRequest(_))
        ));
        assert_eq!(
            cutover_snapshot(&connection, conversation),
            before,
            "{denial}"
        );
    }
}

#[test]
fn owner_cutover_ownership_generation_fence_rejects_without_writes() {
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    let lifecycle_generation = record_terminal_unlink(&client, &config);
    let connection = sql(&config);
    let before = cutover_snapshot(&connection, conversation);
    assert!(matches!(
        client.begin_rcs_owner_unlink_cutover(
            conversation,
            owner.ownership_generation + 1,
            lifecycle_generation,
        ),
        Err(Error::InvalidRequest(_))
    ));
    assert_eq!(cutover_snapshot(&connection, conversation), before);
}

#[test]
fn linked_cutover_publication_failure_rolls_back_ownership_and_outbox() {
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    client
        .record_rcs_provider_detector(
            conversation,
            owner.ownership_generation,
            detector(1, owner.ownership_generation),
        )
        .unwrap();
    let connection = sql(&config);
    let before = cutover_snapshot(&connection, conversation);
    connection.execute_batch("CREATE TRIGGER reject_linked_cutover_publication BEFORE INSERT ON outbox BEGIN SELECT RAISE(ABORT, 'reject'); END;").unwrap();
    assert!(
        client
            .begin_rcs_unlink_cutover_if_current(conversation, owner.ownership_generation)
            .is_err()
    );
    assert_eq!(cutover_snapshot(&connection, conversation), before);
}

#[test]
fn owner_cutover_publication_failure_rolls_back_ownership_and_outbox() {
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    client
        .record_rcs_provider_detector(
            conversation,
            owner.ownership_generation,
            detector(1, owner.ownership_generation),
        )
        .unwrap();
    let lifecycle_generation = record_terminal_unlink(&client, &config);
    let connection = sql(&config);
    let before = cutover_snapshot(&connection, conversation);
    connection.execute_batch("CREATE TRIGGER reject_owner_cutover_publication BEFORE INSERT ON outbox BEGIN SELECT RAISE(ABORT, 'reject'); END;").unwrap();
    assert!(
        client
            .begin_rcs_owner_unlink_cutover(
                conversation,
                owner.ownership_generation,
                lifecycle_generation,
            )
            .is_err()
    );
    assert_eq!(cutover_snapshot(&connection, conversation), before);
}
