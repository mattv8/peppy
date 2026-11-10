mod common;

use common::*;
use std::time::{SystemTime, UNIX_EPOCH};

const GOOGLE_CONVERSATION: &str = "google-media-conversation";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
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

fn incoming_media_record() -> RcsProjectedRecord {
    RcsProjectedRecord {
        google_message_id: "google-media-message".into(),
        google_conversation_id: GOOGLE_CONVERSATION.into(),
        direction: Direction::Incoming,
        body: "caption from Google Messages".into(),
        sent_at_ms: Some(1_735_689_600_123),
        tmp_id: None,
        rcs_reported: true,
        e2ee_reported: Some(true),
        attachments: vec![
            RcsIncomingAttachment {
                original_index: 0,
                media_type: "video/mp4".into(),
                display_name: "oversized.mp4".into(),
                plaintext_bytes: 32 * 1024 * 1024 + 1,
                too_large: true,
                media_ref: "google-ref-slot-zero-secret".into(),
            },
            RcsIncomingAttachment {
                original_index: 1,
                media_type: "image/jpeg".into(),
                display_name: "photo.jpg".into(),
                plaintext_bytes: 12_345,
                too_large: false,
                media_ref: "google-ref-slot-one-secret".into(),
            },
        ],
    }
}

#[test]
fn sealed_media_placeholders_replicate_to_a_second_client_without_download_authority() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "gateway", &vault);
    let desktop_config = config(&directory, "desktop", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    publish_current_gateway(&gateway, &gateway_config);
    let conversation_id = ConversationId::new();
    let ownership = gateway
        .begin_rcs_link_ownership(conversation_id, 0)
        .unwrap();

    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);

    assert_eq!(
        gateway
            .ingest_rcs_projected_record(
                conversation_id,
                ownership.ownership_generation,
                incoming_media_record(),
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    assert_eq!(
        server.upload(&gateway),
        5,
        "parent, two slot states, and reported facts"
    );
    let report = server.sync(&desktop);
    assert_eq!(report.quarantined, 0, "{report:?}");

    let message = only_message(&desktop, conversation_id);
    assert_eq!(message.payload.body, "caption from Google Messages");
    assert_eq!(message.payload.sender_address, None);
    assert_eq!(message.payload.sent_at_ms, Some(1_735_689_600_123));

    let oversized = desktop
        .rcs_media_receive_view(message.payload.record.message_id, 0)
        .unwrap()
        .unwrap();
    assert_eq!(oversized.attachment_index, 0);
    assert_eq!(
        oversized.placeholder_id,
        format!("{}:0", message.payload.record.message_id)
    );
    assert_eq!(oversized.media_type, "video/mp4");
    assert_eq!(oversized.display_name, "oversized.mp4");
    assert_eq!(oversized.plaintext_bytes, 32 * 1024 * 1024 + 1);
    assert_eq!(oversized.state, RcsMediaSyncState::TooLarge);
    assert_eq!(oversized.attachment_id, None);

    let downloading = desktop
        .rcs_media_receive_view(message.payload.record.message_id, 1)
        .unwrap()
        .unwrap();
    assert_eq!(downloading.attachment_index, 1);
    assert_eq!(
        downloading.placeholder_id,
        format!("{}:1", message.payload.record.message_id)
    );
    assert_eq!(downloading.media_type, "image/jpeg");
    assert_eq!(downloading.display_name, "photo.jpg");
    assert_eq!(downloading.plaintext_bytes, 12_345);
    assert_eq!(downloading.state, RcsMediaSyncState::Downloading);
    assert_eq!(downloading.attachment_id, None);
    assert!(
        desktop
            .pending_rcs_media_receive_tasks()
            .unwrap()
            .is_empty()
    );

    drop(desktop);
    let reopened = unlocked(&desktop_config, &vault);
    assert_eq!(
        reopened
            .rcs_media_receive_view(message.payload.record.message_id, 0)
            .unwrap()
            .unwrap(),
        oversized
    );
    assert_eq!(
        reopened
            .rcs_media_receive_view(message.payload.record.message_id, 1)
            .unwrap()
            .unwrap(),
        downloading
    );
    assert!(
        reopened
            .pending_rcs_media_receive_tasks()
            .unwrap()
            .is_empty()
    );

    for (index, envelope) in server.log.iter().enumerate() {
        assert_eq!(
            reopened.ingest(envelope, Cursor(index as u64 + 1)).unwrap(),
            IngestResult::Duplicate
        );
    }
    assert_eq!(reopened.apply_pending(100).unwrap().applied, 0);
    assert_eq!(
        reopened
            .rcs_media_receive_view(message.payload.record.message_id, 1)
            .unwrap()
            .unwrap(),
        downloading
    );

    drop(gateway);
    let locked_gateway = open(&gateway_config);
    locked_gateway
        .mark_rcs_media_download_state(
            message.payload.record.message_id,
            1,
            RcsMediaSyncState::Failed,
        )
        .unwrap();
    let connection = sqlcipher(&gateway_config.database_path);
    let (sequence, plain): (i64, Vec<u8>) = connection
        .query_row(
            "SELECT seq,plain FROM outbox WHERE state='unsealed' ORDER BY seq DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(
        !plain
            .windows(b"google-ref".len())
            .any(|part| part == b"google-ref")
    );
    assert!(
        !plain
            .windows(b"file_key".len())
            .any(|part| part == b"file_key")
    );
    assert!(
        !plain
            .windows(b"media_ref".len())
            .any(|part| part == b"media_ref")
    );
    let mut payload: serde_json::Value = serde_json::from_slice(&plain).unwrap();
    assert_eq!(payload["kind"], "rcs_media_state");
    payload["metadata"]["display_name"] = serde_json::json!("conflict.jpg");
    let conflicting_plain = serde_json::to_vec(&payload).unwrap();
    connection
        .execute(
            "UPDATE outbox SET plain=? WHERE seq=? AND state='unsealed'",
            rusqlite::params![&conflicting_plain, sequence],
        )
        .unwrap();
    drop(connection);
    locked_gateway
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    assert_eq!(server.upload(&locked_gateway), 1);
    let conflict_report = server.sync(&reopened);
    assert_eq!(conflict_report.quarantined, 1, "{conflict_report:?}");
    assert_eq!(
        reopened
            .rcs_media_receive_view(message.payload.record.message_id, 1)
            .unwrap()
            .unwrap(),
        downloading,
        "conflicting authenticated metadata must not replace the canonical slot"
    );

    let wrong_producer_config = config(&directory, "wrong-producer", &vault);
    let wrong_producer = open(&wrong_producer_config);
    publish_current_gateway(&wrong_producer, &wrong_producer_config);
    let connection = sqlcipher(&wrong_producer_config.database_path);
    connection
        .execute(
            "UPDATE outbox SET plain=? WHERE seq=(SELECT MAX(seq) FROM outbox WHERE state='unsealed')",
            [&conflicting_plain],
        )
        .unwrap();
    drop(connection);
    wrong_producer
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    assert_eq!(server.upload(&wrong_producer), 1);
    let wrong_report = server.sync(&reopened);
    assert_eq!(wrong_report.quarantined, 1, "{wrong_report:?}");
    assert_eq!(
        reopened
            .rcs_media_receive_view(message.payload.record.message_id, 1)
            .unwrap()
            .unwrap(),
        downloading,
        "an authenticated state from a device other than the parent producer must not replace the slot"
    );

    for envelope in &server.log {
        let wire = serde_json::to_vec(envelope).unwrap();
        assert!(
            !wire
                .windows(b"google-ref".len())
                .any(|part| part == b"google-ref")
        );
        assert!(
            !wire
                .windows(b"file_key".len())
                .any(|part| part == b"file_key")
        );
    }
}

fn sqlcipher(path: &std::path::Path) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
}

