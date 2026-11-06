#[path = "common/mod.rs"]
mod common;

use common::*;
use peppy_protocol::{TypingOriginV2, TypingStateV2};
use std::collections::BTreeMap;

fn publish_eligible(client: &Client, gateway: DeviceId, conversation_id: ConversationId) {
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

fn enable_pair(sender: &Client, receiver: &Client, sender_id: DeviceId, receiver_id: DeviceId) {
    let roster = vec![sender_id, receiver_id];
    sender
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: roster.clone(),
                local_gateway_device_id: sender_id,
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
                local_gateway_device_id: receiver_id,
                local_executor: LiveExecutor::CurrentGateway,
            },
            LiveClock {
                wall_ms: 1_000,
                mono_ms: 1_000,
            },
        )
        .unwrap();
}

fn seal(
    client: &Client,
    conversation: ConversationId,
    gateway: DeviceId,
    state: TypingStateV2,
    time: u64,
) -> LiveOutbound {
    client
        .seal_typing_signal_v2(
            conversation,
            gateway,
            TypingOriginV2::VaultDevice,
            state,
            LiveClock {
                wall_ms: time as i64,
                mono_ms: time,
            },
        )
        .unwrap()
        .unwrap()
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
fn delayed_same_session_ack_preserves_the_consumed_floor() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "delayed-sender", &vault);
    let receiver_config = config(&directory, "delayed-receiver", &vault);
    let sender = unlocked(&sender_config, &vault);
    let receiver = unlocked(&receiver_config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&receiver, receiver_config.device_id, conversation);
    let mut server = Server::default();
    server.upload(&receiver);
    server.sync(&sender);
    enable_pair(
        &sender,
        &receiver,
        sender_config.device_id,
        receiver_config.device_id,
    );

    let floor = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        1_001,
    );
    let challenge = receiver
        .open_typing_signal_v2(
            &floor.signal,
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
    let initial_update = receiver
        .open_typing_signal_v2(
            &acknowledgement,
            LiveClock {
                wall_ms: 1_002,
                mono_ms: 1_002,
            },
        )
        .unwrap()
        .session_update
        .unwrap();
    let post_floor_start = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        4_001,
    );
    let action = receiver
        .open_typing_signal_v2(
            &post_floor_start.signal,
            LiveClock {
                wall_ms: 4_001,
                mono_ms: 4_001,
            },
        )
        .unwrap()
        .action
        .unwrap();
    assert!(
        matches!(action, LiveAction::GatewayTypingIntent { session_token, .. } if session_token == initial_update.current_session_token)
    );
    let renewal = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        11_002,
    );
    let renewal_challenge = receiver
        .open_typing_signal_v2(
            &renewal.signal,
            LiveClock {
                wall_ms: 11_002,
                mono_ms: 11_002,
            },
        )
        .unwrap()
        .reply
        .unwrap();
    let post_floor_stop = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Stopped,
        11_003,
    );
    let stop_action = receiver
        .open_typing_signal_v2(
            &post_floor_stop.signal,
            LiveClock {
                wall_ms: 11_003,
                mono_ms: 11_003,
            },
        )
        .unwrap()
        .action
        .unwrap();
    assert!(
        matches!(stop_action, LiveAction::GatewayTypingIntent { session_token, .. } if session_token == initial_update.current_session_token)
    );
    let delayed_ack = sender
        .open_typing_signal_v2(
            &renewal_challenge,
            LiveClock {
                wall_ms: 11_003,
                mono_ms: 11_003,
            },
        )
        .unwrap()
        .reply
        .unwrap();
    let accepted = receiver
        .open_typing_signal_v2(
            &delayed_ack,
            LiveClock {
                wall_ms: 16_003,
                mono_ms: 16_003,
            },
        )
        .unwrap();
    assert_eq!(accepted.session_update, None);
    let replay = receiver
        .open_typing_signal_v2(
            &post_floor_start.signal,
            LiveClock {
                wall_ms: 16_004,
                mono_ms: 16_004,
            },
        )
        .unwrap();
    assert!(replay.action.is_none());
    assert_eq!(replay.session_update, None);
}

