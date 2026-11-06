#[path = "common/mod.rs"]
mod common;

use common::*;
use rusqlite::{Connection, OptionalExtension};

fn status_rank(config: &ClientConfig, message_id: MessageId) -> Option<i64> {
    let connection = Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
        .query_row(
            "SELECT status_rank FROM rcs_message_status WHERE message_id=?",
            [message_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

fn status(status: RcsProjectedStatus) -> RcsStatusApplication {
    RcsStatusApplication {
        google_message_id: "provider-message".into(),
        google_conversation_id: "provider-conversation".into(),
        tmp_id: None,
        status,
    }
}

fn record(body: &str) -> RcsProjectedRecord {
    RcsProjectedRecord {
        google_message_id: "provider-message".into(),
        google_conversation_id: "provider-conversation".into(),
        direction: Direction::Incoming,
        body: body.into(),
        sent_at_ms: Some(1),
        tmp_id: None,
        rcs_reported: true,
        e2ee_reported: Some(true),
        history_token: None,
    }
}

#[test]
fn rcs_conversation_metadata_converges_out_of_order_without_provider_identifiers() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_config = config(&directory, "first", &vault);
    let second_config = config(&directory, "second", &vault);
    let first = unlocked(&first_config, &vault);
    let second = unlocked(&second_config, &vault);
    let conversation = ConversationId::new();
    let first_owner = first
        .begin_rcs_link_ownership(conversation, "first", 0)
        .unwrap();

    assert!(first
        .publish_rcs_conversation_metadata(
            conversation,
            first_owner.ownership_generation,
            None,
        )
        .unwrap());
    assert!(
        first
            .publish_rcs_conversation_metadata(
                conversation,
                first_owner.ownership_generation,
                Some(true),
            )
            .unwrap()
    );

    let mut envelopes = first.pending_outbox().unwrap();
    assert_eq!(envelopes.len(), 3);
    envelopes.reverse();
    for (index, envelope) in envelopes.iter().enumerate() {
        assert!(!serde_json::to_string(envelope).unwrap().contains("google_"));
        assert!(matches!(
            second
                .ingest(envelope, Cursor(u64::try_from(index + 1).unwrap()))
                .unwrap(),
            IngestResult::Journaled
        ));
    }
    second.apply_pending(100).unwrap();
    let metadata = second
        .rcs_conversation_metadata(conversation)
        .unwrap()
        .unwrap();
    assert_eq!(metadata.transport, RcsConversationTransport::Rcs);
    assert_eq!(metadata.rcs_encryption, Some(true));
    assert_eq!(metadata.logical_revision, 3);
    assert!(
        !serde_json::to_string(&metadata)
            .unwrap()
            .contains("provider-")
    );
}

#[test]
fn stale_metadata_generation_is_rejected_without_writing() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client
        .begin_rcs_link_ownership(conversation, "token", 0)
        .unwrap();

    assert_eq!(
        client
            .publish_rcs_conversation_metadata(
                conversation,
                owner.ownership_generation + 1,
                Some(true),
            )
            .unwrap_err(),
        Error::InvalidRequest("stale RCS conversation metadata writer")
    );
    assert!(!client
        .rcs_conversation_metadata(conversation)
        .unwrap()
        .unwrap()
        .rcs_history_gap);
}

#[test]
fn rcs_conversation_metadata_omits_encryption_until_reported() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client
        .begin_rcs_link_ownership(conversation, "token", 0)
        .unwrap();

    client
        .publish_rcs_conversation_metadata(conversation, owner.ownership_generation, None)
        .unwrap();
    assert_eq!(
        client
            .rcs_conversation_metadata(conversation)
            .unwrap()
            .unwrap()
            .rcs_encryption,
        None
    );
}

