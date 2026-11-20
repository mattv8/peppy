use crate::common;
use crate::common::*;
use peppy_crypto::{
    EncryptedEnvelope, KeyPurpose, decrypt, derive_purpose_key, derive_root_key, encrypt,
};
use serde_json::json;
use std::collections::BTreeSet;
use tempfile::TempDir;

const ADDRESS: &str = "+14155552671";

fn incoming(body: &str, provider_id: &str) -> IncomingSms {
    let mut sms = common::incoming(body, provider_id);
    sms.sender_address = ADDRESS.into();
    sms
}

fn sms_row(id: &str, body: &str, at: i64, direction: Direction) -> SmsAcquisitionInput {
    SmsAcquisitionInput {
        source: SmsSource {
            source_generation: "install".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: id.into(),
            provider_thread_id: Some("thread-1".into()),
            broadcast_id: None,
        },
        direction,
        sender_address: (direction == Direction::Incoming).then(|| ADDRESS.into()),
        recipients: if direction == Direction::Outgoing {
            vec![ADDRESS.into()]
        } else {
            vec![]
        },
        body: body.into(),
        imported: false,
        sent_at_ms: Some(at),
        date_sent_ms: Some(at),
        creator: None,
        provider_read: None,
        source_row_fingerprint: format!("{id}:{at}"),
    }
}

fn capture_sms(client: &Client, row: SmsAcquisitionInput) -> Captured {
    let acquisition = client.begin_sms_acquisition(row).unwrap();
    client
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap()
}

fn converge(client: &Client) {
    for _ in 0..1000 {
        if !client.converge_conversations(16).unwrap() {
            return;
        }
    }
    panic!("conversation convergence did not finish");
}

fn message_ids(client: &Client, conversation: ConversationId) -> BTreeSet<String> {
    let page = client.messages_page(conversation, None, 100).unwrap();
    assert!(!page.has_more);
    page.messages
        .into_iter()
        .map(|message| message.payload.record.message_id.to_string())
        .collect()
}

fn explicitly_captured(
    client: &Client,
    id: ConversationId,
    body: &str,
    provider: &str,
) -> Captured {
    let mut sms = incoming(body, provider);
    sms.conversation_id = Some(id);
    client.capture_incoming(sms).unwrap()
}

