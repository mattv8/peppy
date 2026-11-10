//! Durable local state for encrypted compose-draft synchronization.
//!
//! Transport owns retries; this module owns the stable revision bytes and all
//! local reconciliation decisions so a retry can never mint a different draft
//! revision.

use crate::{
    AttachmentId, AttachmentState, CipherObject, Client, CommandId, ComposeDraft, ConversationId,
    DraftId, DraftSendParts, Error, GatewayRoute, GroupDraftSend, PurposeKey, Store, VaultId,
    attachment_retention, attachment_row, conversation_match, decode, media,
};
use peppy_crypto::{FileKey, decrypt, encrypt};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::fmt;
#[cfg(debug_assertions)]
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use zeroize::Zeroizing;

#[cfg(debug_assertions)]
#[doc(hidden)]
pub type ResurrectionTestHook = Arc<dyn Fn(DraftId) + Send + Sync>;
#[cfg(debug_assertions)]
static RESURRECTION_TEST_HOOK: Mutex<Option<ResurrectionTestHook>> = Mutex::new(None);

#[derive(Serialize, Deserialize)]
struct SendIntentSnapshot {
    conversation_id: ConversationId,
    text: String,
    recipients: Vec<String>,
    attachment_ids: Vec<AttachmentId>,
    route: Option<GatewayRoute>,
    #[serde(default)]
    parts: Option<DraftSendParts>,
    /// Group fields of a send to two or more recipients; absent for 1:1 intents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<GroupDraftSend>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftRecordAttachmentV1 {
    pub attachment_id: AttachmentId,
    pub media_type: String,
    pub display_name: String,
    pub plaintext_bytes: u64,
    pub ciphertext_bytes: u64,
    pub ciphertext_sha256: String,
    pub stream_version: u32,
    pub file_key: Vec<u8>,
}

impl fmt::Debug for DraftRecordAttachmentV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DraftRecordAttachmentV1")
            .field("attachment_id", &self.attachment_id)
            .field("media_type", &self.media_type)
            .field("plaintext_bytes", &self.plaintext_bytes)
            .field("ciphertext_bytes", &self.ciphertext_bytes)
            .field("stream_version", &self.stream_version)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftRecordV1 {
    pub version: u8,
    pub draft_id: DraftId,
    pub revision_id: Uuid,
    pub text: String,
    pub recipients: Vec<String>,
    pub route: Option<GatewayRoute>,
    pub attachments: Vec<DraftRecordAttachmentV1>,
    pub authored_at_ms: i64,
    /// Group name of a draft to two or more members; serialized only when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_name: Option<String>,
}

