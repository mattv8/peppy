use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DRAFT_RETENTION_DAYS: i64 = 30;
pub const MAX_DRAFT_ATTACHMENTS: usize = 10;
pub const MAX_DRAFT_RECORD_BYTES: usize = 262_144;
pub const MAX_DRAFT_LIVE_BRANCHES: usize = 3;
pub const MAX_DRAFT_CHANGES_PAGE: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftErrorCode {
    DraftSent,
    DraftDiscarded,
    DraftExpired,
    DraftBranchLimit,
    DraftAlreadySent,
    DraftRevisionNotHead,
    DraftAttachmentUnavailable,
    DraftInvalidRequest,
    DraftRecordTooLarge,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PutDraftRevisionRequest {
    pub base_revision_id: Option<Uuid>,
    pub sealed_record: String,
    pub attachment_ids: Vec<Uuid>,
    pub authored: bool,
    #[serde(default)]
    pub resolves_branch_ids: Vec<Uuid>,
}

impl PutDraftRevisionRequest {
    pub fn validate(&self) -> Result<(), DraftErrorCode> {
        let record = URL_SAFE_NO_PAD
            .decode(&self.sealed_record)
            .map_err(|_| DraftErrorCode::DraftInvalidRequest)?;
        if record.len() > MAX_DRAFT_RECORD_BYTES {
            return Err(DraftErrorCode::DraftRecordTooLarge);
        }
        if self.attachment_ids.len() > MAX_DRAFT_ATTACHMENTS
            || self.resolves_branch_ids.len() > MAX_DRAFT_LIVE_BRANCHES
            || self.attachment_ids.iter().any(|id| id.is_nil())
            || self.resolves_branch_ids.iter().any(|id| id.is_nil())
            || has_duplicates(&self.attachment_ids)
            || has_duplicates(&self.resolves_branch_ids)
        {
            return Err(DraftErrorCode::DraftInvalidRequest);
        }
        Ok(())
    }
}

fn has_duplicates(values: &[Uuid]) -> bool {
    values
        .iter()
        .enumerate()
        .any(|(index, value)| values[..index].contains(value))
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PutDraftRevisionResponse {
    pub branch_id: Uuid,
    pub revision_id: Uuid,
    pub expires_at: String,
    pub conflict: bool,
    pub duplicate: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcceptDraftSendRequest {
    pub revision_id: Uuid,
    pub command_id: Uuid,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcceptDraftSendResponse {
    pub command_id: Uuid,
    pub duplicate: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DraftBranchCloseReason {
    Superseded,
    Resolved,
    Expired,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DraftTerminalReason {
    Sent,
    Discarded,
    Expired,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DraftChange {
    Head {
        seq: String,
        draft_id: Uuid,
        branch_id: Uuid,
        revision_id: Uuid,
        parent_revision_id: Option<Uuid>,
        producer_device_id: Uuid,
        sealed_record: String,
        attachment_ids: Vec<Uuid>,
        expires_at: String,
        accepted_at: String,
    },
    BranchClosed {
        seq: String,
        draft_id: Uuid,
        branch_id: Uuid,
        reason: DraftBranchCloseReason,
    },
    Terminal {
        seq: String,
        draft_id: Uuid,
        reason: DraftTerminalReason,
        command_id: Option<Uuid>,
        terminated_at: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftChangesResponse {
    pub changes: Vec<DraftChange>,
    pub next_after: String,
    pub more: bool,
}
