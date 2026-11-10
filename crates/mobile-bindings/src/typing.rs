//! Sanitized UniFFI boundary for memory-only live typing.

use peppy_client_core::{
    Client, ConversationId, DeviceId, LiveAction, LiveClock, LiveEnableContext, LiveError,
    LiveExecutor, LiveOutcome,
};
use peppy_protocol::{TypingOriginV2, TypingStateV2};
use std::{str::FromStr, sync::Mutex};

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NativeLiveEnableContext {
    pub trusted_roster: Vec<String>,
    pub local_gateway_device_id: String,
    pub local_executor: NativeLiveExecutor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeLiveExecutor {
    None,
    CurrentGateway,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct NativeLiveClock {
    pub wall_ms: i64,
    pub mono_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NativeTypingOrigin {
    VaultDevice,
    GoogleContact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, uniffi::Enum)]
pub enum NativeTypingState {
    Started,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, uniffi::Record)]
pub struct NativeLiveOutbound {
    pub generation: u64,
    pub frame_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, uniffi::Record)]
pub struct NativeLiveSessionUpdate {
    pub peer_device_id: String,
    pub replaced_session_token: Option<u64>,
    pub current_session_token: u64,
    pub validation_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, uniffi::Enum)]
pub enum NativeLiveAction {
    Display {
        peer_device_id: String,
        session_token: u64,
        sequence: u64,
        conversation_id: String,
        /// Canonical E.164 group member; `None` for 1:1 typing.
        participant: Option<String>,
        state: NativeTypingState,
        deadline_mono: u64,
    },
    GatewayTypingIntent {
        peer_device_id: String,
        session_token: u64,
        sequence: u64,
        conversation_id: String,
        /// Canonical E.164 group member; `None` for 1:1 typing.
        participant: Option<String>,
        state: NativeTypingState,
        deadline_mono: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, uniffi::Record)]
