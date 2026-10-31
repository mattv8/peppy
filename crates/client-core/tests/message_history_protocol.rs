//! Public carrier-history API: capability revision ownership, coalescing and strict wire values.

use crate::common::*;
use serde_json::{Value, json};
use tempfile::TempDir;

fn value(result: Result<String, Error>) -> Value {
    serde_json::from_str(&result.unwrap()).unwrap()
}

fn source(generation: &str, permission: bool) -> String {
    json!({"generation":generation,"capabilities":{"sms":true,"mms":false},"permission":permission,"policy":true,"exhausted":false}).to_string()
}

#[test]
fn source_advertisement_is_fail_closed_and_projects_only_protocol_fields() {
    let directory = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "history-source", &vault), &vault);

    let advertisement = json!({
        "generation": "1",
        "capabilities": {"sms": true, "mms": false},
        "permission": true,
        "policy": true,
        "exhausted": false,
        "query": "must never leave local search",
        "raw_provider_id": "42"
    });
    let published = value(client.publish_message_history_source(&advertisement.to_string()));
    assert_eq!(published["generation"], "1");
    assert_eq!(published["revision"], "1");
    assert_eq!(published["published"], true);
    assert!(published.get("query").is_none());
    assert!(published.get("raw_provider_id").is_none());

    // A genuinely new advertisement needs the outbox, which is fenced without compaction.
    client.set_server_compaction_state(false, false).unwrap();
    assert!(
        client
            .publish_message_history_source(&source("2", true))
            .is_err()
    );
}

#[test]
fn revision_is_derived_and_unchanged_calls_do_not_flood_the_outbox() {
    let directory = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "revision", &vault), &vault);

    let first = value(client.publish_message_history_source(&source("1", true)));
    assert_eq!(
        (first["revision"].as_str(), first["published"].as_bool()),
        (Some("1"), Some(true))
    );
    let outbox = client.pending_outbox().unwrap().len();

    let same = value(client.publish_message_history_source(&source("1", true)));
    assert_eq!(
        (same["revision"].as_str(), same["published"].as_bool()),
        (Some("1"), Some(false))
    );
    assert_eq!(client.pending_outbox().unwrap().len(), outbox);

    let changed = value(client.publish_message_history_source(&source("1", false)));
    assert_eq!(
        (changed["revision"].as_str(), changed["published"].as_bool()),
        (Some("2"), Some(true))
    );

    let forced_input = json!({"generation":"1","capabilities":{"sms":true,"mms":false},"permission":false,"policy":true,"exhausted":false,"force_republish":true}).to_string();
    let forced = value(client.publish_message_history_source(&forced_input));
    assert_eq!(
        (forced["revision"].as_str(), forced["published"].as_bool()),
        (Some("3"), Some(true))
    );
    assert_eq!(client.pending_outbox().unwrap().len(), outbox + 2);

    // An explicit revision that disagrees with the stored one is rejected even for unchanged fields.
    let stale = json!({"generation":"1","revision":"1","capabilities":{"sms":true,"mms":false},"permission":false,"policy":true,"exhausted":false}).to_string();
    assert!(client.publish_message_history_source(&stale).is_err());
}

#[test]
fn wire_and_capability_values_are_strict() {
    let directory = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "strict", &vault), &vault);
    for input in [
        source("01", true),
        json!({"generation":1,"capabilities":{"sms":true,"mms":false},"permission":true,"policy":true,"exhausted":false}).to_string(),
        json!({"generation":"1","revision":"+1","capabilities":{"sms":true,"mms":false},"permission":true,"policy":true,"exhausted":false}).to_string(),
        json!({"generation":"1","capabilities":{"sms":false,"mms":false},"permission":true,"policy":true,"exhausted":false}).to_string(),
        json!({"generation":"1","capabilities":{"sms":true,"mms":false},"permission":"true","policy":true,"exhausted":false}).to_string(),
    ] {
        assert!(client.publish_message_history_source(&input).is_err(), "{input}");
    }
}

#[test]
fn progress_rejects_unbounded_reasons_and_non_canonical_receipts() {
    let directory = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "progress-strict", &vault), &vault);
    let base = |state: &str, reason: Value, receipts: Value| {
        json!({"schema_version":1,"request_id":uuid::Uuid::new_v4().to_string(),"target":"not-this-device","generation":"1","key_epoch":1,"state":state,"reason":reason,"receipts":receipts}).to_string()
    };
    let message_id = uuid::Uuid::new_v4().to_string();
    let receipt = json!({"messageId": message_id, "revision": "1"});
    for input in [
        base("blocked", json!("free text"), json!([])),
        base("completed", json!("permission_denied"), json!([])),
        base("queued", Value::Null, json!([])),
        base(
            "completed",
            Value::Null,
            json!([{"messageId": message_id.to_uppercase(), "revision": "1"}]),
        ),
        base(
            "completed",
            Value::Null,
            json!([{"messageId": message_id, "revision": "1", "body": "leak"}]),
        ),
        base(
            "completed",
            Value::Null,
            json!([{"messageId": message_id, "revision": "1", "query": "x"}]),
        ),
        base("completed", Value::Null, json!(vec![receipt; 201])),
    ] {
        assert!(
            client.record_message_history_progress(&input).is_err(),
            "{input}"
        );
    }
}

#[test]
fn pending_is_empty_without_work_and_cancel_unknown_is_not_found() {
    let directory = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "empty-work", &vault), &vault);
    let pending = value(client.pending_message_history_requests("{}"));
    assert_eq!(pending["requests"].as_array().unwrap().len(), 0);
    let unknown = json!({"request_id": uuid::Uuid::new_v4().to_string()}).to_string();
    assert!(client.cancel_message_history_request(&unknown).is_err());
    assert_eq!(
        client.message_history_request_status(&unknown).unwrap(),
        "null"
    );
}
