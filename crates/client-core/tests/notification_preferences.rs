//! Vault-wide notification preference replication.
use crate::common::*;
use peppy_crypto::{KeyPurpose, derive_purpose_key, derive_root_key, encrypt};
use tempfile::TempDir;

fn hidden() -> NotificationPreferences {
    NotificationPreferences {
        preview: NotificationPreview::Hidden,
        ..NotificationPreferences::default()
    }
}

/// Produces an authenticated v45 payload, rather than merely deserializing old JSON.
fn legacy_preference_event(
    vault: &Vault,
    producer: DeviceId,
    sequence: u64,
    revision: u64,
) -> Envelope {
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    let mut envelope = Envelope {
        protocol_version: 1,
        envelope_id: EnvelopeId::new(),
        command_id: None,
        vault_id: vault.id,
        producer_device_id: producer,
        producer_sequence: SourceSequence(sequence),
        key_epoch: 1,
        crypto_suite: vault.profile.crypto_suite,
        profile_fingerprint: vault.profile.fingerprint().unwrap(),
        purpose: EnvelopePurpose::Event,
        route: None,
        compaction: None,
        origin_tag: None,
        ciphertext: Vec::new(),
    };
    let mut payload = serde_json::to_value(PrivatePayload::NotificationPreferences {
        preferences: NotificationPreferences {
            message_banners: false,
            mirrored_banners: true,
            preview: NotificationPreview::Hidden,
            mute_with_heads: false,
            show_source: true,
        },
        logical_revision: revision,
        writer_device_id: producer.to_string(),
    })
    .unwrap();
    payload["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("showSource");
    let plaintext = serde_json::to_vec(&payload).unwrap();
    let encrypted = encrypt(&key, &envelope.aad_bytes().unwrap(), &plaintext).unwrap();
    envelope.ciphertext = [encrypted.nonce.as_slice(), &encrypted.ciphertext].concat();
    envelope
}

#[test]
fn full_preview_normalizes_source_visibility_before_deduplication() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "full-normalization", &vault), &vault);
    let contradictory = NotificationPreferences {
        preview: NotificationPreview::Full,
        show_source: true,
        ..NotificationPreferences::default()
    };
    client
        .set_notification_preferences(contradictory.clone())
        .unwrap();
    assert!(
        !client
            .notification_preferences()
            .unwrap()
            .unwrap()
            .show_source
    );
    let events = client.pending_outbox().unwrap().len();
    client.set_notification_preferences(contradictory).unwrap();
    assert_eq!(client.pending_outbox().unwrap().len(), events);
}

#[test]
fn legacy_full_preview_normalizes_source_visibility_before_adoption() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "legacy-full-normalization", &vault), &vault);
    assert!(
        !client
            .adopt_legacy_notification_preferences(NotificationPreferences {
                preview: NotificationPreview::Full,
                show_source: true,
                ..NotificationPreferences::default()
            })
            .unwrap()
    );
    assert!(client.notification_preferences().unwrap().is_none());
}

#[test]
fn preference_source_visibility_defaults_for_legacy_json_and_round_trips() {
    let legacy: NotificationPreferences = serde_json::from_str(
        r#"{"messageBanners":true,"mirroredBanners":true,"preview":"hidden","muteWithHeads":true}"#,
    )
    .unwrap();
    assert!(!legacy.show_source);

    let preference = NotificationPreferences {
        preview: NotificationPreview::Hidden,
        show_source: true,
        ..NotificationPreferences::default()
    };
    let encoded = serde_json::to_value(&preference).unwrap();
    assert_eq!(encoded["showSource"], true);
    assert_eq!(
        serde_json::from_value::<NotificationPreferences>(encoded).unwrap(),
        preference
    );

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct LegacyReaderPreferences {
        preview: NotificationPreview,
    }
    let legacy_reader: LegacyReaderPreferences = serde_json::from_value(serde_json::json!({
        "preview": "hidden",
        "showSource": true,
    }))
    .unwrap();
    assert_eq!(legacy_reader.preview, NotificationPreview::Hidden);
    assert!(
        serde_json::from_value::<NotificationPreferences>(serde_json::json!({
            "messageBanners": true,
            "mirroredBanners": true,
            "preview": "unexpected",
        }))
        .is_err()
    );
}

