use super::*;

use peppy_crypto::{create_vault_check_header, derive_purpose_key, derive_root_key, encrypt};

const PASSPHRASE: &str = "correct horse battery staple";
const PIN_COUNT: u64 = 7;
const CURSOR_KEY: &str = "decoder_recovery_cursor:1";
const OLD_REVISION: &str = "11";

/// A vault whose database holds pins that an older decoder left applied, with the marker
/// reset so that a full decoder recovery is pending. The cache lets each run reinstall the
/// same keys without a passphrase.
struct Seed {
    dir: tempfile::TempDir,
    config: ClientConfig,
    profile: KeyProfile,
    header: VaultCheckHeader,
    cache: NativeKeyCache,
}

#[derive(Debug, PartialEq, Eq)]
struct Dump {
    journal: Vec<(i64, String, Option<String>, bool)>,
    pins: Vec<(String, bool, i64)>,
    marker: Option<String>,
}

fn open(config: &ClientConfig) -> Client {
    Client::open(config.clone(), DatabaseKey::new(&[29; 32]).unwrap()).unwrap()
}

fn pin_envelope(
    profile: &KeyProfile,
    event_key: &PurposeKey,
    producer: DeviceId,
    sequence: u64,
    conversation_id: ConversationId,
) -> Envelope {
    let payload = PrivatePayload::ConversationPin {
        conversation_id,
        pinned: sequence.is_multiple_of(2),
        logical_revision: sequence,
        writer_device_id: producer.to_string(),
    };
    event_envelope(profile, event_key, producer, sequence, &payload)
}

