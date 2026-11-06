#[path = "common/mod.rs"]
mod common;

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use common::*;
use peppy_crypto::{EncryptedEnvelope, KeyPurpose, decrypt, derive_purpose_key, derive_root_key};
use peppy_protocol::{EphemeralSignalV2, TypingOriginV2, TypingStateV2};
use std::collections::BTreeMap;

fn publish_eligible(client: &Client, gateway: DeviceId, conversation_id: ConversationId) {
    publish_eligible_at(client, gateway, conversation_id, 1, 1_000);
}

fn publish_eligible_at(
    client: &Client,
    gateway: DeviceId,
    conversation_id: ConversationId,
    revision: u64,
    observed_at_ms: i64,
) {
    client
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: gateway,
                state: RcsCapabilityState::Linked,
                availability: true,
                health: true,
                phone_responsive: true,
                observed_at_ms,
                capability_revision: revision,
                feature_bits: 1,
            },
            &[RcsEligibilityProjection {
                conversation_id,
                eligibility: RcsEligibility::Eligible,
                feature_bits: 1,
                mapping_revision: revision,
                observed_at_ms,
                gateway_device_id: gateway,
            }],
            observed_at_ms,
        )
        .unwrap();
}

fn enable(client: &Client, device: DeviceId, clock: LiveClock) -> LiveEnableGeneration {
    client
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: vec![device],
                local_gateway_device_id: device,
                local_executor: LiveExecutor::CurrentGateway,
            },
            clock,
        )
        .unwrap()
}

fn seal(
    client: &Client,
    conversation: ConversationId,
    gateway: DeviceId,
    state: TypingStateV2,
    wall_ms: i64,
    mono_ms: u64,
) -> Option<LiveOutbound> {
    client
        .seal_typing_signal_v2(
            conversation,
            gateway,
            TypingOriginV2::VaultDevice,
            state,
            LiveClock { wall_ms, mono_ms },
        )
        .unwrap()
}

fn expected_v2_aad(signal: &EphemeralSignalV2, vault_id: VaultId) -> Vec<u8> {
    let mut aad = b"peppy-ephemeral-typing-v2\0".to_vec();
    aad.extend_from_slice(vault_id.0.as_bytes());
    aad.extend_from_slice(&2_u16.to_be_bytes());
    let kind = match signal.kind {
        peppy_protocol::EphemeralSignalV2Kind::Typing => "typing",
        peppy_protocol::EphemeralSignalV2Kind::Challenge => "challenge",
        peppy_protocol::EphemeralSignalV2Kind::SessionAck => "session_ack",
    };
    aad.push(kind.len() as u8);
    aad.extend_from_slice(kind.as_bytes());
    aad.extend_from_slice(signal.sender_device_id.0.as_bytes());
    aad.extend_from_slice(&signal.key_epoch.to_be_bytes());
    aad.extend_from_slice(&URL_SAFE_NO_PAD.decode(&signal.session_id).unwrap());
    aad.extend_from_slice(&signal.sent_at_ms.parse::<u64>().unwrap().to_be_bytes());
    aad.extend_from_slice(&signal.expires_at_ms.parse::<u64>().unwrap().to_be_bytes());
    aad.extend_from_slice(&signal.sequence.parse::<u64>().unwrap().to_be_bytes());
    aad
}

fn persisted_rows(config: &ClientConfig) -> BTreeMap<String, Vec<Vec<String>>> {
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
            let columns: Vec<String> = connection
                .prepare(&format!("PRAGMA table_info([{table}])"))
                .unwrap()
                .query_map([], |row| row.get(1))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let query = format!(
                "SELECT {} FROM [{table}]",
                columns
                    .iter()
                    .map(|column| format!("quote([{column}])"))
                    .collect::<Vec<_>>()
                    .join(",")
            );
            let rows = connection
                .prepare(&query)
                .unwrap()
                .query_map([], |row| {
                    (0..columns.len())
                        .map(|index| row.get(index))
                        .collect::<Result<Vec<String>, _>>()
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            (table, rows)
        })
        .collect()
}

