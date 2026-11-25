//! Display DTOs for notification dismissal. Core reason codes are snake_case on disk; the UI
//! receives camelCase names, and any code this build does not know reads as `other`.
use peppy_client_core::{
    NotificationDismissalBlock as CoreBlock, NotificationDismissalBlockReason as CoreReason,
};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum NotificationDismissalBlockReason {
    MirroringOff,
    NotificationAccessOff,
    WaitingForWifi,
    Other,
}

impl From<CoreReason> for NotificationDismissalBlockReason {
    fn from(reason: CoreReason) -> Self {
        match reason {
            CoreReason::MirroringOff => Self::MirroringOff,
            CoreReason::NotificationAccessOff => Self::NotificationAccessOff,
            CoreReason::WaitingForWifi => Self::WaitingForWifi,
            CoreReason::Other => Self::Other,
        }
    }
}

/// A phone that cannot currently record dismissals, with the reason it reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct NotificationDismissalBlock {
    pub source_device_id: String,
    pub reason: NotificationDismissalBlockReason,
    #[ts(type = "number")]
    pub observed_at_ms: i64,
}

impl From<CoreBlock> for NotificationDismissalBlock {
    fn from(block: CoreBlock) -> Self {
        Self {
            source_device_id: block.source_device_id,
            reason: block.reason.into(),
            observed_at_ms: block.observed_at_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dismissal_block_serializes_camel_case_reasons() {
        let blocks = [
            CoreReason::MirroringOff,
            CoreReason::NotificationAccessOff,
            CoreReason::WaitingForWifi,
            CoreReason::Other,
        ]
        .map(|reason| {
            serde_json::to_value(NotificationDismissalBlock::from(CoreBlock {
                source_device_id: "phone-1".into(),
                reason,
                observed_at_ms: 1_700_000_000_000,
            }))
            .unwrap()
        });

        assert_eq!(
            blocks,
            [
                json!({"sourceDeviceId": "phone-1", "reason": "mirroringOff", "observedAtMs": 1_700_000_000_000_i64}),
                json!({"sourceDeviceId": "phone-1", "reason": "notificationAccessOff", "observedAtMs": 1_700_000_000_000_i64}),
                json!({"sourceDeviceId": "phone-1", "reason": "waitingForWifi", "observedAtMs": 1_700_000_000_000_i64}),
                json!({"sourceDeviceId": "phone-1", "reason": "other", "observedAtMs": 1_700_000_000_000_i64}),
            ]
        );
    }
}
