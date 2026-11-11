use super::*;

use peppy_crypto::{create_vault_check_header, derive_purpose_key, derive_root_key, encrypt};

const PASSPHRASE: &str = "correct horse battery staple";

fn client_config(dir: &tempfile::TempDir, name: &str, vault_id: VaultId) -> ClientConfig {
    ClientConfig {
        database_path: dir.path().join(format!("{name}.db")),
        vault_id,
        device_id: DeviceId::new(),
    }
}

fn unlocked(config: ClientConfig, profile: &KeyProfile, header: &VaultCheckHeader) -> Client {
    let client = Client::open(config, DatabaseKey::new(&[29; 32]).unwrap()).unwrap();
    client.unlock(profile, header, PASSPHRASE).unwrap();
    client.set_server_compaction_state(true, true).unwrap();
    client
}

fn deliver(source: &Client, targets: &[&Client], cursor: &mut u64) {
    let envelopes = source.pending_outbox().unwrap();
    for envelope in &envelopes {
        source.ack_outbox(envelope.envelope_id).unwrap();
        for target in targets {
            target.ingest(envelope, Cursor(*cursor)).unwrap();
        }
        *cursor += 1;
    }
    for target in targets {
        target.apply_pending(100).unwrap();
    }
}

#[test]
fn decoder_revision_recovers_applied_cancellation_events_without_reopening_commands() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault_id = VaultId::new();
    let profile = KeyProfile::new(vault_id.0, 1).unwrap();
    let root = derive_root_key(PASSPHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    let sender_config = client_config(&dir, "sender", vault_id);
    let gateway_config = client_config(&dir, "gateway", vault_id);
    let target_config = client_config(&dir, "target", vault_id);
    let sender = unlocked(sender_config, &profile, &header);
    let gateway = unlocked(gateway_config.clone(), &profile, &header);
    let target = unlocked(target_config.clone(), &profile, &header);
    let conversation_id = ConversationId::new();
    let queued = sender
        .queue_send(
            OutgoingSms {
                conversation_id,
                recipients: vec!["+15555550100".into()],
                body: "recover cancellation".into(),
            },
            GatewayRoute::Carrier {
                gateway_device_id: gateway_config.device_id,
                subscription_id: "sim-1".into(),
            },
        )
        .unwrap();
    let mut cursor = 1;
    deliver(&sender, &[&sender, &gateway, &target], &mut cursor);
    gateway.publish_send_cancellation_capability(true).unwrap();
    deliver(&gateway, &[&sender, &gateway, &target], &mut cursor);
    assert_eq!(
        sender
            .request_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::Requested
    );
    deliver(&sender, &[&gateway, &target], &mut cursor);
    assert_eq!(
        gateway
            .lock()
            .unwrap()
            .conn
            .query_row(
                "SELECT state FROM send_cancellations WHERE command_id=?",
                [queued.command_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "requested",
    );
    assert!(gateway.process_pending_send_cancellations().unwrap());
    deliver(&gateway, &[&target], &mut cursor);
    let cache = target.export_native_key_cache(1).unwrap();
    {
        let store = target.lock().unwrap();
        store
            .conn
            .execute("DELETE FROM send_cancellations", [])
            .unwrap();
        store
            .conn
            .execute("DELETE FROM send_cancellation_capabilities", [])
            .unwrap();
        set_meta(&store.conn, "decoder_revision:1", "11").unwrap();
        assert_eq!(
            store
                .conn
                .query_row("SELECT COUNT(*) FROM attempts", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    drop(target);

    let recovered = Client::open(target_config, DatabaseKey::new(&[29; 32]).unwrap()).unwrap();
    recovered.import_native_key_cache(&cache).unwrap();
    assert_eq!(
        recovered
            .message_send_cancellation(queued.message_id)
            .unwrap()
            .state,
        SendCancellationState::Confirmed,
    );
    assert!(recovered.messages(conversation_id).unwrap().is_empty());
    let store = recovered.lock().unwrap();
    assert_eq!(
        store
            .conn
            .query_row("SELECT COUNT(*) FROM attempts", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        get_meta(&store.conn, "decoder_revision:1")
            .unwrap()
            .as_deref(),
        Some("12")
    );
}

#[test]
fn decoder_recovery_quarantines_legacy_overflow_capability_without_failing_unlock() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault_id = VaultId::new();
    let profile = KeyProfile::new(vault_id.0, 1).unwrap();
    let root = derive_root_key(PASSPHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    let config = client_config(&dir, "overflow", vault_id);
    let client = unlocked(config.clone(), &profile, &header);
    let producer = DeviceId::new();
    let sequence = i64::MAX as u64 + 1;
    let mut envelope = Envelope {
        protocol_version: PROTOCOL_VERSION,
        envelope_id: EnvelopeId::new(),
        command_id: None,
        vault_id,
        producer_device_id: producer,
        producer_sequence: SourceSequence(sequence),
        key_epoch: 1,
        crypto_suite: profile.crypto_suite,
        profile_fingerprint: profile.fingerprint().unwrap(),
        purpose: EnvelopePurpose::Event,
        route: None,
        compaction: None,
        origin_tag: None,
        ciphertext: Vec::new(),
    };
    let event_key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
    let payload =
        serde_json::to_vec(&PrivatePayload::SendCancellationCapability { supported: true })
            .unwrap();
    envelope.ciphertext =
        seal_frame(&encrypt(&event_key, &envelope.aad_bytes().unwrap(), &payload).unwrap());
    client.ingest(&envelope, Cursor(1)).unwrap();
    client.apply_pending(10).unwrap();
    let cache = client.export_native_key_cache(1).unwrap();
    {
        let store = client.lock().unwrap();
        store
            .conn
            .execute(
                "UPDATE journal SET status='applied',reason=NULL WHERE cursor=1",
                [],
            )
            .unwrap();
        set_meta(&store.conn, "decoder_revision:1", "11").unwrap();
    }
    drop(client);

    let recovered = Client::open(config, DatabaseKey::new(&[29; 32]).unwrap()).unwrap();
    recovered.import_native_key_cache(&cache).unwrap();
    let store = recovered.lock().unwrap();
    let row: (String, Option<String>) = store
        .conn
        .query_row(
            "SELECT status,reason FROM journal WHERE cursor=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(row, ("quarantined".into(), Some("invalid_payload".into())));
    assert_eq!(
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM send_cancellation_capabilities",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
