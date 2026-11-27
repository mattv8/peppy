//! Carrier text for replies: the permit hands the phone the quoted text, while storage and the
//! provider row reconciliation keep the clean reply.
use crate::c4_media::Objects;
use crate::common::*;
use std::fs;
use tempfile::TempDir;

const TARGET: &str = "meet at the big oak tomorrow";
const REPLY: &str = "sounds good";
/// Mirrors the core's outgoing body limit (`MAX_BODY_BYTES`).
const CARRIER_BODY_LIMIT: usize = 64 * 1024;
const DB_KEY_PRAGMA: &str =
    "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\"";

/// Count of carrier replies sent without their quote, read from the gateway's metadata.
fn unquoted_count(fixture: &ReplyFixture) -> Option<String> {
    let conn = rusqlite::Connection::open(&fixture.gateway_config.database_path).unwrap();
    conn.execute_batch(DB_KEY_PRAGMA).unwrap();
    conn.query_row(
        "SELECT v FROM metadata WHERE k='rcs_carrier_reply_unquoted_count'",
        [],
        |row| row.get(0),
    )
    .ok()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn outgoing_provider_row(body: &str, sent_at_ms: i64) -> SmsAcquisitionInput {
    SmsAcquisitionInput {
        source: SmsSource {
            source_generation: "install-a".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: format!("provider-{sent_at_ms}"),
            provider_thread_id: Some("thread-1".into()),
            broadcast_id: None,
        },
        direction: Direction::Outgoing,
        sender_address: None,
        recipients: vec![ADDRESS.into()],
        body: body.into(),
        imported: true,
        sent_at_ms: Some(sent_at_ms),
        date_sent_ms: Some(0),
        creator: Some("dev.peppy.mobile".into()),
        provider_read: None,
        source_row_fingerprint: format!("install-a:provider-1:{sent_at_ms}"),
    }
}

/// Desktop captures the target and queues a reply to it; the gateway receives both.
struct ReplyFixture {
    _dir: TempDir,
    gateway_config: ClientConfig,
    gateway: Client,
    target: Captured,
    queued: QueuedSend,
}

fn reply_fixture() -> ReplyFixture {
    reply_fixture_with(TARGET, REPLY)
}

fn reply_fixture_with(target_text: &str, reply_text: &str) -> ReplyFixture {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop = unlocked(&config(&dir, "desktop", &vault), &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let target = desktop
        .capture_incoming(incoming(target_text, "target-1"))
        .unwrap();
    let draft = desktop
        .create_compose_draft(Some(target.conversation_id))
        .unwrap();
    let draft = desktop
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                group_name: None,
                text: reply_text.into(),
                recipients: vec![ADDRESS.into()],
                route: Some(route(&gateway_config)),
                attachment_ids: vec![],
            },
        )
        .unwrap();
    let queued = desktop
        .send_compose_draft_with_options(
            draft.draft_id,
            draft.revision,
            ComposeSendOptions {
                reply_to: Some(target.message_id),
                ..ComposeSendOptions::default()
            },
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    server.sync(&gateway);
    ReplyFixture {
        _dir: dir,
        gateway_config,
        gateway,
        target,
        queued,
    }
}

fn permit(gateway: &Client, command_id: CommandId) -> ReceivedCommand {
    match gateway.begin_send_attempt(command_id).unwrap() {
        PermitDecision::Permit(received) => received,
        other => panic!("expected a permit, got {other:?}"),
    }
}

#[test]
fn carrier_permit_quotes_the_reply_while_storage_keeps_it_clean() {
    let fixture = reply_fixture();
    let received = permit(&fixture.gateway, fixture.queued.command_id);

    assert_eq!(
        received.message.body,
        format!("“{TARGET}”\n{REPLY}"),
        "carrier text carries the quote"
    );
    assert_eq!(received.message.reply_to, Some(fixture.target.message_id));
    let stored = fixture
        .gateway
        .messages(fixture.target.conversation_id)
        .unwrap()
        .into_iter()
        .find(|message| message.payload.record.message_id == fixture.queued.message_id)
        .expect("the queued reply is stored on the gateway");
    assert_eq!(stored.payload.body, REPLY, "stored body stays the reply");
}

#[test]
fn reply_with_a_missing_target_is_sent_unquoted() {
    let fixture = reply_fixture();
    let conn = rusqlite::Connection::open(&fixture.gateway_config.database_path).unwrap();
    conn.execute_batch(&format!(
        "{DB_KEY_PRAGMA}; DELETE FROM messages WHERE id='{}';",
        fixture.target.message_id
    ))
    .unwrap();
    drop(conn);

    let received = permit(&fixture.gateway, fixture.queued.command_id);
    assert_eq!(received.message.body, REPLY);
    assert_eq!(unquoted_count(&fixture), Some("1".into()));
}

#[test]
fn quote_fits_the_outgoing_limit_up_to_its_exact_boundary() {
    let target = "a".repeat(80);
    let overhead = compose_reply_text(&target, "").len();

    let fits = "b".repeat(CARRIER_BODY_LIMIT - overhead);
    let at_limit = reply_fixture_with(&target, &fits);
    let received = permit(&at_limit.gateway, at_limit.queued.command_id);
    assert_eq!(received.message.body.len(), CARRIER_BODY_LIMIT);
    assert_eq!(received.message.body, compose_reply_text(&target, &fits));
    assert_eq!(unquoted_count(&at_limit), None);

    let over = "b".repeat(CARRIER_BODY_LIMIT - overhead + 1);
    let past_limit = reply_fixture_with(&target, &over);
    let received = permit(&past_limit.gateway, past_limit.queued.command_id);
    assert_eq!(received.message.body, over, "over the limit goes unquoted");
    assert_eq!(unquoted_count(&past_limit), Some("1".into()));
}

#[test]
fn mms_reply_permit_quotes_the_text_and_storage_stays_clean() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop_config = config(&dir, "desktop", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let target = desktop
        .capture_incoming(incoming(TARGET, "target-1"))
        .unwrap();
    let picture = [b"\x89PNG\r\n\x1a\n".as_slice(), &[7_u8; 512]].concat();
    let picked = dir.path().join("cat.png");
    fs::write(&picked, &picture).unwrap();
    let info = desktop
        .prepare_attachment(&picked, "image/png", "cat.png")
        .unwrap();
    let draft = desktop
        .create_compose_draft(Some(target.conversation_id))
        .unwrap();
    let draft = desktop
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                group_name: None,
                text: REPLY.into(),
                recipients: vec![ADDRESS.into()],
                route: Some(route(&gateway_config)),
                attachment_ids: vec![info.attachment_id],
            },
        )
        .unwrap();
    let queued = desktop
        .send_compose_draft_with_options(
            draft.draft_id,
            draft.revision,
            ComposeSendOptions {
                reply_to: Some(target.message_id),
                ..ComposeSendOptions::default()
            },
        )
        .unwrap();

    // The attachment must be uploaded and installed on the gateway before the MMS permit.
    let mut objects = Objects::default();
    assert_eq!(objects.upload_all(&desktop), 1);
    let mut server = Server::default();
    server.upload(&desktop);
    server.sync(&gateway);
    let downloads = gateway.pending_downloads().unwrap();
    let download = dir.path().join("download.bin");
    objects.download_to(&downloads[0], &download);
    gateway
        .install_downloaded_attachment(info.attachment_id, &download)
        .unwrap();

    let received = permit(&gateway, queued.command_id);
    assert_eq!(received.message.transport, Transport::Mms);
    assert_eq!(received.message.body, format!("“{TARGET}”\n{REPLY}"));
    assert_eq!(received.message.reply_to, Some(target.message_id));
    let stored = gateway
        .messages(target.conversation_id)
        .unwrap()
        .into_iter()
        .find(|message| message.payload.record.message_id == queued.message_id)
        .expect("the queued MMS reply is stored on the gateway");
    assert_eq!(stored.payload.body, REPLY, "stored body stays the reply");
    // MMS rows match by transaction id, so a quoted MMS writes no text fingerprint.
    assert_eq!(wire_digest_rows(&gateway_config, queued.command_id), 0);
}

