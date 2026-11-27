#[path = "common/mod.rs"]
mod common;

use common::*;
use rusqlite::{Connection, OptionalExtension, params};
use std::time::{SystemTime, UNIX_EPOCH};

const GOOGLE_CONVERSATION: &str = "google-conversation";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn sql(config: &ClientConfig) -> Connection {
    let connection = Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    connection
}

fn publish_gateway(client: &Client, config: &ClientConfig) {
    let observed_at_ms = now();
    client
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: config.device_id,
                state: RcsCapabilityState::Linked,
                availability: true,
                health: true,
                phone_responsive: true,
                observed_at_ms,
                capability_revision: 1,
                feature_bits: 1,
                linked_since_ms: Some(observed_at_ms),
            },
            &[],
            observed_at_ms,
        )
        .unwrap();
}

struct Fixture {
    _directory: tempfile::TempDir,
    config: ClientConfig,
    client: Client,
    conversation: ConversationId,
    generation: u64,
}

fn fixture() -> Fixture {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-reply", &vault);
    let client = unlocked(&config, &vault);
    publish_gateway(&client, &config);
    let conversation = ConversationId::new();
    let ownership = client.begin_rcs_link_ownership(conversation, 0).unwrap();
    Fixture {
        _directory: directory,
        config,
        client,
        conversation,
        generation: ownership.ownership_generation,
    }
}

fn record(google_message_id: &str, google_conversation_id: &str) -> RcsProjectedRecord {
    RcsProjectedRecord {
        google_message_id: google_message_id.into(),
        google_conversation_id: google_conversation_id.into(),
        direction: Direction::Incoming,
        body: format!("body-{google_message_id}"),
        sent_at_ms: Some(now()),
        tmp_id: None,
        rcs_reported: false,
        e2ee_reported: None,
        alternate_message_ids: Vec::new(),
        attachments: Vec::new(),
    }
}

impl Fixture {
    fn ingest(&self, record: RcsProjectedRecord, reply: Option<&str>) -> RcsReceiveOutcome {
        self.client
            .ingest_rcs_receive_observation_replying(
                self.conversation,
                self.generation,
                record,
                None,
                RcsObservationProvenance::Live,
                reply,
            )
            .unwrap()
    }

    /// The stored message whose body is `body-<google_message_id>`.
    fn stored(&self, google_message_id: &str) -> Message {
        let body = format!("body-{google_message_id}");
        self.client
            .messages(self.conversation)
            .unwrap()
            .into_iter()
            .find(|message| message.payload.body == body)
            .unwrap()
    }

    fn unresolved_count(&self) -> i64 {
        sql(&self.config)
            .query_row(
                "SELECT CAST(v AS INTEGER) FROM metadata WHERE k='rcs_reply_unresolved_count'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
            .unwrap_or(0)
    }
}

#[test]
fn reply_to_a_known_target_is_stored_on_the_new_message() {
    let fixture = fixture();
    fixture.ingest(record("target", GOOGLE_CONVERSATION), None);
    let target = fixture.stored("target").payload.record.message_id;

    let outcome = fixture.ingest(record("reply", GOOGLE_CONVERSATION), Some("target"));

    assert_eq!(outcome, RcsReceiveOutcome::Inserted);
    assert_eq!(fixture.stored("reply").payload.reply_to, Some(target));
    assert_eq!(fixture.unresolved_count(), 0);
}

#[test]
fn a_target_under_another_google_conversation_is_not_resolved() {
    let fixture = fixture();
    fixture.ingest(record("target", "other-google-conversation"), None);

    fixture.ingest(record("reply", GOOGLE_CONVERSATION), Some("target"));

    assert_eq!(fixture.stored("reply").payload.reply_to, None);
    assert_eq!(fixture.unresolved_count(), 1);
}

#[test]
fn an_unknown_target_leaves_the_reply_unlinked_and_counts_the_diagnostic() {
    let fixture = fixture();

    let outcome = fixture.ingest(record("reply", GOOGLE_CONVERSATION), Some("never-seen"));

    assert_eq!(outcome, RcsReceiveOutcome::Inserted);
    assert_eq!(fixture.stored("reply").payload.reply_to, None);
    assert_eq!(fixture.unresolved_count(), 1);
}

#[test]
fn a_record_without_a_reply_id_is_unchanged() {
    let fixture = fixture();
    fixture.ingest(record("target", GOOGLE_CONVERSATION), None);

    fixture.ingest(record("plain", GOOGLE_CONVERSATION), None);

    assert_eq!(fixture.stored("plain").payload.reply_to, None);
    assert_eq!(fixture.unresolved_count(), 0);
}

#[test]
fn an_invalid_reply_id_is_absent_without_a_diagnostic() {
    let fixture = fixture();
    fixture.ingest(record("target", GOOGLE_CONVERSATION), None);
    let long = "x".repeat(513);

    for (index, invalid) in ["", "bad\nid", "bad\tid", long.as_str()]
        .into_iter()
        .enumerate()
    {
        let google_message_id = format!("plain-{index}");
        let outcome = fixture.ingest(
            record(&google_message_id, GOOGLE_CONVERSATION),
            Some(invalid),
        );
        assert_eq!(outcome, RcsReceiveOutcome::Inserted);
        assert_eq!(fixture.stored(&google_message_id).payload.reply_to, None);
    }
    assert_eq!(fixture.unresolved_count(), 0);
}

#[test]
fn a_duplicate_keeps_the_payload_it_already_stored() {
    let fixture = fixture();
    fixture.ingest(record("target", GOOGLE_CONVERSATION), None);
    fixture.ingest(record("reply", GOOGLE_CONVERSATION), None);

    let outcome = fixture.ingest(record("reply", GOOGLE_CONVERSATION), Some("target"));

    assert_eq!(outcome, RcsReceiveOutcome::Duplicate);
    assert_eq!(fixture.stored("reply").payload.reply_to, None);
}

#[test]
fn a_target_in_a_merged_conversation_resolves_to_its_survivor() {
    let fixture = fixture();
    let source = ConversationId::new();
    let source_generation = fixture
        .client
        .begin_rcs_link_ownership(source, 0)
        .unwrap()
        .ownership_generation;
    fixture
        .client
        .ingest_rcs_receive_observation_replying(
            source,
            source_generation,
            record("target", GOOGLE_CONVERSATION),
            None,
            RcsObservationProvenance::Live,
            None,
        )
        .unwrap();
    sql(&fixture.config)
        .execute(
            "INSERT INTO conversation_merges(source_id,target_id) VALUES(?,?)",
            params![source.to_string(), fixture.conversation.to_string()],
        )
        .unwrap();
    let target = fixture
        .client
        .messages(source)
        .unwrap()
        .into_iter()
        .next()
        .unwrap()
        .payload
        .record
        .message_id;

    fixture.ingest(record("reply", GOOGLE_CONVERSATION), Some("target"));

    assert_eq!(fixture.stored("reply").payload.reply_to, Some(target));
}
