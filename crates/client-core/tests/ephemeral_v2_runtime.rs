#[path = "common/mod.rs"]
mod common;

use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use common::*;
use peppy_crypto::{EncryptedEnvelope, KeyPurpose, decrypt, derive_purpose_key, derive_root_key};
use peppy_protocol::{
    EPHEMERAL_FUTURE_TOLERANCE_MS, EphemeralSignalV2, TypingOriginV2, TypingStateV2,
};
use std::collections::BTreeMap;

fn assert_counter_delta<T>(
    client: &Client,
    expected: &[(peppy_client_core::LiveTypingCause, u64)],
    operation: impl FnOnce() -> T,
) -> T {
    let before = client.live_typing_counters().unwrap();
    let result = operation();
    let after = client.live_typing_counters().unwrap();
    assert_eq!(before.len(), after.len());
    for ((cause, old), (new_cause, new)) in before.into_iter().zip(after) {
        assert_eq!(cause, new_cause);
        let delta = expected
            .iter()
            .find(|(item, _)| *item == cause)
            .map_or(0, |(_, value)| *value);
        assert_eq!(new - old, delta, "unexpected delta for {}", cause.name());
    }
    result
}

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
                linked_since_ms: Some(1_000),
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
            None,
            state,
            LiveClock { wall_ms, mono_ms },
        )
        .unwrap()
}

fn enable_pair(
    desktop: &Client,
    desktop_id: DeviceId,
    gateway: &Client,
    gateway_id: DeviceId,
    desktop_clock: LiveClock,
    gateway_clock: LiveClock,
) {
    let roster = vec![desktop_id, gateway_id];
    desktop
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: roster.clone(),
                local_gateway_device_id: gateway_id,
                local_executor: LiveExecutor::None,
            },
            desktop_clock,
        )
        .unwrap();
    gateway
        .enable_live_typing(
            LiveEnableContext {
                trusted_roster: roster,
                local_gateway_device_id: gateway_id,
                local_executor: LiveExecutor::CurrentGateway,
            },
            gateway_clock,
        )
        .unwrap();
}

fn publish_pair_eligibility(
    desktop: &Client,
    gateway: &Client,
    gateway_id: DeviceId,
    conversation: ConversationId,
    observed_at_ms: i64,
) {
    publish_eligible_at(gateway, gateway_id, conversation, 1, observed_at_ms);
    let mut server = Server::default();
    server.upload(gateway);
    server.sync(desktop);
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
                None,
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
            None,
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
fn receive_counter_deltas_distinguish_quota_stale_stop_and_renewal() {
    use peppy_client_core::LiveTypingCause as Cause;
    for contact in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let vault = Vault::new();
        let gateway_config = config(&directory, "delta-gateway", &vault);
        let desktop_config = config(&directory, "delta-desktop", &vault);
        let gateway = unlocked(&gateway_config, &vault);
        let desktop = unlocked(&desktop_config, &vault);
        let conversation = ConversationId::new();
        publish_pair_eligibility(
            &desktop,
            &gateway,
            gateway_config.device_id,
            conversation,
            1_000,
        );
        let clock = |time: i64| LiveClock {
            wall_ms: time,
            mono_ms: time as u64,
        };
        enable_pair(
            &desktop,
            desktop_config.device_id,
            &gateway,
            gateway_config.device_id,
            clock(1_000),
            clock(1_000),
        );
        let (sender, receiver, origin) = if contact {
            (&gateway, &desktop, TypingOriginV2::GoogleContact)
        } else {
            (&desktop, &gateway, TypingOriginV2::VaultDevice)
        };
        let make = |state, time| {
            sender
                .seal_typing_signal_v2(
                    conversation,
                    gateway_config.device_id,
                    origin,
                    None,
                    state,
                    clock(time),
                )
                .unwrap()
                .unwrap()
                .signal
        };
        let first = make(TypingStateV2::Started, 1_001);
        let challenge = assert_counter_delta(
            receiver,
            &[(Cause::FrameReceived, 1), (Cause::SessionChallenged, 1)],
            || {
                receiver
                    .open_typing_signal_v2(&first, clock(1_001))
                    .unwrap()
            },
        )
        .reply
        .unwrap();
        let answer = sender
            .open_typing_signal_v2(&challenge, clock(1_002))
            .unwrap();
        receiver
            .open_typing_signal_v2(&answer.reply.unwrap(), clock(1_003))
            .unwrap();
        receiver
            .open_typing_signal_v2(&answer.follow_ups[0], clock(1_004))
            .unwrap();

        let fresh = make(TypingStateV2::Started, 4_001);
        assert_counter_delta(
            receiver,
            &[(Cause::FrameReceived, 1), (Cause::StartSuppressed, 1)],
            || {
                receiver
                    .open_typing_signal_v2(&fresh, clock(4_001))
                    .unwrap()
            },
        );
        assert_counter_delta(
            receiver,
            &[(Cause::FrameReceived, 1), (Cause::SequenceStale, 1)],
            || {
                receiver
                    .open_typing_signal_v2(&fresh, clock(4_002))
                    .unwrap()
            },
        );

        let stop = make(TypingStateV2::Stopped, 4_003);
        receiver.open_typing_signal_v2(&stop, clock(4_003)).unwrap();
        make(TypingStateV2::Started, 7_002);
        let no_active = make(TypingStateV2::Stopped, 7_003);
        assert_counter_delta(
            receiver,
            &[(Cause::FrameReceived, 1), (Cause::StopWithoutActive, 1)],
            || {
                receiver
                    .open_typing_signal_v2(&no_active, clock(7_003))
                    .unwrap()
            },
        );

        let renewal = make(TypingStateV2::Started, 11_001);
        let outcome = assert_counter_delta(
            receiver,
            &[
                (Cause::FrameReceived, 1),
                (
                    if contact {
                        Cause::Display
                    } else {
                        Cause::GatewayIntent
                    },
                    1,
                ),
                (Cause::RenewalChallenged, 1),
            ],
            || {
                receiver
                    .open_typing_signal_v2(&renewal, clock(11_001))
                    .unwrap()
            },
        );
        assert!(outcome.action.is_some() && outcome.reply.is_some());
    }
}

