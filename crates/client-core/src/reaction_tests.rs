//! Payload, apply-rule, compaction, and schema-61 tests for message reactions.

use super::*;

use peppy_crypto::{create_vault_check_header, derive_root_key};

const PASSPHRASE: &str = "correct horse battery staple";

pub(crate) struct Fixture {
    _dir: tempfile::TempDir,
    pub(crate) client: Client,
    pub(crate) vault_id: VaultId,
    pub(crate) profile: KeyProfile,
    pub(crate) event_key: PurposeKey,
    pub(crate) database_path: std::path::PathBuf,
    pub(crate) device_id: DeviceId,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        let root = derive_root_key(PASSPHRASE, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let device_id = DeviceId::new();
        let database_path = dir.path().join("reactions.db");
        let client = Client::open(
            ClientConfig {
                database_path: database_path.clone(),
                vault_id,
                device_id,
            },
            DatabaseKey::new(&[41; 32]).unwrap(),
        )
        .unwrap();
        client.unlock(&profile, &header, PASSPHRASE).unwrap();
        let event_key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
        Self {
            _dir: dir,
            client,
            vault_id,
            profile,
            event_key,
            database_path,
            device_id,
        }
    }

    pub(crate) fn event(
        &self,
        producer: DeviceId,
        sequence: u64,
        key_epoch: u32,
        payload: impl serde::Serialize,
    ) -> Envelope {
        let mut envelope = Envelope {
            protocol_version: PROTOCOL_VERSION,
            envelope_id: EnvelopeId::new(),
            command_id: None,
            vault_id: self.vault_id,
            producer_device_id: producer,
            producer_sequence: SourceSequence(sequence),
            key_epoch,
            crypto_suite: self.profile.crypto_suite,
            profile_fingerprint: self.profile.fingerprint().unwrap(),
            purpose: EnvelopePurpose::Event,
            route: None,
            compaction: None,
            origin_tag: None,
            ciphertext: Vec::new(),
        };
        let plain = serde_json::to_vec(&payload).unwrap();
        envelope.ciphertext =
            seal_frame(&encrypt(&self.event_key, &envelope.aad_bytes().unwrap(), &plain).unwrap());
        envelope
    }

    pub(crate) fn apply(&self, envelope: &Envelope, cursor: u64) -> ApplyReport {
        self.client.ingest(envelope, Cursor(cursor)).unwrap();
        self.client.apply_pending(100).unwrap()
    }

    pub(crate) fn execute(&self, sql: &str) {
        self.client.lock().unwrap().conn.execute(sql, []).unwrap();
    }

    /// First column of the first row as text; integers and text both read back as strings.
    pub(crate) fn query_text(&self, sql: &str) -> Option<String> {
        let store = self.client.lock().unwrap();
        let value: Option<rusqlite::types::Value> = store
            .conn
            .query_row(sql, [], |row| row.get(0))
            .optional()
            .unwrap();
        match value? {
            rusqlite::types::Value::Integer(number) => Some(number.to_string()),
            rusqlite::types::Value::Text(text) => Some(text),
            other => panic!("unexpected column value {other:?}"),
        }
    }
}

pub(crate) fn reaction(emoji: &str, actors: Vec<ReactionActor>) -> MessageReaction {
    MessageReaction {
        emoji: emoji.to_owned(),
        actors,
    }
}

pub(crate) fn actor(is_self: bool, address: Option<&str>) -> ReactionActor {
    ReactionActor {
        is_self,
        address: address.map(str::to_owned),
    }
}

pub(crate) fn grant_eligibility(
    fixture: &Fixture,
    gateway: DeviceId,
    conversation: ConversationId,
) {
    fixture.execute(&format!(
        "INSERT INTO rcs_link_eligibility_projection(gateway_device_id,conversation_id,eligibility,feature_bits,mapping_revision,observed_at_ms,producer_sequence) \
         VALUES('{gateway}','{conversation}','eligible',0,1,1,1)"
    ));
}