fn event_envelope(
    profile: &KeyProfile,
    event_key: &PurposeKey,
    producer: DeviceId,
    sequence: u64,
    payload: &PrivatePayload,
) -> Envelope {
    let mut envelope = Envelope {
        protocol_version: PROTOCOL_VERSION,
        envelope_id: EnvelopeId::new(),
        command_id: None,
        vault_id: VaultId(profile.vault_id),
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
    let plain = serde_json::to_vec(payload).unwrap();
    envelope.ciphertext =
        seal_frame(&encrypt(event_key, &envelope.aad_bytes().unwrap(), &plain).unwrap());
    envelope
}

fn event_key_for(seed: &Seed) -> PurposeKey {
    let root = derive_root_key(PASSPHRASE, &seed.profile).unwrap();
    derive_purpose_key(&root, &seed.profile, KeyPurpose::Event).unwrap()
}

fn unsealed_outbox(client: &Client) -> i64 {
    let store = client.lock().unwrap();
    store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM outbox WHERE state='unsealed'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn seed() -> Seed {
    let dir = tempfile::TempDir::new().unwrap();
    let vault_id = VaultId::new();
    let profile = KeyProfile::new(vault_id.0, 1).unwrap();
    let root = derive_root_key(PASSPHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    let event_key = derive_purpose_key(&root, &profile, KeyPurpose::Event).unwrap();
    let config = ClientConfig {
        database_path: dir.path().join("seed.db"),
        vault_id,
        device_id: DeviceId::new(),
    };
    let client = open(&config);
    client.unlock(&profile, &header, PASSPHRASE).unwrap();
    let producer = DeviceId::new();
    for sequence in 1..=PIN_COUNT {
        let envelope = pin_envelope(
            &profile,
            &event_key,
            producer,
            sequence,
            ConversationId::new(),
        );
        client.ingest(&envelope, Cursor(sequence)).unwrap();
    }
    client.apply_pending(usize::MAX).unwrap();
    {
        let store = client.lock().unwrap();
        set_meta(&store.conn, "decoder_revision:1", OLD_REVISION).unwrap();
        store
            .conn
            .execute("DELETE FROM conversation_pins", [])
            .unwrap();
    }
    let cache = client.export_native_key_cache(1).unwrap();
    drop(client);
    Seed {
        dir,
        config,
        profile,
        header,
        cache,
    }
}

/// Copies the seed database so each run starts from identical bytes.
fn copy_of(seed: &Seed, name: &str) -> ClientConfig {
    let path = seed.dir.path().join(format!("{name}.db"));
    std::fs::copy(&seed.config.database_path, &path).unwrap();
    ClientConfig {
        database_path: path,
        ..seed.config.clone()
    }
}

fn dump(client: &Client) -> Dump {
    let store = client.lock().unwrap();
    let conn = &store.conn;
    let journal = conn
        .prepare("SELECT cursor,status,reason,historical FROM journal ORDER BY cursor")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get::<_, bool>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let pins = conn
        .prepare("SELECT conversation_id,pinned,logical_revision FROM conversation_pins ORDER BY conversation_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    Dump {
        journal,
        pins,
        marker: get_meta(conn, "decoder_revision:1").unwrap(),
    }
}

fn cursor_record(client: &Client) -> Option<serde_json::Value> {
    let store = client.lock().unwrap();
    get_meta(&store.conn, CURSOR_KEY)
        .unwrap()
        .map(|value| serde_json::from_str(&value).unwrap())
}

fn marker(client: &Client) -> Option<String> {
    let store = client.lock().unwrap();
    get_meta(&store.conn, "decoder_revision:1").unwrap()
}

fn pins_restored(client: &Client) -> usize {
    dump(client).pins.len()
}

/// Runs the inline reference: a native cache import with the default mode completes recovery
/// inside the call.
fn inline_end_state(seed: &Seed) -> Dump {
    let client = open(&copy_of(seed, "inline"));
    client.import_native_key_cache(&seed.cache).unwrap();
    assert_eq!(client.decoder_recovery_pending().unwrap(), None);
    let state = dump(&client);
    assert_eq!(state.marker.as_deref(), Some(DECODER_REVISION));
    assert_eq!(state.pins.len(), PIN_COUNT as usize);
    state
}

fn deferred_import(client: &Client, cache: &NativeKeyCache) {
    client
        .set_decoder_recovery_mode(DecoderRecoveryMode::Deferred)
        .unwrap();
    client.import_native_key_cache(cache).unwrap();
}

/// Runs pages until recovery reports nothing pending, returning every progress value.
fn run_to_completion(client: &Client, max_rows: u32) -> Vec<DecoderRecoveryProgress> {
    let mut progress = Vec::new();
    while let Some(step) = client.recover_decoder_page(max_rows).unwrap() {
        progress.push(step);
    }
    progress
}

#[test]
fn paged_recovery_matches_inline_end_state() {
    let seed = seed();
    let inline = inline_end_state(&seed);

    let client = open(&copy_of(&seed, "paged"));
    deferred_import(&client, &seed.cache);
    assert_eq!(pins_restored(&client), 0);
    let progress = run_to_completion(&client, 2);

    assert_eq!(dump(&client), inline);
    assert_eq!(client.decoder_recovery_pending().unwrap(), None);
    let mut previous = 0;
    for step in &progress {
        assert!(step.completed <= step.total);
        assert!(step.completed >= previous);
        previous = step.completed;
    }
    assert_eq!(progress.last().unwrap().completed, PIN_COUNT);
    assert_eq!(progress.last().unwrap().total, PIN_COUNT);
}

#[test]
fn marker_and_cursor_move_only_with_the_final_page() {
    let seed = seed();
    let client = open(&copy_of(&seed, "markers"));
    deferred_import(&client, &seed.cache);
    let mut calls = 0;
    loop {
        let step = client.recover_decoder_page(2).unwrap();
        calls += 1;
        assert!(
            step.is_some(),
            "recovery must report progress until finished"
        );
        if marker(&client).as_deref() == Some(DECODER_REVISION) {
            assert_eq!(cursor_record(&client), None, "cursor deleted with marker");
            break;
        }
        assert_eq!(marker(&client).as_deref(), Some(OLD_REVISION));
        assert!(
            cursor_record(&client).is_some(),
            "page wrote a resume record"
        );
        assert!(calls < 100, "recovery must terminate");
    }
    // Four data pages of two rows for seven pins, then the page that finds no more rows.
    assert_eq!(calls, 5);
    assert_eq!(client.recover_decoder_page(2).unwrap(), None);
}

#[test]
fn crash_and_resume_converge_at_every_page_boundary() {
    let seed = seed();
    let inline = inline_end_state(&seed);
    for boundary in 0..=5 {
        let config = copy_of(&seed, &format!("crash-{boundary}"));
        {
            let client = open(&config);
            deferred_import(&client, &seed.cache);
            for _ in 0..boundary {
                if client.recover_decoder_page(2).unwrap().is_none() {
                    break;
                }
            }
        }
        let resumed = open(&config);
        deferred_import(&resumed, &seed.cache);
        run_to_completion(&resumed, 2);
        assert_eq!(dump(&resumed), inline, "boundary {boundary}");
    }
}

#[test]
fn changed_source_marker_restarts_recovery() {
    let seed = seed();
    let client = open(&copy_of(&seed, "source"));
    deferred_import(&client, &seed.cache);
    client.recover_decoder_page(2).unwrap();
    assert_eq!(cursor_record(&client).unwrap()["source"], OLD_REVISION);
    {
        let store = client.lock().unwrap();
        set_meta(&store.conn, "decoder_revision:1", "10").unwrap();
    }
    client.recover_decoder_page(3).unwrap();
    let record = cursor_record(&client).unwrap();
    assert_eq!(record["source"], "10");
    assert_eq!(record["completed"], 3);
    assert_eq!(record["total"], PIN_COUNT);
}

#[test]
fn changed_scope_restarts_recovery_from_the_beginning() {
    let seed = seed();
    let client = open(&copy_of(&seed, "scope"));
    deferred_import(&client, &seed.cache);
    {
        let store = client.lock().unwrap();
        set_meta(
            &store.conn,
            CURSOR_KEY,
            r#"{"target":"15","source":"11","scope":"quarantined","last_cursor":4,"completed":4,"total":7}"#,
        )
        .unwrap();
    }
    client.recover_decoder_page(2).unwrap();
    let record = cursor_record(&client).unwrap();
    assert_eq!(record["scope"], "applied");
    assert_eq!(record["last_cursor"], 2);
    assert_eq!(record["completed"], 2);
}

#[test]
fn progress_is_clamped_to_the_total() {
    let seed = seed();
    let client = open(&copy_of(&seed, "clamp"));
    deferred_import(&client, &seed.cache);
    {
        let store = client.lock().unwrap();
        set_meta(
            &store.conn,
            CURSOR_KEY,
            r#"{"target":"15","source":"11","scope":"applied","last_cursor":0,"completed":99,"total":7}"#,
        )
        .unwrap();
    }
    let pending = client.decoder_recovery_pending().unwrap().unwrap();
    assert!(pending.completed <= pending.total);
    assert_eq!(pending.total, PIN_COUNT);
}

#[test]
fn zero_row_epoch_writes_its_marker_on_the_first_page() {
    let seed = seed();
    let client = open(&copy_of(&seed, "zero"));
    {
        let store = client.lock().unwrap();
        store.conn.execute("DELETE FROM journal", []).unwrap();
    }
    deferred_import(&client, &seed.cache);
    assert!(client.decoder_recovery_pending().unwrap().is_some());
    let step = client.recover_decoder_page(2).unwrap().unwrap();
    assert_eq!((step.completed, step.total), (0, 0));
    assert_eq!(marker(&client).as_deref(), Some(DECODER_REVISION));
    assert_eq!(client.decoder_recovery_pending().unwrap(), None);
}

#[test]
fn deferred_native_cache_import_installs_keys_without_recovering() {
    let seed = seed();
    let client = open(&copy_of(&seed, "deferred-cache"));
    deferred_import(&client, &seed.cache);
    assert!(client.lock().unwrap().keys.contains_key(&1));
    assert_eq!(marker(&client).as_deref(), Some(OLD_REVISION));
    assert_eq!(pins_restored(&client), 0);
    assert!(client.decoder_recovery_pending().unwrap().is_some());
    run_to_completion(&client, 3);
    assert_eq!(pins_restored(&client), PIN_COUNT as usize);
    assert_eq!(client.decoder_recovery_pending().unwrap(), None);
}

#[test]
fn deferred_passphrase_unlock_installs_keys_without_recovering() {
    let seed = seed();
    let client = open(&copy_of(&seed, "deferred-unlock"));
    client
        .set_decoder_recovery_mode(DecoderRecoveryMode::Deferred)
        .unwrap();
    client
        .unlock(&seed.profile, &seed.header, PASSPHRASE)
        .unwrap();
    assert!(client.lock().unwrap().keys.contains_key(&1));
    assert_eq!(marker(&client).as_deref(), Some(OLD_REVISION));
    assert_eq!(pins_restored(&client), 0);
    assert!(client.decoder_recovery_pending().unwrap().is_some());
    run_to_completion(&client, 4);
    assert_eq!(pins_restored(&client), PIN_COUNT as usize);
    assert_eq!(client.decoder_recovery_pending().unwrap(), None);
}

#[test]
fn inline_installs_complete_recovery_within_the_call() {
    let seed = seed();
    let client = open(&copy_of(&seed, "inline-unlock"));
    client
        .unlock(&seed.profile, &seed.header, PASSPHRASE)
        .unwrap();
    assert_eq!(marker(&client).as_deref(), Some(DECODER_REVISION));
    assert_eq!(client.decoder_recovery_pending().unwrap(), None);
    assert_eq!(client.recover_decoder_page(2).unwrap(), None);
}

fn fingerprint(client: &Client) -> (Dump, Vec<(String, String)>, i64, i64) {
    let (metadata, outbox, messages) = {
        let store = client.lock().unwrap();
        let metadata = store
            .conn
            .prepare("SELECT k,v FROM metadata ORDER BY k")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<(String, String)>, _>>()
            .unwrap();
        let outbox: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))
            .unwrap();
        let messages: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        (metadata, outbox, messages)
    };
    (dump(client), metadata, outbox, messages)
}

type FencedCall<'a> = (&'static str, Box<dyn Fn(&Client) -> Result<(), Error> + 'a>);

/// Every entry point the recovery fence covers. Each call would otherwise fail validation or
/// succeed, so a refusal can only come from the fence.
fn fenced_calls(envelope: &Envelope) -> Vec<FencedCall<'_>> {
    vec![
        (
            "ingest",
            Box::new(|c: &Client| c.ingest(envelope, Cursor(99)).map(drop)),
        ),
        (
            "ingest_raw",
            Box::new(|c: &Client| c.ingest_raw(b"{}", Cursor(99)).map(drop)),
        ),
        (
            "ingest_raw_batch",
            Box::new(|c: &Client| c.ingest_raw_batch(&[]).map(drop)),
        ),
        (
            "apply_pending",
            Box::new(|c: &Client| c.apply_pending(10).map(drop)),
        ),
        (
            "process_pending_send_cancellations",
            Box::new(|c: &Client| c.process_pending_send_cancellations().map(drop)),
        ),
        (
            "begin_send_attempt",
            Box::new(|c: &Client| c.begin_send_attempt(CommandId::new()).map(drop)),
        ),
        (
            "begin_send_attempt_at",
            Box::new(|c: &Client| c.begin_send_attempt_at(CommandId::new(), 0).map(drop)),
        ),
        (
            "begin_automatic_send_attempt",
            Box::new(|c: &Client| {
                c.begin_automatic_send_attempt(CommandId::new(), None, 0)
                    .map(drop)
            }),
        ),
        (
            "begin_snapshot",
            Box::new(|c: &Client| {
                c.begin_snapshot(Cursor(1), 0, SnapshotPurpose::Restore)
                    .map(drop)
            }),
        ),
        (
            "begin_snapshot_with_compaction",
            Box::new(|c: &Client| {
                c.begin_snapshot_with_compaction(Cursor(1), 0, SnapshotPurpose::Restore, None)
                    .map(drop)
            }),
        ),
        (
            "begin_snapshot_with_manifest",
            Box::new(|c: &Client| c.begin_snapshot_with_manifest(Cursor(1), 0, None).map(drop)),
        ),
        (
            "append_snapshot_page",
            Box::new(|c: &Client| c.append_snapshot_page(1, &[]).map(drop)),
        ),
        (
            "append_snapshot_raw_page",
            Box::new(|c: &Client| c.append_snapshot_raw_page(1, &[]).map(drop)),
        ),
        (
            "append_snapshot_manifest",
            Box::new(|c: &Client| c.append_snapshot_manifest(1, &[]).map(drop)),
        ),
        (
            "finish_snapshot",
            Box::new(|c: &Client| c.finish_snapshot(1).map(drop)),
        ),
        (
            "begin_contact_scan",
            Box::new(|c: &Client| c.begin_contact_scan("{}").map(drop)),
        ),
        (
            "finish_contact_scan",
            Box::new(|c: &Client| c.finish_contact_scan("{}").map(drop)),
        ),
        (
            "next_contact_apply_permit",
            Box::new(|c: &Client| c.next_contact_apply_permit("{}").map(drop)),
        ),
        (
            "begin_message_history_capture",
            Box::new(|c: &Client| {
                c.begin_message_history_capture("generation", "sms")
                    .map(drop)
            }),
        ),
        (
            "finish_message_history_capture",
            Box::new(|c: &Client| c.finish_message_history_capture("{}").map(drop)),
        ),
        (
            "drain_message_history_capture_results",
            Box::new(|c: &Client| {
                c.drain_message_history_capture_results("generation")
                    .map(drop)
            }),
        ),
        (
            "record_message_history_progress",
            Box::new(|c: &Client| c.record_message_history_progress("{}").map(drop)),
        ),
        (
            "begin_reaction_attempt",
            Box::new(|c: &Client| c.begin_reaction_attempt(ReactionRequestId::new()).map(drop)),
        ),
        (
            "set_conversation_folder",
            Box::new(|c: &Client| {
                c.set_conversation_folder(ConversationId::new(), ConversationFolder::Archived)
            }),
        ),
        (
            "set_conversation_pinned",
            Box::new(|c: &Client| c.set_conversation_pinned(ConversationId::new(), true)),
        ),
        (
            "delete_contact_book",
            Box::new(|c: &Client| {
                c.delete_contact_book(
                    r#"{"book_id":"book","revoked_device_ids":[],"sync_current":true}"#,
                )
                .map(drop)
            }),
        ),
        (
            "set_conversation_tone",
            Box::new(|c: &Client| c.set_conversation_tone("seed", None)),
        ),
        (
            "set_notification_preferences",
            Box::new(|c: &Client| {
                c.set_notification_preferences(NotificationPreferences::default())
            }),
        ),
        (
            "adopt_legacy_notification_preferences",
            Box::new(|c: &Client| {
                c.adopt_legacy_notification_preferences(NotificationPreferences {
                    message_banners: false,
                    ..NotificationPreferences::default()
                })
                .map(drop)
            }),
        ),
        (
            "publish_sender_blocklist",
            Box::new(|c: &Client| c.publish_sender_blocklist(None, false).map(drop)),
        ),
        (
            "request_sender_unblock",
            Box::new(|c: &Client| c.request_sender_unblock(ConversationId::new())),
        ),
        (
            "begin_sender_unblock",
            Box::new(|c: &Client| {
                c.begin_sender_unblock(&uuid::Uuid::new_v4().to_string())
                    .map(drop)
            }),
        ),
        (
            "reconcile_provider_archive",
            Box::new(|c: &Client| c.reconcile_provider_archive(vec![]).map(drop)),
        ),
        (
            "set_mms_own_address",
            Box::new(|c: &Client| c.set_mms_own_address("sim-1", "+15555550150")),
        ),
    ]
}

