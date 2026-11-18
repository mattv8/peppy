//! Ownership ordering, authentic compacted-snapshot restore, decoder recovery, and the schema
//! upgrades for message reactions.

use super::*;

use crate::reaction_tests::{Fixture, actor, reaction, seed_message, seed_rcs_identity, state_of};
use peppy_crypto::{create_vault_check_header, derive_root_key};

const PASSPHRASE: &str = "correct horse battery staple";

impl Fixture {
    /// Seals an event the way a client does: the authenticated compaction copy travels inside the
    /// encrypted payload and must match the envelope's metadata.
    fn sealed(&self, producer: DeviceId, sequence: u64, payload: &PrivatePayload) -> Envelope {
        self.client.set_server_compaction_state(true, true).unwrap();
        let compaction = {
            let mut store = self.client.lock().unwrap();
            let (conn, ctx) = store.parts();
            compaction_plan(conn, &ctx, EnvelopePurpose::Event, payload)
                .unwrap()
                .map(|plan| CompactionMetadata {
                    key: plan.key,
                    terminal: plan.terminal,
                    supersedes: Vec::new(),
                    checkpoint: false,
                })
        };
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
            compaction: compaction.clone(),
            origin_tag: None,
            ciphertext: Vec::new(),
        };
        let plain = serialize_authenticated_payload(payload, compaction.as_ref()).unwrap();
        envelope.ciphertext =
            seal_frame(&encrypt(&self.event_key, &envelope.aad_bytes().unwrap(), &plain).unwrap());
        envelope
    }
}

/// An eligibility record at a mapping revision. A newer revision supersedes an older one.
fn eligibility_at(
    gateway: DeviceId,
    conversation: ConversationId,
    state: RcsEligibility,
    mapping_revision: u64,
) -> PrivatePayload {
    PrivatePayload::RcsLinkEligibility(RcsLinkEligibilityPayload {
        eligibility: RcsEligibilityProjection {
            conversation_id: conversation,
            eligibility: state,
            feature_bits: 0,
            mapping_revision,
            observed_at_ms: notifications::now_ms(),
            gateway_device_id: gateway,
        },
    })
}

fn eligibility(
    gateway: DeviceId,
    conversation: ConversationId,
    state: RcsEligibility,
) -> PrivatePayload {
    eligibility_at(gateway, conversation, state, 1)
}

fn stored_reaction(fixture: &Fixture, message: MessageId) -> Option<String> {
    fixture.query_text(&format!(
        "SELECT reactions_json FROM message_reactions WHERE message_id='{message}'"
    ))
}

fn staged_count(fixture: &Fixture) -> i64 {
    fixture
        .query_text("SELECT COUNT(*) FROM reaction_sets_staged")
        .unwrap()
        .parse()
        .unwrap()
}