pub(crate) fn request_payload(
    request_id: ReactionRequestId,
    message_id: MessageId,
    conversation_id: ConversationId,
    target: DeviceId,
    emoji: Option<&str>,
) -> PrivatePayload {
    PrivatePayload::ReactionRequest {
        request_id,
        message_id,
        conversation_id,
        target_gateway_id: target,
        emoji: emoji.map(str::to_owned),
        requested_at_ms: notifications::now_ms(),
    }
}

pub(crate) fn state_of(fixture: &Fixture, request_id: ReactionRequestId) -> Option<String> {
    fixture.query_text(&format!(
        "SELECT state FROM reaction_requests WHERE request_id='{request_id}'"
    ))
}

#[test]
fn reaction_payloads_round_trip_with_their_snake_case_kinds() {
    let message_id = MessageId::new();
    let payloads = [
        (
            "reaction_request",
            request_payload(
                ReactionRequestId::new(),
                message_id,
                ConversationId::new(),
                DeviceId::new(),
                Some("👍"),
            ),
        ),
        (
            "reaction_result",
            PrivatePayload::ReactionResult {
                request_id: ReactionRequestId::new(),
                message_id,
                target_gateway_id: DeviceId::new(),
                state: ReactionResultState::NotSent {
                    reason: ReactionRefusal::MessageUnavailable,
                },
            },
        ),
        (
            "reaction_capability",
            PrivatePayload::ReactionCapability { supported: true },
        ),
        (
            "message_reactions",
            PrivatePayload::MessageReactions {
                message_id,
                conversation_id: ConversationId::new(),
                gateway_id: DeviceId::new(),
                observed_at_ms: 42,
                reactions: vec![reaction(
                    "👍",
                    vec![actor(true, None), actor(false, Some("+15550100"))],
                )],
            },
        ),
    ];
    for (kind, payload) in payloads {
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(value["kind"], kind);
        let decoded: PrivatePayload = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, payload, "{kind} must round-trip");
    }
}

#[test]
fn reaction_result_states_serialize_with_the_refusal_reason() {
    let value = serde_json::to_value(ReactionResultState::NotSent {
        reason: ReactionRefusal::InvalidEmoji,
    })
    .unwrap();
    assert_eq!(
        value,
        serde_json::json!({"not_sent": {"reason": "invalid_emoji"}})
    );
    assert_eq!(
        serde_json::to_value(ReactionResultState::Unknown).unwrap(),
        "unknown"
    );
}

#[test]
fn malformed_known_reaction_events_are_quarantined_not_tolerated() {
    let fixture = Fixture::new();
    let producer = DeviceId::new();
    for (cursor, kind) in [
        "reaction_request",
        "reaction_result",
        "reaction_capability",
        "message_reactions",
    ]
    .into_iter()
    .enumerate()
    {
        let envelope = fixture.event(
            producer,
            cursor as u64 + 1,
            1,
            serde_json::json!({ "kind": kind }),
        );
        let report = fixture.apply(&envelope, cursor as u64 + 1);
        assert_eq!(report.quarantined, 1, "{kind} must be strict");
        assert_eq!(report.applied, 0, "{kind} must not be silently consumed");
    }
}

#[test]
fn reaction_request_with_invalid_emoji_is_quarantined_and_valid_one_is_stored_sending() {
    let fixture = Fixture::new();
    let requester = DeviceId::new();
    let gateway = DeviceId::new();
    let conversation = ConversationId::new();
    let message = MessageId::new();
    for (index, emoji) in ["hello", "👍👍"].into_iter().enumerate() {
        let request = ReactionRequestId::new();
        let envelope = fixture.event(
            requester,
            index as u64 + 1,
            1,
            request_payload(request, message, conversation, gateway, Some(emoji)),
        );
        let report = fixture.apply(&envelope, index as u64 + 1);
        assert_eq!(report.quarantined, 1, "{emoji:?} must be refused");
        assert_eq!(state_of(&fixture, request), None);
    }
    let request = ReactionRequestId::new();
    let envelope = fixture.event(
        requester,
        3,
        1,
        request_payload(request, message, conversation, gateway, Some("👍🏽")),
    );
    assert_eq!(fixture.apply(&envelope, 3).applied, 1);
    assert_eq!(state_of(&fixture, request).as_deref(), Some("sending"));
    let stored_key_epoch = fixture.query_text(&format!(
        "SELECT key_epoch FROM reaction_requests WHERE request_id='{request}'"
    ));
    assert_eq!(stored_key_epoch.as_deref(), Some("1"));
}