#[test]
fn rcs_metadata_replicates_a_history_gap_through_cutover_and_relink() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_config = config(&directory, "first", &vault);
    let second_config = config(&directory, "second", &vault);
    let first = unlocked(&first_config, &vault);
    let second = unlocked(&second_config, &vault);
    let conversation = ConversationId::new();
    let owner = first
        .begin_rcs_link_ownership(conversation, "first", 0)
        .unwrap();
    first
        .publish_rcs_conversation_metadata(conversation, owner.ownership_generation, None)
        .unwrap();
    first
        .record_rcs_provider_detector(RcsProviderDetector {
            conversation_id: conversation,
            provider_row_id: 1,
            provider_thread_id: 1,
            provider_type: 1,
            observed_at_ms: 1,
            observed_generation: owner.ownership_generation,
        })
        .unwrap();
    first.begin_rcs_unlink_cutover(conversation).unwrap();

    let mut server = Server::default();
    server.upload(&first);
    server.sync(&second);
    assert!(second
        .rcs_conversation_metadata(conversation)
        .unwrap()
        .unwrap()
        .rcs_history_gap);

    let _relink = first
        .begin_rcs_link_ownership(conversation, "relink", 1)
        .unwrap();
    assert!(!first
        .rcs_conversation_metadata(conversation)
        .unwrap()
        .unwrap()
        .rcs_history_gap);
    server.upload(&first);
    server.sync(&second);
    assert!(!second
        .rcs_conversation_metadata(conversation)
        .unwrap()
        .unwrap()
        .rcs_history_gap);
}

#[test]
fn compacted_snapshot_restores_rcs_status_and_metadata_without_provider_identifiers() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&directory, "gateway", &vault), &vault);
    let restored = unlocked(&config(&directory, "restored", &vault), &vault);
    let conversation = ConversationId::new();
    let owner = gateway.begin_rcs_link_ownership(conversation, "token", 0).unwrap();
    gateway
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("body"))
        .unwrap();
    gateway
        .apply_rcs_projected_status(
            conversation,
            owner.ownership_generation,
            status(RcsProjectedStatus::Read),
        )
        .unwrap();
    gateway
        .publish_rcs_conversation_metadata(conversation, owner.ownership_generation, Some(true))
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    let records: Vec<SnapshotRecord> = server
        .log
        .iter()
        .enumerate()
        .filter(|(_, envelope)| envelope.compaction.is_some())
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(u64::try_from(index + 1).unwrap()),
            envelope: envelope.clone(),
        })
        .collect();
    assert!(records.iter().all(|record| {
        !serde_json::to_string(&record.envelope)
            .unwrap()
            .contains("provider-")
    }));
    let session = restored
        .begin_snapshot_with_compaction(
            Cursor(u64::try_from(server.log.len()).unwrap()),
            u64::try_from(records.len()).unwrap(),
            SnapshotPurpose::Resync,
            Some(3),
        )
        .unwrap();
    restored.append_snapshot_page(session.generation, &records).unwrap();
    restored.finish_snapshot(session.generation).unwrap();
    while restored.apply_pending(100).unwrap().snapshot_remaining > 0 {}
    let message_id = only_message(&gateway, conversation).payload.record.message_id;
    assert_eq!(status_rank(&config(&directory, "restored", &vault), message_id), Some(5));
    assert_eq!(
        restored.rcs_conversation_metadata(conversation).unwrap(),
        gateway.rcs_conversation_metadata(conversation).unwrap()
    );
}

