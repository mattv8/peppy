//! Typed IPC errors. Messages are fixed, human-readable strings; they never echo credential
//! contents, server bodies, filesystem paths or key material.
use peppy_client_core::Error as CoreError;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeError {
    pub code: &'static str,
    pub message: String,
    /// The stored draft revision when it is known to differ from the caller's, so the UI can
    /// continue without a stale-revision conflict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<String>,
}

impl BridgeError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            current_revision: None,
            conversation_id: None,
            draft_id: None,
        }
    }
    pub fn with_revision(mut self, revision: u64) -> Self {
        self.current_revision = Some(revision.to_string());
        self
    }
    pub fn with_draft_identity(
        mut self,
        draft_id: peppy_client_core::DraftId,
        conversation_id: peppy_client_core::ConversationId,
        revision: u64,
    ) -> Self {
        self.draft_id = Some(draft_id.to_string());
        self.conversation_id = Some(conversation_id.to_string());
        self.current_revision = Some(revision.to_string());
        self
    }
    pub fn host_state() -> Self {
        Self::new("host-state", "Native state is unavailable.")
    }
    pub fn no_session() -> Self {
        Self::new(
            "credentials-required",
            "Import a device credential for the configured server first.",
        )
    }
}

pub type BridgeResult<T> = Result<T, BridgeError>;

/// Maps client-core errors to stable UI codes. Every core message is a static string, so
/// forwarding `InvalidRequest` text cannot leak secrets.
pub fn core_error(error: CoreError) -> BridgeError {
    match error {
        CoreError::WrongPassphrase | CoreError::InvalidPassphrase => {
            BridgeError::new("unlock-failed", "The passphrase did not unlock this vault. No local data was changed.")
        }
        CoreError::StaleDraft { current_revision } => BridgeError::new(
            "stale-draft",
            format!("The draft changed elsewhere (current revision {current_revision}); both versions were kept."),
        )
        .with_revision(current_revision),
        CoreError::DraftMoved { draft_id, conversation_id, current_revision } => BridgeError::new(
            "draft-moved",
            "The draft moved to another conversation.",
        )
        .with_draft_identity(draft_id, conversation_id, current_revision),
        CoreError::DraftNotDiscardable => BridgeError::new(
            "draft-not-discardable",
            "Only an empty recipientless draft can be deleted.",
        ),
        CoreError::RecipientlessDraftExists => BridgeError::new(
            "recipientless-draft-exists",
            "A recipientless draft already exists.",
        ),
        CoreError::DraftMergeOverflow => BridgeError::new(
            "draft-merge-overflow",
            "Combining these drafts would exceed a compose limit.",
        ),
        CoreError::NotFound => BridgeError::new("not-found", "The requested stored item was not found."),
        CoreError::WrongDatabaseKey => BridgeError::new(
            "database-key-mismatch",
            "The protected database key does not open the local database. Nothing was reset.",
        ),
        CoreError::IdentityMismatch => BridgeError::new(
            "credential-mismatch",
            "The local database belongs to a different vault or device. Nothing was reset.",
        ),
        CoreError::InvalidProfile => BridgeError::new(
            "profile-mismatch",
            "The server key profile does not match the profile pinned on this device.",
        ),
        CoreError::KeysUnavailable => BridgeError::new("locked", "Unlock sync before this operation."),
        CoreError::InvalidRequest(reason) => BridgeError::new("invalid-request", format!("Request rejected: {reason}.")),
        CoreError::InvalidMedia => BridgeError::new(
            "attachment-invalid",
            "The attachment failed length, digest or decryption verification and was not installed.",
        ),
        CoreError::Storage => BridgeError::new("attachment-local", "Local attachment storage is unavailable."),
        CoreError::SnapshotMismatch => {
            BridgeError::new("resync-required", "History import was inconsistent; live state is unchanged.")
        }
        CoreError::Conflict => BridgeError::new("sync-conflict", "The server returned conflicting history; sync stopped."),
        other => BridgeError::new("core", other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_errors_map_to_stable_codes() {
        assert_eq!(
            core_error(CoreError::StaleDraft {
                current_revision: 4
            })
            .code,
            "stale-draft"
        );
        assert_eq!(
            core_error(CoreError::WrongDatabaseKey).code,
            "database-key-mismatch"
        );
        assert_eq!(core_error(CoreError::KeysUnavailable).code, "locked");
        let moved = core_error(CoreError::DraftMoved {
            draft_id: peppy_client_core::DraftId::new(),
            conversation_id: peppy_client_core::ConversationId::new(),
            current_revision: 7,
        });
        assert_eq!(moved.code, "draft-moved");
        assert_eq!(moved.current_revision.as_deref(), Some("7"));
        assert!(moved.draft_id.is_some() && moved.conversation_id.is_some());
    }
}