#[test]
fn mms_first_broadcast_and_provider_share_a_conversation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let at = 1_700_000_000_000;
    let mms = client
        .begin_mms_acquisition(MmsAcquisitionInput {
            source: MmsSource {
                source_generation: "install".into(),
                subscription_id: "sim-1".into(),
                provider_message_id: "mms-1".into(),
                provider_thread_id: Some("thread-1".into()),
            },
            direction: Direction::Incoming,
            sender_address: Some(ADDRESS.into()),
            recipients: vec![],
            subject: Some("subject".into()),
            body: "mms body".into(),
            imported: false,
            observed_at_ms: at,
            provider_read: None,
            transaction_id: None,
        })
        .unwrap();
    let mms = client
        .complete_mms_acquisition(&mms.acquisition_id)
        .unwrap();
    let broadcast = client
        .capture_incoming_broadcast(
            incoming("live", "broadcast-1"),
            Some(at + 1_000),
            Some(at + 1_000),
        )
        .unwrap();
    let outgoing = capture_sms(
        &client,
        sms_row("out", "outgoing", at + 2_000, Direction::Outgoing),
    );
    let copy = capture_sms(
        &client,
        sms_row("provider", "live", at + 1_000, Direction::Incoming),
    );
    assert!(copy.duplicate);
    assert_eq!(copy.message_id, broadcast.message_id);
    assert_eq!(mms.conversation_id, broadcast.conversation_id);
    assert_eq!(outgoing.conversation_id, broadcast.conversation_id);
    assert_eq!(client.list_conversations().unwrap().len(), 1);
    assert_eq!(
        message_ids(&client, broadcast.conversation_id),
        [
            mms.message_id.to_string(),
            broadcast.message_id.to_string(),
            outgoing.message_id.to_string()
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn reservation_then_broadcast_keeps_the_thread_route() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let at = 1_700_000_000_000;
    let reserved = client
        .begin_sms_acquisition(sms_row("provider", "live", at, Direction::Incoming))
        .unwrap();
    let broadcast = client
        .capture_incoming_broadcast(incoming("live", "broadcast"), Some(at), Some(at))
        .unwrap();
    let completed = client
        .complete_sms_acquisition(&reserved.acquisition_id)
        .unwrap();
    let outgoing = capture_sms(
        &client,
        sms_row("out", "outgoing", at + 1_000, Direction::Outgoing),
    );
    assert_eq!(completed.message_id, broadcast.message_id);
    assert_eq!(outgoing.conversation_id, broadcast.conversation_id);
    assert_eq!(client.list_conversations().unwrap().len(), 1);
    assert_eq!(
        message_ids(&client, broadcast.conversation_id),
        [
            broadcast.message_id.to_string(),
            outgoing.message_id.to_string()
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn late_history_merges_messages_unread_and_redirects_to_lowest_id() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let old = unlocked(&config(&dir, "old", &vault), &vault);
    let new = unlocked(&config(&dir, "new", &vault), &vault);
    let ids = [
        ConversationId::new(),
        ConversationId::new(),
        ConversationId::new(),
    ];
    let a = explicitly_captured(&old, ids[0], "old history", "old");
    let b = explicitly_captured(&new, ids[1], "new one", "new-1");
    let c = explicitly_captured(&new, ids[2], "new two", "new-2");
    let mut server = Server::default();
    server.upload(&old);
    server.sync(&new);
    converge(&new);
    let survivor = *ids.iter().min_by_key(|id| id.to_string()).unwrap();
    assert_eq!(new.list_conversations().unwrap().len(), 1);
    assert_eq!(
        message_ids(&new, survivor),
        [
            a.message_id.to_string(),
            b.message_id.to_string(),
            c.message_id.to_string()
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(new.unread_count(survivor).unwrap(), 3);
    for source in ids.into_iter().filter(|id| *id != survivor) {
        assert_eq!(new.conversation_redirect(source).unwrap(), Some(survivor));
    }
}

#[test]
fn merged_conversation_is_searchable_by_number_and_contact_name() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "search", &vault);
    let client = unlocked(&cfg, &vault);
    client.capture_contact_book(&json!({
        "schema_version": 1,
        "book": {"id": "book", "owner_device_id": cfg.device_id.to_string(), "generation": "1", "state": "active", "region": "US"},
        "contacts": [{
            "id": "alice", "book_id": "book", "revision": "1", "display_name": "Alice Example",
            "name": {"given": "Alice", "family": "Example"},
            "phones": [{"id": "phone", "value": ADDRESS}], "emails": [], "addresses": [],
            "provenance": {"source_id": "alice-source", "read_only": false}
        }]
    }).to_string()).unwrap();
    client
        .resolve_contact_addresses_json(&json!({"addresses": [ADDRESS]}).to_string())
        .unwrap();
    let ids = [ConversationId::new(), ConversationId::new()];
    explicitly_captured(&client, ids[0], "first", "one");
    explicitly_captured(&client, ids[1], "second", "two");
    converge(&client);
    let survivor = ids
        .iter()
        .min_by_key(|id| id.to_string())
        .unwrap()
        .to_string();
    let conversations = client.list_conversations().unwrap();
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0].conversation_id.to_string(), survivor);
    for query in ["4155552671", "Alice"] {
        let hits = client
            .search_local(LocalSearchRequest {
                query: query.into(),
                kind: Some(SearchKind::Conversation),
                cursor: None,
                limit: Some(20),
            })
            .unwrap()
            .items;
        assert_eq!(hits.len(), 1, "query {query}");
        assert!(
            matches!(&hits[0], LocalSearchHit::Conversation { conversation_id, .. } if conversation_id == &survivor)
        );
    }
}

#[test]
fn pin_and_newer_folder_from_source_survive_merge() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "pin", &vault), &vault);
    let mut ids = [ConversationId::new(), ConversationId::new()];
    ids.sort_by_key(|id| id.to_string());
    explicitly_captured(&client, ids[0], "survivor", "one");
    explicitly_captured(&client, ids[1], "source", "two");
    client.set_conversation_pinned(ids[1], true).unwrap();
    client
        .set_conversation_folder(ids[0], ConversationFolder::Archived)
        .unwrap();
    client
        .set_conversation_folder(ids[1], ConversationFolder::Spam)
        .unwrap();
    converge(&client);
    let rows = client.list_conversations().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].conversation_id, ids[0]);
    assert!(rows[0].pinned);
    assert_eq!(rows[0].folder, ConversationFolder::Spam);
}