#[test]
fn deterministic_rcs_message_ids_converge_without_exposing_provider_identity() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_config = config(&directory, "first", &vault);
    let second_config = config(&directory, "second", &vault);
    let first = unlocked(&first_config, &vault);
    let second = unlocked(&second_config, &vault);
    let conversation = ConversationId::new();
    let first_owner = first
        .begin_rcs_link_ownership(conversation, "first-token", 0)
        .unwrap();
    let second_owner = second
        .begin_rcs_link_ownership(conversation, "second-token", 0)
        .unwrap();

    assert_eq!(
        first
            .ingest_rcs_projected_record(
                conversation,
                first_owner.ownership_generation,
                record("one")
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    assert_eq!(
        second
            .ingest_rcs_projected_record(
                conversation,
                second_owner.ownership_generation,
                record("one")
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    let first_id = only_message(&first, conversation).payload.record.message_id;
    let second_id = only_message(&second, conversation)
        .payload
        .record
        .message_id;
    assert_eq!(first_id, second_id);
    assert_ne!(first_id.to_string(), "provider-message");
    assert_ne!(first_id.to_string(), "provider-conversation");

    let other_vault = Vault::new();
    let other_config = config(&directory, "other", &other_vault);
    let other = unlocked(&other_config, &other_vault);
    let other_owner = other
        .begin_rcs_link_ownership(conversation, "other-token", 0)
        .unwrap();
    other
        .ingest_rcs_projected_record(
            conversation,
            other_owner.ownership_generation,
            record("one"),
        )
        .unwrap();
    assert_ne!(
        first_id,
        only_message(&other, conversation).payload.record.message_id
    );
}

#[test]
fn replicated_rcs_messages_are_idempotent() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_config = config(&directory, "first", &vault);
    let second_config = config(&directory, "second", &vault);
    let first = unlocked(&first_config, &vault);
    let second = unlocked(&second_config, &vault);
    let conversation = ConversationId::new();
    let first_owner = first
        .begin_rcs_link_ownership(conversation, "first-token", 0)
        .unwrap();
    let second_owner = second
        .begin_rcs_link_ownership(conversation, "second-token", 0)
        .unwrap();
    first
        .ingest_rcs_projected_record(
            conversation,
            first_owner.ownership_generation,
            record("one"),
        )
        .unwrap();
    second
        .ingest_rcs_projected_record(
            conversation,
            second_owner.ownership_generation,
            record("one"),
        )
        .unwrap();

    let mut server = Server::default();
    server.upload(&first);
    server.upload(&second);
    server.sync(&first);
    server.sync(&second);
    assert_eq!(first.messages(conversation).unwrap().len(), 1);
    assert_eq!(second.messages(conversation).unwrap().len(), 1);
}

#[test]
fn incoming_rcs_replay_stale_writer_and_page_floor_are_rejected_without_rows() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client
        .begin_rcs_link_ownership(conversation, "first-token", 10)
        .unwrap();

    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("one"))
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("one"))
            .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    assert_eq!(client.messages(conversation).unwrap().len(), 1);
    assert_eq!(
        client
            .ingest_rcs_projected_record(
                conversation,
                owner.ownership_generation + 1,
                record("two")
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
    let mut page = record("page");
    page.google_message_id = "page-message".into();
    page.history_token = Some("page-token".into());
    page.sent_at_ms = Some(10);
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, page)
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::ProviderFloor)
    );
    assert_eq!(client.messages(conversation).unwrap().len(), 1);
}

#[test]
fn page_records_are_imported_while_live_records_remain_unseen() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client
        .begin_rcs_link_ownership(conversation, "token", 0)
        .unwrap();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("live"))
        .unwrap();
    let mut page = record("page");
    page.google_message_id = "page-message".into();
    page.history_token = Some("page-token".into());
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, page)
        .unwrap();

    let messages = client.messages(conversation).unwrap();
    assert_eq!(messages.len(), 2);
    assert!(
        messages
            .iter()
            .any(|message| message.payload.body == "live" && !message.seen)
    );
    assert!(
        messages
            .iter()
            .any(|message| message.payload.body == "page" && message.seen)
    );
    assert_eq!(
        client
            .rcs_receive_state(conversation)
            .unwrap()
            .unwrap()
            .history_token
            .as_deref(),
        Some("page-token")
    );
}