#[test]
fn cold_start_replays_after_ack_in_both_directions() {
    for contact_origin in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let vault = Vault::new();
        let gateway_config = config(&directory, "replay-gateway", &vault);
        let desktop_config = config(&directory, "replay-desktop", &vault);
        let gateway = unlocked(&gateway_config, &vault);
        let desktop = unlocked(&desktop_config, &vault);
        let conversation = ConversationId::new();
        publish_pair_eligibility(
            &desktop,
            &gateway,
            gateway_config.device_id,
            conversation,
            1_000,
        );
        let clock = |time| LiveClock {
            wall_ms: time,
            mono_ms: time as u64,
        };
        enable_pair(
            &desktop,
            desktop_config.device_id,
            &gateway,
            gateway_config.device_id,
            clock(1_000),
            clock(1_000),
        );
        let (sender, receiver, origin) = if contact_origin {
            (&gateway, &desktop, TypingOriginV2::GoogleContact)
        } else {
            (&desktop, &gateway, TypingOriginV2::VaultDevice)
        };
        let initial = sender
            .seal_typing_signal_v2(
                conversation,
                gateway_config.device_id,
                origin,
                None,
                TypingStateV2::Started,
                clock(1_001),
            )
            .unwrap()
            .unwrap()
            .signal;
        use peppy_client_core::LiveTypingCause as Cause;
        let first = assert_counter_delta(
            receiver,
            &[(Cause::FrameReceived, 1), (Cause::SessionChallenged, 1)],
            || {
                receiver
                    .open_typing_signal_v2(&initial, clock(1_001))
                    .unwrap()
            },
        );
        assert!(first.action.is_none());
        let count = |client: &Client, cause| {
            client
                .live_typing_counters()
                .unwrap()
                .into_iter()
                .find(|(candidate, _)| *candidate == cause)
                .unwrap()
                .1
        };
        assert_eq!(count(receiver, Cause::FrameReceived), 1);
        assert_eq!(count(receiver, Cause::SessionChallenged), 1);
        assert_counter_delta(
            receiver,
            &[
                (Cause::FrameReceived, 1),
                (Cause::SessionChallengeSuppressed, 1),
            ],
            || {
                receiver
                    .open_typing_signal_v2(&initial, clock(1_001))
                    .unwrap()
            },
        );
        assert_eq!(count(receiver, Cause::SessionChallengeSuppressed), 1);
        let response = sender
            .open_typing_signal_v2(&first.reply.unwrap(), clock(1_002))
            .unwrap();
        let ack = response.reply.unwrap();
        assert_eq!(response.follow_ups.len(), 1);
        assert_eq!(count(sender, Cause::ChallengeAnswered), 1);
        assert_eq!(count(sender, Cause::ResendSent), 1);
        let follow_up = &response.follow_ups[0];
        assert!(follow_up.sequence.parse::<u64>().unwrap() > ack.sequence.parse::<u64>().unwrap());
        assert_eq!(follow_up.expires_at_ms, initial.expires_at_ms);
        receiver.open_typing_signal_v2(&ack, clock(1_003)).unwrap();
        assert_eq!(count(receiver, Cause::AckAccepted), 1);
        let outcome = receiver
            .open_typing_signal_v2(follow_up, clock(1_004))
            .unwrap();
        let deadline = match (contact_origin, outcome.action) {
            (
                true,
                Some(LiveAction::Display {
                    conversation_id,
                    deadline_mono,
                    ..
                }),
            )
            | (
                false,
                Some(LiveAction::GatewayTypingIntent {
                    conversation_id,
                    deadline_mono,
                    ..
                }),
            ) => {
                assert_eq!(conversation_id, conversation);
                deadline_mono
            }
            other => panic!("unexpected action: {other:?}"),
        };
        assert_eq!(
            count(
                receiver,
                if contact_origin {
                    Cause::Display
                } else {
                    Cause::GatewayIntent
                }
            ),
            1
        );
        receiver
            .open_typing_signal_v2(follow_up, clock(2_005))
            .unwrap();
        assert_eq!(count(receiver, Cause::SequenceStale), 1);
        assert!(deadline <= initial.expires_at_ms.parse().unwrap());
    }
}

#[test]
fn stopped_or_sharing_disabled_before_challenge_prevents_vault_replay() {
    for stop in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let vault = Vault::new();
        let gateway_config = config(&directory, "stop-gateway", &vault);
        let desktop_config = config(&directory, "stop-desktop", &vault);
        let gateway = unlocked(&gateway_config, &vault);
        let desktop = unlocked(&desktop_config, &vault);
        let conversation = ConversationId::new();
        publish_pair_eligibility(
            &desktop,
            &gateway,
            gateway_config.device_id,
            conversation,
            1_000,
        );
        let clock = |time| LiveClock {
            wall_ms: time,
            mono_ms: time as u64,
        };
        enable_pair(
            &desktop,
            desktop_config.device_id,
            &gateway,
            gateway_config.device_id,
            clock(1_000),
            clock(1_000),
        );
        let first = seal(
            &desktop,
            conversation,
            gateway_config.device_id,
            TypingStateV2::Started,
            1_001,
            1_001,
        )
        .unwrap();
        let challenge = gateway
            .open_typing_signal_v2(&first.signal, clock(1_001))
            .unwrap()
            .reply
            .unwrap();
        if stop {
            seal(
                &desktop,
                conversation,
                gateway_config.device_id,
                TypingStateV2::Stopped,
                1_002,
                1_002,
            )
            .unwrap();
        } else {
            desktop.set_typing_sharing(false).unwrap();
        }
        let response = desktop
            .open_typing_signal_v2(&challenge, clock(1_003))
            .unwrap();
        assert!(response.reply.is_some());
        assert!(response.follow_ups.is_empty());
    }
}