#[test]
fn fenced_entry_points_refuse_without_writing_while_recovery_is_pending() {
    let seed = seed();
    let client = open(&copy_of(&seed, "fence"));
    deferred_import(&client, &seed.cache);
    assert!(client.decoder_recovery_pending().unwrap().is_some());

    let root = derive_root_key(PASSPHRASE, &seed.profile).unwrap();
    let event_key = derive_purpose_key(&root, &seed.profile, KeyPurpose::Event).unwrap();
    let envelope = pin_envelope(
        &seed.profile,
        &event_key,
        DeviceId::new(),
        99,
        ConversationId::new(),
    );
    let calls = fenced_calls(&envelope);
    let before = fingerprint(&client);
    for (name, call) in &calls {
        assert_eq!(call(&client), Err(Error::RecoveryPending), "{name}");
        assert_eq!(fingerprint(&client), before, "{name} wrote while pending");
    }

    // Reads and an ordinary local mutation keep working while recovery is pending.
    assert!(client.pending_outbox().is_ok());
    assert!(client.messages(ConversationId::new()).is_ok());
    let queued = client.queue_send(
        OutgoingSms {
            conversation_id: ConversationId::new(),
            recipients: vec!["+15555550100".into()],
            body: "queued while updating".into(),
        },
        GatewayRoute::Carrier {
            gateway_device_id: DeviceId::new(),
            subscription_id: "sim-1".into(),
        },
    );
    assert!(queued.is_ok(), "{queued:?}");
    assert_eq!(fingerprint(&client).0.journal, before.0.journal);
    assert!(client.decoder_recovery_pending().unwrap().is_some());

    // Once recovery completes, the same entry points are no longer refused.
    run_to_completion(&client, 3);
    for (name, call) in &calls {
        assert_ne!(call(&client), Err(Error::RecoveryPending), "{name}");
    }
}

