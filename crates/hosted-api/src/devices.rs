//! Device roster wire contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DevicePresence;

/// The authenticated vault's devices, including revoked devices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceRosterResponse {
    pub devices: Vec<DeviceRosterDevice>,
}

/// A device record in an authenticated roster.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceRosterDevice {
    pub device_id: Uuid,
    /// Legacy devices may not have a display name.
    pub name: Option<String>,
    /// Unknown future client kinds remain representable instead of failing roster decoding.
    pub kind: Option<String>,
    pub role: String,
    pub revoked: bool,
    pub profile_fingerprint: String,
    pub key_epoch: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<DevicePresence>,
    #[serde(default)]
    pub wake_route: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DevicePresence, PresenceState};

    #[test]
    fn presence_and_wake_route_are_additive() {
        let device: DeviceRosterDevice = serde_json::from_value(serde_json::json!({
            "device_id": Uuid::nil(), "name": null, "kind": "android", "role": "gateway",
            "revoked": false, "profile_fingerprint": "fingerprint", "key_epoch": 1
        }))
        .unwrap();
        assert_eq!(device.presence, None);
        assert!(!device.wake_route);
        let value = serde_json::to_value(DeviceRosterDevice {
            presence: Some(DevicePresence {
                state: PresenceState::Seen,
                last_seen_age_seconds: Some(1),
            }),
            wake_route: true,
            ..device
        })
        .unwrap();
        assert_eq!(value["presence"]["last_seen_age_seconds"], 1);
        assert_eq!(value["wake_route"], true);
    }
}