#[test]
fn plain_send_carrier_text_is_unchanged() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop = unlocked(&config(&dir, "desktop", &vault), &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let queued = desktop
        .queue_send(sms(ConversationId::new(), REPLY), route(&gateway_config))
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    server.sync(&gateway);
    let received = permit(&gateway, queued.command_id);
    assert_eq!(received.message.body, REPLY);
    assert_eq!(received.message.reply_to, None);
}

/// Queues a text draft in `conversation` to the gateway route, optionally as a reply.
fn queue_text(
    desktop: &Client,
    conversation: ConversationId,
    text: &str,
    reply_to: Option<MessageId>,
    route: GatewayRoute,
) -> QueuedSend {
    let draft = desktop.create_compose_draft(Some(conversation)).unwrap();
    let draft = desktop
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                group_name: None,
                text: text.into(),
                recipients: vec![ADDRESS.into()],
                route: Some(route),
                attachment_ids: vec![],
            },
        )
        .unwrap();
    desktop
        .send_compose_draft_with_options(
            draft.draft_id,
            draft.revision,
            ComposeSendOptions {
                reply_to,
                ..ComposeSendOptions::default()
            },
        )
        .unwrap()
}

/// Forgets every wire digest on the gateway, as if the attempts predated them.
fn forget_wire_digests(fixture: &ReplyFixture) {
    let conn = rusqlite::Connection::open(&fixture.gateway_config.database_path).unwrap();
    conn.execute_batch(&format!(
        "{DB_KEY_PRAGMA}; DELETE FROM metadata WHERE k LIKE 'carrier_wire_sha256_v1/%';"
    ))
    .unwrap();
}