#[test]
fn replay_renewals_over_a_minute_have_a_bounded_frame_count() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "burst-gateway", &vault);
    let desktop_config = config(&directory, "burst-desktop", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let conversation = ConversationId::new();
    publish_pair_eligibility(
        &desktop,
        &gateway,
        gateway_config.device_id,
        conversation,
        1_000,
    );
    let clock = |time: i64| LiveClock {
        wall_ms: time,
        mono_ms: time as u64,
    };
    enable_pair(
        &desktop,
        desktop_config.device_id,
        &gateway,
        gateway_config.device_id,
        clock(1_000),
        clock(1_000),
    );
    let mut controls = 0;
    let mut replays = 0;
    let mut displays = 0;
    let mut last_challenge = None;
    for time in (1_001..61_001).step_by(3_200) {
        let frame = seal(
            &desktop,
            conversation,
            gateway_config.device_id,
            TypingStateV2::Started,
            time,
            time as u64,
        )
        .unwrap()
        .signal;
        let outcome = gateway.open_typing_signal_v2(&frame, clock(time)).unwrap();
        displays += usize::from(outcome.action.is_some());
        if let Some(challenge) = outcome.reply {
            if let Some(last) = last_challenge {
                assert!(time - last >= 10_000, "challenge before renewal window");
            }
            last_challenge = Some(time);
            let response = desktop
                .open_typing_signal_v2(&challenge, clock(time))
                .unwrap();
            assert!(
                !response.follow_ups.is_empty(),
                "active start must be replayed"
            );
            controls += 2;
            replays += response.follow_ups.len();
            gateway
                .open_typing_signal_v2(&response.reply.unwrap(), clock(time))
                .unwrap();
            for follow_up in response.follow_ups {
                let replay = gateway
                    .open_typing_signal_v2(&follow_up, clock(time))
                    .unwrap();
                assert!(replay.reply.is_none(), "replay triggered another challenge");
                displays += usize::from(replay.action.is_some());
            }
        }
    }
    assert!(
        (3..=18).contains(&(controls + replays)),
        "{controls} control frames and {replays} replays"
    );
    assert!(replays > 0);
    assert!(displays > 0);
}

#[test]
fn three_lost_acknowledgements_preserve_sixty_second_cooldown() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "lost-gateway", &vault);
    let desktop_config = config(&directory, "lost-desktop", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let conversation = ConversationId::new();
    publish_pair_eligibility(
        &desktop,
        &gateway,
        gateway_config.device_id,
        conversation,
        1_000,
    );
    let clock = |time: i64| LiveClock {
        wall_ms: time,
        mono_ms: time as u64,
    };
    enable_pair(
        &desktop,
        desktop_config.device_id,
        &gateway,
        gateway_config.device_id,
        clock(1_000),
        clock(1_000),
    );
    for time in [1_001, 11_001, 21_001] {
        let frame = seal(
            &desktop,
            conversation,
            gateway_config.device_id,
            TypingStateV2::Started,
            time,
            time as u64,
        )
        .unwrap()
        .signal;
        let challenge = gateway
            .open_typing_signal_v2(&frame, clock(time))
            .unwrap()
            .reply
            .unwrap();
        let response = desktop
            .open_typing_signal_v2(&challenge, clock(time))
            .unwrap();
        assert!(response.reply.is_some());
        assert_eq!(response.follow_ups.len(), 1);
        // Both the acknowledgement and the follow-up are lost.
    }
    for (time, expect_challenge) in [(31_001, false), (87_999, false), (91_001, true)] {
        let frame = seal(
            &desktop,
            conversation,
            gateway_config.device_id,
            TypingStateV2::Started,
            time,
            time as u64,
        )
        .unwrap()
        .signal;
        let result = gateway.open_typing_signal_v2(&frame, clock(time)).unwrap();
        assert_eq!(result.reply.is_some(), expect_challenge, "time {time}");
    }
}

#[test]
fn challenge_replays_only_two_most_recent_active_starts() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "many-gateway", &vault);
    let desktop_config = config(&directory, "many-desktop", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let conversations: Vec<_> = (0..5).map(|_| ConversationId::new()).collect();
    for conversation in &conversations {
        publish_eligible(&gateway, gateway_config.device_id, *conversation);
    }
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    let clock = |time: i64| LiveClock {
        wall_ms: time,
        mono_ms: time as u64,
    };
    enable_pair(
        &desktop,
        desktop_config.device_id,
        &gateway,
        gateway_config.device_id,
        clock(1_000),
        clock(1_000),
    );
    let frames: Vec<_> = conversations
        .iter()
        .enumerate()
        .map(|(index, conversation)| {
            let time = 1_001 + index as i64;
            seal(
                &desktop,
                *conversation,
                gateway_config.device_id,
                TypingStateV2::Started,
                time,
                time as u64,
            )
            .unwrap()
            .signal
        })
        .collect();
    let challenge = gateway
        .open_typing_signal_v2(&frames[0], clock(1_006))
        .unwrap()
        .reply
        .unwrap();
    let response = desktop
        .open_typing_signal_v2(&challenge, clock(1_006))
        .unwrap();
    assert!(response.reply.is_some());
    let expiries: Vec<_> = response
        .follow_ups
        .iter()
        .map(|frame| frame.expires_at_ms.as_str())
        .collect();
    let expected: Vec<_> = frames
        .iter()
        .rev()
        .take(2)
        .map(|frame| frame.expires_at_ms.as_str())
        .collect();
    assert_eq!(expiries, expected);
    assert!(
        gateway
            .open_typing_signal_v2(&response.reply.unwrap(), clock(1_006))
            .unwrap()
            .session_update
            .is_some()
    );
    for follow_up in &response.follow_ups {
        assert!(matches!(
            gateway
                .open_typing_signal_v2(follow_up, clock(1_006))
                .unwrap()
                .action,
            Some(LiveAction::GatewayTypingIntent {
                state: TypingStateV2::Started,
                ..
            })
        ));
    }
}

