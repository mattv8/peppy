//! One-time-code detector corpus and `Client::one_time_code_for` lookup coverage.
#[path = "common/mod.rs"]
mod common;

use common::*;
use peppy_client_core::one_time_code::{OneTimeCode, OneTimeCodeSource, detect};
use serde::Deserialize;
use tempfile::TempDir;

const POSITIVE_CORPUS: &str = include_str!("fixtures/one_time_code/positive.json");
const NEGATIVE_CORPUS: &str = include_str!("fixtures/one_time_code/negative.json");

#[derive(Deserialize)]
struct PositiveCase {
    origin: String,
    text: String,
    code: String,
    source: String,
}

#[derive(Deserialize)]
struct NegativeCase {
    origin: String,
    text: String,
}

fn source_named(name: &str) -> OneTimeCodeSource {
    match name {
        "originBound" => OneTimeCodeSource::OriginBound,
        "heuristic" => OneTimeCodeSource::Heuristic,
        other => panic!("unknown corpus source {other}"),
    }
}

#[test]
fn positive_corpus_detects_the_expected_code() {
    let cases: Vec<PositiveCase> = serde_json::from_str(POSITIVE_CORPUS).unwrap();
    assert!(
        cases.len() >= 60,
        "positive corpus has {} cases",
        cases.len()
    );
    for case in &cases {
        let expected = OneTimeCode {
            code: case.code.clone(),
            source: source_named(&case.source),
        };
        assert_eq!(
            detect(&case.text),
            Some(expected),
            "{}: {:?}",
            case.origin,
            case.text
        );
    }
}

#[test]
fn negative_corpus_detects_nothing() {
    let cases: Vec<NegativeCase> = serde_json::from_str(NEGATIVE_CORPUS).unwrap();
    assert!(
        cases.len() >= 30,
        "negative corpus has {} cases",
        cases.len()
    );
    for case in &cases {
        assert_eq!(detect(&case.text), None, "{}: {:?}", case.origin, case.text);
    }
}

#[test]
fn long_chat_mentioning_codes_is_ignored() {
    let text = format!(
        "{} premiere pro 2025 code 482913 is a project setting",
        "lorem ipsum dolor sit amet ".repeat(20)
    );
    assert!(text.chars().count() > 500);
    assert_eq!(detect(&text), None);
}

#[test]
fn origin_line_past_the_input_bound_is_not_read() {
    let text = format!("{}\n@example.com #123456", "a ".repeat(1_100));
    assert_eq!(detect(&text), None);
}

fn capture_notification(
    client: &Client,
    key: &str,
    instance: &str,
    title: &str,
    text: &str,
) -> NotificationTarget {
    client
        .capture_notification(NotificationCapture {
            notification_key: key.into(),
            instance: instance.into(),
            package_name: "com.example.bank".into(),
            app_name: "Bank".into(),
            app_icon_png_base64: None,
            title: title.into(),
            text: text.into(),
            category: None,
            posted_at: 42,
            dismissible: true,
        })
        .unwrap();
    client
        .notification_snapshot()
        .unwrap()
        .notifications
        .into_iter()
        .find(|mirrored| mirrored.target.notification_key == key)
        .expect("captured notification is mirrored")
        .target
}

/// Banners are queued on the receiving device, so captures happen on a phone and reach the
/// desktop through the mock server.
#[test]
fn message_banner_candidate_carries_the_code_and_spam_drops_it() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "otp-banner-phone", &vault), &vault);
    let desktop = unlocked(&config(&dir, "otp-banner-desktop", &vault), &vault);
    let mut server = Server::default();

    let captured = phone
        .capture_incoming(incoming(
            "Your verification code is 482913.",
            "otp-banner-1",
        ))
        .unwrap();
    let id = captured.message_id.to_string();
    server.upload(&phone);
    server.sync(&desktop);
    let candidate = desktop
        .pending_banner_candidates(10)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.id == id)
        .expect("incoming message queues a banner candidate");
    assert_eq!(candidate.kind, "message");
    assert_eq!(candidate.one_time_code.as_deref(), Some("482913"));

    desktop
        .set_conversation_folder(captured.conversation_id, ConversationFolder::Spam)
        .unwrap();
    assert!(
        desktop
            .pending_banner_candidates(10)
            .unwrap()
            .iter()
            .all(|candidate| candidate.id != id),
        "moving to spam removes the banner candidate"
    );

    let mut spam_arrival = incoming("Your verification code is 739204.", "otp-banner-2");
    spam_arrival.conversation_id = Some(captured.conversation_id);
    let spam_id = phone
        .capture_incoming(spam_arrival)
        .unwrap()
        .message_id
        .to_string();
    server.upload(&phone);
    server.sync(&desktop);
    assert!(
        desktop
            .pending_banner_candidates(10)
            .unwrap()
            .iter()
            .all(|candidate| candidate.id != spam_id),
        "a message arriving in a spam conversation queues no banner"
    );
}