#[test]
fn v2_is_disabled_until_a_trusted_context_enables_and_then_seals_a_real_frame() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-runtime", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&client, config.device_id, conversation);
    let clock = LiveClock {
        wall_ms: 1_000,
        mono_ms: 1_000,
    };
    assert_eq!(
        client
            .seal_typing_signal_v2(
                conversation,
                config.device_id,
                TypingOriginV2::VaultDevice,
                TypingStateV2::Started,
                clock,
            )
            .unwrap_err(),
        LiveError::Disabled
    );
    let generation = client
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: vec![config.device_id],
                local_gateway_device_id: config.device_id,
                local_executor: LiveExecutor::CurrentGateway,
            },
            clock,
        )
        .unwrap();
    let outbound = client
        .seal_typing_signal_v2(
            conversation,
            config.device_id,
            TypingOriginV2::VaultDevice,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: 1_001,
                mono_ms: 1_001,
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(outbound.generation, generation);
    assert_eq!(outbound.signal.sequence, "1");
    assert_eq!(outbound.signal.session_id.len(), 22);
    assert!(!outbound.signal.ciphertext_b64.is_empty());
}

#[test]
fn authenticated_two_client_typing_reaches_a_tagged_gateway_intent() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "v2-sender", &vault);
    let receiver_config = config(&directory, "v2-receiver", &vault);
    let sender = unlocked(&sender_config, &vault);
    let receiver = unlocked(&receiver_config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&receiver, receiver_config.device_id, conversation);
    let mut server = Server::default();
    server.upload(&receiver);
    server.sync(&sender);
    let roster = vec![sender_config.device_id, receiver_config.device_id];
    sender
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: roster.clone(),
                local_gateway_device_id: sender_config.device_id,
                local_executor: LiveExecutor::None,
            },
            LiveClock {
                wall_ms: 1_000,
                mono_ms: 1_000,
            },
        )
        .unwrap();
    receiver
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: roster,
                local_gateway_device_id: receiver_config.device_id,
                local_executor: LiveExecutor::CurrentGateway,
            },
            LiveClock {
                wall_ms: 1_000,
                mono_ms: 1_000,
            },
        )
        .unwrap();

    let typing = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        1_001,
        1_001,
    )
    .unwrap();
    let challenge = receiver
        .open_typing_signal_v2(
            &typing.signal,
            LiveClock {
                wall_ms: 1_001,
                mono_ms: 1_001,
            },
        )
        .unwrap()
        .reply
        .unwrap();
    let acknowledgement = sender
        .open_typing_signal_v2(
            &challenge,
            LiveClock {
                wall_ms: 1_002,
                mono_ms: 1_002,
            },
        )
        .unwrap()
        .reply
        .unwrap();
    let bytes = STANDARD.decode(&acknowledgement.ciphertext_b64).unwrap();
    let nonce: [u8; 24] = bytes[..24].try_into().unwrap();
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    let acknowledgement_plaintext: peppy_protocol::SessionAckPlaintextV2 = serde_json::from_slice(
        &decrypt(
            &key,
            &expected_v2_aad(&acknowledgement, vault.id),
            &EncryptedEnvelope {
                nonce,
                ciphertext: bytes[24..].to_vec(),
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        acknowledgement_plaintext.sequence_floor,
        acknowledgement.sequence
    );
    assert!(
        receiver
            .open_typing_signal_v2(
                &acknowledgement,
                LiveClock {
                    wall_ms: 1_003,
                    mono_ms: 1_003
                },
            )
            .unwrap()
            .session_update
            .is_some()
    );

    let typing = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        4_001,
        4_001,
    )
    .unwrap();
    assert!(matches!(
        receiver
            .open_typing_signal_v2(&typing.signal, LiveClock { wall_ms: 4_001, mono_ms: 4_001 })
            .unwrap()
            .action,
        Some(LiveAction::GatewayTypingIntent { conversation_id, state: TypingStateV2::Started, .. }) if conversation_id == conversation
    ));
    let renewal = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        12_000,
        12_000,
    )
    .unwrap();
    let outcome = receiver
        .open_typing_signal_v2(
            &renewal.signal,
            LiveClock {
                wall_ms: 12_000,
                mono_ms: 12_000,
            },
        )
        .unwrap();
    assert!(matches!(
        outcome.action,
        Some(LiveAction::GatewayTypingIntent { .. })
    ));
    assert!(outcome.reply.is_some());
}