#[test]
fn challenge_not_addressed_to_this_device_has_no_response() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "address-gateway", &vault);
    let desktop_config = config(&directory, "address-desktop", &vault);
    let third_config = config(&directory, "address-third", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let third = unlocked(&third_config, &vault);
    let conversation = ConversationId::new();
    publish_pair_eligibility(
        &desktop,
        &gateway,
        gateway_config.device_id,
        conversation,
        1_000,
    );
    let clock = |time: i64| LiveClock {
        wall_ms: time,
        mono_ms: time as u64,
    };
    let roster = vec![
        gateway_config.device_id,
        desktop_config.device_id,
        third_config.device_id,
    ];
    for (client, executor) in [
        (&gateway, LiveExecutor::CurrentGateway),
        (&desktop, LiveExecutor::None),
        (&third, LiveExecutor::None),
    ] {
        client
            .enable_live_typing(
                LiveEnableContext {
                    trusted_roster: roster.clone(),
                    local_gateway_device_id: gateway_config.device_id,
                    local_executor: executor,
                },
                clock(1_000),
            )
            .unwrap();
    }
    let initial = seal(
        &desktop,
        conversation,
        gateway_config.device_id,
        TypingStateV2::Started,
        1_001,
        1_001,
    )
    .unwrap();
    let challenge = gateway
        .open_typing_signal_v2(&initial.signal, clock(1_001))
        .unwrap()
        .reply
        .unwrap();
    let wrong = third
        .open_typing_signal_v2(&challenge, clock(1_002))
        .unwrap();
    assert!(wrong.reply.is_none());
    assert!(wrong.follow_ups.is_empty());
    let correct = desktop
        .open_typing_signal_v2(&challenge, clock(1_002))
        .unwrap();
    assert!(correct.reply.is_some());
    assert_eq!(correct.follow_ups.len(), 1);
}

#[test]
fn refreshing_start_does_not_reset_replay_spacing() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "spacing-gateway", &vault);
    let desktop_config = config(&directory, "spacing-desktop", &vault);
    let third_config = config(&directory, "spacing-third", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let third = unlocked(&third_config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&gateway, gateway_config.device_id, conversation);
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    server.sync(&third);
    let clock = |time: i64| LiveClock {
        wall_ms: time,
        mono_ms: time as u64,
    };
    let roster = vec![
        gateway_config.device_id,
        desktop_config.device_id,
        third_config.device_id,
    ];
    for (client, executor) in [
        (&gateway, LiveExecutor::CurrentGateway),
        (&desktop, LiveExecutor::None),
        (&third, LiveExecutor::None),
    ] {
        client
            .enable_live_typing(
                LiveEnableContext {
                    trusted_roster: roster.clone(),
                    local_gateway_device_id: gateway_config.device_id,
                    local_executor: executor,
                },
                clock(1_000),
            )
            .unwrap();
    }
    let start = gateway
        .seal_typing_signal_v2(
            conversation,
            gateway_config.device_id,
            TypingOriginV2::GoogleContact,
            None,
            TypingStateV2::Started,
            clock(1_001),
        )
        .unwrap()
        .unwrap()
        .signal;
    let first = desktop
        .open_typing_signal_v2(&start, clock(1_002))
        .unwrap()
        .reply
        .unwrap();
    let second = third
        .open_typing_signal_v2(&start, clock(1_002))
        .unwrap()
        .reply
        .unwrap();
    let response = gateway.open_typing_signal_v2(&first, clock(4_000)).unwrap();
    assert!(response.reply.is_some());
    assert_eq!(response.follow_ups.len(), 1);
    assert!(
        gateway
            .seal_typing_signal_v2(
                conversation,
                gateway_config.device_id,
                TypingOriginV2::GoogleContact,
                None,
                TypingStateV2::Started,
                clock(4_001)
            )
            .unwrap()
            .is_some()
    );
    let response = gateway
        .open_typing_signal_v2(&second, clock(4_002))
        .unwrap();
    assert!(response.reply.is_some());
    assert!(response.follow_ups.is_empty());
}