#[test]
fn reaction_result_applies_only_from_the_target_gateway_and_only_while_sending() {
    let fixture = Fixture::new();
    let requester = DeviceId::new();
    let gateway = DeviceId::new();
    let impostor = DeviceId::new();
    let message = MessageId::new();
    let request = ReactionRequestId::new();
    let envelope = fixture.event(
        requester,
        1,
        1,
        request_payload(request, message, ConversationId::new(), gateway, None),
    );
    assert_eq!(fixture.apply(&envelope, 1).applied, 1);

    let result = PrivatePayload::ReactionResult {
        request_id: request,
        message_id: message,
        target_gateway_id: gateway,
        state: ReactionResultState::Sent,
    };
    let forged = fixture.event(impostor, 1, 1, result.clone());
    assert_eq!(fixture.apply(&forged, 2).quarantined, 1);
    assert_eq!(state_of(&fixture, request).as_deref(), Some("sending"));

    let genuine = fixture.event(gateway, 1, 1, result);
    assert_eq!(fixture.apply(&genuine, 3).applied, 1);
    assert_eq!(state_of(&fixture, request).as_deref(), Some("sent"));

    let late = PrivatePayload::ReactionResult {
        request_id: request,
        message_id: message,
        target_gateway_id: gateway,
        state: ReactionResultState::NotSent {
            reason: ReactionRefusal::Expired,
        },
    };
    let late_envelope = fixture.event(gateway, 2, 1, late);
    assert_eq!(fixture.apply(&late_envelope, 4).applied, 1);
    assert_eq!(
        state_of(&fixture, request).as_deref(),
        Some("sent"),
        "a terminal request is not rewritten"
    );
}

#[test]
fn reaction_capability_is_keyed_by_producer_and_never_regresses_within_an_epoch() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let capability = |supported| PrivatePayload::ReactionCapability { supported };
    assert_eq!(
        fixture
            .apply(&fixture.event(gateway, 5, 1, capability(true)), 1)
            .applied,
        1
    );
    assert_eq!(
        fixture
            .apply(&fixture.event(gateway, 4, 1, capability(false)), 2)
            .applied,
        1
    );
    let supported = fixture.query_text(&format!(
        "SELECT supported FROM reaction_capabilities WHERE device_id='{gateway}'"
    ));
    assert_eq!(
        supported.as_deref(),
        Some("1"),
        "an older sequence in the same epoch is ignored"
    );
    let other = DeviceId::new();
    assert_eq!(
        fixture
            .apply(&fixture.event(other, 1, 1, capability(false)), 3)
            .applied,
        1
    );
    let stored = fixture.query_text(&format!(
        "SELECT COUNT(*) FROM reaction_capabilities WHERE device_id='{gateway}'"
    ));
    assert_eq!(stored.as_deref(), Some("1"), "rows are keyed by producer");
}

