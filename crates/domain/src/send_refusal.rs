use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Group send and create outcomes that are not plain transport states. The codes are the
/// desktop-facing vocabulary; they never carry Google identifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SendRefusalReason {
    /// The composer's member revision no longer matches the group; nothing was sent.
    MembersChanged,
    /// A stale client tried to send carrier MMS into an RCS-linked group; nothing was sent.
    UpdateRequired,
    /// The group was withdrawn (left, read-only, or gone); nothing was sent over RCS.
    Withdrawn,
    /// A group create had an unknown outcome; the message was not sent.
    CreateUnconfirmed,
    /// The group is not an RCS group; the message went out as group MMS (informational).
    CreateNotRcs,
}

impl SendRefusalReason {
    pub const fn code(self) -> &'static str {
        match self {
            Self::MembersChanged => "members_changed",
            Self::UpdateRequired => "update_required",
            Self::Withdrawn => "withdrawn",
            Self::CreateUnconfirmed => "create_unconfirmed",
            Self::CreateNotRcs => "create_not_rcs",
        }
    }

    /// True when the message itself did not reach the carrier, so the sender may retry it
    /// deliberately. `CreateNotRcs` is informational: the message was sent as MMS.
    pub const fn message_not_sent(self) -> bool {
        !matches!(self, Self::CreateNotRcs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_match_serde_names() {
        for reason in [
            SendRefusalReason::MembersChanged,
            SendRefusalReason::UpdateRequired,
            SendRefusalReason::Withdrawn,
            SendRefusalReason::CreateUnconfirmed,
            SendRefusalReason::CreateNotRcs,
        ] {
            let json = serde_json::to_string(&reason).unwrap();
            assert_eq!(json, format!("\"{}\"", reason.code()));
        }
    }

    #[test]
    fn only_create_not_rcs_still_sends_the_message() {
        assert!(!SendRefusalReason::CreateNotRcs.message_not_sent());
        assert!(SendRefusalReason::CreateUnconfirmed.message_not_sent());
        assert!(SendRefusalReason::MembersChanged.message_not_sent());
    }
}