#[test]
fn disabled_sharing_preserves_outbound_clock_and_sequence_state() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-sharing", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&client, config.device_id, conversation);
    enable(
        &client,
        config.device_id,
        LiveClock {
            wall_ms: 1_000,
            mono_ms: 1_000,
        },
    );

    client.set_typing_sharing(false).unwrap();
    assert_eq!(
        seal(
            &client,
            conversation,
            config.device_id,
            TypingStateV2::Started,
            -1,
            999,
        ),
        None
    );
    client.set_typing_sharing(true).unwrap();
    assert_eq!(
        seal(
            &client,
            conversation,
            config.device_id,
            TypingStateV2::Started,
            1_001,
            1_001,
        )
        .unwrap()
        .signal
        .sequence,
        "1"
    );
}

#[test]
fn google_contact_typing_displays_only_for_the_current_gateway() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "v2-google-gateway", &vault);
    let desktop_config = config(&directory, "v2-google-desktop", &vault);
    let other_config = config(&directory, "v2-google-other", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let other = unlocked(&other_config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&gateway, gateway_config.device_id, conversation);
    publish_eligible(&other, other_config.device_id, conversation);
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    let roster = vec![
        gateway_config.device_id,
        desktop_config.device_id,
        other_config.device_id,
    ];
    for (client, device, executor) in [
        (
            &gateway,
            gateway_config.device_id,
            LiveExecutor::CurrentGateway,
        ),
        (&other, other_config.device_id, LiveExecutor::CurrentGateway),
    ] {
        client
            .enable_live_typing(
                LiveEnableContext {
                    trusted_roster: roster.clone(),
                    local_gateway_device_id: device,
                    local_executor: executor,
                },
                LiveClock {
                    wall_ms: 1_000,
                    mono_ms: 1_000,
                },
            )
            .unwrap();
    }
    desktop
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: roster,
                local_gateway_device_id: gateway_config.device_id,
                local_executor: LiveExecutor::None,
            },
            LiveClock {
                wall_ms: 1_000,
                mono_ms: 1_000,
            },
        )
        .unwrap();
    let first = gateway
        .seal_typing_signal_v2(
            conversation,
            gateway_config.device_id,
            TypingOriginV2::GoogleContact,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: 1_001,
                mono_ms: 1_001,
            },
        )
        .unwrap()
        .unwrap();
    let challenge = desktop
        .open_typing_signal_v2(
            &first.signal,
            LiveClock {
                wall_ms: 1_001,
                mono_ms: 1_001,
            },
        )
        .unwrap()
        .reply
        .unwrap();
    let acknowledgement = gateway
        .open_typing_signal_v2(
            &challenge,
            LiveClock {
                wall_ms: 1_002,
                mono_ms: 1_002,
            },
        )
        .unwrap()
        .reply
        .unwrap();
    desktop
        .open_typing_signal_v2(
            &acknowledgement,
            LiveClock {
                wall_ms: 1_003,
                mono_ms: 1_003,
            },
        )
        .unwrap();
    let displayed = gateway
        .seal_typing_signal_v2(
            conversation,
            gateway_config.device_id,
            TypingOriginV2::GoogleContact,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: 4_001,
                mono_ms: 4_001,
            },
        )
        .unwrap()
        .unwrap();
    assert!(matches!(
        desktop
            .open_typing_signal_v2(
                &displayed.signal,
                LiveClock {
                    wall_ms: 4_001,
                    mono_ms: 4_001
                }
            )
            .unwrap()
            .action,
        Some(LiveAction::Display { .. })
    ));
    let wrong_gateway = other
        .seal_typing_signal_v2(
            conversation,
            other_config.device_id,
            TypingOriginV2::GoogleContact,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: 1_001,
                mono_ms: 1_001,
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        desktop
            .open_typing_signal_v2(
                &wrong_gateway.signal,
                LiveClock {
                    wall_ms: 4_002,
                    mono_ms: 4_002
                }
            )
            .unwrap()
            .action,
        None
    );
}