#[test]
fn four_challengers_share_a_six_frame_rolling_replay_budget() {
    const MEMBERSHIP_CHALLENGES_PER_MINUTE: usize = 3;
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "budget-gateway", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let peers: Vec<_> = (0..4)
        .map(|index| {
            let config = config(&directory, &format!("budget-peer-{index}"), &vault);
            let client = unlocked(&config, &vault);
            (config, client)
        })
        .collect();
    let inbound_peers: Vec<_> = (0..20)
        .map(|index| {
            let config = config(&directory, &format!("inbound-peer-{index}"), &vault);
            let client = unlocked(&config, &vault);
            (config, client)
        })
        .collect();
    let conversations: Vec<_> = (0..8).map(|_| ConversationId::new()).collect();
    for conversation in &conversations {
        publish_eligible(&gateway, gateway_config.device_id, *conversation);
    }
    let mut server = Server::default();
    server.upload(&gateway);
    for (_, peer) in &peers {
        server.sync(peer);
    }
    for (_, peer) in &inbound_peers {
        server.sync(peer);
    }
    let clock = |time: i64| LiveClock {
        wall_ms: time,
        mono_ms: time as u64,
    };
    let mut roster = vec![gateway_config.device_id];
    roster.extend(peers.iter().map(|(config, _)| config.device_id));
    roster.extend(inbound_peers.iter().map(|(config, _)| config.device_id));
    let context = |executor| LiveEnableContext {
        trusted_roster: roster.clone(),
        local_gateway_device_id: gateway_config.device_id,
        local_executor: executor,
    };
    gateway
        .enable_live_typing(context(LiveExecutor::CurrentGateway), clock(1_000))
        .unwrap();
    let mut starts = vec![None; conversations.len()];
    let (mut start_count, mut stop_count, mut ack_count, mut challenge_count, mut replay_count) =
        (0, 0, 0, 0, 0);
    for offset in (0..60_000).step_by(500) {
        let time = 1_000 + offset;
        let cycle = offset % 12_000;
        if cycle == 0 || cycle == 1_000 {
            let range = if cycle == 0 { 0..5 } else { 5..8 };
            for index in range {
                starts[index] = Some(
                    gateway
                        .seal_typing_signal_v2(
                            conversations[index],
                            gateway_config.device_id,
                            TypingOriginV2::GoogleContact,
                            None,
                            TypingStateV2::Started,
                            clock(time),
                        )
                        .unwrap()
                        .unwrap()
                        .signal,
                );
                start_count += 1;
            }
        }
        if offset >= 3_000 && (offset - 3_000) % 1_500 == 0 {
            let index = ((offset - 3_000) / 1_500) as usize % peers.len();
            let peer = &peers[index].1;
            peer.enable_live_typing(context(LiveExecutor::None), clock(time))
                .unwrap();
            let challenge = peer
                .open_typing_signal_v2(starts[0].as_ref().unwrap(), clock(time))
                .unwrap()
                .reply
                .unwrap();
            let response = gateway
                .open_typing_signal_v2(&challenge, clock(time))
                .unwrap();
            assert!(response.reply.is_some());
            ack_count += 1;
            replay_count += response.follow_ups.len();
        }
        if offset >= 500 && (offset - 500) % 2_500 == 0 {
            let index = ((offset - 500) / 2_500) as usize;
            if let Some((_, peer)) = inbound_peers.get(index) {
                peer.enable_live_typing(context(LiveExecutor::None), clock(time))
                    .unwrap();
                let frame = seal(
                    peer,
                    conversations[0],
                    gateway_config.device_id,
                    TypingStateV2::Started,
                    time,
                    time as u64,
                )
                .unwrap()
                .signal;
                assert!(
                    gateway
                        .open_typing_signal_v2(&frame, clock(time))
                        .unwrap()
                        .reply
                        .is_some()
                );
                challenge_count += 1;
            }
        }
        if offset == 50_500 {
            for conversation in &conversations {
                assert!(
                    gateway
                        .seal_typing_signal_v2(
                            *conversation,
                            gateway_config.device_id,
                            TypingOriginV2::GoogleContact,
                            None,
                            TypingStateV2::Stopped,
                            clock(time)
                        )
                        .unwrap()
                        .is_some()
                );
                stop_count += 1;
            }
        }
    }
    let counts = format!(
        "starts={start_count}, stops={stop_count}, acks={ack_count}, challenges={challenge_count}, follow_ups={replay_count}"
    );
    assert_eq!(start_count, 40, "{counts}");
    assert_eq!(stop_count, 8, "{counts}");
    assert_eq!(ack_count, 38, "{counts}");
    assert_eq!(challenge_count, 20, "{counts}");
    assert_eq!(replay_count, 6, "{counts}");
    assert!(
        start_count
            + stop_count
            + ack_count
            + challenge_count
            + replay_count
            + MEMBERSHIP_CHALLENGES_PER_MINUTE
            <= 120,
        "{counts}"
    );
}

#[test]
fn replay_seen_by_an_already_confirmed_device_never_extends_expiry() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "third-gateway", &vault);
    let desktop_config = config(&directory, "third-desktop", &vault);
    let third_config = config(&directory, "third-observer", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let third = unlocked(&third_config, &vault);
    let conversation = ConversationId::new();
    publish_eligible(&gateway, gateway_config.device_id, conversation);
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    server.sync(&third);
    let clock = |time: i64| LiveClock {
        wall_ms: time,
        mono_ms: time as u64,
    };
    let roster = vec![
        gateway_config.device_id,
        desktop_config.device_id,
        third_config.device_id,
    ];
    for (client, executor) in [
        (&gateway, LiveExecutor::CurrentGateway),
        (&desktop, LiveExecutor::None),
        (&third, LiveExecutor::None),
    ] {
        client
            .enable_live_typing(
                LiveEnableContext {
                    trusted_roster: roster.clone(),
                    local_gateway_device_id: gateway_config.device_id,
                    local_executor: executor,
                },
                clock(1_000),
            )
            .unwrap();
    }
    let initial = gateway
        .seal_typing_signal_v2(
            conversation,
            gateway_config.device_id,
            TypingOriginV2::GoogleContact,
            None,
            TypingStateV2::Started,
            clock(1_001),
        )
        .unwrap()
        .unwrap()
        .signal;
    let third_challenge = third
        .open_typing_signal_v2(&initial, clock(1_001))
        .unwrap()
        .reply
        .unwrap();
    let first_response = gateway
        .open_typing_signal_v2(&third_challenge, clock(1_002))
        .unwrap();
    third
        .open_typing_signal_v2(&first_response.reply.unwrap(), clock(1_002))
        .unwrap();
    assert!(
        third
            .open_typing_signal_v2(&first_response.follow_ups[0], clock(1_002))
            .unwrap()
            .action
            .is_some()
    );
    let challenge = desktop
        .open_typing_signal_v2(&initial, clock(4_002))
        .unwrap()
        .reply
        .unwrap();
    let response = gateway
        .open_typing_signal_v2(&challenge, clock(4_002))
        .unwrap();
    assert_eq!(response.follow_ups.len(), 1);
    let observed = third
        .open_typing_signal_v2(&response.follow_ups[0], clock(4_002))
        .unwrap();
    if let Some(LiveAction::Display { deadline_mono, .. }) = observed.action {
        assert!(deadline_mono <= initial.expires_at_ms.parse::<u64>().unwrap());
    } else {
        assert!(observed.action.is_none());
    }
}

