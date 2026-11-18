//! L11 unit tests: raw ledger states that integration tests cannot reach through the public API.
use crate::*;
use peppy_crypto::create_vault_check_header;
use rusqlite::params;
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple";

fn open_unlocked(directory: &TempDir, device_id: DeviceId) -> (Client, DeviceId) {
    let vault_id = VaultId::new();
    let profile = KeyProfile::new(vault_id.0, 1).unwrap();
    let root = derive_root_key(PASSPHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    let client = Client::open(
        ClientConfig {
            database_path: directory.path().join("stranded.db"),
            vault_id,
            device_id,
        },
        DatabaseKey::new(&[41; 32]).unwrap(),
    )
    .unwrap();
    client.unlock(&profile, &header, PASSPHRASE).unwrap();
    client.set_server_compaction_state(true, true).unwrap();
    (client, device_id)
}

/// A historical ledger row whose command targets `gateway`, as a snapshot would import it.
fn swallowed(client: &Client, gateway: DeviceId) -> CommandId {
    let command = CommandId::new();
    let guard = client.lock().unwrap();
    guard
        .conn
        .execute(
            "INSERT INTO commands(command_id,message_id,gateway_device_id) VALUES(?,?,?)",
            params![
                command.to_string(),
                MessageId::new().to_string(),
                gateway.to_string()
            ],
        )
        .unwrap();
    guard
        .conn
        .execute(
            "INSERT INTO command_ledger(command_id,key_epoch,subscription_id,payload,retired,historical) VALUES(?,1,'sim','{}',0,1)",
            params![command.to_string()],
        )
        .unwrap();
    command
}

fn status_count(client: &Client, command: CommandId) -> i64 {
    client
        .lock()
        .unwrap()
        .conn
        .query_row(
            "SELECT COUNT(*) FROM send_status WHERE command_id=?",
            [command.to_string()],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn raw_swallowed_command_is_retired_and_the_control_passes() {
    let directory = TempDir::new().unwrap();
    let device = DeviceId::new();
    let (client, device) = open_unlocked(&directory, device);
    let command = swallowed(&client, device);
    assert_eq!(client.retire_swallowed_commands().unwrap(), 1);
    assert_eq!(status_count(&client, command), 1);
}

#[test]
fn raw_attempted_command_is_never_retired() {
    let directory = TempDir::new().unwrap();
    let (client, device) = open_unlocked(&directory, DeviceId::new());
    let command = swallowed(&client, device);
    client
        .lock()
        .unwrap()
        .conn
        .execute(
            "INSERT INTO attempts(command_id,state) VALUES(?,'attempt_recorded')",
            [command.to_string()],
        )
        .unwrap();
    assert_eq!(client.retire_swallowed_commands().unwrap(), 0);
}

#[test]
fn raw_rcs_media_command_is_never_retired() {
    let directory = TempDir::new().unwrap();
    let (client, device) = open_unlocked(&directory, DeviceId::new());
    let command = swallowed(&client, device);
    client
        .lock()
        .unwrap()
        .conn
        .execute(
            "INSERT INTO rcs_media_send_ledger(command_id,attachment_index,attachment_id,state) VALUES(?,0,?,'pending')",
            params![command.to_string(), AttachmentId::new().to_string()],
        )
        .unwrap();
    assert_eq!(client.retire_swallowed_commands().unwrap(), 0);
}

#[test]
fn raw_status_from_another_producer_beyond_waiting_blocks_retirement() {
    let directory = TempDir::new().unwrap();
    let (client, device) = open_unlocked(&directory, DeviceId::new());
    let command = swallowed(&client, device);
    client
        .lock()
        .unwrap()
        .conn
        .execute(
            "INSERT INTO send_status(command_id,producer_device_id,producer_sequence,state,status_at_ms,refusal_reason) VALUES(?,?,1,'sent',NULL,NULL)",
            params![command.to_string(), DeviceId::new().to_string()],
        )
        .unwrap();
    assert_eq!(client.retire_swallowed_commands().unwrap(), 0);
}

#[test]
fn raw_status_from_another_producer_in_a_waiting_state_still_retires() {
    let directory = TempDir::new().unwrap();
    let (client, device) = open_unlocked(&directory, DeviceId::new());
    let command = swallowed(&client, device);
    client
        .lock()
        .unwrap()
        .conn
        .execute(
            "INSERT INTO send_status(command_id,producer_device_id,producer_sequence,state,status_at_ms,refusal_reason) VALUES(?,?,1,'accepted_server',NULL,NULL)",
            params![command.to_string(), DeviceId::new().to_string()],
        )
        .unwrap();
    assert_eq!(client.retire_swallowed_commands().unwrap(), 1);
    assert_eq!(status_count(&client, command), 2);
}

#[test]
fn raw_status_this_device_produced_blocks_retirement() {
    let directory = TempDir::new().unwrap();
    let (client, device) = open_unlocked(&directory, DeviceId::new());
    let command = swallowed(&client, device);
    client
        .lock()
        .unwrap()
        .conn
        .execute(
            "INSERT INTO send_status(command_id,producer_device_id,producer_sequence,state,status_at_ms,refusal_reason) VALUES(?,?,1,'persisted_gateway',NULL,NULL)",
            params![command.to_string(), device.to_string()],
        )
        .unwrap();
    assert_eq!(client.retire_swallowed_commands().unwrap(), 0);
}

#[test]
fn raw_command_with_a_ledger_row_but_no_commands_row_is_never_retired() {
    let directory = TempDir::new().unwrap();
    let (client, _) = open_unlocked(&directory, DeviceId::new());
    let command = CommandId::new();
    client
        .lock()
        .unwrap()
        .conn
        .execute(
            "INSERT INTO command_ledger(command_id,key_epoch,subscription_id,payload,retired,historical) VALUES(?,1,'sim','{}',0,1)",
            params![command.to_string()],
        )
        .unwrap();
    assert_eq!(client.retire_swallowed_commands().unwrap(), 0);
}

#[test]
fn retirement_candidate_plan_is_recorded_for_the_bounded_query() {
    let directory = TempDir::new().unwrap();
    let (client, device) = open_unlocked(&directory, DeviceId::new());
    let guard = client.lock().unwrap();
    let mut statement = guard
        .conn
        .prepare(&format!("EXPLAIN QUERY PLAN {SWALLOWED_CANDIDATES_SQL}"))
        .unwrap();
    let plan: Vec<String> = statement
        .query_map(
            params![
                device.to_string(),
                RETIRE_SWALLOWED_BATCH as i64,
                rcs_group::SEND_REASON_PREFIX
            ],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    // Pinned plan: the query scans the ledger (`SCAN l`) and probes each row's other tables by
    // index. There is no ledger index on `historical` and no schema change is allowed, so the
    // scan is bounded by `LIMIT` and skipped entirely once the retirement watermark is set.
    assert!(plan.iter().any(|line| line == "SCAN l"), "plan: {plan:?}");
    assert!(
        plan.iter()
            .filter(|line| line.starts_with("SEARCH"))
            .all(|line| line.contains("USING")),
        "every per-row probe is indexed: {plan:?}"
    );
}

#[test]
fn a_window_resolves_send_again_markers_with_one_lookup() {
    let directory = TempDir::new().unwrap();
    let (client, device) = open_unlocked(&directory, DeviceId::new());
    let gateway = DeviceId::new();
    let conversation = ConversationId::new();
    let route = GatewayRoute::Carrier {
        gateway_device_id: gateway,
        subscription_id: "sim-1".into(),
    };
    let mut message_ids = Vec::new();
    for number in 0..12 {
        let queued = client
            .queue_send(
                OutgoingSms {
                    conversation_id: conversation,
                    recipients: vec!["+15555550100".into()],
                    body: format!("window {number}"),
                },
                route.clone(),
            )
            .unwrap();
        message_ids.push(queued.message_id);
    }
    client.send_again(message_ids[5], route.clone()).unwrap();
    let _ = device;

    SENT_AGAIN_LOOKUPS.with(|count| count.set(0));
    let window = client
        .load_message_window(MessageWindowRequest::Around {
            conversation_id: conversation.to_string(),
            message_id: message_ids[6].to_string(),
        })
        .unwrap();

    assert!(
        window.messages.len() > 6,
        "the window holds several messages"
    );
    assert_eq!(
        SENT_AGAIN_LOOKUPS.with(|count| count.get()),
        1,
        "one batched marker lookup per window, never one per message"
    );
    let original = window
        .messages
        .iter()
        .find(|message| message.payload.record.message_id == message_ids[5])
        .unwrap();
    assert!(original.sent_again_message_id.is_some());
}

#[test]
fn a_stale_marker_is_treated_as_absent_and_overwritten_by_the_next_copy() {
    let directory = TempDir::new().unwrap();
    let (client, _) = open_unlocked(&directory, DeviceId::new());
    let gateway = DeviceId::new();
    let route = GatewayRoute::Carrier {
        gateway_device_id: gateway,
        subscription_id: "sim-1".into(),
    };
    let queued = client
        .queue_send(
            OutgoingSms {
                conversation_id: ConversationId::new(),
                recipients: vec!["+15555550100".into()],
                body: "stale".into(),
            },
            route.clone(),
        )
        .unwrap();
    let first = client.send_again(queued.message_id, route.clone()).unwrap();
    assert_eq!(
        client.send_again(queued.message_id, route.clone()),
        Err(Error::AlreadySentAgain)
    );
    // The copy row disappears (for example a local purge): the marker is now stale.
    client
        .lock()
        .unwrap()
        .conn
        .execute(
            "DELETE FROM messages WHERE id=?",
            params![first.message_id.to_string()],
        )
        .unwrap();

    let second = client.send_again(queued.message_id, route).unwrap();

    assert_ne!(second.message_id, first.message_id);
    let copy: String = client
        .lock()
        .unwrap()
        .conn
        .query_row(
            "SELECT v FROM metadata WHERE k=?",
            [send_again_key(&queued.message_id.to_string())],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(copy, second.message_id.to_string());
}

#[test]
fn media_original_recorded_on_an_rcs_link_is_refused_by_its_route() {
    let directory = TempDir::new().unwrap();
    let (client, _) = open_unlocked(&directory, DeviceId::new());
    let gateway = DeviceId::new();
    let source = directory.path().join("rcs-media.jpg");
    std::fs::write(&source, b"ciphertext source").unwrap();
    let attachment = client
        .prepare_attachment(&source, "image/jpeg", "rcs-media.jpg")
        .unwrap();
    // The state the compose path cannot reach for RCS media: an MMS-shaped message with an
    // attachment, queued on an RCS-link route.
    let queued = {
        let mut guard = client.lock().unwrap();
        let (conn, ctx) = guard.parts();
        let tx = conn.transaction().unwrap();
        let queued = queue_message(
            &tx,
            &ctx,
            OutgoingSms {
                conversation_id: ConversationId::new(),
                recipients: vec!["+15555550100".into()],
                body: "rcs media".into(),
            },
            &GatewayRoute::RcsLink {
                gateway_device_id: gateway,
            },
            local_references(&tx, &[attachment.attachment_id]).unwrap(),
            Transport::Mms,
            None,
            None,
            None,
        )
        .unwrap();
        tx.commit().unwrap();
        queued
    };
    let carrier = GatewayRoute::Carrier {
        gateway_device_id: gateway,
        subscription_id: "sim-1".into(),
    };
    assert_eq!(
        client.send_again(queued.message_id, carrier),
        Err(Error::SendAgainNotAllowed)
    );
}