#[test]
fn sealed_media_states_wait_across_reopen_until_the_parent_arrives() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "deferred-gateway", &vault);
    let desktop_config = config(&directory, "deferred-desktop", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    publish_current_gateway(&gateway, &gateway_config);
    let conversation_id = ConversationId::new();
    let ownership = gateway
        .begin_rcs_link_ownership(conversation_id, 0)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);

    assert_eq!(
        gateway
            .ingest_rcs_projected_record(
                conversation_id,
                ownership.ownership_generation,
                incoming_media_record(),
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    let message_id = only_message(&gateway, conversation_id)
        .payload
        .record
        .message_id;
    let batch_start = server.log.len();
    assert_eq!(server.upload(&gateway), 5);
    let batch = server.log.split_off(batch_start);

    let wrong_config = config(&directory, "deferred-wrong", &vault);
    let wrong = open(&wrong_config);
    publish_current_gateway(&wrong, &wrong_config);
    let connection = sqlcipher(&wrong_config.database_path);
    connection
        .execute(
            "UPDATE outbox SET plain=? WHERE seq=(SELECT MAX(seq) FROM outbox WHERE state='unsealed')",
            [serde_json::to_vec(&serde_json::json!({
                "kind": "rcs_media_state",
                "message_id": message_id,
                "attachment_index": 0,
                "state": "failed",
                "metadata": {
                    "media_type": "video/mp4",
                    "display_name": "oversized.mp4",
                    "plaintext_bytes": 32 * 1024 * 1024 + 1,
                },
            }))
            .unwrap()],
        )
        .unwrap();
    drop(connection);
    wrong
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    assert_eq!(server.upload(&wrong), 1);
    server.log.extend([batch[1].clone(), batch[2].clone()]);

    let deferred = server.sync(&desktop);
    assert_eq!(deferred.quarantined, 0, "{deferred:?}");
    assert!(desktop.messages(conversation_id).unwrap().is_empty());
    drop(desktop);

    let reopened = unlocked(&desktop_config, &vault);
    server
        .log
        .extend([batch[0].clone(), batch[3].clone(), batch[4].clone()]);
    let reconciled = server.sync(&reopened);
    assert_eq!(reconciled.quarantined, 0, "{reconciled:?}");
    let message = only_message(&reopened, conversation_id);
    assert_eq!(
        reopened
            .rcs_media_receive_view(message.payload.record.message_id, 0)
            .unwrap()
            .unwrap()
            .state,
        RcsMediaSyncState::TooLarge
    );
    assert_eq!(
        reopened
            .rcs_media_receive_view(message.payload.record.message_id, 1)
            .unwrap()
            .unwrap()
            .state,
        RcsMediaSyncState::Downloading
    );
    assert!(
        reopened
            .pending_rcs_media_receive_tasks()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn schema_55_backfills_local_metadata_and_keeps_remote_only_legacy_rows_unrendered() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "schema-55", &vault);
    let client = unlocked(&config, &vault);
    let backfilled_message = MessageId::new();
    let remote_only_message = MessageId::new();
    let connection = sqlcipher(&config.database_path);
    connection
        .execute(
            "INSERT INTO rcs_media_receive(message_id,attachment_index,state) VALUES(?,0,'downloading'),(?,0,'failed')",
            [
                backfilled_message.to_string(),
                remote_only_message.to_string(),
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO rcs_media_receive_local(message_id,attachment_index,source_gateway_id,media_ref,media_type,display_name,plaintext_bytes) VALUES(?,0,?,'local-secret','image/png','before-upgrade.png',4567)",
            [backfilled_message.to_string(), config.device_id.to_string()],
        )
        .unwrap();
    connection
        .execute_batch(
            "ALTER TABLE rcs_media_receive DROP COLUMN plaintext_bytes;
             ALTER TABLE rcs_media_receive DROP COLUMN display_name;
             ALTER TABLE rcs_media_receive DROP COLUMN media_type;
             UPDATE schema_meta SET version=55;",
        )
        .unwrap();
    drop(connection);
    drop(client);

    let upgraded = unlocked(&config, &vault);
    let view = upgraded
        .rcs_media_receive_view(backfilled_message, 0)
        .unwrap()
        .unwrap();
    assert_eq!(view.media_type, "image/png");
    assert_eq!(view.display_name, "before-upgrade.png");
    assert_eq!(view.plaintext_bytes, 4_567);
    assert_eq!(view.state, RcsMediaSyncState::Downloading);
    assert_eq!(
        upgraded.rcs_media_receive_view(remote_only_message, 0),
        Ok(None),
        "a legacy replicated row without local metadata remains non-renderable rather than failing the whole view"
    );
}

#[test]
fn reverse_sequence_media_states_converge_to_the_newest_state() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "sequence-gateway", &vault);
    let desktop_config = config(&directory, "sequence-desktop", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    publish_current_gateway(&gateway, &gateway_config);
    let conversation_id = ConversationId::new();
    let ownership = gateway
        .begin_rcs_link_ownership(conversation_id, 0)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);

    assert_eq!(
        gateway
            .ingest_rcs_projected_record(
                conversation_id,
                ownership.ownership_generation,
                incoming_media_record(),
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    let message_id = only_message(&gateway, conversation_id)
        .payload
        .record
        .message_id;
    let start = server.log.len();
    assert_eq!(server.upload(&gateway), 5);
    let initial = server.log.split_off(start);
    gateway
        .mark_rcs_media_download_state(message_id, 1, RcsMediaSyncState::Failed)
        .unwrap();
    assert_eq!(server.upload(&gateway), 1);
    let newer = server.log.pop().unwrap();
    server.log.extend([
        newer,
        initial[2].clone(),
        initial[0].clone(),
        initial[1].clone(),
        initial[3].clone(),
        initial[4].clone(),
    ]);

    let report = server.sync(&desktop);
    assert_eq!(report.quarantined, 0, "{report:?}");
    assert_eq!(
        desktop
            .rcs_media_receive_view(message_id, 1)
            .unwrap()
            .unwrap()
            .state,
        RcsMediaSyncState::Failed
    );
}

#[test]
fn deferred_real_stored_descriptor_survives_newer_failures_in_both_orders() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "stored-gateway", &vault);
    let first_config = config(&directory, "stored-first", &vault);
    let failed_first_config = config(&directory, "failed-first", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let first = unlocked(&first_config, &vault);
    let failed_first = unlocked(&failed_first_config, &vault);
    publish_current_gateway(&gateway, &gateway_config);
    let conversation_id = ConversationId::new();
    let ownership = gateway
        .begin_rcs_link_ownership(conversation_id, 0)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&first);
    server.sync(&failed_first);

    assert_eq!(
        gateway
            .ingest_rcs_projected_record(
                conversation_id,
                ownership.ownership_generation,
                incoming_media_record(),
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    let message_id = only_message(&gateway, conversation_id)
        .payload
        .record
        .message_id;
    let token = match gateway
        .admit_rcs_media_download(message_id, 1, Some(u64::MAX))
        .unwrap()
    {
        ReceiveDownloadAdmission::Held { token } => token,
        other => panic!("unexpected admission: {other:?}"),
    };
    let plaintext = directory.path().join("stored-photo.jpg");
    std::fs::write(&plaintext, vec![7_u8; 12_345]).unwrap();
    let attachment = gateway
        .import_rcs_media_download(message_id, 1, &token, &plaintext)
        .unwrap();
    gateway
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    gateway.finalize_rcs_media_download(message_id, 1).unwrap();
    gateway
        .demand_rcs_media_retry(message_id, 1, gateway_config.device_id)
        .unwrap();
    let connection = sqlcipher(&gateway_config.database_path);
    connection
        .execute(
            "UPDATE outbox SET plain=? WHERE seq=(SELECT MAX(seq) FROM outbox WHERE state='unsealed')",
            [serde_json::to_vec(&serde_json::json!({
                "kind": "rcs_media_state",
                "message_id": message_id,
                "attachment_index": 1,
                "state": "failed",
                "metadata": {
                    "media_type": "image/jpeg",
                    "display_name": "photo.jpg",
                    "plaintext_bytes": 12_345,
                },
            }))
            .unwrap()],
        )
        .unwrap();
    drop(connection);

    let start = server.log.len();
    assert_eq!(server.upload(&gateway), 7);
    let batch = server.log.split_off(start);
    let prefix = server.log.clone();
    let mut stored_then_failed = Server {
        log: prefix.clone(),
    };
    stored_then_failed
        .log
        .extend([batch[5].clone(), batch[6].clone(), batch[1].clone()]);
    let mut failed_then_stored = Server { log: prefix };
    failed_then_stored
        .log
        .extend([batch[6].clone(), batch[5].clone(), batch[1].clone()]);
    assert_eq!(stored_then_failed.sync(&first).quarantined, 0);
    assert_eq!(failed_then_stored.sync(&failed_first).quarantined, 0);
    drop(first);
    drop(failed_first);

    let first = unlocked(&first_config, &vault);
    let failed_first = unlocked(&failed_first_config, &vault);
    let tail = [
        batch[0].clone(),
        batch[2].clone(),
        batch[3].clone(),
        batch[4].clone(),
    ];
    stored_then_failed.log.extend(tail.clone());
    failed_then_stored.log.extend(tail);
    for (recipient, replica) in [
        (&first, &stored_then_failed),
        (&failed_first, &failed_then_stored),
    ] {
        let report = replica.sync(recipient);
        assert_eq!(report.quarantined, 0, "{report:?}");
        let stored = recipient
            .rcs_media_receive_view(message_id, 1)
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, RcsMediaSyncState::Stored);
        assert_eq!(stored.attachment_id, Some(attachment.attachment_id));
        assert_eq!(
            recipient
                .rcs_media_receive_view(message_id, 0)
                .unwrap()
                .unwrap()
                .state,
            RcsMediaSyncState::TooLarge
        );
    }
}

#[test]
fn outgoing_state_waits_for_actual_command_authority() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "outgoing-sender", &vault);
    let gateway_config = config(&directory, "outgoing-gateway", &vault);
    let attacker_config = config(&directory, "outgoing-attacker", &vault);
    let observer_config = config(&directory, "outgoing-observer", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let attacker = unlocked(&attacker_config, &vault);
    let observer = unlocked(&observer_config, &vault);

    let plaintext = directory.path().join("outgoing.jpg");
    std::fs::write(&plaintext, [9_u8; 32]).unwrap();
    let attachment = sender
        .prepare_attachment(&plaintext, "image/jpeg", "outgoing.jpg")
        .unwrap();
    sender
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let queued = sender
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: String::new(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(&gateway_config),
        )
        .unwrap();

    publish_current_gateway(&gateway, &gateway_config);
    publish_current_gateway(&attacker, &attacker_config);
    let mut server = Server::default();
    assert_eq!(server.upload(&gateway), 1);
    assert_eq!(server.upload(&attacker), 1);
    assert_eq!(server.sync(&observer).quarantined, 0);
    drop(gateway);
    drop(attacker);
    let gateway = open(&gateway_config);
    let attacker = open(&attacker_config);
    for (client, config, state) in [
        (&gateway, &gateway_config, "sent"),
        (&attacker, &attacker_config, "not_sent"),
    ] {
        client
            .demand_rcs_media_retry(queued.message_id, 0, config.device_id)
            .unwrap();
        let connection = sqlcipher(&config.database_path);
        let mut state_payload = serde_json::json!({
            "kind": "rcs_media_state",
            "message_id": queued.message_id,
            "attachment_index": 0,
            "state": state,
        });
        if state == "not_sent" {
            state_payload["reason"] = serde_json::json!("rejected");
        }
        assert_eq!(
            connection
                .execute(
                    "UPDATE outbox SET plain=? WHERE seq=(SELECT MAX(seq) FROM outbox WHERE state='unsealed')",
                    [serde_json::to_vec(&state_payload).unwrap()],
                )
                .unwrap(),
            1
        );
    }
    gateway
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    attacker
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    assert_eq!(server.upload(&gateway), 1);
    assert_eq!(server.upload(&attacker), 1);
    let staged = server.sync(&observer);
    assert_eq!(staged.quarantined, 0, "{staged:?}");
    assert_eq!(staged.applied, 2, "{staged:?}");
    let connection = sqlcipher(&observer_config.database_path);
    let pending: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pending_rcs_media_states WHERE message_id=?",
            [queued.message_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 2);

    assert_eq!(server.upload(&sender), 1);
    let report = server.sync(&observer);
    assert_eq!(report.quarantined, 0, "{report:?}");
    assert_eq!(
        observer
            .rcs_media_outgoing_status(queued.message_id, 0)
            .unwrap()
            .unwrap()
            .state,
        RcsMediaSyncState::Sent
    );
    let pending: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pending_rcs_media_states WHERE message_id=?",
            [queued.message_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        pending, 0,
        "the authorized and denied candidates were reconciled"
    );
}