#[test]
fn challenge_ack_survives_when_conversation_becomes_ineligible() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "ineligible-gateway", &vault);
    let desktop_config = config(&directory, "ineligible-desktop", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let conversation = ConversationId::new();
    publish_pair_eligibility(
        &desktop,
        &gateway,
        gateway_config.device_id,
        conversation,
        1_000,
    );
    let clock = |time: i64| LiveClock {
        wall_ms: time,
        mono_ms: time as u64,
    };
    enable_pair(
        &desktop,
        desktop_config.device_id,
        &gateway,
        gateway_config.device_id,
        clock(1_000),
        clock(1_000),
    );
    let initial = gateway
        .seal_typing_signal_v2(
            conversation,
            gateway_config.device_id,
            TypingOriginV2::GoogleContact,
            None,
            TypingStateV2::Started,
            clock(1_001),
        )
        .unwrap()
        .unwrap()
        .signal;
    let challenge = desktop
        .open_typing_signal_v2(&initial, clock(1_001))
        .unwrap()
        .reply
        .unwrap();
    gateway
        .publish_rcs_capability(
            &RcsLinkStatusEvent {
                gateway_device_id: gateway_config.device_id,
                state: RcsCapabilityState::Unlinked,
                availability: false,
                health: false,
                phone_responsive: false,
                observed_at_ms: 1_002,
                capability_revision: 2,
                feature_bits: 0,
                linked_since_ms: None,
            },
            &[RcsEligibilityProjection {
                conversation_id: conversation,
                eligibility: RcsEligibility::NotEligible,
                feature_bits: 0,
                mapping_revision: 2,
                observed_at_ms: 1_002,
                gateway_device_id: gateway_config.device_id,
            }],
            1_002,
        )
        .unwrap();
    let response = gateway
        .open_typing_signal_v2(&challenge, clock(1_002))
        .unwrap();
    assert!(response.reply.is_some());
    assert!(response.follow_ups.is_empty());
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

    receiver.set_typing_sharing(false).unwrap();
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
    receiver.set_typing_sharing(true).unwrap();
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
fn skewed_two_core_handshakes_preserve_typing_actions() {
    for skew_ms in [1_250, -1_250] {
        let directory = tempfile::tempdir().unwrap();
        let vault = Vault::new();
        let gateway_config = config(&directory, "v2-skew-gateway", &vault);
        let desktop_config = config(&directory, "v2-skew-desktop", &vault);
        let gateway = unlocked(&gateway_config, &vault);
        let desktop = unlocked(&desktop_config, &vault);
        let conversation = ConversationId::new();
        let base_wall_ms = 50_000;
        let gateway_wall_ms = base_wall_ms + skew_ms;
        publish_pair_eligibility(
            &desktop,
            &gateway,
            gateway_config.device_id,
            conversation,
            base_wall_ms.min(gateway_wall_ms),
        );
        enable_pair(
            &desktop,
            desktop_config.device_id,
            &gateway,
            gateway_config.device_id,
            LiveClock {
                wall_ms: base_wall_ms,
                mono_ms: 1_000,
            },
            LiveClock {
                wall_ms: gateway_wall_ms,
                mono_ms: 1_000,
            },
        );
        let initial = seal(
            &desktop,
            conversation,
            gateway_config.device_id,
            TypingStateV2::Started,
            base_wall_ms + 1,
            1_001,
        )
        .unwrap();
        let challenge = gateway
            .open_typing_signal_v2(
                &initial.signal,
                LiveClock {
                    wall_ms: gateway_wall_ms + 1,
                    mono_ms: 1_001,
                },
            )
            .unwrap();
        assert_eq!(challenge.action, None);
        assert!(challenge.reply.is_some(), "skew {skew_ms}");
        let challenge = challenge.reply.unwrap();
        let acknowledgement = desktop
            .open_typing_signal_v2(
                &challenge,
                LiveClock {
                    wall_ms: base_wall_ms + 2,
                    mono_ms: 1_002,
                },
            )
            .unwrap()
            .reply
            .unwrap();
        assert!(
            gateway
                .open_typing_signal_v2(
                    &acknowledgement,
                    LiveClock {
                        wall_ms: gateway_wall_ms + 3,
                        mono_ms: 1_003,
                    },
                )
                .unwrap()
                .session_update
                .is_some()
        );

        let action = seal(
            &desktop,
            conversation,
            gateway_config.device_id,
            TypingStateV2::Started,
            base_wall_ms + 4_001,
            5_001,
        )
        .unwrap();
        assert!(matches!(
            gateway
                .open_typing_signal_v2(
                    &action.signal,
                    LiveClock {
                        wall_ms: gateway_wall_ms + 4_001,
                        mono_ms: 5_001,
                    },
                )
                .unwrap()
                .action,
            Some(LiveAction::GatewayTypingIntent { conversation_id, state: TypingStateV2::Started, .. }) if conversation_id == conversation
        ));

        let initial = gateway
            .seal_typing_signal_v2(
                conversation,
                gateway_config.device_id,
                TypingOriginV2::GoogleContact,
                None,
                TypingStateV2::Started,
                LiveClock {
                    wall_ms: gateway_wall_ms + 8_001,
                    mono_ms: 9_001,
                },
            )
            .unwrap()
            .unwrap();
        let challenge = desktop
            .open_typing_signal_v2(
                &initial.signal,
                LiveClock {
                    wall_ms: base_wall_ms + 8_001,
                    mono_ms: 9_001,
                },
            )
            .unwrap()
            .reply
            .unwrap();
        let acknowledgement = gateway
            .open_typing_signal_v2(
                &challenge,
                LiveClock {
                    wall_ms: gateway_wall_ms + 8_002,
                    mono_ms: 9_002,
                },
            )
            .unwrap()
            .reply
            .unwrap();
        assert!(
            desktop
                .open_typing_signal_v2(
                    &acknowledgement,
                    LiveClock {
                        wall_ms: base_wall_ms + 8_003,
                        mono_ms: 9_003,
                    },
                )
                .unwrap()
                .session_update
                .is_some()
        );
        let displayed = gateway
            .seal_typing_signal_v2(
                conversation,
                gateway_config.device_id,
                TypingOriginV2::GoogleContact,
                None,
                TypingStateV2::Started,
                LiveClock {
                    wall_ms: gateway_wall_ms + 12_001,
                    mono_ms: 13_001,
                },
            )
            .unwrap()
            .unwrap();
        assert!(matches!(
            desktop
                .open_typing_signal_v2(
                    &displayed.signal,
                    LiveClock {
                        wall_ms: base_wall_ms + 12_001,
                        mono_ms: 13_001,
                    },
                )
                .unwrap()
                .action,
            Some(LiveAction::Display { .. })
        ));
    }
}

#[test]
fn skewed_sender_at_future_tolerance_is_accepted() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "v2-skew-tolerance-gateway", &vault);
    let desktop_config = config(&directory, "v2-skew-tolerance-desktop", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let conversation = ConversationId::new();
    let base_wall_ms = 50_000;
    let gateway_wall_ms = base_wall_ms - EPHEMERAL_FUTURE_TOLERANCE_MS as i64;
    publish_pair_eligibility(
        &desktop,
        &gateway,
        gateway_config.device_id,
        conversation,
        gateway_wall_ms,
    );
    enable_pair(
        &desktop,
        desktop_config.device_id,
        &gateway,
        gateway_config.device_id,
        LiveClock {
            wall_ms: base_wall_ms,
            mono_ms: 1_000,
        },
        LiveClock {
            wall_ms: gateway_wall_ms,
            mono_ms: 1_000,
        },
    );

    let typing = seal(
        &desktop,
        conversation,
        gateway_config.device_id,
        TypingStateV2::Started,
        base_wall_ms + 1,
        1_001,
    )
    .unwrap();
    let outcome = gateway
        .open_typing_signal_v2(
            &typing.signal,
            LiveClock {
                wall_ms: gateway_wall_ms + 1,
                mono_ms: 1_001,
            },
        )
        .unwrap();
    assert_eq!(outcome.action, None);
    assert!(outcome.reply.is_some());
}