impl fmt::Debug for DraftRecordV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DraftRecordV1")
            .field("version", &self.version)
            .field("draft_id", &self.draft_id)
            .field("revision_id", &self.revision_id)
            .field("attachment_count", &self.attachments.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftPush {
    pub draft_id: DraftId,
    pub revision_id: Uuid,
    pub base_revision_id: Option<Uuid>,
    pub sealed_record: Vec<u8>,
    pub attachment_ids: Vec<AttachmentId>,
    pub authored: bool,
    pub resolves_branch_ids: Vec<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftUpload {
    pub object: CipherObject,
    pub draft_id: DraftId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DraftTerminalReasonCore {
    Sent,
    Discarded,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DraftPushOutcome {
    Accepted {
        branch_id: Uuid,
        expires_at_ms: i64,
        conflict: bool,
    },
    Terminated(DraftTerminalReasonCore),
    BranchLimit,
    AttachmentUnavailable,
    Rejected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteDraftChange {
    Head {
        draft_id: DraftId,
        branch_id: Uuid,
        revision_id: Uuid,
        parent_revision_id: Option<Uuid>,
        sealed_record: Vec<u8>,
        attachment_ids: Vec<AttachmentId>,
        expires_at_ms: i64,
    },
    BranchClosed {
        draft_id: DraftId,
        branch_id: Uuid,
    },
    Terminal {
        draft_id: DraftId,
        reason: DraftTerminalReasonCore,
        command_id: Option<CommandId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteDraftSend {
    pub draft_id: DraftId,
    pub revision_id: Uuid,
    pub command_id: CommandId,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftSendGate {
    pub remote: Option<RemoteDraftSend>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftSyncState {
    LocalOnly,
    SavedLocal,
    Syncing,
    Synced,
    Paused,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftNotice {
    RestoredAfterExpiry,
    KeptAfterSent,
    UpdatedFromOtherDevice,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftConflictVersion {
    pub branch_id: Uuid,
    pub text: String,
    pub attachment_ids: Vec<AttachmentId>,
    pub recipient_ids: Vec<String>,
    pub route: Option<GatewayRoute>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftSyncStatus {
    pub state: DraftSyncState,
    pub expires_at_ms: Option<i64>,
    pub conflicts: Vec<DraftConflictVersion>,
    pub notice: Option<DraftNotice>,
    pub send_requires_connection: bool,
}
/// Transfer and local-availability state of an attachment referenced by a compose draft.
/// This is independent of the draft's local save and remote synchronization acknowledgements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftAttachmentState {
    Saving,
    Local,
    Waiting,
    Uploading,
    Uploaded,
    UploadFailed,
    Downloading,
    DownloadFailed,
    Unavailable,
}
/// Direction of a host-reported transfer failure for a draft attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DraftTransferDirection {
    Upload,
    Download,
}
type PendingPushRow = (
    String,
    String,
    Option<String>,
    Vec<u8>,
    i64,
    String,
    Option<String>,
);
type AttachmentDescriptorRow = (String, String, i64, i64, String, i64, Vec<u8>);

impl Client {
    pub fn set_draft_sync_supported(&self, supported: bool) -> Result<(), Error> {
        let store = self.lock()?;
        store.conn.execute(
            "UPDATE draft_sync_meta SET supported=? WHERE id=1",
            [i64::from(supported)],
        )?;
        if supported {
            store.conn.execute(
                "UPDATE draft_sync SET state='syncing' WHERE state='paused' AND pending_revision_id IS NOT NULL",
                [],
            )?;
        } else {
            store.conn.execute(
                "UPDATE draft_sync SET state='paused' WHERE state IN ('saved_local','syncing','synced','failed')",
                [],
            )?;
        }
        drop(store);
        self.recover_terminal_send_intents()?;
        Ok(())
    }

    pub fn draft_sync_cursor(&self) -> Result<String, Error> {
        let store = self.lock()?;
        store
            .conn
            .query_row("SELECT cursor FROM draft_sync_meta WHERE id=1", [], |row| {
                row.get(0)
            })
            .map_err(Into::into)
    }

    /// An interrupted bootstrap must restart at cursor zero; incremental pages never prove a full scan.
    pub fn draft_bootstrap_required(&self) -> Result<bool, Error> {
        let store = self.lock()?;
        let complete: i64 = store.conn.query_row(
            "SELECT bootstrap_complete FROM draft_sync_meta WHERE id=1",
            [],
            |row| row.get(0),
        )?;
        Ok(complete == 0)
    }

    pub fn pending_draft_uploads(&self) -> Result<Vec<DraftUpload>, Error> {
        let store = self.lock()?;
        let ids: Vec<(String, String)> = store.conn.prepare(
            "SELECT DISTINCT a.attachment_id, d.draft_id FROM attachments a JOIN compose_drafts c ON instr(c.attachment_ids,a.attachment_id)>0 JOIN draft_sync d ON d.draft_id=c.draft_id WHERE a.state='pending_upload' AND d.state NOT IN ('local_only','paused') AND NOT EXISTS(SELECT 1 FROM draft_sync_resurrection_attachments r WHERE r.new_attachment_id=a.attachment_id AND r.available=0)"
        )?.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<Result<_, _>>()?;
        ids.into_iter()
            .map(|(attachment, draft)| {
                let attachment_id = AttachmentId(parse_uuid(&attachment)?);
                let draft_id = DraftId(parse_uuid(&draft)?);
                let row = attachment_row(&store.conn, attachment_id)?.ok_or(Error::Database)?;
                Ok(DraftUpload {
                    draft_id,
                    object: CipherObject {
                        attachment_id,
                        ciphertext_bytes: row.ciphertext_bytes,
                        ciphertext_sha256: row.ciphertext_sha256,
                        remote_object_id: row.remote_object_id,
                    },
                })
            })
            .collect()
    }

    /// Pushes are gated only on this device's own uploads still pending. Remote-origin
    /// attachments are already stored by the server, so they never hold a push back.
    pub fn pending_draft_pushes(&self) -> Result<Vec<DraftPush>, Error> {
        let store = self.lock()?;
        let rows: Vec<PendingPushRow> = store.conn.prepare(
            "SELECT d.draft_id,d.pending_revision_id,d.accepted_revision_id,d.pending_sealed_record,d.authored_dirty,c.attachment_ids,d.resolves_json FROM draft_sync d JOIN compose_drafts c ON c.draft_id=d.draft_id WHERE d.pending_revision_id IS NOT NULL AND d.pending_sealed_record IS NOT NULL AND d.state NOT IN ('paused','local_only') AND d.discard_pending=0 AND NOT EXISTS(SELECT 1 FROM json_each(c.attachment_ids) ids JOIN attachments a ON a.attachment_id=ids.value WHERE a.state='pending_upload')"
        )?.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)))?.collect::<Result<_, _>>()?;
        rows.into_iter()
            .map(
                |(draft, revision, base, sealed_record, authored, attachments, resolves)| {
                    Ok(DraftPush {
                        draft_id: DraftId(parse_uuid(&draft)?),
                        revision_id: parse_uuid(&revision)?,
                        base_revision_id: parse_optional_uuid(base)?,
                        sealed_record,
                        attachment_ids: serde_json::from_str(&attachments)
                            .map_err(|_| Error::Database)?,
                        authored: authored != 0,
                        resolves_branch_ids: resolves
                            .map(|json| {
                                serde_json::from_str::<Vec<Uuid>>(&json)
                                    .map_err(|_| Error::Database)
                            })
                            .transpose()?
                            .unwrap_or_default(),
                    })
                },
            )
            .collect()
    }

    /// Records a PUT attempt before transport. An ACK can be lost after the server accepts it,
    /// so this evidence must outlive the pending snapshot and support negotiation.
    pub fn mark_draft_push_attempt(
        &self,
        draft_id: DraftId,
        revision_id: Uuid,
    ) -> Result<bool, Error> {
        let mut store = self.lock()?;
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let eligible: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM draft_sync WHERE draft_id=? AND pending_revision_id=? AND pending_sealed_record IS NOT NULL AND state NOT IN ('retired', 'local_only') AND discard_pending=0)",
            params![draft_id.to_string(), revision_id.to_string()],
            |row| row.get(0),
        )?;
        if eligible {
            tx.execute(
                "INSERT OR IGNORE INTO draft_push_attempts(draft_id,revision_id) VALUES(?,?)",
                params![draft_id.to_string(), revision_id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(eligible)
    }

    pub fn record_draft_push(
        &self,
        draft_id: DraftId,
        revision_id: Uuid,
        outcome: DraftPushOutcome,
    ) -> Result<(), Error> {
        if let DraftPushOutcome::Terminated(reason) = outcome {
            return self.apply_draft_changes(
                vec![RemoteDraftChange::Terminal {
                    draft_id,
                    reason,
                    command_id: None,
                }],
                &self.draft_sync_cursor()?,
            );
        }
        let store = self.lock()?;
        match outcome {
            DraftPushOutcome::Accepted {
                branch_id,
                expires_at_ms,
                ..
            } => {
                let pending: Option<Vec<u8>> = store.conn.query_row(
                    "SELECT pending_sealed_record FROM draft_sync WHERE draft_id=? AND pending_revision_id=?",
                    params![draft_id.to_string(), revision_id.to_string()],
                    |row| row.get(0),
                ).optional()?;
                if let Some(sealed) = pending {
                    let record = open_record(&store, draft_id, revision_id, &sealed)?;
                    store_merge_base(&store.conn, &record)?;
                } else {
                    clear_merge_state(&store.conn, draft_id)?;
                }
                let local_revision: i64 = store.conn.query_row(
                    "SELECT local_revision FROM draft_sync_revisions WHERE draft_id=? AND revision_id=?",
                    params![draft_id.to_string(), revision_id.to_string()],
                    |row| row.get(0),
                ).optional()?.ok_or(Error::Database)?;
                store.conn.execute(
                    "UPDATE draft_sync SET branch_id=?,branch_closed=0,accepted_revision_id=?,accepted_local_revision=?,pending_revision_id=CASE WHEN pending_revision_id=? THEN NULL ELSE pending_revision_id END,pending_sealed_record=CASE WHEN pending_revision_id=? THEN NULL ELSE pending_sealed_record END,pending_local_revision=CASE WHEN pending_revision_id=? THEN NULL ELSE pending_local_revision END,authored_dirty=CASE WHEN pending_revision_id=? THEN 0 ELSE authored_dirty END,expires_at_ms=?,resolves_json=CASE WHEN pending_revision_id=? THEN NULL ELSE resolves_json END,state=CASE WHEN pending_revision_id=? THEN 'synced' ELSE state END WHERE draft_id=? AND state!='retired' AND (accepted_local_revision IS NULL OR accepted_local_revision<=?)",
                    params![branch_id.to_string(), revision_id.to_string(), local_revision, revision_id.to_string(), revision_id.to_string(), revision_id.to_string(), revision_id.to_string(), expires_at_ms, revision_id.to_string(), revision_id.to_string(), draft_id.to_string(), local_revision],
                )?;
                store.conn.execute(
                    "DELETE FROM draft_sync_merge_fallbacks WHERE draft_id=? AND NOT EXISTS(SELECT 1 FROM draft_sync WHERE draft_id=? AND pending_revision_id=?)",
                    params![draft_id.to_string(), draft_id.to_string(), revision_id.to_string()],
                )?;
            }
            DraftPushOutcome::Terminated(_) => {
                unreachable!("handled before acquiring the store lock")
            }
            DraftPushOutcome::BranchLimit | DraftPushOutcome::Rejected => {
                store.conn.execute(
                    "UPDATE draft_sync SET state='failed' WHERE draft_id=? AND state!='retired'",
                    [draft_id.to_string()],
                )?;
                promote_merge_fallback(&store.conn, draft_id)?;
            }
            DraftPushOutcome::AttachmentUnavailable => {
                store.conn.execute(
                    "UPDATE draft_sync SET state='failed' WHERE draft_id=? AND state!='retired'",
                    [draft_id.to_string()],
                )?;
            }
        }
        Ok(())
    }

    pub fn pending_draft_discards(&self) -> Result<Vec<DraftId>, Error> {
        let store = self.lock()?;
        store
            .conn
            .prepare("SELECT draft_id FROM draft_sync WHERE discard_pending=1")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(DraftId(parse_uuid(&r?)?)))
            .collect()
    }

    pub fn record_draft_discard(&self, draft_id: DraftId) -> Result<(), Error> {
        self.lock()?.conn.execute(
            "UPDATE draft_sync SET discard_pending=0 WHERE draft_id=?",
            [draft_id.to_string()],
        )?;
        Ok(())
    }

    pub fn apply_draft_changes(
        &self,
        changes: Vec<RemoteDraftChange>,
        next_after: &str,
    ) -> Result<(), Error> {
        if !canonical_cursor(next_after) {
            return Err(Error::InvalidCursor);
        }
        let records = {
            let store = self.lock()?;
            changes
                .iter()
                .map(|change| match change {
                    RemoteDraftChange::Head {
                        draft_id,
                        revision_id,
                        sealed_record,
                        ..
                    } => match open_record(&store, *draft_id, *revision_id, sealed_record) {
                        Ok(record) => Ok(Some(record)),
                        // A malformed or unauthentic record can never become valid on retry.
                        // Preserve only opaque identities and let unrelated records progress.
                        Err(Error::Crypto) => Ok(None),
                        // Historical keys may arrive after this page; do not advance the cursor.
                        Err(error) => Err(error),
                    },
                    _ => Ok(None),
                })
                .collect::<Result<Vec<Option<DraftRecordV1>>, Error>>()?
        };
        for draft_id in changes.iter().filter_map(|change| match change {
            RemoteDraftChange::Terminal {
                draft_id,
                reason: DraftTerminalReasonCore::Expired,
                ..
            } => Some(*draft_id),
            _ => None,
        }) {
            self.resurrect_expired_draft(draft_id)?;
        }
        let mut store = self.lock()?;
        let merge_key = store.active_epoch.and_then(|epoch| {
            store
                .keys
                .get(&epoch)
                .and_then(|keys| keys.draft.clone())
                .map(|key| (epoch, key))
        });
        let vault_id = store.config.vault_id;
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (change, record) in changes.into_iter().zip(records) {
            match change {
                RemoteDraftChange::Head {
                    draft_id,
                    branch_id,
                    revision_id,
                    sealed_record: _,
                    attachment_ids,
                    expires_at_ms,
                    ..
                } => {
                    let Some(record) = record else {
                        quarantine_draft_head(&tx, draft_id, revision_id)?;
                        continue;
                    };
                    tx.execute_batch("SAVEPOINT draft_head")?;
                    if let Err(error) = apply_head(
                        &tx,
                        branch_id,
                        expires_at_ms,
                        attachment_ids,
                        record,
                        vault_id,
                        merge_key.as_ref(),
                    ) {
                        if error == Error::Crypto {
                            tx.execute_batch("ROLLBACK TO draft_head; RELEASE draft_head")?;
                            quarantine_draft_head(&tx, draft_id, revision_id)?;
                            continue;
                        }
                        return Err(error);
                    }
                    tx.execute_batch("RELEASE draft_head")?;
                }
                RemoteDraftChange::BranchClosed {
                    draft_id,
                    branch_id,
                } => {
                    remove_media_references(&tx, "conflict", &branch_id.to_string())?;
                    tx.execute(
                        "DELETE FROM draft_sync_conflicts WHERE remote_draft_id=? AND branch_id=?",
                        params![draft_id.to_string(), branch_id.to_string()],
                    )?;
                    remove_pending_resolve(&tx, draft_id, branch_id)?;
                    tx.execute(
                        "UPDATE draft_sync SET branch_closed=1 WHERE draft_id=? AND branch_id=?",
                        params![draft_id.to_string(), branch_id.to_string()],
                    )?;
                }
                RemoteDraftChange::Terminal {
                    draft_id,
                    reason,
                    command_id,
                } => {
                    tx.execute(
                        "INSERT INTO draft_sync_terminals(draft_id,reason,command_id) VALUES(?,?,?) ON CONFLICT(draft_id) DO UPDATE SET reason=excluded.reason,command_id=COALESCE(excluded.command_id,draft_sync_terminals.command_id)",
                        params![draft_id.to_string(), terminal_code(reason), command_id.map(|id| id.to_string())],
                    )?;
                    match reason {
                        DraftTerminalReasonCore::Discarded => remove_terminal_draft(&tx, draft_id)?,
                        DraftTerminalReasonCore::Sent => {
                            let intent: Option<(String, i64)> = tx.query_row(
                                "SELECT command_id,queued FROM draft_send_intents WHERE draft_id=?",
                                [draft_id.to_string()],
                                |row| Ok((row.get(0)?, row.get(1)?)),
                            ).optional()?;
                            match (intent.as_ref(), command_id) {
                                (Some((intent_id, _)), Some(command_id))
                                    if *intent_id != command_id.to_string() =>
                                {
                                    keep_local_after_terminal(&tx, draft_id, "kept-after-sent")?;
                                }
                                (None, _) => {
                                    keep_local_after_terminal(&tx, draft_id, "kept-after-sent")?;
                                }
                                _ => {}
                            }
                        }
                        DraftTerminalReasonCore::Expired => {
                            keep_local_after_terminal(&tx, draft_id, "restored-after-expiry")?
                        }
                    }
                }
            }
        }
        tx.execute(
            "UPDATE draft_sync_meta SET cursor=? WHERE id=1",
            [next_after],
        )?;
        tx.commit()?;
        drop(store);
        self.recover_terminal_send_intents()?;
        Ok(())
    }

    /// Call this only after an entire `after=0` bootstrap has completed. Partial pages never
    /// prove absence and must not call it.
    pub fn finish_draft_bootstrap(&self, seen_draft_ids: &[DraftId]) -> Result<(), Error> {
        let mut store = self.lock()?;
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids: Vec<String> = tx
            .prepare("SELECT draft_id FROM draft_sync WHERE branch_id IS NOT NULL")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        let mut dirty_absent = Vec::new();
        for id in ids {
            let draft_id = DraftId(parse_uuid(&id)?);
            if !seen_draft_ids.contains(&draft_id) {
                if is_dirty(&tx, draft_id)? {
                    dirty_absent.push(draft_id);
                } else {
                    remove_terminal_draft(&tx, draft_id)?;
                }
            }
        }
        tx.execute(
            "UPDATE draft_sync SET state='synced' WHERE branch_id IS NOT NULL AND state='paused' AND pending_revision_id IS NULL AND pending_sealed_record IS NULL AND authored_dirty=0",
            [],
        )?;
        tx.commit()?;
        drop(store);
        for draft_id in dirty_absent {
            self.resurrect_expired_draft(draft_id)?;
        }
        self.lock()?.conn.execute(
            "UPDATE draft_sync_meta SET bootstrap_complete=1 WHERE id=1",
            [],
        )?;
        Ok(())
    }

    pub fn begin_draft_send(
        &self,
        draft_id: DraftId,
        expected_revision: u64,
        parts: Option<DraftSendParts>,
    ) -> Result<DraftSendGate, Error> {
        self.begin_draft_send_for_group(draft_id, expected_revision, parts, None)
    }

    /// Like `begin_draft_send`, and persists the group fields of a send to two or more members
    /// in the intent, so the remote acceptance sends with them.
    pub fn begin_draft_send_for_group(
        &self,
        draft_id: DraftId,
        expected_revision: u64,
        parts: Option<DraftSendParts>,
        group: Option<GroupDraftSend>,
    ) -> Result<DraftSendGate, Error> {
        let store = self.lock()?;
        let existing: Option<(String, String)> = store
            .conn
            .query_row(
                "SELECT revision_id,command_id FROM draft_send_intents WHERE draft_id=?",
                [draft_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((revision_id, command_id)) = existing {
            return Ok(DraftSendGate {
                remote: Some(RemoteDraftSend {
                    draft_id,
                    revision_id: parse_uuid(&revision_id)?,
                    command_id: CommandId(parse_uuid(&command_id)?),
                }),
            });
        }
        let row: Option<(Option<String>, Option<String>, i64)> = store
            .conn
            .query_row(
                "SELECT accepted_revision_id,pending_revision_id,authored_dirty FROM draft_sync WHERE draft_id=?",
                [draft_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((accepted, pending, dirty)) = row else {
            return Ok(DraftSendGate { remote: None });
        };
        if accepted.is_none() {
            let attempted = match pending.as_deref() {
                Some(_) => store.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM draft_push_attempts WHERE draft_id=?)",
                    [draft_id.to_string()],
                    |row| row.get(0),
                )?,
                None => false,
            };
            if attempted {
                return Err(Error::DraftNotSynced);
            }
            return Ok(DraftSendGate { remote: None });
        }
        if dirty != 0 || pending.is_some() {
            return Err(Error::DraftNotSynced);
        }
        let revision_id = parse_uuid(&accepted.ok_or(Error::DraftNotSynced)?)?;
        let command_id = CommandId::new();
        let draft = crate::load_compose_draft(&store.conn, "draft_id", &draft_id.to_string())?
            .ok_or(Error::NotFound)?;
        if draft.revision != expected_revision {
            return Err(Error::StaleDraft {
                current_revision: draft.revision,
            });
        }
        if let Some(parts) = parts.as_ref() {
            crate::validate_draft_send_parts(&draft, parts)?;
        }
        crate::compose_references(&store.conn, &draft.attachment_ids)?;
        let attachment_ids = draft.attachment_ids.clone();
        // The stored draft's name is authoritative, so the accepted send matches the stored draft.
        let group = group.map(|group| GroupDraftSend {
            group_name: draft.group_name.clone(),
            ..group
        });
        let snapshot = serde_json::to_vec(&SendIntentSnapshot {
            conversation_id: draft.conversation_id,
            text: draft.text,
            recipients: draft.recipients,
            attachment_ids: draft.attachment_ids,
            route: draft.route,
            parts,
            group,
        })
        .map_err(|_| Error::Database)?;
        store.conn.execute(
                "INSERT INTO draft_send_intents(draft_id,revision_id,command_id,expected_revision,sealed_snapshot,queued) VALUES(?,?,?,?,?,0)",
                params![draft_id.to_string(), revision_id.to_string(), command_id.to_string(), i64::try_from(expected_revision).map_err(|_| Error::Database)?, snapshot],
        )?;
        add_media_references(
            &store.conn,
            "intent",
            &command_id.to_string(),
            &attachment_ids,
        )?;
        Ok(DraftSendGate {
            remote: Some(RemoteDraftSend {
                draft_id,
                revision_id,
                command_id,
            }),
        })
    }

    /// Drops a locally unqueued intent only after a definitive server rejection for that command.
    pub fn abort_draft_send_intent(
        &self,
        draft_id: DraftId,
        command_id: CommandId,
    ) -> Result<(), Error> {
        self.lock()?.conn.execute(
            "DELETE FROM draft_send_intents WHERE draft_id=? AND command_id=? AND queued=0",
            params![draft_id.to_string(), command_id.to_string()],
        )?;
        self.lock()?.conn.execute(
            "DELETE FROM draft_media_references WHERE kind='intent' AND owner_id=? AND NOT EXISTS(SELECT 1 FROM draft_send_intents WHERE command_id=? AND queued=0)",
            params![command_id.to_string(), command_id.to_string()],
        )?;
        Ok(())
    }

    /// Replays the narrow crash window after a sent terminal commits but before its matching
    /// immutable intent has been queued. The command ID remains the queue idempotency key.
    fn recover_terminal_send_intents(&self) -> Result<(), Error> {
        let intents: Vec<(String, String, i64, Vec<u8>)> = {
            let store = self.lock()?;
            store.conn.prepare(
                "SELECT i.draft_id,i.command_id,i.expected_revision,i.sealed_snapshot FROM draft_send_intents i JOIN draft_sync_terminals t ON t.draft_id=i.draft_id WHERE t.reason='sent' AND t.command_id=i.command_id AND i.queued=0",
            )?.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?.collect::<Result<_, _>>()?
        };
        for (draft_id, command_id, expected_revision, snapshot) in intents {
            let parts = serde_json::from_slice::<SendIntentSnapshot>(&snapshot)
                .map_err(|_| Error::Database)?
                .parts;
            self.send_compose_draft_accepted_with_parts(
                DraftId(parse_uuid(&draft_id)?),
                u64::try_from(expected_revision).map_err(|_| Error::Database)?,
                CommandId(parse_uuid(&command_id)?),
                parts,
            )?;
        }
        Ok(())
    }

    pub fn send_compose_draft_accepted(
        &self,
        draft_id: DraftId,
        expected_revision: u64,
        command_id: CommandId,
    ) -> Result<(), Error> {
        self.send_compose_draft_accepted_with_parts(draft_id, expected_revision, command_id, None)
    }

    /// Returns the durable number of messages for this exact send intent.
    pub fn draft_send_intent_parts_count(
        &self,
        draft_id: DraftId,
        command_id: CommandId,
    ) -> Result<Option<u32>, Error> {
        let snapshot: Option<Vec<u8>> = self
            .lock()?
            .conn
            .query_row(
                "SELECT sealed_snapshot FROM draft_send_intents WHERE draft_id=? AND command_id=?",
                params![draft_id.to_string(), command_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        snapshot
            .map(|snapshot| {
                let snapshot: SendIntentSnapshot =
                    serde_json::from_slice(&snapshot).map_err(|_| Error::Database)?;
                match snapshot.parts {
                    None => Ok(1),
                    Some(parts) => u32::try_from(parts.image_groups.len())
                        .map_err(|_| Error::Database)?
                        .checked_add(u32::from(parts.caption))
                        .ok_or(Error::Database),
                }
            })
            .transpose()
    }

    pub fn send_compose_draft_accepted_with_parts(
        &self,
        draft_id: DraftId,
        expected_revision: u64,
        command_id: CommandId,
        parts: Option<DraftSendParts>,
    ) -> Result<(), Error> {
        // Acceptance is durable before this call; existing send machinery remains the only code
        // path allowed to create a local command envelope.
        let store = self.lock()?;
        let matched: Option<(i64, Vec<u8>)> = store
            .conn
            .query_row(
                "SELECT queued,sealed_snapshot FROM draft_send_intents WHERE draft_id=? AND command_id=?",
                params![draft_id.to_string(), command_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if matched.as_ref().is_some_and(|(queued, _)| *queued == 1) {
            return Ok(());
        }
        if matched.is_none() {
            return Err(Error::Conflict);
        }
        // The command/outbox transaction may have committed just before a crash, before the
        // intent flag was updated.  The command id is the durable idempotency key.
        let already_queued: bool = store.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM commands WHERE command_id=?)",
            [command_id.to_string()],
            |row| row.get(0),
        )?;
        if already_queued {
            store.conn.execute(
                "UPDATE draft_send_intents SET queued=1 WHERE draft_id=?",
                [draft_id.to_string()],
            )?;
            remove_media_references(&store.conn, "intent", &command_id.to_string())?;
            return Ok(());
        }
        let (_, snapshot) = matched.ok_or(Error::Database)?;
        let snapshot: SendIntentSnapshot =
            serde_json::from_slice(&snapshot).map_err(|_| Error::Database)?;
        let draft = ComposeDraft {
            draft_id,
            conversation_id: snapshot.conversation_id,
            text: snapshot.text.clone(),
            recipients: snapshot.recipients.clone(),
            attachment_ids: snapshot.attachment_ids.clone(),
            route: snapshot.route.clone(),
            revision: expected_revision,
            group_name: snapshot
                .group
                .as_ref()
                .and_then(|group| group.group_name.clone()),
        };
        if parts.is_some() && parts != snapshot.parts {
            return Err(Error::Conflict);
        }
        let parts = snapshot.parts;
        let group = snapshot.group;
        drop(store);
        self.send_compose_draft_snapshot(
            draft_id,
            expected_revision,
            command_id,
            draft,
            parts.as_ref(),
            group.as_ref(),
        )?;
        self.lock()?.conn.execute(
            "UPDATE draft_send_intents SET queued=1 WHERE draft_id=?",
            [draft_id.to_string()],
        )?;
        self.lock()?.conn.execute(
            "DELETE FROM draft_media_references WHERE kind='intent' AND owner_id=?",
            [command_id.to_string()],
        )?;
        Ok(())
    }

    /// Resolves the conflict versions of `draft_id`. `None` keeps the local content; `Some`
    /// applies that version locally. Either way the local content is re-sealed as an authored
    /// revision, and the push closes the other same-identity conflict branches.
    pub fn resolve_draft_conflict(
        &self,
        draft_id: DraftId,
        keep_branch: Option<Uuid>,
    ) -> Result<ComposeDraft, Error> {
        let mut guard = self.lock()?;
        let vault_id = guard.config.vault_id;
        let (epoch, key) = guard
            .active_epoch
            .and_then(|epoch| {
                guard
                    .keys
                    .get(&epoch)
                    .and_then(|keys| keys.draft.clone())
                    .map(|key| (epoch, key))
            })
            .ok_or(Error::KeysUnavailable)?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = crate::load_compose_draft(&tx, "draft_id", &draft_id.to_string())?
            .ok_or(Error::NotFound)?;
        let tracked_branch = sync_row(&tx, draft_id)?.and_then(|row| row.branch_id);
        let rows = conflicts(&tx, draft_id)?;
        let mut next = current.clone();
        let mut identity_base = None;
        if let Some(branch_id) = keep_branch {
            let (_, record) = rows
                .iter()
                .find(|(id, _)| *id == branch_id)
                .ok_or(Error::NotFound)?;
            next = ComposeDraft {
                draft_id,
                conversation_id: current.conversation_id,
                text: record.text.clone(),
                recipients: record.recipients.clone(),
                attachment_ids: record_attachment_ids(record),
                route: record.route.clone(),
                revision: current.revision.checked_add(1).ok_or(Error::Database)?,
                group_name: record.group_name.clone(),
            };
            tx.execute(
                "UPDATE compose_drafts SET text=?,recipients=?,attachment_ids=?,route=?,revision=?,group_name=? WHERE draft_id=?",
                params![
                    next.text,
                    json_string(&next.recipients)?,
                    json_string(&next.attachment_ids)?,
                    route_json(&next.route)?,
                    revision_i64(next.revision)?,
                    next.group_name,
                    draft_id.to_string()
                ],
            )?;
            tx.execute(
                "DELETE FROM draft_sync_conflicts WHERE draft_id=? AND branch_id=?",
                params![draft_id.to_string(), branch_id.to_string()],
            )?;
            remove_media_references(&tx, "conflict", &branch_id.to_string())?;
            if record.draft_id == draft_id {
                identity_base = Some((branch_id, record.revision_id));
            }
        }
        seal_pending(&tx, &next, true, vault_id, epoch, &key)?;
        // Versions from another remote identity cannot be closed by this draft's push.
        let effective_target = identity_base
            .as_ref()
            .map(|(branch_id, _)| *branch_id)
            .or(tracked_branch);
        let mut resolves: Vec<Uuid> = rows
            .iter()
            .filter(|(branch_id, record)| {
                record.draft_id == draft_id && effective_target != Some(*branch_id)
            })
            .map(|(branch_id, _)| *branch_id)
            .collect();
        if let Some(branch_id) =
            tracked_branch.filter(|branch_id| Some(*branch_id) != effective_target)
        {
            resolves.push(branch_id);
        }
        resolves.sort_unstable();
        resolves.dedup();
        for (branch_id, _) in rows
            .iter()
            .filter(|(_, record)| record.draft_id != draft_id)
        {
            tx.execute(
                "DELETE FROM draft_sync_conflicts WHERE draft_id=? AND branch_id=?",
                params![draft_id.to_string(), branch_id.to_string()],
            )?;
            remove_media_references(&tx, "conflict", &branch_id.to_string())?;
        }
        let resolves_json = if resolves.is_empty() {
            None
        } else {
            Some(json_string(&resolves)?)
        };
        tx.execute(
            "UPDATE draft_sync SET resolves_json=?,branch_closed=0 WHERE draft_id=?",
            params![resolves_json, draft_id.to_string()],
        )?;
        if let Some((branch_id, revision_id)) = identity_base {
            tx.execute(
                "UPDATE draft_sync SET branch_id=?,branch_closed=0,accepted_revision_id=? WHERE draft_id=?",
                params![
                    branch_id.to_string(),
                    revision_id.to_string(),
                    draft_id.to_string()
                ],
            )?;
        }
        tx.commit()?;
        drop(guard);
        self.compose_draft(draft_id)?.ok_or(Error::NotFound)
    }

    pub fn draft_sync_status(&self, draft_id: DraftId) -> Result<DraftSyncStatus, Error> {
        let store = self.lock()?;
        let row: Option<DraftSyncStatusRow> = store
            .conn
            .query_row(
                "SELECT state,expires_at_ms,notice,accepted_revision_id,EXISTS(SELECT 1 FROM draft_push_attempts a WHERE a.draft_id=draft_sync.draft_id) FROM draft_sync WHERE draft_id=?",
                [draft_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let (state, expires_at_ms, notice, accepted_revision_id, pending_attempted) =
            row.unwrap_or(("local_only".into(), None, None, None, false));
        let conflicts = conflicts(&store.conn, draft_id)?
            .iter()
            .map(|(branch_id, record)| conflict_version(*branch_id, record))
            .collect();
        Ok(DraftSyncStatus {
            state: state_from_code(&state),
            expires_at_ms,
            conflicts,
            notice: notice.as_deref().and_then(notice_from_code),
            send_requires_connection: accepted_revision_id.is_some() || pending_attempted,
        })
    }

    pub fn acknowledge_draft_notice(&self, draft_id: DraftId) -> Result<(), Error> {
        self.lock()?.conn.execute(
            "UPDATE draft_sync SET notice=NULL WHERE draft_id=?",
            [draft_id.to_string()],
        )?;
        Ok(())
    }

    /// Records a transfer failure reported by the host. The fact only affects the presentation
    /// mapping until the attachment's state advances or the host clears it.
    pub fn record_draft_attachment_transfer_failure(
        &self,
        attachment_id: AttachmentId,
        direction: DraftTransferDirection,
    ) -> Result<(), Error> {
        let store = self.lock()?;
        if attachment_row(&store.conn, attachment_id)?.is_none() {
            return Err(Error::NotFound);
        }
        store.conn.execute(
            "INSERT INTO draft_attachment_failures(attachment_id,direction) VALUES(?,?) ON CONFLICT(attachment_id) DO UPDATE SET direction=excluded.direction",
            params![attachment_id.to_string(), transfer_code(direction)],
        )?;
        Ok(())
    }

    /// Clears a recorded transfer failure, for example when the host retries the transfer.
    pub fn clear_draft_attachment_transfer_failure(
        &self,
        attachment_id: AttachmentId,
    ) -> Result<(), Error> {
        self.lock()?.conn.execute(
            "DELETE FROM draft_attachment_failures WHERE attachment_id=?",
            [attachment_id.to_string()],
        )?;
        Ok(())
    }

    pub fn draft_attachment_states(
        &self,
        draft_id: DraftId,
    ) -> Result<Vec<(AttachmentId, DraftAttachmentState)>, Error> {
        let store = self.lock()?;
        let ids: String = store.conn.query_row(
            "SELECT attachment_ids FROM compose_drafts WHERE draft_id=?",
            [draft_id.to_string()],
            |r| r.get(0),
        )?;
        let ids: Vec<AttachmentId> = serde_json::from_str(&ids).map_err(|_| Error::Database)?;
        let sync_enabled = sync_row(&store.conn, draft_id)?
            .is_some_and(|row| !matches!(row.state.as_str(), "local_only" | "paused"));
        ids.into_iter()
            .map(|id| Ok((id, attachment_state(&store, id, sync_enabled)?)))
            .collect()
    }
}

/// Presentation mapping for one attachment of a compose draft. Missing rows are media that is
/// still being prepared; storage-policy removal and missing local bytes are `Unavailable`.
fn attachment_state(
    store: &Store,
    id: AttachmentId,
    sync_enabled: bool,
) -> Result<DraftAttachmentState, Error> {
    let conn = &store.conn;
    let unavailable_resurrection: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM draft_sync_resurrection_attachments WHERE new_attachment_id=? AND available=0)",
        [id.to_string()],
        |r| r.get(0),
    )?;
    if unavailable_resurrection {
        return Ok(DraftAttachmentState::Unavailable);
    }
    let Some(row) = attachment_row(conn, id)? else {
        return Ok(DraftAttachmentState::Saving);
    };
    let removed: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attachment_remote_state WHERE attachment_id=? AND removed_by_storage_policy=1)",
        [id.to_string()],
        |r| r.get(0),
    )?;
    if removed {
        return Ok(DraftAttachmentState::Unavailable);
    }
    let failure: Option<String> = conn
        .query_row(
            "SELECT direction FROM draft_attachment_failures WHERE attachment_id=?",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let has_bytes = media::cipher_path(&store.media_dir, id).exists();
    Ok(match row.state {
        AttachmentState::PendingDownload => {
            if failure.as_deref() == Some("download") {
                DraftAttachmentState::DownloadFailed
            } else {
                DraftAttachmentState::Downloading
            }
        }
        _ if !has_bytes
            && !(row.state == AttachmentState::Available && row.remote_object_id.is_some()) =>
        {
            DraftAttachmentState::Unavailable
        }
        AttachmentState::PendingUpload if sync_enabled && failure.as_deref() == Some("upload") => {
            DraftAttachmentState::UploadFailed
        }
        AttachmentState::PendingUpload if sync_enabled => DraftAttachmentState::Waiting,
        AttachmentState::PendingUpload => DraftAttachmentState::Local,
        AttachmentState::Uploaded => DraftAttachmentState::Uploaded,
        AttachmentState::Available if row.remote_object_id.is_some() => {
            DraftAttachmentState::Uploaded
        }
        AttachmentState::Available => DraftAttachmentState::Local,
    })
}

/// Stores or adopts one verified remote head. A head never overwrites locally dirty content.
fn apply_head(
    conn: &Connection,
    branch_id: Uuid,
    expires_at_ms: i64,
    attachment_ids: Vec<AttachmentId>,
    record: DraftRecordV1,
    vault_id: VaultId,
    merge_key: Option<&(u32, PurposeKey)>,
) -> Result<(), Error> {
    if attachment_ids != record_attachment_ids(&record) {
        return Err(Error::Crypto);
    }
    let draft_id = record.draft_id;
    if is_retired_draft(conn, draft_id)? || discard_pending(conn, draft_id)? {
        return Ok(());
    }
    install_remote_attachments(conn, &record)?;
    if crate::load_compose_draft(conn, "draft_id", &draft_id.to_string())?.is_some() {
        if let Some(sync) = sync_row(conn, draft_id)? {
            if sync.branch_id == Some(branch_id)
                && sync.accepted_revision == Some(record.revision_id)
            {
                return acknowledge_echo(conn, &record, branch_id, expires_at_ms);
            }
            if (sync.branch_id.is_some() && sync.branch_id != Some(branch_id)) || sync.dirty {
                if let Some((epoch, key)) = merge_key
                    && try_merge_head(
                        conn,
                        draft_id,
                        branch_id,
                        expires_at_ms,
                        &record,
                        vault_id,
                        *epoch,
                        key,
                    )?
                {
                    return Ok(());
                }
                return store_conflict(conn, draft_id, branch_id, &record);
            }
        } else if is_dirty(conn, draft_id)? {
            return store_conflict(conn, draft_id, branch_id, &record);
        }
        conn.execute(
            "UPDATE compose_drafts SET text=?,recipients=?,attachment_ids=?,route=?,group_name=?,revision=revision+1 WHERE draft_id=?",
            params![
                record.text,
                json_string(&record.recipients)?,
                json_string(&record_attachment_ids(&record))?,
                route_json(&record.route)?,
                record.group_name,
                draft_id.to_string()
            ],
        )?;
        return mark_synced(conn, &record, branch_id, expires_at_ms);
    }
    let conversation =
        matching_conversation(conn, &record.recipients)?.unwrap_or_else(ConversationId::new);
    if let Some(local) =
        crate::load_compose_draft(conn, "conversation_id", &conversation.to_string())?
    {
        if !is_blank(conn, &local)? {
            return store_conflict(conn, local.draft_id, branch_id, &record);
        }
        retire_blank_draft(conn, local.draft_id)?;
    }
    insert_adopted(conn, conversation, &record, branch_id, expires_at_ms)
}

fn store_conflict(
    conn: &Connection,
    local_draft_id: DraftId,
    branch_id: Uuid,
    record: &DraftRecordV1,
) -> Result<(), Error> {
    let json = serde_json::to_vec(record).map_err(|_| Error::Database)?;
    remove_media_references(conn, "conflict", &branch_id.to_string())?;
    conn.execute(
        "INSERT INTO draft_sync_conflicts(draft_id,remote_draft_id,branch_id,record) VALUES(?,?,?,?) ON CONFLICT(draft_id,branch_id) DO UPDATE SET remote_draft_id=excluded.remote_draft_id,record=excluded.record",
        params![local_draft_id.to_string(), record.draft_id.to_string(), branch_id.to_string(), json],
    )?;
    add_media_references(
        conn,
        "conflict",
        &branch_id.to_string(),
        &record_attachment_ids(record),
    )?;
    Ok(())
}

fn insert_adopted(
    conn: &Connection,
    conversation: ConversationId,
    record: &DraftRecordV1,
    branch_id: Uuid,
    expires_at_ms: i64,
) -> Result<(), Error> {
    let has_messages: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE conversation_id=?)",
        params![conversation.to_string()],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO compose_drafts(draft_id,conversation_id,text,recipients,attachment_ids,route,revision,group_name,draft_only) VALUES(?,?,?,?,?,?,0,?,?)",
        params![
            record.draft_id.to_string(),
            conversation.to_string(),
            record.text,
            json_string(&record.recipients)?,
            json_string(&record_attachment_ids(record))?,
            route_json(&record.route)?,
            record.group_name,
            i64::from(!has_messages)
        ],
    )?;
    mark_synced(conn, record, branch_id, expires_at_ms)
}

fn mark_synced(
    conn: &Connection,
    record: &DraftRecordV1,
    branch_id: Uuid,
    expires_at_ms: i64,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO draft_sync(draft_id,branch_id,accepted_revision_id,authored_dirty,expires_at_ms,state) VALUES(?,?,?,0,?,'synced') ON CONFLICT(draft_id) DO UPDATE SET branch_id=excluded.branch_id,branch_closed=0,accepted_revision_id=excluded.accepted_revision_id,pending_revision_id=NULL,pending_sealed_record=NULL,authored_dirty=0,expires_at_ms=excluded.expires_at_ms,state='synced'",
        params![
            record.draft_id.to_string(),
            branch_id.to_string(),
            record.revision_id.to_string(),
            expires_at_ms
        ],
    )?;
    store_merge_base(conn, record)?;
    Ok(())
}

/// An accepted echo acknowledges only the accepted revision; a later local save remains pending.
fn acknowledge_echo(
    conn: &Connection,
    record: &DraftRecordV1,
    branch_id: Uuid,
    expires_at_ms: i64,
) -> Result<(), Error> {
    conn.execute(
        "UPDATE draft_sync SET branch_id=?,branch_closed=0,accepted_revision_id=?,expires_at_ms=?,state=CASE WHEN pending_revision_id IS NULL THEN 'synced' ELSE state END WHERE draft_id=?",
        params![branch_id.to_string(), record.revision_id.to_string(), expires_at_ms, record.draft_id.to_string()],
    )?;
    store_merge_base(conn, record)?;
    Ok(())
}

/// Removes a blank local draft that a remote identity now occupies. A synced one is discarded
/// remotely as well, because its identity would otherwise remain live on the server.
fn retire_blank_draft(conn: &Connection, draft_id: DraftId) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM compose_drafts WHERE draft_id=?",
        [draft_id.to_string()],
    )?;
    queue_discard_if_synced(conn, draft_id)?;
    conn.execute(
        "DELETE FROM draft_sync WHERE draft_id=? AND branch_id IS NULL",
        [draft_id.to_string()],
    )?;
    clear_merge_state(conn, draft_id)?;
    Ok(())
}

/// Called by local discard: a draft with a remote branch needs a remote discard.
pub(crate) fn queue_discard_if_synced(conn: &Connection, draft_id: DraftId) -> Result<(), Error> {
    conn.execute(
        "UPDATE draft_sync SET discard_pending=1 WHERE draft_id=? AND branch_id IS NOT NULL",
        [draft_id.to_string()],
    )?;
    clear_merge_state(conn, draft_id)?;
    Ok(())
}

/// A local send retires its old remote draft identity before creating the next blank compose
/// draft. A delayed acceptance may still establish that old identity remotely, so retain the
/// discard marker until the host has acknowledged its cleanup.
pub(crate) fn retire_sent_draft(
    conn: &Connection,
    draft_id: DraftId,
    cleared_revision: u64,
    queue_remote_discard: bool,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO draft_sync(draft_id,state,retired_revision,discard_pending) VALUES(?,'retired',?,?) ON CONFLICT(draft_id) DO UPDATE SET pending_revision_id=NULL,pending_sealed_record=NULL,pending_local_revision=NULL,authored_dirty=0,resolves_json=NULL,discard_pending=excluded.discard_pending,state='retired',retired_revision=excluded.retired_revision",
        params![draft_id.to_string(), revision_i64(cleared_revision)?, i64::from(queue_remote_discard)],
    )?;
    clear_merge_state(conn, draft_id)?;
    Ok(())
}

pub(crate) fn is_retired_draft(conn: &Connection, draft_id: DraftId) -> Result<bool, Error> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM draft_sync WHERE draft_id=? AND state='retired')",
        [draft_id.to_string()],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

pub(crate) fn retired_draft_revision(
    conn: &Connection,
    draft_id: DraftId,
) -> Result<Option<u64>, Error> {
    conn.query_row(
        "SELECT retired_revision FROM draft_sync WHERE draft_id=? AND state='retired'",
        [draft_id.to_string()],
        |row| row.get::<_, Option<i64>>(0),
    )
    .optional()?
    .flatten()
    .map(|revision| u64::try_from(revision).map_err(|_| Error::Database))
    .transpose()
}

fn matching_conversation(
    conn: &Connection,
    recipients: &[String],
) -> Result<Option<ConversationId>, Error> {
    // An empty recipient set would match every recipient-less draft.
    if recipients.is_empty() {
        return Ok(None);
    }
    let region = conversation_match::identity_region(conn)?;
    crate::conversation_for_compose_recipients(conn, recipients, ConversationId::new(), &region)
}

fn is_blank(conn: &Connection, draft: &ComposeDraft) -> Result<bool, Error> {
    Ok(
        draft.text.is_empty()
            && draft.attachment_ids.is_empty()
            && !is_dirty(conn, draft.draft_id)?,
    )
}

fn is_dirty(conn: &Connection, draft_id: DraftId) -> Result<bool, Error> {
    Ok(sync_row(conn, draft_id)?.is_some_and(|row| row.dirty))
}

fn discard_pending(conn: &Connection, draft_id: DraftId) -> Result<bool, Error> {
    let pending: Option<i64> = conn
        .query_row(
            "SELECT discard_pending FROM draft_sync WHERE draft_id=?",
            [draft_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(pending.unwrap_or(0) != 0)
}

struct SyncRow {
    branch_id: Option<Uuid>,
    branch_closed: bool,
    accepted_revision: Option<Uuid>,
    dirty: bool,
    state: String,
}

type SyncRowColumns = (Option<String>, i64, Option<String>, i64, String);

type DraftSyncStatusRow = (String, Option<i64>, Option<String>, Option<String>, bool);

fn sync_row(conn: &Connection, draft_id: DraftId) -> Result<Option<SyncRow>, Error> {
    let row: Option<SyncRowColumns> = conn
        .query_row(
            "SELECT branch_id,branch_closed,accepted_revision_id,authored_dirty,state FROM draft_sync WHERE draft_id=?",
            [draft_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()?;
    row.map(|(branch, branch_closed, accepted, dirty, state)| {
        Ok(SyncRow {
            branch_id: parse_optional_uuid(branch)?,
            branch_closed: branch_closed != 0,
            accepted_revision: parse_optional_uuid(accepted)?,
            dirty: dirty != 0,
            state,
        })
    })
    .transpose()
}

/// Live conflict versions of a local draft, in arrival order.
fn conflicts(conn: &Connection, draft_id: DraftId) -> Result<Vec<(Uuid, DraftRecordV1)>, Error> {
    let rows: Vec<(String, Vec<u8>)> = conn
        .prepare(
            "SELECT branch_id,record FROM draft_sync_conflicts WHERE draft_id=? ORDER BY rowid",
        )?
        .query_map([draft_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    rows.into_iter()
        .map(|(branch, record)| {
            Ok((
                parse_uuid(&branch)?,
                serde_json::from_slice(&record).map_err(|_| Error::Database)?,
            ))
        })
        .collect()
}

fn pending_resolves(conn: &Connection, draft_id: DraftId) -> Result<Vec<Uuid>, Error> {
    let resolves: Option<String> = conn
        .query_row(
            "SELECT resolves_json FROM draft_sync WHERE draft_id=?",
            [draft_id.to_string()],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    resolves
        .map(|json| serde_json::from_str(&json).map_err(|_| Error::Database))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn remove_pending_resolve(
    conn: &Connection,
    draft_id: DraftId,
    closed_branch_id: Uuid,
) -> Result<(), Error> {
    let mut resolves = pending_resolves(conn, draft_id)?;
    resolves.retain(|branch_id| *branch_id != closed_branch_id);
    conn.execute(
        "UPDATE draft_sync SET resolves_json=? WHERE draft_id=?",
        params![
            (!resolves.is_empty())
                .then(|| json_string(&resolves))
                .transpose()?,
            draft_id.to_string(),
        ],
    )?;
    Ok(())
}

fn conflict_version(branch_id: Uuid, record: &DraftRecordV1) -> DraftConflictVersion {
    DraftConflictVersion {
        branch_id,
        text: record.text.clone(),
        attachment_ids: record_attachment_ids(record),
        recipient_ids: record.recipients.clone(),
        route: record.route.clone(),
    }
}

fn record_attachment_ids(record: &DraftRecordV1) -> Vec<AttachmentId> {
    record
        .attachments
        .iter()
        .map(|attachment| attachment.attachment_id)
        .collect()
}

#[derive(Deserialize)]
struct MergeBase {
    text: String,
    recipients: Vec<String>,
    attachment_ids: Vec<AttachmentId>,
    route: Option<GatewayRoute>,
}

fn store_merge_base(conn: &Connection, record: &DraftRecordV1) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO draft_sync_merge_bases(draft_id,text,recipients,attachment_ids,route) VALUES(?,?,?,?,?) ON CONFLICT(draft_id) DO UPDATE SET text=excluded.text,recipients=excluded.recipients,attachment_ids=excluded.attachment_ids,route=excluded.route",
        params![
            record.draft_id.to_string(),
            record.text,
            json_string(&record.recipients)?,
            json_string(&record_attachment_ids(record))?,
            route_json(&record.route)?,
        ],
    )?;
    Ok(())
}

fn merge_base(conn: &Connection, draft_id: DraftId) -> Result<Option<MergeBase>, Error> {
    let row: Option<(String, String, String, Option<String>)> = conn
        .query_row(
            "SELECT text,recipients,attachment_ids,route FROM draft_sync_merge_bases WHERE draft_id=?",
            [draft_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    row.map(|(text, recipients, attachment_ids, route)| {
        Ok(MergeBase {
            text,
            recipients: serde_json::from_str(&recipients).map_err(|_| Error::Database)?,
            attachment_ids: serde_json::from_str(&attachment_ids).map_err(|_| Error::Database)?,
            route: route.map(|value| decode(value.as_bytes())).transpose()?,
        })
    })
    .transpose()
}

#[allow(clippy::too_many_arguments)]
fn try_merge_head(
    conn: &Connection,
    draft_id: DraftId,
    remote_branch_id: Uuid,
    remote_expires_at_ms: i64,
    remote: &DraftRecordV1,
    vault_id: VaultId,
    epoch: u32,
    key: &PurposeKey,
) -> Result<bool, Error> {
    let Some(base) = merge_base(conn, draft_id)? else {
        return Ok(false);
    };
    if conflicts(conn, draft_id)?
        .iter()
        .any(|(branch_id, record)| record.draft_id == draft_id && *branch_id != remote_branch_id)
    {
        return Ok(false);
    }
    let local = crate::load_compose_draft(conn, "draft_id", &draft_id.to_string())?
        .ok_or(Error::NotFound)?;
    let remote_attachment_ids = record_attachment_ids(remote);
    if base.recipients != local.recipients
        || base.recipients != remote.recipients
        || base.route != local.route
        || base.route != remote.route
        || base.attachment_ids != local.attachment_ids
        || base.attachment_ids != remote_attachment_ids
    {
        return Ok(false);
    }
    let Some(text) = merge_text(&base.text, &local.text, &remote.text) else {
        return Ok(false);
    };
    let next = ComposeDraft {
        text,
        revision: local.revision.checked_add(1).ok_or(Error::Database)?,
        ..local
    };
    let effective_target = Some(remote_branch_id);
    let mut resolves = pending_resolves(conn, draft_id)?;
    if let Some(branch_id) = sync_row(conn, draft_id)?
        .and_then(|sync| (!sync.branch_closed).then_some(sync.branch_id).flatten())
        && effective_target != Some(branch_id)
    {
        resolves.push(branch_id);
    }
    resolves.sort_unstable();
    resolves.dedup();
    resolves.retain(|branch_id| effective_target != Some(*branch_id));
    // The protocol permits at most three branch closures on one authored revision.
    if resolves.len() > 3 {
        return Ok(false);
    }
    conn.execute(
        "UPDATE compose_drafts SET text=?,revision=? WHERE draft_id=?",
        params![
            next.text,
            revision_i64(next.revision)?,
            draft_id.to_string()
        ],
    )?;
    // The merged revision is authored on top of the incoming head, not the stale local base.
    // This matches a fast-forward before sealing the next local revision.
    conn.execute(
        "UPDATE draft_sync SET branch_id=?,branch_closed=0,accepted_revision_id=?,expires_at_ms=? WHERE draft_id=?",
        params![
            remote_branch_id.to_string(),
            remote.revision_id.to_string(),
            remote_expires_at_ms,
            draft_id.to_string()
        ],
    )?;
    store_merge_base(conn, remote)?;
    seal_pending(conn, &next, true, vault_id, epoch, key)?;
    conn.execute(
        "UPDATE draft_sync SET resolves_json=?,notice='updated-from-other-device' WHERE draft_id=?",
        params![json_string(&resolves)?, draft_id.to_string()],
    )?;
    let encoded_remote = serde_json::to_vec(remote).map_err(|_| Error::Database)?;
    conn.execute(
        "INSERT INTO draft_sync_merge_fallbacks(draft_id,branch_id,record) VALUES(?,?,?) ON CONFLICT(draft_id) DO UPDATE SET branch_id=excluded.branch_id,record=excluded.record",
        params![draft_id.to_string(), remote_branch_id.to_string(), encoded_remote],
    )?;
    Ok(true)
}

fn promote_merge_fallback(conn: &Connection, draft_id: DraftId) -> Result<(), Error> {
    let fallback: Option<(String, Vec<u8>)> = conn
        .query_row(
            "SELECT branch_id,record FROM draft_sync_merge_fallbacks WHERE draft_id=?",
            [draft_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((branch_id, record)) = fallback else {
        return Ok(());
    };
    let branch_id = parse_uuid(&branch_id)?;
    let record: DraftRecordV1 = serde_json::from_slice(&record).map_err(|_| Error::Database)?;
    store_conflict(conn, draft_id, branch_id, &record)?;
    conn.execute(
        "DELETE FROM draft_sync_merge_fallbacks WHERE draft_id=?",
        [draft_id.to_string()],
    )?;
    Ok(())
}

fn merge_text(base: &str, local: &str, remote: &str) -> Option<String> {
    let local_change = text_change(base, local);
    let remote_change = text_change(base, remote);
    if local_change.0 == local_change.1 && local_change.2.is_empty()
        || remote_change.0 == remote_change.1 && remote_change.2.is_empty()
    {
        return None;
    }
    // A full replacement has no stable insertion point to merge against. Keep it for explicit
    // conflict resolution even when both peers happened to choose the same replacement.
    if local_change.0 == 0
        && local_change.1 == base.len()
        && remote_change.0 == 0
        && remote_change.1 == base.len()
    {
        return None;
    }
    if local_change == remote_change {
        let mut merged = String::with_capacity(base.len() + local_change.2.len());
        merged.push_str(base.get(..local_change.0)?);
        merged.push_str(local_change.2);
        merged.push_str(base.get(local_change.1..)?);
        return Some(merged);
    }
    if local_change.1 >= remote_change.0 && remote_change.1 >= local_change.0 {
        return None;
    }
    let (first, second) = if local_change.0 <= remote_change.0 {
        (local_change, remote_change)
    } else {
        (remote_change, local_change)
    };
    let mut merged = String::with_capacity(base.len() + first.2.len() + second.2.len());
    merged.push_str(base.get(..first.0)?);
    merged.push_str(first.2);
    merged.push_str(base.get(first.1..second.0)?);
    merged.push_str(second.2);
    merged.push_str(base.get(second.1..)?);
    Some(merged)
}

fn text_change<'a>(base: &str, edited: &'a str) -> (usize, usize, &'a str) {
    let prefix = base
        .chars()
        .zip(edited.chars())
        .take_while(|(left, right)| left == right)
        .map(|(character, _)| character.len_utf8())
        .sum();
    let suffix = base[prefix..]
        .chars()
        .rev()
        .zip(edited[prefix..].chars().rev())
        .take_while(|(left, right)| left == right)
        .map(|(character, _)| character.len_utf8())
        .sum::<usize>();
    let base_end = base.len() - suffix;
    let edited_end = edited.len() - suffix;
    (prefix, base_end, &edited[prefix..edited_end])
}

fn clear_merge_state(conn: &Connection, draft_id: DraftId) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM draft_sync_merge_bases WHERE draft_id=?",
        [draft_id.to_string()],
    )?;
    conn.execute(
        "DELETE FROM draft_sync_merge_fallbacks WHERE draft_id=?",
        [draft_id.to_string()],
    )?;
    Ok(())
}

fn parse_uuid(value: &str) -> Result<Uuid, Error> {
    Uuid::parse_str(value).map_err(|_| Error::Database)
}

fn parse_optional_uuid(value: Option<String>) -> Result<Option<Uuid>, Error> {
    value.as_deref().map(parse_uuid).transpose()
}

fn canonical_cursor(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
        && value.parse::<u64>().is_ok()
}

fn quarantine_draft_head(
    conn: &Connection,
    draft_id: DraftId,
    revision_id: Uuid,
) -> Result<(), Error> {
    conn.execute(
        "INSERT OR IGNORE INTO draft_sync_quarantines(draft_id,revision_id,reason) VALUES(?,?,'invalid_record')",
        params![draft_id.to_string(), revision_id.to_string()],
    )?;
    Ok(())
}

fn remove_terminal_draft(conn: &Connection, draft_id: DraftId) -> Result<(), Error> {
    clear_merge_state(conn, draft_id)?;
    let branches: Vec<String> = conn
        .prepare(
            "SELECT branch_id FROM draft_sync_conflicts WHERE draft_id=? OR remote_draft_id=?",
        )?
        .query_map(params![draft_id.to_string(), draft_id.to_string()], |row| {
            row.get(0)
        })?
        .collect::<Result<_, _>>()?;
    for branch_id in branches {
        remove_media_references(conn, "conflict", &branch_id)?;
    }
    let intent_commands: Vec<String> = conn
        .prepare("SELECT command_id FROM draft_send_intents WHERE draft_id=?")?
        .query_map([draft_id.to_string()], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    for command_id in intent_commands {
        remove_media_references(conn, "intent", &command_id)?;
    }
    conn.execute(
        "DELETE FROM draft_sync_conflicts WHERE draft_id=? OR remote_draft_id=?",
        params![draft_id.to_string(), draft_id.to_string()],
    )?;
    conn.execute(
        "DELETE FROM draft_send_intents WHERE draft_id=?",
        [draft_id.to_string()],
    )?;
    conn.execute(
        "DELETE FROM compose_drafts WHERE draft_id=? AND NOT EXISTS(SELECT 1 FROM draft_sync WHERE draft_id=? AND state='retired')",
        params![draft_id.to_string(), draft_id.to_string()],
    )?;
    conn.execute(
        "DELETE FROM draft_sync WHERE draft_id=? AND state!='retired'",
        [draft_id.to_string()],
    )?;
    // A retained sync row may still need a late acceptance's local-revision mapping. Once a
    // terminal fully removes that row, no acknowledgement can safely target this draft again.
    conn.execute(
        "DELETE FROM draft_sync_revisions WHERE draft_id=? AND NOT EXISTS(SELECT 1 FROM draft_sync WHERE draft_id=?)",
        params![draft_id.to_string(), draft_id.to_string()],
    )?;
    conn.execute(
        "DELETE FROM draft_push_attempts WHERE draft_id=? AND NOT EXISTS(SELECT 1 FROM draft_sync WHERE draft_id=?)",
        params![draft_id.to_string(), draft_id.to_string()],
    )?;
    Ok(())
}

fn add_media_references(
    conn: &Connection,
    kind: &str,
    owner_id: &str,
    attachment_ids: &[AttachmentId],
) -> Result<(), Error> {
    for attachment_id in attachment_ids {
        conn.execute(
            "INSERT OR IGNORE INTO draft_media_references(kind,owner_id,attachment_id) VALUES(?,?,?)",
            params![kind, owner_id, attachment_id.to_string()],
        )?;
    }
    Ok(())
}

fn remove_media_references(conn: &Connection, kind: &str, owner_id: &str) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM draft_media_references WHERE kind=? AND owner_id=?",
        params![kind, owner_id],
    )?;
    Ok(())
}

/// Terminal revisions must never silently erase authored content.  A sent remote terminal can
/// keep the same attachment references (the server still owns them); expiry uses the same local
/// preservation path until the host can reprepare media into a fresh incarnation.
fn keep_local_after_terminal(
    conn: &Connection,
    draft_id: DraftId,
    notice: &str,
) -> Result<(), Error> {
    let Some(draft) = crate::load_compose_draft(conn, "draft_id", &draft_id.to_string())? else {
        return Ok(());
    };
    if !is_dirty(conn, draft_id)? {
        return remove_terminal_draft(conn, draft_id);
    }
    let new_draft_id: Option<String> = conn
        .query_row(
            "SELECT new_draft_id FROM draft_sync_resurrections WHERE old_draft_id=?",
            [draft_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if new_draft_id.is_some() {
        return Ok(());
    }
    let replacement = DraftId::new();
    // The compose table permits one draft per conversation. Remove the terminal row first,
    // but keep its loaded content and resurrection mapping in this transaction.
    remove_terminal_draft(conn, draft_id)?;
    conn.execute(
        "INSERT INTO compose_drafts(draft_id,conversation_id,text,recipients,attachment_ids,route,revision,draft_only) VALUES(?,?,?,?,?,?,0,0)",
        params![replacement.to_string(), draft.conversation_id.to_string(), draft.text, json_string(&draft.recipients)?, json_string(&draft.attachment_ids)?, route_json(&draft.route)?],
    )?;
    conn.execute(
        "INSERT INTO draft_sync(draft_id,state,notice) VALUES(?,'local_only',?)",
        params![replacement.to_string(), notice],
    )?;
    conn.execute(
        "INSERT INTO draft_sync_resurrections(old_draft_id,new_draft_id) VALUES(?,?)",
        params![draft_id.to_string(), replacement.to_string()],
    )?;
    Ok(())
}

enum ResurrectionAttachment {
    Prepared {
        old: AttachmentId,
        new: AttachmentId,
    },
    Unavailable {
        old: AttachmentId,
        info: crate::AttachmentInfo,
    },
}

const MAX_RESURRECTION_RETRIES: usize = 3;

enum ResurrectionCommit {
    Restored,
    Skipped,
    Changed,
}

impl Client {
    #[doc(hidden)]
    #[cfg(debug_assertions)]
    pub fn set_resurrection_test_hook(&self, hook: Option<ResurrectionTestHook>) {
        *RESURRECTION_TEST_HOOK
            .lock()
            .expect("resurrection test hook") = hook;
    }

    fn resurrect_expired_draft(&self, draft_id: DraftId) -> Result<(), Error> {
        for _ in 0..MAX_RESURRECTION_RETRIES {
            let draft = {
                let store = self.lock()?;
                let resurrected: bool = store.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM draft_sync_resurrections WHERE old_draft_id=?)",
                    [draft_id.to_string()],
                    |row| row.get(0),
                )?;
                if resurrected || !is_dirty(&store.conn, draft_id)? {
                    return Ok(());
                }
                crate::load_compose_draft(&store.conn, "draft_id", &draft_id.to_string())?
            };
            let Some(draft) = draft else {
                return Ok(());
            };
            let mut replacements = Vec::with_capacity(draft.attachment_ids.len());
            for old in &draft.attachment_ids {
                let info = self.attachment_info(*old)?;
                match self.open_native_plaintext(*old) {
                    Ok(plaintext) => match self.prepare_attachment(
                        plaintext.path(),
                        &info.media_type,
                        &info.display_name,
                    ) {
                        Ok(new) => replacements.push(ResurrectionAttachment::Prepared {
                            old: *old,
                            new: new.attachment_id,
                        }),
                        Err(error) => {
                            cleanup_replacements(self, &replacements);
                            return Err(error);
                        }
                    },
                    Err(Error::InvalidMedia) => {
                        replacements.push(ResurrectionAttachment::Unavailable { old: *old, info })
                    }
                    Err(error) => {
                        cleanup_replacements(self, &replacements);
                        return Err(error);
                    }
                }
            }
            #[cfg(debug_assertions)]
            if let Some(hook) = RESURRECTION_TEST_HOOK
                .lock()
                .expect("resurrection test hook")
                .clone()
            {
                hook(draft_id);
            }
            let result = (|| {
                let mut store = self.lock()?;
                let vault_id = store.config.vault_id;
                let (epoch, key) = store
                    .active_epoch
                    .and_then(|epoch| {
                        store
                            .keys
                            .get(&epoch)
                            .and_then(|keys| keys.draft.clone())
                            .map(|key| (epoch, key))
                    })
                    .ok_or(Error::KeysUnavailable)?;
                let tx = store
                    .conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                let resurrected: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM draft_sync_resurrections WHERE old_draft_id=?)",
                    [draft_id.to_string()],
                    |row| row.get(0),
                )?;
                if resurrected || !is_dirty(&tx, draft_id)? {
                    return Ok(ResurrectionCommit::Skipped);
                }
                let Some(current) =
                    crate::load_compose_draft(&tx, "draft_id", &draft_id.to_string())?
                else {
                    return Ok(ResurrectionCommit::Skipped);
                };
                if current.revision != draft.revision
                    || current.attachment_ids != draft.attachment_ids
                {
                    return Ok(ResurrectionCommit::Changed);
                }
                let replacement_id = DraftId::new();
                let mut attachment_ids = Vec::with_capacity(replacements.len());
                for replacement in &replacements {
                    let (old, new, available) = match replacement {
                        ResurrectionAttachment::Prepared { old, new } => (*old, *new, true),
                        ResurrectionAttachment::Unavailable { old, info } => {
                            let new = AttachmentId::new();
                            let key = FileKey::generate().map_err(|_| Error::Crypto)?;
                            let key_bytes = key.with_encrypted_reference_bytes(|bytes| *bytes);
                            tx.execute("INSERT INTO attachments(attachment_id,state,media_type,display_name,plaintext_bytes,ciphertext_bytes,ciphertext_sha256,stream_version,file_key,remote_object_id) VALUES(?,?,?,?,?,?,?,?,?,NULL)", params![new.to_string(), AttachmentState::PendingUpload.code(), info.media_type, info.display_name, i64::try_from(info.plaintext_bytes).map_err(|_| Error::Database)?, i64::try_from(info.ciphertext_bytes).map_err(|_| Error::Database)?, info.ciphertext_sha256, u32::from(crate::media::STREAM_VERSION), key_bytes.as_slice()])?;
                            attachment_retention::record_upload_retention(
                                &tx,
                                new,
                                crate::AttachmentRetentionClass::MessageMedia,
                                true,
                            )?;
                            (*old, new, false)
                        }
                    };
                    attachment_ids.push(new);
                    tx.execute("INSERT INTO draft_sync_resurrection_attachments(old_draft_id,old_attachment_id,new_attachment_id,available) VALUES(?,?,?,?)", params![draft_id.to_string(), old.to_string(), new.to_string(), i64::from(available)])?;
                }
                let replacement = ComposeDraft {
                    draft_id: replacement_id,
                    conversation_id: current.conversation_id,
                    text: current.text,
                    recipients: current.recipients,
                    attachment_ids,
                    route: current.route,
                    revision: 0,
                    group_name: current.group_name,
                };
                remove_terminal_draft(&tx, draft_id)?;
                tx.execute("INSERT INTO compose_drafts(draft_id,conversation_id,text,recipients,attachment_ids,route,revision,group_name,draft_only) VALUES(?,?,?,?,?,?,0,?,0)", params![replacement.draft_id.to_string(), replacement.conversation_id.to_string(), replacement.text, json_string(&replacement.recipients)?, json_string(&replacement.attachment_ids)?, route_json(&replacement.route)?, replacement.group_name])?;
                seal_pending(&tx, &replacement, true, vault_id, epoch, &key)?;
                tx.execute(
                    "UPDATE draft_sync SET notice='restored-after-expiry' WHERE draft_id=?",
                    [replacement.draft_id.to_string()],
                )?;
                tx.execute(
                    "INSERT INTO draft_sync_resurrections(old_draft_id,new_draft_id) VALUES(?,?)",
                    params![draft_id.to_string(), replacement.draft_id.to_string()],
                )?;
                tx.commit()?;
                Ok(ResurrectionCommit::Restored)
            })();
            match result {
                Ok(ResurrectionCommit::Restored) => {
                    for old in draft.attachment_ids {
                        let _ = self.discard_unreferenced_attachment(old);
                    }
                    return Ok(());
                }
                Ok(ResurrectionCommit::Skipped) => {
                    cleanup_replacements(self, &replacements);
                    return Ok(());
                }
                Ok(ResurrectionCommit::Changed) => cleanup_replacements(self, &replacements),
                Err(error) => {
                    cleanup_replacements(self, &replacements);
                    return Err(error);
                }
            }
        }
        Err(Error::Conflict)
    }
}

fn cleanup_replacements(client: &Client, replacements: &[ResurrectionAttachment]) {
    for replacement in replacements {
        if let ResurrectionAttachment::Prepared { new, .. } = replacement {
            let _ = client.discard_unreferenced_attachment(*new);
        }
    }
}

fn json_string<T: Serialize + ?Sized>(value: &T) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|_| Error::Database)
}

fn route_json(route: &Option<GatewayRoute>) -> Result<Option<String>, Error> {
    route.as_ref().map(json_string).transpose()
}

fn revision_i64(revision: u64) -> Result<i64, Error> {
    i64::try_from(revision).map_err(|_| Error::Database)
}

fn terminal_code(reason: DraftTerminalReasonCore) -> &'static str {
    match reason {
        DraftTerminalReasonCore::Sent => "sent",
        DraftTerminalReasonCore::Discarded => "discarded",
        DraftTerminalReasonCore::Expired => "expired",
    }
}
fn transfer_code(direction: DraftTransferDirection) -> &'static str {
    match direction {
        DraftTransferDirection::Upload => "upload",
        DraftTransferDirection::Download => "download",
    }
}
fn state_from_code(code: &str) -> DraftSyncState {
    match code {
        "saved_local" => DraftSyncState::SavedLocal,
        "syncing" => DraftSyncState::Syncing,
        "synced" => DraftSyncState::Synced,
        "paused" => DraftSyncState::Paused,
        "failed" => DraftSyncState::Failed,
        _ => DraftSyncState::LocalOnly,
    }
}
fn notice_from_code(code: &str) -> Option<DraftNotice> {
    match code {
        "restored-after-expiry" => Some(DraftNotice::RestoredAfterExpiry),
        "kept-after-sent" => Some(DraftNotice::KeptAfterSent),
        "updated-from-other-device" => Some(DraftNotice::UpdatedFromOtherDevice),
        _ => None,
    }
}

pub(crate) fn record_local_save(
    conn: &Connection,
    previous: &ComposeDraft,
    mut next: ComposeDraft,
    vault_id: VaultId,
    epoch: u32,
    key: &PurposeKey,
) -> Result<ComposeDraft, Error> {
    let authored = previous.text != next.text
        || previous.recipients != next.recipients
        || previous.attachment_ids != next.attachment_ids;
    if is_retired_draft(conn, next.draft_id)? {
        if !authored {
            return Ok(next);
        }
        let retired_id = next.draft_id;
        next.draft_id = DraftId::new();
        conn.execute(
            "UPDATE compose_drafts SET draft_id=? WHERE draft_id=?",
            params![next.draft_id.to_string(), retired_id.to_string()],
        )?;
        conn.execute(
            "UPDATE compose_draft_redirects SET target_draft_id=? WHERE target_draft_id=?",
            params![next.draft_id.to_string(), retired_id.to_string()],
        )?;
    }
    // Compose validation deliberately permits a host to save before media preparation finishes.
    // A changed draft must still invalidate an older pending snapshot, even though it cannot yet
    // be sealed for upload without every descriptor.
    if next
        .attachment_ids
        .iter()
        .map(|id| attachment_row(conn, *id))
        .collect::<Result<Vec<_>, Error>>()?
        .iter()
        .any(Option::is_none)
    {
        conn.execute(
            "INSERT INTO draft_sync(draft_id,authored_dirty,state) VALUES(?,?,?) ON CONFLICT(draft_id) DO UPDATE SET pending_revision_id=NULL,pending_sealed_record=NULL,pending_local_revision=NULL,resolves_json=NULL,authored_dirty=CASE WHEN excluded.authored_dirty=1 THEN 1 ELSE draft_sync.authored_dirty END,state=CASE WHEN excluded.authored_dirty=1 THEN 'saved_local' ELSE draft_sync.state END",
            params![next.draft_id.to_string(), i64::from(authored), "saved_local"],
        )?;
        return Ok(next);
    }
    seal_pending(conn, &next, authored, vault_id, epoch, key)?;
    Ok(next)
}

/// Seals `draft` as the pending revision. `authored` marks the draft dirty until a push with
/// this content is accepted; a non-authored save only changes route or recipients metadata.
fn seal_pending(
    conn: &Connection,
    draft: &ComposeDraft,
    authored: bool,
    vault_id: VaultId,
    epoch: u32,
    key: &PurposeKey,
) -> Result<(), Error> {
    let supported: i64 = conn
        .query_row(
            "SELECT supported FROM draft_sync_meta WHERE id=1",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(1);
    let revision_id = Uuid::new_v4();
    let attachments = draft
        .attachment_ids
        .iter()
        .map(|id| {
            let row = attachment_row(conn, *id)?.ok_or(Error::NotFound)?;
            Ok(DraftRecordAttachmentV1 {
                attachment_id: *id,
                media_type: row.media_type,
                display_name: row.display_name,
                plaintext_bytes: row.plaintext_bytes,
                ciphertext_bytes: row.ciphertext_bytes,
                ciphertext_sha256: row.ciphertext_sha256,
                stream_version: u32::from(crate::media::STREAM_VERSION),
                file_key: row.file_key.to_vec(),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let record = DraftRecordV1 {
        version: 1,
        draft_id: draft.draft_id,
        revision_id,
        text: draft.text.clone(),
        recipients: draft.recipients.clone(),
        route: draft.route.clone(),
        attachments,
        authored_at_ms: now_ms(),
        group_name: draft.group_name.clone(),
    };
    let plaintext = Zeroizing::new(serde_json::to_vec(&record).map_err(|_| Error::Database)?);
    let sealed = encrypt(
        key,
        &draft_aad(vault_id, draft.draft_id, revision_id),
        &plaintext,
    )
    .map_err(|_| Error::Crypto)?;
    let mut sealed_record = Vec::with_capacity(4 + 24 + sealed.ciphertext.len());
    sealed_record.extend_from_slice(&epoch.to_be_bytes());
    sealed_record.extend_from_slice(&crate::seal_frame(&sealed));
    conn.execute(
        "INSERT INTO draft_sync(draft_id,pending_revision_id,pending_sealed_record,authored_dirty,pending_local_revision,state) VALUES(?,?,?,?,?,?) ON CONFLICT(draft_id) DO UPDATE SET pending_revision_id=excluded.pending_revision_id,pending_sealed_record=excluded.pending_sealed_record,authored_dirty=CASE WHEN excluded.authored_dirty=1 THEN 1 ELSE draft_sync.authored_dirty END,pending_local_revision=excluded.pending_local_revision,state=excluded.state",
        params![
            draft.draft_id.to_string(),
            revision_id.to_string(),
            sealed_record,
            i64::from(authored),
            revision_i64(draft.revision)?,
            if supported != 0 { "syncing" } else { "paused" }
        ],
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO draft_sync_revisions(draft_id,revision_id,local_revision) VALUES(?,?,?)",
        params![draft.draft_id.to_string(), revision_id.to_string(), revision_i64(draft.revision)?],
    )?;
    Ok(())
}

fn draft_aad(vault_id: VaultId, draft_id: DraftId, revision_id: Uuid) -> Vec<u8> {
    let mut aad = Vec::with_capacity(16 * 3 + 16);
    aad.extend_from_slice(b"peppy.draft.v1\0");
    aad.extend_from_slice(vault_id.0.as_bytes());
    aad.extend_from_slice(draft_id.0.as_bytes());
    aad.extend_from_slice(revision_id.as_bytes());
    aad
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |value| {
            i64::try_from(value.as_millis()).unwrap_or(i64::MAX)
        })
}

fn open_record(
    store: &Store,
    draft_id: DraftId,
    revision_id: Uuid,
    sealed: &[u8],
) -> Result<DraftRecordV1, Error> {
    let (epoch, frame) = sealed.split_at_checked(4).ok_or(Error::Crypto)?;
    let epoch = u32::from_be_bytes(epoch.try_into().map_err(|_| Error::Crypto)?);
    let key = store
        .keys
        .get(&epoch)
        .and_then(|keys| keys.draft.as_ref())
        .ok_or(Error::KeysUnavailable)?;
    let envelope = crate::open_frame(frame).ok_or(Error::Crypto)?;
    let plaintext = Zeroizing::new(
        decrypt(
            key,
            &draft_aad(store.config.vault_id, draft_id, revision_id),
            &envelope,
        )
        .map_err(|_| Error::Crypto)?,
    );
    let record: DraftRecordV1 = serde_json::from_slice(&plaintext).map_err(|_| Error::Crypto)?;
    if record.version != 1 || record.draft_id != draft_id || record.revision_id != revision_id {
        return Err(Error::Crypto);
    }
    Ok(record)
}

fn install_remote_attachments(conn: &Connection, record: &DraftRecordV1) -> Result<(), Error> {
    let mut seen = std::collections::HashSet::new();
    for attachment in &record.attachments {
        if !seen.insert(attachment.attachment_id) {
            return Err(Error::Crypto);
        }
        let key: [u8; 32] = attachment
            .file_key
            .as_slice()
            .try_into()
            .map_err(|_| Error::Crypto)?;
        let existing: Option<AttachmentDescriptorRow> = conn
            .query_row(
                "SELECT media_type,display_name,plaintext_bytes,ciphertext_bytes,ciphertext_sha256,stream_version,file_key FROM attachments WHERE attachment_id=?",
                [attachment.attachment_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
            )
            .optional()?;
        let plaintext_bytes =
            i64::try_from(attachment.plaintext_bytes).map_err(|_| Error::Crypto)?;
        let ciphertext_bytes =
            i64::try_from(attachment.ciphertext_bytes).map_err(|_| Error::Crypto)?;
        let stream_version = i64::from(attachment.stream_version);
        if let Some(existing) = existing {
            if existing
                != (
                    attachment.media_type.clone(),
                    attachment.display_name.clone(),
                    plaintext_bytes,
                    ciphertext_bytes,
                    attachment.ciphertext_sha256.clone(),
                    stream_version,
                    key.to_vec(),
                )
            {
                return Err(Error::Crypto);
            }
            continue;
        }
        conn.execute(
            "INSERT INTO attachments(attachment_id,state,media_type,display_name,plaintext_bytes,ciphertext_bytes,ciphertext_sha256,stream_version,file_key,remote_object_id) VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(attachment_id) DO NOTHING",
            params![
                attachment.attachment_id.to_string(),
                AttachmentState::PendingDownload.code(),
                attachment.media_type,
                attachment.display_name,
                plaintext_bytes,
                ciphertext_bytes,
                attachment.ciphertext_sha256,
                stream_version,
                key.as_slice(),
                attachment.attachment_id.to_string()
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::SendIntentSnapshot;

    #[test]
    fn legacy_send_intent_snapshot_defaults_to_single_part_send() {
        let snapshot = serde_json::json!({
            "conversation_id": uuid::Uuid::new_v4(),
            "text": "caption",
            "recipients": ["+15555550100"],
            "attachment_ids": [],
            "route": null,
        });
        let parsed: SendIntentSnapshot = serde_json::from_value(snapshot).unwrap();
        assert!(parsed.parts.is_none());
    }
}
