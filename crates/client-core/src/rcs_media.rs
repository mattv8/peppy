//! Gateway-local RCS media send ledger, Google media references, tmp-ID dedup, and the sanitized
//! projections synced between devices. Google media IDs, keys, and references stay in this
//! SQLCipher database; payloads carry only Peppy descriptors and per-attachment states.
//!
//! Every mutator takes a `Transaction` so a multi-row transition commits or rolls back as one.
//! The ledger entry points are consumed by CORE-A2 admission and bindings, so they are unused in
//! this lane's build until that lane lands.
#![allow(dead_code)]
use super::*;
use crate::media::{MAX_ATTACHMENT_CIPHERTEXT_BYTES, MAX_ATTACHMENT_PLAINTEXT_BYTES};
use crate::rcs_native::NativeRcsMedia;
use crate::rcs_receive::RcsIncomingAttachment;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rusqlite::Transaction;
use serde::Deserialize;
use serde::Serialize;

/// Attachments per RCS command: one ledger row each. Shares the MMS ceiling.
pub(crate) const MAX_RCS_MEDIA_ATTACHMENTS: usize = MAX_MMS_ATTACHMENTS;
/// Text rides only on index 0 and keeps the existing text-send bound.
const MAX_RCS_TEXT_BYTES: usize = 4096;
pub const RCS_MEDIA_RECEIVE_LOCAL_BUDGET_BYTES: u64 = 128 * 1024 * 1024;

/// Replicated, renderer-safe identity for one incoming RCS media slot. Google references remain
/// in `rcs_media_receive_local` and are intentionally not represented here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RcsMediaSlotMetadata {
    pub media_type: String,
    pub display_name: String,
    pub plaintext_bytes: u64,
}

impl RcsMediaSlotMetadata {
    fn is_valid(&self) -> bool {
        !self.media_type.is_empty()
            && self.media_type.len() <= 255
            && !self.display_name.is_empty()
            && self.display_name.len() <= 1024
            && self.plaintext_bytes <= i64::MAX as u64
    }
}
/// Bounds one opaque Google reference stored in SQLCipher.
const MAX_MEDIA_REF_BYTES: usize = 16 * 1024;
/// Google gives no reference TTL, so older references are uploaded again before permit issuance.
const REFERENCE_MAX_AGE_MS: i64 = 60 * 60 * 1000;
/// Failed pre-permit upload attempts per attachment before the attachment is refused as `failed`.
pub(crate) const MAX_PRE_PERMIT_UPLOAD_FAILURES: i64 = 3;
const TERMINAL_SQL: &str = "'accepted','outcome_unknown','refused'";
const ROW_COLUMNS: &str = "attachment_index,attachment_id,state,uploaded_at_ms,permit_at_ms,tmp_id";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LedgerState {
    Pending,
    Uploading,
    ReferenceReady,
    PermitIssued,
    Committed,
    Accepted,
    OutcomeUnknown,
    Refused,
}

impl LedgerState {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Uploading => "uploading",
            Self::ReferenceReady => "reference_ready",
            Self::PermitIssued => "permit_issued",
            Self::Committed => "committed",
            Self::Accepted => "accepted",
            Self::OutcomeUnknown => "outcome_unknown",
            Self::Refused => "refused",
        }
    }

    fn from_code(value: &str) -> Result<Self, Error> {
        Ok(match value {
            "pending" => Self::Pending,
            "uploading" => Self::Uploading,
            "reference_ready" => Self::ReferenceReady,
            "permit_issued" => Self::PermitIssued,
            "committed" => Self::Committed,
            "accepted" => Self::Accepted,
            "outcome_unknown" => Self::OutcomeUnknown,
            "refused" => Self::Refused,
            _ => return Err(Error::Database),
        })
    }

    const fn is_terminal(self) -> bool {
        matches!(self, Self::Accepted | Self::OutcomeUnknown | Self::Refused)
    }
}

/// One materialized `(command_id, attachment_index)` ledger row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LedgerRow {
    pub(crate) attachment_index: u32,
    pub(crate) attachment_id: AttachmentId,
    pub(crate) state: LedgerState,
    pub(crate) uploaded_at_ms: Option<i64>,
    pub(crate) permit_at_ms: Option<i64>,
    pub(crate) tmp_id: Option<String>,
}

/// One attachment of a command before its ledger rows exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannedAttachment {
    pub(crate) attachment_index: u32,
    pub(crate) attachment_id: AttachmentId,
    /// The command's text on index 0 only (addendum A1); empty for every later index.
    pub(crate) content: String,
    pub(crate) tmp_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PermitDecision {
    Issued,
    /// The reference was missing, unreadable, or at least an hour old. The row is pending again.
    ReuploadRequired,
}

/// What a recorded outcome means for the attachment and its command. Refused remaining indexes
/// all share the `members_changed` reason, because that is the only code that refuses them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OutcomeReport {
    pub(crate) sync: RcsMediaSyncState,
    pub(crate) reason: Option<RcsMediaNotSentReason>,
    pub(crate) refused_indexes: Vec<u32>,
}

/// Plans one ledger row per attachment in index order. Text rides on index 0 only.
pub(crate) fn plan_expansion(
    text: &str,
    attachments: &[AttachmentId],
) -> Result<Vec<PlannedAttachment>, Error> {
    if attachments.is_empty() || attachments.len() > MAX_RCS_MEDIA_ATTACHMENTS {
        return Err(Error::InvalidRequest("RCS media attachment count"));
    }
    if text.len() > MAX_RCS_TEXT_BYTES {
        return Err(Error::InvalidRequest("RCS text"));
    }
    let mut plan = Vec::with_capacity(attachments.len());
    for (position, attachment_id) in attachments.iter().enumerate() {
        plan.push(PlannedAttachment {
            attachment_index: u32::try_from(position).map_err(|_| Error::Database)?,
            attachment_id: *attachment_id,
            content: if position == 0 {
                text.to_owned()
            } else {
                String::new()
            },
            tmp_id: uuid::Uuid::new_v4().to_string(),
        });
    }
    Ok(plan)
}

/// Inserts the command's pending ledger rows and their tmp-ID dedup rows. Returns `Ok(false)`
/// when the same plan already exists, so a replayed admission is a no-op. A different attachment
/// list for an existing command is refused.
pub(crate) fn materialize(
    tx: &Transaction<'_>,
    command_id: CommandId,
    plan: &[PlannedAttachment],
) -> Result<bool, Error> {
    let existing = ledger_rows(tx, command_id)?;
    if !existing.is_empty() {
        let same = existing.len() == plan.len()
            && existing.iter().zip(plan).all(|(row, planned)| {
                row.attachment_index == planned.attachment_index
                    && row.attachment_id == planned.attachment_id
            });
        return if same {
            Ok(false)
        } else {
            Err(Error::InvalidRequest("RCS media plan changed"))
        };
    }
    for planned in plan {
        tx.execute(
            "INSERT INTO rcs_media_send_ledger(command_id,attachment_index,attachment_id,state,tmp_id) VALUES(?,?,?,'pending',?)",
            params![
                command_id.to_string(),
                i64::from(planned.attachment_index),
                planned.attachment_id.to_string(),
                planned.tmp_id
            ],
        )?;
        tx.execute(
            "INSERT INTO rcs_media_tmp_dedup(google_tmp_id,command_id,attachment_index) VALUES(?,?,?)",
            params![
                planned.tmp_id,
                command_id.to_string(),
                i64::from(planned.attachment_index)
            ],
        )?;
    }
    Ok(true)
}

