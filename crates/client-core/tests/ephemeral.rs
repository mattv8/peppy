#[path = "common/mod.rs"]
mod common;

use common::*;
use std::collections::BTreeMap;

fn table_counts(config: &ClientConfig) -> BTreeMap<String, i64> {
    let connection = rusqlite::Connection::open(&config.database_path).unwrap();
    connection
        .execute_batch(
            "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
        )
        .unwrap();
    let tables: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let count = connection
                .query_row(&format!("SELECT COUNT(*) FROM [{table}]"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            (table, count)
        })
        .collect()
}

fn enable_typing(client: &Client, gateway: DeviceId, conversation_id: ConversationId) {
    client
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: gateway,
                state: RcsCapabilityState::Linked,
                availability: true,
                health: true,
                phone_responsive: true,
                observed_at_ms: 1_000,
                capability_revision: 1,
                feature_bits: 1,
            },
            &[RcsEligibilityProjection {
                conversation_id,
                eligibility: RcsEligibility::Eligible,
                feature_bits: 1,
                mapping_revision: 1,
                observed_at_ms: 1_000,
                gateway_device_id: gateway,
            }],
            1_000,
        )
        .unwrap();
}

#[test]
fn seals_opens_and_rejects_aad_tampering_without_durable_writes() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "ephemeral-sender", &vault);
    let receiver_config = config(&directory, "ephemeral-receiver", &vault);
    let sender = unlocked(&sender_config, &vault);
    let receiver = unlocked(&receiver_config, &vault);
    let conversation_id = ConversationId::new();
    enable_typing(&sender, sender_config.device_id, conversation_id);
    let sender_counts = table_counts(&sender_config);
    let receiver_counts = table_counts(&receiver_config);

    let signal = sender
        .seal_typing_signal(
            conversation_id,
            sender_config.device_id,
            TypingOrigin::VaultDevice,
            TypingState::Started,
            1_000,
        )
        .unwrap()
        .unwrap();
    let view = receiver
        .open_typing_signal(&signal, &[sender_config.device_id], 1_001)
        .unwrap();
    assert_eq!(
        view,
        Some(TypingView {
            conversation_id,
            state: TypingState::Started,
            origin: TypingOrigin::VaultDevice,
            expires_at_ms: 16_000,
        })
    );

    let mut altered = signal.clone();
    altered.sender_device_id = receiver_config.device_id;
    assert_eq!(
        receiver
            .open_typing_signal(&altered, &[sender_config.device_id], 1_001)
            .unwrap(),
        None
    );
    let mut tampered = signal.clone();
    let replacement = if tampered.ciphertext_b64.starts_with('A') {
        "B"
    } else {
        "A"
    };
    tampered.ciphertext_b64.replace_range(0..1, replacement);
    assert_eq!(
        receiver
            .open_typing_signal(&tampered, &[sender_config.device_id], 1_001)
            .unwrap(),
        None
    );
    assert_eq!(table_counts(&sender_config), sender_counts);
    assert_eq!(table_counts(&receiver_config), receiver_counts);
    assert!(
        sender
            .typing_signal_allowed(conversation_id, sender_config.device_id, 1_000)
            .unwrap()
    );
}

#[test]
fn quota_sequence_expiry_and_roster_are_memory_only() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "ephemeral-limits-sender", &vault);
    let receiver_config = config(&directory, "ephemeral-limits-receiver", &vault);
    let sender = unlocked(&sender_config, &vault);
    let receiver = unlocked(&receiver_config, &vault);
    let conversation_id = ConversationId::new();
    enable_typing(&sender, sender_config.device_id, conversation_id);

    let started = sender
        .seal_typing_signal(
            conversation_id,
            sender_config.device_id,
            TypingOrigin::GoogleContact,
            TypingState::Started,
            1_000,
        )
        .unwrap()
        .unwrap();
    assert!(
        sender
            .seal_typing_signal(
                conversation_id,
                sender_config.device_id,
                TypingOrigin::GoogleContact,
                TypingState::Started,
                1_001,
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(
        receiver.open_typing_signal(&started, &[], 1_001).unwrap(),
        None
    );
    assert!(
        receiver
            .open_typing_signal(&started, &[sender_config.device_id], 16_001)
            .unwrap()
            .is_none()
    );
    assert!(
        receiver
            .open_typing_signal(&started, &[sender_config.device_id], 1_001)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        receiver
            .open_typing_signal(&started, &[sender_config.device_id], 1_002)
            .unwrap(),
        None
    );

    let stopped = sender
        .seal_typing_signal(
            conversation_id,
            sender_config.device_id,
            TypingOrigin::GoogleContact,
            TypingState::Stopped,
            1_001,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        receiver
            .open_typing_signal(&stopped, &[sender_config.device_id], 1_002)
            .unwrap()
            .unwrap()
            .state,
        TypingState::Stopped
    );
}

#[test]
fn opens_matching_unlocked_old_epoch_after_cutover_and_discards_missing_epoch() {
    let directory = tempfile::tempdir().unwrap();
    let vault_v1 = Vault::new();
    let vault_v2 = Vault::epoch(vault_v1.id, 2);
    let sender_config = config(&directory, "ephemeral-epoch-sender", &vault_v1);
    let receiver_config = config(&directory, "ephemeral-epoch-receiver", &vault_v1);
    let locked_config = config(&directory, "ephemeral-epoch-locked", &vault_v1);
    let sender = unlocked(&sender_config, &vault_v1);
    let receiver = unlocked(&receiver_config, &vault_v1);
    let locked_receiver = unlocked(&locked_config, &vault_v1);
    let conversation_id = ConversationId::new();
    enable_typing(&sender, sender_config.device_id, conversation_id);

    let old_epoch = sender
        .seal_typing_signal(
            conversation_id,
            sender_config.device_id,
            TypingOrigin::VaultDevice,
            TypingState::Started,
            1_000,
        )
        .unwrap()
        .unwrap();
    sender
        .unlock(&vault_v2.profile, &vault_v2.header, PASSPHRASE)
        .unwrap();
    receiver
        .unlock(&vault_v2.profile, &vault_v2.header, PASSPHRASE)
        .unwrap();
    sender.activate_epoch(2).unwrap();
    receiver.activate_epoch(2).unwrap();

    assert!(
        receiver
            .open_typing_signal(&old_epoch, &[sender_config.device_id], 1_001)
            .unwrap()
            .is_some()
    );
    let new_epoch = sender
        .seal_typing_signal(
            conversation_id,
            sender_config.device_id,
            TypingOrigin::VaultDevice,
            TypingState::Started,
            4_001,
        )
        .unwrap()
        .unwrap();
    assert_eq!(new_epoch.key_epoch, 2);
    assert_eq!(
        locked_receiver
            .open_typing_signal(&new_epoch, &[sender_config.device_id], 4_002)
            .unwrap(),
        None
    );
}
