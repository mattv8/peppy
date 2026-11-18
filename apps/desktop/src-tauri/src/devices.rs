//! Device-authenticated roster and revocation adapters for the native settings UI.
use crate::{
    error::{BridgeError, BridgeResult},
    net::{NetError, MAX_JSON_BYTES},
    session::Session,
};
use peppy_hosted_api::DeviceRosterResponse;
use serde::Serialize;
use std::str::FromStr;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRosterView {
    current_device_id: String,
    devices: Vec<ManagedDeviceView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagedDeviceView {
    id: String,
    name: String,
    kind: Option<String>,
    role: String,
    revoked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence: Option<peppy_hosted_api::DevicePresence>,
    wake_route: bool,
    #[serde(default)]
    setting_up: bool,
}

pub async fn list(session: &Session) -> BridgeResult<DeviceRosterView> {
    let roster: DeviceRosterResponse = session
        .api
        .get_json("/v1/devices", MAX_JSON_BYTES)
        .await
        .map_err(roster_error)?;
    // Setup state is decoration: an unreadable capabilities list must not hide the roster or its actions.
    let capabilities: Option<peppy_desktop_api::gateways::CapabilitiesResponse> = session
        .api
        .get_json("/v1/capabilities", MAX_JSON_BYTES)
        .await
        .ok();
    Ok(DeviceRosterView {
        current_device_id: session.binding.device_id.clone(),
        devices: roster
            .devices
            .into_iter()
            .map(|device| ManagedDeviceView {
                id: device.device_id.to_string(),
                name: device.name.unwrap_or_else(|| "Unnamed device".into()),
                kind: device.kind,
                role: device.role,
                revoked: device.revoked,
                presence: device.presence,
                wake_route: device.wake_route,
                setting_up: setting_up_for(capabilities.as_ref(), &device.device_id.to_string()),
            })
            .collect(),
    })
}

/// Whether a device's capability report says it is importing history. Missing reports read as not importing.
fn setting_up_for(
    capabilities: Option<&peppy_desktop_api::gateways::CapabilitiesResponse>,
    device_id: &str,
) -> bool {
    capabilities.is_some_and(|capabilities| {
        capabilities.capabilities.iter().any(|capability| {
            capability.device_id == device_id
                && peppy_desktop_api::gateways::setting_up(&capability.capabilities)
        })
    })
}

pub async fn revoke(session: &Session, device_id: &str) -> BridgeResult<()> {
    let device_id = uuid::Uuid::from_str(device_id)
        .map_err(|_| BridgeError::new("invalid-request", "The device ID is invalid."))?;
    session
        .api
        .post_discard(&format!("/v1/devices/{device_id}/revoke"), &())
        .await
        .map_err(roster_error)
}

fn roster_error(error: NetError) -> BridgeError {
    match error {
        NetError::Revoked => BridgeError::new(
            "invalid_bearer",
            "The server rejected this device credential.",
        ),
        NetError::Status {
            status,
            code: Some(code),
        } => match code.as_str() {
            "owner_required" => {
                BridgeError::new("owner_required", "Only an owner can remove devices.")
            }
            "last_active_owner" => BridgeError::new(
                "last_active_owner",
                "The last active owner cannot be removed.",
            ),
            "device_already_revoked" => {
                BridgeError::new("device_already_revoked", "This device was already removed.")
            }
            "device_not_found" => {
                BridgeError::new("device_not_found", "This device is no longer enrolled.")
            }
            "invalid_bearer" => BridgeError::new(
                "invalid_bearer",
                "The server rejected this device credential.",
            ),
            _ => BridgeError::from(NetError::Status {
                status,
                code: Some(code),
            }),
        },
        error => error.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unreadable_capabilities_drop_only_the_setup_badge() {
        assert!(!setting_up_for(None, "phone"));
    }

    #[test]
    fn importing_capability_marks_only_its_device() {
        let capabilities: peppy_desktop_api::gateways::CapabilitiesResponse =
            serde_json::from_value(json!({"capabilities":[{
                "device_id":"phone","simulator":false,
                "capabilities":{"sims":[],"setup":{"state":"importing"}}
            }]}))
            .unwrap();
        assert!(setting_up_for(Some(&capabilities), "phone"));
        assert!(!setting_up_for(Some(&capabilities), "other"));
    }
}
