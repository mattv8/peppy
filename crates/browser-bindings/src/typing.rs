use crate::{BrowserCore, Failure, core, invalid, unknown_command};
use peppy_client_core::{
    DeviceId, LiveAction, LiveClock, LiveEnableContext, LiveError, LiveExecutor, LiveOutcome,
    PeerSessionUpdate,
};
use peppy_protocol::{TypingOriginV2, TypingStateV2};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) fn handles(command: &str) -> bool {
    matches!(
        command,
        "_worker_typing_enable"
            | "_worker_typing_disable"
            | "_worker_typing_set_sharing"
            | "_worker_typing_seal"
            | "_worker_typing_open"
    )
}

impl BrowserCore {
    /// Worker-private live typing calls. Hosts publish outcomes in generation, session
    /// token/revision, sequence, then deadline order; Host Task H owns that fence.
    pub(super) fn typing_command(&self, command: &str, args: Value) -> Result<Value, Failure> {
        match command {
            "_worker_typing_enable" => self.enable_typing(args),
            "_worker_typing_disable" => {
                no_args(args)?;
                self.client()?.disable_live_typing().map_err(core)?;
                Ok(json!({}))
            }
            "_worker_typing_set_sharing" => self.set_typing_sharing(args),
            "_worker_typing_seal" => self.seal_typing(args),
            "_worker_typing_open" => self.open_typing(args),
            _ => Err(unknown_command()),
        }
    }

    fn enable_typing(&self, args: Value) -> Result<Value, Failure> {
        let input: EnableInput = serde_json::from_value(args).map_err(|_| invalid())?;
        if input.trusted_roster.len() > 100 {
            return Err(invalid());
        }
        let context = LiveEnableContext {
            trusted_roster: parse_device_ids(input.trusted_roster)?,
            local_gateway_device_id: parse_device_id(&input.local_gateway_device_id)?,
            local_executor: input.executor.into(),
        };
        let generation = self
            .client()?
            .enable_live_typing(context, input.clock.into())
            .map_err(live_error)?;
        Ok(json!({"generation": generation.0.to_string()}))
    }

    fn set_typing_sharing(&self, args: Value) -> Result<Value, Failure> {
        let input: SharingInput = serde_json::from_value(args).map_err(|_| invalid())?;
        self.client()?
            .set_typing_sharing(input.sharing)
            .map_err(live_error)?;
        Ok(json!({}))
    }

    fn seal_typing(&self, args: Value) -> Result<Value, Failure> {
        let input: SealInput = serde_json::from_value(args).map_err(|_| invalid())?;
        let conversation_id = input.conversation_id.parse().map_err(|_| invalid())?;
        let gateway_device_id = parse_device_id(&input.gateway_device_id)?;
        let Some(outbound) = self
            .client()?
            .seal_typing_signal_v2(
                conversation_id,
                gateway_device_id,
                input.origin,
                input.state,
                input.clock.into(),
            )
            .map_err(live_error)?
        else {
            return Ok(Value::Null);
        };
        let frame_json =
            serde_json::to_string(&outbound.signal).map_err(|_| live_error(LiveError::Crypto))?;
        Ok(json!({
            "generation": outbound.generation.0.to_string(),
            "frameJson": frame_json,
        }))
    }

