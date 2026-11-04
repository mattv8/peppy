//! Re-pair regression coverage for message-origin deduplication.

use crate::common::*;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn sms_row(
    generation: &str,
    id: &str,
    body: &str,
    sent_at_ms: i64,
    imported: bool,
) -> SmsAcquisitionInput {
    SmsAcquisitionInput {
        source: SmsSource {
            source_generation: generation.into(),
            subscription_id: "sim-1".into(),
            provider_message_id: id.into(),
            provider_thread_id: Some("thread-1".into()),
            broadcast_id: None,
        },
        direction: Direction::Incoming,
        sender_address: Some(ADDRESS.into()),
        recipients: vec![],
        body: body.into(),
        imported,
        sent_at_ms: Some(sent_at_ms),
        date_sent_ms: Some(sent_at_ms),
        creator: None,
        source_row_fingerprint: format!("{generation}:{id}:{sent_at_ms}"),
    }
}

fn outgoing_row(generation: &str, id: &str, body: &str, sent_at_ms: i64) -> SmsAcquisitionInput {
    let mut row = sms_row(generation, id, body, sent_at_ms, true);
    row.direction = Direction::Outgoing;
    row.sender_address = None;
    row.recipients = vec![ADDRESS.into()];
    row.date_sent_ms = Some(0);
    row.creator = Some("dev.peppy.mobile".into());
    row
}

fn capture_sms(client: &Client, row: SmsAcquisitionInput) -> Captured {
    let acquisition = client.begin_sms_acquisition(row).unwrap();
    client
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap()
}

fn mms_row(generation: &str, id: &str, observed_at_ms: i64) -> MmsAcquisitionInput {
    MmsAcquisitionInput {
        source: MmsSource {
            source_generation: generation.into(),
            subscription_id: "sim-1".into(),
            provider_message_id: id.into(),
            provider_thread_id: Some("thread-1".into()),
        },
        direction: Direction::Incoming,
        sender_address: Some(ADDRESS.into()),
        recipients: vec![],
        subject: Some("subject".into()),
        body: "mms body".into(),
        imported: true,
        observed_at_ms,
        is_sent_box: true,
        transaction_id: None,
    }
}

fn capture_mms(client: &Client, row: MmsAcquisitionInput) -> Captured {
    let acquisition = client.begin_mms_acquisition(row).unwrap();
    client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap()
}

fn capture_mms_with_parts(
    client: &Client,
    dir: &TempDir,
    row: MmsAcquisitionInput,
    parts: &[&[u8]],
) -> (Captured, Vec<AttachmentId>) {
    let acquisition = client.begin_mms_acquisition(row).unwrap();
    let attachment_ids = parts
        .iter()
        .enumerate()
        .map(|(position, bytes)| {
            let path = dir.path().join(format!("mms-part-{position}.png"));
            fs::write(&path, bytes).unwrap();
            let attachment_id = client
                .prepare_attachment(&path, "image/png", "part.png")
                .unwrap()
                .attachment_id;
            client
                .set_mms_acquisition_part(
                    &acquisition.acquisition_id,
                    &format!("part-{position}"),
                    attachment_id,
                )
                .unwrap();
            attachment_id
        })
        .collect();
    (
        client
            .complete_mms_acquisition(&acquisition.acquisition_id)
            .unwrap(),
        attachment_ids,
    )
}

fn replay_to(server: &Server, client: &Client) {
    client
        .observe_server_high_water(Cursor(server.log.len() as u64))
        .unwrap();
    server.sync(client);
    assert!(client.initial_replay_caught_up().unwrap());
}

#[test]
fn repaired_phone_claims_a_live_broadcast_when_history_uses_a_new_generation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a = unlocked(&config(&dir, "phone-a", &vault), &vault);
    let b = unlocked(&config(&dir, "phone-b", &vault), &vault);
    let receipt = now_ms();
    let live = a
        .capture_incoming_broadcast(
            incoming("meet at noon", "broadcast-1"),
            Some(receipt - 3_000),
            Some(receipt),
        )
        .unwrap();
    let imported = capture_sms(
        &a,
        sms_row(
            "install-a",
            "provider-1",
            "meet at noon",
            receipt - 3_000,
            true,
        ),
    );
    assert!(imported.duplicate);
    assert_eq!(imported.message_id, live.message_id);

    let mut server = Server::default();
    assert_eq!(server.upload(&a), 1);
    replay_to(&server, &b);

    let repaired = capture_sms(
        &b,
        sms_row(
            "install-b",
            "provider-1",
            "meet at noon",
            receipt - 3_000,
            true,
        ),
    );
    assert!(repaired.duplicate);
    assert_eq!(repaired.message_id, live.message_id);
    assert!(b.pending_outbox().unwrap().is_empty());
}