fn settle_snapshot(client: &Client) {
    loop {
        let report = client.apply_pending(100).unwrap();
        if report.applied + report.drained + report.quarantined + report.superseded == 0
            && report.snapshot_remaining == 0
        {
            return;
        }
    }
}

fn import_snapshot(client: &Client, records: &[SnapshotRecord], high_water: u64, generation: u64) {
    let session = client
        .begin_snapshot_with_compaction(
            Cursor(high_water),
            records.len() as u64,
            SnapshotPurpose::Resync,
            Some(generation),
        )
        .unwrap();
    if !records.is_empty() {
        client
            .append_snapshot_page(session.generation, records)
            .unwrap();
    }
    client.finish_snapshot(session.generation).unwrap();
    settle_snapshot(client);
}

#[test]
fn locked_default_and_clocked_adoption_are_refused() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "locked", &vault);
    let locked = open(&cfg);
    assert_eq!(locked.notification_preferences().unwrap(), None);
    assert!(matches!(
        locked.set_notification_preferences(hidden()),
        Err(Error::KeysUnavailable)
    ));
    assert!(
        !locked
            .adopt_legacy_notification_preferences(hidden())
            .unwrap()
    );
    let client = unlocked(&config(&dir, "defaults", &vault), &vault);
    assert!(
        !client
            .adopt_legacy_notification_preferences(NotificationPreferences::default())
            .unwrap()
    );
}

#[test]
fn explicit_replication_tie_breaking_and_adoption_promotion() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let left = unlocked(&config(&dir, "left", &vault), &vault);
    let right = unlocked(&config(&dir, "right", &vault), &vault);
    let left_value = NotificationPreferences {
        show_source: true,
        ..hidden()
    };
    let right_value = NotificationPreferences {
        message_banners: false,
        ..Default::default()
    };
    left.set_notification_preferences(left_value.clone())
        .unwrap();
    right
        .set_notification_preferences(right_value.clone())
        .unwrap();
    let left_event = left.pending_outbox().unwrap().pop().unwrap();
    let right_event = right.pending_outbox().unwrap().pop().unwrap();
    let expected =
        if left_event.producer_device_id.to_string() > right_event.producer_device_id.to_string() {
            left_value
        } else {
            right_value
        };
    for client in [&left, &right] {
        client.ingest(&left_event, Cursor(1)).unwrap();
        client.ingest(&right_event, Cursor(2)).unwrap();
        client.apply_pending(10).unwrap();
        assert_eq!(
            client.notification_preferences().unwrap(),
            Some(expected.clone())
        );
    }
    let adopter = unlocked(&config(&dir, "adopter", &vault), &vault);
    assert!(
        adopter
            .adopt_legacy_notification_preferences(hidden())
            .unwrap()
    );
    let before = adopter.pending_outbox().unwrap().len();
    adopter.set_notification_preferences(hidden()).unwrap();
    assert_eq!(adopter.pending_outbox().unwrap().len(), before + 1);
}

#[test]
fn higher_revision_source_visibility_wins_live_replication_deterministically() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let source = unlocked(&config(&dir, "higher-source", &vault), &vault);
    let target = unlocked(&config(&dir, "lower-target", &vault), &vault);
    let lower = hidden();
    let higher = NotificationPreferences {
        show_source: true,
        ..hidden()
    };

    source.set_notification_preferences(lower.clone()).unwrap();
    source.set_notification_preferences(higher.clone()).unwrap();
    target.set_notification_preferences(lower).unwrap();
    let higher_event = source.pending_outbox().unwrap().pop().unwrap();
    target.ingest(&higher_event, Cursor(1)).unwrap();
    target.apply_pending(10).unwrap();

    assert_eq!(target.notification_preferences().unwrap(), Some(higher));
}

#[test]
fn adoption_refuses_a_pending_preference_already_in_the_compaction_frontier() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "frontier-writer", &vault), &vault);
    let target = unlocked(&config(&dir, "frontier-target", &vault), &vault);
    writer.set_notification_preferences(hidden()).unwrap();
    let event = writer.pending_outbox().unwrap().pop().unwrap();
    target.ingest(&event, Cursor(1)).unwrap();
    target.set_server_compaction_state(true, true).unwrap();
    assert!(
        !target
            .adopt_legacy_notification_preferences(NotificationPreferences {
                message_banners: false,
                ..Default::default()
            })
            .unwrap()
    );
}