/// Number of wire fingerprint rows the gateway holds for one attempt.
fn wire_digest_rows(config: &ClientConfig, command_id: CommandId) -> i64 {
    let conn = rusqlite::Connection::open(&config.database_path).unwrap();
    conn.execute_batch(DB_KEY_PRAGMA).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM metadata WHERE k=?",
        [format!("carrier_wire_sha256_v1/{command_id}")],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn plain_carrier_attempt_writes_no_wire_fingerprint() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop = unlocked(&config(&dir, "desktop", &vault), &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let queued = desktop
        .queue_send(sms(ConversationId::new(), REPLY), route(&gateway_config))
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    server.sync(&gateway);
    permit(&gateway, queued.command_id);
    assert_eq!(wire_digest_rows(&gateway_config, queued.command_id), 0);
}

#[test]
fn a_quoted_attempt_writes_one_wire_fingerprint_until_its_row_matches() {
    let fixture = reply_fixture();
    permit(&fixture.gateway, fixture.queued.command_id);
    assert_eq!(
        wire_digest_rows(&fixture.gateway_config, fixture.queued.command_id),
        1
    );

    let acquisition = fixture
        .gateway
        .begin_sms_acquisition(outgoing_provider_row(
            &format!("“{TARGET}”\n{REPLY}"),
            now_ms() + 350,
        ))
        .unwrap();
    let captured = fixture
        .gateway
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    assert!(captured.duplicate);
    assert_eq!(captured.message_id, fixture.queued.message_id);
    assert_eq!(
        wire_digest_rows(&fixture.gateway_config, fixture.queued.command_id),
        0,
        "a matched attempt drops its fingerprint"
    );
}

#[test]
fn a_carrier_failure_drops_the_wire_fingerprint() {
    let fixture = reply_fixture();
    permit(&fixture.gateway, fixture.queued.command_id);
    fixture
        .gateway
        .record_send_result(
            fixture.queued.command_id,
            SendResult::FailedBeforeSubmission,
        )
        .unwrap();
    assert_eq!(
        wire_digest_rows(&fixture.gateway_config, fixture.queued.command_id),
        0
    );
}

#[test]
fn quoted_reply_and_plain_message_with_the_same_text_each_match_their_own_row() {
    // Both arrival orders: the rows are the same text to the same recipient in one window.
    for reply_row_first in [true, false] {
        let dir = TempDir::new().unwrap();
        let vault = Vault::new();
        let desktop = unlocked(&config(&dir, "desktop", &vault), &vault);
        let gateway_config = config(&dir, "gateway", &vault);
        let gateway = unlocked(&gateway_config, &vault);
        let target = desktop
            .capture_incoming(incoming(TARGET, "target-1"))
            .unwrap();
        let reply = queue_text(
            &desktop,
            target.conversation_id,
            REPLY,
            Some(target.message_id),
            route(&gateway_config),
        );
        let plain = queue_text(
            &desktop,
            target.conversation_id,
            REPLY,
            None,
            route(&gateway_config),
        );
        let mut server = Server::default();
        server.upload(&desktop);
        server.sync(&gateway);
        let reply_permit = permit(&gateway, reply.command_id);
        let plain_permit = permit(&gateway, plain.command_id);
        assert_eq!(reply_permit.message.body, format!("“{TARGET}”\n{REPLY}"));
        assert_eq!(plain_permit.message.body, REPLY);

        let now = now_ms();
        let quoted_row = outgoing_provider_row(&format!("“{TARGET}”\n{REPLY}"), now + 350);
        let plain_row = outgoing_provider_row(REPLY, now + 400);
        let rows = if reply_row_first {
            [
                (quoted_row, reply.message_id),
                (plain_row, plain.message_id),
            ]
        } else {
            [
                (plain_row, plain.message_id),
                (quoted_row, reply.message_id),
            ]
        };
        for (row, expected) in rows {
            let acquisition = gateway.begin_sms_acquisition(row).unwrap();
            let captured = gateway
                .complete_sms_acquisition(&acquisition.acquisition_id)
                .unwrap();
            assert!(captured.duplicate, "the row reconciles, not imports");
            assert_eq!(
                captured.message_id, expected,
                "each row claims its own message"
            );
        }
        let messages = gateway.messages(target.conversation_id).unwrap();
        assert_eq!(
            messages.len(),
            3,
            "one target, one reply, one plain; no duplicate"
        );
    }
}

#[test]
fn legacy_attempt_without_a_wire_digest_still_matches_its_clean_body() {
    let fixture = reply_fixture();
    permit(&fixture.gateway, fixture.queued.command_id);
    forget_wire_digests(&fixture);

    let acquisition = fixture
        .gateway
        .begin_sms_acquisition(outgoing_provider_row(REPLY, now_ms() + 350))
        .unwrap();
    let captured = fixture
        .gateway
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    assert!(captured.duplicate);
    assert_eq!(captured.message_id, fixture.queued.message_id);
}

#[test]
fn a_target_deleted_after_admission_still_matches_the_composed_provider_row() {
    let fixture = reply_fixture();
    permit(&fixture.gateway, fixture.queued.command_id);
    let conn = rusqlite::Connection::open(&fixture.gateway_config.database_path).unwrap();
    conn.execute_batch(&format!(
        "{DB_KEY_PRAGMA}; DELETE FROM messages WHERE id='{}';",
        fixture.target.message_id
    ))
    .unwrap();
    drop(conn);

    let acquisition = fixture
        .gateway
        .begin_sms_acquisition(outgoing_provider_row(
            &format!("“{TARGET}”\n{REPLY}"),
            now_ms() + 350,
        ))
        .unwrap();
    let captured = fixture
        .gateway
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    assert!(captured.duplicate, "the admitted wire text is the record");
    assert_eq!(captured.message_id, fixture.queued.message_id);
}

#[test]
fn composed_reply_provider_row_reconciles_to_the_one_queued_message() {
    let fixture = reply_fixture();
    permit(&fixture.gateway, fixture.queued.command_id);

    // The platform stores the text it was handed, which is the composed carrier text.
    let acquisition = fixture
        .gateway
        .begin_sms_acquisition(outgoing_provider_row(
            &format!("“{TARGET}”\n{REPLY}"),
            now_ms() + 350,
        ))
        .unwrap();
    let captured = fixture
        .gateway
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();

    assert!(
        captured.duplicate,
        "the provider row reconciles, not imports"
    );
    assert_eq!(captured.message_id, fixture.queued.message_id);
    let messages = fixture
        .gateway
        .messages(fixture.target.conversation_id)
        .unwrap();
    assert_eq!(messages.len(), 2, "one target and one reply, no duplicate");
    let reply = messages
        .iter()
        .find(|message| message.payload.record.message_id == fixture.queued.message_id)
        .unwrap();
    assert_eq!(reply.payload.body, REPLY, "stored body stays clean");
}

/// The pre-admission read: returns the body it reports and checks it equals the permit's body.
/// The read records nothing, so no digest exists and no unquoted counter is set before the permit.
fn assert_read_matches_permit(fixture: &ReplyFixture) -> String {
    let read = fixture
        .gateway
        .carrier_send_body(fixture.queued.command_id)
        .unwrap();
    assert_eq!(
        wire_digest_rows(&fixture.gateway_config, fixture.queued.command_id),
        0,
        "the read writes no wire digest"
    );
    assert_eq!(unquoted_count(fixture), None, "the read counts nothing");
    let received = permit(&fixture.gateway, fixture.queued.command_id);
    assert_eq!(read, received.message.body, "read equals the permit body");
    read
}

#[test]
fn carrier_send_body_matches_the_permit_for_every_quote_outcome() {
    let target = "a".repeat(80);
    let overhead = compose_reply_text(&target, "").len();
    let at_limit = "b".repeat(CARRIER_BODY_LIMIT - overhead);
    let over_limit = "b".repeat(CARRIER_BODY_LIMIT - overhead + 1);
    // (target text, reply text, expected carrier body): quoted, empty excerpt, at and over limit.
    let cases = [
        (TARGET, REPLY, compose_reply_text(TARGET, REPLY)),
        ("  \n\t ", REPLY, REPLY.to_owned()),
        (
            target.as_str(),
            at_limit.as_str(),
            compose_reply_text(&target, &at_limit),
        ),
        (target.as_str(), over_limit.as_str(), over_limit.clone()),
    ];
    for (target_text, reply_text, expected) in cases {
        let fixture = reply_fixture_with(target_text, reply_text);
        assert_eq!(assert_read_matches_permit(&fixture), expected);
    }
}

#[test]
fn carrier_send_body_of_a_plain_text_is_its_stored_body() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop = unlocked(&config(&dir, "desktop", &vault), &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let queued = desktop
        .queue_send(sms(ConversationId::new(), REPLY), route(&gateway_config))
        .unwrap();
    let mut server = Server::default();
    server.upload(&desktop);
    server.sync(&gateway);
    assert_eq!(gateway.carrier_send_body(queued.command_id).unwrap(), REPLY);
    assert_eq!(permit(&gateway, queued.command_id).message.body, REPLY);
}

#[test]
fn carrier_send_body_of_a_reply_to_a_missing_target_is_the_reply() {
    let fixture = reply_fixture();
    let conn = rusqlite::Connection::open(&fixture.gateway_config.database_path).unwrap();
    conn.execute_batch(&format!(
        "{DB_KEY_PRAGMA}; DELETE FROM messages WHERE id='{}';",
        fixture.target.message_id
    ))
    .unwrap();
    drop(conn);
    assert_eq!(assert_read_matches_permit(&fixture), REPLY);
}
