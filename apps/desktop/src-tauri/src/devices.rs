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
}

pub async fn list(session: &Session) -> BridgeResult<DeviceRosterView> {
    let roster: DeviceRosterResponse = session
        .api
        .get_json("/v1/devices", MAX_JSON_BYTES)
        .await
        .map_err(roster_error)?;
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
            })
            .collect(),
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
