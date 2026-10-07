//! Worker-only application of authenticated server roster and capability responses.
use super::*;
use peppy_desktop_api::gateways::{self, CapabilitiesResponse, DevicesResponse};

const MAX_CONTEXT_ROWS: usize = 1_000;
const MAX_CONNECTION_CHARS: usize = 32;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerContextInput {
    connection: String,
    devices: Option<Value>,
    capabilities: Option<Value>,
}

impl BrowserCore {
    /// Applies raw authenticated server responses only after the browser identity is verified in
    /// the roster. This is intentionally not renderer-routable.
    pub(crate) fn context_command(&mut self, command: &str, args: Value) -> Result<Value, Failure> {
        match command {
            "_worker_apply_server_context" => self.apply_server_context(args),
            _ => Err(unknown_command()),
        }
    }

    fn apply_server_context(&mut self, args: Value) -> Result<Value, Failure> {
        let input: ServerContextInput = serde_json::from_value(args).map_err(|_| invalid())?;
        if input.connection.chars().count() > MAX_CONNECTION_CHARS
            || !matches!(
                input.connection.as_str(),
                "connected" | "offline" | "error" | "revoked" | "key-mismatch"
            )
        {
            return Err(invalid());
        }
        if matches!(input.connection.as_str(), "offline" | "error") {
            if input.devices.is_some() || input.capabilities.is_some() {
                return Err(invalid());
            }
            self.context.connection = Some(input.connection);
            return Ok(
                json!({"connection": self.context.connection, "deviceRole": self.context.device_role, "gateways": self.context.gateways}),
            );
        }
        let devices: DevicesResponse =
            serde_json::from_value(input.devices.ok_or_else(invalid)?).map_err(|_| invalid())?;
        let capabilities: CapabilitiesResponse =
            serde_json::from_value(input.capabilities.ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
        if devices.devices.len() > MAX_CONTEXT_ROWS
            || capabilities.capabilities.len() > MAX_CONTEXT_ROWS
        {
            return Err(invalid());
        }
        let device_id = self.own_context_device_id()?;
        let role = gateways::extract_device_role(&devices, &device_id).ok_or_else(|| {
            Failure::new("device-revoked", "This browser device is no longer active.")
        })?;
        if !matches!(role.as_str(), "owner" | "device" | "gateway") {
            return Err(invalid());
        }
        let views = gateways::gateway_views(&devices, &capabilities);
        self.context.connection = Some(input.connection);
        self.context.device_role = Some(role.clone());
        self.context.gateways = Some(views.clone());
        self.gateways_known = true;
        serde_json::to_value(
            json!({"connection": self.context.connection, "deviceRole": role, "gateways": views}),
        )
        .map_err(|_| core(CoreError::Database))
    }

    fn own_context_device_id(&self) -> Result<String, Failure> {
        // `DeviceId` is held by the core owner, never supplied by a renderer or HTTP response.
        self.device_id
            .as_ref()
            .map(ToString::to_string)
            .ok_or_else(unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_crypto::{create_vault_check_header, derive_root_key};

    fn response(core: &mut BrowserCore, command: &str, args: Value) -> Value {
        let response: Value = serde_json::from_str(
            &core.dispatch(&json!({"command": command, "args": args}).to_string()),
        )
        .unwrap();
        assert_eq!(response["ok"], true, "{response}");
        response["value"].clone()
    }

    fn error(core: &mut BrowserCore, args: Value) -> Value {
        let response: Value = serde_json::from_str(&core.dispatch(
            &json!({"command": "_worker_apply_server_context", "args": args}).to_string(),
        ))
        .unwrap();
        assert_eq!(response["ok"], false, "{response}");
        response["error"].clone()
    }

    fn unlocked_core() -> (tempfile::TempDir, BrowserCore, DeviceId) {
        let root = tempfile::tempdir().unwrap();
        let vault = VaultId::new();
        let device = DeviceId::new();
        let mut core = BrowserCore::new(root.path().to_owned());
        response(
            &mut core,
            "open",
            json!({"vaultId": vault, "deviceId": device, "databaseKey": vec![7; 32], "origin": "https://peppy.test/", "deviceRole": "device"}),
        );
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("passphrase", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        response(
            &mut core,
            "unlock",
            json!({"profile": profile, "header": header, "passphrase": "passphrase"}),
        );
        (root, core, device)
    }

    fn context(devices: Value, capabilities: Value) -> Value {
        json!({"connection": "connected", "devices": devices, "capabilities": capabilities})
    }

    #[test]
    fn malformed_capabilities_are_rejected_without_context_update() {
        let (_root, mut core, device) = unlocked_core();
        let result = error(
            &mut core,
            context(
                json!({"devices":[{"device_id":device,"role":"device","revoked":false}]}),
                json!({"capabilities":"invalid"}),
            ),
        );
        assert_eq!(result["code"], "invalid-request");
        assert!(core.context.gateways.is_none());
    }

    #[test]
    fn missing_or_revoked_self_is_rejected() {
        let (_root, mut core, device) = unlocked_core();
        for devices in [
            json!({"devices":[]}),
            json!({"devices":[{"device_id":device,"role":"device","revoked":true}]}),
        ] {
            let result = error(&mut core, context(devices, json!({"capabilities":[]})));
            assert_eq!(result["code"], "device-revoked");
            assert!(core.context.gateways.is_none());
        }
    }

    #[test]
    fn valid_gateway_context_enables_a_real_draft_send() {
        let (_root, mut core, device) = unlocked_core();
        let gateway = DeviceId::new();
        let applied = response(
            &mut core,
            "_worker_apply_server_context",
            context(
                json!({"devices":[
                    {"device_id":device,"role":"device","revoked":false},
                    {"device_id":gateway,"role":"gateway","revoked":false}
                ]}),
                json!({"capabilities":[{"device_id":gateway,"simulator":false,"capabilities":{"sims":[{"subscription_id":"sim-1","sms":"available","mms":"available","mms_content_version":2}]}}]}),
            ),
        );
        assert_eq!(applied["deviceRole"], "device");
        assert_eq!(applied["gateways"][0]["online"], false);
        assert!(applied.to_string().contains("sim-1"));
        let draft = response(
            &mut core,
            "save_draft",
            json!({"id":"new","conversationId":"","text":"hello","recipientIds":["+15555550100"],"attachmentIds":[],"gatewayId":gateway,"simId":"sim-1","expectedRevision":"0"}),
        );
        let sent = response(
            &mut core,
            "send_draft",
            json!({"id":draft["id"],"conversationId":draft["conversationId"],"text":draft["text"],"recipientIds":draft["recipientIds"],"attachmentIds":draft["attachmentIds"],"gatewayId":gateway,"simId":"sim-1","expectedRevision":draft["revision"]}),
        );
        assert_eq!(sent["status"], "queued-local");
        assert_eq!(core.client().unwrap().pending_outbox().unwrap().len(), 1);
    }

    #[test]
    fn offline_context_preserves_the_last_valid_gateway_set() {
        let (_root, mut core, device) = unlocked_core();
        let gateway = DeviceId::new();
        response(
            &mut core,
            "_worker_apply_server_context",
            context(
                json!({"devices":[{"device_id":device,"role":"device","revoked":false},{"device_id":gateway,"role":"gateway","revoked":false}]}),
                json!({"capabilities":[{"device_id":gateway,"simulator":false,"capabilities":{"sims":[{"subscription_id":"sim-1","sms":"available","mms":"available","mms_content_version":2}]}}]}),
            ),
        );
        let offline = response(
            &mut core,
            "_worker_apply_server_context",
            json!({"connection":"offline","devices":null,"capabilities":null}),
        );
        assert_eq!(offline["connection"], "offline");
        assert_eq!(offline["gateways"][0]["id"], gateway.to_string());
    }
}