#[test]
fn old_unlocked_epoch_frames_survive_cutover_and_unknown_epochs_do_not_mutate_receive_state() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let next_epoch = Vault::epoch(vault.id, 2);
    let sender_config = config(&directory, "v2-old-epoch-sender", &vault);
    let receiver_config = config(&directory, "v2-old-epoch-receiver", &vault);
    let sender = unlocked(&sender_config, &vault);
    let receiver = unlocked(&receiver_config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&receiver, receiver_config.device_id, conversation);
    let mut server = Server::default();
    server.upload(&receiver);
    server.sync(&sender);
    let roster = vec![sender_config.device_id, receiver_config.device_id];
    sender
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: roster.clone(),
                local_gateway_device_id: sender_config.device_id,
                local_executor: LiveExecutor::None,
            },
            LiveClock {
                wall_ms: 1_000,
                mono_ms: 1_000,
            },
        )
        .unwrap();
    receiver
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: roster,
                local_gateway_device_id: receiver_config.device_id,
                local_executor: LiveExecutor::CurrentGateway,
            },
            LiveClock {
                wall_ms: 1_000,
                mono_ms: 1_000,
            },
        )
        .unwrap();
    let initial = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        1_001,
        1_001,
    )
    .unwrap();
    let challenge = receiver
        .open_typing_signal_v2(
            &initial.signal,
            LiveClock {
                wall_ms: 1_001,
                mono_ms: 1_001,
            },
        )
        .unwrap()
        .reply
        .unwrap();
    let acknowledgement = sender
        .open_typing_signal_v2(
            &challenge,
            LiveClock {
                wall_ms: 1_002,
                mono_ms: 1_002,
            },
        )
        .unwrap()
        .reply
        .unwrap();
    receiver
        .open_typing_signal_v2(
            &acknowledgement,
            LiveClock {
                wall_ms: 1_003,
                mono_ms: 1_003,
            },
        )
        .unwrap();
    let old_epoch = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        4_001,
        4_001,
    )
    .unwrap();
    sender
        .unlock(&next_epoch.profile, &next_epoch.header, PASSPHRASE)
        .unwrap();
    receiver
        .unlock(&next_epoch.profile, &next_epoch.header, PASSPHRASE)
        .unwrap();
    sender.activate_epoch(2).unwrap();
    receiver.activate_epoch(2).unwrap();
    let mut unknown_epoch = old_epoch.signal.clone();
    unknown_epoch.key_epoch = 99;
    assert_eq!(
        receiver
            .open_typing_signal_v2(
                &unknown_epoch,
                LiveClock {
                    wall_ms: 4_001,
                    mono_ms: 4_001
                }
            )
            .unwrap()
            .action,
        None
    );
    assert!(matches!(
        receiver
            .open_typing_signal_v2(
                &old_epoch.signal,
                LiveClock {
                    wall_ms: 4_002,
                    mono_ms: 4_002
                }
            )
            .unwrap()
            .action,
        Some(LiveAction::GatewayTypingIntent { .. })
    ));
}