#[test]
fn message_reactions_apply_only_from_the_eligible_owning_gateway_and_newest_wins() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let other_gateway = DeviceId::new();
    let conversation = ConversationId::new();
    let message = MessageId::new();
    let set = |emoji: &str, observed_at_ms: i64| PrivatePayload::MessageReactions {
        message_id: message,
        conversation_id: conversation,
        gateway_id: gateway,
        observed_at_ms,
        reactions: vec![reaction(emoji, vec![actor(false, Some("+15550100"))])],
    };

    assert_eq!(
        fixture
            .apply(&fixture.event(gateway, 1, 1, set("👍", 10)), 1)
            .applied,
        1,
        "no eligibility record yet: the set is staged, not quarantined"
    );
    grant_eligibility(&fixture, gateway, conversation);
    assert_eq!(
        fixture
            .apply(&fixture.event(other_gateway, 1, 1, set("👍", 10)), 2)
            .quarantined,
        1,
        "producer must be the gateway"
    );

    assert_eq!(
        fixture
            .apply(&fixture.event(gateway, 2, 1, set("👍", 20)), 3)
            .applied,
        1
    );
    assert_eq!(
        fixture
            .apply(&fixture.event(gateway, 3, 1, set("❤️", 15)), 4)
            .applied,
        1
    );
    let stored = fixture
        .query_text(&format!(
            "SELECT reactions_json FROM message_reactions WHERE message_id='{message}'"
        ))
        .unwrap();
    assert!(
        stored.contains("👍") && !stored.contains("❤️"),
        "older observation must not replace the set: {stored}"
    );

    assert_eq!(
        fixture
            .apply(&fixture.event(gateway, 4, 1, set("😂", 20)), 5)
            .applied,
        1
    );
    let stored = fixture
        .query_text(&format!(
            "SELECT reactions_json FROM message_reactions WHERE message_id='{message}'"
        ))
        .unwrap();
    assert!(
        stored.contains("😂"),
        "same observation time: the later producer sequence wins: {stored}"
    );
}

#[test]
fn message_reactions_with_a_violating_set_are_quarantined() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let conversation = ConversationId::new();
    grant_eligibility(&fixture, gateway, conversation);
    let message = MessageId::new();
    let bad_sets = [
        vec![
            reaction("👍", vec![actor(false, None)]),
            reaction("👍", vec![actor(false, None)]),
        ],
        vec![reaction("text", vec![actor(false, None)])],
        vec![reaction("👍", Vec::new())],
        vec![
            reaction("👍", vec![actor(true, None)]),
            reaction("❤️", vec![actor(true, None)]),
        ],
    ];
    for (index, reactions) in bad_sets.into_iter().enumerate() {
        let payload = PrivatePayload::MessageReactions {
            message_id: message,
            conversation_id: conversation,
            gateway_id: gateway,
            observed_at_ms: 1,
            reactions,
        };
        let envelope = fixture.event(gateway, index as u64 + 1, 1, payload);
        assert_eq!(
            fixture.apply(&envelope, index as u64 + 1).quarantined,
            1,
            "bad set {index}"
        );
    }
    assert_eq!(
        fixture
            .query_text("SELECT COUNT(*) FROM message_reactions")
            .as_deref(),
        Some("0")
    );
}

#[test]
fn reaction_events_past_the_storage_sequence_range_are_quarantined() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let envelope = fixture.event(
        gateway,
        u64::MAX,
        1,
        PrivatePayload::ReactionCapability { supported: true },
    );
    let report = fixture.apply(&envelope, 1);
    assert_eq!(report.quarantined, 1);
    assert_eq!(report.applied, 0);
}

#[test]
fn reaction_compaction_groups_follow_the_spec() {
    let message = MessageId::new();
    let request = ReactionRequestId::new();
    let gateway = DeviceId::new();
    let group = |payload: &PrivatePayload| compaction_group(payload).unwrap().unwrap();
    let request_group = group(&request_payload(
        request,
        message,
        ConversationId::new(),
        gateway,
        None,
    ));
    assert!(request_group.2, "requests are terminal per request_id");
    let result_group = group(&PrivatePayload::ReactionResult {
        request_id: request,
        message_id: message,
        target_gateway_id: gateway,
        state: ReactionResultState::Sent,
    });
    assert_eq!(
        result_group.1, request_group.1,
        "request and result share the request_id key"
    );
    assert!(result_group.2);
    let set = group(&PrivatePayload::MessageReactions {
        message_id: message,
        conversation_id: ConversationId::new(),
        gateway_id: gateway,
        observed_at_ms: 1,
        reactions: Vec::new(),
    });
    assert!(
        !set.2,
        "message reaction sets are non-terminal like RcsMessageStatus"
    );
    assert!(
        compaction_group(&PrivatePayload::ReactionCapability { supported: true })
            .unwrap()
            .is_none()
    );
}

