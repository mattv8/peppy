//! Device connectivity projection shared by hosted API consumers.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PresenceState {
    Connected,
    Seen,
    Never,
    /// Forward-compatible decode value. Servers never emit this state.
    #[serde(other)]
    Unknown,
}

/// A server-computed, non-authoritative connectivity observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DevicePresence {
    pub state: PresenceState,
    pub last_seen_age_seconds: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_the_frozen_snake_case_contract() {
        let value = serde_json::to_value(DevicePresence {
            state: PresenceState::Connected,
            last_seen_age_seconds: Some(30),
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"state":"connected","last_seen_age_seconds":30})
        );
    }
}