fn permit_rows(client: &Client) -> (i64, String) {
    let store = client.lock().unwrap();
    let attempts: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM sender_unblock_attempts", [], |row| {
            row.get(0)
        })
        .unwrap();
    let status: String = store
        .conn
        .query_row("SELECT status FROM sender_unblock_requests", [], |row| {
            row.get(0)
        })
        .unwrap();
    (attempts, status)
}

#[test]
fn unblock_permit_is_refused_while_recovery_is_pending_and_writes_nothing() {
    let seed = seed();
    let config = copy_of(&seed, "unblock");
    let request_id = uuid::Uuid::new_v4();
    {
        let client = open(&config);
        let store = client.lock().unwrap();
        let device = config.device_id.to_string();
        store
            .conn
            .execute(
                "INSERT INTO sender_blocklists(producer_device_id,numbers,can_unblock,logical_revision,writer_device_id,source_sequence) VALUES(?,'[\"+15555550100\"]',1,1,?,1)",
                params![device, device],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO sender_unblock_requests(request_id,conversation_id,target_device_id,numbers,status,historical,created_at_ms,key_epoch) VALUES(?,?,?,'[\"+15555550100\"]','pending',0,?,1)",
                params![
                    request_id.to_string(),
                    ConversationId::new().to_string(),
                    device,
                    notifications::now_ms()
                ],
            )
            .unwrap();
    }
    let client = open(&config);
    deferred_import(&client, &seed.cache);
    let before = (fingerprint(&client), permit_rows(&client));
    assert_eq!(
        client.begin_sender_unblock(&request_id.to_string()),
        Err(Error::RecoveryPending)
    );
    assert_eq!((fingerprint(&client), permit_rows(&client)), before);

    run_to_completion(&client, 3);
    assert_eq!(
        client.begin_sender_unblock(&request_id.to_string()),
        Ok(true)
    );
    assert_eq!(permit_rows(&client).0, 1);
}

