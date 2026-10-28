use crate::{
    AttachmentId, Captured, Client, ConversationId, Direction, Error, Inserted, MessageId,
    MessagePayload, MessageRecord, PrivatePayload, SourceSequence, Transport,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_MMS_ACQUISITIONS: usize = 1_000;
pub const MAX_PENDING_MMS_MEDIA_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MmsSource {
    pub source_generation: String,
    pub subscription_id: String,
    pub provider_message_id: String,
    pub provider_thread_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MmsAcquisitionInput {
    pub source: MmsSource,
    pub direction: Direction,
    pub sender_address: Option<String>,
    pub recipients: Vec<String>,
    pub subject: Option<String>,
    pub body: String,
    pub imported: bool,
    pub observed_at_ms: i64,
    /// Provider PDU transaction identity. Peppy sends use `peppy-<canonical command UUID>`.
    #[serde(default)]
    pub transaction_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MmsContext {
    pub source_generation: String,
    pub subscription_id: String,
    pub provider_thread_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MmsAcquisitionState {
    Pending,
    Blocked,
    Unavailable,
    Complete,
}
impl MmsAcquisitionState {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Blocked => "blocked",
            Self::Unavailable => "unavailable",
            Self::Complete => "complete",
        }
    }
    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "blocked" => Self::Blocked,
            "unavailable" => Self::Unavailable,
            "complete" => Self::Complete,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MmsAcquisition {
    pub acquisition_id: String,
    pub conversation_id: ConversationId,
    pub input: MmsAcquisitionInput,
    pub state: MmsAcquisitionState,
    pub reason: Option<String>,
    pub attachment_ids: Vec<AttachmentId>,
}

/// The durable result of resolving a provider row. Quarantine is non-fatal: the host may advance
/// its scan checkpoint because core retained the ambiguous evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MmsBeginOutcome {
    Acquired(MmsAcquisition),
    Quarantined(MmsProviderIdentityQuarantine),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MmsProviderIdentityQuarantine {
    pub source: MmsSource,
    pub direction: Direction,
    pub reason: String,
}

/// A durable provider-part identity, used to reuse an already prepared local attachment on retry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MmsAcquisitionPart {
    pub provider_part_id: String,
    pub attachment_id: AttachmentId,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub thumbnail_attachment_id: Option<AttachmentId>,
    pub preview_state: MmsPreviewState,
    pub metadata_recorded: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MmsPreviewState {
    Pending,
    Deferred,
    PermanentlyUnavailable,
    Ready,
}

impl MmsPreviewState {
    fn code(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Deferred => "deferred",
            Self::PermanentlyUnavailable => "permanently_unavailable",
            Self::Ready => "ready",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "deferred" => Self::Deferred,
            "permanently_unavailable" => Self::PermanentlyUnavailable,
            "ready" => Self::Ready,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MmsReplyContext {
    pub recipients: Vec<String>,
    pub subject: Option<String>,
}

impl Client {
    /// Reserves a local, source-scoped MMS acquisition. It remains local until completed.
    pub fn begin_mms_acquisition(
        &self,
        input: MmsAcquisitionInput,
    ) -> Result<MmsAcquisition, Error> {
        match self.begin_mms_acquisition_outcome(input)? {
            MmsBeginOutcome::Acquired(acquisition) => Ok(acquisition),
            // Preserve the established exceptional surface for callers that have not adopted the
            // additive outcome API. Native scanners use the typed outcome instead.
            MmsBeginOutcome::Quarantined(_) => Err(Error::Conflict),
        }
    }

    pub fn begin_mms_acquisition_outcome(
        &self,
        input: MmsAcquisitionInput,
    ) -> Result<MmsBeginOutcome, Error> {
        self.begin_mms_acquisition_resolved(input)
    }

    fn begin_mms_acquisition_resolved(
        &self,
        input: MmsAcquisitionInput,
    ) -> Result<MmsBeginOutcome, Error> {
        let (input, unavailable_reason) = normalize_pending_input(input)?;
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior = acquisitions_by_provider_source(&tx, &input.source, input.direction)?;
        let completed_ambiguity = prior.iter().any(|acquisition| {
            acquisition.state == MmsAcquisitionState::Complete
                && !same_provider_identity(&acquisition.input, &input)
                && !strongly_distinct_provider_instance(&acquisition.input, &input)
        });
        if completed_ambiguity {
            let ambiguous_pending = prior.iter().filter(|acquisition| {
                acquisition.state == MmsAcquisitionState::Pending
                    && !strongly_distinct_provider_instance(&acquisition.input, &input)
            });
            let detached = retire_provider_identity_acquisitions(
                &tx,
                ambiguous_pending,
                "provider identity is ambiguous",
            )?;
            let quarantine =
                provider_identity_quarantine(&tx, &input, "provider identity is ambiguous")?;
            prune_terminal_acquisitions(&tx)?;
            tx.commit()?;
            drop(guard);
            discard_detached(self, detached)?;
            return Ok(MmsBeginOutcome::Quarantined(quarantine));
        }
        if let Some(completed) = prior.iter().find(|acquisition| {
            acquisition.state == MmsAcquisitionState::Complete
                && same_provider_identity(&acquisition.input, &input)
        }) {
            tx.commit()?;
            return Ok(MmsBeginOutcome::Acquired(completed.clone()));
        }

        let exact = prior.iter().find(|acquisition| {
            acquisition.state == MmsAcquisitionState::Pending
                && same_provider_identity(&acquisition.input, &input)
        });
        let compatible = prior
            .iter()
            .filter(|acquisition| {
                acquisition.state == MmsAcquisitionState::Pending
                    && !strongly_distinct_provider_instance(&acquisition.input, &input)
            })
            .collect::<Vec<_>>();
        let terminal = prior
            .iter()
            .filter(|acquisition| {
                is_terminal_acquisition_state(acquisition.state)
                    && !strongly_distinct_provider_instance(&acquisition.input, &input)
            })
            .collect::<Vec<_>>();
        if exact.is_none() && compatible.len() + terminal.len() > 1 {
            let detached = retire_provider_identity_acquisitions(
                &tx,
                compatible.iter().copied(),
                "provider identity is ambiguous",
            )?;
            let quarantine =
                provider_identity_quarantine(&tx, &input, "provider identity is ambiguous")?;
            prune_terminal_acquisitions(&tx)?;
            tx.commit()?;
            drop(guard);
            discard_detached(self, detached)?;
            return Ok(MmsBeginOutcome::Quarantined(quarantine));
        }
        let existing = exact
            .cloned()
            .or_else(|| compatible.first().map(|row| (*row).clone()));
        if existing.is_none() {
            if terminal.len() == 1
                && same_provider_identity(&terminal[0].input, &input)
                && same_terminal_provider_input(&terminal[0].input, &input)
            {
                tx.commit()?;
                return Ok(MmsBeginOutcome::Acquired(terminal[0].clone()));
            }
            if !terminal.is_empty() {
                let quarantine =
                    provider_identity_quarantine(&tx, &input, "provider identity is ambiguous")?;
                tx.commit()?;
                return Ok(MmsBeginOutcome::Quarantined(quarantine));
            }
        }
        let mut retired_detached = Vec::new();
        if existing.is_none() {
            let stale = prior.iter().filter(|acquisition| {
                acquisition.state == MmsAcquisitionState::Pending
                    && strongly_distinct_provider_instance(&acquisition.input, &input)
            });
            retired_detached =
                retire_provider_identity_acquisitions(&tx, stale, "provider identity superseded")?;
        }

        let reconciliation = if unavailable_reason.is_none() {
            reconcile_outgoing_provider_send(&tx, ctx.device_id, &input)?
        } else {
            Reconciliation::OriginalProviderMessage
        };
        if let Reconciliation::Matched {
            message_id,
            conversation_id,
            source_sequence,
        } = reconciliation
        {
            let mut stored_input = input;
            let (acquisition_id, old_conversation) = if let Some(existing) = existing.as_ref() {
                validate_thread_update(&existing.input.source, &stored_input.source)?;
                stored_input.imported = existing.input.imported;
                stored_input.source.provider_thread_id =
                    existing.input.source.provider_thread_id.clone();
                (
                    existing.acquisition_id.clone(),
                    Some(existing.conversation_id),
                )
            } else {
                (uuid::Uuid::new_v4().to_string(), None)
            };
            link_reconciled_thread(
                &tx,
                &ctx.device_id.to_string(),
                &stored_input.source,
                conversation_id,
                old_conversation,
                &acquisition_id,
                ctx.device_id,
            )?;
            let detached = if existing.is_some() {
                unlink_acquisition_parts(&tx, &acquisition_id)?
            } else {
                Vec::new()
            };
            if existing.is_some() {
                tx.execute(
                    "UPDATE mms_acquisitions SET conversation_id=?,input=?,source_instance_id=?,state='complete',reason=NULL,message_id=?,source_sequence=? WHERE acquisition_id=?",
                    params![
                        conversation_id.to_string(),
                        super::json(&stored_input)?,
                        source_instance_id(&stored_input),
                        message_id.to_string(),
                        super::to_i64(source_sequence.0)?,
                        acquisition_id,
                    ],
                )?;
            } else {
                let order = next_acquisition_order(&tx)?;
                tx.execute(
                    "INSERT INTO mms_acquisitions(acquisition_id,source_generation,subscription_id,provider_message_id,source_instance_id,direction,conversation_id,input,state,reason,message_id,source_sequence,local_order) VALUES(?,?,?,?,?,?,? ,?,'complete',NULL,?,?,?)",
                    params![
                        acquisition_id,
                        stored_input.source.source_generation,
                        stored_input.source.subscription_id,
                        stored_input.source.provider_message_id,
                        source_instance_id(&stored_input),
                        direction_code(stored_input.direction),
                        conversation_id.to_string(),
                        super::json(&stored_input)?,
                        message_id.to_string(),
                        super::to_i64(source_sequence.0)?,
                        order,
                    ],
                )?;
            }
            prune_terminal_acquisitions(&tx)?;
            tx.commit()?;
            drop(guard);
            retired_detached.extend(detached);
            discard_detached(self, retired_detached)?;
            return Ok(MmsBeginOutcome::Acquired(MmsAcquisition {
                acquisition_id,
                conversation_id,
                input: stored_input,
                state: MmsAcquisitionState::Complete,
                reason: None,
                attachment_ids: Vec::new(),
            }));
        }

        if reconciliation == Reconciliation::Ambiguous {
            const REASON: &str = "transaction id reconciliation unavailable";
            let mut stored_input = input;
            let (acquisition_id, conversation_id, message_id, source_sequence) =
                if let Some(existing) = existing.as_ref() {
                    validate_thread_update(&existing.input.source, &stored_input.source)?;
                    stored_input.imported = existing.input.imported;
                    stored_input.source.provider_thread_id =
                        existing.input.source.provider_thread_id.clone();
                    let (message_id, source_sequence) =
                        acquisition_identity(&tx, &existing.acquisition_id)?;
                    (
                        existing.acquisition_id.clone(),
                        existing.conversation_id,
                        message_id,
                        source_sequence,
                    )
                } else {
                    let conversation_id = ConversationId::new();
                    (
                        uuid::Uuid::new_v4().to_string(),
                        conversation_id,
                        MessageId::new(),
                        next_acquisition_sequence(&tx, conversation_id, ctx.device_id)?,
                    )
                };
            let detached = if existing.is_some() {
                unlink_acquisition_parts(&tx, &acquisition_id)?
            } else {
                Vec::new()
            };
            if existing.is_some() {
                tx.execute(
                    "UPDATE mms_acquisitions SET input=?,source_instance_id=?,state='unavailable',reason=? WHERE acquisition_id=?",
                    params![
                        super::json(&stored_input)?,
                        source_instance_id(&stored_input),
                        REASON,
                        acquisition_id
                    ],
                )?;
            } else {
                let order = next_acquisition_order(&tx)?;
                tx.execute(
                "INSERT INTO mms_acquisitions(acquisition_id,source_generation,subscription_id,provider_message_id,source_instance_id,direction,conversation_id,input,state,reason,message_id,source_sequence,local_order) VALUES(?,?,?,?,?,?,? ,?,'unavailable',?,?,?,?)",
                    params![
                        acquisition_id,
                        stored_input.source.source_generation,
                        stored_input.source.subscription_id,
                        stored_input.source.provider_message_id,
                        source_instance_id(&stored_input),
                        direction_code(stored_input.direction),
                        conversation_id.to_string(),
                        super::json(&stored_input)?,
                        REASON,
                        message_id.to_string(),
                        super::to_i64(source_sequence.0)?,
                        order,
                    ],
                )?;
            }
            prune_terminal_acquisitions(&tx)?;
            tx.commit()?;
            drop(guard);
            retired_detached.extend(detached);
            discard_detached(self, retired_detached)?;
            return Ok(MmsBeginOutcome::Acquired(MmsAcquisition {
                acquisition_id,
                conversation_id,
                input: stored_input,
                state: MmsAcquisitionState::Unavailable,
                reason: Some(REASON.into()),
                attachment_ids: Vec::new(),
            }));
        }

        if let Some(mut acquisition) = existing {
            let mut stored_input = input;
            validate_thread_update(&acquisition.input.source, &stored_input.source)?;
            stored_input.imported = acquisition.input.imported;
            stored_input.source.provider_thread_id =
                acquisition.input.source.provider_thread_id.clone();
            let state = if unavailable_reason.is_some() {
                MmsAcquisitionState::Unavailable
            } else {
                acquisition.state
            };
            let reason = if let Some(reason) = unavailable_reason {
                Some(reason.to_owned())
            } else {
                acquisition.reason.clone()
            };
            let detached = if state == MmsAcquisitionState::Unavailable {
                unlink_acquisition_parts(&tx, &acquisition.acquisition_id)?
            } else {
                Vec::new()
            };
            tx.execute(
                "UPDATE mms_acquisitions SET input=?,source_instance_id=?,state=?,reason=? WHERE acquisition_id=?",
                params![
                    super::json(&stored_input)?,
                    source_instance_id(&stored_input),
                    state.code(),
                    reason,
                    acquisition.acquisition_id
                ],
            )?;
            prune_terminal_acquisitions(&tx)?;
            tx.commit()?;
            acquisition.input = stored_input;
            acquisition.state = state;
            acquisition.reason = reason;
            if !detached.is_empty() {
                acquisition.attachment_ids.clear();
            }
            drop(guard);
            retired_detached.extend(detached);
            discard_detached(self, retired_detached)?;
            return Ok(MmsBeginOutcome::Acquired(acquisition));
        }

        let state = if unavailable_reason.is_some() {
            MmsAcquisitionState::Unavailable
        } else {
            MmsAcquisitionState::Pending
        };
        if matches!(
            state,
            MmsAcquisitionState::Pending | MmsAcquisitionState::Blocked
        ) && pending_acquisition_capacity_reached(&tx)?
        {
            return Err(Error::MmsAcquisitionLimit);
        }
        let mapped = mapped_thread_conversation(&tx, &ctx.device_id.to_string(), &input.source)?;
        let conversation_id = if let Some(mapped) = mapped {
            mapped
        } else {
            let chosen = conversation_for_mms_participants(&tx, ctx.device_id, &input)?
                .unwrap_or_else(ConversationId::new);
            link_thread_conversation(&tx, &ctx.device_id.to_string(), &input.source, chosen)?;
            chosen
        };
        let source_sequence = next_acquisition_sequence(&tx, conversation_id, ctx.device_id)?;
        let acquisition = MmsAcquisition {
            acquisition_id: uuid::Uuid::new_v4().to_string(),
            conversation_id,
            input,
            state,
            reason: unavailable_reason.map(str::to_owned),
            attachment_ids: Vec::new(),
        };
        tx.execute(
            "INSERT INTO mms_acquisitions(acquisition_id,source_generation,subscription_id,provider_message_id,source_instance_id,direction,conversation_id,input,state,reason,message_id,source_sequence,local_order) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                acquisition.acquisition_id,
                acquisition.input.source.source_generation,
                acquisition.input.source.subscription_id,
                acquisition.input.source.provider_message_id,
                source_instance_id(&acquisition.input),
                direction_code(acquisition.input.direction),
                acquisition.conversation_id.to_string(),
                super::json(&acquisition.input)?,
                acquisition.state.code(),
                acquisition.reason,
                MessageId::new().to_string(),
                super::to_i64(source_sequence.0)?,
                next_acquisition_order(&tx)?,
            ],
        )?;
        prune_terminal_acquisitions(&tx)?;
        tx.commit()?;
        drop(guard);
        discard_detached(self, retired_detached)?;
        Ok(MmsBeginOutcome::Acquired(acquisition))
    }

    pub(crate) fn cleanup_terminal_mms_parts(&self) -> Result<(), Error> {
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let acquisition_ids = {
            let mut query = tx.prepare(
                "SELECT DISTINCT a.acquisition_id FROM mms_acquisitions a
                 JOIN mms_acquisition_parts p ON p.acquisition_id=a.acquisition_id
                 WHERE a.state IN ('blocked','unavailable')",
            )?;
            query
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut detached = Vec::new();
        for acquisition_id in acquisition_ids {
            detached.extend(unlink_acquisition_parts(&tx, &acquisition_id)?);
        }
        detached.sort();
        detached.dedup();
        prune_terminal_acquisitions(&tx)?;
        tx.commit()?;
        drop(guard);
        discard_detached(self, detached)
    }

    pub fn mms_acquisitions(&self, limit: usize) -> Result<Vec<MmsAcquisition>, Error> {
        let limit = limit.min(MAX_MMS_ACQUISITIONS);
        let guard = self.lock()?;
        let mut query = guard.conn.prepare(
            "SELECT acquisition_id,conversation_id,input,state,reason FROM mms_acquisitions WHERE state != 'complete' ORDER BY CASE state WHEN 'pending' THEN 0 WHEN 'blocked' THEN 1 ELSE 2 END, CASE WHEN state='unavailable' THEN -local_order ELSE local_order END LIMIT ?",
        )?;
        let rows = query
            .query_map(params![i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|row| decode_acquisition_row(&guard.conn, row))
            .collect()
    }

    pub fn mms_acquisition_parts(&self, id: &str) -> Result<Vec<MmsAcquisitionPart>, Error> {
        let guard = self.lock()?;
        let exists: Option<()> = guard
            .conn
            .query_row(
                "SELECT 1 FROM mms_acquisitions WHERE acquisition_id=?",
                params![id],
                |_| Ok(()),
            )
            .optional()?;
        if exists.is_none() {
            return Err(Error::NotFound);
        }
        acquisition_part_mappings(&guard.conn, id)
    }

    pub fn set_mms_acquisition_state(
        &self,
        id: &str,
        state: MmsAcquisitionState,
        reason: Option<&str>,
    ) -> Result<(), Error> {
        if id.is_empty() || id.len() > 256 || state == MmsAcquisitionState::Complete {
            return Err(Error::InvalidRequest("MMS acquisition state"));
        }
        if reason.is_some_and(|value| {
            value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
        }) {
            return Err(Error::InvalidRequest("MMS acquisition reason"));
        }
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<String> = tx
            .query_row(
                "SELECT state FROM mms_acquisitions WHERE acquisition_id=?",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current) = current else {
            return Err(Error::NotFound);
        };
        if MmsAcquisitionState::parse(&current) == Some(MmsAcquisitionState::Complete) {
            return Err(Error::Conflict);
        }
        let detached = if matches!(
            state,
            MmsAcquisitionState::Blocked | MmsAcquisitionState::Unavailable
        ) {
            unlink_acquisition_parts(&tx, id)?
        } else {
            Vec::new()
        };
        tx.execute(
            "UPDATE mms_acquisitions SET state=?,reason=? WHERE acquisition_id=?",
            params![state.code(), reason, id],
        )?;
        prune_terminal_acquisitions(&tx)?;
        tx.commit()?;
        drop(guard);
        discard_detached(self, detached)
    }

    /// Maps provider parts in provider sequence order only. This API deliberately has no position
    /// parameter: callers must stop at the first copy failure and retry that ordered prefix before
    /// mapping a later part. Provider IDs are opaque, so core cannot infer a different order.
    pub fn set_mms_acquisition_part(
        &self,
        id: &str,
        provider_part_id: &str,
        attachment_id: AttachmentId,
    ) -> Result<(), Error> {
        if id.is_empty()
            || provider_part_id.is_empty()
            || provider_part_id.len() > super::MAX_PROVIDER_ID_BYTES
        {
            return Err(Error::InvalidRequest("MMS provider part id"));
        }
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM mms_acquisitions WHERE acquisition_id=?",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(state) = state else {
            return Err(Error::NotFound);
        };
        let existing: Option<String> = tx.query_row("SELECT attachment_id FROM mms_acquisition_parts WHERE acquisition_id=? AND provider_part_id=?", params![id, provider_part_id], |row| row.get(0)).optional()?;
        if let Some(existing) = existing {
            if existing != attachment_id.to_string() {
                return Err(Error::Conflict);
            }
            tx.commit()?;
            return Ok(());
        }
        // Only pending acquisitions accept new part mappings; blocked, unavailable and complete
        // rows are terminal for mapping (the existing-mapping replay above still succeeds).
        if MmsAcquisitionState::parse(&state) != Some(MmsAcquisitionState::Pending) {
            return Err(Error::Conflict);
        }
        let mapped_elsewhere: Option<()> = tx
            .query_row(
                "SELECT 1 FROM mms_acquisition_parts WHERE attachment_id=? LIMIT 1",
                params![attachment_id.to_string()],
                |_| Ok(()),
            )
            .optional()?;
        if mapped_elsewhere.is_some() {
            return Err(Error::Conflict);
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM mms_acquisition_parts WHERE acquisition_id=?",
            params![id],
            |row| row.get(0),
        )?;
        if usize::try_from(count).map_err(|_| Error::Database)? >= super::MAX_MMS_ATTACHMENTS {
            return Err(Error::InvalidRequest("too many MMS parts"));
        }
        super::local_references(&tx, &[attachment_id])?;
        let pending = pending_media_bytes(&tx)?;
        let bytes: i64 = tx.query_row(
            "SELECT plaintext_bytes FROM attachments WHERE attachment_id=?",
            params![attachment_id.to_string()],
            |row| row.get(0),
        )?;
        let bytes = u64::try_from(bytes).map_err(|_| Error::Database)?;
        if pending.checked_add(bytes).ok_or(Error::Database)? > MAX_PENDING_MMS_MEDIA_BYTES {
            return Err(Error::MmsMediaQuota);
        }
        tx.execute(
            "INSERT INTO mms_acquisition_parts(acquisition_id,provider_part_id,attachment_id,position) VALUES(?,?,?,?)",
            params![id, provider_part_id, attachment_id.to_string(), count],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn complete_mms_acquisition(&self, id: &str) -> Result<Captured, Error> {
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        type Row = (String, String, String, String, i64);
        let row: Option<Row> = tx.query_row(
            "SELECT conversation_id,input,state,message_id,source_sequence FROM mms_acquisitions WHERE acquisition_id=?",
            params![id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        ).optional()?;
        let Some((conversation, encoded_input, state, message, sequence)) = row else {
            return Err(Error::NotFound);
        };
        let conversation_id = super::parse(&conversation)?;
        let message_id = super::parse(&message)?;
        if MmsAcquisitionState::parse(&state) == Some(MmsAcquisitionState::Complete) {
            return Ok(Captured {
                message_id,
                conversation_id,
                duplicate: true,
            });
        }
        let input: MmsAcquisitionInput = super::decode(encoded_input.as_bytes())?;
        validate_complete_input(&input)?;
        let parts = acquisition_part_mappings(&tx, id)?;
        if parts
            .iter()
            .any(|part| part.preview_state == MmsPreviewState::Deferred)
        {
            return Err(Error::Conflict);
        }
        let attachments: Vec<_> = parts.iter().map(|part| part.attachment_id).collect();
        if input.body.is_empty() && attachments.is_empty() {
            return Err(Error::InvalidRequest("MMS requires text or parts"));
        }
        match reconcile_outgoing_provider_send(&tx, ctx.device_id, &input)? {
            Reconciliation::Matched {
                message_id,
                conversation_id,
                source_sequence,
            } => {
                let detached = unlink_acquisition_parts(&tx, id)?;
                tx.execute(
                    "UPDATE mms_acquisitions SET state='complete',reason=NULL,message_id=?,conversation_id=?,source_sequence=? WHERE acquisition_id=?",
                    params![message_id.to_string(), conversation_id.to_string(), super::to_i64(source_sequence.0)?, id],
                )?;
                tx.commit()?;
                drop(guard);
                discard_detached(self, detached)?;
                return Ok(Captured {
                    message_id,
                    conversation_id,
                    duplicate: true,
                });
            }
            Reconciliation::Ambiguous => {
                let detached = unlink_acquisition_parts(&tx, id)?;
                tx.execute(
                    "UPDATE mms_acquisitions SET state='unavailable',reason='transaction id reconciliation unavailable' WHERE acquisition_id=?",
                    params![id],
                )?;
                prune_terminal_acquisitions(&tx)?;
                tx.commit()?;
                drop(guard);
                discard_detached(self, detached)?;
                return Err(Error::Conflict);
            }
            Reconciliation::OriginalProviderMessage => {}
        }
        let payload = MessagePayload {
            record: MessageRecord {
                message_id,
                conversation_id,
                source_sequence: SourceSequence(
                    u64::try_from(sequence).map_err(|_| Error::Database)?,
                ),
                attachments: super::local_references(&tx, &attachments)?,
            },
            source_device_id: ctx.device_id,
            provider_message_id: Some(namespaced_provider_id(&input)),
            sender_address: input.sender_address.clone(),
            recipients: input.recipients.clone(),
            subject: input.subject.clone(),
            body: input.body.clone(),
            transport: Transport::Mms,
            direction: input.direction,
            imported: input.imported,
            // Non-positive provider dates are unknown, never 1970.
            sent_at_ms: Some(input.observed_at_ms).filter(|time| *time > 0),
            ordered_at_ms: (input.observed_at_ms <= 0).then(crate::notifications::now_ms),
            mms_context: Some(MmsContext {
                source_generation: input.source.source_generation.clone(),
                subscription_id: input.source.subscription_id.clone(),
                provider_thread_id: input.source.provider_thread_id.clone(),
            }),
        };
        match super::insert_message(&tx, &payload)? {
            Inserted::New => {}
            Inserted::Conflict => return Err(Error::Conflict),
            Inserted::Same => return Err(Error::Database),
        }
        let thumbnail_ids: Vec<_> = parts
            .iter()
            .filter_map(|part| part.thumbnail_attachment_id)
            .collect();
        let media_meta = parts
            .iter()
            .map(|part| super::MmsMediaMeta {
                attachment_id: part.attachment_id,
                width: part.width,
                height: part.height,
                duration_ms: part.duration_ms,
                thumbnail: None,
            })
            .collect::<Vec<_>>();
        super::store_local_message_media_meta(&tx, message_id, &media_meta, id)?;
        super::queue_message_banner(&tx, &payload)?;
        let (envelope_id, _) = super::enqueue(
            &tx,
            &ctx,
            crate::EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::MmsMessage {
                message: payload,
                media: Vec::new(),
                media_meta: Vec::new(),
            },
        )?;
        for thumbnail_id in thumbnail_ids {
            tx.execute(
                "INSERT OR IGNORE INTO outbox_attachments(envelope_id,attachment_id) VALUES(?,?)",
                params![envelope_id.to_string(), thumbnail_id.to_string()],
            )?;
        }
        if !input.imported {
            crate::deferred_media::register_capture(&tx, message_id, envelope_id)?;
        }
        tx.execute(
            "UPDATE mms_acquisitions SET state='complete',reason=NULL WHERE acquisition_id=?",
            params![id],
        )?;
        tx.commit()?;
        Ok(Captured {
            message_id,
            conversation_id,
            duplicate: false,
        })
    }

    pub fn mms_scan_checkpoint(
        &self,
        source_generation: &str,
        subscription_id: &str,
        imported: bool,
    ) -> Result<Option<String>, Error> {
        validate_checkpoint_scope(source_generation, subscription_id)?;
        let guard = self.lock()?;
        guard.conn.query_row(
            "SELECT provider_message_id FROM mms_scan_checkpoints WHERE source_generation=? AND subscription_id=? AND imported=?",
            params![source_generation, subscription_id, imported], |row| row.get(0),
        ).optional().map_err(Into::into)
    }

    pub fn set_mms_scan_checkpoint(
        &self,
        source_generation: &str,
        subscription_id: &str,
        imported: bool,
        provider_message_id: &str,
    ) -> Result<(), Error> {
        validate_checkpoint_scope(source_generation, subscription_id)?;
        let candidate = checkpoint_number(provider_message_id)?;
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx.query_row(
            "SELECT provider_message_id FROM mms_scan_checkpoints WHERE source_generation=? AND subscription_id=? AND imported=?",
            params![source_generation, subscription_id, imported], |row| row.get(0),
        ).optional()?;
        if existing
            .as_deref()
            .map(checkpoint_number)
            .transpose()?
            .is_none_or(|current| candidate > current)
        {
            tx.execute(
                "INSERT INTO mms_scan_checkpoints(source_generation,subscription_id,imported,provider_message_id) VALUES(?,?,?,?) ON CONFLICT(source_generation,subscription_id,imported) DO UPDATE SET provider_message_id=excluded.provider_message_id",
                params![source_generation, subscription_id, imported, provider_message_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn mms_pending_media_bytes(&self) -> Result<u64, Error> {
        let guard = self.lock()?;
        pending_media_bytes(&guard.conn)
    }

    pub fn set_mms_acquisition_part_metadata(
        &self,
        id: &str,
        provider_part_id: &str,
        width: Option<u32>,
        height: Option<u32>,
        duration_ms: Option<u64>,
        thumbnail_attachment_id: Option<AttachmentId>,
    ) -> Result<(), Error> {
        if width.is_some_and(|value| value == 0)
            || height.is_some_and(|value| value == 0)
            || duration_ms.is_some_and(|value| value == 0)
        {
            return Err(Error::InvalidRequest("MMS media metadata"));
        }
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (original, state, current): (AttachmentId, String, crate::MediaMetaColumns) = tx.query_row(
            "SELECT p.attachment_id,a.state,p.width,p.height,p.duration_ms,p.thumbnail_attachment_id FROM mms_acquisition_parts p JOIN mms_acquisitions a ON a.acquisition_id=p.acquisition_id WHERE p.acquisition_id=? AND p.provider_part_id=?",
            params![id, provider_part_id],
            |row| Ok((row.get::<_, String>(0)?, row.get(1)?, (row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))),
        ).optional()?.ok_or(Error::NotFound).and_then(|(original, state, current)| Ok((super::parse(&original)?, state, current)))?;
        if MmsAcquisitionState::parse(&state) != Some(MmsAcquisitionState::Pending) {
            return Err(Error::Conflict);
        }
        let requested = (
            width.map(i64::from),
            height.map(i64::from),
            duration_ms.map(super::to_i64).transpose()?,
            thumbnail_attachment_id.map(|value| value.to_string()),
        );
        if current == requested {
            return Ok(());
        }
        if let Some(thumbnail) = thumbnail_attachment_id {
            if thumbnail == original {
                return Err(Error::InvalidRequest("thumbnail equals original"));
            }
            let visible: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM mms_acquisition_parts WHERE acquisition_id=? AND attachment_id=?)",
                params![id, thumbnail.to_string()],
                |row| row.get(0),
            )?;
            if visible {
                return Err(Error::InvalidRequest("thumbnail is visible part"));
            }
            let owned_elsewhere: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM mms_acquisition_parts WHERE thumbnail_attachment_id=? AND (acquisition_id<>? OR provider_part_id<>?))",
                params![thumbnail.to_string(), id, provider_part_id],
                |row| row.get(0),
            )?;
            if owned_elsewhere {
                return Err(Error::Conflict);
            }
            super::local_references(&tx, &[thumbnail])?;
        }
        tx.execute(
            "UPDATE mms_acquisition_parts SET width=?,height=?,duration_ms=?,thumbnail_attachment_id=?,metadata_recorded=1 WHERE acquisition_id=? AND provider_part_id=?",
            params![requested.0, requested.1, requested.2, requested.3, id, provider_part_id],
        )?;
        if pending_media_bytes(&tx)? > MAX_PENDING_MMS_MEDIA_BYTES {
            return Err(Error::MmsMediaQuota);
        }
        let detached = current.3.map(|value| super::parse(&value)).transpose()?;
        tx.commit()?;
        if let Some(detached) = detached {
            self.discard_unreferenced_attachment(detached)?;
        }
        Ok(())
    }

    /// Records preview production independently from original acquisition. A permanently
    /// unavailable optional preview cannot stall the original message.
    pub fn set_mms_acquisition_part_preview_state(
        &self,
        id: &str,
        provider_part_id: &str,
        state: MmsPreviewState,
    ) -> Result<(), Error> {
        let guard = self.lock()?;
        let available: Option<bool> = guard
            .conn
            .query_row(
                "SELECT thumbnail_attachment_id IS NOT NULL FROM mms_acquisition_parts WHERE acquisition_id=? AND provider_part_id=?",
                params![id, provider_part_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(available) = available else {
            return Err(Error::NotFound);
        };
        if state == MmsPreviewState::Ready && !available {
            return Err(Error::InvalidRequest("MMS preview output"));
        }
        guard.conn.execute(
            "UPDATE mms_acquisition_parts SET preview_state=? WHERE acquisition_id=? AND provider_part_id=?",
            params![state.code(), id, provider_part_id],
        )?;
        Ok(())
    }
}

/// Keeps provider identity while reducing malformed metadata to a bounded health record. Callers
/// must not advance a scan checkpoint when this returns `Some`; Android may retry the same row.
fn normalize_pending_input(
    mut input: MmsAcquisitionInput,
) -> Result<(MmsAcquisitionInput, Option<&'static str>), Error> {
    validate_checkpoint_scope(
        &input.source.source_generation,
        &input.source.subscription_id,
    )?;
    for provider_id in [&input.source.provider_message_id]
        .into_iter()
        .chain(input.source.provider_thread_id.as_ref())
    {
        if provider_id.is_empty() || provider_id.len() > super::MAX_PROVIDER_ID_BYTES {
            return Err(Error::InvalidRequest("MMS provider id"));
        }
    }
    let malformed = input.body.len() > super::MAX_BODY_BYTES
        || input.transaction_id.as_ref().is_some_and(|transaction_id| {
            transaction_id.is_empty() || transaction_id.len() > super::MAX_PROVIDER_ID_BYTES
        })
        || input
            .subject
            .as_ref()
            .is_some_and(|subject| subject.len() > super::MAX_BODY_BYTES)
        || input.recipients.len() > super::MAX_MMS_RECIPIENTS
        || input
            .sender_address
            .as_deref()
            .is_some_and(|address| super::require_address(address).is_err())
        || input
            .recipients
            .iter()
            .any(|address| super::require_address(address).is_err());
    if malformed {
        input.sender_address = None;
        input.recipients.clear();
        input.subject = None;
        input.body.clear();
        return Ok((input, Some("provider metadata unavailable")));
    }
    input.recipients.sort();
    input.recipients.dedup();
    Ok((input, None))
}

fn validate_complete_input(input: &MmsAcquisitionInput) -> Result<(), Error> {
    let (_, unavailable) = normalize_pending_input(input.clone())?;
    if unavailable.is_some() {
        return Err(Error::InvalidRequest("MMS metadata"));
    }
    match input.direction {
        Direction::Incoming if input.sender_address.is_none() => {
            Err(Error::InvalidRequest("MMS sender"))
        }
        Direction::Outgoing if input.recipients.is_empty() => {
            Err(Error::InvalidRequest("MMS recipients"))
        }
        _ => Ok(()),
    }
}

/// Provider capture provenance and mutable presentation metadata are not immutable identity. A
/// changed date, subject, sender presentation, or recipient order must not duplicate a completed
/// message. A differing non-null transaction ID is handled by the source-instance conflict path.
fn same_provider_identity(existing: &MmsAcquisitionInput, candidate: &MmsAcquisitionInput) -> bool {
    same_raw_provider_source(existing, candidate)
        && existing.transaction_id == candidate.transaction_id
}

fn strongly_distinct_provider_instance(
    existing: &MmsAcquisitionInput,
    candidate: &MmsAcquisitionInput,
) -> bool {
    same_raw_provider_source(existing, candidate)
        && existing.transaction_id.is_some()
        && candidate.transaction_id.is_some()
        && existing.transaction_id != candidate.transaction_id
}

fn same_raw_provider_source(
    existing: &MmsAcquisitionInput,
    candidate: &MmsAcquisitionInput,
) -> bool {
    existing.source.source_generation == candidate.source.source_generation
        && existing.source.subscription_id == candidate.source.subscription_id
        && existing.source.provider_message_id == candidate.source.provider_message_id
        && existing.direction == candidate.direction
}

fn source_instance_id(input: &MmsAcquisitionInput) -> &str {
    input.transaction_id.as_deref().unwrap_or("")
}

fn is_terminal_acquisition_state(state: MmsAcquisitionState) -> bool {
    matches!(
        state,
        MmsAcquisitionState::Blocked | MmsAcquisitionState::Unavailable
    )
}

fn same_terminal_provider_input(
    existing: &MmsAcquisitionInput,
    candidate: &MmsAcquisitionInput,
) -> bool {
    let mut candidate = candidate.clone();
    candidate.imported = existing.imported;
    existing == &candidate
}

fn provider_identity_quarantine(
    conn: &rusqlite::Connection,
    input: &MmsAcquisitionInput,
    reason: &str,
) -> Result<MmsProviderIdentityQuarantine, Error> {
    super::mms_backfill::record_provider_identity_quarantine(conn, input, reason)?;
    Ok(MmsProviderIdentityQuarantine {
        source: input.source.clone(),
        direction: input.direction,
        reason: reason.into(),
    })
}

fn retire_provider_identity_acquisitions<'a>(
    conn: &rusqlite::Connection,
    acquisitions: impl Iterator<Item = &'a MmsAcquisition>,
    reason: &str,
) -> Result<Vec<AttachmentId>, Error> {
    let mut detached = Vec::new();
    for acquisition in acquisitions {
        if acquisition.state != MmsAcquisitionState::Pending {
            continue;
        }
        super::mms_backfill::record_provider_identity_quarantine(conn, &acquisition.input, reason)?;
        detached.extend(unlink_acquisition_parts(conn, &acquisition.acquisition_id)?);
        conn.execute(
            "UPDATE mms_acquisitions SET state='unavailable',reason=? WHERE acquisition_id=?",
            params![reason, acquisition.acquisition_id],
        )?;
    }
    detached.sort();
    detached.dedup();
    Ok(detached)
}

fn validate_checkpoint_scope(source_generation: &str, subscription_id: &str) -> Result<(), Error> {
    if source_generation.is_empty()
        || source_generation.len() > super::MAX_PROVIDER_ID_BYTES
        || subscription_id.is_empty()
        || subscription_id.len() > super::MAX_SUBSCRIPTION_BYTES
    {
        return Err(Error::InvalidRequest("MMS source or subscription"));
    }
    Ok(())
}

fn acquisitions_by_provider_source(
    conn: &rusqlite::Connection,
    source: &MmsSource,
    direction: Direction,
) -> Result<Vec<MmsAcquisition>, Error> {
    let mut query = conn.prepare(
        "SELECT acquisition_id,conversation_id,input,state,reason FROM mms_acquisitions WHERE source_generation=? AND subscription_id=? AND provider_message_id=? AND direction=?",
    )?;
    query
        .query_map(
            params![
                source.source_generation,
                source.subscription_id,
                source.provider_message_id,
                direction_code(direction)
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|row| decode_acquisition_row(conn, row))
        .collect()
}

type AcquisitionRow = (String, String, String, String, Option<String>);

fn decode_acquisition_row(
    conn: &rusqlite::Connection,
    (acquisition_id, conversation, input, state, reason): AcquisitionRow,
) -> Result<MmsAcquisition, Error> {
    Ok(MmsAcquisition {
        attachment_ids: acquisition_parts(conn, &acquisition_id)?,
        acquisition_id,
        conversation_id: super::parse(&conversation)?,
        input: super::decode(input.as_bytes())?,
        state: MmsAcquisitionState::parse(&state).ok_or(Error::Database)?,
        reason,
    })
}

fn acquisition_parts(conn: &rusqlite::Connection, id: &str) -> Result<Vec<AttachmentId>, Error> {
    let mut query = conn.prepare(
        "SELECT attachment_id FROM mms_acquisition_parts WHERE acquisition_id=? ORDER BY position",
    )?;
    query
        .query_map(params![id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|id| super::parse(&id))
        .collect()
}

fn acquisition_part_mappings(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<Vec<MmsAcquisitionPart>, Error> {
    let mut query = conn.prepare(
        "SELECT provider_part_id,attachment_id,width,height,duration_ms,thumbnail_attachment_id,preview_state,metadata_recorded FROM mms_acquisition_parts WHERE acquisition_id=? ORDER BY position",
    )?;
    query
        .query_map(params![id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, bool>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(
            |(
                provider_part_id,
                attachment_id,
                width,
                height,
                duration_ms,
                thumbnail_attachment_id,
                preview_state,
                metadata_recorded,
            )| {
                Ok(MmsAcquisitionPart {
                    provider_part_id,
                    attachment_id: super::parse(&attachment_id)?,
                    width: width
                        .map(u32::try_from)
                        .transpose()
                        .map_err(|_| Error::Database)?,
                    height: height
                        .map(u32::try_from)
                        .transpose()
                        .map_err(|_| Error::Database)?,
                    duration_ms: duration_ms
                        .map(u64::try_from)
                        .transpose()
                        .map_err(|_| Error::Database)?,
                    thumbnail_attachment_id: thumbnail_attachment_id
                        .map(|value| super::parse(&value))
                        .transpose()?,
                    preview_state: MmsPreviewState::parse(&preview_state).ok_or(Error::Database)?,
                    metadata_recorded,
                })
            },
        )
        .collect()
}

fn mapped_thread_conversation(
    conn: &rusqlite::Connection,
    device_id: &str,
    source: &MmsSource,
) -> Result<Option<ConversationId>, Error> {
    let Some(thread) = source.provider_thread_id.as_deref() else {
        return Ok(None);
    };
    conn.query_row(
        "SELECT conversation_id FROM mms_thread_conversations WHERE source_device_id=? AND source_generation=? AND subscription_id=? AND provider_thread_id=?",
        params![device_id, source.source_generation, source.subscription_id, thread], |row| row.get::<_, String>(0),
    ).optional()?.map(|id| super::parse(&id)).transpose()
}

fn link_thread_conversation(
    conn: &rusqlite::Connection,
    device_id: &str,
    source: &MmsSource,
    conversation: ConversationId,
) -> Result<(), Error> {
    let Some(thread) = source.provider_thread_id.as_deref() else {
        return Ok(());
    };
    conn.execute(
        "INSERT INTO mms_thread_conversations(source_device_id,source_generation,subscription_id,provider_thread_id,conversation_id) VALUES(?,?,?,?,?)",
        params![device_id, source.source_generation, source.subscription_id, thread, conversation.to_string()],
    )?;
    Ok(())
}

/// Reuse only an exact, fully-known participant set. An incoming group without a confirmed local
/// SIM number stays separate rather than guessing which participant is self.
fn conversation_for_mms_participants(
    conn: &rusqlite::Connection,
    device_id: crate::DeviceId,
    input: &MmsAcquisitionInput,
) -> Result<Option<ConversationId>, Error> {
    let region = crate::conversation_match::identity_region(conn)?;
    let mut own_addresses = crate::conversation_match::OwnAddressCache::new();
    let Some(expected) = crate::conversation_match::mms_participant_set(
        conn,
        input.direction,
        input.sender_address.as_deref(),
        &input.recipients,
        Some((&device_id, &input.source.subscription_id)),
        Some(&region),
        &mut own_addresses,
    )?
    else {
        return Ok(None);
    };
    let mut statement = conn.prepare(
        "SELECT conversation_id,payload FROM messages
          WHERE json_extract(payload,'$.transport')='mms'
            AND (conversation_id,sort_at_ms,local_order) IN (
              SELECT conversation_id,sort_at_ms,local_order FROM (SELECT conversation_id,sort_at_ms,local_order,ROW_NUMBER() OVER (PARTITION BY conversation_id ORDER BY sort_at_ms DESC,local_order DESC) AS newest FROM messages WHERE json_extract(payload,'$.transport')='mms') WHERE newest=1
            )
          ORDER BY sort_at_ms DESC,local_order DESC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (conversation_id, encoded) = row?;
        let payload: MessagePayload = crate::decode(encoded.as_bytes())?;
        let identity = payload
            .mms_context
            .as_ref()
            .map(|context| (&payload.source_device_id, context.subscription_id.as_str()));
        let Some(candidates) = crate::conversation_match::mms_participant_set(
            conn,
            payload.direction,
            payload.sender_address.as_deref(),
            &payload.recipients,
            identity,
            Some(&region),
            &mut own_addresses,
        )?
        else {
            continue;
        };
        if candidates == expected {
            return crate::parse(&conversation_id).map(Some);
        }
    }
    Ok(None)
}

fn next_acquisition_sequence(
    conn: &rusqlite::Connection,
    conversation: ConversationId,
    device: crate::DeviceId,
) -> Result<SourceSequence, Error> {
    let message = super::next_source_sequence(conn, conversation, device)?
        .0
        .saturating_sub(1);
    let acquisition: i64 = conn.query_row(
        "SELECT COALESCE(MAX(source_sequence),0) FROM mms_acquisitions WHERE conversation_id=?",
        params![conversation.to_string()],
        |row| row.get(0),
    )?;
    let acquisition = u64::try_from(acquisition).map_err(|_| Error::Database)?;
    Ok(SourceSequence(
        message
            .max(acquisition)
            .checked_add(1)
            .ok_or(Error::Database)?,
    ))
}

pub(crate) fn pending_acquisition_capacity_reached(
    conn: &rusqlite::Connection,
) -> Result<bool, Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM mms_acquisitions WHERE state='pending'",
        [],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count).map_err(|_| Error::Database)? >= MAX_MMS_ACQUISITIONS)
}

fn pending_media_bytes(conn: &rusqlite::Connection) -> Result<u64, Error> {
    let bytes: i64 = conn.query_row(
        "SELECT COALESCE(SUM(a.plaintext_bytes),0) FROM attachments a WHERE a.state != 'uploaded' AND NOT EXISTS(SELECT 1 FROM gateway_mms_media_evictions e WHERE e.attachment_id=a.attachment_id) AND a.attachment_id IN (SELECT attachment_id FROM mms_acquisition_parts UNION SELECT thumbnail_attachment_id FROM mms_acquisition_parts WHERE thumbnail_attachment_id IS NOT NULL)",
        [], |row| row.get(0),
    )?;
    u64::try_from(bytes).map_err(|_| Error::Database)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reconciliation {
    Matched {
        message_id: MessageId,
        conversation_id: ConversationId,
        source_sequence: SourceSequence,
    },
    Ambiguous,
    OriginalProviderMessage,
}

fn reconcile_outgoing_provider_send(
    conn: &rusqlite::Connection,
    local_device_id: crate::DeviceId,
    input: &MmsAcquisitionInput,
) -> Result<Reconciliation, Error> {
    if input.direction != Direction::Outgoing {
        return Ok(Reconciliation::OriginalProviderMessage);
    }
    let Some(transaction_id) = input.transaction_id.as_deref() else {
        return Ok(Reconciliation::OriginalProviderMessage);
    };
    let Some(raw_command_id) = transaction_id.strip_prefix("peppy-") else {
        return Ok(Reconciliation::OriginalProviderMessage);
    };
    let Ok(command_id) = super::parse::<crate::CommandId>(raw_command_id) else {
        return Ok(Reconciliation::OriginalProviderMessage);
    };
    if command_id.to_string() != raw_command_id {
        return Ok(Reconciliation::OriginalProviderMessage);
    }
    let row: Option<(String, String, String, i64)> = conn
        .query_row(
            "SELECT m.id,m.conversation_id,m.payload,m.source_sequence
             FROM commands c
             JOIN command_ledger l ON l.command_id=c.command_id
             JOIN attempts a ON a.command_id=c.command_id
             JOIN messages m ON m.id=c.message_id
             WHERE c.command_id=? AND c.gateway_device_id=? AND l.subscription_id=?",
            params![
                command_id.to_string(),
                local_device_id.to_string(),
                input.source.subscription_id
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((message_id, conversation_id, encoded, source_sequence)) = row else {
        return Ok(Reconciliation::Ambiguous);
    };
    let message: MessagePayload = super::decode(encoded.as_bytes())?;
    if message.transport != Transport::Mms || message.direction != Direction::Outgoing {
        return Ok(Reconciliation::Ambiguous);
    }
    Ok(Reconciliation::Matched {
        message_id: super::parse(&message_id)?,
        conversation_id: super::parse(&conversation_id)?,
        source_sequence: SourceSequence(
            u64::try_from(source_sequence).map_err(|_| Error::Database)?,
        ),
    })
}

fn validate_thread_update(old: &MmsSource, new: &MmsSource) -> Result<(), Error> {
    if let (Some(old), Some(new)) = (
        old.provider_thread_id.as_deref(),
        new.provider_thread_id.as_deref(),
    ) && old != new
    {
        return Err(Error::Conflict);
    }
    Ok(())
}

fn acquisition_identity(
    conn: &rusqlite::Connection,
    acquisition_id: &str,
) -> Result<(MessageId, SourceSequence), Error> {
    let (message_id, source_sequence): (String, i64) = conn.query_row(
        "SELECT message_id,source_sequence FROM mms_acquisitions WHERE acquisition_id=?",
        params![acquisition_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((
        super::parse(&message_id)?,
        SourceSequence(u64::try_from(source_sequence).map_err(|_| Error::Database)?),
    ))
}

fn next_acquisition_order(conn: &rusqlite::Connection) -> Result<i64, Error> {
    conn.query_row(
        "SELECT COALESCE(MAX(local_order), 0) + 1 FROM mms_acquisitions",
        [],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn unlink_acquisition_parts(
    conn: &rusqlite::Connection,
    acquisition_id: &str,
) -> Result<Vec<AttachmentId>, Error> {
    let mut attachments = acquisition_parts(conn, acquisition_id)?;
    let mut thumbnails = conn.prepare(
        "SELECT thumbnail_attachment_id FROM mms_acquisition_parts WHERE acquisition_id=? AND thumbnail_attachment_id IS NOT NULL",
    )?;
    attachments.extend(
        thumbnails
            .query_map(params![acquisition_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|id| super::parse::<AttachmentId>(&id))
            .collect::<Result<Vec<_>, Error>>()?,
    );
    attachments.sort();
    attachments.dedup();
    conn.execute(
        "DELETE FROM mms_acquisition_parts WHERE acquisition_id=?",
        params![acquisition_id],
    )?;
    Ok(attachments)
}

fn discard_detached(client: &Client, attachments: Vec<AttachmentId>) -> Result<(), Error> {
    for attachment in attachments {
        client.discard_unreferenced_attachment(attachment)?;
    }
    Ok(())
}

fn prune_terminal_acquisitions(conn: &rusqlite::Connection) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM mms_acquisitions WHERE acquisition_id IN (
            SELECT a.acquisition_id FROM mms_acquisitions a
            WHERE a.state IN ('blocked','unavailable')
              AND NOT EXISTS (SELECT 1 FROM mms_acquisition_parts p WHERE p.acquisition_id=a.acquisition_id)
            ORDER BY
              CASE WHEN COALESCE(CAST(json_extract(a.input,'$.observed_at_ms') AS INTEGER),0)>0 THEN 0 ELSE 1 END,
              CASE WHEN CAST(json_extract(a.input,'$.observed_at_ms') AS INTEGER)>0 THEN CAST(json_extract(a.input,'$.observed_at_ms') AS INTEGER) END DESC,
              a.local_order DESC,
              a.acquisition_id DESC
            LIMIT -1 OFFSET ?
        )",
        params![MAX_MMS_ACQUISITIONS as i64],
    )?;
    Ok(())
}

fn link_reconciled_thread(
    conn: &rusqlite::Connection,
    device_id: &str,
    source: &MmsSource,
    target: ConversationId,
    old_conversation: Option<ConversationId>,
    current_acquisition: &str,
    local_device: crate::DeviceId,
) -> Result<(), Error> {
    let Some(thread) = source.provider_thread_id.as_deref() else {
        return Ok(());
    };
    let mapped: Option<String> = conn
        .query_row(
            "SELECT conversation_id FROM mms_thread_conversations WHERE source_device_id=? AND source_generation=? AND subscription_id=? AND provider_thread_id=?",
            params![device_id, source.source_generation, source.subscription_id, thread],
            |row| row.get(0),
        )
        .optional()?;
    let Some(mapped) = mapped else {
        conn.execute(
            "INSERT INTO mms_thread_conversations(source_device_id,source_generation,subscription_id,provider_thread_id,conversation_id) VALUES(?,?,?,?,?)",
            params![device_id, source.source_generation, source.subscription_id, thread, target.to_string()],
        )?;
        return Ok(());
    };
    let mapped_id: ConversationId = super::parse(&mapped)?;
    if mapped_id == target {
        return Ok(());
    }
    let populated: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE conversation_id=?)",
        params![mapped],
        |row| row.get(0),
    )?;
    if populated {
        return Ok(());
    }
    conn.execute(
        "UPDATE mms_thread_conversations SET conversation_id=? WHERE source_device_id=? AND source_generation=? AND subscription_id=? AND provider_thread_id=? AND conversation_id=?",
        params![target.to_string(), device_id, source.source_generation, source.subscription_id, thread, mapped],
    )?;
    let pending: Vec<String> = {
        let mut query = conn.prepare(
            "SELECT acquisition_id FROM mms_acquisitions WHERE conversation_id=? AND state='pending' AND acquisition_id!=? ORDER BY local_order",
        )?;
        query
            .query_map(params![mapped, current_acquisition], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    for acquisition_id in pending {
        let sequence = next_acquisition_sequence(conn, target, local_device)?;
        conn.execute(
            "UPDATE mms_acquisitions SET conversation_id=?,source_sequence=? WHERE acquisition_id=?",
            params![target.to_string(), super::to_i64(sequence.0)?, acquisition_id],
        )?;
    }
    // The current exact match is moved separately by its caller. A pre-feature row may have used
    // an isolated empty conversation without a thread mapping; that is safe to abandon.
    let _ = old_conversation;
    Ok(())
}

fn checkpoint_number(value: &str) -> Result<u64, Error> {
    if value.is_empty()
        || value.len() > super::MAX_PROVIDER_ID_BYTES
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Error::InvalidRequest("MMS checkpoint"));
    }
    value
        .parse()
        .map_err(|_| Error::InvalidRequest("MMS checkpoint"))
}

pub(crate) fn direction_code(direction: Direction) -> &'static str {
    match direction {
        Direction::Incoming => "incoming",
        Direction::Outgoing => "outgoing",
    }
}

fn namespaced_provider_id(input: &MmsAcquisitionInput) -> String {
    let mut hasher = Sha256::new();
    for value in [
        &input.source.source_generation,
        &input.source.subscription_id,
        &input.source.provider_message_id,
        source_instance_id(input),
        direction_code(input.direction),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(4 + digest.len() * 2);
    encoded.push_str("mms:");
    for byte in digest {
        use std::fmt::Write;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_row(conn: &rusqlite::Connection) -> AcquisitionRow {
        conn.execute_batch(
            "CREATE TABLE mms_acquisition_parts (
                acquisition_id TEXT NOT NULL,
                attachment_id TEXT NOT NULL,
                position INTEGER NOT NULL
            )",
        )
        .unwrap();
        let attachment_id = AttachmentId::new();
        conn.execute(
            "INSERT INTO mms_acquisition_parts(acquisition_id,attachment_id,position) VALUES('acquisition',?,0)",
            params![attachment_id.to_string()],
        )
        .unwrap();
        let input = MmsAcquisitionInput {
            source: MmsSource {
                source_generation: "generation".into(),
                subscription_id: "subscription".into(),
                provider_message_id: "message".into(),
                provider_thread_id: None,
            },
            direction: Direction::Incoming,
            sender_address: Some("+15555550100".into()),
            recipients: Vec::new(),
            subject: None,
            body: "body".into(),
            imported: false,
            observed_at_ms: 1,
            transaction_id: None,
        };
        (
            "acquisition".into(),
            ConversationId::new().to_string(),
            serde_json::to_string(&input).unwrap(),
            "blocked".into(),
            Some("awaiting media".into()),
        )
    }

    #[test]
    fn decoded_acquisition_preserves_state_and_reason() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();

        let acquisition = decode_acquisition_row(&conn, seeded_row(&conn)).unwrap();

        assert_eq!(acquisition.state, MmsAcquisitionState::Blocked);
        assert_eq!(acquisition.reason.as_deref(), Some("awaiting media"));
        assert_eq!(acquisition.attachment_ids.len(), 1);
    }

    #[test]
    fn decoded_acquisition_rejects_a_malformed_attachment_id() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let row = seeded_row(&conn);
        conn.execute(
            "UPDATE mms_acquisition_parts SET attachment_id='malformed'",
            [],
        )
        .unwrap();

        assert_eq!(decode_acquisition_row(&conn, row), Err(Error::Database));
    }

    #[test]
    fn terminal_pruning_uses_local_order_for_undated_rows() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mms_acquisitions(
                acquisition_id TEXT PRIMARY KEY,
                state TEXT NOT NULL,
                input TEXT NOT NULL,
                local_order INTEGER NOT NULL
            );
            CREATE TABLE mms_acquisition_parts(acquisition_id TEXT NOT NULL);",
        )
        .unwrap();
        for order in 1..=MAX_MMS_ACQUISITIONS + 1 {
            conn.execute(
                "INSERT INTO mms_acquisitions(acquisition_id,state,input,local_order) VALUES(?, 'blocked', '{\"observed_at_ms\":0}', ?)",
                params![order.to_string(), i64::try_from(order).unwrap()],
            )
            .unwrap();
        }

        prune_terminal_acquisitions(&conn).unwrap();

        let first: Option<String> = conn
            .query_row(
                "SELECT acquisition_id FROM mms_acquisitions WHERE acquisition_id='1'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        let newest: Option<String> = conn
            .query_row(
                "SELECT acquisition_id FROM mms_acquisitions WHERE acquisition_id=?",
                params![(MAX_MMS_ACQUISITIONS + 1).to_string()],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(first, None);
        assert_eq!(newest, Some((MAX_MMS_ACQUISITIONS + 1).to_string()));
    }

    #[test]
    fn decoded_acquisition_rejects_a_malformed_conversation_id() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let mut row = seeded_row(&conn);
        row.1 = "malformed".into();

        assert_eq!(decode_acquisition_row(&conn, row), Err(Error::Database));
    }

    #[test]
    fn decoded_acquisition_rejects_malformed_input_json() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let mut row = seeded_row(&conn);
        row.2 = "not JSON".into();

        assert_eq!(decode_acquisition_row(&conn, row), Err(Error::Database));
    }

    #[test]
    fn decoded_acquisition_rejects_an_unknown_state() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let mut row = seeded_row(&conn);
        row.3 = "unknown".into();

        assert_eq!(decode_acquisition_row(&conn, row), Err(Error::Database));
    }
}
