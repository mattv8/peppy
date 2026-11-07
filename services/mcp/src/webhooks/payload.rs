use peppy_client_core::{MessagePayload, Transport};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const DELIVERY_PREFIX: &str = "msg_";
const DELIVERY_TOKEN_LENGTH: usize = 26;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentMeta {
    pub id: String,
    pub content_type: Option<String>,
    pub size: Option<u64>,
}

/// Builds a Standard Webhooks payload for an inbound message.
pub fn message_payload(
    webhook_id: Uuid,
    source: &str,
    msg: &MessagePayload,
    contact_name: Option<&str>,
    attachments: &[AttachmentMeta],
    now_ms: i64,
) -> Value {
    json!({
        "type": "message.received",
        "timestamp": rfc3339_seconds(now_ms),
        "source": source,
        "data": {
            "webhook_id": webhook_id.to_string(),
            "message_id": msg.record.message_id.to_string(),
            "conversation_id": msg.record.conversation_id.to_string(),
            "from": { "address": &msg.sender_address, "contact_name": contact_name },
            "to": &msg.recipients,
            "transport": transport_name(msg.transport),
            "subject": &msg.subject,
            "body": &msg.body,
            "sent_at": msg.sent_at_ms.and_then(rfc3339_seconds_checked),
            "attachments": attachments.iter().map(|attachment| json!({
                "id": attachment.id,
                "content_type": attachment.content_type,
                "size": attachment.size,
            })).collect::<Vec<_>>(),
        }
    })
}

/// Builds a test event payload without any vault content.
pub fn test_payload(webhook_id: Uuid, source: &str, now_ms: i64) -> Value {
    json!({
        "type": "webhook.test",
        "timestamp": rfc3339_seconds(now_ms),
        "source": source,
        "data": { "webhook_id": webhook_id.to_string(), "message": "This is a test event from Peppy." },
    })
}

/// Creates the stable delivery identifier shared by retries of the same event.
pub fn delivery_id(webhook_id: Uuid, message_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(webhook_id.as_bytes());
    hasher.update(message_id.as_bytes());
    let digest = hasher.finalize();
    base32_delivery_id(&digest)
}

/// Creates a random delivery identifier for a test event.
pub fn random_delivery_id() -> String {
    let _ = libsodium_rs::ensure_init();
    let mut bytes = [0_u8; 32];
    libsodium_rs::random::fill_bytes(&mut bytes);
    base32_delivery_id(&bytes)
}

fn transport_name(transport: Transport) -> &'static str {
    match transport {
        Transport::Sms => "sms",
        Transport::Mms => "mms",
        Transport::Rcs => "rcs",
    }
}

fn base32_delivery_id(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(DELIVERY_TOKEN_LENGTH);
    let mut buffer = 0_u16;
    let mut bits = 0_u8;
    for byte in bytes {
        buffer = (buffer << 8) | u16::from(*byte);
        bits += 8;
        while bits >= 5 && encoded.len() < DELIVERY_TOKEN_LENGTH {
            bits -= 5;
            let index = (buffer >> bits) & 0x1f;
            encoded.push(b"abcdefghijklmnopqrstuvwxyz234567"[usize::from(index)] as char);
        }
    }
    format!("{DELIVERY_PREFIX}{encoded}")
}

/// Formats a Unix millisecond timestamp as RFC 3339 UTC with whole seconds.
pub(crate) fn rfc3339_seconds(timestamp_ms: i64) -> String {
    let seconds = timestamp_ms.div_euclid(1_000);
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_date(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn rfc3339_seconds_checked(timestamp_ms: i64) -> Option<String> {
    let seconds = timestamp_ms.div_euclid(1_000);
    let days = seconds.div_euclid(86_400);
    let (year, _, _) = civil_date(days);
    (0..=9_999)
        .contains(&year)
        .then(|| rfc3339_seconds(timestamp_ms))
}

fn civil_date(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month_period = (5 * doy + 2) / 153;
    let day = doy - (153 * month_period + 2) / 5 + 1;
    let month = month_period + if month_period < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message() -> MessagePayload {
        serde_json::from_value(json!({
            "message_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            "conversation_id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            "source_sequence": "1",
            "attachments": [],
            "source_device_id": "cccccccc-cccc-cccc-cccc-cccccccccccc",
            "provider_message_id": null,
            "sender_address": "+15550100",
            "recipients": ["+15550200"],
            "subject": null,
            "body": "Hello",
            "transport": "sms",
            "direction": "incoming",
            "imported": false,
            "sent_at_ms": 1_614_265_330_999_i64,
            "mms_context": null
        }))
        .expect("minimal message payload")
    }

    #[test]
    fn message_payload_has_expected_shape_and_nulls() {
        let webhook = Uuid::nil();
        let value = message_payload(webhook, "https://app.peppy.pro", &message(), None, &[], 0);
        assert_eq!(value["type"], "message.received");
        assert_eq!(value["timestamp"], "1970-01-01T00:00:00Z");
        assert_eq!(value["data"]["from"]["contact_name"], Value::Null);
        assert_eq!(value["data"]["sent_at"], "2021-02-25T15:02:10Z");
        assert_eq!(value["data"]["attachments"], json!([]));
    }

    #[test]
    fn delivery_ids_are_deterministic_and_distinct() {
        let webhook = Uuid::nil();
        let first = delivery_id(webhook, "message-a");
        assert_eq!(first, delivery_id(webhook, "message-a"));
        assert_ne!(first, delivery_id(webhook, "message-b"));
        assert_eq!(first.len(), DELIVERY_PREFIX.len() + DELIVERY_TOKEN_LENGTH);
        assert_ne!(random_delivery_id(), random_delivery_id());
    }

    #[test]
    fn rfc3339_formats_epoch_and_leap_days() {
        assert_eq!(rfc3339_seconds(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_seconds(1_709_164_800_999), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn sent_at_omits_timestamps_outside_rfc3339_four_digit_years() {
        const LAST_VALID: i64 = 253_402_300_799_000;
        assert_eq!(
            rfc3339_seconds_checked(LAST_VALID).as_deref(),
            Some("9999-12-31T23:59:59Z")
        );
        assert_eq!(rfc3339_seconds_checked(LAST_VALID + 1_000), None);
        assert_eq!(rfc3339_seconds_checked(i64::MAX), None);
        assert_eq!(rfc3339_seconds_checked(-62_167_219_201_000), None);

        let mut message = message();
        message.sent_at_ms = Some(i64::MAX);
        assert_eq!(
            message_payload(Uuid::nil(), "https://app.peppy.pro", &message, None, &[], 0)["data"]["sent_at"],
            Value::Null
        );
    }

    #[test]
    fn test_event_has_no_message_content() {
        let value = test_payload(Uuid::nil(), "https://app.peppy.pro", 0);
        assert_eq!(value["type"], "webhook.test");
        assert_eq!(value["data"]["message"], "This is a test event from Peppy.");
    }
}
