use super::*;

use peppy_crypto::{create_vault_check_header, derive_root_key};
use std::path::PathBuf;

const PASSPHRASE: &str = "correct horse battery staple";

struct Fixture {
    _dir: tempfile::TempDir,
    client: Client,
    vault_id: VaultId,
    device_id: DeviceId,
    database_path: PathBuf,
    profile: KeyProfile,
    event_key: PurposeKey,
}

impl Fixture {
    fn new() -> Self {
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        Self::with_profile(vault_id, profile)
    }

    fn with_profile(vault_id: VaultId, profile: KeyProfile) -> Self {
        let dir = tempfile::TempDir::new().unwrap();
        let root = derive_root_key(PASSPHRASE, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let device_id = DeviceId::new();
        let database_path = dir.path().join("cancellation-validation.db");
        let client = Client::open(
            ClientConfig {
                database_path: database_path.clone(),
                vault_id,
                device_id,
            },
            DatabaseKey::new(&[31; 32]).unwrap(),
        )
        .unwrap();
        client.unlock(&profile, &header, PASSPHRASE).unwrap();
        let event_key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
        Self {
            _dir: dir,
            client,
            vault_id,
            device_id,
            database_path,
            profile,
            event_key,
        }
    }

    fn event(&self, producer: DeviceId, sequence: u64, payload: impl serde::Serialize) -> Envelope {
        let mut envelope = Envelope {
            protocol_version: PROTOCOL_VERSION,
            envelope_id: EnvelopeId::new(),
            command_id: None,
            vault_id: self.vault_id,
            producer_device_id: producer,
            producer_sequence: SourceSequence(sequence),
            key_epoch: 1,
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

    fn apply(&self, envelope: &Envelope, cursor: u64) -> ApplyReport {
        self.client.ingest(envelope, Cursor(cursor)).unwrap();
        self.client.apply_pending(100).unwrap()
    }
}

fn fixture_pair() -> (Fixture, Fixture) {
    let vault_id = VaultId::new();
    let profile = KeyProfile::new(vault_id.0, 1).unwrap();
    (
        Fixture::with_profile(vault_id, profile.clone()),
        Fixture::with_profile(vault_id, profile),
    )
}

fn receive_queued_command(source: &Fixture, gateway: &Fixture, queued: QueuedSend) -> u64 {
    let envelopes = source.client.pending_outbox().unwrap();
    for (index, envelope) in envelopes.iter().enumerate() {
        gateway
            .client
            .ingest(envelope, Cursor(index as u64 + 1))
            .unwrap();
    }
    let report = gateway.client.apply_pending(100).unwrap();
    assert_eq!(report.quarantined, 0);
    let store = gateway.client.lock().unwrap();
    let ledger: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM command_ledger WHERE command_id=?",
            [queued.command_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        ledger, 1,
        "the executing gateway must receive the command ledger row"
    );
    envelopes.len() as u64
}

#[test]
fn malformed_known_cancellation_events_are_quarantined_not_tolerated_as_future_events() {
    let fixture = Fixture::new();
    let producer = DeviceId::new();

    for (cursor, kind) in [
        "send_cancellation_request",
        "send_cancellation_result",
        "send_cancellation_capability",
    ]
    .into_iter()
    .enumerate()
    {
        let envelope = fixture.event(
            producer,
            cursor as u64 + 1,
            serde_json::json!({ "kind": kind }),
        );
        let report = fixture.apply(&envelope, cursor as u64 + 1);
        assert_eq!(report.quarantined, 1, "{kind} must be strict");
        assert_eq!(report.applied, 0, "{kind} must not be silently consumed");
    }
}

#[test]
fn stale_same_epoch_capability_event_cannot_regress_a_newer_sequence() {
    let fixture = Fixture::new();
    let producer = DeviceId::new();

    let newer_true = fixture.event(
        producer,
        2,
        PrivatePayload::SendCancellationCapability { supported: true },
    );
    fixture.apply(&newer_true, 1);
    let stale_false = fixture.event(
        producer,
        1,
        PrivatePayload::SendCancellationCapability { supported: false },
    );
    fixture.apply(&stale_false, 2);

    let store = fixture.client.lock().unwrap();
    let row: (bool, u32, i64) = store
        .conn
        .query_row(
            "SELECT supported,key_epoch,producer_sequence FROM send_cancellation_capabilities WHERE device_id=?",
            [producer.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, (true, 1, 2));
}

fn queue_local_send(fixture: &Fixture, body: &str) -> QueuedSend {
    fixture
        .client
        .queue_send(
            OutgoingSms {
                conversation_id: ConversationId::new(),
                recipients: vec!["+15555550100".into()],
                body: body.into(),
            },
            GatewayRoute::Carrier {
                gateway_device_id: fixture.device_id,
                subscription_id: "sim-1".into(),
            },
        )
        .unwrap()
}

#[test]
fn bound_result_before_request_keeps_each_terminal_state_and_identical_echo_idempotent() {
    for state in [
        SendCancellationState::Confirmed,
        SendCancellationState::TooLate,
        SendCancellationState::Unknown,
    ] {
        let fixture = Fixture::new();
        let queued = queue_local_send(&fixture, "terminal result ordering");
        let result = PrivatePayload::SendCancellationResult {
            message_id: queued.message_id,
            command_id: queued.command_id,
            target_gateway_id: fixture.device_id,
            state,
        };
        let result_envelope = fixture.event(fixture.device_id, 1, &result);
        assert_eq!(
            fixture.apply(&result_envelope, 1).quarantined,
            0,
            "{state:?}"
        );

        let request = fixture.event(
            DeviceId::new(),
            1,
            PrivatePayload::SendCancellationRequest {
                message_id: queued.message_id,
                command_id: queued.command_id,
                target_gateway_id: fixture.device_id,
            },
        );
        assert_eq!(fixture.apply(&request, 2).quarantined, 0, "{state:?}");
        let echo = fixture.event(fixture.device_id, 2, &result);
        assert_eq!(fixture.apply(&echo, 3).quarantined, 0, "{state:?}");
        assert_eq!(
            fixture
                .client
                .message_send_cancellation(queued.message_id)
                .unwrap()
                .state,
            state,
            "{state:?} must survive request arrival and an identical executor echo"
        );
    }
}

#[test]
fn schema_57_upgrade_preserves_queued_and_attempted_send_rows() {
    let fixture = Fixture::new();
    let queued = queue_local_send(&fixture, "queued before migration");
    let attempted = queue_local_send(&fixture, "attempted before migration");
    {
        let store = fixture.client.lock().unwrap();
        store.conn.execute(
            "INSERT INTO command_ledger(command_id,key_epoch,subscription_id,payload,retired,historical)
             SELECT ?,1,'sim-1',payload,0,0 FROM messages WHERE id=?",
            params![attempted.command_id.to_string(), attempted.message_id.to_string()],
        ).unwrap();
        record_finalized_attempt(&store.conn, attempted.command_id, "carrier", 1).unwrap();
    }
    let config = ClientConfig {
        database_path: fixture.database_path.clone(),
        vault_id: fixture.vault_id,
        device_id: fixture.device_id,
    };
    let profile = fixture.profile.clone();
    drop(fixture.client);

    let connection = Connection::open(&config.database_path).unwrap();
    apply_database_key(&connection, &DatabaseKey::new(&[31; 32]).unwrap()).unwrap();
    connection
        .execute_batch(
            "DROP TABLE send_cancellation_capabilities;
             DROP TABLE send_cancellations;
             UPDATE schema_meta SET version=57;",
        )
        .unwrap();
    drop(connection);

    let root = derive_root_key(PASSPHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    let reopened = Client::open(config, DatabaseKey::new(&[31; 32]).unwrap()).unwrap();
    reopened.unlock(&profile, &header, PASSPHRASE).unwrap();
    let store = reopened.lock().unwrap();
    let version: i64 = store
        .conn
        .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
        .unwrap();
    let commands: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM commands WHERE command_id IN (?,?)",
            params![
                queued.command_id.to_string(),
                attempted.command_id.to_string()
            ],
            |row| row.get(0),
        )
        .unwrap();
    let attempt: String = store
        .conn
        .query_row(
            "SELECT state FROM attempts WHERE command_id=?",
            [attempted.command_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    assert_eq!(commands, 2);
    assert_eq!(attempt, "outcome_unknown");
}

#[test]
fn restore_guard_and_historical_ledger_never_emit_a_fresh_confirmation() {
    for historical in [false, true] {
        let (source, gateway) = fixture_pair();
        let queued = source
            .client
            .queue_send(
                OutgoingSms {
                    conversation_id: ConversationId::new(),
                    recipients: vec!["+15555550100".into()],
                    body: "must not confirm during recovery".into(),
                },
                GatewayRoute::Carrier {
                    gateway_device_id: gateway.device_id,
                    subscription_id: "sim-1".into(),
                },
            )
            .unwrap();
        let request_cursor = receive_queued_command(&source, &gateway, queued.clone()) + 1;
        let request = source.event(
            source.device_id,
            3,
            PrivatePayload::SendCancellationRequest {
                message_id: queued.message_id,
                command_id: queued.command_id,
                target_gateway_id: gateway.device_id,
            },
        );
        let request_report = gateway.apply(&request, request_cursor);
        assert_eq!((request_report.applied, request_report.quarantined), (1, 0));
        assert_eq!(
            gateway
                .client
                .message_send_cancellation(queued.message_id)
                .unwrap()
                .state,
            SendCancellationState::Requested
        );
        let outbox_before = gateway.client.pending_outbox_count().unwrap();
        {
            let store = gateway.client.lock().unwrap();
            if historical {
                let changed = store
                    .conn
                    .execute(
                        "UPDATE command_ledger SET historical=1 WHERE command_id=?",
                        [queued.command_id.to_string()],
                    )
                    .unwrap();
                assert_eq!(changed, 1);
            } else {
                set_meta(&store.conn, "restore_guard", "1").unwrap();
            }
        }
        assert!(!gateway.client.process_pending_send_cancellations().unwrap());
        assert_eq!(
            gateway.client.pending_outbox_count().unwrap(),
            outbox_before
        );
        assert_eq!(
            gateway
                .client
                .message_send_cancellation(queued.message_id)
                .unwrap()
                .state,
            SendCancellationState::Requested
        );
    }
}

#[test]
fn media_child_with_a_permit_cannot_confirm_the_whole_message_or_erase_unknown_evidence() {
    let (source, gateway) = fixture_pair();
    let queued = source
        .client
        .queue_send(
            OutgoingSms {
                conversation_id: ConversationId::new(),
                recipients: vec!["+15555550100".into()],
                body: "media child outcome is durable evidence".into(),
            },
            GatewayRoute::Carrier {
                gateway_device_id: gateway.device_id,
                subscription_id: "sim-1".into(),
            },
        )
        .unwrap();
    let request_cursor = receive_queued_command(&source, &gateway, queued.clone()) + 1;
    let attachment_id = AttachmentId::new();
    {
        let store = gateway.client.lock().unwrap();
        store.conn.execute(
            "INSERT INTO rcs_media_send_ledger(command_id,attachment_index,attachment_id,state,permit_at_ms)
             VALUES(?,0,?,'outcome_unknown',1)",
            params![queued.command_id.to_string(), attachment_id.to_string()],
        ).unwrap();
    }
    let request = source.event(
        source.device_id,
        3,
        PrivatePayload::SendCancellationRequest {
            message_id: queued.message_id,
            command_id: queued.command_id,
            target_gateway_id: gateway.device_id,
        },
    );
    let request_report = gateway.apply(&request, request_cursor);
    assert_eq!((request_report.applied, request_report.quarantined), (1, 0));
    assert_eq!(
        gateway
            .client
            .message_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::Requested
    );
    assert!(gateway.client.process_pending_send_cancellations().unwrap());
    assert_eq!(
        gateway
            .client
            .message_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::TooLate
    );
    let store = gateway.client.lock().unwrap();
    let child: (String, Option<i64>) = store.conn.query_row(
        "SELECT state,permit_at_ms FROM rcs_media_send_ledger WHERE command_id=? AND attachment_index=0",
        [queued.command_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(child, ("outcome_unknown".into(), Some(1)));
}

#[test]
fn wrong_executor_and_unbound_confirmations_never_confirm_or_hide_a_message() {
    let fixture = Fixture::new();
    let gateway = DeviceId::new();
    let conversation_id = ConversationId::new();
    let queued = fixture
        .client
        .queue_send(
            OutgoingSms {
                conversation_id,
                recipients: vec!["+15555550100".into()],
                body: "must remain visible".into(),
            },
            GatewayRoute::Carrier {
                gateway_device_id: gateway,
                subscription_id: "sim-1".into(),
            },
        )
        .unwrap();
    let impostor = DeviceId::new();
    let wrong_executor = fixture.event(
        impostor,
        1,
        PrivatePayload::SendCancellationResult {
            message_id: queued.message_id,
            command_id: queued.command_id,
            target_gateway_id: impostor,
            state: SendCancellationState::Confirmed,
        },
    );
    let report = fixture.apply(&wrong_executor, 1);
    assert_eq!(report.quarantined, 1);
    assert_eq!(
        fixture
            .client
            .message_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::None
    );
    assert_eq!(fixture.client.messages(conversation_id).unwrap().len(), 1);

    let unbound_message = MessageId::new();
    let unbound = fixture.event(
        gateway,
        2,
        PrivatePayload::SendCancellationResult {
            message_id: unbound_message,
            command_id: CommandId::new(),
            target_gateway_id: gateway,
            state: SendCancellationState::Confirmed,
        },
    );
    assert_eq!(fixture.apply(&unbound, 2).quarantined, 0);
    let store = fixture.client.lock().unwrap();
    let row: (String, Option<String>, bool) = store
        .conn
        .query_row(
            "SELECT state,staged_state,bound FROM send_cancellations WHERE message_id=?",
            [unbound_message.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, ("requested".into(), Some("confirmed".into()), false));
}

#[test]
fn only_the_designated_gateway_sent_status_blocks_a_cancellation_request() {
    for designated_producer in [false, true] {
        let fixture = Fixture::new();
        let gateway = DeviceId::new();
        let producer = if designated_producer {
            gateway
        } else {
            DeviceId::new()
        };
        let queued = fixture
            .client
            .queue_send(
                OutgoingSms {
                    conversation_id: ConversationId::new(),
                    recipients: vec!["+15555550100".into()],
                    body: "executor-bound cancellation eligibility".into(),
                },
                GatewayRoute::Carrier {
                    gateway_device_id: gateway,
                    subscription_id: "sim-1".into(),
                },
            )
            .unwrap();
        let capability = fixture.event(
            gateway,
            1,
            PrivatePayload::SendCancellationCapability { supported: true },
        );
        let report = fixture.apply(&capability, 1);
        assert_eq!(report.applied, 1);
        assert_eq!(report.quarantined, 0);

        let status = fixture.event(
            producer,
            2,
            PrivatePayload::SendStatus {
                command_id: queued.command_id,
                state: SendState::Sent,
                at_ms: Some(1_725_363_180_000),
                reason: None,
            },
        );
        let report = fixture.apply(&status, 2);
        assert_eq!(report.applied, 1);
        assert_eq!(report.quarantined, 0);
        {
            let store = fixture.client.lock().unwrap();
            let status_exists: bool = store
                .conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM send_status WHERE command_id=? AND producer_device_id=? AND state='sent')",
                    params![queued.command_id.to_string(), producer.to_string()],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(
                status_exists,
                "the producer filter must see a stored Sent row"
            );
        }

        let view = fixture
            .client
            .message_send_cancellation(queued.message_id)
            .unwrap();
        assert_eq!(view.state, SendCancellationState::None);
        assert_eq!(view.can_request, !designated_producer);
        assert!(!view.requires_gateway_update);
        let outbox_before = fixture.client.pending_outbox().unwrap().len();
        let requested = fixture
            .client
            .request_send_cancellation(queued.message_id)
            .unwrap();
        let expected_state = if designated_producer {
            SendCancellationState::TooLate
        } else {
            SendCancellationState::Requested
        };
        assert_eq!(requested.state, expected_state);
        assert!(!requested.can_request);
        assert_eq!(
            fixture.client.pending_outbox().unwrap().len(),
            outbox_before + usize::from(!designated_producer)
        );
        let store = fixture.client.lock().unwrap();
        let persisted_state: Option<String> = store
            .conn
            .query_row(
                "SELECT state FROM send_cancellations WHERE message_id=?",
                [queued.message_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(
            persisted_state,
            (!designated_producer).then(|| "requested".to_string())
        );
    }
}