pub struct NativeLiveOutcome {
    pub generation: u64,
    pub session_update: Option<NativeLiveSessionUpdate>,
    pub action: Option<NativeLiveAction>,
    pub reply_frame_json: Option<String>,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum NativeLiveTypingError {
    #[error("clock")]
    Clock,
    #[error("disabled")]
    Disabled,
    #[error("counter_exhausted")]
    CounterExhausted,
    #[error("crypto")]
    Crypto,
    #[error("invalid_context")]
    InvalidContext,
}

pub(crate) fn with_live_client<T>(
    handle: &Mutex<Option<Client>>,
    operation: impl FnOnce(&Client) -> Result<T, NativeLiveTypingError>,
) -> Result<T, NativeLiveTypingError> {
    let client = handle
        .lock()
        .map_err(|_| NativeLiveTypingError::InvalidContext)?
        .as_ref()
        .ok_or(NativeLiveTypingError::InvalidContext)?
        .clone();
    operation(&client)
}

pub(crate) fn enable_context(
    value: NativeLiveEnableContext,
) -> Result<LiveEnableContext, NativeLiveTypingError> {
    let trusted_roster = value
        .trusted_roster
        .iter()
        .map(|device_id| parse_device_id(device_id))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(LiveEnableContext {
        trusted_roster,
        local_gateway_device_id: parse_device_id(&value.local_gateway_device_id)?,
        local_executor: match value.local_executor {
            NativeLiveExecutor::None => LiveExecutor::None,
            NativeLiveExecutor::CurrentGateway => LiveExecutor::CurrentGateway,
        },
    })
}

pub(crate) fn parse_device_id(value: &str) -> Result<DeviceId, NativeLiveTypingError> {
    DeviceId::from_str(value).map_err(|_| NativeLiveTypingError::InvalidContext)
}

pub(crate) fn parse_conversation_id(value: &str) -> Result<ConversationId, NativeLiveTypingError> {
    ConversationId::from_str(value).map_err(|_| NativeLiveTypingError::InvalidContext)
}

pub(crate) fn clock(value: NativeLiveClock) -> Result<LiveClock, NativeLiveTypingError> {
    if value.wall_ms < 0 {
        return Err(NativeLiveTypingError::Clock);
    }
    Ok(LiveClock {
        wall_ms: value.wall_ms,
        mono_ms: value.mono_ms,
    })
}

pub(crate) fn origin(value: NativeTypingOrigin) -> TypingOriginV2 {
    match value {
        NativeTypingOrigin::VaultDevice => TypingOriginV2::VaultDevice,
        NativeTypingOrigin::GoogleContact => TypingOriginV2::GoogleContact,
    }
}

pub(crate) fn state(value: NativeTypingState) -> TypingStateV2 {
    match value {
        NativeTypingState::Started => TypingStateV2::Started,
        NativeTypingState::Stopped => TypingStateV2::Stopped,
    }
}

pub(crate) fn outbound_view(
    value: Option<peppy_client_core::LiveOutbound>,
) -> Result<Option<NativeLiveOutbound>, NativeLiveTypingError> {
    value
        .map(|outbound| {
            Ok(NativeLiveOutbound {
                generation: outbound.generation.0,
                frame_json: serde_json::to_string(&outbound.signal)
                    .map_err(|_| NativeLiveTypingError::Crypto)?,
            })
        })
        .transpose()
}

pub(crate) fn outcome_view(value: LiveOutcome) -> Result<NativeLiveOutcome, NativeLiveTypingError> {
    Ok(NativeLiveOutcome {
        generation: value.generation.0,
        session_update: value.session_update.map(|update| NativeLiveSessionUpdate {
            peer_device_id: update.peer_device_id.to_string(),
            replaced_session_token: update.replaced_session_token,
            current_session_token: update.current_session_token,
            validation_revision: update.validation_revision,
        }),
        action: value.action.map(action_view),
        reply_frame_json: value
            .reply
            .map(|frame| serde_json::to_string(&frame).map_err(|_| NativeLiveTypingError::Crypto))
            .transpose()?,
    })
}

fn action_view(value: LiveAction) -> NativeLiveAction {
    match value {
        LiveAction::Display {
            peer_device_id,
            session_token,
            sequence,
            conversation_id,
            participant,
            state,
            deadline_mono,
        } => NativeLiveAction::Display {
            peer_device_id: peer_device_id.to_string(),
            session_token,
            sequence,
            conversation_id: conversation_id.to_string(),
            participant,
            state: state_view(state),
            deadline_mono,
        },
        LiveAction::GatewayTypingIntent {
            peer_device_id,
            session_token,
            sequence,
            conversation_id,
            participant,
            state,
            deadline_mono,
        } => NativeLiveAction::GatewayTypingIntent {
            peer_device_id: peer_device_id.to_string(),
            session_token,
            sequence,
            conversation_id: conversation_id.to_string(),
            participant,
            state: state_view(state),
            deadline_mono,
        },
    }
}

fn state_view(value: TypingStateV2) -> NativeTypingState {
    match value {
        TypingStateV2::Started => NativeTypingState::Started,
        TypingStateV2::Stopped => NativeTypingState::Stopped,
    }
}

pub(crate) fn live_error(value: LiveError) -> NativeLiveTypingError {
    match value {
        LiveError::Clock => NativeLiveTypingError::Clock,
        LiveError::Disabled => NativeLiveTypingError::Disabled,
        LiveError::CounterExhausted => NativeLiveTypingError::CounterExhausted,
        LiveError::Crypto => NativeLiveTypingError::Crypto,
        LiveError::InvalidContext => NativeLiveTypingError::InvalidContext,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NativeOpenConfig, open_native_client};
    use peppy_client_core::{
        ClientConfig, Cursor, DatabaseKey, IngestResult, RcsCapabilityState, RcsEligibility,
        RcsEligibilityProjection, RcsLinkStatusEvent, VaultId,
    };
    use peppy_crypto::{create_vault_check_header, derive_root_key};

    fn native_client() -> std::sync::Arc<crate::NativeClient> {
        open_native_client(NativeOpenConfig {
            database_path: std::env::temp_dir()
                .join(format!("peppy-live-typing-{}.db", uuid::Uuid::new_v4()))
                .display()
                .to_string(),
            vault_id: uuid::Uuid::new_v4().to_string(),
            device_id: uuid::Uuid::new_v4().to_string(),
            database_key: vec![7; 32],
        })
        .unwrap()
    }

    struct LiveFixture {
        sender: std::sync::Arc<crate::NativeClient>,
        receiver: std::sync::Arc<crate::NativeClient>,
        sender_id: DeviceId,
        receiver_id: DeviceId,
        conversation: ConversationId,
    }

    fn live_fixture() -> LiveFixture {
        let vault_id = VaultId::new();
        let profile = peppy_client_core::KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let root_path =
            std::env::temp_dir().join(format!("peppy-live-typing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root_path).unwrap();
        let open = |name: &str| {
            let config = ClientConfig {
                database_path: root_path.join(format!("{name}.db")),
                vault_id,
                device_id: DeviceId::new(),
            };
            let client = Client::open(config.clone(), DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
            client
                .unlock(&profile, &header, "correct horse battery staple")
                .unwrap();
            client.set_server_compaction_state(true, true).unwrap();
            (config, client)
        };
        let (sender_config, sender_core) = open("sender");
        let (receiver_config, receiver_core) = open("receiver");
        let conversation = ConversationId::new();
        receiver_core
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    gateway_device_id: receiver_config.device_id,
                    state: RcsCapabilityState::Linked,
                    availability: true,
                    health: true,
                    phone_responsive: true,
                    observed_at_ms: 1_000,
                    capability_revision: 1,
                    feature_bits: 1,
                    linked_since_ms: Some(1_000),
                },
                &[RcsEligibilityProjection {
                    conversation_id: conversation,
                    eligibility: RcsEligibility::Eligible,
                    feature_bits: 1,
                    mapping_revision: 1,
                    observed_at_ms: 1_000,
                    gateway_device_id: receiver_config.device_id,
                }],
                1_000,
            )
            .unwrap();
        for (index, envelope) in receiver_core
            .pending_outbox()
            .unwrap()
            .into_iter()
            .enumerate()
        {
            receiver_core.ack_outbox(envelope.envelope_id).unwrap();
            assert!(matches!(
                sender_core
                    .ingest(&envelope, Cursor(index as u64 + 1))
                    .unwrap(),
                IngestResult::Journaled
            ));
        }
        sender_core.apply_pending(100).unwrap();
        LiveFixture {
            sender: std::sync::Arc::new(crate::NativeClient {
                client: Mutex::new(Some(sender_core.clone())),
            }),
            receiver: std::sync::Arc::new(crate::NativeClient {
                client: Mutex::new(Some(receiver_core.clone())),
            }),
            sender_id: sender_config.device_id,
            receiver_id: receiver_config.device_id,
            conversation,
        }
    }

    fn clock(mono_ms: u64) -> NativeLiveClock {
        NativeLiveClock {
            wall_ms: mono_ms as i64,
            mono_ms,
        }
    }

    #[test]
    fn errors_use_the_fixed_host_codes() {
        assert!(matches!(
            live_error(LiveError::Disabled),
            NativeLiveTypingError::Disabled
        ));
        assert!(matches!(
            parse_device_id("not-a-device"),
            Err(NativeLiveTypingError::InvalidContext)
        ));
    }

    #[test]
    fn malformed_frame_still_observes_disabled_admission() {
        let client = native_client();
        assert!(matches!(
            client.open_typing_signal_v2(
                "not-json".into(),
                NativeLiveClock {
                    wall_ms: 1,
                    mono_ms: 1,
                },
            ),
            Err(NativeLiveTypingError::Disabled)
        ));
    }

    #[test]
    fn invalid_roster_is_rejected_by_the_native_client_boundary() {
        let client = native_client();
        assert!(matches!(
            client.enable_live_typing(
                NativeLiveEnableContext {
                    trusted_roster: vec!["not-a-device".into()],
                    local_gateway_device_id: uuid::Uuid::new_v4().to_string(),
                    local_executor: NativeLiveExecutor::None,
                },
                NativeLiveClock {
                    wall_ms: 1,
                    mono_ms: 1,
                },
            ),
            Err(NativeLiveTypingError::InvalidContext)
        ));
    }

    #[test]
    fn native_clients_complete_the_real_typing_handshake_and_preserve_sanitized_metadata() {
        let fixture = live_fixture();
        let roster = vec![
            fixture.sender_id.to_string(),
            fixture.receiver_id.to_string(),
        ];
        let sender_generation = fixture
            .sender
            .enable_live_typing(
                NativeLiveEnableContext {
                    trusted_roster: roster.clone(),
                    local_gateway_device_id: fixture.sender_id.to_string(),
                    local_executor: NativeLiveExecutor::None,
                },
                clock(1_000),
            )
            .unwrap();
        let receiver_generation = fixture
            .receiver
            .enable_live_typing(
                NativeLiveEnableContext {
                    trusted_roster: roster,
                    local_gateway_device_id: fixture.receiver_id.to_string(),
                    local_executor: NativeLiveExecutor::CurrentGateway,
                },
                clock(1_000),
            )
            .unwrap();
        let typing = fixture
            .sender
            .seal_typing_signal_v2(
                fixture.conversation.to_string(),
                fixture.receiver_id.to_string(),
                NativeTypingOrigin::VaultDevice,
                None,
                NativeTypingState::Started,
                clock(1_001),
            )
            .unwrap()
            .unwrap();
        let challenge = fixture
            .receiver
            .open_typing_signal_v2(typing.frame_json, clock(1_001))
            .unwrap()
            .reply_frame_json
            .unwrap();
        let acknowledgement = fixture
            .sender
            .open_typing_signal_v2(challenge, clock(1_002))
            .unwrap()
            .reply_frame_json
            .unwrap();
        let installed = fixture
            .receiver
            .open_typing_signal_v2(acknowledgement, clock(1_003))
            .unwrap();
        assert_eq!(installed.generation, receiver_generation);
        assert!(installed.session_update.is_some());
        let fresh = fixture
            .sender
            .seal_typing_signal_v2(
                fixture.conversation.to_string(),
                fixture.receiver_id.to_string(),
                NativeTypingOrigin::VaultDevice,
                None,
                NativeTypingState::Started,
                clock(4_002),
            )
            .unwrap()
            .unwrap();
        assert_eq!(fresh.generation, sender_generation);
        let outcome = fixture
            .receiver
            .open_typing_signal_v2(fresh.frame_json, clock(4_002))
            .unwrap();
        assert_eq!(outcome.generation, receiver_generation);
        match outcome.action.unwrap() {
            NativeLiveAction::GatewayTypingIntent {
                peer_device_id,
                session_token,
                sequence,
                conversation_id,
                participant,
                state,
                deadline_mono,
            } => {
                assert_eq!(peer_device_id, fixture.sender_id.to_string());
                assert!(session_token > 0);
                assert_eq!(sequence, 3);
                assert_eq!(conversation_id, fixture.conversation.to_string());
                assert_eq!(participant, None);
                assert_eq!(state, NativeTypingState::Started);
                assert!(deadline_mono > 4_002);
            }
            action => panic!("unexpected action: {action:?}"),
        }
    }

    #[test]
    fn participant_seal_fails_closed_until_the_roster_confirms_and_rejects_vault_frames() {
        let fixture = live_fixture();
        fixture
            .receiver
            .enable_live_typing(
                NativeLiveEnableContext {
                    trusted_roster: vec![fixture.receiver_id.to_string()],
                    local_gateway_device_id: fixture.receiver_id.to_string(),
                    local_executor: NativeLiveExecutor::CurrentGateway,
                },
                clock(1_000),
            )
            .unwrap();
        assert_eq!(
            fixture
                .receiver
                .seal_typing_signal_v2(
                    fixture.conversation.to_string(),
                    fixture.receiver_id.to_string(),
                    NativeTypingOrigin::GoogleContact,
                    Some("+15551230001".into()),
                    NativeTypingState::Started,
                    clock(1_001),
                )
                .unwrap(),
            None
        );
        assert!(matches!(
            fixture.receiver.seal_typing_signal_v2(
                fixture.conversation.to_string(),
                fixture.receiver_id.to_string(),
                NativeTypingOrigin::VaultDevice,
                Some("+15551230001".into()),
                NativeTypingState::Started,
                clock(1_002),
            ),
            Err(NativeLiveTypingError::InvalidContext)
        ));
    }

    #[test]
    fn enabled_malformed_json_keeps_the_current_generation() {
        let fixture = live_fixture();
        let generation = fixture
            .receiver
            .enable_live_typing(
                NativeLiveEnableContext {
                    trusted_roster: vec![fixture.receiver_id.to_string()],
                    local_gateway_device_id: fixture.receiver_id.to_string(),
                    local_executor: NativeLiveExecutor::CurrentGateway,
                },
                clock(1_000),
            )
            .unwrap();
        let outcome = fixture
            .receiver
            .open_typing_signal_v2("not-json".into(), clock(1_001))
            .unwrap();
        assert_eq!(outcome.generation, generation);
        assert_eq!(outcome.session_update, None);
        assert_eq!(outcome.action, None);
        assert_eq!(outcome.reply_frame_json, None);
    }

    #[test]
    fn large_u64s_and_serialized_records_remain_lossless_and_sanitized() {
        let value = u64::MAX - 7;
        let outcome = outcome_view(LiveOutcome {
            generation: peppy_client_core::LiveEnableGeneration(value),
            session_update: None,
            action: Some(LiveAction::Display {
                peer_device_id: DeviceId::new(),
                session_token: value,
                sequence: value,
                conversation_id: ConversationId::new(),
                participant: None,
                state: TypingStateV2::Started,
                deadline_mono: value,
            }),
            reply: None,
        })
        .unwrap();
        let json = serde_json::to_value(&outcome).unwrap();
        assert_eq!(json["generation"], value);
        assert_eq!(json["action"]["Display"]["sequence"], value);
        let fields = json.as_object().unwrap();
        assert_eq!(fields.len(), 4);
        assert!(
            fields.contains_key("generation")
                && fields.contains_key("session_update")
                && fields.contains_key("action")
                && fields.contains_key("reply_frame_json")
        );
        assert!(
            !json.to_string().contains("ciphertext") && !json.to_string().contains("event_key")
        );
    }
}