#[test]
fn skewed_sender_beyond_tolerance_is_dropped_without_reply_or_action() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&directory, "v2-skew-drop-gateway", &vault);
    let desktop_config = config(&directory, "v2-skew-drop-desktop", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let conversation = ConversationId::new();
    let base_wall_ms = 50_000;
    let gateway_wall_ms = base_wall_ms - EPHEMERAL_FUTURE_TOLERANCE_MS as i64 - 1;
    publish_pair_eligibility(
        &desktop,
        &gateway,
        gateway_config.device_id,
        conversation,
        gateway_wall_ms,
    );
    enable_pair(
        &desktop,
        desktop_config.device_id,
        &gateway,
        gateway_config.device_id,
        LiveClock {
            wall_ms: base_wall_ms,
            mono_ms: 1_000,
        },
        LiveClock {
            wall_ms: gateway_wall_ms,
            mono_ms: 1_000,
        },
    );

    let typing = seal(
        &desktop,
        conversation,
        gateway_config.device_id,
        TypingStateV2::Started,
        base_wall_ms + 1,
        1_001,
    )
    .unwrap();
    let outcome = gateway
        .open_typing_signal_v2(
            &typing.signal,
            LiveClock {
                wall_ms: gateway_wall_ms + 1,
                mono_ms: 1_001,
            },
        )
        .unwrap();
    assert_eq!(outcome.reply, None);
    assert_eq!(outcome.action, None);
}

#[test]
fn disabled_sharing_validates_and_commits_the_outbound_clock_without_sealing() {
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
            1_001,
            1_001,
        ),
        None
    );
    assert_eq!(
        client.seal_typing_signal_v2(
            conversation,
            config.device_id,
            TypingOriginV2::VaultDevice,
            None,
            TypingStateV2::Stopped,
            LiveClock {
                wall_ms: 1_002,
                mono_ms: 1_000,
            },
        ),
        Err(LiveError::Clock)
    );
    client.set_typing_sharing(true).unwrap();
    assert_eq!(
        seal(
            &client,
            conversation,
            config.device_id,
            TypingStateV2::Started,
            1_002,
            1_002,
        )
        .unwrap()
        .signal
        .sequence,
        "1"
    );
}