#[test]
fn repaired_phone_claims_each_same_body_sms_once() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a = unlocked(&config(&dir, "phone-a", &vault), &vault);
    let b = unlocked(&config(&dir, "phone-b", &vault), &vault);
    let first_at = now_ms();
    let first = a
        .capture_incoming_broadcast(
            incoming("same body", "broadcast-1"),
            Some(first_at),
            Some(first_at),
        )
        .unwrap();
    let second = a
        .capture_incoming_broadcast(
            incoming("same body", "broadcast-2"),
            Some(first_at + 30_000),
            Some(first_at + 30_000),
        )
        .unwrap();
    let mut server = Server::default();
    assert_eq!(server.upload(&a), 2);
    replay_to(&server, &b);

    let replayed_first = capture_sms(
        &b,
        sms_row("install-b", "provider-1", "same body", first_at, true),
    );
    let replayed_second = capture_sms(
        &b,
        sms_row(
            "install-b",
            "provider-2",
            "same body",
            first_at + 30_000,
            true,
        ),
    );
    assert!(replayed_first.duplicate);
    assert!(replayed_second.duplicate);
    assert_eq!(replayed_first.message_id, first.message_id);
    assert_eq!(replayed_second.message_id, second.message_id);
    assert_ne!(replayed_first.message_id, replayed_second.message_id);
    assert!(b.pending_outbox().unwrap().is_empty());
}

#[test]
fn imported_history_stays_paused_before_replay_while_live_capture_publishes() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "phone", &vault), &vault);
    assert!(!client.observe_history_import_gate_at(0).unwrap());
    assert!(client.observe_history_import_gate_at(600_001).unwrap());

    let plans = client.plan_sms_scans("install", "provider", "10").unwrap();
    assert!(!plans.iter().find(|plan| !plan.imported).unwrap().paused);
    assert!(plans.iter().find(|plan| plan.imported).unwrap().paused);

    let live = capture_sms(
        &client,
        sms_row("install", "live", "live still publishes", now_ms(), false),
    );
    assert!(!live.duplicate);
    assert_eq!(client.pending_outbox().unwrap().len(), 1);
}

#[test]
fn repaired_phone_does_not_publish_an_exact_mms_history_match() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a = unlocked(&config(&dir, "phone-a", &vault), &vault);
    let b = unlocked(&config(&dir, "phone-b", &vault), &vault);
    let captured = capture_mms(&a, mms_row("install-a", "mms-1", 1_700_000_000_000));
    let mut server = Server::default();
    assert_eq!(server.upload(&a), 1);
    replay_to(&server, &b);

    let repaired = capture_mms(&b, mms_row("install-b", "mms-1", 1_700_000_000_000));
    assert!(repaired.duplicate);
    assert_eq!(repaired.message_id, captured.message_id);
    assert!(b.pending_outbox().unwrap().is_empty());
}

#[test]
fn repaired_phone_does_not_publish_an_exact_mms_history_match_with_parts() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a = unlocked(&config(&dir, "phone-a", &vault), &vault);
    let b = unlocked(&config(&dir, "phone-b", &vault), &vault);
    let observed_at_ms = 1_700_000_000_000;
    let (captured, source_parts) = capture_mms_with_parts(
        &a,
        &dir,
        mms_row("install-a", "mms-1", observed_at_ms),
        &[b"small", b"a larger image part"],
    );
    for attachment_id in &source_parts {
        a.mark_attachment_uploaded(*attachment_id, &uuid::Uuid::new_v4().to_string())
            .unwrap();
    }
    let mut server = Server::default();
    assert_eq!(server.upload(&a), 1);
    replay_to(&server, &b);

    let (repaired, repaired_parts) = capture_mms_with_parts(
        &b,
        &dir,
        mms_row("install-b", "mms-1", observed_at_ms),
        &[b"small", b"a larger image part"],
    );
    assert_ne!(
        source_parts, repaired_parts,
        "re-capture encrypts fresh attachments"
    );
    assert!(repaired.duplicate);
    assert_eq!(repaired.message_id, captured.message_id);
    assert!(b.pending_outbox().unwrap().is_empty());
}