#[test]
fn reaction_compose_text_matches_the_shared_fixture() {
    let entries: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../packages/contracts/fixtures/reaction-text.json"
    ))
    .unwrap();
    assert!(!entries.is_empty());
    for entry in entries {
        let body = entry["body"].as_str().unwrap();
        let emoji = entry["emoji"].as_str().unwrap();
        let text = entry["text"].as_str().unwrap();
        assert_eq!(compose_reaction_text(body, emoji), text, "{body}");
    }
}

#[test]
fn reaction_schema_fresh_install_creates_the_schema_61_objects_and_constraints() {
    let fixture = Fixture::new();
    let store = fixture.client.lock().unwrap();
    let version: i64 = store
        .conn
        .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 63);
    for table in [
        "reaction_requests",
        "reaction_capabilities",
        "message_reactions",
        "reaction_attempts",
    ] {
        assert!(table_exists(&store.conn, table).unwrap(), "missing {table}");
    }
    let index_exists: bool = store.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name='rcs_message_identities_message_id')",
        [],
        |row| row.get(0),
    ).unwrap();
    assert!(index_exists);
    assert!(store.conn.execute(
        "INSERT INTO reaction_requests(request_id,message_id,conversation_id,target_gateway_id,requested_at_ms,requester_device_id,key_epoch,state,updated_at_ms)
         VALUES('r','m','c','g',0,'d',1,'bogus',0)",
        [],
    ).is_err());
}

fn reopen(database_path: std::path::PathBuf, vault_id: VaultId, device_id: DeviceId) -> Client {
    let config = ClientConfig {
        database_path,
        vault_id,
        device_id,
    };
    Client::open(config, DatabaseKey::new(&[41; 32]).unwrap()).unwrap()
}

#[test]
fn reaction_schema_60_upgrade_adds_the_reaction_objects_and_index() {
    let fixture = Fixture::new();
    {
        let store = fixture.client.lock().unwrap();
        store
            .conn
            .execute_batch(
                "DROP INDEX rcs_message_identities_message_id;
                 DROP TABLE reaction_attempts;
                 DROP TABLE message_reactions;
                 DROP TABLE reaction_capabilities;
                 DROP TABLE reaction_requests;
                 UPDATE schema_meta SET version=60;",
            )
            .unwrap();
    }
    let (path, vault, device) = (
        fixture.database_path.clone(),
        fixture.vault_id,
        fixture.device_id,
    );
    drop(fixture.client);
    let reopened = reopen(path, vault, device);
    let store = reopened.lock().unwrap();
    let version: i64 = store
        .conn
        .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 63);
    for table in [
        "reaction_requests",
        "reaction_capabilities",
        "message_reactions",
        "reaction_attempts",
    ] {
        assert!(table_exists(&store.conn, table).unwrap(), "missing {table}");
    }
    let index_exists: bool = store
        .conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name='rcs_message_identities_message_id')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(index_exists);
}