    fn open_typing(&self, args: Value) -> Result<Value, Failure> {
        let input: OpenInput = serde_json::from_value(args).map_err(|_| invalid())?;
        let outcome = self
            .client()?
            .open_typing_signal_v2_json(&input.frame_json, input.clock.into())
            .map_err(live_error)?;
        outcome_value(outcome)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnableInput {
    trusted_roster: Vec<String>,
    local_gateway_device_id: String,
    executor: ExecutorInput,
    clock: ClockInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExecutorInput {
    None,
    CurrentGateway,
}

impl From<ExecutorInput> for LiveExecutor {
    fn from(value: ExecutorInput) -> Self {
        match value {
            ExecutorInput::None => Self::None,
            ExecutorInput::CurrentGateway => Self::CurrentGateway,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClockInput {
    wall_ms: i64,
    mono_ms: u64,
}

impl From<ClockInput> for LiveClock {
    fn from(value: ClockInput) -> Self {
        Self {
            wall_ms: value.wall_ms,
            mono_ms: value.mono_ms,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SharingInput {
    sharing: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SealInput {
    conversation_id: String,
    gateway_device_id: String,
    origin: TypingOriginV2,
    state: TypingStateV2,
    clock: ClockInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OpenInput {
    frame_json: String,
    clock: ClockInput,
}

fn no_args(args: Value) -> Result<(), Failure> {
    match args {
        Value::Null => Ok(()),
        Value::Object(ref object) if object.is_empty() => Ok(()),
        _ => Err(invalid()),
    }
}

fn parse_device_ids(ids: Vec<String>) -> Result<Vec<DeviceId>, Failure> {
    ids.into_iter().map(|id| parse_device_id(&id)).collect()
}

fn parse_device_id(id: &str) -> Result<DeviceId, Failure> {
    id.parse().map_err(|_| invalid())
}

/// Outputs must be published by hosts in generation, session token/revision, sequence, then
/// deadline order. Host Task H owns the publication fence.
fn outcome_value(outcome: LiveOutcome) -> Result<Value, Failure> {
    let reply_frame_json = outcome
        .reply
        .map(|reply| serde_json::to_string(&reply).map_err(|_| live_error(LiveError::Crypto)))
        .transpose()?;
    Ok(json!({
        "generation": outcome.generation.0.to_string(),
        "sessionUpdate": outcome.session_update.map(session_update_value),
        "action": outcome.action.map(action_value),
        "replyFrameJson": reply_frame_json,
    }))
}

fn session_update_value(update: PeerSessionUpdate) -> Value {
    json!({
        "peerDeviceId": update.peer_device_id.to_string(),
        "replacedSessionToken": update.replaced_session_token.map(|value| value.to_string()),
        "currentSessionToken": update.current_session_token.to_string(),
        "validationRevision": update.validation_revision.to_string(),
    })
}

fn action_value(action: LiveAction) -> Value {
    let (kind, peer_device_id, session_token, sequence, conversation_id, state, deadline_mono) =
        match action {
            LiveAction::Display {
                peer_device_id,
                session_token,
                sequence,
                conversation_id,
                state,
                deadline_mono,
            } => (
                "display",
                peer_device_id,
                session_token,
                sequence,
                conversation_id,
                state,
                deadline_mono,
            ),
            LiveAction::GatewayTypingIntent {
                peer_device_id,
                session_token,
                sequence,
                conversation_id,
                state,
                deadline_mono,
            } => (
                "gateway_typing_intent",
                peer_device_id,
                session_token,
                sequence,
                conversation_id,
                state,
                deadline_mono,
            ),
        };
    json!({
        "kind": kind,
        "peerDeviceId": peer_device_id.to_string(),
        "sessionToken": session_token.to_string(),
        "sequence": sequence.to_string(),
        "conversationId": conversation_id.to_string(),
        "state": state,
        "deadlineMono": deadline_mono.to_string(),
    })
}

fn live_error(error: LiveError) -> Failure {
    match error {
        LiveError::Clock => Failure::new("clock", "The live typing clock is invalid."),
        LiveError::Disabled => Failure::new("disabled", "Live typing is disabled."),
        LiveError::CounterExhausted => {
            Failure::new("counter_exhausted", "Live typing capacity is exhausted.")
        }
        LiveError::Crypto => Failure::new("crypto", "Live typing could not be processed."),
        LiveError::InvalidContext => {
            Failure::new("invalid_context", "The live typing context is invalid.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BrowserCore;
    use peppy_client_core::{ConversationId, VaultId};
    use peppy_crypto::{KeyProfile, create_vault_check_header, derive_root_key};
    use peppy_protocol::{EphemeralSignalV2, EphemeralSignalV2Kind};

    fn response(core: &mut BrowserCore, command: &str, args: Value) -> Value {
        serde_json::from_str(&core.dispatch(&json!({"command": command, "args": args}).to_string()))
            .unwrap()
    }

    fn unlocked_core(vault: VaultId, device: DeviceId) -> (tempfile::TempDir, BrowserCore) {
        let directory = tempfile::tempdir().unwrap();
        let mut core = BrowserCore::new(directory.path().to_owned());
        assert_eq!(
            response(
                &mut core,
                "open",
                json!({
                    "vaultId": vault,
                    "deviceId": device,
                    "databaseKey": vec![1; 32],
                    "origin": "https://peppy.test",
                    "deviceRole": "device",
                })
            )["ok"],
            true
        );
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("test passphrase", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        assert_eq!(
            response(
                &mut core,
                "unlock",
                json!({
                    "profile": profile,
                    "header": header,
                    "passphrase": "test passphrase",
                })
            )["ok"],
            true
        );
        core.client()
            .unwrap()
            .set_server_compaction_state(true, true)
            .unwrap();
        (directory, core)
    }

    fn enable(core: &mut BrowserCore, roster: &[DeviceId], gateway: DeviceId, executor: &str) {
        assert_eq!(
            response(
                core,
                "_worker_typing_enable",
                json!({
                    "trustedRoster": roster,
                    "localGatewayDeviceId": gateway,
                    "executor": executor,
                    "clock": {"wallMs": 1000, "monoMs": 1000},
                })
            )["ok"],
            true
        );
    }

    #[test]
    fn disabled_enablement_is_reported_with_a_fixed_code() {
        let vault = VaultId::new();
        let (_directory, mut core) = unlocked_core(vault, DeviceId::new());
        let response = response(
            &mut core,
            "_worker_typing_set_sharing",
            json!({"sharing": true}),
        );
        assert_eq!(response["error"]["code"], "disabled");
    }

    #[test]
    fn invalid_roster_is_reported_with_a_fixed_code() {
        let vault = VaultId::new();
        let device = DeviceId::new();
        let (_directory, mut core) = unlocked_core(vault, device);
        let response = response(
            &mut core,
            "_worker_typing_enable",
            json!({
                "trustedRoster": [],
                "localGatewayDeviceId": device,
                "executor": "current_gateway",
                "clock": {"wallMs": 1000, "monoMs": 1000},
            }),
        );
        assert_eq!(response["error"]["code"], "invalid_context");
    }

    #[test]
    fn outcomes_serialize_a_tagged_action_and_private_reply() {
        let peer = DeviceId::new();
        let conversation = ConversationId::new();
        let value = outcome_value(LiveOutcome {
            generation: peppy_client_core::LiveEnableGeneration((1_u64 << 53) + 1),
            session_update: Some(PeerSessionUpdate {
                peer_device_id: peer,
                replaced_session_token: Some(7),
                current_session_token: 8,
                validation_revision: 9,
            }),
            action: Some(LiveAction::GatewayTypingIntent {
                peer_device_id: peer,
                session_token: 8,
                sequence: (1_u64 << 53) + 1,
                conversation_id: conversation,
                state: TypingStateV2::Started,
                deadline_mono: (1_u64 << 53) + 2,
            }),
            reply: Some(EphemeralSignalV2 {
                protocol_version: 2,
                kind: EphemeralSignalV2Kind::Challenge,
                sender_device_id: peer,
                key_epoch: 1,
                session_id: "AAAAAAAAAAAAAAAAAAAAAA".into(),
                sent_at_ms: "1".into(),
                expires_at_ms: "2".into(),
                sequence: "1".into(),
                ciphertext_b64: "AA==".into(),
            }),
        })
        .unwrap();

        assert_eq!(value["action"]["kind"], "gateway_typing_intent");
        assert!(value["replyFrameJson"].is_string());
        assert_eq!(value["generation"], ((1_u64 << 53) + 1).to_string());
        assert!(value.get("ciphertextB64").is_none());
    }

    #[test]
    fn serializes_large_u64_values_as_decimal_strings() {
        let action = action_value(LiveAction::Display {
            peer_device_id: DeviceId::new(),
            session_token: u64::MAX,
            sequence: (1_u64 << 53) + 1,
            conversation_id: ConversationId::new(),
            state: TypingStateV2::Started,
            deadline_mono: u64::MAX,
        });
        assert_eq!(action["sessionToken"], u64::MAX.to_string());
        assert_eq!(action["sequence"], ((1_u64 << 53) + 1).to_string());
        assert_eq!(action["deadlineMono"], u64::MAX.to_string());
    }

    #[test]
    fn malformed_frames_return_an_empty_outcome() {
        let vault = VaultId::new();
        let device = DeviceId::new();
        let (_directory, mut core) = unlocked_core(vault, device);
        enable(&mut core, &[device], device, "current_gateway");
        let opened = response(
            &mut core,
            "_worker_typing_open",
            json!({
                "frameJson": "not-json",
                "clock": {"wallMs": 1001, "monoMs": 1001},
            }),
        );
        assert_eq!(opened["ok"], true);
        assert_eq!(opened["value"]["generation"], "1");
        assert!(opened["value"]["sessionUpdate"].is_null());
        assert!(opened["value"]["action"].is_null());
        assert!(opened["value"]["replyFrameJson"].is_null());
    }
}