#[test]
fn opposite_event_order_converges_on_both_devices() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let first = unlocked(&config(&dir, "first-writer", &vault), &vault);
    let second = unlocked(&config(&dir, "second-writer", &vault), &vault);
    let left = unlocked(&config(&dir, "left", &vault), &vault);
    let right = unlocked(&config(&dir, "right", &vault), &vault);
    let ids = [ConversationId::new(), ConversationId::new()];
    let a = explicitly_captured(&first, ids[0], "first", "first");
    let b = explicitly_captured(&second, ids[1], "second", "second");
    let mut forward = Server::default();
    forward.upload(&first);
    forward.upload(&second);
    let reverse = Server {
        log: forward.log.iter().rev().cloned().collect(),
    };
    forward.sync(&left);
    reverse.sync(&right);
    converge(&left);
    converge(&right);
    let survivor = *ids.iter().min_by_key(|id| id.to_string()).unwrap();
    let expected = [a.message_id.to_string(), b.message_id.to_string()]
        .into_iter()
        .collect();
    for client in [&left, &right] {
        let rows = client.list_conversations().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].conversation_id, survivor);
        assert_eq!(message_ids(client, survivor), expected);
    }
}

#[test]
fn merged_source_accepts_quick_reply_and_new_source_message() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "reply", &vault);
    let client = unlocked(&cfg, &vault);
    let mut ids = [ConversationId::new(), ConversationId::new()];
    ids.sort_by_key(|id| id.to_string());
    let first = explicitly_captured(&client, ids[0], "survivor", "one");
    let source = explicitly_captured(&client, ids[1], "source", "two");
    converge(&client);
    let reply = client
        .queue_quick_reply(QuickReplyInput {
            command_id: CommandId::new(),
            conversation_id: ids[0],
            body: "reply to source".into(),
            route: route(&cfg),
            expected_transport: Transport::Sms,
            expected_member_revision: None,
        })
        .unwrap();
    let late = explicitly_captured(&client, ids[1], "old build", "three");
    assert_eq!(
        message_ids(&client, ids[0]),
        [
            first.message_id.to_string(),
            source.message_id.to_string(),
            reply.message_id.to_string(),
            late.message_id.to_string()
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn group_with_same_participant_is_not_merged() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "group", &vault), &vault);
    let direct = client
        .capture_incoming(incoming("direct", "direct"))
        .unwrap();
    let group = client
        .capture_incoming_mms(IncomingMms {
            conversation_id: Some(ConversationId::new()),
            sender_address: ADDRESS.into(),
            body: "group".into(),
            provider_message_id: Some("group".into()),
            imported: false,
            attachment_ids: vec![],
            recipients: vec!["+14155552672".into(), "+14155552673".into()],
            subject: None,
        })
        .unwrap();
    converge(&client);
    assert_ne!(direct.conversation_id, group.conversation_id);
    assert_eq!(client.list_conversations().unwrap().len(), 2);
    assert_eq!(
        client.conversation_redirect(group.conversation_id).unwrap(),
        None
    );
    assert_eq!(
        message_ids(&client, group.conversation_id),
        [group.message_id.to_string()].into_iter().collect()
    );
}

