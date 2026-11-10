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
            return Err(live_error(LiveError::InvalidContext));
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
        let conversation_id = input
            .conversation_id
            .parse()
            .map_err(|_| live_error(LiveError::InvalidContext))?;
        let gateway_device_id = parse_device_id(&input.gateway_device_id)?;
        let Some(outbound) = self
            .client()?
            .seal_typing_signal_v2(
                conversation_id,
                gateway_device_id,
                input.origin,
                input.participant,
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
    participant: Option<String>,
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
    id.parse()
        .map_err(|_| live_error(LiveError::InvalidContext))
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
    let (
        kind,
        peer_device_id,
        session_token,
        sequence,
        conversation_id,
        participant,
        state,
        deadline_mono,
    ) = match action {
        LiveAction::Display {
            peer_device_id,
            session_token,
            sequence,
            conversation_id,
            participant,
            state,
            deadline_mono,
        } => (
            "display",
            peer_device_id,
            session_token,
            sequence,
            conversation_id,
            participant,
            state,
            deadline_mono,
        ),
        LiveAction::GatewayTypingIntent {
            peer_device_id,
            session_token,
            sequence,
            conversation_id,
            participant,
            state,
            deadline_mono,
        } => (
            "gateway_typing_intent",
            peer_device_id,
            session_token,
            sequence,
            conversation_id,
            participant,
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
        "participant": participant,
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
    use peppy_client_core::{
        ConversationId, RcsCapabilityState, RcsEligibility, RcsEligibilityProjection,
        RcsLinkStatusEvent, VaultId,
    };
    use peppy_crypto::{KeyProfile, create_vault_check_header, derive_root_key};

    fn response(core: &mut BrowserCore, command: &str, args: Value) -> Value {
        serde_json::from_str(&core.dispatch(&json!({"command": command, "args": args}).to_string()))
            .unwrap()
    }

    fn unlocked_core(vault: VaultId, device: DeviceId) -> (tempfile::TempDir, BrowserCore) {
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("test passphrase", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        unlocked_core_with_material(vault, device, profile, header)
    }

    fn unlocked_core_with_material(
        vault: VaultId,
        device: DeviceId,
        profile: KeyProfile,
        header: peppy_crypto::VaultCheckHeader,
    ) -> (tempfile::TempDir, BrowserCore) {
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

    fn assert_keys(value: &Value, keys: &[&str]) {
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), keys.len());
        assert!(keys.iter().all(|key| object.contains_key(*key)));
    }

    fn deliver_gateway_capability(
        gateway: &mut BrowserCore,
        recipient: &mut BrowserCore,
        conversation: ConversationId,
        gateway_device: DeviceId,
    ) {
        gateway
            .client()
            .unwrap()
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    gateway_device_id: gateway_device,
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
                    gateway_device_id: gateway_device,
                }],
                1_000,
            )
            .unwrap();
        let pending = response(gateway, "pending_outbox", json!({"limit": 10}));
        let envelopes = pending["value"]["envelopes"].as_array().unwrap();
        assert_eq!(envelopes.len(), 2);
        for (index, envelope) in envelopes.iter().enumerate() {
            let ingested = response(
                recipient,
                "ingest",
                json!({"cursor": (index + 1).to_string(), "envelope": envelope}),
            );
            assert_eq!(ingested["ok"], true);
            assert_eq!(ingested["value"]["state"], "journaled");
        }
        let applied = response(recipient, "apply_pending", json!({"limit": 10}));
        assert_eq!(applied["ok"], true);
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
    fn malformed_typing_identifiers_are_invalid_context() {
        let vault = VaultId::new();
        let device = DeviceId::new();
        let (_directory, mut core) = unlocked_core(vault, device);
        let oversized_roster = vec![device.to_string(); 101];
        for args in [
            json!({
                "trustedRoster": ["not-a-device"],
                "localGatewayDeviceId": device,
                "executor": "current_gateway",
                "clock": {"wallMs": 1000, "monoMs": 1000},
            }),
            json!({
                "trustedRoster": oversized_roster,
                "localGatewayDeviceId": device,
                "executor": "current_gateway",
                "clock": {"wallMs": 1000, "monoMs": 1000},
            }),
        ] {
            assert_eq!(
                response(&mut core, "_worker_typing_enable", args)["error"]["code"],
                "invalid_context"
            );
        }
        enable(&mut core, &[device], device, "current_gateway");
        for args in [
            json!({
                "conversationId": "not-a-conversation",
                "gatewayDeviceId": device,
                "origin": "vault_device",
                "state": "started",
                "clock": {"wallMs": 1001, "monoMs": 1001},
            }),
            json!({
                "conversationId": ConversationId::new(),
                "gatewayDeviceId": "not-a-device",
                "origin": "vault_device",
                "state": "started",
                "clock": {"wallMs": 1001, "monoMs": 1001},
            }),
        ] {
            assert_eq!(
                response(&mut core, "_worker_typing_seal", args)["error"]["code"],
                "invalid_context"
            );
        }
    }

    #[test]
    fn two_clients_dispatch_a_typed_handshake_and_action() {
        let vault = VaultId::new();
        let sender_device = DeviceId::new();
        let gateway_device = DeviceId::new();
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("test passphrase", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        let (_sender_directory, mut sender) =
            unlocked_core_with_material(vault, sender_device, profile.clone(), header.clone());
        let (_gateway_directory, mut gateway) =
            unlocked_core_with_material(vault, gateway_device, profile, header);
        let conversation = ConversationId::new();
        deliver_gateway_capability(&mut gateway, &mut sender, conversation, gateway_device);
        let roster = [sender_device, gateway_device];
        enable(&mut sender, &roster, sender_device, "none");
        enable(&mut gateway, &roster, gateway_device, "current_gateway");

        let sealed = response(
            &mut sender,
            "_worker_typing_seal",
            json!({
                "conversationId": conversation,
                "gatewayDeviceId": gateway_device,
                "origin": "vault_device",
                "state": "started",
                "clock": {"wallMs": 1001, "monoMs": 1001},
            }),
        );
        assert_keys(&sealed["value"], &["generation", "frameJson"]);
        let challenge = response(
            &mut gateway,
            "_worker_typing_open",
            json!({
                "frameJson": sealed["value"]["frameJson"],
                "clock": {"wallMs": 1001, "monoMs": 1001},
            }),
        );
        assert_keys(
            &challenge["value"],
            &["generation", "sessionUpdate", "action", "replyFrameJson"],
        );
        assert!(challenge["value"]["replyFrameJson"].is_string());
        let acknowledgement = response(
            &mut sender,
            "_worker_typing_open",
            json!({
                "frameJson": challenge["value"]["replyFrameJson"],
                "clock": {"wallMs": 1002, "monoMs": 1002},
            }),
        );
        assert_keys(
            &acknowledgement["value"],
            &["generation", "sessionUpdate", "action", "replyFrameJson"],
        );
        assert!(acknowledgement["value"]["replyFrameJson"].is_string());
        let installed = response(
            &mut gateway,
            "_worker_typing_open",
            json!({
                "frameJson": acknowledgement["value"]["replyFrameJson"],
                "clock": {"wallMs": 1003, "monoMs": 1003},
            }),
        );
        assert_keys(
            &installed["value"],
            &["generation", "sessionUpdate", "action", "replyFrameJson"],
        );
        assert_keys(
            &installed["value"]["sessionUpdate"],
            &[
                "peerDeviceId",
                "replacedSessionToken",
                "currentSessionToken",
                "validationRevision",
            ],
        );

        let fresh = response(
            &mut sender,
            "_worker_typing_seal",
            json!({
                "conversationId": conversation,
                "gatewayDeviceId": gateway_device,
                "origin": "vault_device",
                "state": "started",
                "clock": {"wallMs": 4001, "monoMs": 4001},
            }),
        );
        let opened = response(
            &mut gateway,
            "_worker_typing_open",
            json!({
                "frameJson": fresh["value"]["frameJson"],
                "clock": {"wallMs": 4001, "monoMs": 4001},
            }),
        );
        assert_keys(
            &opened["value"]["action"],
            &[
                "kind",
                "peerDeviceId",
                "sessionToken",
                "sequence",
                "conversationId",
                "participant",
                "state",
                "deadlineMono",
            ],
        );
        assert_eq!(opened["value"]["action"]["kind"], "gateway_typing_intent");
        assert_eq!(
            opened["value"]["action"]["conversationId"],
            conversation.to_string()
        );
    }

    #[test]
    fn serializes_large_u64_values_as_decimal_strings() {
        let action = action_value(LiveAction::Display {
            peer_device_id: DeviceId::new(),
            session_token: u64::MAX,
            sequence: (1_u64 << 53) + 1,
            conversation_id: ConversationId::new(),
            participant: None,
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