#[test]
fn snapshot_without_preference_record_never_deletes_existing_value() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let producer_config = config(&dir, "noncompact-producer", &vault);
    let producer = open(&producer_config);
    producer
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    let peer = unlocked(&config(&dir, "noncompact-peer", &vault), &vault);
    producer.set_notification_preferences(hidden()).unwrap();
    let event = producer.pending_outbox().unwrap().pop().unwrap();
    assert!(event.compaction.is_none());
    producer.ingest(&event, Cursor(1)).unwrap();
    producer.apply_pending(10).unwrap();
    peer.ingest(&event, Cursor(1)).unwrap();
    peer.apply_pending(10).unwrap();
    import_snapshot(&producer, &[], 1, 1);
    import_snapshot(&peer, &[], 1, 1);
    assert_eq!(producer.notification_preferences().unwrap(), Some(hidden()));
    assert_eq!(peer.notification_preferences().unwrap(), Some(hidden()));
}

#[test]
fn compacted_snapshot_stages_and_merges_the_vault_winner() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "compact-writer", &vault), &vault);
    let fresh_config = config(&dir, "compact-fresh", &vault);
    let fresh = unlocked(&fresh_config, &vault);
    let preference = NotificationPreferences {
        show_source: true,
        ..hidden()
    };
    writer
        .set_notification_preferences(preference.clone())
        .unwrap();
    let event = writer.pending_outbox().unwrap().pop().unwrap();
    assert!(event.compaction.is_some());
    import_snapshot(
        &fresh,
        &[SnapshotRecord {
            cursor: Cursor(1),
            envelope: event,
        }],
        1,
        1,
    );
    assert_eq!(
        fresh.notification_preferences().unwrap(),
        Some(preference.clone())
    );
    drop(fresh);
    let reopened = unlocked(&fresh_config, &vault);
    assert_eq!(
        reopened.notification_preferences().unwrap(),
        Some(preference)
    );
}

#[test]
fn interrupted_compacted_snapshot_promotion_preserves_source_visibility() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let writer = unlocked(&config(&dir, "interrupted-writer", &vault), &vault);
    let target_config = config(&dir, "interrupted-target", &vault);
    let target = unlocked(&target_config, &vault);
    let preference = NotificationPreferences {
        show_source: true,
        ..hidden()
    };
    writer
        .set_notification_preferences(preference.clone())
        .unwrap();
    let event = writer.pending_outbox().unwrap().pop().unwrap();
    let session = target
        .begin_snapshot_with_compaction(Cursor(1), 1, SnapshotPurpose::Resync, Some(91))
        .unwrap();
    target
        .append_snapshot_page(
            session.generation,
            &[SnapshotRecord {
                cursor: Cursor(1),
                envelope: event,
            }],
        )
        .unwrap();
    drop(target);

    let resumed = unlocked(&target_config, &vault);
    resumed.finish_snapshot(session.generation).unwrap();
    settle_snapshot(&resumed);
    assert_eq!(
        resumed.notification_preferences().unwrap(),
        Some(preference)
    );
}

#[test]
fn authenticated_legacy_event_and_snapshot_clear_source_visibility() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let producer = DeviceId::new();
    let legacy = legacy_preference_event(&vault, producer, 1, 900);
    let current = NotificationPreferences {
        show_source: true,
        ..hidden()
    };

    let live = unlocked(&config(&dir, "legacy-live", &vault), &vault);
    live.set_notification_preferences(current.clone()).unwrap();
    live.ingest(&legacy, Cursor(1)).unwrap();
    live.apply_pending(10).unwrap();
    assert_eq!(
        live.notification_preferences().unwrap(),
        Some(NotificationPreferences {
            message_banners: false,
            mirrored_banners: true,
            preview: NotificationPreview::Hidden,
            mute_with_heads: false,
            show_source: false,
        })
    );

    let staged = unlocked(&config(&dir, "legacy-snapshot", &vault), &vault);
    staged.set_notification_preferences(current).unwrap();
    import_snapshot(
        &staged,
        &[SnapshotRecord {
            cursor: Cursor(1),
            envelope: legacy,
        }],
        1,
        90,
    );
    assert!(
        !staged
            .notification_preferences()
            .unwrap()
            .unwrap()
            .show_source
    );
}