/// Maps a Google tmp ID back to its command and attachment index, for echo and recovery dedup.
pub(crate) fn attachment_for_tmp_id(
    conn: &Connection,
    tmp_id: &str,
) -> Result<Option<(CommandId, u32)>, Error> {
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT command_id,attachment_index FROM rcs_media_tmp_dedup WHERE google_tmp_id=?",
            params![tmp_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    row.map(|(command, index)| -> Result<(CommandId, u32), Error> {
        Ok((
            parse(&command)?,
            u32::try_from(index).map_err(|_| Error::Database)?,
        ))
    })
    .transpose()
}

pub(crate) fn bind_echo_target(
    conn: &Connection,
    command: CommandId,
    index: u32,
    google_conversation: &str,
) -> Result<(), Error> {
    let key = format!("rcs-media-target:{command}:{index}");
    if get_meta(conn, &key)?.is_some_and(|stored| stored != google_conversation) {
        return Err(Error::Conflict);
    }
    set_meta(conn, &key, google_conversation)
}

pub(crate) fn echo_slot(
    conn: &Connection,
    device: DeviceId,
    conversation: ConversationId,
    google_conversation: &str,
    tmp_id: &str,
) -> Result<Option<(CommandId, u32, MessageId)>, Error> {
    let Some((command, index)) = attachment_for_tmp_id(conn, tmp_id)? else {
        return Ok(None);
    };
    if !ledger_rows(conn, command)?.iter().any(|row| {
        row.attachment_index == index
            && matches!(
                row.state,
                LedgerState::Committed | LedgerState::Accepted | LedgerState::OutcomeUnknown
            )
    }) {
        return Ok(None);
    }
    if get_meta(conn, &format!("rcs-media-target:{command}:{index}"))?.as_deref()
        != Some(google_conversation)
    {
        return Ok(None);
    }
    let id: Option<String> = conn
        .query_row(
            "SELECT message_id FROM commands WHERE command_id=? AND gateway_device_id=?",
            params![command.to_string(), device.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(id) = id else {
        return Ok(None);
    };
    let id = parse(&id)?;
    let Some(message) = load_message(conn, id)? else {
        return Ok(None);
    };
    Ok(
        (message.direction == Direction::Outgoing
            && message.record.conversation_id == conversation)
            .then_some((command, index, id)),
    )
}

pub(crate) fn echo_identity_key(
    google_conversation: &str,
    google_message: &str,
) -> Result<String, Error> {
    Ok(format!(
        "rcs-media-echo:{}",
        json(&(google_conversation, google_message))?
    ))
}

/// A delivery/read notification belongs to one slot, not to the entire album.
pub(crate) fn apply_echo_receipt(
    conn: &Connection,
    ctx: &Ctx<'_>,
    command: CommandId,
    index: u32,
    message: MessageId,
    status: RcsProjectedStatus,
) -> Result<Option<RcsMessageStatus>, Error> {
    let rank = match status {
        RcsProjectedStatus::Sending | RcsProjectedStatus::Failed => return Ok(None),
        RcsProjectedStatus::Sent => 1,
        RcsProjectedStatus::Delivered => 2,
        RcsProjectedStatus::Read => 3,
    };
    let key = format!("rcs-media-receipt:{command}:{index}");
    let previous = get_meta(conn, &key)?
        .map(|value| value.parse::<u8>().map_err(|_| Error::Database))
        .transpose()?
        .unwrap_or(0);
    set_meta(conn, &key, &previous.max(rank).to_string())?;
    let changed = conn.execute("UPDATE rcs_media_send_ledger SET state='accepted' WHERE command_id=? AND attachment_index=? AND state IN ('committed','outcome_unknown')",
        params![command.to_string(), index])?;
    if changed != 0 {
        conn.execute("UPDATE rcs_media_send_status SET state='sent',reason=NULL WHERE message_id=? AND attachment_index=? AND state IN ('sending','outcome_unknown')", params![message.to_string(), index])?;
        enqueue_state_checkpoint(conn, ctx, message, index)?;
    }
    let mut minimum = 3;
    for row in ledger_rows(conn, command)? {
        let current = get_meta(
            conn,
            &format!("rcs-media-receipt:{command}:{}", row.attachment_index),
        )?
        .map(|value| value.parse::<u8>().map_err(|_| Error::Database))
        .transpose()?
        .unwrap_or(0);
        minimum = minimum.min(current);
    }
    Ok(match minimum {
        1 => Some(RcsMessageStatus::Sent),
        2 => Some(RcsMessageStatus::Delivered),
        3 => Some(RcsMessageStatus::Read),
        _ => None,
    })
}

pub(crate) fn ledger_rows(
    conn: &Connection,
    command_id: CommandId,
) -> Result<Vec<LedgerRow>, Error> {
    let mut query = conn.prepare(&format!(
        "SELECT {ROW_COLUMNS} FROM rcs_media_send_ledger WHERE command_id=? ORDER BY attachment_index"
    ))?;
    let raw = query
        .query_map(params![command_id.to_string()], raw_row)?
        .collect::<Result<Vec<_>, _>>()?;
    raw.into_iter().map(ledger_row).collect()
}

/// The lowest-index attachment that is not yet terminal. Sends run strictly in this order.
pub(crate) fn next_attachment(
    conn: &Connection,
    command_id: CommandId,
) -> Result<Option<LedgerRow>, Error> {
    conn.query_row(
        &format!(
            "SELECT {ROW_COLUMNS} FROM rcs_media_send_ledger WHERE command_id=? AND state NOT IN ({TERMINAL_SQL}) ORDER BY attachment_index LIMIT 1"
        ),
        params![command_id.to_string()],
        raw_row,
    )
    .optional()?
    .map(ledger_row)
    .transpose()
}

/// Returns whether the host must upload. An expired or missing local Google reference is reset
/// before it is exposed, so the coordinator cannot accidentally reuse it.
pub(crate) fn upload_required(
    tx: &Transaction<'_>,
    row: &LedgerRow,
    command_id: CommandId,
    now_ms: i64,
) -> Result<bool, Error> {
    if row.state != LedgerState::ReferenceReady {
        return Ok(true);
    }
    let fresh = row.uploaded_at_ms.is_some_and(|uploaded_at_ms| {
        uploaded_at_ms <= now_ms && now_ms - uploaded_at_ms < REFERENCE_MAX_AGE_MS
    }) && media_ref(tx, command_id, row.attachment_index)?.is_some();
    if fresh {
        return Ok(false);
    }
    set_state(
        tx,
        command_id,
        row.attachment_index,
        LedgerState::ReferenceReady,
        LedgerState::Pending,
    )?;
    tx.execute(
        "UPDATE rcs_media_send_ledger SET uploaded_at_ms=NULL WHERE command_id=? AND attachment_index=?",
        params![command_id.to_string(), i64::from(row.attachment_index)],
    )?;
    tx.execute(
        "DELETE FROM rcs_google_media_refs WHERE command_id=? AND attachment_index=?",
        params![command_id.to_string(), i64::from(row.attachment_index)],
    )?;
    Ok(true)
}

/// The Google media reference for this attachment, when one is held for the current upload.
pub(crate) fn media_ref(
    conn: &Connection,
    command_id: CommandId,
    attachment_index: u32,
) -> Result<Option<String>, Error> {
    let bytes: Option<Vec<u8>> = conn
        .query_row(
            "SELECT media_ref FROM rcs_google_media_refs WHERE command_id=? AND attachment_index=?",
            params![command_id.to_string(), i64::from(attachment_index)],
            |row| row.get(0),
        )
        .optional()?;
    bytes
        .map(|bytes| String::from_utf8(bytes).map_err(|_| Error::Database))
        .transpose()
}

/// Decodes the exact Go media wire without issuing a permit. Preparation is advisory, so the row
/// must remain `reference_ready` until native begin has revalidated all route authority.
pub(crate) fn prepared_media(
    conn: &Connection,
    command_id: CommandId,
    attachment_index: u32,
) -> Result<NativeRcsMedia, Error> {
    let row = require_next(conn, command_id, attachment_index)?;
    expect_state(&row, LedgerState::ReferenceReady)?;
    decode_media_ref(conn, command_id, attachment_index)
}

/// Rechecks that the immutable command attachment still owns the prepared ledger row.
pub(crate) fn require_prepared_attachment(
    conn: &Connection,
    command_id: CommandId,
    attachment_index: u32,
    expected_attachment_id: AttachmentId,
) -> Result<(), Error> {
    let row = require_next(conn, command_id, attachment_index)?;
    if row.attachment_id != expected_attachment_id {
        return Err(Error::InvalidRequest("RCS media attachment changed"));
    }
    expect_state(&row, LedgerState::ReferenceReady)
}

/// Atomically consumes the prepared reference after native authority revalidation. The returned
/// tmp ID belongs to this attachment row, never to the command-wide text ledger.
pub(crate) fn issue_prepared_media(
    tx: &Transaction<'_>,
    command_id: CommandId,
    attachment_index: u32,
    expected: &NativeRcsMedia,
    now_ms: i64,
) -> Result<(NativeRcsMedia, String), Error> {
    let row = require_next(tx, command_id, attachment_index)?;
    expect_state(&row, LedgerState::ReferenceReady)?;
    let media = decode_media_ref(tx, command_id, attachment_index)?;
    if &media != expected {
        return Err(Error::InvalidRequest("RCS media reference changed"));
    }
    if issue_permit(tx, command_id, attachment_index, now_ms)? != PermitDecision::Issued {
        return Err(Error::InvalidRequest("RCS media reupload required"));
    }
    Ok((
        media,
        row.tmp_id
            .ok_or(Error::InvalidRequest("RCS media tmp id"))?,
    ))
}

fn decode_media_ref(
    conn: &Connection,
    command_id: CommandId,
    attachment_index: u32,
) -> Result<NativeRcsMedia, Error> {
    let reference = media_ref(conn, command_id, attachment_index)?
        .ok_or(Error::InvalidRequest("RCS media reference"))?;
    let reference: GoMediaReference = serde_json::from_str(&reference)
        .map_err(|_| Error::InvalidRequest("RCS media reference"))?;
    let key = STANDARD
        .decode(reference.key)
        .map_err(|_| Error::InvalidRequest("RCS media reference"))?;
    if key.len() != 32 {
        return Err(Error::InvalidRequest("RCS media reference"));
    }
    let media = NativeRcsMedia {
        media_id: reference.media_id,
        key,
        mime: reference.mime,
        size: reference.size,
        name: reference.name,
    };
    if media.media_id.is_empty()
        || media.key.len() != 32
        || !media::valid_media_type(&media.mime)
        || media.name.is_empty()
        || media.size == 0
        || media.size > MAX_ATTACHMENT_PLAINTEXT_BYTES
    {
        return Err(Error::InvalidRequest("RCS media reference"));
    }
    Ok(media)
}

#[cfg(test)]
pub(crate) fn issue_media(
    tx: &Transaction<'_>,
    command_id: CommandId,
    attachment_index: u32,
    now_ms: i64,
) -> Result<NativeRcsMedia, Error> {
    let prepared = prepared_media(tx, command_id, attachment_index)?;
    issue_prepared_media(tx, command_id, attachment_index, &prepared, now_ms)
        .map(|(media, _)| media)
}

/// Exact gateway-local JSON emitted by Go's `json.Marshal(rcsMediaReference)`. Go serializes a
/// `[]byte` key as canonical padded base64; this DTO intentionally rejects numeric arrays and
/// unknown fields so only that private hand-off format is accepted.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GoMediaReference {
    media_id: String,
    key: String,
    mime: String,
    size: u64,
    name: String,
}

/// Returns the caption assigned to an attachment. Only index zero carries text.
pub(crate) fn planned_content(
    conn: &Connection,
    command_id: CommandId,
    attachment_index: u32,
) -> Result<String, Error> {
    let payload: String = conn.query_row(
        "SELECT payload FROM command_ledger WHERE command_id=?",
        params![command_id.to_string()],
        |row| row.get(0),
    )?;
    let (message, _) = localize(conn, decode(payload.as_bytes())?)?;
    if attachment_index == 0 {
        Ok(message.body)
    } else {
        Ok(String::new())
    }
}

pub(crate) fn mark_uploading(
    tx: &Transaction<'_>,
    command_id: CommandId,
    attachment_index: u32,
) -> Result<(), Error> {
    let row = require_next(tx, command_id, attachment_index)?;
    expect_state(&row, LedgerState::Pending)?;
    set_state(
        tx,
        command_id,
        attachment_index,
        LedgerState::Pending,
        LedgerState::Uploading,
    )
}

/// Stores the Google reference after a completed upload. The reference lives only in this row.
pub(crate) fn mark_reference_ready(
    tx: &Transaction<'_>,
    command_id: CommandId,
    attachment_index: u32,
    media_ref: &str,
    uploaded_at_ms: i64,
) -> Result<(), Error> {
    if media_ref.is_empty() || media_ref.len() > MAX_MEDIA_REF_BYTES || uploaded_at_ms < 0 {
        return Err(Error::InvalidRequest("RCS media reference"));
    }
    let row = require_next(tx, command_id, attachment_index)?;
    expect_state(&row, LedgerState::Uploading)?;
    set_state(
        tx,
        command_id,
        attachment_index,
        LedgerState::Uploading,
        LedgerState::ReferenceReady,
    )?;
    tx.execute(
        "UPDATE rcs_media_send_ledger SET uploaded_at_ms=? WHERE command_id=? AND attachment_index=?",
        params![
            uploaded_at_ms,
            command_id.to_string(),
            i64::from(attachment_index)
        ],
    )?;
    tx.execute(
        "INSERT INTO rcs_google_media_refs(command_id,attachment_index,media_ref) VALUES(?,?,?) ON CONFLICT(command_id,attachment_index) DO UPDATE SET media_ref=excluded.media_ref",
        params![
            command_id.to_string(),
            i64::from(attachment_index),
            media_ref.as_bytes()
        ],
    )?;
    Ok(())
}

/// Issues the single-use permit only for a fresh reference. A stale reference returns the row to
/// `pending`, so the caller re-uploads before any permit exists.
pub(crate) fn issue_permit(
    tx: &Transaction<'_>,
    command_id: CommandId,
    attachment_index: u32,
    now_ms: i64,
) -> Result<PermitDecision, Error> {
    let row = require_next(tx, command_id, attachment_index)?;
    expect_state(&row, LedgerState::ReferenceReady)?;
    let uploaded_at_ms = row.uploaded_at_ms.ok_or(Error::Database)?;
    let age_ms = now_ms
        .checked_sub(uploaded_at_ms)
        .ok_or(Error::InvalidRequest("RCS media clock"))?;
    if !(0..REFERENCE_MAX_AGE_MS).contains(&age_ms) {
        reset_to_pending(
            tx,
            command_id,
            attachment_index,
            LedgerState::ReferenceReady,
        )?;
        return Ok(PermitDecision::ReuploadRequired);
    }
    set_state(
        tx,
        command_id,
        attachment_index,
        LedgerState::ReferenceReady,
        LedgerState::PermitIssued,
    )?;
    tx.execute(
        "UPDATE rcs_media_send_ledger SET permit_at_ms=? WHERE command_id=? AND attachment_index=?",
        params![now_ms, command_id.to_string(), i64::from(attachment_index)],
    )?;
    Ok(PermitDecision::Issued)
}

pub(crate) fn record_commit(
    tx: &Transaction<'_>,
    command_id: CommandId,
    attachment_index: u32,
) -> Result<(), Error> {
    let row = require_next(tx, command_id, attachment_index)?;
    expect_state(&row, LedgerState::PermitIssued)?;
    set_state(
        tx,
        command_id,
        attachment_index,
        LedgerState::PermitIssued,
        LedgerState::Committed,
    )
}

/// Records the result of the current attachment's send attempt.
///
/// `ok` is success only after commit. Any other committed result is `outcome_unknown`. A permit
/// without commit refuses on the explicit refusal codes and is `outcome_unknown` otherwise. Upload-
/// stage transient codes return the row to `pending`. `members_changed` refuses every later
/// attachment of the command as well.
pub(crate) fn record_outcome(
    tx: &Transaction<'_>,
    command_id: CommandId,
    attachment_index: u32,
    code: &str,
) -> Result<OutcomeReport, Error> {
    // The outer media coordinator also reports definite early refusals/unknown results.
    // If the admitted native handle already settled the same result, do not touch the next slot.
    if let Some(row) = ledger_rows(tx, command_id)?
        .into_iter()
        .find(|row| row.attachment_index == attachment_index)
    {
        let sync = match (row.state, code) {
            (LedgerState::OutcomeUnknown, "outcome_unknown" | "members_changed") => {
                Some(RcsMediaSyncState::OutcomeUnknown)
            }
            (LedgerState::Refused, "members_changed") => Some(RcsMediaSyncState::MembersChanged),
            _ => None,
        };
        if let Some(sync) = sync {
            return Ok(OutcomeReport {
                sync,
                reason: None,
                refused_indexes: vec![],
            });
        }
    }
    let row = require_next(tx, command_id, attachment_index)?;
    let (mut next, refuses_rest) = decide_outcome(row.state, code)?;
    let retry_capped = next == LedgerState::Pending
        && count_upload_failure(tx, command_id, attachment_index)?
            >= MAX_PRE_PERMIT_UPLOAD_FAILURES;
    if retry_capped {
        next = LedgerState::Refused;
    }
    if next == LedgerState::Pending {
        reset_to_pending(tx, command_id, attachment_index, row.state)?;
    } else {
        set_state(tx, command_id, attachment_index, row.state, next)?;
        if next.is_terminal() {
            delete_ref(tx, command_id, attachment_index)?;
        }
    }
    let mut refused_indexes = Vec::new();
    if refuses_rest {
        refused_indexes = remaining_open_indexes(tx, command_id, attachment_index)?;
        tx.execute(
            &format!(
                "UPDATE rcs_media_send_ledger SET state='refused' WHERE command_id=? AND attachment_index>? AND state NOT IN ({TERMINAL_SQL})"
            ),
            params![command_id.to_string(), i64::from(attachment_index)],
        )?;
        tx.execute(
            "DELETE FROM rcs_google_media_refs WHERE command_id=? AND attachment_index>?",
            params![command_id.to_string(), i64::from(attachment_index)],
        )?;
    }
    let members_changed = code == "members_changed";
    let sync = match next {
        LedgerState::Accepted => RcsMediaSyncState::Sent,
        LedgerState::OutcomeUnknown => RcsMediaSyncState::OutcomeUnknown,
        LedgerState::Refused if members_changed => RcsMediaSyncState::MembersChanged,
        LedgerState::Refused => RcsMediaSyncState::NotSent,
        _ => RcsMediaSyncState::Sending,
    };
    let reason = match sync {
        RcsMediaSyncState::NotSent if retry_capped => Some(RcsMediaNotSentReason::Failed),
        RcsMediaSyncState::NotSent => not_sent_reason(code),
        _ => None,
    };
    Ok(OutcomeReport {
        sync,
        reason,
        refused_indexes,
    })
}

/// Crash recovery, run once when the gateway opens its database and never during a live pass:
/// upload-stage rows retry from `pending`, a permit or commit without a recorded outcome becomes
/// `outcome_unknown`, and every held Google reference is dropped.
pub(crate) fn recover_interrupted(tx: &Transaction<'_>) -> Result<(), Error> {
    tx.execute(
        "UPDATE rcs_media_send_ledger SET state='pending', uploaded_at_ms=NULL WHERE state IN ('uploading','reference_ready')",
        [],
    )?;
    tx.execute(
        "UPDATE rcs_media_send_ledger SET state='outcome_unknown' WHERE state IN ('permit_issued','committed')",
        [],
    )?;
    tx.execute("DELETE FROM rcs_google_media_refs", [])?;
    Ok(())
}

fn decide_outcome(state: LedgerState, code: &str) -> Result<(LedgerState, bool), Error> {
    use LedgerState as S;
    Ok(match (state, code) {
        (S::Committed, "ok") => (S::Accepted, false),
        (S::Committed, "members_changed") => (S::OutcomeUnknown, true),
        (
            S::Committed,
            "failed" | "busy" | "expired" | "unknown" | "outcome_unknown" | "not_allowed_mime"
            | "too_large" | "rejected",
        ) => (S::OutcomeUnknown, false),
        (S::PermitIssued, "members_changed") => (S::Refused, true),
        (S::PermitIssued, "not_allowed_mime" | "too_large" | "rejected") => (S::Refused, false),
        (S::PermitIssued, "failed" | "busy" | "expired" | "unknown" | "outcome_unknown") => {
            (S::OutcomeUnknown, false)
        }
        (S::Pending | S::Uploading | S::ReferenceReady, "members_changed") => (S::Refused, true),
        (
            S::Pending | S::Uploading | S::ReferenceReady,
            "not_allowed_mime" | "too_large" | "rejected",
        ) => (S::Refused, false),
        (
            S::Pending | S::Uploading | S::ReferenceReady,
            "failed" | "busy" | "expired" | "unknown",
        ) => (S::Pending, false),
        _ => return Err(Error::InvalidRequest("RCS media outcome")),
    })
}

/// Counts one more failed pre-permit upload attempt and returns the new total.
fn count_upload_failure(
    tx: &Transaction<'_>,
    command_id: CommandId,
    attachment_index: u32,
) -> Result<i64, Error> {
    tx.execute(
        "UPDATE rcs_media_send_ledger SET upload_failures=MIN(upload_failures+1,3) WHERE command_id=? AND attachment_index=?",
        params![command_id.to_string(), i64::from(attachment_index)],
    )?;
    tx.query_row(
        "SELECT upload_failures FROM rcs_media_send_ledger WHERE command_id=? AND attachment_index=?",
        params![command_id.to_string(), i64::from(attachment_index)],
        |row| row.get(0),
    )
    .map_err(Error::from)
}

fn not_sent_reason(code: &str) -> Option<RcsMediaNotSentReason> {
    match code {
        "not_allowed_mime" => Some(RcsMediaNotSentReason::NotAllowedMime),
        "too_large" => Some(RcsMediaNotSentReason::TooLarge),
        "rejected" => Some(RcsMediaNotSentReason::Rejected),
        _ => None,
    }
}

fn remaining_open_indexes(
    tx: &Transaction<'_>,
    command_id: CommandId,
    attachment_index: u32,
) -> Result<Vec<u32>, Error> {
    let mut query = tx.prepare(&format!(
        "SELECT attachment_index FROM rcs_media_send_ledger WHERE command_id=? AND attachment_index>? AND state NOT IN ({TERMINAL_SQL}) ORDER BY attachment_index"
    ))?;
    query
        .query_map(
            params![command_id.to_string(), i64::from(attachment_index)],
            |row| row.get::<_, i64>(0),
        )?
        .map(|index| -> Result<u32, Error> { u32::try_from(index?).map_err(|_| Error::Database) })
        .collect()
}

fn raw_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

type RawRow = (
    i64,
    String,
    String,
    Option<i64>,
    Option<i64>,
    Option<String>,
);

fn ledger_row(raw: RawRow) -> Result<LedgerRow, Error> {
    Ok(LedgerRow {
        attachment_index: u32::try_from(raw.0).map_err(|_| Error::Database)?,
        attachment_id: parse(&raw.1)?,
        state: LedgerState::from_code(&raw.2)?,
        uploaded_at_ms: raw.3,
        permit_at_ms: raw.4,
        tmp_id: raw.5,
    })
}

/// Only the lowest open attachment may transition; a later index never overtakes an earlier one.
fn require_next(
    conn: &Connection,
    command_id: CommandId,
    attachment_index: u32,
) -> Result<LedgerRow, Error> {
    match next_attachment(conn, command_id)? {
        Some(row) if row.attachment_index == attachment_index => Ok(row),
        Some(_) => Err(Error::InvalidRequest("RCS media attachment order")),
        None => Err(Error::InvalidRequest("RCS media command closed")),
    }
}

fn expect_state(row: &LedgerRow, expected: LedgerState) -> Result<(), Error> {
    if row.state == expected {
        Ok(())
    } else {
        Err(Error::InvalidRequest("RCS media state"))
    }
}

/// Compare-and-set: the row must still be in `from`, so a stale caller cannot skip a state.
fn set_state(
    tx: &Transaction<'_>,
    command_id: CommandId,
    attachment_index: u32,
    from: LedgerState,
    to: LedgerState,
) -> Result<(), Error> {
    let changed = tx.execute(
        "UPDATE rcs_media_send_ledger SET state=? WHERE command_id=? AND attachment_index=? AND state=?",
        params![
            to.code(),
            command_id.to_string(),
            i64::from(attachment_index),
            from.code()
        ],
    )?;
    if changed == 1 {
        Ok(())
    } else {
        Err(Error::InvalidRequest("RCS media state"))
    }
}

fn reset_to_pending(
    tx: &Transaction<'_>,
    command_id: CommandId,
    attachment_index: u32,
    from: LedgerState,
) -> Result<(), Error> {
    set_state(tx, command_id, attachment_index, from, LedgerState::Pending)?;
    tx.execute(
        "UPDATE rcs_media_send_ledger SET uploaded_at_ms=NULL WHERE command_id=? AND attachment_index=?",
        params![command_id.to_string(), i64::from(attachment_index)],
    )?;
    delete_ref(tx, command_id, attachment_index)
}

fn delete_ref(
    tx: &Transaction<'_>,
    command_id: CommandId,
    attachment_index: u32,
) -> Result<(), Error> {
    tx.execute(
        "DELETE FROM rcs_google_media_refs WHERE command_id=? AND attachment_index=?",
        params![command_id.to_string(), i64::from(attachment_index)],
    )?;
    Ok(())
}

/// The canonical identity of a demand's index list, used for compaction and dedup.
pub(crate) fn index_key(indexes: &[u32]) -> String {
    indexes
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) fn load_message(
    conn: &Connection,
    message_id: MessageId,
) -> Result<Option<MessagePayload>, Error> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT payload FROM messages WHERE id=?",
            params![message_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    payload
        .map(|payload| decode(payload.as_bytes()))
        .transpose()
}

/// Trusted-native-host download work. `media_ref` is intentionally redacted from Debug and is
/// never suitable for a renderer, sync payload, or log.
#[derive(Clone, PartialEq, Eq)]
pub struct PendingReceiveTask {
    pub message_id: MessageId,
    pub attachment_index: u32,
    pub source_gateway_id: DeviceId,
    pub media_ref: String,
    pub media_type: String,
    pub display_name: String,
    pub plaintext_bytes: u64,
    pub state: RcsMediaSyncState,
}

/// A local-only pre-download decision. This deliberately is not a hosted reservation: ciphertext
/// does not exist until after the Go download has completed and core has encrypted it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiveDownloadAdmission {
    Held { token: String },
    ExistingPrepared { attachment_id: AttachmentId },
    TooLarge,
    QuotaFull,
    CapacityUnavailable,
}

/// Sanitized receive placeholder for desktop and other renderer-facing adapters. It contains no
/// Google reference, file key, or native-only admission state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsMediaReceiveView {
    pub message_id: MessageId,
    pub attachment_index: u32,
    pub placeholder_id: String,
    pub media_type: String,
    pub display_name: String,
    pub plaintext_bytes: u64,
    pub state: RcsMediaSyncState,
    pub attachment_id: Option<AttachmentId>,
}

