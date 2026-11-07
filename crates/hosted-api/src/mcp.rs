use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    computer_enrollment::{
        ComputerEnrollmentConfirmRequest, ComputerEnrollmentStatus, ComputerPairingQr,
    },
    enrollment::AdmissionComplete,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum McpDeviceState {
    Online,
    Offline,
    NeedsApproval,
    Revoked,
}

impl fmt::Display for McpDeviceState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Online => "online",
            Self::Offline => "offline",
            Self::NeedsApproval => "needs_approval",
            Self::Revoked => "revoked",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
pub enum McpScope {
    #[serde(rename = "conversations:read")]
    ConversationsRead,
    #[serde(rename = "messages:send")]
    MessagesSend,
    #[serde(rename = "media:send")]
    MediaSend,
}

impl fmt::Display for McpScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ConversationsRead => "conversations:read",
            Self::MessagesSend => "messages:send",
            Self::MediaSend => "media:send",
        })
    }
}

impl McpScope {
    pub const ALL: [Self; 3] = [Self::ConversationsRead, Self::MessagesSend, Self::MediaSend];

    pub fn is_valid_set(scopes: &[Self]) -> bool {
        !scopes.is_empty()
            && scopes.len() <= Self::ALL.len()
            && scopes
                .iter()
                .enumerate()
                .all(|(index, scope)| !scopes[..index].contains(scope))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McpToolScopes {
    pub name: &'static str,
    pub required_scopes: &'static [McpScope],
}

pub const MCP_TOOL_SCOPES: &[McpToolScopes] = &[
    McpToolScopes {
        name: "list_conversations",
        required_scopes: &[McpScope::ConversationsRead],
    },
    McpToolScopes {
        name: "get_messages",
        required_scopes: &[McpScope::ConversationsRead],
    },
    McpToolScopes {
        name: "get_attachment",
        required_scopes: &[McpScope::ConversationsRead],
    },
    McpToolScopes {
        name: "list_contacts",
        required_scopes: &[McpScope::ConversationsRead],
    },
    McpToolScopes {
        name: "send_message",
        required_scopes: &[McpScope::MessagesSend],
    },
    McpToolScopes {
        name: "send_reaction_text",
        required_scopes: &[McpScope::MessagesSend],
    },
    McpToolScopes {
        name: "send_image",
        required_scopes: &[McpScope::MediaSend],
    },
];

pub fn mcp_tool_scopes(name: &str) -> Option<&'static [McpScope]> {
    MCP_TOOL_SCOPES
        .iter()
        .find(|tool| tool.name == name)
        .map(|tool| tool.required_scopes)
}

pub fn mcp_scopes_allow(granted: &[McpScope], required: &[McpScope]) -> bool {
    McpScope::is_valid_set(granted)
        && McpScope::is_valid_set(required)
        && required.iter().all(|scope| granted.contains(scope))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpDeviceView {
    pub device_id: Uuid,
    pub name: String,
    pub state: McpDeviceState,
    pub last_synced_at: Option<String>,
    pub cleanup_pending: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpDeviceStatus {
    pub vault_id: Uuid,
    pub available: bool,
    pub server_origin: String,
    pub hosted: bool,
    pub device: Option<McpDeviceView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpGrantView {
    pub id: Uuid,
    pub client_name: String,
    pub scopes: Vec<McpScope>,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum WebhookFilterMode {
    All,
    Only,
    Except,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
pub enum WebhookEvent {
    #[serde(rename = "message.received")]
    MessageReceived,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookFilterEntry {
    pub address: String,
    pub label: Option<String>,
    pub contact_id: Option<String>,
    pub book_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookInput {
    pub name: Option<String>,
    pub url: String,
    pub events: Vec<WebhookEvent>,
    pub filter_mode: WebhookFilterMode,
    pub filter: Vec<WebhookFilterEntry>,
    pub paused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum WebhookDeliveryState {
    Never,
    Delivered,
    Retrying,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookView {
    pub id: Uuid,
    pub name: String,
    pub name_is_derived: bool,
    pub url: String,
    pub events: Vec<WebhookEvent>,
    pub filter_mode: WebhookFilterMode,
    pub filter: Vec<WebhookFilterEntry>,
    pub paused: bool,
    pub created_at: String,
    pub last_delivery_state: WebhookDeliveryState,
    pub last_delivery_at: Option<String>,
    pub last_status_code: Option<u16>,
    pub last_error: Option<String>,
    pub pending_deliveries: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookList {
    pub available: bool,
    pub webhooks: Vec<WebhookView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookSecret {
    pub id: Uuid,
    pub secret: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookCreated {
    pub webhook: WebhookView,
    pub secret: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebhookTestResult {
    pub ok: bool,
    pub status_code: Option<u16>,
    pub duration_ms: u32,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpAuthorizationView {
    pub id: String,
    pub client_name: String,
    pub client_id: String,
    pub requested_scopes: Vec<McpScope>,
    pub server_origin: String,
    pub resource: String,
    pub redirect_origin: String,
    pub redirect_is_loopback: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpAuthorizationDecision {
    pub redirect_uri: String,
}

/// Private host transport only. Never serialize this through shared renderer contracts.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpEnrollmentPreparation {
    pub operation_id: Uuid,
    pub pairing: ComputerPairingQr,
}

/// Private host transport only. Never serialize this through shared renderer contracts.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpHostActivationRequest {
    pub operation_id: Uuid,
    pub status: ComputerEnrollmentStatus,
}

/// Private host transport only. Never serialize this through shared renderer contracts.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpHostCompletionRequest {
    pub operation_id: Uuid,
    pub admission: AdmissionComplete,
}

/// Trusted server-to-worker control wire. It is intentionally snake_case and
/// must never be exposed to renderers or public OAuth clients.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpWorkerPrepareRequest {
    pub vault_id: Uuid,
    pub owner_device_id: Uuid,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpWorkerPrepareResponse {
    pub operation_id: Uuid,
    pub pairing: ComputerPairingQr,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpWorkerCancelRequest {
    pub vault_id: Uuid,
    pub owner_device_id: Uuid,
    pub invitation_id: Uuid,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpWorkerActivateRequest {
    pub vault_id: Uuid,
    pub operation_id: Uuid,
    /// Owner-authorized enrollment state, relayed by the host without account credentials.
    pub status: ComputerEnrollmentStatus,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpWorkerActivateResponse {
    /// The worker's signed claimant confirmation. The host submits it to the
    /// ordinary enrollment endpoint using its current account authorization.
    pub confirm: ComputerEnrollmentConfirmRequest,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpWorkerCompleteRequest {
    pub vault_id: Uuid,
    pub operation_id: Uuid,
    pub admission: AdmissionComplete,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpWorkerStatusRequest {
    pub vault_id: Uuid,
    pub device_id: Uuid,
}

/// Trusted worker-to-host status wire. It is intentionally snake_case and
/// identifies the device whose runtime state was observed.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpWorkerStatusResponse {
    pub vault_id: Uuid,
    pub device_id: Uuid,
    pub state: McpDeviceState,
    pub last_synced_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpWorkerRevokeRequest {
    pub vault_id: Uuid,
    pub device_id: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpWorkerCallRequest {
    pub vault_id: Uuid,
    pub device_id: Uuid,
    pub grant_id: Uuid,
    pub scopes: Vec<McpScope>,
    pub request: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpWorkerWebhookOp {
    List,
    Create,
    Update,
    Delete,
    RevealSecret,
    RotateSecret,
    Test,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct McpWorkerWebhookRequest {
    pub vault_id: Uuid,
    pub device_id: Uuid,
    pub source_origin: String,
    pub op: McpWorkerWebhookOp,
    #[serde(default)]
    pub id: Option<Uuid>,
    #[serde(default)]
    pub input: Option<WebhookInput>,
}

#[cfg(test)]
mod tests {
    use super::{
        MCP_TOOL_SCOPES, McpDeviceState, McpDeviceStatus, McpScope, McpWorkerStatusRequest,
        McpWorkerStatusResponse, McpWorkerWebhookRequest, WebhookEvent, WebhookFilterMode,
        WebhookInput, mcp_scopes_allow, mcp_tool_scopes,
    };
    use uuid::Uuid;

    #[test]
    fn management_dtos_use_the_shared_camel_case_wire_format() {
        let status = McpDeviceStatus {
            vault_id: Uuid::nil(),
            available: true,
            server_origin: "https://peppy.example".into(),
            hosted: false,
            device: None,
        };

        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!({
                "vaultId": Uuid::nil(),
                "available": true,
                "serverOrigin": "https://peppy.example",
                "hosted": false,
                "device": null,
            })
        );
        assert_eq!(McpDeviceState::Online.to_string(), "online");
        assert_eq!(McpScope::MessagesSend.to_string(), "messages:send");
    }

    #[test]
    fn webhook_dtos_round_trip_and_reject_unknown_fields() {
        let input = WebhookInput {
            name: Some("Inbox".into()),
            url: "https://hooks.example/messages".into(),
            events: vec![WebhookEvent::MessageReceived],
            filter_mode: WebhookFilterMode::Only,
            filter: Vec::new(),
            paused: false,
        };
        let value = serde_json::to_value(&input).unwrap();

        assert_eq!(value["events"], serde_json::json!(["message.received"]));
        assert_eq!(
            serde_json::from_value::<WebhookInput>(value).unwrap(),
            input
        );
        assert!(
            serde_json::from_value::<WebhookInput>(serde_json::json!({
                "name": null,
                "url": "https://hooks.example/messages",
                "events": ["message.received"],
                "filterMode": "all",
                "filter": [],
                "paused": false,
                "unexpected": true,
            }))
            .is_err()
        );
    }

    #[test]
    fn worker_webhook_request_rejects_unknown_fields() {
        let vault_id = Uuid::new_v4();
        let device_id = Uuid::new_v4();
        let request: McpWorkerWebhookRequest = serde_json::from_value(serde_json::json!({
            "vault_id": vault_id,
            "device_id": device_id,
            "source_origin": "https://app.example.test",
            "op": "list",
        }))
        .unwrap();

        assert_eq!(request.vault_id, vault_id);
        assert_eq!(request.source_origin, "https://app.example.test");
        assert!(request.id.is_none());
        assert!(request.input.is_none());
        assert!(
            serde_json::from_value::<McpWorkerWebhookRequest>(serde_json::json!({
                "vault_id": vault_id,
                "device_id": device_id,
                "source_origin": "https://app.example.test",
                "op": "list",
                "unexpected": true,
            }))
            .is_err()
        );
    }

    #[test]
    fn tool_scopes_are_unique_and_require_complete_valid_sets() {
        assert_eq!(MCP_TOOL_SCOPES.len(), 7);
        assert!(MCP_TOOL_SCOPES.iter().enumerate().all(|(index, tool)| {
            MCP_TOOL_SCOPES[..index]
                .iter()
                .all(|prior| prior.name != tool.name)
                && McpScope::is_valid_set(tool.required_scopes)
        }));
        assert_eq!(
            mcp_tool_scopes("send_image"),
            Some(&[McpScope::MediaSend][..])
        );
        assert!(mcp_scopes_allow(
            &[McpScope::ConversationsRead, McpScope::MediaSend],
            mcp_tool_scopes("send_image").unwrap(),
        ));
        assert!(!mcp_scopes_allow(
            &[McpScope::MessagesSend],
            mcp_tool_scopes("send_image").unwrap(),
        ));
        assert!(!McpScope::is_valid_set(&[
            McpScope::ConversationsRead,
            McpScope::MediaSend,
            McpScope::ConversationsRead,
        ]));
    }

    #[test]
    fn worker_status_requests_are_bound_to_a_device() {
        let vault_id = Uuid::nil();
        let device_id = Uuid::new_v4();
        assert!(
            serde_json::from_value::<McpWorkerStatusRequest>(serde_json::json!({
                "vault_id": vault_id,
                "device_id": device_id,
            }))
            .is_ok()
        );
        assert!(
            serde_json::from_value::<McpWorkerStatusRequest>(serde_json::json!({
                "vault_id": vault_id,
            }))
            .is_err()
        );
    }

    #[test]
    fn worker_status_responses_identify_the_observed_device() {
        let vault_id = Uuid::nil();
        let device_id = Uuid::new_v4();
        let response = McpWorkerStatusResponse {
            vault_id,
            device_id,
            state: McpDeviceState::Online,
            last_synced_at: None,
        };
        assert_eq!(
            serde_json::to_value(response).unwrap(),
            serde_json::json!({
                "vault_id": vault_id,
                "device_id": device_id,
                "state": "online",
                "last_synced_at": null,
            })
        );
    }
}