fn observed_threads(client: &Client) -> i64 {
    let store = client.lock().unwrap();
    store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM provider_archive_observed",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn provider_archive_reconciliation_is_refused_while_recovery_is_pending() {
    let seed = seed();
    let config = copy_of(&seed, "archive");
    let conversation = ConversationId::new();
    {
        let client = open(&config);
        let store = client.lock().unwrap();
        let device = config.device_id.to_string();
        store
            .conn
            .execute(
                "INSERT INTO sms_acquisitions(acquisition_id,source_generation,subscription_id,provider_message_id,source_row_fingerprint,direction,conversation_id,input,state,message_id,source_sequence,local_order) VALUES('a','gen','sim','p','f','incoming',?,'{}','pending','m',1,1)",
                [conversation.to_string()],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO mms_thread_conversations(source_device_id,source_generation,subscription_id,provider_thread_id,conversation_id) VALUES(?,'gen','sim','thread',?)",
                params![device, conversation.to_string()],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO messages(id,conversation_id,effective_conversation_id,source_device_id,source_sequence,payload,seen,sort_at_ms) VALUES('m',?,?,?,1,'{}',0,42)",
                params![conversation.to_string(), conversation.to_string(), device],
            )
            .unwrap();
    }
    let client = open(&config);
    deferred_import(&client, &seed.cache);
    assert_eq!(
        client.reconcile_provider_archive(vec!["thread".into()]),
        Err(Error::RecoveryPending)
    );
    assert_eq!(observed_threads(&client), 0);

    run_to_completion(&client, 3);
    assert_eq!(
        client
            .reconcile_provider_archive(vec!["thread".into()])
            .unwrap(),
        1
    );
    assert_eq!(observed_threads(&client), 1);
}

