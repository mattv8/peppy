//! `compose::send_again` against the real core: the copy, its refusals, and the route guard.
use crate::compose::{self, ComposeError, SendAgainInput};
use crate::{GatewayAvailability, GatewayView, send_recovery_view};
use peppy_client_core::{
    AttachmentId, Client, ClientConfig, ConversationId, DatabaseKey, DeviceId, Error as CoreError,
    GatewayRoute, MessageId, OutgoingMms, OutgoingSms, Transport, VaultId,
};
use std::collections::HashSet;
use std::str::FromStr;

const PASSPHRASE: &str = "send again test passphrase";
const SIM: &str = "sim";

struct Fixture {
    _directory: tempfile::TempDir,
    path: std::path::PathBuf,
    client: Client,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("send-again.db");
    let vault_id = VaultId::new();
    let client = Client::open(
        ClientConfig {
            database_path: path.clone(),
            vault_id,
            device_id: DeviceId::new(),
        },
        DatabaseKey::new(&[7; 32]).unwrap(),
    )
    .unwrap();
    let profile = peppy_crypto::KeyProfile::new(vault_id.0, 1).unwrap();
    let root = peppy_crypto::derive_root_key(PASSPHRASE, &profile).unwrap();
    let header = peppy_crypto::create_vault_check_header(&root, profile.clone()).unwrap();
    client.unlock(&profile, &header, PASSPHRASE).unwrap();
    Fixture {
        _directory: directory,
        path,
        client,
    }
}

fn route(device: DeviceId) -> GatewayRoute {
    GatewayRoute::Carrier {
        gateway_device_id: device,
        subscription_id: SIM.into(),
    }
}

fn phone(id: DeviceId, availability: GatewayAvailability, mms: bool) -> GatewayView {
    GatewayView {
        id: id.to_string(),
        name: "Phone".into(),
        sim_id: SIM.into(),
        online: availability == GatewayAvailability::Connected,
        simulated: false,
        supports_sms: true,
        supports_mms: mms,
        availability,
        setting_up: availability == GatewayAvailability::SettingUp,
        last_seen_age_seconds: None,
        presence: None,
        wake_route: false,
        capability_note: None,
        mms_content_version: Some(2),
        mms_max_bytes: None,
        mms_limit_source: None,
        mms_max_recipients: None,
    }
}

fn revoked(devices: &[DeviceId]) -> HashSet<String> {
    devices.iter().map(ToString::to_string).collect()
}

fn copy_input(original: MessageId, device: DeviceId) -> SendAgainInput {
    SendAgainInput {
        message_id: original.to_string(),
        gateway_id: device.to_string(),
        sim_id: SIM.into(),
    }
}

/// The error code a host shows for a refusal (`ComposeError` has no `Debug`).
fn code(error: ComposeError) -> String {
    match error {
        ComposeError::Ui { code, .. } => code.to_owned(),
        ComposeError::Core(CoreError::SendAgainNotAllowed) => "invalid-request".into(),
        ComposeError::Core(CoreError::AlreadySentAgain) => "already-sent-again".into(),
        ComposeError::Core(CoreError::SendAgainAttachmentsUnavailable) => {
            "attachments-unavailable".into()
        }
        ComposeError::Core(_) => "core".into(),
        ComposeError::AfterRoute { .. } => "after-route".into(),
    }
}

fn expect_error(result: Result<compose::SendAgainResultView, ComposeError>) -> String {
    match result {
        Ok(_) => panic!("send again should be refused"),
        Err(error) => code(error),
    }
}

fn queue_sms(client: &Client, device: DeviceId, body: &str) -> (ConversationId, MessageId) {
    let conversation = ConversationId::new();
    let queued = client
        .queue_send(
            OutgoingSms {
                conversation_id: conversation,
                recipients: vec!["+15555550123".into()],
                body: body.into(),
            },
            route(device),
        )
        .unwrap();
    (conversation, queued.message_id)
}