pub(crate) fn receive_view(
    conn: &Connection,
    message_id: MessageId,
    attachment_index: u32,
) -> Result<Option<RcsMediaReceiveView>, Error> {
    conn.query_row(
        "SELECT r.media_type,r.display_name,r.plaintext_bytes,r.state,r.attachment_id FROM rcs_media_receive r WHERE r.message_id=? AND r.attachment_index=? AND r.media_type IS NOT NULL AND r.display_name IS NOT NULL AND r.plaintext_bytes IS NOT NULL",
        params![message_id.to_string(), i64::from(attachment_index)],
        |row| {
            let state: String = row.get(3)?;
            let state = match state.as_str() {
                "downloading" => RcsMediaSyncState::Downloading,
                "stored" => RcsMediaSyncState::Stored,
                "too_large" => RcsMediaSyncState::TooLarge,
                "quota_full" => RcsMediaSyncState::QuotaFull,
                "failed" => RcsMediaSyncState::Failed,
                "expired" => RcsMediaSyncState::Expired,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            let attachment_id = row.get::<_, Option<String>>(4)?
                .map(|value| value.parse().map_err(|_| rusqlite::Error::InvalidQuery))
                .transpose()?;
            Ok(RcsMediaReceiveView {
                message_id,
                attachment_index,
                placeholder_id: format!("{message_id}:{attachment_index}"),
                media_type: row.get(0)?,
                display_name: row.get(1)?,
                plaintext_bytes: u64::try_from(row.get::<_, i64>(2)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
                state,
                attachment_id,
            })
        },
    ).optional().map_err(Into::into)
}

fn receive_metadata(
    conn: &Connection,
    message_id: MessageId,
    attachment_index: u32,
) -> Result<Option<RcsMediaSlotMetadata>, Error> {
    conn.query_row(
        "SELECT media_type,display_name,plaintext_bytes FROM rcs_media_receive WHERE message_id=? AND attachment_index=?",
        params![message_id.to_string(), i64::from(attachment_index)],
        |row| {
            let plaintext_bytes: Option<i64> = row.get(2)?;
            match (row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?, plaintext_bytes) {
                (Some(media_type), Some(display_name), Some(plaintext_bytes)) => Ok(Some(RcsMediaSlotMetadata {
                    media_type,
                    display_name,
                    plaintext_bytes: u64::try_from(plaintext_bytes).map_err(|_| rusqlite::Error::InvalidQuery)?,
                })),
                (None, None, None) => Ok(None),
                _ => Err(rusqlite::Error::InvalidQuery),
            }
        },
    )
    .optional()
    .map(|metadata| metadata.flatten())
    .map_err(Into::into)
}

fn admission_bytes(plaintext_bytes: u64) -> u64 {
    // PPSS v1: 5-byte magic + 24-byte header + 21-byte final frame. Each read adds
    // a 17-byte tag and a 4-byte length. Even one-byte short reads fit this bound;
    // encryption also enforces the global ciphertext cap before publishing.
    plaintext_bytes
        .saturating_mul(22)
        .saturating_add(50)
        .min(MAX_ATTACHMENT_CIPHERTEXT_BYTES)
}

fn local_receive_usage(tx: &Transaction<'_>) -> Result<u64, Error> {
    tx.query_row(
        "SELECT COALESCE(SUM(CASE WHEN l.prepared_attachment_id IS NULL THEN l.admission_bytes ELSE a.ciphertext_bytes END),0) FROM rcs_media_receive_local l JOIN rcs_media_receive r USING(message_id,attachment_index) LEFT JOIN attachments a ON a.attachment_id=l.prepared_attachment_id WHERE r.state!='stored'",
        [],
        |row| row.get::<_, i64>(0),
    )
    .and_then(|value| u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value)))
    .map_err(Into::into)
}

pub(crate) fn admit_receive_download(
    tx: &Transaction<'_>,
    device_id: DeviceId,
    message_id: MessageId,
    attachment_index: u32,
    available_bytes: Option<u64>,
) -> Result<ReceiveDownloadAdmission, Error> {
    let row: Option<(String, String, i64, Option<String>, i64)> = tx
        .query_row(
            "SELECT l.source_gateway_id,r.state,l.plaintext_bytes,l.prepared_attachment_id,l.admission_bytes FROM rcs_media_receive_local l JOIN rcs_media_receive r USING(message_id,attachment_index) WHERE l.message_id=? AND l.attachment_index=?",
            params![message_id.to_string(), i64::from(attachment_index)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()?;
    let Some((
        source_gateway_id,
        state,
        plaintext_bytes,
        prepared_attachment_id,
        previous_admission,
    )) = row
    else {
        return Err(Error::NotFound);
    };
    if source_gateway_id != device_id.to_string()
        || !matches!(state.as_str(), "downloading" | "failed" | "quota_full")
    {
        return Err(Error::InvalidRequest("RCS receive download is not allowed"));
    }
    if let Some(attachment_id) = prepared_attachment_id {
        consume_receive_demand(tx, device_id, message_id, attachment_index)?;
        return attachment_id
            .parse()
            .map(|attachment_id| ReceiveDownloadAdmission::ExistingPrepared { attachment_id })
            .map_err(|_| Error::Database);
    }
    let Some(available_bytes) = available_bytes else {
        return Ok(ReceiveDownloadAdmission::CapacityUnavailable);
    };
    let plaintext_bytes = u64::try_from(plaintext_bytes).map_err(|_| Error::Database)?;
    if plaintext_bytes > MAX_ATTACHMENT_PLAINTEXT_BYTES {
        return Ok(ReceiveDownloadAdmission::TooLarge);
    }
    let required = admission_bytes(plaintext_bytes);
    let usage = local_receive_usage(tx)?
        .saturating_sub(u64::try_from(previous_admission).map_err(|_| Error::Database)?);
    if available_bytes < required {
        let minimum = plaintext_bytes
            + 50
            + plaintext_bytes.div_ceil(peppy_crypto::STREAM_CHUNK_BYTES as u64) * 21;
        return Ok(if available_bytes < minimum {
            ReceiveDownloadAdmission::QuotaFull
        } else {
            ReceiveDownloadAdmission::CapacityUnavailable
        });
    }
    if usage.saturating_add(required) > RCS_MEDIA_RECEIVE_LOCAL_BUDGET_BYTES {
        return Ok(ReceiveDownloadAdmission::CapacityUnavailable);
    }
    let token = uuid::Uuid::new_v4().to_string();
    tx.execute(
        "UPDATE rcs_media_receive_local SET admission_bytes=?,admission_token=? WHERE message_id=? AND attachment_index=?",
        params![to_i64(required)?, token, message_id.to_string(), i64::from(attachment_index)],
    )?;
    consume_receive_demand(tx, device_id, message_id, attachment_index)?;
    Ok(ReceiveDownloadAdmission::Held { token })
}

fn consume_receive_demand(
    conn: &Connection,
    device_id: DeviceId,
    message_id: MessageId,
    attachment_index: u32,
) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM rcs_media_retry_demand WHERE message_id=? AND attachment_index=? AND source_gateway_id=?",
        params![message_id.to_string(), i64::from(attachment_index), device_id.to_string()],
    )?;
    Ok(())
}

pub(crate) fn receive_import_details(
    conn: &Connection,
    message_id: MessageId,
    attachment_index: u32,
    token: &str,
) -> Result<Option<(String, String, u64)>, Error> {
    conn.query_row(
        "SELECT media_type,display_name,plaintext_bytes FROM rcs_media_receive_local WHERE message_id=? AND attachment_index=? AND admission_token=? AND prepared_attachment_id IS NULL",
        params![message_id.to_string(), i64::from(attachment_index), token],
        |row| Ok((row.get(0)?, row.get(1)?, u64::try_from(row.get::<_, i64>(2)?).map_err(|_| rusqlite::Error::InvalidQuery)?)),
    ).optional().map_err(Into::into)
}

pub(crate) fn bind_prepared_receive_attachment(
    tx: &Transaction<'_>,
    message_id: MessageId,
    attachment_index: u32,
    token: &str,
    attachment_id: AttachmentId,
) -> Result<bool, Error> {
    Ok(tx.execute(
        "UPDATE rcs_media_receive_local SET prepared_attachment_id=?,admission_bytes=0,admission_token=NULL WHERE message_id=? AND attachment_index=? AND admission_token=? AND prepared_attachment_id IS NULL",
        params![attachment_id.to_string(), message_id.to_string(), i64::from(attachment_index), token],
    )? == 1)
}

pub(crate) fn finalize_prepared_receive_attachment(
    tx: &Transaction<'_>,
    ctx: &Ctx<'_>,
    message_id: MessageId,
    attachment_index: u32,
) -> Result<(), Error> {
    let attachment_id: String = tx.query_row(
        "SELECT prepared_attachment_id FROM rcs_media_receive_local WHERE message_id=? AND attachment_index=? AND prepared_attachment_id IS NOT NULL",
        params![message_id.to_string(), i64::from(attachment_index)],
        |row| row.get(0),
    )?;
    let attachment_id: AttachmentId = attachment_id.parse().map_err(|_| Error::Database)?;
    let descriptor = descriptor_for_attachment(tx, attachment_id)?.ok_or(Error::InvalidRequest(
        "RCS receive attachment is not hosted",
    ))?;
    let message = load_message(tx, message_id)?.ok_or(Error::NotFound)?;
    if apply_receive_state(
        tx,
        message,
        attachment_index,
        RcsMediaSyncState::Stored,
        Some(&descriptor),
        receive_metadata(tx, message_id, attachment_index)?.as_ref(),
    )?
    .is_some()
    {
        return Err(Error::Conflict);
    }
    enqueue_state_checkpoint(tx, ctx, message_id, attachment_index)
}