#[test]
fn hostile_v2_headers_drop_without_retiring_a_valid_roster_receiver() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "v2-hostile-sender", &vault);
    let receiver_config = config(&directory, "v2-hostile-receiver", &vault);
    let sender = unlocked(&sender_config, &vault);
    let receiver = unlocked(&receiver_config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&receiver, receiver_config.device_id, conversation);
    let mut server = Server::default();
    server.upload(&receiver);
    server.sync(&sender);
    let altered_sender = DeviceId::new();
    let roster = vec![
        sender_config.device_id,
        receiver_config.device_id,
        altered_sender,
    ];
    sender
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: roster.clone(),
                local_gateway_device_id: sender_config.device_id,
                local_executor: LiveExecutor::None,
            },
            LiveClock {
                wall_ms: 1_000,
                mono_ms: 1_000,
            },
        )
        .unwrap();
    receiver
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: roster,
                local_gateway_device_id: receiver_config.device_id,
                local_executor: LiveExecutor::CurrentGateway,
            },
            LiveClock {
                wall_ms: 1_000,
                mono_ms: 1_000,
            },
        )
        .unwrap();
    let valid = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        1_001,
        1_001,
    )
    .unwrap()
    .signal;

    let mut aad_tampered = valid.clone();
    aad_tampered.sender_device_id = altered_sender;
    assert_eq!(
        receiver
            .open_typing_signal_v2(
                &aad_tampered,
                LiveClock {
                    wall_ms: 1_001,
                    mono_ms: 1_001
                }
            )
            .unwrap()
            .reply,
        None
    );
    for (sent_at_ms, expires_at_ms) in [
        ("0", "1"),
        ("2000", "2001"),
        ("18446744073709551614", "18446744073709551615"),
    ] {
        let mut malformed = valid.clone();
        malformed.sent_at_ms = sent_at_ms.into();
        malformed.expires_at_ms = expires_at_ms.into();
        assert_eq!(
            receiver
                .open_typing_signal_v2(
                    &malformed,
                    LiveClock {
                        wall_ms: 1_001,
                        mono_ms: 1_002
                    }
                )
                .unwrap()
                .reply,
            None
        );
    }
    assert_eq!(
        receiver.open_typing_signal_v2(
            &valid,
            LiveClock {
                wall_ms: -1,
                mono_ms: 1_003
            }
        ),
        Err(LiveError::Clock)
    );
    assert!(
        receiver
            .open_typing_signal_v2(
                &valid,
                LiveClock {
                    wall_ms: 1_001,
                    mono_ms: 1_004
                }
            )
            .unwrap()
            .reply
            .is_some()
    );
}

#[test]
fn v2_rejects_empty_host_roster() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-empty-roster", &vault);
    let client = unlocked(&config, &vault);
    assert_eq!(
        client
            .enable_live_typing(
                LiveEnableContext {
                    trusted_roster: vec![],
                    local_gateway_device_id: config.device_id,
                    local_executor: LiveExecutor::CurrentGateway,
                },
                LiveClock {
                    wall_ms: 1_000,
                    mono_ms: 1_000,
                },
            )
            .unwrap_err(),
        LiveError::InvalidContext
    );
}

#[test]
fn eligibility_changes_refuse_without_consuming_the_next_sequence() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-eligibility", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&client, config.device_id, conversation);
    enable(
        &client,
        config.device_id,
        LiveClock {
            wall_ms: 1_000,
            mono_ms: 1_000,
        },
    );
    assert_eq!(
        seal(
            &client,
            conversation,
            config.device_id,
            TypingStateV2::Started,
            1_001,
            1_001
        )
        .unwrap()
        .signal
        .sequence,
        "1"
    );
    client
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: config.device_id,
                state: RcsCapabilityState::Unlinked,
                availability: false,
                health: false,
                phone_responsive: false,
                observed_at_ms: 1_002,
                capability_revision: 2,
                feature_bits: 0,
            },
            &[RcsEligibilityProjection {
                conversation_id: conversation,
                eligibility: RcsEligibility::NotEligible,
                feature_bits: 0,
                mapping_revision: 2,
                observed_at_ms: 1_002,
                gateway_device_id: config.device_id,
            }],
            1_002,
        )
        .unwrap();
    assert_eq!(
        seal(
            &client,
            conversation,
            config.device_id,
            TypingStateV2::Stopped,
            1_002,
            1_002
        ),
        None
    );
    publish_eligible_at(&client, config.device_id, conversation, 3, 1_003);
    assert_eq!(
        seal(
            &client,
            conversation,
            config.device_id,
            TypingStateV2::Stopped,
            1_003,
            1_003
        )
        .unwrap()
        .signal
        .sequence,
        "2"
    );
}

