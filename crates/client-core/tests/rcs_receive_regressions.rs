#[path = "common/mod.rs"]
mod common;

use common::*;
use rusqlite::{Connection, params};
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
    let ownership = client
        .begin_rcs_link_ownership(conversation, "initial", 0)
        .unwrap();
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
        history_token: None,
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
fn matched_duplicate_page_advances_token_and_publication_failure_is_atomic() {
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
    let mut echo = record("echo");
    echo.direction = Direction::Outgoing;
    echo.tmp_id = Some("tmp".into());
    echo.history_token = Some("A".into());
    echo.sent_at_ms = Some(now());
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, echo.clone())
            .unwrap(),
        RcsReceiveOutcome::OutgoingMatched
    );
    echo.history_token = Some("B".into());
    echo.rcs_reported = true;
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, echo)
            .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    assert_eq!(
        client
            .rcs_receive_state(conversation)
            .unwrap()
            .unwrap()
            .history_token
            .as_deref(),
        Some("B")
    );
    drop(client);
    let reopened = unlocked(&config, &vault);
    assert_eq!(
        reopened
            .rcs_receive_state(conversation)
            .unwrap()
            .unwrap()
            .history_token
            .as_deref(),
        Some("B")
    );
    let before_unchanged_duplicate: i64 = connection
        .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
        .unwrap();
    let mut unchanged = record("echo");
    unchanged.direction = Direction::Outgoing;
    unchanged.tmp_id = Some("tmp".into());
    unchanged.history_token = Some("C".into());
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
    assert_eq!(
        reopened
            .rcs_receive_state(conversation)
            .unwrap()
            .unwrap()
            .history_token
            .as_deref(),
        Some("C")
    );

    connection.execute_batch("CREATE TRIGGER reject_rcs_fact BEFORE INSERT ON outbox BEGIN SELECT RAISE(ABORT, 'reject'); END;").unwrap();
    let before = receive_snapshot(&connection, conversation);
    let mut failed = record("failed");
    failed.direction = Direction::Outgoing;
    failed.tmp_id = Some("tmp".into());
    failed.history_token = Some("D".into());
    failed.sent_at_ms = Some(now());
    failed.e2ee_reported = Some(true);
    assert!(
        reopened
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, failed)
            .is_err()
    );
    assert_eq!(receive_snapshot(&connection, conversation), before);
    // Reverting the duplicate checkpoint or no-op fact publication fences fails token C or the count.
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
        page.history_token = Some("page".into());
        page.sent_at_ms = sent_at_ms;
        assert_eq!(
            client
                .ingest_rcs_projected_record(conversation, owner.ownership_generation, page)
                .unwrap(),
            RcsReceiveOutcome::Rejected(RcsReceiveCode::ProviderFloor)
        );
        assert_eq!(receive_snapshot(&connection, conversation), before);
    }
    let mut newer = record("newer");
    newer.history_token = Some("newer".into());
    newer.sent_at_ms = Some(proof_time + 1);
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, newer)
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
                None
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
            client
                .begin_rcs_link_ownership(conversation, "newer", 0)
                .unwrap();
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
    history_token: Option<String>,
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
    let (messages, identities, outbox, detectors, high_water, history_token, alert): (
        i64,
        i64,
        i64,
        i64,
        Option<i64>,
        Option<String>,
        Option<String>,
    ) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM messages), (SELECT COUNT(*) FROM rcs_message_identities), (SELECT COUNT(*) FROM outbox), (SELECT COUNT(*) FROM rcs_provider_detectors WHERE conversation_id=?1), (SELECT observed_detector_high_water FROM rcs_lane_ownership WHERE conversation_id=?1), (SELECT history_token FROM rcs_lane_ownership WHERE conversation_id=?1), (SELECT alert_code FROM rcs_lane_ownership WHERE conversation_id=?1)",
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
        history_token,
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
fn fenced_checkpoint_rejects_gateway_and_ownership_denials_without_writes() {
    for denial in DENIALS {
        let scene = denied_scene(denial);
        let connection = sql(&scene.config);
        let before = fenced_snapshot(&scene.client, &connection, scene.conversation);
        assert_eq!(
            scene
                .client
                .checkpoint_rcs_history(scene.conversation, scene.generation, "fenced-token")
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
    // Positive control: the unique current gateway records the checkpoint (guard: unique_current_gateway in checkpoint_rcs_history, 94109b0f).
    let (_directory, _vault, config, client, conversation, owner) = fixture();
    assert_eq!(
        client
            .checkpoint_rcs_history(conversation, owner.ownership_generation, "fenced-token")
            .unwrap(),
        RcsIdentityOutcome::Recorded
    );
    let connection = sql(&config);
    assert_eq!(
        fenced_snapshot(&client, &connection, conversation).history_token,
        Some("fenced-token".into())
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
    let relinked = client
        .begin_rcs_link_ownership(conversation, "relink", 0)
        .unwrap();
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