#[test]
fn replacement_ack_retires_actions_from_the_old_session() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "replacement-sender", &vault);
    let receiver_config = config(&directory, "replacement-receiver", &vault);
    let sender = unlocked(&sender_config, &vault);
    let receiver = unlocked(&receiver_config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&receiver, receiver_config.device_id, conversation);
    let mut server = Server::default();
    server.upload(&receiver);
    server.sync(&sender);
    enable_pair(
        &sender,
        &receiver,
        sender_config.device_id,
        receiver_config.device_id,
    );

    let first = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        1_001,
    );
    let challenge = receiver
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
    let acknowledgement = sender
        .open_typing_signal_v2(
            &challenge,
            LiveClock {
                wall_ms: 4_001,
                mono_ms: 4_001,
            },
        )
        .unwrap()
        .reply
        .unwrap();
    let first_update = receiver
        .open_typing_signal_v2(
            &acknowledgement,
            LiveClock {
                wall_ms: 4_002,
                mono_ms: 4_002,
            },
        )
        .unwrap()
        .session_update
        .unwrap();
    let old_action = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        4_003,
    );
    assert!(matches!(
        receiver
            .open_typing_signal_v2(
                &old_action.signal,
                LiveClock {
                    wall_ms: 4_003,
                    mono_ms: 4_003,
                },
            )
            .unwrap()
            .action,
        Some(LiveAction::GatewayTypingIntent { session_token, .. }) if session_token == first_update.current_session_token
    ));
    let stale = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Stopped,
        4_004,
    );

    sender.disable_live_typing().unwrap();
    sender
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: vec![sender_config.device_id, receiver_config.device_id],
                local_gateway_device_id: sender_config.device_id,
                local_executor: LiveExecutor::None,
            },
            LiveClock {
                wall_ms: 5_000,
                mono_ms: 5_000,
            },
        )
        .unwrap();
    let replacement = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        5_001,
    );
    let challenge = receiver
        .open_typing_signal_v2(
            &replacement.signal,
            LiveClock {
                wall_ms: 5_001,
                mono_ms: 5_001,
            },
        )
        .unwrap()
        .reply
        .unwrap();
    let acknowledgement = sender
        .open_typing_signal_v2(
            &challenge,
            LiveClock {
                wall_ms: 5_002,
                mono_ms: 5_002,
            },
        )
        .unwrap()
        .reply
        .unwrap();
    let replacement_update = receiver
        .open_typing_signal_v2(
            &acknowledgement,
            LiveClock {
                wall_ms: 5_003,
                mono_ms: 5_003,
            },
        )
        .unwrap()
        .session_update
        .unwrap();
    assert_eq!(
        replacement_update.replaced_session_token,
        Some(first_update.current_session_token)
    );
    assert_ne!(
        replacement_update.current_session_token,
        first_update.current_session_token
    );
    assert_eq!(
        replacement_update.validation_revision,
        first_update.validation_revision + 1
    );
    assert!(
        receiver
            .open_typing_signal_v2(
                &stale.signal,
                LiveClock {
                    wall_ms: 5_004,
                    mono_ms: 5_004
                }
            )
            .unwrap()
            .action
            .is_none()
    );
}

#[test]
fn ingress_and_challenge_budgets_recover_after_the_window() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "budget-sender", &vault);
    let receiver_config = config(&directory, "budget-receiver", &vault);
    let sender = unlocked(&sender_config, &vault);
    let receiver = unlocked(&receiver_config, &vault);
    let conversations: Vec<_> = (0..5).map(|_| ConversationId::new()).collect();
    for conversation in &conversations {
        publish_eligible(&receiver, receiver_config.device_id, *conversation);
    }
    let mut server = Server::default();
    server.upload(&receiver);
    server.sync(&sender);
    enable_pair(
        &sender,
        &receiver,
        sender_config.device_id,
        receiver_config.device_id,
    );
    let bootstrap = seal(
        &sender,
        conversations[0],
        receiver_config.device_id,
        TypingStateV2::Started,
        1_001,
    );
    let challenge = receiver
        .open_typing_signal_v2(
            &bootstrap.signal,
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
    assert!(
        receiver
            .open_typing_signal_v2(
                &acknowledgement,
                LiveClock {
                    wall_ms: 1_003,
                    mono_ms: 1_003,
                },
            )
            .unwrap()
            .session_update
            .is_some()
    );

    let frames: Vec<_> = conversations
        .iter()
        .map(|conversation| {
            seal(
                &sender,
                *conversation,
                receiver_config.device_id,
                TypingStateV2::Started,
                4_001,
            )
        })
        .collect();
    for frame in &frames[..4] {
        let outcome = receiver
            .open_typing_signal_v2(
                &frame.signal,
                LiveClock {
                    wall_ms: 4_001,
                    mono_ms: 4_001,
                },
            )
            .unwrap();
        assert!(outcome.action.is_some());
    }
    let excess = receiver
        .open_typing_signal_v2(
            &frames[4].signal,
            LiveClock {
                wall_ms: 4_001,
                mono_ms: 4_001,
            },
        )
        .unwrap();
    assert!(excess.action.is_none() && excess.reply.is_none() && excess.session_update.is_none());

    let recovered = receiver
        .open_typing_signal_v2(
            &frames[4].signal,
            LiveClock {
                wall_ms: 5_001,
                mono_ms: 5_001,
            },
        )
        .unwrap();
    assert!(recovered.action.is_some() && recovered.session_update.is_none());
}