#[test]
fn notification_banner_candidate_uses_text_then_title() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "otp-notif-phone", &vault), &vault);
    let desktop = unlocked(&config(&dir, "otp-notif-desktop", &vault), &vault);
    let mut server = Server::default();
    let target = capture_notification(&phone, "n-text", "one", "Bank", "Your code is 739204");
    let title_only = capture_notification(&phone, "n-title", "one", "Code 528146", "hello");
    server.upload(&phone);
    server.sync(&desktop);

    let candidates = desktop.pending_banner_candidates(10).unwrap();
    let code_for = |target: &NotificationTarget| {
        candidates
            .iter()
            .find(|candidate| candidate.notification_target.as_ref() == Some(target))
            .expect("notification queues a banner candidate")
            .one_time_code
            .clone()
    };
    assert_eq!(code_for(&target).as_deref(), Some("739204"));
    assert_eq!(code_for(&title_only).as_deref(), Some("528146"));
}

#[test]
fn incoming_message_yields_code_and_conversation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "otp-incoming", &vault), &vault);
    let captured = client
        .capture_incoming(incoming("Your verification code is 482913.", "otp-in-1"))
        .unwrap();
    let target = OneTimeCodeTarget::Message {
        message_id: captured.message_id.to_string(),
    };
    assert_eq!(
        client.one_time_code_for(&target).unwrap(),
        Some(OneTimeCodeMatch {
            code: "482913".into(),
            conversation_id: Some(captured.conversation_id.to_string()),
        })
    );
}

#[test]
fn outgoing_message_yields_nothing() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = config(&dir, "otp-outgoing-gateway", &vault);
    let client = unlocked(&config(&dir, "otp-outgoing", &vault), &vault);
    let conversation = client
        .capture_incoming(incoming("hello", "otp-out-1"))
        .unwrap()
        .conversation_id;
    let queued = client
        .queue_send(
            sms(conversation, "Your verification code is 482913."),
            route(&gateway),
        )
        .unwrap();
    let target = OneTimeCodeTarget::Message {
        message_id: queued.message_id.to_string(),
    };
    assert_eq!(client.one_time_code_for(&target).unwrap(), None);
}

#[test]
fn spam_conversation_yields_nothing() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "otp-spam", &vault), &vault);
    let captured = client
        .capture_incoming(incoming("Your verification code is 482913.", "otp-spam-1"))
        .unwrap();
    let target = OneTimeCodeTarget::Message {
        message_id: captured.message_id.to_string(),
    };
    client
        .set_conversation_folder(captured.conversation_id, ConversationFolder::Spam)
        .unwrap();
    assert_eq!(client.one_time_code_for(&target).unwrap(), None);
}

#[test]
fn notification_yields_code_and_stops_after_removal() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "otp-notification", &vault), &vault);
    let target = capture_notification(&client, "n-otp", "one", "Bank", "Your code is 739204");
    let lookup = OneTimeCodeTarget::Notification {
        target: target.clone(),
    };
    assert_eq!(
        client.one_time_code_for(&lookup).unwrap(),
        Some(OneTimeCodeMatch {
            code: "739204".into(),
            conversation_id: None,
        })
    );
    let mirrored = client.notification_snapshot().unwrap().notifications;
    assert_eq!(mirrored.len(), 1);
    assert_eq!(mirrored[0].one_time_code.as_deref(), Some("739204"));
    client.remove_notification("n-otp", "one").unwrap();
    assert_eq!(client.one_time_code_for(&lookup).unwrap(), None);
    assert!(
        client
            .notification_snapshot()
            .unwrap()
            .notifications
            .is_empty()
    );
}

#[test]
fn unknown_message_id_yields_nothing() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "otp-unknown", &vault), &vault);
    let target = OneTimeCodeTarget::Message {
        message_id: "00000000-0000-4000-8000-000000000000".into(),
    };
    assert_eq!(client.one_time_code_for(&target).unwrap(), None);
}

#[test]
fn locked_client_yields_nothing() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "otp-locked", &vault);
    let captured = {
        let client = unlocked(&cfg, &vault);
        client
            .capture_incoming(incoming(
                "Your verification code is 482913.",
                "otp-locked-1",
            ))
            .unwrap()
    };
    let locked = open(&cfg);
    let target = OneTimeCodeTarget::Message {
        message_id: captured.message_id.to_string(),
    };
    assert_eq!(locked.one_time_code_for(&target).unwrap(), None);
}
