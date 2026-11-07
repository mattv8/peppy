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
    Pending,
    Online,
    Offline,
    NeedsApproval,
    Revoked,
}

impl fmt::Display for McpDeviceState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Pending => "pending",
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

#[cfg(test)]
mod tests {
    use super::{McpDeviceState, McpDeviceStatus, McpScope};
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
        assert_eq!(McpDeviceState::Pending.to_string(), "pending");
        assert_eq!(McpScope::MessagesSend.to_string(), "messages:send");
    }
}