#[test]
fn invalid_context_and_clock_do_not_create_outbound_authority() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-context", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&client, config.device_id, conversation);
    client
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: vec![config.device_id],
                local_gateway_device_id: DeviceId::new(),
                local_executor: LiveExecutor::None,
            },
            LiveClock {
                wall_ms: 1_000,
                mono_ms: 1_000,
            },
        )
        .unwrap();
    assert_eq!(
        client.seal_typing_signal_v2(
            conversation,
            DeviceId::new(),
            TypingOriginV2::VaultDevice,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: 1_001,
                mono_ms: 1_001
            }
        ),
        Err(LiveError::InvalidContext)
    );
    assert_eq!(
        client.seal_typing_signal_v2(
            conversation,
            config.device_id,
            TypingOriginV2::GoogleContact,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: 1_001,
                mono_ms: 1_001
            }
        ),
        Err(LiveError::InvalidContext)
    );
    assert_eq!(
        client.seal_typing_signal_v2(
            conversation,
            config.device_id,
            TypingOriginV2::VaultDevice,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: -1,
                mono_ms: 1_001
            }
        ),
        Err(LiveError::Clock)
    );
    assert_eq!(
        seal(
            &client,
            conversation,
            config.device_id,
            TypingStateV2::Started,
            1_001,
            1_001
        )
        .unwrap()
        .signal
        .sequence,
        "1"
    );
}

#[test]
fn frozen_wall_cannot_prolong_eligibility_and_clock_errors_precede_missing_gates() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-effective-wall", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&client, config.device_id, conversation);
    enable(
        &client,
        config.device_id,
        LiveClock {
            wall_ms: 1_000,
            mono_ms: 1_000,
        },
    );
    seal(
        &client,
        conversation,
        config.device_id,
        TypingStateV2::Started,
        1_001,
        1_001,
    )
    .unwrap();
    assert_eq!(
        seal(
            &client,
            conversation,
            config.device_id,
            TypingStateV2::Started,
            1_000,
            1_801_001
        ),
        None
    );
    assert_eq!(
        client.seal_typing_signal_v2(
            ConversationId::new(),
            config.device_id,
            TypingOriginV2::VaultDevice,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: -1,
                mono_ms: 1_801_002
            }
        ),
        Err(LiveError::Clock)
    );
    assert_eq!(
        client.seal_typing_signal_v2(
            ConversationId::new(),
            config.device_id,
            TypingOriginV2::VaultDevice,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: 1_000,
                mono_ms: 1_000,
            },
        ),
        Err(LiveError::Clock)
    );
}

#[test]
fn global_reservations_keep_stops_available_without_consuming_suppressed_sequences() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-budget", &vault);
    let client = unlocked(&config, &vault);
    let conversations: Vec<_> = (0..26).map(|_| ConversationId::new()).collect();
    for conversation in &conversations {
        publish_eligible(&client, config.device_id, *conversation);
    }
    enable(
        &client,
        config.device_id,
        LiveClock {
            wall_ms: 1_000,
            mono_ms: 1_000,
        },
    );
    for (index, conversation) in conversations[..25].iter().enumerate() {
        let batch = index / 5;
        let frame = seal(
            &client,
            *conversation,
            config.device_id,
            TypingStateV2::Started,
            1_001 + batch as i64 * 1_000,
            1_001 + batch as u64 * 1_000,
        )
        .unwrap();
        assert_eq!(frame.signal.sequence, (index + 1).to_string());
    }
    assert_eq!(
        seal(
            &client,
            conversations[25],
            config.device_id,
            TypingStateV2::Started,
            5_001,
            5_001
        ),
        None
    );
    assert_eq!(
        seal(
            &client,
            conversations[0],
            config.device_id,
            TypingStateV2::Stopped,
            5_002,
            5_002
        )
        .unwrap()
        .signal
        .sequence,
        "26"
    );
    assert_eq!(
        seal(
            &client,
            conversations[0],
            config.device_id,
            TypingStateV2::Stopped,
            5_003,
            5_003
        ),
        None
    );
}