#[test]
fn own_device_mms_address_recovered_on_a_later_page_is_not_overwritten_by_a_local_edit() {
    let seed = seed();
    let config = copy_of(&seed, "own-address");
    let historical = event_envelope(
        &seed.profile,
        &event_key_for(&seed),
        config.device_id,
        200,
        &PrivatePayload::MmsOwnAddress {
            source_device_id: config.device_id,
            subscription_id: "sim-1".into(),
            address: "+15555550199".into(),
            revision: 10,
        },
    );
    {
        let client = open(&config);
        let store = client.lock().unwrap();
        store
            .conn
            .execute(
                "INSERT INTO journal(cursor,key_epoch,wire,status,reason,historical) VALUES(200,1,?,'applied',NULL,0)",
                [typed_wire(&historical).unwrap()],
            )
            .unwrap();
    }
    let client = open(&config);
    deferred_import(&client, &seed.cache);
    // Pins fill the first pages; the own-address revision 10 is recovered on a later page.
    client.recover_decoder_page(2).unwrap();
    assert_eq!(client.mms_own_address("sim-1").unwrap(), None);
    assert_eq!(
        client.set_mms_own_address("sim-1", "+15555550150"),
        Err(Error::RecoveryPending)
    );

    run_to_completion(&client, 2);
    assert_eq!(
        client.mms_own_address("sim-1").unwrap().as_deref(),
        Some("+15555550199")
    );
}

