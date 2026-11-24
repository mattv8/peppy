use crate::{BrowserCore, Failure, core, invalid, now_ms, unknown_command};
use peppy_client_core::{
    Client, ConversationId, DeviceId, LiveAction, LiveClock, LiveEnableContext, LiveError,
    LiveExecutor, LiveOutcome, PeerSessionUpdate,
};
use peppy_desktop_api::rcs_status::{self, RcsStatusSnapshot, SnapshotAccess};
use peppy_protocol::{TypingOriginV2, TypingStateV2};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;

const MAX_TRUSTED_ROSTER: usize = 100;

pub(super) fn handles(command: &str) -> bool {
    matches!(
        command,
        "_worker_typing_enable"
            | "_worker_typing_disable"
            | "_worker_typing_set_sharing"
            | "_worker_typing_seal"
            | "_worker_typing_open"
            | "_worker_typing_context_key"
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
            "_worker_typing_context_key" => {
                no_args(args)?;
                Ok(json!({"key": self.typing_trust().map(|trust| trust.key())}))
            }
            _ => Err(unknown_command()),
        }
    }

    fn enable_typing(&self, args: Value) -> Result<Value, Failure> {
        let input: EnableInput = serde_json::from_value(args).map_err(|_| invalid())?;
        let context = self
            .typing_trust()
            .ok_or_else(|| live_error(LiveError::InvalidContext))?
            .enable_context();
        let generation = self
            .client()?
            .enable_live_typing(context, input.clock.into())
            .map_err(live_error)?;
        Ok(json!({"generation": generation.0.to_string()}))
    }

    /// Trust for live typing comes only from the verified server roster and the local core:
    /// a validated non-empty roster, one current linked gateway inside it, and an unlocked
    /// active epoch. Renderer arguments never contribute.
    fn typing_trust(&self) -> Option<TypingTrust> {
        let client = self.client().ok()?;
        if !self.context.rcs_roster_validated || !roster_is_usable(&self.context.rcs_live_roster) {
            return None;
        }
        let epoch = active_unlocked_epoch(client)?;
        let snapshot = self.rcs_snapshot(client, None, true)?;
        let gateway = rcs_status::unique_current_linked_gateway(&snapshot)?;
        self.context
            .rcs_live_roster
            .contains(&gateway)
            .then(|| TypingTrust {
                roster: self.context.rcs_live_roster.clone(),
                gateway,
                epoch,
            })
    }

    /// Mirrors the desktop target lookup: the one eligible current linked gateway for a
    /// conversation, from a snapshot that treats that conversation as the active one.
    fn resolve_typing_target(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<DeviceId>, Failure> {
        let client = self.client()?;
        let unlocked = active_unlocked_epoch(client).is_some();
        Ok(self
            .rcs_snapshot(client, Some(conversation_id), unlocked)
            .and_then(|snapshot| rcs_status::typing_target(&snapshot, conversation_id)))
    }

    fn rcs_snapshot(
        &self,
        client: &Client,
        active_conversation: Option<ConversationId>,
        unlocked: bool,
    ) -> Option<RcsStatusSnapshot> {
        rcs_status::snapshot_at(
            client,
            self.context.gateways.as_deref().unwrap_or_default(),
            &self.context.revoked_device_ids,
            SnapshotAccess {
                active_conversation,
                active_is_existing: active_conversation.is_some(),
                roster_validated: self.context.rcs_roster_validated,
                unlocked,
                now_ms: now_ms(),
            },
        )
        .ok()
        .flatten()
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
        let conversation_id: ConversationId = input
            .conversation_id
            .parse()
            .map_err(|_| live_error(LiveError::InvalidContext))?;
        let gateway_device_id = match input.gateway_device_id {
            Some(gateway) => parse_device_id(&gateway)?,
            None => match self.resolve_typing_target(conversation_id)? {
                Some(gateway) => gateway,
                None => return Ok(Value::Null),
            },
        };
        let Some(outbound) = self
            .client()?
            .seal_typing_signal_v2(
                conversation_id,
                gateway_device_id,
                input.origin.unwrap_or(TypingOriginV2::VaultDevice),
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

/// The verified context that live typing may be enabled with, derived in Rust.
struct TypingTrust {
    roster: Vec<DeviceId>,
    gateway: DeviceId,
    epoch: u32,
}

impl TypingTrust {
    fn enable_context(self) -> LiveEnableContext {
        LiveEnableContext {
            trusted_roster: self.roster,
            local_gateway_device_id: self.gateway,
            local_executor: LiveExecutor::None,
        }
    }

    /// Opaque, stable for one roster, gateway and epoch. Sorted ids keep the key order-free.
    fn key(&self) -> String {
        let mut roster: Vec<String> = self.roster.iter().map(ToString::to_string).collect();
        roster.sort();
        format!("{}|{}|{}", roster.join(","), self.gateway, self.epoch)
    }
}

/// Enable accepts at most `MAX_TRUSTED_ROSTER` distinct devices. An over-large or duplicated
/// roster is refused whole, never truncated or deduplicated, so key and enable always agree.
fn roster_is_usable(roster: &[DeviceId]) -> bool {
    let distinct: HashSet<&DeviceId> = roster.iter().collect();
    !roster.is_empty() && roster.len() <= MAX_TRUSTED_ROSTER && distinct.len() == roster.len()
}

/// The active epoch when it is unlocked; `None` after a lock or with no active key.
fn active_unlocked_epoch(client: &Client) -> Option<u32> {
    let keys = client.key_status().ok()?;
    keys.active_epoch
        .filter(|epoch| keys.unlocked_epochs.contains(epoch))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnableInput {
    clock: ClockInput,
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
    gateway_device_id: Option<String>,
    /// Defaults to the vault device, the only origin the minimal request names.
    origin: Option<TypingOriginV2>,
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
    let follow_up_frames_json = outcome
        .follow_ups
        .iter()
        .map(|frame| serde_json::to_string(frame).map_err(|_| live_error(LiveError::Crypto)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "generation": outcome.generation.0.to_string(),
        "sessionUpdate": outcome.session_update.map(session_update_value),
        "action": outcome.action.map(action_value),
        "replyFrameJson": reply_frame_json,
        "followUpFramesJson": follow_up_frames_json,
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
            Failure::new("counter-exhausted", "Live typing capacity is exhausted.")
        }
        LiveError::Crypto => Failure::new("crypto", "Live typing could not be processed."),
        LiveError::InvalidContext => {
            Failure::new("invalid-context", "The live typing context is invalid.")
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

    fn vault_material(vault: VaultId) -> (KeyProfile, peppy_crypto::VaultCheckHeader) {
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("test passphrase", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        (profile, header)
    }

    fn unlocked_core(vault: VaultId, device: DeviceId) -> (tempfile::TempDir, BrowserCore) {
        let (profile, header) = vault_material(vault);
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

    fn enable(core: &mut BrowserCore) {
        assert_eq!(
            response(
                core,
                "_worker_typing_enable",
                json!({"clock": wall_clock()})
            )["ok"],
            true
        );
    }

    fn assert_keys(value: &Value, keys: &[&str]) {
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), keys.len());
        assert!(keys.iter().all(|key| object.contains_key(*key)));
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
    fn caller_supplied_trust_fields_are_rejected_and_typing_stays_disabled() {
        let (_directory, mut core, device, gateway) = trusted_core(Vec::new());
        for args in [
            json!({"trustedRoster": [device, gateway], "clock": wall_clock()}),
            json!({"localGatewayDeviceId": gateway, "clock": wall_clock()}),
            json!({"executor": "none", "clock": wall_clock()}),
            json!({"trustedRoster": [device, gateway], "localGatewayDeviceId": gateway, "executor": "none", "clock": wall_clock()}),
        ] {
            let rejected = response(&mut core, "_worker_typing_enable", args);
            assert_eq!(rejected["error"]["code"], "invalid-request", "{rejected}");
            let disabled = response(
                &mut core,
                "_worker_typing_set_sharing",
                json!({"sharing": true}),
            );
            assert_eq!(disabled["error"]["code"], "disabled", "{disabled}");
        }
    }

    #[test]
    fn malformed_typing_identifiers_are_invalid_context() {
        let (_directory, mut core, device, _gateway) = trusted_core(Vec::new());
        enable(&mut core);
        for args in [
            json!({
                "conversationId": "not-a-conversation",
                "gatewayDeviceId": device,
                "origin": "vault_device",
                "state": "started",
                "clock": wall_clock(),
            }),
            json!({
                "conversationId": ConversationId::new(),
                "gatewayDeviceId": "not-a-device",
                "origin": "vault_device",
                "state": "started",
                "clock": wall_clock(),
            }),
        ] {
            assert_eq!(
                response(&mut core, "_worker_typing_seal", args)["error"]["code"],
                "invalid-context"
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
        assert_eq!(
            deliver_fresh_status(
                &mut gateway,
                &mut sender,
                gateway_device,
                vec![(conversation, RcsEligibility::Eligible)],
                0,
            ),
            2
        );
        let roster = [sender_device, gateway_device];
        apply_roster(&mut sender, sender_device, &[gateway_device], &[]);
        enable(&mut sender);
        // A native gateway executes intents; browser workers never receive that authority.
        gateway
            .client()
            .unwrap()
            .enable_live_typing(
                LiveEnableContext {
                    trusted_roster: roster.to_vec(),
                    local_gateway_device_id: gateway_device,
                    local_executor: LiveExecutor::CurrentGateway,
                },
                serde_json::from_value::<ClockInput>(wall_clock())
                    .unwrap()
                    .into(),
            )
            .unwrap();
        let wall = now_ms();

        let sealed = response(
            &mut sender,
            "_worker_typing_seal",
            json!({
                "conversationId": conversation,
                "gatewayDeviceId": gateway_device,
                "origin": "vault_device",
                "state": "started",
                "clock": {"wallMs": wall + 1, "monoMs": 1001},
            }),
        );
        assert_keys(&sealed["value"], &["generation", "frameJson"]);
        let challenge = response(
            &mut gateway,
            "_worker_typing_open",
            json!({
                "frameJson": sealed["value"]["frameJson"],
                "clock": {"wallMs": wall + 1, "monoMs": 1001},
            }),
        );
        assert_keys(
            &challenge["value"],
            &[
                "generation",
                "sessionUpdate",
                "action",
                "replyFrameJson",
                "followUpFramesJson",
            ],
        );
        assert!(challenge["value"]["replyFrameJson"].is_string());
        assert_eq!(challenge["value"]["followUpFramesJson"], json!([]));
        let acknowledgement = response(
            &mut sender,
            "_worker_typing_open",
            json!({
                "frameJson": challenge["value"]["replyFrameJson"],
                "clock": {"wallMs": wall + 2, "monoMs": 1002},
            }),
        );
        assert_keys(
            &acknowledgement["value"],
            &[
                "generation",
                "sessionUpdate",
                "action",
                "replyFrameJson",
                "followUpFramesJson",
            ],
        );
        assert!(acknowledgement["value"]["replyFrameJson"].is_string());
        // The challenge was answered while the start was active: one fresh start follows the ack.
        assert_eq!(
            acknowledgement["value"]["followUpFramesJson"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        let installed = response(
            &mut gateway,
            "_worker_typing_open",
            json!({
                "frameJson": acknowledgement["value"]["replyFrameJson"],
                "clock": {"wallMs": wall + 3, "monoMs": 1003},
            }),
        );
        assert_keys(
            &installed["value"],
            &[
                "generation",
                "sessionUpdate",
                "action",
                "replyFrameJson",
                "followUpFramesJson",
            ],
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
                "clock": {"wallMs": wall + 3001, "monoMs": 4001},
            }),
        );
        let opened = response(
            &mut gateway,
            "_worker_typing_open",
            json!({
                "frameJson": fresh["value"]["frameJson"],
                "clock": {"wallMs": wall + 3001, "monoMs": 4001},
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

    /// Applies a verified roster: this device, its gateways, and extra non-gateway devices.
    fn apply_roster(
        core: &mut BrowserCore,
        device: DeviceId,
        gateways: &[DeviceId],
        extra_devices: &[DeviceId],
    ) {
        let mut devices = vec![json!({"device_id": device, "role": "device", "revoked": false})];
        devices.extend(
            gateways
                .iter()
                .map(|gateway| json!({"device_id": gateway, "role": "gateway", "revoked": false})),
        );
        devices.extend(
            extra_devices
                .iter()
                .map(|other| json!({"device_id": other, "role": "device", "revoked": false})),
        );
        let capabilities: Vec<Value> = gateways
            .iter()
            .map(|gateway| {
                json!({"device_id": gateway, "simulator": false, "capabilities": {"sims": [
                    {"subscription_id": "sim-1", "sms": "available", "mms": "available", "mms_content_version": 2}
                ]}})
            })
            .collect();
        let applied = response(
            core,
            "_worker_apply_server_context",
            json!({"connection": "connected", "devices": {"devices": devices}, "capabilities": {"capabilities": capabilities}}),
        );
        assert_eq!(applied["ok"], true, "{applied}");
    }

    /// A gateway publishes only from its own core, so its fresh linked status and eligibilities
    /// reach the recipient as journaled envelopes. Cursors must stay contiguous, so the caller
    /// passes the last cursor used and receives the new one.
    fn deliver_fresh_status(
        gateway_core: &mut BrowserCore,
        recipient: &mut BrowserCore,
        gateway: DeviceId,
        eligibilities: Vec<(ConversationId, RcsEligibility)>,
        last_cursor: u64,
    ) -> u64 {
        let now = now_ms();
        let projections: Vec<RcsEligibilityProjection> = eligibilities
            .into_iter()
            .map(|(conversation, eligibility)| RcsEligibilityProjection {
                conversation_id: conversation,
                eligibility,
                feature_bits: 1,
                mapping_revision: 1,
                observed_at_ms: now,
                gateway_device_id: gateway,
            })
            .collect();
        gateway_core
            .client()
            .unwrap()
            .publish_rcs_capability(
                &RcsLinkStatusEvent {
                    gateway_device_id: gateway,
                    state: RcsCapabilityState::Linked,
                    availability: true,
                    health: true,
                    phone_responsive: true,
                    observed_at_ms: now,
                    capability_revision: 1,
                    feature_bits: 1,
                    linked_since_ms: Some(now),
                },
                &projections,
                now,
            )
            .unwrap();
        let pending = response(gateway_core, "pending_outbox", json!({"limit": 10}));
        let envelopes = pending["value"]["envelopes"].as_array().unwrap();
        let mut cursor = last_cursor;
        for envelope in envelopes {
            cursor += 1;
            let ingested = response(
                recipient,
                "ingest",
                json!({"cursor": cursor.to_string(), "envelope": envelope}),
            );
            assert_eq!(ingested["ok"], true, "{ingested}");
            assert_eq!(ingested["value"]["state"], "journaled", "{ingested}");
        }
        let applied = response(recipient, "apply_pending", json!({"limit": 10}));
        assert_eq!(applied["ok"], true, "{applied}");
        cursor
    }

    /// An unlocked core whose verified roster holds this device and one linked gateway.
    fn trusted_core(
        eligibilities: Vec<(ConversationId, RcsEligibility)>,
    ) -> (tempfile::TempDir, BrowserCore, DeviceId, DeviceId) {
        trusted_core_in(VaultId::new(), eligibilities)
    }

    fn trusted_core_in(
        vault: VaultId,
        eligibilities: Vec<(ConversationId, RcsEligibility)>,
    ) -> (tempfile::TempDir, BrowserCore, DeviceId, DeviceId) {
        let device = DeviceId::new();
        let gateway = DeviceId::new();
        let (profile, header) = vault_material(vault);
        let (directory, mut core) =
            unlocked_core_with_material(vault, device, profile.clone(), header.clone());
        let (_gateway_directory, mut gateway_core) =
            unlocked_core_with_material(vault, gateway, profile, header);
        apply_roster(&mut core, device, &[gateway], &[]);
        deliver_fresh_status(&mut gateway_core, &mut core, gateway, eligibilities, 0);
        (directory, core, device, gateway)
    }

    /// Core currentness is judged against the clock the host passes in, so these tests use wall time.
    fn wall_clock() -> Value {
        json!({"wallMs": now_ms(), "monoMs": 1000})
    }

    fn context_key(core: &mut BrowserCore) -> Value {
        let key = response(core, "_worker_typing_context_key", json!({}));
        assert_eq!(key["ok"], true, "{key}");
        key["value"]["key"].clone()
    }

    /// Every file under the core root, with its bytes, in path order.
    fn database_files(root: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_owned()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let bytes = std::fs::read(&path).unwrap();
                    files.push((path, bytes));
                }
            }
        }
        files.sort();
        files
    }

    fn is_worker_error_code(code: &str) -> bool {
        let mut characters = code.chars();
        code.len() <= 64
            && characters
                .next()
                .is_some_and(|first| first.is_ascii_lowercase())
            && characters.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    }

    #[test]
    fn typing_failure_codes_match_the_worker_rejection_grammar() {
        for failure in [
            LiveError::Clock,
            LiveError::Disabled,
            LiveError::CounterExhausted,
            LiveError::Crypto,
            LiveError::InvalidContext,
        ]
        .map(live_error)
        {
            assert!(is_worker_error_code(failure.code), "{}", failure.code);
        }
        assert!(is_worker_error_code(invalid().code));
        assert!(!is_worker_error_code("invalid_context"));
    }

    #[test]
    fn context_key_is_null_without_a_verified_roster() {
        let (_directory, mut core) = unlocked_core(VaultId::new(), DeviceId::new());
        assert_eq!(context_key(&mut core), Value::Null);
    }

    #[test]
    fn context_key_is_null_with_two_linked_gateways() {
        let vault = VaultId::new();
        let device = DeviceId::new();
        let gateways = [DeviceId::new(), DeviceId::new()];
        let (profile, header) = vault_material(vault);
        let (_directory, mut core) =
            unlocked_core_with_material(vault, device, profile.clone(), header.clone());
        apply_roster(&mut core, device, &gateways, &[]);
        let mut cursor = 0;
        for gateway in gateways {
            let (_gateway_directory, mut gateway_core) =
                unlocked_core_with_material(vault, gateway, profile.clone(), header.clone());
            cursor =
                deliver_fresh_status(&mut gateway_core, &mut core, gateway, Vec::new(), cursor);
        }
        assert_eq!(context_key(&mut core), Value::Null);
    }

    #[test]
    fn context_key_is_stable_and_changes_with_the_roster() {
        let (_directory, mut core, device, gateway) = trusted_core(Vec::new());
        let key = context_key(&mut core);
        assert!(key.is_string());
        assert_eq!(context_key(&mut core), key);

        apply_roster(&mut core, device, &[gateway], &[DeviceId::new()]);
        let changed = context_key(&mut core);
        assert!(changed.is_string());
        assert_ne!(changed, key);
    }

    fn filler_devices(count: usize) -> Vec<DeviceId> {
        (0..count).map(|_| DeviceId::new()).collect()
    }

    #[test]
    fn roster_size_limit_agrees_between_key_and_enable() {
        let (_directory, mut core, device, gateway) = trusted_core(Vec::new());
        // This device, its gateway and 98 others: exactly the 100-device limit.
        apply_roster(&mut core, device, &[gateway], &filler_devices(98));
        assert!(context_key(&mut core).is_string());
        let enabled = response(
            &mut core,
            "_worker_typing_enable",
            json!({"clock": wall_clock()}),
        );
        assert_eq!(enabled["ok"], true, "{enabled}");

        // One more device: no context, so neither the key nor enable is available.
        apply_roster(&mut core, device, &[gateway], &filler_devices(99));
        assert_eq!(context_key(&mut core), Value::Null);
        let rejected = response(
            &mut core,
            "_worker_typing_enable",
            json!({"clock": wall_clock()}),
        );
        assert_eq!(rejected["error"]["code"], "invalid-context", "{rejected}");
    }

    #[test]
    fn duplicated_roster_device_yields_no_context() {
        let (_directory, mut core, device, gateway) = trusted_core(Vec::new());
        // The gateway listed twice is refused whole: never deduplicated silently.
        apply_roster(&mut core, device, &[gateway], &[gateway]);
        assert_eq!(context_key(&mut core), Value::Null);
        let rejected = response(
            &mut core,
            "_worker_typing_enable",
            json!({"clock": wall_clock()}),
        );
        assert_eq!(rejected["error"]["code"], "invalid-context", "{rejected}");
    }

    #[test]
    fn empty_roster_and_missing_gateway_refuse_trust() {
        let (_directory, mut core, device, gateway) = trusted_core(Vec::new());
        for roster in [vec![], vec![device]] {
            core.context.rcs_live_roster = roster;
            assert_eq!(context_key(&mut core), Value::Null);
            let rejected = response(
                &mut core,
                "_worker_typing_enable",
                json!({"clock": wall_clock()}),
            );
            assert_eq!(rejected["error"]["code"], "invalid-context", "{rejected}");
        }
        assert_ne!(device, gateway);
    }

    #[test]
    fn context_key_changes_when_the_gateway_changes() {
        let vault = VaultId::new();
        let device = DeviceId::new();
        let first = DeviceId::new();
        let second = DeviceId::new();
        let (profile, header) = vault_material(vault);
        let (_directory, mut core) =
            unlocked_core_with_material(vault, device, profile.clone(), header.clone());
        let mut cursor = 0;
        for gateway in [first, second] {
            let (_gateway_directory, mut gateway_core) =
                unlocked_core_with_material(vault, gateway, profile.clone(), header.clone());
            cursor =
                deliver_fresh_status(&mut gateway_core, &mut core, gateway, Vec::new(), cursor);
        }
        // The same three devices stay in the roster; only which one is the gateway changes.
        apply_roster(&mut core, device, &[first], &[second]);
        let with_first = context_key(&mut core);
        assert!(with_first.as_str().unwrap().contains(&format!("|{first}|")));
        apply_roster(&mut core, device, &[second], &[first]);
        let with_second = context_key(&mut core);
        assert!(
            with_second
                .as_str()
                .unwrap()
                .contains(&format!("|{second}|"))
        );
        assert_ne!(with_first, with_second);
    }

    #[test]
    fn context_key_changes_when_the_active_epoch_changes() {
        let vault = VaultId::new();
        let (_directory, mut core, _device, _gateway) = trusted_core_in(vault, Vec::new());
        let first = context_key(&mut core);
        assert!(first.as_str().unwrap().ends_with("|1"), "{first}");

        let profile = KeyProfile::new(vault.0, 2).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("test passphrase", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        let unlocked = response(
            &mut core,
            "unlock",
            json!({"profile": profile, "header": header, "passphrase": "test passphrase"}),
        );
        assert_eq!(unlocked["ok"], true, "{unlocked}");
        let activated = response(&mut core, "activate_epoch", json!({"epoch": "2"}));
        assert_eq!(activated["ok"], true, "{activated}");

        let second = context_key(&mut core);
        assert!(second.as_str().unwrap().ends_with("|2"), "{second}");
        assert_ne!(first, second);
    }

    #[test]
    fn context_key_is_null_once_the_client_is_closed() {
        let (_directory, mut core, _device, _gateway) = trusted_core(Vec::new());
        assert!(context_key(&mut core).is_string());
        // Closing is the browser's lock path: it drops the client and the verified roster.
        let closed = response(&mut core, "close", json!({}));
        assert_eq!(closed["ok"], true, "{closed}");
        assert_eq!(context_key(&mut core), Value::Null);
    }

    #[test]
    fn argument_free_enable_uses_the_verified_context() {
        let (_directory, mut core, _device, _gateway) = trusted_core(Vec::new());
        let enabled = response(
            &mut core,
            "_worker_typing_enable",
            json!({"clock": wall_clock()}),
        );
        assert_eq!(enabled["ok"], true, "{enabled}");
        assert!(enabled["value"]["generation"].is_string());
    }

    #[test]
    fn argument_free_enable_without_a_verified_context_is_invalid() {
        let (_directory, mut core) = unlocked_core(VaultId::new(), DeviceId::new());
        let rejected = response(
            &mut core,
            "_worker_typing_enable",
            json!({"clock": wall_clock()}),
        );
        assert_eq!(rejected["error"]["code"], "invalid-context");

        let partial = response(
            &mut core,
            "_worker_typing_enable",
            json!({"trustedRoster": [], "clock": wall_clock()}),
        );
        assert_eq!(partial["error"]["code"], "invalid-request");
    }

    #[test]
    fn seal_without_a_gateway_resolves_the_target_or_seals_nothing() {
        let eligible = ConversationId::new();
        let ineligible = ConversationId::new();
        let (_directory, mut core, _device, _gateway) = trusted_core(vec![
            (eligible, RcsEligibility::Eligible),
            (ineligible, RcsEligibility::NotEligible),
        ]);
        let enabled = response(
            &mut core,
            "_worker_typing_enable",
            json!({"clock": wall_clock()}),
        );
        assert_eq!(enabled["ok"], true, "{enabled}");

        let sealed = response(
            &mut core,
            "_worker_typing_seal",
            json!({
                "conversationId": eligible,
                "origin": "vault_device",
                "state": "started",
                "clock": wall_clock(),
            }),
        );
        assert_eq!(sealed["ok"], true, "{sealed}");
        assert_keys(&sealed["value"], &["generation", "frameJson"]);

        let skipped = response(
            &mut core,
            "_worker_typing_seal",
            json!({
                "conversationId": ineligible,
                "origin": "vault_device",
                "state": "started",
                "clock": wall_clock(),
            }),
        );
        assert_eq!(skipped["ok"], true, "{skipped}");
        assert!(skipped["value"].is_null());
    }

    /// Detects committed changes: the SQLite connection is private to client-core, so every file
    /// under the core root is compared byte for byte after each command, not only at the end.
    #[test]
    fn live_typing_worker_commands_never_write_the_database_after_any_command() {
        let conversation = ConversationId::new();
        let (directory, mut core, _device, _gateway) =
            trusted_core(vec![(conversation, RcsEligibility::Eligible)]);
        let before = database_files(directory.path());
        // Rollback-journal mode: there is no WAL file, so the byte snapshot shows every committed change.
        let step = |core: &mut BrowserCore, command: &str, args: Value| {
            let result = response(core, command, args);
            assert_eq!(result["ok"], true, "{command}: {result}");
            assert_eq!(
                database_files(directory.path()),
                before,
                "{command} changed the database"
            );
            result
        };

        let key = step(&mut core, "_worker_typing_context_key", json!({}));
        assert!(key["value"]["key"].is_string(), "{key}");
        step(
            &mut core,
            "_worker_typing_enable",
            json!({"clock": wall_clock()}),
        );
        step(
            &mut core,
            "_worker_typing_set_sharing",
            json!({"sharing": true}),
        );
        let sealed = step(
            &mut core,
            "_worker_typing_seal",
            json!({
                "conversationId": conversation,
                "state": "started",
                "clock": wall_clock(),
            }),
        );
        let opened = step(
            &mut core,
            "_worker_typing_open",
            json!({"frameJson": sealed["value"]["frameJson"], "clock": wall_clock()}),
        );
        assert!(opened["value"]["followUpFramesJson"].is_array(), "{opened}");
        step(&mut core, "_worker_typing_disable", json!({}));
    }

    #[test]
    fn seal_accepts_the_minimal_origin_free_request() {
        let conversation = ConversationId::new();
        let (_directory, mut core, _device, _gateway) =
            trusted_core(vec![(conversation, RcsEligibility::Eligible)]);
        let enabled = response(
            &mut core,
            "_worker_typing_enable",
            json!({"clock": wall_clock()}),
        );
        assert_eq!(enabled["ok"], true, "{enabled}");

        let sealed = response(
            &mut core,
            "_worker_typing_seal",
            json!({"conversationId": conversation, "state": "started", "clock": wall_clock()}),
        );
        assert_eq!(sealed["ok"], true, "{sealed}");
        assert_keys(&sealed["value"], &["generation", "frameJson"]);
    }

    #[test]
    fn malformed_frames_return_an_empty_outcome() {
        let (_directory, mut core, _device, _gateway) = trusted_core(Vec::new());
        enable(&mut core);
        let opened = response(
            &mut core,
            "_worker_typing_open",
            json!({
                "frameJson": "not-json",
                "clock": wall_clock(),
            }),
        );
        assert_eq!(opened["ok"], true);
        assert_eq!(opened["value"]["generation"], "1");
        assert!(opened["value"]["sessionUpdate"].is_null());
        assert!(opened["value"]["action"].is_null());
        assert!(opened["value"]["replyFrameJson"].is_null());
        assert_eq!(opened["value"]["followUpFramesJson"], json!([]));
    }
}