#[test]
fn rcs_status_lattice_is_monotonic() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client
        .begin_rcs_link_ownership(conversation, "token", 0)
        .unwrap();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("live"))
        .unwrap();
    for projected_status in [
        RcsProjectedStatus::Sending,
        RcsProjectedStatus::Sent,
        RcsProjectedStatus::Failed,
        RcsProjectedStatus::Delivered,
        RcsProjectedStatus::Read,
    ] {
        assert_eq!(
            client
                .apply_rcs_projected_status(
                    conversation,
                    owner.ownership_generation,
                    status(projected_status),
                )
                .unwrap(),
            RcsReceiveOutcome::StatusApplied
        );
    }
    assert_eq!(
        client
            .apply_rcs_projected_status(
                conversation,
                owner.ownership_generation,
                status(RcsProjectedStatus::Failed),
            )
            .unwrap(),
        RcsReceiveOutcome::StatusUnchanged
    );
}

#[test]
fn statuses_converge_and_rejections_leave_no_status_row() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_config = config(&directory, "first", &vault);
    let second_config = config(&directory, "second", &vault);
    let first = unlocked(&first_config, &vault);
    let second = unlocked(&second_config, &vault);
    let conversation = ConversationId::new();
    let first_owner = first
        .begin_rcs_link_ownership(conversation, "first", 0)
        .unwrap();
    let second_owner = second
        .begin_rcs_link_ownership(conversation, "second", 0)
        .unwrap();
    for (client, generation) in [
        (&first, first_owner.ownership_generation),
        (&second, second_owner.ownership_generation),
    ] {
        client
            .ingest_rcs_projected_record(conversation, generation, record("body"))
            .unwrap();
    }
    let message_id = only_message(&first, conversation).payload.record.message_id;
    first
        .apply_rcs_projected_status(
            conversation,
            first_owner.ownership_generation,
            status(RcsProjectedStatus::Failed),
        )
        .unwrap();
    first
        .apply_rcs_projected_status(
            conversation,
            first_owner.ownership_generation,
            status(RcsProjectedStatus::Read),
        )
        .unwrap();
    second
        .apply_rcs_projected_status(
            conversation,
            second_owner.ownership_generation,
            status(RcsProjectedStatus::Delivered),
        )
        .unwrap();
    second
        .apply_rcs_projected_status(
            conversation,
            second_owner.ownership_generation,
            status(RcsProjectedStatus::Sent),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&first);
    server.upload(&second);
    server.sync(&first);
    server.sync(&second);
    assert_eq!(status_rank(&first_config, message_id), Some(5));
    assert_eq!(status_rank(&second_config, message_id), Some(5));
    assert_eq!(
        first
            .apply_rcs_projected_status(
                conversation,
                first_owner.ownership_generation + 1,
                status(RcsProjectedStatus::Read)
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
    assert_eq!(
        first
            .apply_rcs_projected_status(
                conversation,
                first_owner.ownership_generation,
                RcsStatusApplication {
                    google_message_id: "unknown".into(),
                    ..status(RcsProjectedStatus::Read)
                }
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::IdentityConflict)
    );
    assert_eq!(status_rank(&first_config, message_id), Some(5));
}

#[test]
fn unmatched_foreign_outgoing_is_inserted_once_without_a_command() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client
        .begin_rcs_link_ownership(conversation, "token", 0)
        .unwrap();
    let mut foreign = record("foreign");
    foreign.direction = Direction::Outgoing;
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, foreign.clone())
            .unwrap(),
        RcsReceiveOutcome::UnmatchedOutgoing
    );
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, foreign)
            .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    let message = only_message(&client, conversation);
    assert_eq!(message.payload.direction, Direction::Outgoing);
    assert_eq!(message.send_state, None);
    let message_id = message.payload.record.message_id;
    assert_eq!(
        client
            .apply_rcs_projected_status(
                conversation,
                owner.ownership_generation,
                status(RcsProjectedStatus::Sent),
            )
            .unwrap(),
        RcsReceiveOutcome::StatusApplied
    );
    assert_eq!(status_rank(&config, message_id), Some(2));
    for envelope in client.pending_outbox().unwrap() {
        let serialized = serde_json::to_string(&envelope).unwrap();
        assert!(!serialized.contains("provider-message"));
        assert!(!serialized.contains("provider-conversation"));
    }
}