#[test]
fn deferred_reinstall_with_current_markers_seals_locked_work() {
    let seed = seed();
    let config = copy_of(&seed, "reseal");
    {
        let client = open(&config);
        client.import_native_key_cache(&seed.cache).unwrap();
    }
    let client = open(&config);
    client
        .queue_send(
            OutgoingSms {
                conversation_id: ConversationId::new(),
                recipients: vec!["+15555550100".into()],
                body: "written while locked".into(),
            },
            GatewayRoute::Carrier {
                gateway_device_id: DeviceId::new(),
                subscription_id: "sim-1".into(),
            },
        )
        .unwrap();
    assert_eq!(unsealed_outbox(&client), 1);
    deferred_import(&client, &seed.cache);
    assert_eq!(client.decoder_recovery_pending().unwrap(), None);
    assert_eq!(unsealed_outbox(&client), 0);
}

#[test]
fn failed_post_recovery_seal_is_retried_by_the_next_call() {
    let seed = seed();
    let client = open(&copy_of(&seed, "seal-retry"));
    client
        .queue_send(
            OutgoingSms {
                conversation_id: ConversationId::new(),
                recipients: vec!["+15555550100".into()],
                body: "written while locked".into(),
            },
            GatewayRoute::Carrier {
                gateway_device_id: DeviceId::new(),
                subscription_id: "sim-1".into(),
            },
        )
        .unwrap();
    assert_eq!(unsealed_outbox(&client), 1);
    deferred_import(&client, &seed.cache);
    for _ in 0..4 {
        assert!(client.recover_decoder_page(2).unwrap().is_some());
    }
    client.lock().unwrap().conn.execute_batch(
        "CREATE TEMP TRIGGER fail_seal BEFORE UPDATE OF wire ON outbox BEGIN SELECT RAISE(ABORT, 'seal failure'); END;",
    ).unwrap();
    assert!(client.recover_decoder_page(2).is_err());
    assert_eq!(marker(&client).as_deref(), Some(DECODER_REVISION));
    assert!(
        client.decoder_recovery_pending().unwrap().is_some(),
        "the seal obligation survives the failed finishing call"
    );
    assert_eq!(unsealed_outbox(&client), 1);
    client
        .lock()
        .unwrap()
        .conn
        .execute_batch("DROP TRIGGER fail_seal")
        .unwrap();
    assert_eq!(client.recover_decoder_page(2).unwrap(), None);
    assert_eq!(client.decoder_recovery_pending().unwrap(), None);
    assert_eq!(unsealed_outbox(&client), 0);
    let envelopes = client.pending_outbox_batch(10).unwrap();
    assert_eq!(envelopes.len(), 1);
    assert!(!envelopes[0].ciphertext.is_empty());
}

