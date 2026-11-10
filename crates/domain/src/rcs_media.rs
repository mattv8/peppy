use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Sanitized per-attachment RCS media state synced between devices. Incoming and outgoing
/// vocabularies share one wire enum; `is_incoming` selects the projection that stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RcsMediaSyncState {
    Downloading,
    Stored,
    TooLarge,
    QuotaFull,
    Failed,
    Expired,
    Sending,
    Sent,
    NotSent,
    OutcomeUnknown,
    MembersChanged,
}

impl RcsMediaSyncState {
    pub fn from_code(value: &str) -> Option<Self> {
        Some(match value {
            "downloading" => Self::Downloading,
            "stored" => Self::Stored,
            "too_large" => Self::TooLarge,
            "quota_full" => Self::QuotaFull,
            "failed" => Self::Failed,
            "expired" => Self::Expired,
            "sending" => Self::Sending,
            "sent" => Self::Sent,
            "not_sent" => Self::NotSent,
            "outcome_unknown" => Self::OutcomeUnknown,
            "members_changed" => Self::MembersChanged,
            _ => return None,
        })
    }
    pub const fn code(self) -> &'static str {
        match self {
            Self::Downloading => "downloading",
            Self::Stored => "stored",
            Self::TooLarge => "too_large",
            Self::QuotaFull => "quota_full",
            Self::Failed => "failed",
            Self::Expired => "expired",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::NotSent => "not_sent",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::MembersChanged => "members_changed",
        }
    }

    /// Receive states describe an incoming attachment; the rest describe an outgoing send.
    pub const fn is_incoming(self) -> bool {
        matches!(
            self,
            Self::Downloading
                | Self::Stored
                | Self::TooLarge
                | Self::QuotaFull
                | Self::Failed
                | Self::Expired
        )
    }
}

/// Why an outgoing RCS media attachment was not sent. Only valid with `RcsMediaSyncState::NotSent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RcsMediaNotSentReason {
    NotAllowedMime,
    TooLarge,
    Rejected,
    /// Upload failed the retry cap before any permit existed; nothing reached Google's send.
    Failed,
}

impl RcsMediaNotSentReason {
    pub fn from_code(value: &str) -> Option<Self> {
        Some(match value {
            "not_allowed_mime" => Self::NotAllowedMime,
            "too_large" => Self::TooLarge,
            "rejected" => Self::Rejected,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotAllowedMime => "not_allowed_mime",
            Self::TooLarge => "too_large",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_serde_names() {
        for state in [
            RcsMediaSyncState::Downloading,
            RcsMediaSyncState::Stored,
            RcsMediaSyncState::TooLarge,
            RcsMediaSyncState::QuotaFull,
            RcsMediaSyncState::Failed,
            RcsMediaSyncState::Expired,
            RcsMediaSyncState::Sending,
            RcsMediaSyncState::Sent,
            RcsMediaSyncState::NotSent,
            RcsMediaSyncState::OutcomeUnknown,
            RcsMediaSyncState::MembersChanged,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, format!("\"{}\"", state.code()));
        }
        for reason in [
            RcsMediaNotSentReason::NotAllowedMime,
            RcsMediaNotSentReason::TooLarge,
            RcsMediaNotSentReason::Rejected,
            RcsMediaNotSentReason::Failed,
        ] {
            let json = serde_json::to_string(&reason).unwrap();
            assert_eq!(json, format!("\"{}\"", reason.code()));
        }
    }

    #[test]
    fn incoming_and_outgoing_vocabularies_are_disjoint() {
        assert!(RcsMediaSyncState::Stored.is_incoming());
        assert!(RcsMediaSyncState::Expired.is_incoming());
        assert!(!RcsMediaSyncState::Sending.is_incoming());
        assert!(!RcsMediaSyncState::MembersChanged.is_incoming());
    }
}