#[test]
fn compacted_media_snapshot_retains_sparse_stored_descriptor_and_all_placeholders() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "compact-gateway", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    publish_current_gateway(&gateway, &gateway_config);
    let conversation_id = ConversationId::new();
    let ownership = gateway
        .begin_rcs_link_ownership(conversation_id, 0)
        .unwrap();
    gateway
        .ingest_rcs_projected_record(
            conversation_id,
            ownership.ownership_generation,
            incoming_media_record(),
        )
        .unwrap();
    let parent = only_message(&gateway, conversation_id).payload;
    let message_id = parent.record.message_id;
    let mut server = Server::default();
    server.upload(&gateway);
    for state in [RcsMediaSyncState::Failed, RcsMediaSyncState::Downloading] {
        gateway
            .mark_rcs_media_download_state(message_id, 1, state)
            .unwrap();
        server.upload(&gateway);
    }
    let token = match gateway
        .admit_rcs_media_download(message_id, 1, Some(u64::MAX))
        .unwrap()
    {
        ReceiveDownloadAdmission::Held { token } => token,
        other => panic!("unexpected admission: {other:?}"),
    };
    let plaintext = directory.path().join("snapshot-photo.jpg");
    std::fs::write(&plaintext, vec![7_u8; 12_345]).unwrap();
    let attachment = gateway
        .import_rcs_media_download(message_id, 1, &token, &plaintext)
        .unwrap();
    gateway
        .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    gateway.finalize_rcs_media_download(message_id, 1).unwrap();
    server.upload(&gateway);
    let stored_envelope = server.log.last().unwrap();
    let compact = stored_envelope
        .compaction
        .as_ref()
        .expect("media states must compact by original slot");
    assert!(
        !compact.terminal,
        "Stored history cannot expire as a terminal root"
    );
    assert!(!compact.supersedes.is_empty());
    gateway
        .mark_rcs_media_download_state(message_id, 1, RcsMediaSyncState::Failed)
        .unwrap();
    assert_eq!(
        server.upload(&gateway),
        0,
        "stale failure must not replace the only descriptor checkpoint"
    );

    let superseded: std::collections::HashSet<_> = server
        .log
        .iter()
        .filter_map(|event| event.compaction.as_ref())
        .flat_map(|metadata| &metadata.supersedes)
        .map(|reference| (reference.producer_device_id, reference.producer_sequence.0))
        .collect();
    let records: Vec<_> = server
        .log
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            !superseded.contains(&(event.producer_device_id, event.producer_sequence.0))
        })
        .map(|(index, event)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: event.clone(),
        })
        .collect();
    let restored_config = config(&directory, "compact-restored", &vault);
    let restored = unlocked(&restored_config, &vault);
    let session = restored
        .begin_snapshot_with_compaction(
            Cursor(server.log.len() as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
            Some(1),
        )
        .unwrap();
    for page in records.chunks(2) {
        restored
            .append_snapshot_page(session.generation, page)
            .unwrap();
    }
    restored.finish_snapshot(session.generation).unwrap();
    while restored.apply_pending(100).unwrap().applied > 0 {}
    let message = only_message(&restored, conversation_id);
    assert_eq!(message.payload.body, parent.body);
    assert_eq!(message.payload.sender_address, parent.sender_address);
    assert_eq!(message.payload.sent_at_ms, parent.sent_at_ms);
    assert_eq!(
        restored
            .rcs_media_receive_view(message_id, 0)
            .unwrap()
            .unwrap()
            .state,
        RcsMediaSyncState::TooLarge
    );
    let stored = restored
        .rcs_media_receive_view(message_id, 1)
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, RcsMediaSyncState::Stored);
    assert_eq!(stored.attachment_id, Some(attachment.attachment_id));
    assert_eq!(stored.display_name, "photo.jpg");
    assert!(
        restored
            .pending_rcs_media_receive_tasks()
            .unwrap()
            .is_empty()
    );
    let db = sqlcipher(&restored_config.database_path);
    let position: i64 = db
        .query_row(
            "SELECT position FROM message_attachments WHERE message_id=? AND attachment_id=?",
            rusqlite::params![message_id.to_string(), attachment.attachment_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(position, 1);
    let available: i64 = db.query_row("SELECT COUNT(*) FROM attachments WHERE attachment_id=? AND remote_object_id IS NOT NULL",
        [attachment.attachment_id.to_string()], |row| row.get(0)).unwrap();
    assert_eq!(available, 1);
}

#[test]
fn retry_demand_uses_original_gateway_coalesces_and_is_consumed_by_admission() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "retry-gateway", &vault);
    let desktop_config = config(&directory, "retry-desktop", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    publish_current_gateway(&gateway, &gateway_config);
    let conversation = ConversationId::new();
    let ownership = gateway.begin_rcs_link_ownership(conversation, 0).unwrap();
    gateway
        .ingest_rcs_projected_record(
            conversation,
            ownership.ownership_generation,
            incoming_media_record(),
        )
        .unwrap();
    let id = only_message(&gateway, conversation)
        .payload
        .record
        .message_id;
    gateway
        .mark_rcs_media_download_state(id, 1, RcsMediaSyncState::QuotaFull)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    assert_eq!(server.sync(&desktop).quarantined, 0);
    desktop
        .demand_rcs_media_retry_for_attachment(id, 1)
        .unwrap();
    desktop
        .demand_rcs_media_retry_for_attachment(id, 1)
        .unwrap();
    assert_eq!(
        server.upload(&desktop),
        1,
        "one pending request per original slot"
    );
    assert_eq!(server.sync(&gateway).quarantined, 0);
    let db = sqlcipher(&gateway_config.database_path);
    let demands = || {
        db.query_row(
            "SELECT COUNT(*) FROM rcs_media_retry_demand WHERE message_id=?",
            [id.to_string()],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    };
    assert_eq!(demands(), 1);
    assert!(matches!(
        gateway
            .admit_rcs_media_download(id, 1, Some(u64::MAX))
            .unwrap(),
        ReceiveDownloadAdmission::Held { .. }
    ));
    assert_eq!(
        demands(),
        0,
        "the scheduled transfer consumes the coalesced demand"
    );
    assert!(
        desktop
            .demand_rcs_media_retry_for_attachment(id, 0)
            .is_err()
    );
    assert!(
        desktop
            .pending_rcs_media_receive_tasks()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn historical_oversized_media_still_replicates_its_manifest() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "history-gateway", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&config(&directory, "history-desktop", &vault), &vault);
    publish_current_gateway(&gateway, &gateway_config);
    let conversation = ConversationId::new();
    let ownership = gateway.begin_rcs_link_ownership(conversation, 0).unwrap();
    let mut record = incoming_media_record();
    record.sent_at_ms = Some(now_ms() + 1_000);
    assert_eq!(
        gateway
            .ingest_rcs_receive_observation(
                conversation,
                ownership.ownership_generation,
                record,
                None,
                RcsObservationProvenance::Historical
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    let mut server = Server::default();
    server.upload(&gateway);
    assert_eq!(server.sync(&desktop).quarantined, 0);
    let id = only_message(&desktop, conversation)
        .payload
        .record
        .message_id;
    assert_eq!(
        desktop
            .rcs_media_receive_view(id, 0)
            .unwrap()
            .unwrap()
            .state,
        RcsMediaSyncState::TooLarge
    );
}

#[test]
fn small_receive_admission_uses_its_actual_ciphertext_bound() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&directory, "small-admission", &vault);
    let client = unlocked(&cfg, &vault);
    publish_current_gateway(&client, &cfg);
    let conversation = ConversationId::new();
    let ownership = client.begin_rcs_link_ownership(conversation, 0).unwrap();
    client
        .ingest_rcs_projected_record(
            conversation,
            ownership.ownership_generation,
            incoming_media_record(),
        )
        .unwrap();
    let id = only_message(&client, conversation)
        .payload
        .record
        .message_id;
    assert_eq!(
        client
            .admit_rcs_media_download(id, 1, Some(64 * 1024))
            .unwrap(),
        ReceiveDownloadAdmission::CapacityUnavailable,
        "headroom between the framing minimum and conservative maximum is not confirmed quota exhaustion"
    );
    assert!(matches!(
        client
            .admit_rcs_media_download(id, 1, Some(1024 * 1024))
            .unwrap(),
        ReceiveDownloadAdmission::Held { .. }
    ));
}

#[test]
fn abandoned_admissions_can_be_reacquired_after_reopen() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&directory, "admission-reopen", &vault);
    let client = unlocked(&cfg, &vault);
    publish_current_gateway(&client, &cfg);
    let conversation = ConversationId::new();
    let ownership = client.begin_rcs_link_ownership(conversation, 0).unwrap();
    let mut record = incoming_media_record();
    record.attachments = (0..3)
        .map(|index| RcsIncomingAttachment {
            original_index: index,
            plaintext_bytes: 32 * 1024 * 1024,
            too_large: false,
            media_type: "video/mp4".into(),
            display_name: "video.mp4".into(),
            media_ref: format!("private-{index}"),
        })
        .collect();
    client
        .ingest_rcs_projected_record(conversation, ownership.ownership_generation, record)
        .unwrap();
    let id = only_message(&client, conversation)
        .payload
        .record
        .message_id;
    for index in 0..3 {
        assert!(matches!(
            client
                .admit_rcs_media_download(id, index, Some(u64::MAX))
                .unwrap(),
            ReceiveDownloadAdmission::Held { .. }
        ));
    }
    drop(client);
    let reopened = unlocked(&cfg, &vault);
    assert!(matches!(
        reopened
            .admit_rcs_media_download(id, 0, Some(u64::MAX))
            .unwrap(),
        ReceiveDownloadAdmission::Held { .. }
    ));
}