/// Re-opens the database under the same key and unlocks it, as a restarted phone would.
pub(crate) fn reopen_unlocked(fixture: Fixture) -> (tempfile::TempDir, Client, KeyProfile) {
    let Fixture {
        _dir,
        client,
        vault_id,
        profile,
        database_path,
        device_id,
        ..
    } = fixture;
    drop(client);
    let config = ClientConfig {
        database_path,
        vault_id,
        device_id,
    };
    let reopened = Client::open(config, DatabaseKey::new(&[41; 32]).unwrap()).unwrap();
    let root = derive_root_key(PASSPHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    reopened.unlock(&profile, &header, PASSPHRASE).unwrap();
    (_dir, reopened, profile)
}

/// Inserts a message the way the core stores one, so projections and phone checks read it.
pub(crate) fn seed_message(
    fixture: &Fixture,
    transport: Transport,
    direction: Direction,
    body: &str,
    conversation_id: ConversationId,
) -> MessageId {
    let message_id = MessageId::new();
    let payload = MessagePayload {
        record: MessageRecord {
            message_id,
            conversation_id,
            source_sequence: SourceSequence(1),
            attachments: Vec::new(),
        },
        source_device_id: DeviceId::new(),
        provider_message_id: None,
        sender_address: (direction == Direction::Incoming).then(|| "+15550100".to_owned()),
        recipients: vec!["+15550199".to_owned()],
        subject: None,
        body: body.to_owned(),
        transport,
        direction,
        rcs_capture: false,
        imported: false,
        sent_at_ms: Some(1),
        ordered_at_ms: Some(1),
        mms_context: None,
    };
    let store = fixture.client.lock().unwrap();
    insert_message(&store.conn, &payload).unwrap();
    message_id
}

/// Links an RCS message to a Google id and the link mirror, as the link receive path does.
pub(crate) fn seed_rcs_identity(
    fixture: &Fixture,
    message_id: MessageId,
    conversation_id: ConversationId,
    google_message: &str,
) {
    let store = fixture.client.lock().unwrap();
    store.conn.execute(
        "INSERT INTO rcs_message_identities(google_message_id,google_conversation_id,conversation_id,ownership_generation,message_id) VALUES(?,?,?,1,?)",
        params![google_message, "google-conversation", conversation_id.to_string(), message_id.to_string()],
    ).unwrap();
    crate::rcs_mirror::record(
        &store.conn,
        crate::rcs_mirror::MirrorKey {
            mirror_id: google_message,
            conversation_id,
            source: crate::rcs_mirror::MirrorSource::Link,
        },
        message_id,
    )
    .unwrap();
}

/// Marks the local RCS link as linked, the state `current_link_state` reads.
pub(crate) fn mark_link_linked(fixture: &Fixture) {
    fixture.execute("INSERT INTO metadata(k,v) VALUES('rcs_link_lifecycle_v1','\"linked\"') ON CONFLICT(k) DO UPDATE SET v=excluded.v");
}

#[test]
fn reaction_unfinished_attempt_is_unknown_after_restart_and_never_retried() {
    let fixture = Fixture::new();
    let request = ReactionRequestId::new();
    let own = fixture.device_id;
    fixture.execute(&format!(
        "INSERT INTO reaction_requests(request_id,message_id,conversation_id,target_gateway_id,emoji,requested_at_ms,requester_device_id,key_epoch,state,updated_at_ms)
         VALUES('{request}','{}','{}','{own}','👍',{now},'{}',1,'sending',0)",
        MessageId::new(),
        ConversationId::new(),
        DeviceId::new(),
        now = notifications::now_ms(),
    ));
    fixture.execute(&format!(
        "INSERT INTO reaction_attempts(request_id,begun_at_ms) VALUES('{request}',0)"
    ));
    let outbox_before = outbox_count(&fixture);
    let (_dir, reopened, _profile) = reopen_unlocked(fixture);
    assert!(
        reopened.pending_reaction_jobs().unwrap().is_empty(),
        "an unfinished attempt is never a job"
    );
    let state: String = reopened
        .lock()
        .unwrap()
        .conn
        .query_row(
            "SELECT state FROM reaction_requests WHERE request_id=?",
            [request.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "unknown");
    assert_eq!(
        reopened.begin_reaction_attempt(request).unwrap(),
        ReactionAttemptDecision::Refused {
            reason: ReactionRefusal::Busy
        }
    );
    assert_eq!(
        outbox_before + 1,
        outbox_count_of(&reopened),
        "exactly one ReactionResult is queued"
    );
}

pub(crate) fn outbox_count(fixture: &Fixture) -> i64 {
    outbox_count_of(&fixture.client)
}

pub(crate) fn outbox_count_of(client: &Client) -> i64 {
    client
        .lock()
        .unwrap()
        .conn
        .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
        .unwrap()
}