#[test]
fn no_frame_admission_leaves_the_clock_and_sequence_candidate_uncommitted() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-no-frame-candidate", &vault);
    let client = unlocked(&config, &vault);
    let first = ConversationId::new();
    let second = ConversationId::new();
    publish_eligible(&client, config.device_id, first);
    publish_eligible(&client, config.device_id, second);
    enable(
        &client,
        config.device_id,
        LiveClock {
            wall_ms: 1_000,
            mono_ms: 1_000,
        },
    );
    assert_eq!(
        seal(
            &client,
            first,
            config.device_id,
            TypingStateV2::Started,
            1_001,
            1_001
        )
        .unwrap()
        .signal
        .sequence,
        "1"
    );
    assert_eq!(
        seal(
            &client,
            first,
            config.device_id,
            TypingStateV2::Stopped,
            1_002,
            1_002
        )
        .unwrap()
        .signal
        .sequence,
        "2"
    );
    assert_eq!(
        seal(
            &client,
            first,
            config.device_id,
            TypingStateV2::Stopped,
            1_003,
            1_003
        ),
        None
    );
    assert_eq!(
        seal(
            &client,
            second,
            config.device_id,
            TypingStateV2::Started,
            1_002,
            1_002
        )
        .unwrap()
        .signal
        .sequence,
        "3"
    );
}

#[test]
fn v2_memory_operations_do_not_change_any_sqlite_rows() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-rows", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&client, config.device_id, conversation);
    let before = persisted_rows(&config);
    enable(
        &client,
        config.device_id,
        LiveClock {
            wall_ms: 1_000,
            mono_ms: 1_000,
        },
    );
    seal(
        &client,
        conversation,
        config.device_id,
        TypingStateV2::Started,
        1_001,
        1_001,
    )
    .unwrap();
    seal(
        &client,
        conversation,
        config.device_id,
        TypingStateV2::Stopped,
        1_002,
        1_002,
    )
    .unwrap();
    client.disable_live_typing().unwrap();
    assert_eq!(persisted_rows(&config), before);
}

#[test]
fn v2_frame_decrypts_with_its_independent_canonical_aad() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-aad", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&client, config.device_id, conversation);
    enable(
        &client,
        config.device_id,
        LiveClock {
            wall_ms: 1_000,
            mono_ms: 1_000,
        },
    );
    let outbound = seal(
        &client,
        conversation,
        config.device_id,
        TypingStateV2::Started,
        1_001,
        1_001,
    )
    .unwrap();
    let bytes = STANDARD.decode(&outbound.signal.ciphertext_b64).unwrap();
    let nonce: [u8; 24] = bytes[..24].try_into().unwrap();
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    let plaintext = decrypt(
        &key,
        &expected_v2_aad(&outbound.signal, vault.id),
        &EncryptedEnvelope {
            nonce,
            ciphertext: bytes[24..].to_vec(),
        },
    )
    .unwrap();
    let value: peppy_protocol::TypingPlaintextV2 = serde_json::from_slice(&plaintext).unwrap();
    assert_eq!(value.conversation_id, conversation.0);
    assert_eq!(value.gateway_device_id, Some(config.device_id));
}

#[test]
fn epoch_cutover_preserves_v2_session_counter_and_reenable_rotates_both() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let next_epoch = Vault::epoch(vault.id, 2);
    let config = config(&directory, "v2-cutover", &vault);
    let client = unlocked(&config, &vault);
    let first_conversation = ConversationId::new();
    let second_conversation = ConversationId::new();
    publish_eligible(&client, config.device_id, first_conversation);
    publish_eligible(&client, config.device_id, second_conversation);
    let first_generation = enable(
        &client,
        config.device_id,
        LiveClock {
            wall_ms: 1_000,
            mono_ms: 1_000,
        },
    );
    let first = seal(
        &client,
        first_conversation,
        config.device_id,
        TypingStateV2::Started,
        1_001,
        1_001,
    )
    .unwrap();
    client
        .unlock(&next_epoch.profile, &next_epoch.header, PASSPHRASE)
        .unwrap();
    client.activate_epoch(2).unwrap();
    let second = seal(
        &client,
        second_conversation,
        config.device_id,
        TypingStateV2::Started,
        1_002,
        1_002,
    )
    .unwrap();
    assert_eq!(second.signal.session_id, first.signal.session_id);
    assert_eq!(second.signal.sequence, "2");
    assert_eq!(second.signal.key_epoch, 2);
    client.disable_live_typing().unwrap();
    let second_generation = enable(
        &client,
        config.device_id,
        LiveClock {
            wall_ms: 1_003,
            mono_ms: 1_003,
        },
    );
    let rotated = seal(
        &client,
        first_conversation,
        config.device_id,
        TypingStateV2::Started,
        1_004,
        1_004,
    )
    .unwrap();
    assert_ne!(rotated.signal.session_id, first.signal.session_id);
    assert_ne!(second_generation, first_generation);
    assert_eq!(rotated.signal.sequence, "1");
}