#[test]
fn outgoing_tmp_echo_binds_existing_message_and_replays_after_reopen() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client
        .begin_rcs_link_ownership(conversation, "token", 0)
        .unwrap();
    let mut existing = record("existing");
    existing.google_message_id = "existing-message".into();
    existing.google_conversation_id = "existing-conversation".into();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, existing)
        .unwrap();
    let message_id = only_message(&client, conversation)
        .payload
        .record
        .message_id;
    let command_id = CommandId::new();
    let connection = Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO commands(command_id,message_id,gateway_device_id) VALUES(?,?,?)",
            [
                command_id.to_string(),
                message_id.to_string(),
                DeviceId::new().to_string(),
            ],
        )
        .unwrap();
    connection.execute(
        "INSERT INTO rcs_send_ledger(command_id,tmp_id,mapping_revision,proof_digest,google_conversation_id,attempted_at_ms) VALUES(?,?,1,zeroblob(32),?,1)",
        [command_id.to_string(), "tmp".into(), "provider-conversation".into()],
    ).unwrap();
    drop(connection);
    let mut echo = record("echo");
    echo.direction = Direction::Outgoing;
    echo.tmp_id = Some("tmp".into());
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, echo.clone())
            .unwrap(),
        RcsReceiveOutcome::OutgoingMatched
    );
    assert_eq!(client.messages(conversation).unwrap().len(), 1);
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, echo)
            .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    drop(client);
    let reopened = unlocked(&config, &vault);
    let mut server = Server::default();
    server.upload(&reopened);
    assert_eq!(reopened.messages(conversation).unwrap().len(), 1);
}

#[test]
fn matched_echo_status_replicates_without_provider_identifiers() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_config = config(&directory, "first", &vault);
    let second_config = config(&directory, "second", &vault);
    let first = unlocked(&first_config, &vault);
    let second = unlocked(&second_config, &vault);
    let conversation = ConversationId::new();
    let first_owner = first
        .begin_rcs_link_ownership(conversation, "first", 0)
        .unwrap();
    let second_owner = second
        .begin_rcs_link_ownership(conversation, "second", 0)
        .unwrap();
    first
        .ingest_rcs_projected_record(
            conversation,
            first_owner.ownership_generation,
            record("body"),
        )
        .unwrap();
    second
        .ingest_rcs_projected_record(
            conversation,
            second_owner.ownership_generation,
            record("body"),
        )
        .unwrap();
    let before = only_message(&first, conversation).send_state;
    assert_eq!(
        first
            .apply_rcs_projected_status(
                conversation,
                first_owner.ownership_generation,
                status(RcsProjectedStatus::Read)
            )
            .unwrap(),
        RcsReceiveOutcome::StatusApplied
    );
    assert_eq!(only_message(&first, conversation).send_state, before);
    let mut server = Server::default();
    for envelope in first.pending_outbox().unwrap() {
        let serialized = serde_json::to_string(&envelope).unwrap();
        assert!(!serialized.contains("provider-message"));
        assert!(!serialized.contains("provider-conversation"));
    }
    server.upload(&first);
    server.sync(&second);
    let connection = Connection::open(&second_config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    let payload: String = connection
        .query_row("SELECT payload FROM messages LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert!(!payload.contains("provider-message"));
    assert!(!payload.contains("provider-conversation"));
}

#[test]
fn conflicting_identity_rejects_without_partial_message() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client
        .begin_rcs_link_ownership(conversation, "token", 0)
        .unwrap();
    let connection = Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection.execute("INSERT INTO rcs_message_identities(google_message_id,google_conversation_id,conversation_id,ownership_generation,message_id) VALUES('provider-message','provider-conversation',?,1,NULL)", [ConversationId::new().to_string()]).unwrap();
    let before: (i64, i64) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM rcs_message_identities)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("body"))
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::IdentityConflict)
    );
    let after: (i64, i64) = connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM rcs_message_identities)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(after, before);
}