#[test]
fn an_sms_stranded_on_a_removed_phone_is_copied_to_a_second_phone_once() {
    let fixture = fixture();
    let removed = DeviceId::new();
    let second = DeviceId::new();
    let (conversation, original) = queue_sms(&fixture.client, removed, "stranded");
    let roster = revoked(&[removed]);
    let phones = [phone(second, GatewayAvailability::Connected, false)];

    let copy = compose::send_again(
        &fixture.client,
        &copy_input(original, second),
        Some(&phones),
        &roster,
    )
    .ok()
    .unwrap();

    let copied = fixture
        .client
        .message_by_id(MessageId::from_str(&copy.message_id).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(copied.payload.body, "stranded");
    assert_eq!(copied.payload.transport, Transport::Sms);
    assert_eq!(copied.payload.record.conversation_id, conversation);
    assert_eq!(copied.target_gateway_id, Some(second));
    let original_view = fixture.client.message_by_id(original).unwrap().unwrap();
    let recovery = send_recovery_view(&fixture.client, &original_view, &roster)
        .unwrap()
        .unwrap();
    assert!(!recovery.can_send_again);
    assert_eq!(
        recovery.sent_again.map(|copy| copy.message_id),
        Some(copy.message_id.clone())
    );
    assert_eq!(
        expect_error(compose::send_again(
            &fixture.client,
            &copy_input(original, second),
            Some(&phones),
            &roster,
        )),
        "already-sent-again"
    );
}

/// The sending device's half of the flow: the original is offered a copy once its phone is
/// removed, the copy goes to a second phone, and the original then shows the copy and no button.
/// Other devices see the copy as an ordinary message (R16); they are not covered here.
#[test]
fn the_sending_device_offers_one_copy_after_its_phone_is_removed() {
    let fixture = fixture();
    let removed = DeviceId::new();
    let second = DeviceId::new();
    let (conversation, original) = queue_sms(&fixture.client, removed, "flow");
    let roster = revoked(&[removed]);
    let phones = [phone(second, GatewayAvailability::Connected, false)];
    let before = fixture.client.message_by_id(original).unwrap().unwrap();
    assert!(
        send_recovery_view(&fixture.client, &before, &roster)
            .unwrap()
            .is_some_and(|view| view.can_send_again)
    );

    let copy = compose::send_again(
        &fixture.client,
        &copy_input(original, second),
        Some(&phones),
        &roster,
    )
    .ok()
    .unwrap();

    let after = fixture.client.message_by_id(original).unwrap().unwrap();
    let view = send_recovery_view(&fixture.client, &after, &roster)
        .unwrap()
        .unwrap();
    assert!(!view.can_send_again);
    assert_eq!(
        view.sent_again.map(|copy| copy.message_id),
        Some(copy.message_id.clone())
    );
    let copies: Vec<_> = fixture
        .client
        .messages(conversation)
        .unwrap()
        .into_iter()
        .filter(|message| message.payload.record.message_id.to_string() == copy.message_id)
        .collect();
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].target_gateway_id, Some(second));
    assert_eq!(copies[0].payload.body, "flow");
}

#[test]
fn a_sent_again_link_whose_copy_row_is_gone_is_not_projected() {
    let fixture = fixture();
    let removed = DeviceId::new();
    let second = DeviceId::new();
    let (_, original) = queue_sms(&fixture.client, removed, "purged copy");
    let roster = revoked(&[removed]);
    let phones = [phone(second, GatewayAvailability::Connected, false)];
    let copy = compose::send_again(
        &fixture.client,
        &copy_input(original, second),
        Some(&phones),
        &roster,
    )
    .ok()
    .unwrap();
    let original_message = fixture.client.message_by_id(original).unwrap().unwrap();
    let linked =
        crate::rcs_receive::message_view(&fixture.client, &original_message, vec![], &roster)
            .unwrap();
    assert_eq!(
        linked.sent_again.map(|link| link.message_id),
        Some(copy.message_id.clone())
    );

    // The copy row disappears (for example a local purge): the core stops reporting the link.
    {
        let db = rusqlite::Connection::open(&fixture.path).unwrap();
        db.execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
        db.execute(
            "DELETE FROM messages WHERE id=?",
            [copy.message_id.as_str()],
        )
        .unwrap();
    }
    let after = fixture.client.message_by_id(original).unwrap().unwrap();
    let view = crate::rcs_receive::message_view(&fixture.client, &after, vec![], &roster).unwrap();

    assert!(view.sent_again.is_none());
    assert!(
        view.send_recovery
            .as_ref()
            .is_some_and(|r| r.sent_again.is_none())
    );
    assert!(
        serde_json::to_value(&view)
            .unwrap()
            .get("sentAgain")
            .is_none()
    );
}