#[test]
fn same_second_mms_burst_with_different_part_sizes_stays_distinct() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "phone", &vault), &vault);
    let observed_at_ms = 1_700_000_000_000;
    let (first, _) = capture_mms_with_parts(
        &client,
        &dir,
        mms_row("install", "mms-1", observed_at_ms),
        &[b"small", b"a larger image part"],
    );
    let (second, _) = capture_mms_with_parts(
        &client,
        &dir,
        mms_row("install", "mms-2", observed_at_ms),
        &[b"small", b"a differently sized image part"],
    );

    assert!(!first.duplicate);
    assert!(!second.duplicate);
    assert_ne!(first.message_id, second.message_id);
}

#[test]
fn reconciliation_alias_deduplicates_the_repaired_outgoing_provider_row() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop_config = config(&dir, "desktop", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let repaired = unlocked(&config(&dir, "repaired", &vault), &vault);
    let queued = desktop
        .queue_send(
            sms(ConversationId::new(), "desktop composed"),
            route(&gateway_config),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    server.sync(&gateway);
    server.upload(&gateway);
    assert!(matches!(
        gateway.begin_send_attempt(queued.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));

    let provider_time = now_ms() + 350;
    let reconciled = capture_sms(
        &gateway,
        outgoing_row("install-a", "provider-1", "desktop composed", provider_time),
    );
    assert!(reconciled.duplicate);
    assert_eq!(
        gateway.pending_outbox().unwrap().len(),
        1,
        "reconciliation publishes one alias event"
    );
    server.upload(&gateway);
    replay_to(&server, &repaired);

    let duplicate = capture_sms(
        &repaired,
        outgoing_row("install-b", "provider-1", "desktop composed", provider_time),
    );
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.message_id, reconciled.message_id);
    assert!(repaired.pending_outbox().unwrap().is_empty());
}

#[test]
fn message_envelopes_are_tagged_but_send_commands_are_not() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client_config = config(&dir, "phone", &vault);
    let client = unlocked(&client_config, &vault);
    client.set_server_origin_tags_supported(true).unwrap();
    capture_sms(
        &client,
        sms_row("install", "provider-1", "tagged", now_ms(), false),
    );
    let message = client.pending_outbox().unwrap().remove(0);
    assert!(message.origin_tag.is_some());

    client
        .queue_send(
            sms(ConversationId::new(), "untagged command"),
            route(&client_config),
        )
        .unwrap();
    let command = client.pending_outbox().unwrap().remove(0);
    assert_eq!(command.purpose, EnvelopePurpose::Command);
    assert!(command.origin_tag.is_none());
}

#[test]
fn message_tags_are_absent_until_the_server_advertises_support() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "phone", &vault), &vault);
    capture_sms(
        &client,
        sms_row("install", "provider-1", "untagged", now_ms(), false),
    );
    assert!(
        client
            .pending_outbox()
            .unwrap()
            .remove(0)
            .origin_tag
            .is_none()
    );
}

#[test]
fn replay_backfill_marks_existing_messages_ready_for_history_import() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "phone", &vault);
    let client = unlocked(&config, &vault);
    client
        .capture_incoming(incoming("pre-upgrade", "legacy"))
        .unwrap();
    drop(client);

    let conn = rusqlite::Connection::open(&config.database_path).unwrap();
    conn.execute_batch("PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\"; DELETE FROM message_origins; DELETE FROM metadata WHERE k='message_origin_backfill_complete';").unwrap();
    drop(conn);

    let client = unlocked(&config, &vault);
    client.observe_server_high_water(Cursor(0)).unwrap();
    assert!(client.initial_replay_caught_up().unwrap());
}

#[test]
fn replay_inserts_peer_messages_even_when_their_coarse_origin_key_matches() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let a = unlocked(&config(&dir, "phone-a", &vault), &vault);
    let b = unlocked(&config(&dir, "phone-b", &vault), &vault);
    let first = a
        .capture_incoming_broadcast(
            incoming("same", "one"),
            Some(1_700_000_000_000),
            Some(1_700_000_000_000),
        )
        .unwrap();
    a.capture_incoming_broadcast(
        incoming("same", "two"),
        Some(1_700_000_030_000),
        Some(1_700_000_030_000),
    )
    .unwrap();
    let mut server = Server::default();
    assert_eq!(server.upload(&a), 2);
    replay_to(&server, &b);
    assert_eq!(b.messages(first.conversation_id).unwrap().len(), 2);
}