fn mapping_observation() -> RcsMappingObservation {
    let outgoing = NativeRcsOutgoing {
        outgoing_id: "google-outgoing".into(),
        sim_participant_id: "google-outgoing".into(),
        own_identity_raw: "+12025550100".into(),
        sim_payload: NativeRcsSimPayload {
            two: 2,
            sim_number: 7,
        },
        rcs_chats_enabled: true,
    };
    RcsMappingObservation {
        google_conversation_id: "google-conversation".into(),
        google_candidate_count: 1,
        participants: vec![RcsParticipantObservation {
            identity: ADDRESS.into(),
            google_participant_id: "google-peer".into(),
            identifier_type: 1,
            is_visible: true,
            is_me: false,
        }],
        conversation_type: RcsConversationType::Rcs,
        status: "eligible".into(),
        send_mode: RcsSendMode::Automatic,
        read_only: false,
        default_outgoing: true,
        selected_sim_id: "android-subscription-7".into(),
        selected_sim_own_identity: "+12025550100".into(),
        carrier_binding: NativeCarrierRouteBinding {
            carrier_route_id: "android-subscription-7".into(),
            android_subscription_id: 7,
            is_current_default_sms: true,
            own_number_raw: "+12025550100".into(),
            own_number_source: NativeOwnNumberSource::SubscriptionManagerPhoneNumber,
            route_observed_at_ms: 1_000,
            route_context_revision: 1,
            same_handset_evidence_id: "verified-handset".into(),
        },
        outgoing: outgoing.clone(),
        native_evidence: NativeRcsEvidence {
            lifetime: NativeRcsLifetime {
                runtime_id: "runtime".into(),
                run_generation: 1,
                link_generation: 4,
                session_generation: 7,
                context_revision: 1,
            },
            lookup_id: "lookup".into(),
            lookup_complete: true,
            google_candidate_count: 1,
            settings_revision: 1,
            settings_provenance: NativeSettingsProvenance::CurrentEvent,
            settings_observed_at_ms: 1_000,
            settings_stream_epoch: 1,
            settings_currentness: NativeSettingsCurrentness::ContinuousVerifiedFeed,
            current_settings_sims: vec![outgoing],
            settings_validated_at_ms: 1_000,
            conversation_observed_at_ms: 1_000,
        },
        identity_region: "ZZ".into(),
        observed_at_ms: 1_000,
        link_generation: 4,
        session_generation: 7,
        context_revision: 1,
        canonicalizer_revision: 1,
        mapping_revision: 1,
        link_state: RcsLinkState::Linked,
    }
}

#[test]
fn rcs_mapped_conversation_is_not_merged() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "rcs", &vault), &vault);
    let mapped = client
        .capture_incoming(incoming("mapped", "mapped"))
        .unwrap();
    let outcome = client
        .verify_rcs_mapping(&mapping_observation(), 1_000)
        .unwrap();
    assert!(
        matches!(outcome, RcsMappingOutcome::Mapped(ref mapping) if mapping.conversation_id == mapped.conversation_id)
    );
    let other = explicitly_captured(&client, ConversationId::new(), "carrier", "carrier");
    converge(&client);
    assert_ne!(mapped.conversation_id, other.conversation_id);
    assert_eq!(client.list_conversations().unwrap().len(), 2);
    assert_eq!(
        client
            .conversation_redirect(mapped.conversation_id)
            .unwrap(),
        None
    );
    assert_eq!(
        client.conversation_redirect(other.conversation_id).unwrap(),
        None
    );
}

#[test]
fn reapplying_same_message_after_merge_does_not_quarantine() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let producer = unlocked(&config(&dir, "producer", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let mut ids = [ConversationId::new(), ConversationId::new()];
    ids.sort_by_key(|id| id.to_string());
    let original = explicitly_captured(&producer, ids[1], "replayed", "replayed");
    explicitly_captured(&receiver, ids[0], "survivor", "survivor");
    let mut server = Server::default();
    server.upload(&producer);
    let original_envelope = server.log[0].clone();
    assert_eq!(server.sync(&receiver).quarantined, 0);
    converge(&receiver);
    assert_eq!(
        receiver.conversation_redirect(ids[1]).unwrap(),
        Some(ids[0])
    );

    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    let (nonce, ciphertext) = original_envelope.ciphertext.split_at(24);
    let encrypted = EncryptedEnvelope {
        nonce: nonce.try_into().unwrap(),
        ciphertext: ciphertext.to_vec(),
    };
    let plain = decrypt(&key, &original_envelope.aad_bytes().unwrap(), &encrypted).unwrap();
    let mut redelivery = original_envelope.clone();
    redelivery.envelope_id = EnvelopeId::new();
    redelivery.producer_sequence = SourceSequence(original_envelope.producer_sequence.0 + 1);
    let encrypted = encrypt(&key, &redelivery.aad_bytes().unwrap(), &plain).unwrap();
    redelivery.ciphertext = [encrypted.nonce.as_slice(), &encrypted.ciphertext].concat();
    receiver.ingest(&redelivery, Cursor(2)).unwrap();
    let report = receiver.apply_pending(100).unwrap();
    assert_eq!(report.quarantined, 0);
    assert_eq!(report.applied, 1);
    assert!(message_ids(&receiver, ids[0]).contains(&original.message_id.to_string()));
    assert_eq!(message_ids(&receiver, ids[0]).len(), 2);
}
