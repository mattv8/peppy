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
fn retirement_scan_is_a_primary_key_range_search_not_a_full_scan() {
    let directory = TempDir::new().unwrap();
    let (client, device) = open_unlocked(&directory, DeviceId::new());
    let guard = client.lock().unwrap();
    let mut statement = guard
        .conn
        .prepare(&format!("EXPLAIN QUERY PLAN {SWALLOWED_SCAN_SQL}"))
        .unwrap();
    let plan: Vec<String> = statement
        .query_map(
            params![
                device.to_string(),
                0_i64,
                RETIRE_SWALLOWED_SCAN as i64,
                rcs_group::SEND_REASON_PREFIX
            ],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|line| line.contains("SEARCH l USING INTEGER PRIMARY KEY (rowid>?)")),
        "the scan starts at the saved position by primary key: {plan:?}"
    );
    assert!(
        !plan.iter().any(|line| line == "SCAN l"),
        "no full ledger scan: {plan:?}"
    );
    assert!(
        plan.iter()
            .filter(|line| line.starts_with("SEARCH") && !line.contains("rowid>?"))
            .all(|line| line.contains("USING")),
        "every per-row probe is indexed: {plan:?}"
    );
}

#[test]
fn a_window_resolves_send_again_markers() {
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

/// One vault's key profile and check header. Every device of a vault must share the same profile.
fn vault_profile(vault_id: VaultId) -> (KeyProfile, VaultCheckHeader) {
    let profile = KeyProfile::new(vault_id.0, 1).unwrap();
    let root = derive_root_key(PASSPHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    (profile, header)
}

/// Opens one more client on an existing vault, so two devices can share one vault in a test.
fn open_client(
    directory: &TempDir,
    name: &str,
    vault_id: VaultId,
    profile: &KeyProfile,
    header: &VaultCheckHeader,
    device: DeviceId,
) -> Client {
    let client = Client::open(
        ClientConfig {
            database_path: directory.path().join(format!("{name}.db")),
            vault_id,
            device_id: device,
        },
        DatabaseKey::new(&[42; 32]).unwrap(),
    )
    .unwrap();
    client.unlock(profile, header, PASSPHRASE).unwrap();
    client.set_server_compaction_state(true, true).unwrap();
    client
}

/// A ledger row and its command row, as the ingest path leaves them. `historical` decides whether the
/// row is a snapshot import (eligible for retirement) or a live command (never eligible).
fn insert_ledger_row(conn: &Connection, gateway: DeviceId, historical: bool) {
    let command = CommandId::new();
    conn.execute(
        "INSERT INTO commands(command_id,message_id,gateway_device_id) VALUES(?,?,?)",
        params![
            command.to_string(),
            MessageId::new().to_string(),
            gateway.to_string()
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO command_ledger(command_id,key_epoch,subscription_id,payload,retired,historical) VALUES(?,1,'sim','{}',0,?)",
        params![command.to_string(), historical],
    )
    .unwrap();
}

fn scan_position(client: &Client) -> i64 {
    client
        .lock()
        .unwrap()
        .conn
        .query_row(
            "SELECT v FROM metadata WHERE k='swallowed_retirement_position_v1'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn retirement_examines_at_most_the_scan_bound_per_call_and_covers_a_long_ledger() {
    let directory = TempDir::new().unwrap();
    let (client, device) = open_unlocked(&directory, DeviceId::new());
    {
        let guard = client.lock().unwrap();
        for _ in 0..2_000 {
            insert_ledger_row(&guard.conn, device, false);
        }
        for _ in 0..3 {
            insert_ledger_row(&guard.conn, device, true);
        }
    }

    // Each call examines at most the bound (1,000 rows) and stores where it stopped.
    assert_eq!(client.retire_swallowed_commands().unwrap(), 0);
    assert_eq!(scan_position(&client), RETIRE_SWALLOWED_SCAN as i64);
    assert!(client.swallowed_retirement_more_remains().unwrap());

    assert_eq!(client.retire_swallowed_commands().unwrap(), 0);
    assert_eq!(scan_position(&client), 2 * RETIRE_SWALLOWED_SCAN as i64);
    assert!(client.swallowed_retirement_more_remains().unwrap());

    // The last call reaches the three eligible rows and the end of the ledger.
    assert_eq!(client.retire_swallowed_commands().unwrap(), 3);
    assert_eq!(scan_position(&client), 2_003);
    assert!(!client.swallowed_retirement_more_remains().unwrap());

    // Nothing left: later calls examine nothing and retire nothing.
    assert_eq!(client.retire_swallowed_commands().unwrap(), 0);
    assert_eq!(scan_position(&client), 2_003);
}

#[test]
fn a_command_ingested_without_route_records_has_no_route_and_cannot_be_copied() {
    let directory = TempDir::new().unwrap();
    let vault_id = VaultId::new();
    let (profile, header) = vault_profile(vault_id);
    let author = open_client(
        &directory,
        "author",
        vault_id,
        &profile,
        &header,
        DeviceId::new(),
    );
    let gateway = DeviceId::new();
    let carrier = GatewayRoute::Carrier {
        gateway_device_id: gateway,
        subscription_id: "sim-1".into(),
    };
    let queued = author
        .queue_send(
            OutgoingSms {
                conversation_id: ConversationId::new(),
                recipients: vec!["+15555550100".into()],
                body: "authored elsewhere".into(),
            },
            carrier.clone(),
        )
        .unwrap();
    let envelope = author
        .pending_outbox()
        .unwrap()
        .into_iter()
        .find(|envelope| envelope.command_id == Some(queued.command_id))
        .unwrap();

    // A non-authoring device applies the command, then loses its route record, as a device that
    // ingested before route records existed would have.
    let peer = open_client(
        &directory,
        "peer",
        vault_id,
        &profile,
        &header,
        DeviceId::new(),
    );
    assert_eq!(
        peer.ingest(&envelope, Cursor(1)).unwrap(),
        IngestResult::Journaled
    );
    peer.apply_pending(usize::MAX).unwrap();
    peer.lock()
        .unwrap()
        .conn
        .execute(
            "DELETE FROM metadata WHERE k=?",
            [command_route_key(queued.command_id)],
        )
        .unwrap();

    let shown = peer.message_by_id(queued.message_id).unwrap().unwrap();
    assert_eq!(shown.send_route, None, "unknown, not guessed");
    assert_eq!(
        peer.send_again(queued.message_id, carrier),
        Err(Error::SendAgainNotAllowed)
    );
}