/// A fresh device of the same vault, able to read the source's sealed envelopes.
fn restore_target(source: &Fixture) -> (tempfile::TempDir, Client) {
    let directory = tempfile::TempDir::new().unwrap();
    let profile = source.profile.clone();
    let root = derive_root_key(PASSPHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    let target = Client::open(
        ClientConfig {
            database_path: directory.path().join("restored.db"),
            vault_id: source.vault_id,
            device_id: DeviceId::new(),
        },
        DatabaseKey::new(&[43; 32]).unwrap(),
    )
    .unwrap();
    target.unlock(&profile, &header, PASSPHRASE).unwrap();
    target.set_server_compaction_state(true, true).unwrap();
    (directory, target)
}

/// Restores the records as one compacted snapshot through the real staging path, then applies it.
fn restore_snapshot(target: &Client, high_water: u64, records: Vec<SnapshotRecord>) {
    let count = records.len() as u64;
    let session = target
        .begin_snapshot_with_compaction(
            Cursor(high_water),
            count,
            SnapshotPurpose::Resync,
            Some(45),
        )
        .unwrap();
    target
        .append_snapshot_page(session.generation, &records)
        .unwrap();
    target.finish_snapshot(session.generation).unwrap();
    for _ in 0..6 {
        let report = target.apply_pending(100).unwrap();
        if report.applied + report.quarantined == 0 {
            break;
        }
    }
}

fn quarantined_count(target: &Client) -> i64 {
    target
        .lock()
        .unwrap()
        .conn
        .query_row(
            "SELECT COUNT(*) FROM journal WHERE status='quarantined'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn projected_reactions(target: &Client, message: MessageId) -> Option<String> {
    target
        .lock()
        .unwrap()
        .conn
        .query_row(
            "SELECT reactions_json FROM message_reactions WHERE message_id=?",
            [message.to_string()],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

#[test]
fn reaction_set_before_its_eligibility_in_a_compacted_snapshot_is_restored() {
    // The gateway (source) produces a set at cursor 1 and its link eligibility at cursor 2.
    let source = Fixture::new();
    let gateway = source.device_id;
    let conversation = ConversationId::new();
    let message = seed_message(
        &source,
        Transport::Rcs,
        Direction::Incoming,
        "hi",
        conversation,
    );
    seed_rcs_identity(&source, message, conversation, "g-snapshot");
    let set = source.sealed(
        gateway,
        1,
        &PrivatePayload::MessageReactions {
            message_id: message,
            conversation_id: conversation,
            gateway_id: gateway,
            observed_at_ms: 100,
            reactions: vec![reaction("👍", vec![actor(true, None)])],
        },
    );
    let proof = source.sealed(
        gateway,
        2,
        &eligibility(gateway, conversation, RcsEligibility::Eligible),
    );
    assert!(
        set.compaction.is_some(),
        "the set travels in the compacted lineage"
    );

    let (_directory, target) = restore_target(&source);
    restore_snapshot(
        &target,
        2,
        vec![
            SnapshotRecord {
                cursor: Cursor(1),
                envelope: set,
            },
            SnapshotRecord {
                cursor: Cursor(2),
                envelope: proof,
            },
        ],
    );
    let stored = projected_reactions(&target, message)
        .expect("the restored set is projected once ownership arrives");
    assert!(stored.contains("👍"), "{stored}");
    assert_eq!(
        quarantined_count(&target),
        0,
        "a valid set is never quarantined for ordering"
    );
}

#[test]
fn reaction_set_restored_over_a_stale_non_owner_projection_is_published_by_the_newer_proof() {
    // An existing client holds an older `not_eligible` projection (mapping revision 1). The gateway
    // relinked: its set (cursor 20) precedes a refreshed eligible record (cursor 30, revision 2).
    let source = Fixture::new();
    let gateway = source.device_id;
    let conversation = ConversationId::new();
    let message = seed_message(
        &source,
        Transport::Rcs,
        Direction::Incoming,
        "hi",
        conversation,
    );
    seed_rcs_identity(&source, message, conversation, "g-stale");
    let stale = source.sealed(
        gateway,
        1,
        &eligibility_at(gateway, conversation, RcsEligibility::NotEligible, 1),
    );
    let set = source.sealed(
        gateway,
        2,
        &PrivatePayload::MessageReactions {
            message_id: message,
            conversation_id: conversation,
            gateway_id: gateway,
            observed_at_ms: 200,
            reactions: vec![reaction("👍", vec![actor(true, None)])],
        },
    );
    let refreshed = source.sealed(
        gateway,
        3,
        &eligibility_at(gateway, conversation, RcsEligibility::Eligible, 2),
    );

    let (_directory, target) = restore_target(&source);
    target.ingest(&stale, Cursor(1)).unwrap();
    target.apply_pending(10).unwrap();
    let before: String = target
        .lock()
        .unwrap()
        .conn
        .query_row(
            "SELECT eligibility FROM rcs_link_eligibility_projection WHERE gateway_device_id=?",
            [gateway.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        before, "not_eligible",
        "the existing client starts with the stale projection"
    );

    restore_snapshot(
        &target,
        30,
        vec![
            SnapshotRecord {
                cursor: Cursor(20),
                envelope: set,
            },
            SnapshotRecord {
                cursor: Cursor(30),
                envelope: refreshed,
            },
        ],
    );

    let stored = projected_reactions(&target, message)
        .expect("the set is published once the newer eligible proof arrives");
    assert!(stored.contains("👍"), "{stored}");
    let after: String = target
        .lock()
        .unwrap()
        .conn
        .query_row(
            "SELECT eligibility FROM rcs_link_eligibility_projection WHERE gateway_device_id=?",
            [gateway.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, "eligible");
    assert_eq!(
        quarantined_count(&target),
        0,
        "the stale projection never quarantines the set"
    );
    let staged: i64 = target
        .lock()
        .unwrap()
        .conn
        .query_row("SELECT COUNT(*) FROM reaction_sets_staged", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(staged, 0, "the staged set is consumed");
}

#[test]
fn reaction_set_staged_before_eligibility_is_published_when_the_owner_proof_arrives() {
    let fixture = Fixture::new();
    let gateway = fixture.device_id;
    let conversation = ConversationId::new();
    let message = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "hi",
        conversation,
    );
    let set = PrivatePayload::MessageReactions {
        message_id: message,
        conversation_id: conversation,
        gateway_id: gateway,
        observed_at_ms: 10,
        reactions: vec![reaction("❤️", vec![actor(false, Some("+15550100"))])],
    };
    assert_eq!(
        fixture.apply(&fixture.event(gateway, 1, 1, set), 1).applied,
        1
    );
    assert_eq!(staged_count(&fixture), 1);
    assert!(stored_reaction(&fixture, message).is_none());

    assert_eq!(
        fixture
            .apply(
                &fixture.event(
                    gateway,
                    2,
                    1,
                    eligibility(gateway, conversation, RcsEligibility::Eligible)
                ),
                2
            )
            .applied,
        1
    );
    assert_eq!(staged_count(&fixture), 0, "the staged set is consumed");
    assert!(stored_reaction(&fixture, message).unwrap().contains("❤️"));
}

#[test]
fn reaction_set_from_a_gateway_that_does_not_own_the_link_is_staged_not_quarantined() {
    let fixture = Fixture::new();
    let gateway = fixture.device_id;
    let conversation = ConversationId::new();
    let message = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "hi",
        conversation,
    );
    assert_eq!(
        fixture
            .apply(
                &fixture.event(
                    gateway,
                    1,
                    1,
                    eligibility(gateway, conversation, RcsEligibility::NotEligible)
                ),
                1
            )
            .applied,
        1
    );
    let set = PrivatePayload::MessageReactions {
        message_id: message,
        conversation_id: conversation,
        gateway_id: gateway,
        observed_at_ms: 10,
        reactions: vec![reaction("❤️", vec![actor(false, Some("+15550100"))])],
    };
    assert_eq!(
        fixture.apply(&fixture.event(gateway, 2, 1, set), 2).applied,
        1,
        "authentic and well formed: kept"
    );
    assert_eq!(staged_count(&fixture), 1);
    assert!(stored_reaction(&fixture, message).is_none());
}

#[test]
fn reaction_staged_set_survives_a_stale_non_owner_projection_until_a_newer_proof() {
    let fixture = Fixture::new();
    let gateway = fixture.device_id;
    let conversation = ConversationId::new();
    let message = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "hi",
        conversation,
    );
    let set = PrivatePayload::MessageReactions {
        message_id: message,
        conversation_id: conversation,
        gateway_id: gateway,
        observed_at_ms: 10,
        reactions: vec![reaction("❤️", vec![actor(false, Some("+15550100"))])],
    };
    fixture.apply(&fixture.event(gateway, 1, 1, set), 1);
    assert_eq!(staged_count(&fixture), 1);
    fixture.apply(
        &fixture.event(
            gateway,
            2,
            1,
            eligibility_at(gateway, conversation, RcsEligibility::NotEligible, 1),
        ),
        2,
    );
    assert_eq!(
        staged_count(&fixture),
        1,
        "a non-owner projection keeps the set staged"
    );
    assert!(stored_reaction(&fixture, message).is_none());
    fixture.apply(
        &fixture.event(
            gateway,
            3,
            1,
            eligibility_at(gateway, conversation, RcsEligibility::Eligible, 2),
        ),
        3,
    );
    assert_eq!(staged_count(&fixture), 0);
    assert!(stored_reaction(&fixture, message).unwrap().contains("❤️"));
}

#[test]
fn reaction_applied_set_prunes_staged_sets_it_supersedes_and_keeps_newer_ones() {
    let fixture = Fixture::new();
    let owner = fixture.device_id;
    let other = DeviceId::new();
    let conversation = ConversationId::new();
    let older = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "older",
        conversation,
    );
    let newer = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "newer",
        conversation,
    );
    let staged_set = |message: MessageId, observed_at_ms: i64| PrivatePayload::MessageReactions {
        message_id: message,
        conversation_id: conversation,
        gateway_id: other,
        observed_at_ms,
        reactions: vec![reaction("😂", vec![actor(false, Some("+15550100"))])],
    };
    fixture.apply(&fixture.event(other, 1, 1, staged_set(older, 40)), 1);
    fixture.apply(&fixture.event(other, 2, 1, staged_set(newer, 60)), 2);
    assert_eq!(staged_count(&fixture), 2);

    fixture.apply(
        &fixture.event(
            owner,
            1,
            1,
            eligibility_at(owner, conversation, RcsEligibility::Eligible, 1),
        ),
        3,
    );
    let owned = PrivatePayload::MessageReactions {
        message_id: older,
        conversation_id: conversation,
        gateway_id: owner,
        observed_at_ms: 50,
        reactions: vec![reaction("👍", vec![actor(true, None)])],
    };
    assert_eq!(
        fixture.apply(&fixture.event(owner, 2, 1, owned), 4).applied,
        1
    );
    assert_eq!(
        staged_count(&fixture),
        1,
        "the staged set older than the applied one is pruned"
    );
    assert_eq!(
        fixture
            .query_text(&format!(
                "SELECT message_id FROM reaction_sets_staged WHERE gateway_id='{other}'"
            ))
            .unwrap(),
        newer.to_string(),
        "a newer staged set survives"
    );
}

#[test]
fn reaction_set_from_a_gateway_that_is_not_the_producer_is_still_refused() {
    let fixture = Fixture::new();
    let gateway = fixture.device_id;
    let conversation = ConversationId::new();
    let message = seed_message(
        &fixture,
        Transport::Rcs,
        Direction::Incoming,
        "hi",
        conversation,
    );
    let forged = PrivatePayload::MessageReactions {
        message_id: message,
        conversation_id: conversation,
        gateway_id: gateway,
        observed_at_ms: 10,
        reactions: vec![reaction("❤️", vec![actor(false, Some("+15550100"))])],
    };
    // Producer is not the named gateway: refused whatever the projection says.
    assert_eq!(
        fixture
            .apply(&fixture.event(DeviceId::new(), 1, 1, forged), 1)
            .quarantined,
        1
    );
    assert_eq!(staged_count(&fixture), 0);
}

#[test]
fn reaction_events_left_unknown_by_an_older_decoder_are_recovered_as_history() {
    let fixture = Fixture::new();
    let own = fixture.device_id;
    let request = ReactionRequestId::new();
    let conversation = ConversationId::new();
    let envelope = fixture.event(
        DeviceId::new(),
        1,
        1,
        PrivatePayload::ReactionRequest {
            request_id: request,
            message_id: MessageId::new(),
            conversation_id: conversation,
            target_gateway_id: own,
            emoji: Some("👍".to_owned()),
            requested_at_ms: notifications::now_ms(),
        },
    );
    fixture.client.ingest(&envelope, Cursor(1)).unwrap();
    // The older decoder kept the event as unknown: applied, with no request row.
    fixture.execute("UPDATE journal SET status='applied',reason=NULL WHERE cursor=1");
    fixture.execute("INSERT INTO metadata(k,v) VALUES('decoder_revision:1','12') ON CONFLICT(k) DO UPDATE SET v=excluded.v");
    assert_eq!(state_of(&fixture, request), None);

    let (path, vault, device) = (
        fixture.database_path.clone(),
        fixture.vault_id,
        fixture.device_id,
    );
    let profile = fixture.profile.clone();
    drop(fixture.client);
    let config = ClientConfig {
        database_path: path,
        vault_id: vault,
        device_id: device,
    };
    let reopened = Client::open(config, DatabaseKey::new(&[41; 32]).unwrap()).unwrap();
    let root = derive_root_key(PASSPHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    reopened.unlock(&profile, &header, PASSPHRASE).unwrap();
    reopened.apply_pending(100).unwrap();

    let store = reopened.lock().unwrap();
    let (state, from_snapshot): (String, bool) = store
        .conn
        .query_row(
            "SELECT state,from_snapshot FROM reaction_requests WHERE request_id=?",
            [request.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the recovered request is applied");
    drop(store);
    assert_eq!(state, "sending");
    assert!(
        from_snapshot,
        "a recovered request is history, never actionable"
    );
    assert!(reopened.pending_reaction_jobs().unwrap().is_empty());
}

#[test]
fn reaction_schema_61_upgrade_rebuilds_the_staging_table_per_producer() {
    let fixture = Fixture::new();
    {
        let store = fixture.client.lock().unwrap();
        store
            .conn
            .execute_batch("DROP INDEX reaction_sets_staged_owner; DROP TABLE reaction_sets_staged; UPDATE schema_meta SET version=61;")
            .unwrap();
    }
    let (path, vault, device) = (
        fixture.database_path.clone(),
        fixture.vault_id,
        fixture.device_id,
    );
    drop(fixture.client);
    let reopened = Client::open(
        ClientConfig {
            database_path: path,
            vault_id: vault,
            device_id: device,
        },
        DatabaseKey::new(&[41; 32]).unwrap(),
    )
    .unwrap();
    let store = reopened.lock().unwrap();
    let version: i64 = store
        .conn
        .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 63);
    assert!(table_exists(&store.conn, "reaction_sets_staged").unwrap());
    let key_columns: Vec<String> = store
        .conn
        .prepare(
            "SELECT name FROM pragma_table_info('reaction_sets_staged') WHERE pk>0 ORDER BY pk",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(key_columns, ["message_id", "gateway_id"]);
}