pub(crate) fn mark_receive_state(
    tx: &Transaction<'_>,
    ctx: &Ctx<'_>,
    device_id: DeviceId,
    message_id: MessageId,
    attachment_index: u32,
    state: RcsMediaSyncState,
) -> Result<(), Error> {
    if !matches!(
        state,
        RcsMediaSyncState::Downloading
            | RcsMediaSyncState::Failed
            | RcsMediaSyncState::QuotaFull
            | RcsMediaSyncState::Expired
    ) {
        return Err(Error::InvalidRequest("RCS receive state"));
    }
    let source: String = tx.query_row(
        "SELECT source_gateway_id FROM rcs_media_receive_local WHERE message_id=? AND attachment_index=?",
        params![message_id.to_string(), i64::from(attachment_index)],
        |row| row.get(0),
    )?;
    if source != device_id.to_string() {
        return Err(Error::InvalidRequest("RCS receive download is not allowed"));
    }
    let current: String = tx.query_row(
        "SELECT state FROM rcs_media_receive WHERE message_id=? AND attachment_index=?",
        params![message_id.to_string(), i64::from(attachment_index)],
        |row| row.get(0),
    )?;
    if current == "stored" {
        return Ok(());
    }
    tx.execute(
        "UPDATE rcs_media_receive SET state=? WHERE message_id=? AND attachment_index=?",
        params![
            state.code(),
            message_id.to_string(),
            i64::from(attachment_index)
        ],
    )?;
    tx.execute(
        "UPDATE rcs_media_receive_local SET admission_bytes=0,admission_token=NULL WHERE message_id=? AND attachment_index=?",
        params![message_id.to_string(), i64::from(attachment_index)],
    )?;
    enqueue_state_checkpoint(tx, ctx, message_id, attachment_index)
}