#[test]
fn an_mms_copy_reuses_the_attachment_on_this_device() {
    let fixture = fixture();
    let removed = DeviceId::new();
    let second = DeviceId::new();
    let source = fixture._directory.path().join("photo.bin");
    std::fs::write(&source, b"photo").unwrap();
    let attachment = fixture
        .client
        .prepare_attachment(&source, "image/jpeg", "photo.jpg")
        .unwrap();
    let conversation = ConversationId::new();
    let original = fixture
        .client
        .queue_mms(
            OutgoingMms {
                conversation_id: conversation,
                recipients: vec!["+15555550123".into()],
                body: "look".into(),
                attachment_ids: vec![attachment.attachment_id],
                subject: None,
            },
            route(removed),
        )
        .unwrap()
        .message_id;
    let roster = revoked(&[removed]);
    let phones = [phone(second, GatewayAvailability::Connected, true)];

    let copy = compose::send_again(
        &fixture.client,
        &copy_input(original, second),
        Some(&phones),
        &roster,
    )
    .ok()
    .unwrap();

    let copied = fixture
        .client
        .message_by_id(MessageId::from_str(&copy.message_id).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(copied.payload.transport, Transport::Mms);
    assert_eq!(copied.payload.body, "look");
    let attachments: Vec<AttachmentId> = copied
        .payload
        .record
        .attachments
        .iter()
        .map(|reference| reference.attachment_id)
        .collect();
    assert_eq!(attachments, vec![attachment.attachment_id]);
}

#[test]
fn a_target_that_is_not_revoked_is_refused_with_the_generic_code() {
    let fixture = fixture();
    let target = DeviceId::new();
    let second = DeviceId::new();
    let (_, original) = queue_sms(&fixture.client, target, "still waiting");
    let phones = [phone(second, GatewayAvailability::Connected, false)];

    assert_eq!(
        expect_error(compose::send_again(
            &fixture.client,
            &copy_input(original, second),
            Some(&phones),
            &HashSet::new(),
        )),
        "invalid-request"
    );
}

#[test]
fn a_group_conversation_is_refused_even_when_its_phone_is_removed() {
    let fixture = fixture();
    let removed = DeviceId::new();
    let second = DeviceId::new();
    let (conversation, original) = queue_sms(&fixture.client, removed, "group");
    {
        let db = rusqlite::Connection::open(&fixture.path).unwrap();
        db.execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
        db.execute(
            "INSERT INTO conversation_group_members(conversation_id, canonical_address) VALUES(?,?),(?,?)",
            rusqlite::params![
                conversation.to_string(),
                "+15551230001",
                conversation.to_string(),
                "+15551230002"
            ],
        )
        .unwrap();
    }
    let phones = [phone(second, GatewayAvailability::Connected, false)];

    assert_eq!(
        expect_error(compose::send_again(
            &fixture.client,
            &copy_input(original, second),
            Some(&phones),
            &revoked(&[removed]),
        )),
        "invalid-request"
    );
}

#[test]
fn an_unknown_original_is_refused_with_the_generic_code() {
    let fixture = fixture();
    let second = DeviceId::new();
    let input = copy_input(MessageId::new(), second);
    let phones = [phone(second, GatewayAvailability::Connected, false)];

    assert_eq!(
        expect_error(compose::send_again(
            &fixture.client,
            &input,
            Some(&phones),
            &HashSet::new()
        )),
        "invalid-request"
    );
}

#[test]
fn a_route_that_cannot_take_the_copy_is_route_unavailable() {
    let fixture = fixture();
    let removed = DeviceId::new();
    let (_, original) = queue_sms(&fixture.client, removed, "route");
    let roster = revoked(&[removed]);
    let second = DeviceId::new();
    let refused = |gateways: Option<&[GatewayView]>, roster: &HashSet<String>| {
        expect_error(compose::send_again(
            &fixture.client,
            &copy_input(original, second),
            gateways,
            roster,
        ))
    };

    assert_eq!(
        refused(None, &roster),
        "route-unavailable",
        "roster not loaded"
    );
    assert_eq!(
        refused(Some(&[]), &roster),
        "route-unavailable",
        "route not reported"
    );
    let setting_up = [phone(second, GatewayAvailability::SettingUp, false)];
    assert_eq!(
        refused(Some(&setting_up), &roster),
        "route-unavailable",
        "setting up"
    );
    let offline = [phone(second, GatewayAvailability::Offline, false)];
    assert_eq!(
        refused(Some(&offline), &roster),
        "route-unavailable",
        "gateway offline"
    );
    let mut no_sms = phone(second, GatewayAvailability::Connected, false);
    no_sms.supports_sms = false;
    assert_eq!(
        refused(Some(&[no_sms]), &roster),
        "route-unavailable",
        "no SMS"
    );
    let chosen_removed = [phone(second, GatewayAvailability::Connected, false)];
    assert_eq!(
        refused(Some(&chosen_removed), &revoked(&[removed, second])),
        "route-unavailable",
        "the chosen phone is revoked"
    );
}

#[test]
fn the_original_phone_is_never_a_route_for_its_own_copy_even_if_the_roster_still_lists_it() {
    let fixture = fixture();
    let removed = DeviceId::new();
    let (_, original) = queue_sms(&fixture.client, removed, "same phone");
    // A stale phone list still shows the removed phone as a route.
    let stale = [phone(removed, GatewayAvailability::Connected, false)];

    assert_eq!(
        expect_error(compose::send_again(
            &fixture.client,
            &copy_input(original, removed),
            Some(&stale),
            &revoked(&[removed]),
        )),
        "route-unavailable"
    );
}

#[test]
fn an_mms_copy_to_a_phone_without_mms_version_two_is_route_unavailable() {
    let fixture = fixture();
    let removed = DeviceId::new();
    let second = DeviceId::new();
    let conversation = ConversationId::new();
    let original = fixture
        .client
        .queue_mms(
            OutgoingMms {
                conversation_id: conversation,
                recipients: vec!["+15555550123".into()],
                body: "mms".into(),
                attachment_ids: vec![],
                subject: None,
            },
            route(removed),
        )
        .unwrap()
        .message_id;
    let mut old_phone = phone(second, GatewayAvailability::Connected, true);
    old_phone.mms_content_version = Some(1);

    assert_eq!(
        expect_error(compose::send_again(
            &fixture.client,
            &copy_input(original, second),
            Some(&[old_phone]),
            &revoked(&[removed]),
        )),
        "route-unavailable"
    );
}

#[test]
fn core_refusals_keep_their_codes_and_route_refusals_map_to_route_unavailable() {
    for (error, expected) in [
        (CoreError::AlreadySentAgain, "already-sent-again"),
        (
            CoreError::SendAgainAttachmentsUnavailable,
            "attachments-unavailable",
        ),
        (CoreError::SendAgainNotAllowed, "invalid-request"),
        (
            CoreError::InvalidRequest("carrier route required"),
            "route-unavailable",
        ),
        (
            CoreError::InvalidRequest("subscription id"),
            "route-unavailable",
        ),
        (
            CoreError::InvalidRequest("MMS requires 1..=20 recipients"),
            "route-unavailable",
        ),
        (CoreError::InvalidRequest("subject"), "core"),
    ] {
        assert_eq!(code(compose::send_again_error(error)), expected);
    }
}

#[test]
fn the_command_input_and_result_use_camel_case_fields() {
    let input: SendAgainInput = serde_json::from_value(serde_json::json!({
        "messageId": "message-1",
        "gatewayId": "gateway-1",
        "simId": "sim-1"
    }))
    .unwrap();
    assert_eq!(input.message_id, "message-1");
    assert_eq!(input.gateway_id, "gateway-1");
    assert_eq!(input.sim_id, "sim-1");
    assert_eq!(
        serde_json::to_value(compose::SendAgainResultView {
            message_id: "message-2".into()
        })
        .unwrap(),
        serde_json::json!({"messageId": "message-2"})
    );
}