#[test]
fn disabled_sharing_suppresses_both_states_without_consuming_sequence_or_budget() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-sharing-stop", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let google_conversation = ConversationId::new();
    publish_eligible(&client, config.device_id, conversation);
    publish_eligible(&client, config.device_id, google_conversation);
    enable(
        &client,
        config.device_id,
        LiveClock {
            wall_ms: 1_000,
            mono_ms: 1_000,
        },
    );
    assert!(
        seal(
            &client,
            conversation,
            config.device_id,
            TypingStateV2::Started,
            1_001,
            1_001,
        )
        .is_some()
    );

    client.set_typing_sharing(false).unwrap();
    assert_eq!(
        seal(
            &client,
            conversation,
            config.device_id,
            TypingStateV2::Started,
            1_002,
            1_002,
        ),
        None
    );
    assert_eq!(
        seal(
            &client,
            conversation,
            config.device_id,
            TypingStateV2::Stopped,
            1_003,
            1_003,
        ),
        None
    );
    assert_eq!(
        client
            .seal_typing_signal_v2(
                google_conversation,
                config.device_id,
                TypingOriginV2::GoogleContact,
                None,
                TypingStateV2::Started,
                LiveClock {
                    wall_ms: 1_004,
                    mono_ms: 1_004,
                },
            )
            .unwrap()
            .unwrap()
            .signal
            .sequence,
        "2"
    );
    assert_eq!(
        client
            .seal_typing_signal_v2(
                google_conversation,
                config.device_id,
                TypingOriginV2::GoogleContact,
                None,
                TypingStateV2::Stopped,
                LiveClock {
                    wall_ms: 1_005,
                    mono_ms: 1_005,
                },
            )
            .unwrap()
            .unwrap()
            .signal
            .sequence,
        "3"
    );
    client.set_typing_sharing(true).unwrap();
    let stopped = seal(
        &client,
        conversation,
        config.device_id,
        TypingStateV2::Stopped,
        1_006,
        1_006,
    )
    .unwrap();
    assert_eq!(stopped.signal.sequence, "4");
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
            None,
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
            None,
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
            None,
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
fn json_receive_preserves_host_clock_errors_and_empty_outcome_generation() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-json-receive", &vault);
    let client = unlocked(&config, &vault);
    assert_eq!(
        client.open_typing_signal_v2_json(
            "{",
            LiveClock {
                wall_ms: 1,
                mono_ms: 1
            }
        ),
        Err(LiveError::Disabled)
    );
    enable(
        &client,
        config.device_id,
        LiveClock {
            wall_ms: 1_000,
            mono_ms: 1_000,
        },
    );
    assert_eq!(
        client.open_typing_signal_v2_json(
            "{",
            LiveClock {
                wall_ms: -1,
                mono_ms: 1_001
            }
        ),
        Err(LiveError::Clock)
    );
    client.disable_live_typing().unwrap();
    let generation = enable(
        &client,
        config.device_id,
        LiveClock {
            wall_ms: 1_000,
            mono_ms: 1_000,
        },
    );
    for frame_json in [
        "{",
        &"x".repeat(peppy_protocol::POSTGRES_NOTIFY_PAYLOAD_BYTES + 1),
    ] {
        let outcome = client
            .open_typing_signal_v2_json(
                frame_json,
                LiveClock {
                    wall_ms: 1_001,
                    mono_ms: 1_001,
                },
            )
            .unwrap();
        assert_eq!(outcome.generation, generation);
        assert!(
            outcome.action.is_none() && outcome.reply.is_none() && outcome.session_update.is_none()
        );
    }
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
                linked_since_ms: None,
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
    assert_eq!(
        client.seal_typing_signal_v2(
            conversation,
            config.device_id,
            TypingOriginV2::VaultDevice,
            None,
            TypingStateV2::Stopped,
            LiveClock {
                wall_ms: 1_003,
                mono_ms: 1_001,
            },
        ),
        Err(LiveError::Clock)
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
fn rate_limited_start_advances_the_clock_without_consuming_sequence() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "v2-rate-limited-clock", &vault);
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
            1_001,
        )
        .unwrap()
        .signal
        .sequence,
        "1"
    );
    assert_eq!(
        seal(
            &client,
            conversation,
            config.device_id,
            TypingStateV2::Started,
            1_002,
            1_002,
        ),
        None
    );
    assert_eq!(
        client.seal_typing_signal_v2(
            conversation,
            config.device_id,
            TypingOriginV2::VaultDevice,
            None,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: 1_003,
                mono_ms: 1_001,
            },
        ),
        Err(LiveError::Clock)
    );
    assert_eq!(
        seal(
            &client,
            conversation,
            config.device_id,
            TypingStateV2::Started,
            4_002,
            4_002,
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
            None,
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
            None,
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
            None,
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
            None,
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
            None,
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
            6_001,
            6_001
        ),
        None
    );
    assert_eq!(
        seal(
            &client,
            conversations[0],
            config.device_id,
            TypingStateV2::Stopped,
            6_002,
            6_002
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
            6_003,
            6_003
        ),
        None
    );
}

#[test]
fn suppressed_redundant_stop_advances_the_clock_without_consuming_sequence() {
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
        client.seal_typing_signal_v2(
            second,
            config.device_id,
            TypingOriginV2::VaultDevice,
            None,
            TypingStateV2::Started,
            LiveClock {
                wall_ms: 1_004,
                mono_ms: 1_002,
            },
        ),
        Err(LiveError::Clock)
    );
    assert_eq!(
        seal(
            &client,
            second,
            config.device_id,
            TypingStateV2::Started,
            1_004,
            1_004
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
            None,
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
            None,
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
            None,
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
            None,
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