/// Every compactable event repeats the canonical projection, including any hosted descriptor.
/// A stale callback must never supersede terminal evidence with its obsolete input state.
pub(crate) fn enqueue_state_checkpoint(
    conn: &Connection,
    ctx: &Ctx<'_>,
    message_id: MessageId,
    attachment_index: u32,
) -> Result<(), Error> {
    let message = load_message(conn, message_id)?.ok_or(Error::NotFound)?;
    let (state, reason, attachment, metadata) = if message.direction == Direction::Incoming {
        if message.source_device_id != ctx.device_id || message.transport != Transport::Rcs {
            return Err(Error::InvalidRequest("RCS media checkpoint source"));
        }
        let view = receive_view(conn, message_id, attachment_index)?.ok_or(Error::NotFound)?;
        let attachment = if view.state == RcsMediaSyncState::Stored {
            Some(
                descriptor_for_attachment(conn, view.attachment_id.ok_or(Error::Database)?)?
                    .ok_or(Error::Database)?,
            )
        } else {
            None
        };
        (
            view.state,
            None,
            attachment,
            Some(RcsMediaSlotMetadata {
                media_type: view.media_type,
                display_name: view.display_name,
                plaintext_bytes: view.plaintext_bytes,
            }),
        )
    } else {
        if !outgoing_status_authorized(conn, ctx.device_id, message_id, attachment_index)? {
            return Err(Error::InvalidRequest("RCS media checkpoint gateway"));
        }
        let (state, reason): (String, Option<String>) = conn.query_row(
            "SELECT state,reason FROM rcs_media_send_status WHERE message_id=? AND attachment_index=?",
            params![message_id.to_string(), i64::from(attachment_index)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        (
            RcsMediaSyncState::from_code(&state).ok_or(Error::Database)?,
            reason
                .map(|value| RcsMediaNotSentReason::from_code(&value).ok_or(Error::Database))
                .transpose()?,
            None,
            None,
        )
    };
    enqueue(
        conn,
        ctx,
        EnvelopePurpose::Event,
        None,
        None,
        &PrivatePayload::RcsMediaState {
            message_id,
            attachment_index,
            state,
            reason,
            attachment,
            metadata,
        },
    )?;
    Ok(())
}

impl std::fmt::Debug for PendingReceiveTask {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingReceiveTask")
            .field("message_id", &self.message_id)
            .field("attachment_index", &self.attachment_index)
            .field("source_gateway_id", &self.source_gateway_id)
            .field("media_ref", &"[redacted]")
            .field("media_type", &self.media_type)
            .field("display_name", &self.display_name)
            .field("plaintext_bytes", &self.plaintext_bytes)
            .field("state", &self.state)
            .finish()
    }
}

pub(crate) fn pending_receive_tasks(
    conn: &Connection,
    device_id: DeviceId,
) -> Result<Vec<PendingReceiveTask>, Error> {
    let mut statement = conn.prepare(
        "SELECT l.message_id,l.attachment_index,l.source_gateway_id,l.media_ref,l.media_type,l.display_name,l.plaintext_bytes,r.state FROM rcs_media_receive_local l JOIN rcs_media_receive r USING(message_id,attachment_index) WHERE l.source_gateway_id=? AND r.state IN ('downloading','failed','quota_full') ORDER BY l.rowid DESC",
    )?;
    statement
        .query_map([device_id.to_string()], |row| {
            let state: String = row.get(7)?;
            let state = match state.as_str() {
                "downloading" => RcsMediaSyncState::Downloading,
                "failed" => RcsMediaSyncState::Failed,
                "quota_full" => RcsMediaSyncState::QuotaFull,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(PendingReceiveTask {
                message_id: row
                    .get::<_, String>(0)?
                    .parse()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                attachment_index: u32::try_from(row.get::<_, i64>(1)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                source_gateway_id: row
                    .get::<_, String>(2)?
                    .parse()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                media_ref: String::from_utf8(row.get(3)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                media_type: row.get(4)?,
                display_name: row.get(5)?,
                plaintext_bytes: u64::try_from(row.get::<_, i64>(6)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                state,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Applies a sanitized per-attachment state. Incoming states update the receive projection and,
/// for `stored`, attach the hosted descriptor to the message exactly like MMS media. Outgoing
/// states update the send-status projection.
#[derive(Clone, Copy)]
pub(crate) struct StateContent<'a> {
    pub(crate) attachment: Option<&'a MediaDescriptor>,
    pub(crate) metadata: Option<&'a RcsMediaSlotMetadata>,
}

type ReceiveProjectionRow = (
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
);

pub(crate) fn apply_state(
    conn: &Connection,
    producer: DeviceId,
    message_id: MessageId,
    attachment_index: u32,
    state: RcsMediaSyncState,
    reason: Option<RcsMediaNotSentReason>,
    attachment: Option<&MediaDescriptor>,
) -> Result<Option<QuarantineReason>, Error> {
    apply_state_with_metadata(
        conn,
        producer,
        message_id,
        attachment_index,
        state,
        reason,
        StateContent {
            attachment,
            metadata: None,
        },
    )
}

pub(crate) const PENDING_RCS_MEDIA_STATE_BUDGET: i64 = 4096;

#[derive(Clone, Copy, PartialEq, Eq)]
enum StateAuthority {
    Unavailable,
    Allowed,
    Denied,
}

#[derive(Clone, Copy)]
enum StateLane {
    Nonterminal,
    Stored,
    Terminal,
}

fn state_lane(state: RcsMediaSyncState) -> StateLane {
    if state == RcsMediaSyncState::Stored {
        StateLane::Stored
    } else if !state.is_incoming() && state != RcsMediaSyncState::Sending {
        StateLane::Terminal
    } else {
        StateLane::Nonterminal
    }
}

fn validate_state_content(
    attachment_index: u32,
    state: RcsMediaSyncState,
    reason: Option<RcsMediaNotSentReason>,
    content: StateContent<'_>,
) -> bool {
    let attachment_shape_ok = match (state, content.attachment) {
        (RcsMediaSyncState::Stored, Some(descriptor)) => descriptor.is_valid(),
        (RcsMediaSyncState::Stored, None) => false,
        (_, Some(_)) => false,
        (_, None) => true,
    };
    (attachment_index as usize) < MAX_RCS_MEDIA_ATTACHMENTS
        && attachment_shape_ok
        && (state == RcsMediaSyncState::NotSent) == reason.is_some()
        && (!state.is_incoming() || content.metadata.is_none_or(RcsMediaSlotMetadata::is_valid))
}

fn state_authority(
    conn: &Connection,
    producer: DeviceId,
    message_id: MessageId,
) -> Result<StateAuthority, Error> {
    let Some(message) = load_message(conn, message_id)? else {
        return Ok(StateAuthority::Unavailable);
    };
    if message.direction == Direction::Incoming {
        return Ok(
            if message.transport == Transport::Rcs && message.source_device_id == producer {
                StateAuthority::Allowed
            } else {
                StateAuthority::Denied
            },
        );
    }
    let gateway: Option<String> = conn
        .query_row(
            "SELECT gateway_device_id FROM commands WHERE message_id=?",
            [message_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(match gateway {
        None => StateAuthority::Unavailable,
        Some(gateway) if gateway == producer.to_string() => StateAuthority::Allowed,
        Some(_) => StateAuthority::Denied,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_authenticated_state(
    conn: &Connection,
    producer: DeviceId,
    producer_sequence: u64,
    message_id: MessageId,
    attachment_index: u32,
    state: RcsMediaSyncState,
    reason: Option<RcsMediaNotSentReason>,
    content: StateContent<'_>,
) -> Result<Option<QuarantineReason>, Error> {
    if !validate_state_content(attachment_index, state, reason, content) {
        return Ok(Some(QuarantineReason::InvalidPayload));
    }
    match state_authority(conn, producer, message_id)? {
        StateAuthority::Unavailable => stage_state(
            conn,
            producer,
            producer_sequence,
            message_id,
            attachment_index,
            state,
            reason,
            content,
        ),
        StateAuthority::Denied => Ok(Some(QuarantineReason::InvalidPayload)),
        StateAuthority::Allowed => apply_ordered_state(
            conn,
            producer,
            producer_sequence,
            message_id,
            attachment_index,
            state,
            reason,
            content,
        ),
    }
}

pub(crate) fn apply_state_with_metadata(
    conn: &Connection,
    producer: DeviceId,
    message_id: MessageId,
    attachment_index: u32,
    state: RcsMediaSyncState,
    reason: Option<RcsMediaNotSentReason>,
    content: StateContent<'_>,
) -> Result<Option<QuarantineReason>, Error> {
    if !validate_state_content(attachment_index, state, reason, content) {
        return Ok(Some(QuarantineReason::InvalidPayload));
    }
    apply_authorized_state(
        conn,
        producer,
        message_id,
        attachment_index,
        state,
        reason,
        content,
    )
}

#[allow(clippy::too_many_arguments)]
fn stage_state(
    conn: &Connection,
    producer: DeviceId,
    producer_sequence: u64,
    message_id: MessageId,
    attachment_index: u32,
    state: RcsMediaSyncState,
    reason: Option<RcsMediaNotSentReason>,
    content: StateContent<'_>,
) -> Result<Option<QuarantineReason>, Error> {
    let key = params![
        producer.to_string(),
        message_id.to_string(),
        i64::from(attachment_index)
    ];
    let existing: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT metadata,stored_attachment FROM pending_rcs_media_states WHERE producer_device_id=? AND message_id=? AND attachment_index=?",
            key,
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let metadata = content.metadata.map(json).transpose()?;
    let attachment = content.attachment.map(json).transpose()?;
    if let Some((known_metadata, known_attachment)) = &existing {
        if metadata
            .as_ref()
            .is_some_and(|value| known_metadata.as_ref().is_some_and(|known| known != value))
            || attachment.as_ref().is_some_and(|value| {
                known_attachment
                    .as_ref()
                    .is_some_and(|known| known != value)
            })
        {
            return Ok(Some(QuarantineReason::PayloadConflict));
        }
    } else {
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM pending_rcs_media_states", [], |row| {
                row.get(0)
            })?;
        if count >= PENDING_RCS_MEDIA_STATE_BUDGET {
            return Ok(Some(QuarantineReason::InvalidPayload));
        }
        conn.execute(
            "INSERT INTO pending_rcs_media_states(producer_device_id,message_id,attachment_index,metadata) VALUES(?,?,?,?)",
            params![producer.to_string(), message_id.to_string(), i64::from(attachment_index), metadata],
        )?;
    }
    if existing.is_some() && metadata.is_some() {
        conn.execute(
            "UPDATE pending_rcs_media_states SET metadata=COALESCE(metadata,?) WHERE producer_device_id=? AND message_id=? AND attachment_index=?",
            params![metadata, producer.to_string(), message_id.to_string(), i64::from(attachment_index)],
        )?;
    }
    let sequence = to_i64(producer_sequence)?;
    let (sequence_column, state_column, reason_column) = match state_lane(state) {
        StateLane::Nonterminal => (
            "nonterminal_sequence",
            "nonterminal_state",
            "nonterminal_reason",
        ),
        StateLane::Stored => ("stored_sequence", "stored_state", "stored_reason"),
        StateLane::Terminal => ("terminal_sequence", "terminal_state", "terminal_reason"),
    };
    let sql = format!(
        "UPDATE pending_rcs_media_states SET {sequence_column}=?,{state_column}=?,{reason_column}=?{} WHERE producer_device_id=? AND message_id=? AND attachment_index=? AND ({sequence_column} IS NULL OR {sequence_column}<?)",
        if matches!(state_lane(state), StateLane::Stored) {
            ",stored_attachment=?"
        } else {
            ""
        }
    );
    let producer = producer.to_string();
    let message = message_id.to_string();
    if matches!(state_lane(state), StateLane::Stored) {
        conn.execute(
            &sql,
            params![
                sequence,
                state.code(),
                reason.map(RcsMediaNotSentReason::code),
                attachment,
                producer,
                message,
                i64::from(attachment_index),
                sequence
            ],
        )?;
    } else {
        conn.execute(
            &sql,
            params![
                sequence,
                state.code(),
                reason.map(RcsMediaNotSentReason::code),
                producer,
                message,
                i64::from(attachment_index),
                sequence
            ],
        )?;
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn apply_ordered_state(
    conn: &Connection,
    producer: DeviceId,
    producer_sequence: u64,
    message_id: MessageId,
    attachment_index: u32,
    state: RcsMediaSyncState,
    reason: Option<RcsMediaNotSentReason>,
    content: StateContent<'_>,
) -> Result<Option<QuarantineReason>, Error> {
    let lane = state_lane(state);
    let column = match lane {
        StateLane::Nonterminal => "nonterminal_sequence",
        StateLane::Stored => "stored_sequence",
        StateLane::Terminal => "terminal_sequence",
    };
    let known: Option<i64> = conn
        .query_row(
            &format!("SELECT {column} FROM rcs_media_state_clocks WHERE producer_device_id=? AND message_id=? AND attachment_index=?"),
            params![producer.to_string(), message_id.to_string(), i64::from(attachment_index)],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let sequence = to_i64(producer_sequence)?;
    if known.is_some_and(|known| known >= sequence) {
        return Ok(None);
    }
    let result = apply_authorized_state(
        conn,
        producer,
        message_id,
        attachment_index,
        state,
        reason,
        content,
    )?;
    if result.is_none() {
        conn.execute(
            "INSERT OR IGNORE INTO rcs_media_state_clocks(producer_device_id,message_id,attachment_index) VALUES(?,?,?)",
            params![producer.to_string(), message_id.to_string(), i64::from(attachment_index)],
        )?;
        conn.execute(
            &format!("UPDATE rcs_media_state_clocks SET {column}=? WHERE producer_device_id=? AND message_id=? AND attachment_index=?"),
            params![sequence, producer.to_string(), message_id.to_string(), i64::from(attachment_index)],
        )?;
    }
    Ok(result)
}

fn apply_authorized_state(
    conn: &Connection,
    producer: DeviceId,
    message_id: MessageId,
    attachment_index: u32,
    state: RcsMediaSyncState,
    reason: Option<RcsMediaNotSentReason>,
    content: StateContent<'_>,
) -> Result<Option<QuarantineReason>, Error> {
    let Some(message) = load_message(conn, message_id)? else {
        return Ok(Some(QuarantineReason::InvalidPayload));
    };
    if state.is_incoming() {
        if message.source_device_id != producer
            || message.transport != Transport::Rcs
            || message.direction != Direction::Incoming
        {
            return Ok(Some(QuarantineReason::InvalidPayload));
        }
        return apply_receive_state(
            conn,
            message,
            attachment_index,
            state,
            content.attachment,
            content.metadata,
        );
    }
    if message.direction != Direction::Outgoing
        || !outgoing_status_authorized(conn, producer, message_id, attachment_index)?
    {
        return Ok(Some(QuarantineReason::InvalidPayload));
    }
    let current: Option<String> = conn
        .query_row(
            "SELECT state FROM rcs_media_send_status WHERE message_id=? AND attachment_index=?",
            params![message_id.to_string(), i64::from(attachment_index)],
            |row| row.get(0),
        )
        .optional()?;
    if current.is_some_and(|current| {
        current != RcsMediaSyncState::Sending.code()
            && !(current == RcsMediaSyncState::OutcomeUnknown.code()
                && state == RcsMediaSyncState::Sent)
    }) {
        return Ok(None);
    }
    conn.execute(
        "INSERT INTO rcs_media_send_status(message_id,attachment_index,state,reason) VALUES(?,?,?,?) ON CONFLICT(message_id,attachment_index) DO UPDATE SET state=excluded.state,reason=excluded.reason",
        params![message_id.to_string(), i64::from(attachment_index), state.code(), reason.map(RcsMediaNotSentReason::code)],
    )?;
    Ok(None)
}

pub(crate) fn reconcile_staged_states(
    conn: &Connection,
    message_id: MessageId,
) -> Result<(), Error> {
    let mut query = conn.prepare(
        "SELECT producer_device_id,attachment_index,metadata,nonterminal_sequence,nonterminal_state,nonterminal_reason,stored_sequence,stored_attachment,terminal_sequence,terminal_state,terminal_reason FROM pending_rcs_media_states WHERE message_id=? ORDER BY producer_device_id,attachment_index",
    )?;
    let rows = query
        .query_map([message_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, u32>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<u64>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<u64>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<u64>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(query);
    for row in rows {
        let producer: DeviceId = row.0.parse().map_err(|_| Error::Database)?;
        match state_authority(conn, producer, message_id)? {
            StateAuthority::Unavailable => continue,
            StateAuthority::Denied => {
                delete_staged_state(conn, producer, message_id, row.1)?;
                continue;
            }
            StateAuthority::Allowed => {}
        }
        conn.execute_batch("SAVEPOINT rcs_media_candidate")?;
        let metadata: Option<RcsMediaSlotMetadata> = row
            .2
            .as_deref()
            .map(|value| decode(value.as_bytes()))
            .transpose()?;
        let mut rejected = false;
        let candidates = [
            (row.3, row.4.as_deref(), row.5.as_deref(), None),
            (
                row.6,
                Some(RcsMediaSyncState::Stored.code()),
                None,
                row.7.as_deref(),
            ),
            (row.8, row.9.as_deref(), row.10.as_deref(), None),
        ];
        for (sequence, state, reason, attachment) in candidates {
            let (Some(sequence), Some(state)) = (sequence, state) else {
                continue;
            };
            let state = RcsMediaSyncState::from_code(state).ok_or(Error::Database)?;
            let reason = reason
                .map(|value| RcsMediaNotSentReason::from_code(value).ok_or(Error::Database))
                .transpose()?;
            let attachment: Option<MediaDescriptor> = attachment
                .map(|value| decode(value.as_bytes()))
                .transpose()?;
            if apply_ordered_state(
                conn,
                producer,
                sequence,
                message_id,
                row.1,
                state,
                reason,
                StateContent {
                    attachment: attachment.as_ref(),
                    metadata: metadata.as_ref(),
                },
            )?
            .is_some()
            {
                rejected = true;
                break;
            }
        }
        if rejected {
            conn.execute_batch("ROLLBACK TO rcs_media_candidate; RELEASE rcs_media_candidate")?;
        } else {
            conn.execute_batch("RELEASE rcs_media_candidate")?;
        }
        delete_staged_state(conn, producer, message_id, row.1)?;
    }
    Ok(())
}

fn delete_staged_state(
    conn: &Connection,
    producer: DeviceId,
    message_id: MessageId,
    attachment_index: u32,
) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM pending_rcs_media_states WHERE producer_device_id=? AND message_id=? AND attachment_index=?",
        params![producer.to_string(), message_id.to_string(), i64::from(attachment_index)],
    )?;
    Ok(())
}

/// Outgoing media states are produced by the executing gateway, while the message itself was
/// normally composed by another device. Bind the status to the received command and its durable
/// per-index ledger rather than incorrectly treating the composer as the gateway. This also
/// deliberately accepts group messages whose compose transport remained MMS under D5.
fn outgoing_status_authorized(
    conn: &Connection,
    producer: DeviceId,
    message_id: MessageId,
    attachment_index: u32,
) -> Result<bool, Error> {
    // `commands` is written from the command envelope route both by the composing sender and
    // the addressed receiver. Unlike the gateway-local ledger, it is present on every device.
    let gateway: Option<String> = conn
        .query_row(
            "SELECT gateway_device_id FROM commands WHERE message_id=?",
            [message_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(gateway.as_deref() == Some(&producer.to_string())
        && load_message(conn, message_id)?
            .is_some_and(|message| (attachment_index as usize) < message.record.attachments.len()))
}

fn apply_receive_state(
    conn: &Connection,
    mut message: MessagePayload,
    attachment_index: u32,
    state: RcsMediaSyncState,
    attachment: Option<&MediaDescriptor>,
    metadata: Option<&RcsMediaSlotMetadata>,
) -> Result<Option<QuarantineReason>, Error> {
    use QuarantineReason as Q;
    let message_id = message.record.message_id.to_string();
    let attachment_id = attachment.map(|descriptor| descriptor.attachment_id.to_string());
    let known: Option<ReceiveProjectionRow> = conn
        .query_row(
            "SELECT attachment_id,state,media_type,display_name,plaintext_bytes FROM rcs_media_receive WHERE message_id=? AND attachment_index=?",
            params![message_id, i64::from(attachment_index)],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()?;
    if known
        .as_ref()
        .is_some_and(|(known_attachment, known_state, ..)| {
            known_attachment.is_some()
                && known_state == RcsMediaSyncState::Stored.code()
                && state != RcsMediaSyncState::Stored
        })
    {
        return Ok(None);
    }
    if let (Some((Some(known), ..)), Some(incoming)) = (&known, &attachment_id)
        && known != incoming
    {
        return Ok(Some(Q::PayloadConflict));
    }
    if let Some(descriptor) = attachment
        && (!store_remote_media(conn, std::slice::from_ref(descriptor))?
            || !attach_reference(
                conn,
                &mut message,
                attachment_index,
                descriptor.attachment_id,
            )?)
    {
        return Ok(Some(Q::PayloadConflict));
    }
    let metadata = metadata.cloned().or_else(|| {
        known
            .as_ref()
            .and_then(|(_, _, media_type, display_name, plaintext_bytes)| {
                Some(RcsMediaSlotMetadata {
                    media_type: media_type.clone()?,
                    display_name: display_name.clone()?,
                    plaintext_bytes: u64::try_from((*plaintext_bytes)?).ok()?,
                })
            })
    });
    if let Some(existing) = known.as_ref()
        && let Some(metadata) = metadata.as_ref()
        && (existing.2.as_deref() != Some(metadata.media_type.as_str())
            || existing.3.as_deref() != Some(metadata.display_name.as_str())
            || existing.4 != Some(metadata.plaintext_bytes as i64))
    {
        return Ok(Some(Q::PayloadConflict));
    }
    conn.execute(
        "INSERT INTO rcs_media_receive(message_id,attachment_index,attachment_id,state,media_type,display_name,plaintext_bytes) VALUES(?,?,?,?,?,?,?) ON CONFLICT(message_id,attachment_index) DO UPDATE SET state=excluded.state,attachment_id=COALESCE(excluded.attachment_id,rcs_media_receive.attachment_id)",
        params![
            message_id,
            i64::from(attachment_index),
            attachment_id,
            state.code(),
            metadata.as_ref().map(|metadata| metadata.media_type.as_str()),
            metadata.as_ref().map(|metadata| metadata.display_name.as_str()),
            metadata.as_ref().map(|metadata| to_i64(metadata.plaintext_bytes)).transpose()?
        ],
    )?;
    Ok(None)
}

/// Binds a hosted reference at its original Google index. Successful later indexes are independent
/// of an earlier failed/oversized index, while the message's public attachment list remains in
/// original-index order with gaps represented only by the receive projection.
fn attach_reference(
    conn: &Connection,
    message: &mut MessagePayload,
    attachment_index: u32,
    attachment_id: AttachmentId,
) -> Result<bool, Error> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT attachment_id FROM message_attachments WHERE message_id=? AND position=?",
            params![
                message.record.message_id.to_string(),
                to_i64(u64::from(attachment_index))?
            ],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        return Ok(existing == attachment_id.to_string());
    }
    let message_id = message.record.message_id.to_string();
    conn.execute(
        "INSERT OR IGNORE INTO message_attachments(message_id,position,attachment_id) VALUES(?,?,?)",
        params![message_id, to_i64(u64::from(attachment_index))?, attachment_id.to_string()],
    )?;
    // `MessageRecord.attachments` cannot represent sparse original indexes. Rebuild its compact
    // display order from the durable original-index bindings rather than requiring index zero to
    // have stored first.
    let mut statement = conn.prepare(
        "SELECT attachment_id FROM message_attachments WHERE message_id=? ORDER BY position",
    )?;
    message.record.attachments = statement
        .query_map([message_id.as_str()], |row| row.get::<_, String>(0))?
        .map(|row| {
            Ok(AttachmentReference {
                attachment_id: row?.parse().map_err(|_| Error::Database)?,
                pending: true,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    conn.execute(
        "UPDATE messages SET payload=? WHERE id=?",
        params![json(&*message)?, message_id],
    )?;
    Ok(true)
}

/// Records a demand addressed to this gateway. Other gateways ignore it, and history never
/// triggers a download.
pub(crate) fn apply_demand(
    conn: &Connection,
    device_id: DeviceId,
    message_id: MessageId,
    source_gateway_id: DeviceId,
    attachment_indexes: &[u32],
    historical: bool,
) -> Result<Option<QuarantineReason>, Error> {
    let bounded = !attachment_indexes.is_empty()
        && attachment_indexes.len() <= MAX_RCS_MEDIA_ATTACHMENTS
        && attachment_indexes.windows(2).all(|pair| pair[0] < pair[1])
        && attachment_indexes
            .iter()
            .all(|index| (*index as usize) < MAX_RCS_MEDIA_ATTACHMENTS);
    if !bounded {
        return Ok(Some(QuarantineReason::InvalidPayload));
    }
    if historical || source_gateway_id != device_id {
        return Ok(None);
    }
    for index in attachment_indexes {
        conn.execute(
            "INSERT OR IGNORE INTO rcs_media_retry_demand(message_id,attachment_index,source_gateway_id) VALUES(?,?,?)",
            params![
                message_id.to_string(),
                i64::from(*index),
                source_gateway_id.to_string()
            ],
        )?;
    }
    Ok(None)
}

/// Persists every incoming placeholder before the host is allowed to download.  The Google
/// reference is deliberately absent: it remains in the Android SQLCipher projection.
pub(crate) fn record_receive_manifest(
    tx: &Transaction<'_>,
    message_id: MessageId,
    source_gateway_id: DeviceId,
    attachments: &[RcsIncomingAttachment],
    _historical: bool,
    ctx: &Ctx<'_>,
) -> Result<(), Error> {
    if attachments.len() > MAX_RCS_MEDIA_ATTACHMENTS
        || attachments.iter().enumerate().any(|(position, item)| {
            item.original_index != position as u32
                || item.media_type.is_empty()
                || item.media_type.len() > 255
                || item.display_name.is_empty()
                || item.display_name.len() > 1024
        })
    {
        return Err(Error::InvalidRequest("RCS receive manifest"));
    }
    for item in attachments {
        let state = if item.too_large {
            RcsMediaSyncState::TooLarge
        } else {
            RcsMediaSyncState::Downloading
        };
        let metadata = RcsMediaSlotMetadata {
            media_type: item.media_type.clone(),
            display_name: item.display_name.clone(),
            plaintext_bytes: item.plaintext_bytes,
        };
        tx.execute(
            "INSERT OR IGNORE INTO rcs_media_receive(message_id,attachment_index,state,media_type,display_name,plaintext_bytes) VALUES(?,?,?,?,?,?)",
            params![message_id.to_string(), i64::from(item.original_index), state.code(), metadata.media_type, metadata.display_name, to_i64(metadata.plaintext_bytes)?],
        )?;
        // The task row is gateway-local. A replay with the same descriptor is harmless, but a
        // rebound reference or descriptor is refused rather than silently replacing download
        // authority.
        let changed = tx.execute(
            "INSERT INTO rcs_media_receive_local(message_id,attachment_index,source_gateway_id,media_ref,media_type,display_name,plaintext_bytes) VALUES(?,?,?,?,?,?,?) ON CONFLICT(message_id,attachment_index) DO NOTHING",
            params![message_id.to_string(), i64::from(item.original_index), source_gateway_id.to_string(), item.media_ref.as_bytes(), item.media_type, item.display_name, to_i64(item.plaintext_bytes)?],
        )?;
        if changed == 0 {
            let same: bool = tx.query_row(
                "SELECT source_gateway_id=? AND media_ref=? AND media_type=? AND display_name=? AND plaintext_bytes=? FROM rcs_media_receive_local WHERE message_id=? AND attachment_index=?",
                params![source_gateway_id.to_string(), item.media_ref.as_bytes(), item.media_type, item.display_name, to_i64(item.plaintext_bytes)?, message_id.to_string(), i64::from(item.original_index)],
                |row| row.get(0),
            )?;
            if !same {
                return Err(Error::Conflict);
            }
        }
        enqueue_state_checkpoint(tx, ctx, message_id, item.original_index)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(RCS_MEDIA_MIGRATION_54).unwrap();
        conn
    }

    fn command_with(conn: &mut Connection, count: usize) -> (CommandId, Vec<AttachmentId>) {
        let command = CommandId::new();
        let attachments: Vec<AttachmentId> = (0..count).map(|_| AttachmentId::new()).collect();
        let plan = plan_expansion("caption", &attachments).unwrap();
        let tx = conn.transaction().unwrap();
        assert!(materialize(&tx, command, &plan).unwrap());
        tx.commit().unwrap();
        (command, attachments)
    }

    fn state_of(conn: &Connection, command: CommandId, index: u32) -> String {
        conn.query_row(
            "SELECT state FROM rcs_media_send_ledger WHERE command_id=? AND attachment_index=?",
            params![command.to_string(), i64::from(index)],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn ref_exists(conn: &Connection, command: CommandId, index: u32) -> bool {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM rcs_google_media_refs WHERE command_id=? AND attachment_index=?)",
            params![command.to_string(), i64::from(index)],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// Runs one closure inside a transaction and commits it, so each step is atomic.
    fn step<T>(
        conn: &mut Connection,
        f: impl FnOnce(&Transaction<'_>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let tx = conn.transaction().unwrap();
        let value = f(&tx)?;
        tx.commit().unwrap();
        Ok(value)
    }

    fn drive_to_reference_ready(conn: &mut Connection, command: CommandId, index: u32, at: i64) {
        step(conn, |tx| mark_uploading(tx, command, index)).unwrap();
        step(conn, |tx| {
            mark_reference_ready(tx, command, index, "{\"media_id\":\"m\"}", at)
        })
        .unwrap();
    }

    #[test]
    fn issue_media_decodes_the_strict_go_base64_reference() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 1);
        let key = STANDARD.encode([7_u8; 32]);
        step(&mut conn, |tx| mark_uploading(tx, command, 0)).unwrap();
        step(&mut conn, |tx| {
            mark_reference_ready(
                tx,
                command,
                0,
                &format!(r#"{{"media_id":"go-media","key":"{key}","mime":"image/jpeg","size":42,"name":"photo.jpg"}}"#),
                1,
            )
        })
        .unwrap();
        let media = step(&mut conn, |tx| issue_media(tx, command, 0, 2)).unwrap();
        assert_eq!(media.key, vec![7; 32]);
        assert_eq!(media.media_id, "go-media");
    }

    #[test]
    fn expansion_puts_text_on_index_zero_only_and_gives_each_index_a_tmp_id() {
        let attachments = [
            AttachmentId::new(),
            AttachmentId::new(),
            AttachmentId::new(),
        ];
        let plan = plan_expansion("caption", &attachments).unwrap();
        assert_eq!(
            plan.iter().map(|p| p.attachment_index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(plan[0].content, "caption");
        assert!(plan[1..].iter().all(|p| p.content.is_empty()));
        let tmp_ids: std::collections::BTreeSet<_> = plan.iter().map(|p| &p.tmp_id).collect();
        assert_eq!(tmp_ids.len(), 3);
    }

    #[test]
    fn expansion_allows_media_only_and_refuses_empty_oversized_and_long_text() {
        let one = [AttachmentId::new()];
        assert_eq!(plan_expansion("", &one).unwrap()[0].content, "");
        assert!(plan_expansion("x", &[]).is_err());
        let too_many: Vec<_> = (0..=MAX_RCS_MEDIA_ATTACHMENTS)
            .map(|_| AttachmentId::new())
            .collect();
        assert!(plan_expansion("x", &too_many).is_err());
        let long = "x".repeat(MAX_RCS_TEXT_BYTES + 1);
        assert!(plan_expansion(&long, &one).is_err());
    }

    #[test]
    fn materialize_is_idempotent_for_the_same_plan_and_refuses_a_changed_one() {
        let mut conn = ledger();
        let command = CommandId::new();
        let attachments = [AttachmentId::new(), AttachmentId::new()];
        let plan = plan_expansion("hi", &attachments).unwrap();
        {
            let tx = conn.transaction().unwrap();
            assert!(materialize(&tx, command, &plan).unwrap());
            tx.commit().unwrap();
        }
        {
            let tx = conn.transaction().unwrap();
            assert!(!materialize(&tx, command, &plan).unwrap());
            let other = plan_expansion("hi", &[AttachmentId::new(), AttachmentId::new()]).unwrap();
            assert_eq!(
                materialize(&tx, command, &other),
                Err(Error::InvalidRequest("RCS media plan changed"))
            );
        }
        let rows = ledger_rows(&conn, command).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.state == LedgerState::Pending));
    }

    #[test]
    fn tmp_id_dedup_maps_each_google_id_back_to_its_command_and_index() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 2);
        let rows = ledger_rows(&conn, command).unwrap();
        for row in &rows {
            let tmp = row.tmp_id.as_deref().unwrap();
            assert_eq!(
                attachment_for_tmp_id(&conn, tmp).unwrap(),
                Some((command, row.attachment_index))
            );
        }
        assert_eq!(attachment_for_tmp_id(&conn, "unknown").unwrap(), None);
    }

    #[test]
    fn happy_path_runs_every_transition_then_accepts_and_drops_the_reference() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 1);
        drive_to_reference_ready(&mut conn, command, 0, 1_000);
        assert_eq!(state_of(&conn, command, 0), "reference_ready");
        assert_eq!(
            media_ref(&conn, command, 0).unwrap().as_deref(),
            Some("{\"media_id\":\"m\"}")
        );
        assert_eq!(
            step(&mut conn, |tx| issue_permit(tx, command, 0, 1_500)).unwrap(),
            PermitDecision::Issued
        );
        assert_eq!(state_of(&conn, command, 0), "permit_issued");
        step(&mut conn, |tx| record_commit(tx, command, 0)).unwrap();
        assert_eq!(state_of(&conn, command, 0), "committed");
        let report = step(&mut conn, |tx| record_outcome(tx, command, 0, "ok")).unwrap();
        assert_eq!(report.sync, RcsMediaSyncState::Sent);
        assert_eq!(state_of(&conn, command, 0), "accepted");
        assert!(!ref_exists(&conn, command, 0));
        assert_eq!(next_attachment(&conn, command).unwrap(), None);
    }

    #[test]
    fn upload_must_start_from_pending_and_only_the_lowest_open_index_moves() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 2);
        assert_eq!(
            step(&mut conn, |tx| mark_uploading(tx, command, 1)),
            Err(Error::InvalidRequest("RCS media attachment order"))
        );
        step(&mut conn, |tx| mark_uploading(tx, command, 0)).unwrap();
        assert_eq!(
            step(&mut conn, |tx| mark_uploading(tx, command, 0)),
            Err(Error::InvalidRequest("RCS media state"))
        );
    }

    #[test]
    fn reference_must_be_stored_by_an_upload_and_be_bounded() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 1);
        assert_eq!(
            step(&mut conn, |tx| mark_reference_ready(
                tx, command, 0, "{}", 1
            )),
            Err(Error::InvalidRequest("RCS media state"))
        );
        step(&mut conn, |tx| mark_uploading(tx, command, 0)).unwrap();
        assert_eq!(
            step(&mut conn, |tx| mark_reference_ready(tx, command, 0, "", 1)),
            Err(Error::InvalidRequest("RCS media reference"))
        );
        let huge = "x".repeat(MAX_MEDIA_REF_BYTES + 1);
        assert_eq!(
            step(&mut conn, |tx| mark_reference_ready(
                tx, command, 0, &huge, 1
            )),
            Err(Error::InvalidRequest("RCS media reference"))
        );
    }

    #[test]
    fn permit_refuses_a_stale_or_future_reference_and_returns_it_to_pending() {
        for now in [
            1_000 + REFERENCE_MAX_AGE_MS,
            1_000 + REFERENCE_MAX_AGE_MS + 5,
            999,
        ] {
            let mut conn = ledger();
            let (command, _) = command_with(&mut conn, 1);
            drive_to_reference_ready(&mut conn, command, 0, 1_000);
            assert_eq!(
                step(&mut conn, |tx| issue_permit(tx, command, 0, now)).unwrap(),
                PermitDecision::ReuploadRequired,
                "now={now}"
            );
            assert_eq!(state_of(&conn, command, 0), "pending");
            assert!(!ref_exists(&conn, command, 0));
        }
    }

    #[test]
    fn a_reference_just_under_an_hour_still_permits() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 1);
        drive_to_reference_ready(&mut conn, command, 0, 1_000);
        assert_eq!(
            step(&mut conn, |tx| issue_permit(
                tx,
                command,
                0,
                1_000 + REFERENCE_MAX_AGE_MS - 1
            ))
            .unwrap(),
            PermitDecision::Issued
        );
    }

    #[test]
    fn permit_and_commit_follow_their_predecessor_states_only() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 1);
        assert_eq!(
            step(&mut conn, |tx| issue_permit(tx, command, 0, 1)),
            Err(Error::InvalidRequest("RCS media state"))
        );
        assert_eq!(
            step(&mut conn, |tx| record_commit(tx, command, 0)),
            Err(Error::InvalidRequest("RCS media state"))
        );
        drive_to_reference_ready(&mut conn, command, 0, 1);
        assert_eq!(
            step(&mut conn, |tx| record_commit(tx, command, 0)),
            Err(Error::InvalidRequest("RCS media state"))
        );
    }

    #[test]
    fn committed_non_success_is_terminal_outcome_unknown() {
        for code in [
            "rejected",
            "failed",
            "unknown",
            "outcome_unknown",
            "too_large",
        ] {
            let mut conn = ledger();
            let (command, _) = command_with(&mut conn, 2);
            drive_to_reference_ready(&mut conn, command, 0, 1);
            step(&mut conn, |tx| issue_permit(tx, command, 0, 2)).unwrap();
            step(&mut conn, |tx| record_commit(tx, command, 0)).unwrap();
            let report = step(&mut conn, |tx| record_outcome(tx, command, 0, code)).unwrap();
            assert_eq!(report.sync, RcsMediaSyncState::OutcomeUnknown, "{code}");
            assert_eq!(state_of(&conn, command, 0), "outcome_unknown", "{code}");
            assert_eq!(
                next_attachment(&conn, command)
                    .unwrap()
                    .unwrap()
                    .attachment_index,
                1,
                "a later attachment still proceeds after {code}"
            );
        }
    }

    #[test]
    fn permit_without_commit_refuses_known_codes_and_is_unknown_otherwise() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 2);
        drive_to_reference_ready(&mut conn, command, 0, 1);
        step(&mut conn, |tx| issue_permit(tx, command, 0, 2)).unwrap();
        let report = step(&mut conn, |tx| {
            record_outcome(tx, command, 0, "not_allowed_mime")
        })
        .unwrap();
        assert_eq!(report.sync, RcsMediaSyncState::NotSent);
        assert_eq!(report.reason, Some(RcsMediaNotSentReason::NotAllowedMime));
        assert_eq!(state_of(&conn, command, 0), "refused");
        assert_eq!(
            next_attachment(&conn, command)
                .unwrap()
                .unwrap()
                .attachment_index,
            1
        );

        drive_to_reference_ready(&mut conn, command, 1, 3);
        step(&mut conn, |tx| issue_permit(tx, command, 1, 4)).unwrap();
        let report = step(&mut conn, |tx| record_outcome(tx, command, 1, "failed")).unwrap();
        assert_eq!(report.sync, RcsMediaSyncState::OutcomeUnknown);
        assert_eq!(state_of(&conn, command, 1), "outcome_unknown");
    }

    #[test]
    fn permit_stage_refusal_codes_map_to_their_reasons() {
        for (code, reason) in [
            ("too_large", RcsMediaNotSentReason::TooLarge),
            ("rejected", RcsMediaNotSentReason::Rejected),
        ] {
            let mut conn = ledger();
            let (command, _) = command_with(&mut conn, 1);
            drive_to_reference_ready(&mut conn, command, 0, 1);
            step(&mut conn, |tx| issue_permit(tx, command, 0, 2)).unwrap();
            let report = step(&mut conn, |tx| record_outcome(tx, command, 0, code)).unwrap();
            assert_eq!(report.reason, Some(reason));
        }
    }

    #[test]
    fn upload_stage_transient_codes_return_to_pending_and_refusals_are_terminal() {
        for code in ["failed", "busy", "expired", "unknown"] {
            let mut conn = ledger();
            let (command, _) = command_with(&mut conn, 1);
            step(&mut conn, |tx| mark_uploading(tx, command, 0)).unwrap();
            let report = step(&mut conn, |tx| record_outcome(tx, command, 0, code)).unwrap();
            assert_eq!(report.sync, RcsMediaSyncState::Sending, "{code}");
            assert_eq!(state_of(&conn, command, 0), "pending", "{code}");
        }
        for code in ["not_allowed_mime", "too_large", "rejected"] {
            let mut conn = ledger();
            let (command, _) = command_with(&mut conn, 1);
            drive_to_reference_ready(&mut conn, command, 0, 1);
            let report = step(&mut conn, |tx| record_outcome(tx, command, 0, code)).unwrap();
            assert_eq!(report.sync, RcsMediaSyncState::NotSent, "{code}");
            assert_eq!(state_of(&conn, command, 0), "refused", "{code}");
            assert!(!ref_exists(&conn, command, 0), "{code}");
        }
    }

    #[test]
    fn members_changed_refuses_the_current_and_all_later_indexes_as_a_unit() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 4);
        step(&mut conn, |tx| mark_uploading(tx, command, 0)).unwrap();
        let report = step(&mut conn, |tx| {
            record_outcome(tx, command, 0, "members_changed")
        })
        .unwrap();
        assert_eq!(report.sync, RcsMediaSyncState::MembersChanged);
        assert_eq!(report.refused_indexes, vec![1, 2, 3]);
        for index in 0..4 {
            assert_eq!(state_of(&conn, command, index), "refused", "index {index}");
        }
        assert_eq!(next_attachment(&conn, command).unwrap(), None);
    }

    #[test]
    fn members_changed_after_commit_is_unknown_and_still_refuses_the_rest() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 2);
        drive_to_reference_ready(&mut conn, command, 0, 1);
        step(&mut conn, |tx| issue_permit(tx, command, 0, 2)).unwrap();
        step(&mut conn, |tx| record_commit(tx, command, 0)).unwrap();
        let report = step(&mut conn, |tx| {
            record_outcome(tx, command, 0, "members_changed")
        })
        .unwrap();
        assert_eq!(report.sync, RcsMediaSyncState::OutcomeUnknown);
        assert_eq!(report.refused_indexes, vec![1]);
        assert_eq!(state_of(&conn, command, 1), "refused");
    }

    #[test]
    fn outcomes_outside_the_vocabulary_for_their_state_are_rejected() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 1);
        drive_to_reference_ready(&mut conn, command, 0, 1);
        step(&mut conn, |tx| issue_permit(tx, command, 0, 2)).unwrap();
        assert_eq!(
            step(&mut conn, |tx| record_outcome(tx, command, 0, "ok")),
            Err(Error::InvalidRequest("RCS media outcome"))
        );
        assert_eq!(
            step(&mut conn, |tx| record_outcome(tx, command, 0, "made_up")),
            Err(Error::InvalidRequest("RCS media outcome"))
        );
        assert_eq!(state_of(&conn, command, 0), "permit_issued");
    }

    #[test]
    fn third_failed_upload_attempt_refuses_the_attachment_as_failed_and_keeps_the_rest() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 2);
        for attempt in 1..MAX_PRE_PERMIT_UPLOAD_FAILURES {
            step(&mut conn, |tx| mark_uploading(tx, command, 0)).unwrap();
            let report = step(&mut conn, |tx| record_outcome(tx, command, 0, "busy")).unwrap();
            assert_eq!(report.sync, RcsMediaSyncState::Sending, "attempt {attempt}");
            assert_eq!(state_of(&conn, command, 0), "pending", "attempt {attempt}");
        }
        step(&mut conn, |tx| mark_uploading(tx, command, 0)).unwrap();
        let report = step(&mut conn, |tx| record_outcome(tx, command, 0, "failed")).unwrap();
        assert_eq!(report.sync, RcsMediaSyncState::NotSent);
        assert_eq!(report.reason, Some(RcsMediaNotSentReason::Failed));
        assert!(report.refused_indexes.is_empty());
        assert_eq!(state_of(&conn, command, 0), "refused");
        assert!(!ref_exists(&conn, command, 0));
        assert_eq!(state_of(&conn, command, 1), "pending");
        step(&mut conn, |tx| mark_uploading(tx, command, 1)).unwrap();
    }

    #[test]
    fn a_retry_after_a_transient_upload_failure_uses_the_same_tmp_id() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 1);
        let before = ledger_rows(&conn, command).unwrap()[0].tmp_id.clone();
        step(&mut conn, |tx| mark_uploading(tx, command, 0)).unwrap();
        step(&mut conn, |tx| record_outcome(tx, command, 0, "busy")).unwrap();
        assert_eq!(ledger_rows(&conn, command).unwrap()[0].tmp_id, before);
    }

    #[test]
    fn crash_recovery_retries_uploads_and_marks_permits_and_commits_unknown() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 4);
        // 0 uploading, 1 reference_ready, 2 permit_issued, 3 committed (by hand: crash coverage).
        conn.execute_batch(&format!(
            "UPDATE rcs_media_send_ledger SET state='uploading' WHERE command_id='{c}' AND attachment_index=0;
             UPDATE rcs_media_send_ledger SET state='reference_ready',uploaded_at_ms=1 WHERE command_id='{c}' AND attachment_index=1;
             INSERT INTO rcs_google_media_refs(command_id,attachment_index,media_ref) VALUES('{c}',1,x'7b7d');
             UPDATE rcs_media_send_ledger SET state='permit_issued' WHERE command_id='{c}' AND attachment_index=2;
             UPDATE rcs_media_send_ledger SET state='committed' WHERE command_id='{c}' AND attachment_index=3;",
            c = command
        ))
        .unwrap();
        step(&mut conn, recover_interrupted).unwrap();
        assert_eq!(state_of(&conn, command, 0), "pending");
        assert_eq!(state_of(&conn, command, 1), "pending");
        assert!(!ref_exists(&conn, command, 1));
        assert_eq!(state_of(&conn, command, 2), "outcome_unknown");
        assert_eq!(state_of(&conn, command, 3), "outcome_unknown");
        assert_eq!(
            next_attachment(&conn, command)
                .unwrap()
                .unwrap()
                .attachment_index,
            0
        );
    }

    #[test]
    fn crash_recovery_leaves_terminal_rows_and_their_absence_of_references_alone() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 2);
        drive_to_reference_ready(&mut conn, command, 0, 1);
        step(&mut conn, |tx| issue_permit(tx, command, 0, 2)).unwrap();
        step(&mut conn, |tx| record_commit(tx, command, 0)).unwrap();
        step(&mut conn, |tx| record_outcome(tx, command, 0, "ok")).unwrap();
        step(&mut conn, recover_interrupted).unwrap();
        assert_eq!(state_of(&conn, command, 0), "accepted");
        assert_eq!(state_of(&conn, command, 1), "pending");
    }

    #[test]
    fn next_attachment_skips_terminal_rows_in_index_order() {
        let mut conn = ledger();
        let (command, _) = command_with(&mut conn, 3);
        step(&mut conn, |tx| mark_uploading(tx, command, 0)).unwrap();
        step(&mut conn, |tx| {
            record_outcome(tx, command, 0, "not_allowed_mime")
        })
        .unwrap();
        assert_eq!(
            next_attachment(&conn, command)
                .unwrap()
                .unwrap()
                .attachment_index,
            1
        );
    }

    #[test]
    fn media_ref_and_permit_reject_cross_command_lookups() {
        let mut conn = ledger();
        let (first, _) = command_with(&mut conn, 1);
        let (second, _) = command_with(&mut conn, 1);
        drive_to_reference_ready(&mut conn, first, 0, 1);
        assert!(media_ref(&conn, second, 0).unwrap().is_none());
        assert_eq!(
            step(&mut conn, |tx| issue_permit(tx, second, 0, 2)),
            Err(Error::InvalidRequest("RCS media state"))
        );
    }

    #[test]
    fn demand_is_recorded_only_on_the_addressed_gateway() {
        let conn = ledger();
        let gateway = DeviceId::new();
        let other = DeviceId::new();
        let message = MessageId::new();
        let demanded = [0, 2];
        assert_eq!(
            apply_demand(&conn, other, message, gateway, &demanded, false).unwrap(),
            None
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM rcs_media_retry_demand", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(
            apply_demand(&conn, gateway, message, gateway, &demanded, false).unwrap(),
            None
        );
        assert_eq!(
            apply_demand(&conn, gateway, message, gateway, &demanded, false).unwrap(),
            None
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM rcs_media_retry_demand", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn demand_validation_refuses_unsorted_duplicate_and_out_of_range_indexes() {
        let conn = ledger();
        let gateway = DeviceId::new();
        let message = MessageId::new();
        for indexes in [
            vec![],
            vec![2, 1],
            vec![1, 1],
            vec![MAX_RCS_MEDIA_ATTACHMENTS as u32],
        ] {
            assert_eq!(
                apply_demand(&conn, gateway, message, gateway, &indexes, false).unwrap(),
                Some(QuarantineReason::InvalidPayload),
                "{indexes:?}"
            );
        }
    }

    #[test]
    fn demand_from_history_is_ignored() {
        let conn = ledger();
        let gateway = DeviceId::new();
        assert_eq!(
            apply_demand(&conn, gateway, MessageId::new(), gateway, &[0], true).unwrap(),
            None
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM rcs_media_retry_demand", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn payload_kinds_are_known_and_identities_are_latest_per_message_and_index() {
        let message_id = MessageId::new();
        let gateway = DeviceId::new();
        let state = PrivatePayload::RcsMediaState {
            message_id,
            attachment_index: 2,
            state: RcsMediaSyncState::Downloading,
            reason: None,
            attachment: None,
            metadata: None,
        };
        let demand = PrivatePayload::RcsMediaDemand {
            message_id,
            source_gateway_id: gateway,
            attachment_indexes: vec![1, 2],
        };
        assert!(is_known_private_payload_kind("rcs_media_state"));
        assert!(is_known_private_payload_kind("rcs_media_demand"));
        assert_eq!(
            compaction_identity(&state),
            Some(format!("rcs-media-state:{message_id}:2"))
        );
        assert_eq!(
            compaction_identity(&demand),
            Some(format!("rcs-media-demand:{message_id}:{gateway}:1,2"))
        );
    }

    #[test]
    fn sanitized_state_payload_never_names_google_identifiers() {
        let payload = PrivatePayload::RcsMediaState {
            message_id: MessageId::new(),
            attachment_index: 0,
            state: RcsMediaSyncState::NotSent,
            reason: Some(RcsMediaNotSentReason::TooLarge),
            attachment: None,
            metadata: None,
        };
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(value["kind"], "rcs_media_state");
        assert_eq!(value["state"], "not_sent");
        assert_eq!(value["reason"], "too_large");
        assert!(value.get("attachment").is_none());
        let without_reason = PrivatePayload::RcsMediaState {
            message_id: MessageId::new(),
            attachment_index: 0,
            state: RcsMediaSyncState::Sent,
            reason: None,
            attachment: None,
            metadata: None,
        };
        let value = serde_json::to_value(&without_reason).unwrap();
        assert!(value.get("reason").is_none());
    }

    fn client() -> (tempfile::TempDir, Client, DeviceId) {
        let dir = tempfile::TempDir::new().unwrap();
        let device = DeviceId::new();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("rcs-media.db"),
                vault_id: VaultId::new(),
                device_id: device,
            },
            DatabaseKey::new(&[11; 32]).unwrap(),
        )
        .unwrap();
        (dir, client, device)
    }

    fn rcs_message(source: DeviceId, direction: Direction) -> MessagePayload {
        MessagePayload {
            record: MessageRecord {
                message_id: MessageId::new(),
                conversation_id: ConversationId::new(),
                source_sequence: SourceSequence(1),
                attachments: vec![],
            },
            source_device_id: source,
            provider_message_id: Some("rcs-media-test".into()),
            sender_address: Some("+15555550100".into()),
            recipients: vec![],
            subject: None,
            body: "caption".into(),
            transport: Transport::Rcs,
            direction,
            imported: false,
            sent_at_ms: Some(1),
            ordered_at_ms: None,
            mms_context: None,
        }
    }

    fn descriptor(id: AttachmentId) -> MediaDescriptor {
        MediaDescriptor {
            attachment_id: id,
            remote_object_id: uuid::Uuid::new_v4().to_string(),
            media_type: "image/jpeg".into(),
            display_name: "a.jpg".into(),
            plaintext_bytes: 42,
            ciphertext_bytes: 64,
            ciphertext_sha256: "ab".repeat(32),
            stream_version: STREAM_VERSION,
            file_key: [7; 32],
        }
    }

    #[test]
    fn stored_state_attaches_the_hosted_descriptor_like_mms_media() {
        let (_dir, client, _device) = client();
        let gateway = DeviceId::new();
        let message = rcs_message(gateway, Direction::Incoming);
        let message_id = message.record.message_id;
        let store = client.lock().unwrap();
        let conn = &store.conn;
        insert_message(conn, &message).unwrap();
        let id = AttachmentId::new();
        let media = descriptor(id);
        assert_eq!(
            apply_state(
                conn,
                gateway,
                message_id,
                0,
                RcsMediaSyncState::Stored,
                None,
                Some(&media)
            )
            .unwrap(),
            None
        );
        let row = attachment_row(conn, id).unwrap().unwrap();
        assert_eq!(
            row.remote_object_id.as_deref(),
            Some(media.remote_object_id.as_str())
        );
        let stored = load_message(conn, message_id).unwrap().unwrap();
        assert_eq!(stored.record.attachments.len(), 1);
        assert_eq!(stored.record.attachments[0].attachment_id, id);
        let receive: (String, Option<String>) = conn
            .query_row(
                "SELECT state,attachment_id FROM rcs_media_receive WHERE message_id=? AND attachment_index=0",
                [message_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(receive, ("stored".into(), Some(id.to_string())));
    }

    #[test]
    fn sparse_stored_states_preserve_original_indexes_and_cannot_rebind_an_attachment() {
        let (_dir, client, _device) = client();
        let gateway = DeviceId::new();
        let message = rcs_message(gateway, Direction::Incoming);
        let message_id = message.record.message_id;
        let store = client.lock().unwrap();
        let conn = &store.conn;
        insert_message(conn, &message).unwrap();
        assert_eq!(
            apply_state(
                conn,
                gateway,
                message_id,
                0,
                RcsMediaSyncState::TooLarge,
                None,
                None,
            )
            .unwrap(),
            None
        );
        let out_of_order = descriptor(AttachmentId::new());
        assert_eq!(
            apply_state(
                conn,
                gateway,
                message_id,
                1,
                RcsMediaSyncState::Stored,
                None,
                Some(&out_of_order)
            )
            .unwrap(),
            None
        );
        let position: i64 = conn
            .query_row(
                "SELECT position FROM message_attachments WHERE message_id=? AND attachment_id=?",
                params![
                    message_id.to_string(),
                    out_of_order.attachment_id.to_string()
                ],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(position, 1);
        let first = descriptor(AttachmentId::new());
        assert_eq!(
            apply_state(
                conn,
                gateway,
                message_id,
                0,
                RcsMediaSyncState::Stored,
                None,
                Some(&first)
            )
            .unwrap(),
            None
        );
        let rebind = descriptor(AttachmentId::new());
        assert_eq!(
            apply_state(
                conn,
                gateway,
                message_id,
                0,
                RcsMediaSyncState::Stored,
                None,
                Some(&rebind)
            )
            .unwrap(),
            Some(QuarantineReason::PayloadConflict)
        );
    }

    #[test]
    fn exhausted_local_receive_budget_defers_without_claiming_hosted_quota_full() {
        let (_dir, client, device) = client();
        let message_id = MessageId::new();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        conn.execute(
            "INSERT INTO rcs_media_receive(message_id,attachment_index,state) VALUES(?,?,?)",
            params![message_id.to_string(), 0_i64, "downloading"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rcs_media_receive_local(message_id,attachment_index,source_gateway_id,media_ref,media_type,display_name,plaintext_bytes,admission_bytes) VALUES(?,?,?,?,?,?,?,?)",
            params![
                message_id.to_string(),
                0_i64,
                device.to_string(),
                b"local-only-ref".as_slice(),
                "image/jpeg",
                "a.jpg",
                42_i64,
                to_i64(RCS_MEDIA_RECEIVE_LOCAL_BUDGET_BYTES).unwrap(),
            ],
        )
        .unwrap();

        conn.execute("INSERT INTO rcs_media_receive(message_id,attachment_index,state) VALUES(?,1,'downloading')", [message_id.to_string()]).unwrap();
        conn.execute("INSERT INTO rcs_media_receive_local(message_id,attachment_index,source_gateway_id,media_ref,media_type,display_name,plaintext_bytes) VALUES(?,1,?,X'00','image/jpeg','new.jpg',42)",
            params![message_id.to_string(), device.to_string()]).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        assert_eq!(
            admit_receive_download(
                &tx,
                device,
                message_id,
                1,
                Some(MAX_ATTACHMENT_CIPHERTEXT_BYTES),
            )
            .unwrap(),
            ReceiveDownloadAdmission::CapacityUnavailable,
        );
    }

    #[test]
    fn pending_receive_tasks_include_prepared_retryable_work_for_finalization_after_restart() {
        let (_dir, client, device) = client();
        let message_id = MessageId::new();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        conn.execute(
            "INSERT INTO rcs_media_receive(message_id,attachment_index,state) VALUES(?,?,?)",
            params![message_id.to_string(), 0_i64, "failed"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rcs_media_receive_local(message_id,attachment_index,source_gateway_id,media_ref,media_type,display_name,plaintext_bytes,prepared_attachment_id) VALUES(?,?,?,?,?,?,?,?)",
            params![
                message_id.to_string(),
                0_i64,
                device.to_string(),
                b"local-only-ref".as_slice(),
                "image/jpeg",
                "a.jpg",
                42_i64,
                AttachmentId::new().to_string(),
            ],
        )
        .unwrap();

        let tasks = pending_receive_tasks(conn, device).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].message_id, message_id);
        assert_eq!(tasks[0].state, RcsMediaSyncState::Failed);
    }

    #[test]
    fn stale_receive_failure_cannot_regress_a_stored_descriptor() {
        let (_dir, client, _device) = client();
        let gateway = DeviceId::new();
        let message = rcs_message(gateway, Direction::Incoming);
        let message_id = message.record.message_id;
        let store = client.lock().unwrap();
        let conn = &store.conn;
        insert_message(conn, &message).unwrap();
        let stored = descriptor(AttachmentId::new());
        assert_eq!(
            apply_state(
                conn,
                gateway,
                message_id,
                0,
                RcsMediaSyncState::Stored,
                None,
                Some(&stored),
            )
            .unwrap(),
            None,
        );

        assert_eq!(
            apply_state(
                conn,
                gateway,
                message_id,
                0,
                RcsMediaSyncState::Failed,
                None,
                None,
            )
            .unwrap(),
            None,
        );
        let state: String = conn
            .query_row(
                "SELECT state FROM rcs_media_receive WHERE message_id=? AND attachment_index=0",
                [message_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, RcsMediaSyncState::Stored.code());
    }

    #[test]
    fn stored_receive_objects_release_the_pending_ciphertext_budget() {
        let (_dir, client, device) = client();
        let mut store = client.lock().unwrap();
        let tx = store.conn.transaction().unwrap();
        let message = MessageId::new();
        let media = descriptor(AttachmentId::new());
        assert!(store_remote_media(&tx, std::slice::from_ref(&media)).unwrap());
        tx.execute("INSERT INTO rcs_media_receive(message_id,attachment_index,state,attachment_id) VALUES(?,0,'failed',?)",
            params![message.to_string(), media.attachment_id.to_string()]).unwrap();
        tx.execute("INSERT INTO rcs_media_receive_local(message_id,attachment_index,source_gateway_id,media_ref,media_type,display_name,plaintext_bytes,prepared_attachment_id) VALUES(?,0,?,X'00','image/png','image.png',1,?)",
            params![message.to_string(), device.to_string(), media.attachment_id.to_string()]).unwrap();
        assert_eq!(local_receive_usage(&tx).unwrap(), media.ciphertext_bytes);
        tx.execute(
            "UPDATE rcs_media_receive SET state='stored' WHERE message_id=?",
            [message.to_string()],
        )
        .unwrap();
        assert_eq!(
            local_receive_usage(&tx).unwrap(),
            0,
            "hosted Stored objects are not pending receive work"
        );
    }

    #[test]
    fn receive_states_have_their_attachment_shape_and_authority() {
        let (_dir, client, _device) = client();
        let gateway = DeviceId::new();
        let message = rcs_message(gateway, Direction::Incoming);
        let message_id = message.record.message_id;
        let store = client.lock().unwrap();
        let conn = &store.conn;
        insert_message(conn, &message).unwrap();
        let media = descriptor(AttachmentId::new());
        let invalid = [
            // Stored without its descriptor.
            (RcsMediaSyncState::Stored, None, None, gateway),
            // A descriptor on a non-stored state.
            (RcsMediaSyncState::Downloading, Some(&media), None, gateway),
            // Outgoing state on an incoming message.
            (RcsMediaSyncState::Sent, None, None, gateway),
            // Not-sent without a reason.
            (RcsMediaSyncState::NotSent, None, None, gateway),
            // Wrong producer.
            (RcsMediaSyncState::Downloading, None, None, DeviceId::new()),
        ];
        for (state, attachment, reason, producer) in invalid {
            assert_eq!(
                apply_state(conn, producer, message_id, 0, state, reason, attachment).unwrap(),
                Some(QuarantineReason::InvalidPayload),
                "{state:?}"
            );
        }
        assert_eq!(
            apply_state(
                conn,
                gateway,
                message_id,
                0,
                RcsMediaSyncState::TooLarge,
                None,
                None
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn outgoing_status_is_recorded_per_attachment_and_refuses_receive_states() {
        let (_dir, client, device) = client();
        let mut message = rcs_message(device, Direction::Outgoing);
        let attachment_id = AttachmentId::new();
        message.record.attachments.push(AttachmentReference {
            attachment_id,
            pending: true,
        });
        let message_id = message.record.message_id;
        let store = client.lock().unwrap();
        let conn = &store.conn;
        insert_message(conn, &message).unwrap();
        let command_id = CommandId::new();
        let payload = PrivatePayload::SendCommand {
            message: message.clone(),
            expected_member_revision: None,
            group_create: None,
        };
        conn.execute(
            "INSERT INTO command_ledger(command_id,key_epoch,subscription_id,payload,historical) VALUES(?,1,'sim',?,0)",
            params![command_id.to_string(), json(&payload).unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO rcs_media_send_ledger(command_id,attachment_index,attachment_id,state,tmp_id) VALUES(?,0,?,'pending','tmp')",
            params![command_id.to_string(), attachment_id.to_string()],
        )
        .unwrap();
        record_command(conn, command_id, message_id, device).unwrap();
        assert_eq!(
            apply_state(
                conn,
                device,
                message_id,
                0,
                RcsMediaSyncState::Sending,
                None,
                None
            )
            .unwrap(),
            None
        );
        assert_eq!(
            apply_state(
                conn,
                device,
                message_id,
                0,
                RcsMediaSyncState::NotSent,
                Some(RcsMediaNotSentReason::NotAllowedMime),
                None
            )
            .unwrap(),
            None
        );
        let (state, reason): (String, Option<String>) = conn
            .query_row(
                "SELECT state,reason FROM rcs_media_send_status WHERE message_id=? AND attachment_index=0",
                [message_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (state.as_str(), reason.as_deref()),
            ("not_sent", Some("not_allowed_mime"))
        );
        assert_eq!(
            apply_state(
                conn,
                device,
                message_id,
                0,
                RcsMediaSyncState::Sending,
                None,
                None
            )
            .unwrap(),
            None
        );
        let attacker = DeviceId::new();
        assert_eq!(
            apply_state(
                conn,
                attacker,
                message_id,
                0,
                RcsMediaSyncState::Sent,
                None,
                None
            )
            .unwrap(),
            Some(QuarantineReason::InvalidPayload)
        );
        assert_eq!(
            apply_state(
                conn,
                device,
                message_id,
                0,
                RcsMediaSyncState::Downloading,
                None,
                None
            )
            .unwrap(),
            Some(QuarantineReason::InvalidPayload)
        );
    }

    #[test]
    fn outgoing_checkpoint_repeats_terminal_projection_instead_of_stale_sending() {
        let (_dir, client, device) = client();
        let mut message = rcs_message(device, Direction::Outgoing);
        message.record.attachments.push(AttachmentReference {
            attachment_id: AttachmentId::new(),
            pending: true,
        });
        let command = CommandId::new();
        let mut store = client.lock().unwrap();
        let (conn, ctx) = store.parts();
        insert_message(conn, &message).unwrap();
        record_command(conn, command, message.record.message_id, device).unwrap();
        emit_rcs_media_state(conn, &ctx, command, 0, RcsMediaSyncState::Sent, None).unwrap();
        emit_rcs_media_state(conn, &ctx, command, 0, RcsMediaSyncState::Sending, None).unwrap();
        let plain: Vec<u8> = conn
            .query_row(
                "SELECT plain FROM outbox ORDER BY seq DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let payload: PrivatePayload = decode(&plain).unwrap();
        assert!(matches!(
            payload,
            PrivatePayload::RcsMediaState {
                state: RcsMediaSyncState::Sent,
                ..
            }
        ));
    }

    #[test]
    fn state_for_an_unknown_message_is_quarantined() {
        let (_dir, client, device) = client();
        let store = client.lock().unwrap();
        assert_eq!(
            apply_state(
                &store.conn,
                device,
                MessageId::new(),
                0,
                RcsMediaSyncState::Downloading,
                None,
                None
            )
            .unwrap(),
            Some(QuarantineReason::InvalidPayload)
        );
    }

    #[test]
    fn deferred_stored_evidence_survives_newer_failure_in_both_arrival_orders() {
        for stored_first in [true, false] {
            let (_dir, client, _device) = client();
            let gateway = DeviceId::new();
            let message = rcs_message(gateway, Direction::Incoming);
            let message_id = message.record.message_id;
            let stored = descriptor(AttachmentId::new());
            let store = client.lock().unwrap();
            let conn = &store.conn;
            let stored_event = || {
                apply_authenticated_state(
                    conn,
                    gateway,
                    1,
                    message_id,
                    1,
                    RcsMediaSyncState::Stored,
                    None,
                    StateContent {
                        attachment: Some(&stored),
                        metadata: None,
                    },
                )
                .unwrap()
            };
            let failed_event = || {
                apply_authenticated_state(
                    conn,
                    gateway,
                    2,
                    message_id,
                    1,
                    RcsMediaSyncState::Failed,
                    None,
                    StateContent {
                        attachment: None,
                        metadata: None,
                    },
                )
                .unwrap()
            };
            if stored_first {
                assert_eq!(stored_event(), None);
                assert_eq!(failed_event(), None);
            } else {
                assert_eq!(failed_event(), None);
                assert_eq!(stored_event(), None);
            }
            insert_message(conn, &message).unwrap();
            reconcile_staged_states(conn, message_id).unwrap();
            let row: (String, Option<String>) = conn
                .query_row(
                    "SELECT state,attachment_id FROM rcs_media_receive WHERE message_id=? AND attachment_index=1",
                    [message_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(
                row,
                (
                    RcsMediaSyncState::Stored.code().into(),
                    Some(stored.attachment_id.to_string())
                )
            );
        }
    }

    #[test]
    fn conflicting_deferred_descriptor_isolated_from_parent_projection() {
        let (_dir, client, _device) = client();
        let gateway = DeviceId::new();
        let message = rcs_message(gateway, Direction::Incoming);
        let message_id = message.record.message_id;
        let id = AttachmentId::new();
        let established = descriptor(id);
        let mut conflict = established.clone();
        conflict.remote_object_id = uuid::Uuid::new_v4().to_string();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        assert!(store_remote_media(conn, std::slice::from_ref(&established)).unwrap());
        assert_eq!(
            apply_authenticated_state(
                conn,
                gateway,
                1,
                message_id,
                0,
                RcsMediaSyncState::Stored,
                None,
                StateContent {
                    attachment: Some(&conflict),
                    metadata: None,
                },
            )
            .unwrap(),
            None
        );
        assert_eq!(insert_message(conn, &message).unwrap(), Inserted::New);
        reconcile_staged_states(conn, message_id).unwrap();
        assert!(load_message(conn, message_id).unwrap().is_some());
        assert!(
            conn.query_row(
                "SELECT 1 FROM rcs_media_receive WHERE message_id=?",
                [message_id.to_string()],
                |_| Ok(()),
            )
            .optional()
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn pending_state_capacity_allows_existing_keys_and_rejects_new_ones() {
        let (_dir, client, _device) = client();
        let producer = DeviceId::new();
        let parent = rcs_message(DeviceId::new(), Direction::Incoming);
        let existing_message = parent.record.message_id;
        let new_message = MessageId::new();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        conn.execute(
            "INSERT INTO pending_rcs_media_states(producer_device_id,message_id,attachment_index,nonterminal_sequence,nonterminal_state) VALUES(?,?,0,1,'downloading')",
            params![producer.to_string(), existing_message.to_string()],
        )
        .unwrap();
        conn.execute(
            "WITH RECURSIVE rows(value) AS (SELECT 1 UNION ALL SELECT value+1 FROM rows WHERE value<?) INSERT INTO pending_rcs_media_states(producer_device_id,message_id,attachment_index) SELECT ?,printf('dummy-%d',value),0 FROM rows",
            params![PENDING_RCS_MEDIA_STATE_BUDGET - 1, producer.to_string()],
        )
        .unwrap();
        assert_eq!(
            apply_authenticated_state(
                conn,
                producer,
                2,
                existing_message,
                0,
                RcsMediaSyncState::Failed,
                None,
                StateContent {
                    attachment: None,
                    metadata: None
                },
            )
            .unwrap(),
            None
        );
        assert_eq!(
            apply_authenticated_state(
                conn,
                producer,
                1,
                new_message,
                0,
                RcsMediaSyncState::Failed,
                None,
                StateContent {
                    attachment: None,
                    metadata: None
                },
            )
            .unwrap(),
            Some(QuarantineReason::InvalidPayload)
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM pending_rcs_media_states", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, PENDING_RCS_MEDIA_STATE_BUDGET);
        let clocks: i64 = conn
            .query_row("SELECT COUNT(*) FROM rcs_media_state_clocks", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(clocks, 0);
        assert_eq!(insert_message(conn, &parent).unwrap(), Inserted::New);
        reconcile_staged_states(conn, existing_message).unwrap();
        assert_eq!(
            apply_authenticated_state(
                conn,
                producer,
                1,
                new_message,
                0,
                RcsMediaSyncState::Failed,
                None,
                StateContent {
                    attachment: None,
                    metadata: None
                },
            )
            .unwrap(),
            None,
            "discarding the denied producer candidate frees one global slot"
        );
    }

    #[test]
    fn outgoing_state_waits_for_command_authority_and_terminal_does_not_regress() {
        let (_dir, client, _device) = client();
        let composer = DeviceId::new();
        let gateway = DeviceId::new();
        let attacker = DeviceId::new();
        let mut message = rcs_message(composer, Direction::Outgoing);
        message.record.attachments.push(AttachmentReference {
            attachment_id: AttachmentId::new(),
            pending: true,
        });
        let message_id = message.record.message_id;
        let store = client.lock().unwrap();
        let conn = &store.conn;
        for producer in [attacker, gateway] {
            assert_eq!(
                apply_authenticated_state(
                    conn,
                    producer,
                    2,
                    message_id,
                    0,
                    RcsMediaSyncState::Sent,
                    None,
                    StateContent {
                        attachment: None,
                        metadata: None
                    },
                )
                .unwrap(),
                None
            );
        }
        assert_eq!(insert_message(conn, &message).unwrap(), Inserted::New);
        reconcile_staged_states(conn, message_id).unwrap();
        assert_eq!(insert_message(conn, &message).unwrap(), Inserted::Same);
        reconcile_staged_states(conn, message_id).unwrap();
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pending_rcs_media_states WHERE message_id=?",
                [message_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            pending, 2,
            "a parent alone does not establish outgoing authority"
        );
        record_command(conn, CommandId::new(), message_id, gateway).unwrap();
        reconcile_staged_states(conn, message_id).unwrap();
        assert_eq!(
            apply_authenticated_state(
                conn,
                gateway,
                1,
                message_id,
                0,
                RcsMediaSyncState::Sending,
                None,
                StateContent {
                    attachment: None,
                    metadata: None
                },
            )
            .unwrap(),
            None
        );
        let state: String = conn
            .query_row(
                "SELECT state FROM rcs_media_send_status WHERE message_id=? AND attachment_index=0",
                [message_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, RcsMediaSyncState::Sent.code());
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pending_rcs_media_states WHERE message_id=?",
                [message_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            pending, 0,
            "authorized and denied candidates are both reconciled"
        );
    }
}