#[test]
fn remote_gateway_capability_reaches_an_ordinary_desktop_before_vault_sealing() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "v2-remote-gateway", &vault);
    let desktop_config = config(&directory, "v2-remote-desktop", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&gateway, gateway_config.device_id, conversation);
    let mut server = Server::default();
    assert_eq!(server.upload(&gateway), 2);
    server.sync(&desktop);
    assert_eq!(
        desktop
            .rcs_eligibility_projection(conversation, gateway_config.device_id, 1_000)
            .unwrap()
            .unwrap()
            .eligibility,
        RcsEligibility::Eligible
    );
    let before = persisted_rows(&desktop_config);
    desktop
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: vec![desktop_config.device_id, gateway_config.device_id],
                local_gateway_device_id: gateway_config.device_id,
                local_executor: LiveExecutor::None,
            },
            LiveClock {
                wall_ms: 1_000,
                mono_ms: 1_000,
            },
        )
        .unwrap();
    let outbound = desktop
        .seal_typing_signal_v2(
            conversation,
            gateway_config.device_id,
            TypingOriginV2::VaultDevice,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: 1_001,
                mono_ms: 1_001,
            },
        )
        .unwrap()
        .unwrap();
    let bytes = STANDARD.decode(&outbound.signal.ciphertext_b64).unwrap();
    let nonce: [u8; 24] = bytes[..24].try_into().unwrap();
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    let plaintext = decrypt(
        &key,
        &expected_v2_aad(&outbound.signal, vault.id),
        &EncryptedEnvelope {
            nonce,
            ciphertext: bytes[24..].to_vec(),
        },
    )
    .unwrap();
    let value: peppy_protocol::TypingPlaintextV2 = serde_json::from_slice(&plaintext).unwrap();
    assert_eq!(value.origin, TypingOriginV2::VaultDevice);
    assert_eq!(value.gateway_device_id, Some(gateway_config.device_id));
    assert_eq!(
        desktop.seal_typing_signal_v2(
            conversation,
            desktop_config.device_id,
            TypingOriginV2::GoogleContact,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: 1_002,
                mono_ms: 1_002,
            },
        ),
        Err(LiveError::InvalidContext)
    );
    assert_eq!(
        desktop.seal_typing_signal_v2(
            conversation,
            DeviceId::new(),
            TypingOriginV2::VaultDevice,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: 1_002,
                mono_ms: 1_002,
            },
        ),
        Err(LiveError::InvalidContext)
    );
    desktop.disable_live_typing().unwrap();
    desktop
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: vec![desktop_config.device_id],
                local_gateway_device_id: desktop_config.device_id,
                local_executor: LiveExecutor::None,
            },
            LiveClock {
                wall_ms: 1_002,
                mono_ms: 1_002,
            },
        )
        .unwrap();
    assert_eq!(
        desktop.seal_typing_signal_v2(
            conversation,
            gateway_config.device_id,
            TypingOriginV2::VaultDevice,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: 1_003,
                mono_ms: 1_003,
            },
        ),
        Err(LiveError::InvalidContext)
    );
    assert_eq!(persisted_rows(&desktop_config), before);
}