#[test]
fn global_challenge_budget_spans_distinct_unknown_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let receiver_config = config(&directory, "challenge-receiver", &vault);
    let receiver = unlocked(&receiver_config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&receiver, receiver_config.device_id, conversation);
    let senders: Vec<_> = (0..25)
        .map(|index| {
            let sender_config = config(&directory, &format!("challenge-sender-{index}"), &vault);
            let sender = unlocked(&sender_config, &vault);
            (sender_config, sender)
        })
        .collect();
    let mut server = Server::default();
    server.upload(&receiver);
    for (_, sender) in &senders {
        server.sync(sender);
    }
    let roster: Vec<_> = std::iter::once(receiver_config.device_id)
        .chain(senders.iter().map(|(config, _)| config.device_id))
        .collect();
    receiver
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: roster.clone(),
                local_gateway_device_id: receiver_config.device_id,
                local_executor: LiveExecutor::CurrentGateway,
            },
            LiveClock {
                wall_ms: 1_000,
                mono_ms: 1_000,
            },
        )
        .unwrap();
    for (sender_config, sender) in &senders {
        sender
            .enable_live_typing(
                LiveEnableContext {
                    trusted_roster: vec![sender_config.device_id, receiver_config.device_id],
                    local_gateway_device_id: sender_config.device_id,
                    local_executor: LiveExecutor::None,
                },
                LiveClock {
                    wall_ms: 1_000,
                    mono_ms: 1_000,
                },
            )
            .unwrap();
    }

    let mut challenges = 0;
    for (index, (sender_config, sender)) in senders.iter().take(24).enumerate() {
        let batch_time = 1_001 + (index / 6) as u64 * 1_000;
        let frame = seal(
            sender,
            conversation,
            receiver_config.device_id,
            TypingStateV2::Started,
            batch_time,
        );
        let outcome = receiver
            .open_typing_signal_v2(
                &frame.signal,
                LiveClock {
                    wall_ms: batch_time as i64,
                    mono_ms: batch_time,
                },
            )
            .unwrap();
        let expected = index % 6 < 5;
        assert_eq!(outcome.reply.is_some(), expected);
        challenges += usize::from(outcome.reply.is_some());
        assert_eq!(sender_config.device_id, frame.signal.sender_device_id);
    }
    assert_eq!(challenges, 20);

    let (blocked_config, blocked_sender) = &senders[24];
    let blocked = seal(
        blocked_sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        5_001,
    );
    assert!(
        receiver
            .open_typing_signal_v2(
                &blocked.signal,
                LiveClock {
                    wall_ms: 5_001,
                    mono_ms: 5_001,
                },
            )
            .unwrap()
            .reply
            .is_none()
    );

    blocked_sender.disable_live_typing().unwrap();
    blocked_sender
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: vec![blocked_config.device_id, receiver_config.device_id],
                local_gateway_device_id: blocked_config.device_id,
                local_executor: LiveExecutor::None,
            },
            LiveClock {
                wall_ms: 61_001,
                mono_ms: 61_001,
            },
        )
        .unwrap();
    let resumed = seal(
        blocked_sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        61_002,
    );
    assert!(
        receiver
            .open_typing_signal_v2(
                &resumed.signal,
                LiveClock {
                    wall_ms: 61_002,
                    mono_ms: 61_002,
                },
            )
            .unwrap()
            .reply
            .is_some()
    );
}

#[test]
fn receive_handshake_and_disable_leave_every_sqlite_row_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&directory, "rows-sender", &vault);
    let receiver_config = config(&directory, "rows-receiver", &vault);
    let sender = unlocked(&sender_config, &vault);
    let receiver = unlocked(&receiver_config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&receiver, receiver_config.device_id, conversation);
    let mut server = Server::default();
    server.upload(&receiver);
    server.sync(&sender);
    let before = persisted_rows(&receiver_config);
    enable_pair(
        &sender,
        &receiver,
        sender_config.device_id,
        receiver_config.device_id,
    );
    let typing = seal(
        &sender,
        conversation,
        receiver_config.device_id,
        TypingStateV2::Started,
        1_001,
    );
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
    receiver
        .open_typing_signal_v2(
            &acknowledgement,
            LiveClock {
                wall_ms: 1_003,
                mono_ms: 1_003,
            },
        )
        .unwrap();
    receiver.disable_live_typing().unwrap();
    assert_eq!(persisted_rows(&receiver_config), before);
}
