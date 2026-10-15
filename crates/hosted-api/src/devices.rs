//! Device roster wire contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
}