#[test]
fn pending_epoch_totals_include_later_epochs_while_an_earlier_epoch_pages() {
    let seed = seed();
    let client = open(&copy_of(&seed, "two-epochs"));
    {
        let store = client.lock().unwrap();
        for cursor in 300..303 {
            store
                .conn
                .execute(
                    "INSERT INTO journal(cursor,key_epoch,wire,status,reason,historical) VALUES(?,2,?,'applied',NULL,0)",
                    params![cursor, b"not a wire".to_vec()],
                )
                .unwrap();
        }
    }
    deferred_import(&client, &seed.cache);
    let profile2 = KeyProfile::new(seed.profile.vault_id, 2).unwrap();
    let root2 = derive_root_key(PASSPHRASE, &profile2).unwrap();
    let header2 = create_vault_check_header(&root2, profile2.clone()).unwrap();
    client.unlock(&profile2, &header2, PASSPHRASE).unwrap();

    let pending = client.decoder_recovery_pending().unwrap().unwrap();
    assert_eq!(pending.epoch, 1);
    assert_eq!(pending.total, PIN_COUNT + 3);
    run_to_completion(&client, 2);
    assert_eq!(client.decoder_recovery_pending().unwrap(), None);
}

#[test]
fn stranded_supersede_scan_is_visible_to_work_status_until_the_scan_finishes() {
    let seed = seed();
    let client = open(&copy_of(&seed, "stranded"));
    client.import_native_key_cache(&seed.cache).unwrap();
    {
        let store = client.lock().unwrap();
        store
            .conn
            .execute(
                "INSERT INTO projection_generations(generation,high_water,state) VALUES(1,2000,'promoted')",
                [],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO journal(cursor,key_epoch,wire,status,reason,historical) VALUES(1000,1,?,'pending','snapshot_rcs_isolated',0)",
                [b"not a wire".to_vec()],
            )
            .unwrap();
    }
    assert!(client.sync_work_status().unwrap().pending_apply);
    client.apply_pending(10).unwrap();
    assert!(!client.sync_work_status().unwrap().pending_apply);
}

#[test]
fn reinstall_after_the_eligible_set_changed_refreshes_total_and_keeps_the_resume_position() {
    let seed = seed();
    let add_rows = |conn: &Connection| {
        for cursor in [50_i64, 51] {
            conn.execute(
                "INSERT INTO journal(cursor,key_epoch,wire,status,reason,historical) VALUES(?,1,?,'applied',NULL,0)",
                params![cursor, b"not a wire".to_vec()],
            )
            .unwrap();
        }
    };
    let reference = open(&copy_of(&seed, "restart-inline"));
    add_rows(&reference.lock().unwrap().conn);
    reference.import_native_key_cache(&seed.cache).unwrap();
    let inline = dump(&reference);

    let config = copy_of(&seed, "restart");
    {
        let client = open(&config);
        deferred_import(&client, &seed.cache);
        client.recover_decoder_page(2).unwrap();
        client.recover_decoder_page(2).unwrap();
    }
    let client = open(&config);
    // Rows arrive while the process is closed: the eligible set grows past the resume position.
    add_rows(&client.lock().unwrap().conn);
    deferred_import(&client, &seed.cache);
    let record = cursor_record(&client).unwrap();
    assert_eq!(record["last_cursor"], 4, "{record}");
    assert_eq!(record["completed"], 4, "{record}");
    assert_eq!(record["total"], 9, "{record}");
    let pending = client.decoder_recovery_pending().unwrap().unwrap();
    assert_eq!((pending.completed, pending.total), (4, 9));

    run_to_completion(&client, 2);
    assert_eq!(dump(&client), inline);
}
