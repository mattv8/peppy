use crate::sms_reconciliation::{
    ProviderSmsEvidence, SmsReconciliation, canonical_counterpart,
    forget_carrier_wire_bodies_for_message, reconcile_provider_sms,
};
use crate::{
    Captured, Client, ConversationId, Direction, Error, IncomingSms, Inserted, MessageId,
    MessagePayload, MessageRecord, PrivatePayload, SourceSequence, Transport,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

/// Bounded queue of provider rows awaiting publication. Completed acquisitions are durable
/// bookkeeping and never count toward it.
pub const MAX_PENDING_SMS_ACQUISITIONS: usize = 1_000;
pub const MAX_PENDING_SMS_ACQUISITION_BYTES: usize = 8 * 1024 * 1024;
/// Headroom kept below the snapshot recovery limits for contacts, notifications, statuses and
/// other retained event state that this estimate does not enumerate.
const RECOVERY_RECORD_RESERVE: u64 = 10_000;
const RECOVERY_BYTE_RESERVE: u64 = 64 * 1024 * 1024;
/// Completed full-provider reconciliation sweeps restart at most this often.
pub const SMS_RECONCILIATION_INTERVAL_MS: i64 = 6 * 60 * 60 * 1000;
const LOCAL_REUSE_WINDOW: u64 = 200;
const LIVE_GAP_THRESHOLD_IDS: u64 = 500;

/// Host reason codes for provider rows that are not (yet) messages: drafts and failed sends stay
/// rechecked, but neither fill the pending queue nor report as reconciliation work.
pub const SMS_REASON_DRAFT: &str = "provider_box_draft";
pub const SMS_REASON_FAILED: &str = "provider_box_failed";
/// A provider row whose content cannot be represented (for example a missing or invalid address).
pub const SMS_REASON_INVALID: &str = "provider_row_invalid";
pub const SMS_REASON_AMBIGUOUS: &str = "reconciliation_ambiguous";
const PAUSE_RECOVERY_CAPACITY: &str = "recovery_capacity";
const PARKED: &str = "('provider_box_draft','provider_box_failed')";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmsSource {
    pub source_generation: String,
    pub subscription_id: String,
    pub provider_message_id: String,
    pub provider_thread_id: Option<String>,
    /// Deterministic `SMS_RECEIVED` identity the host derives from provider evidence, when known.
    #[serde(default)]
    pub broadcast_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmsAcquisitionInput {
    pub source: SmsSource,
    pub direction: Direction,
    pub sender_address: Option<String>,
    pub recipients: Vec<String>,
    pub body: String,
    pub imported: bool,
    /// Provider `date`: local insertion/receipt time in milliseconds. Non-positive is unknown.
    pub sent_at_ms: Option<i64>,
    /// Provider `date_sent`: SMSC time for incoming rows; Android writes 0 for outgoing rows.
    pub date_sent_ms: Option<i64>,
    pub creator: Option<String>,
    /// Mutable provider `read` evidence. It is intentionally excluded from source identity.
    #[serde(default)]
    pub provider_read: Option<bool>,
    /// Length-delimited hash of immutable provider-row evidence. Provider IDs can be reused.
    pub source_row_fingerprint: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SmsAcquisitionState {
    Pending,
    Unavailable,
    Complete,
    Paused,
}
impl SmsAcquisitionState {
    fn code(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Unavailable => "unavailable",
            Self::Complete => "complete",
            Self::Paused => "paused",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => Self::Pending,
            "unavailable" => Self::Unavailable,
            "complete" => Self::Complete,
            "paused" => Self::Paused,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmsAcquisition {
    pub acquisition_id: String,
    pub conversation_id: ConversationId,
    pub input: SmsAcquisitionInput,
    pub state: SmsAcquisitionState,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmsScanStatus {
    pub checkpoint: Option<String>,
    pub history_complete: bool,
    pub paused: bool,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmsScanPlan {
    pub source_generation: String,
    pub subscription_id: String,
    pub imported: bool,
    pub checkpoint: Option<String>,
    pub descending: bool,
    pub history_floor: Option<String>,
    pub history_upper_bound: Option<String>,
    pub history_complete: bool,
    pub paused: bool,
    pub pending_recheck_cursor: Option<String>,
    /// An independently fenced descending window left behind after a large live-id jump.
    pub live_gap: Option<SmsLiveGapPlan>,
    /// Maximum provider rows this plan may import. Live work always retains its normal bound.
    pub budget: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmsLiveGapPlan {
    pub source_generation: String,
    pub subscription_id: String,
    pub upper_bound: String,
    pub floor: String,
    pub cursor: Option<String>,
    pub token: String,
}

/// Volatile host facts used only to size older text-history pages. They are never persisted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistoryImportConditions {
    pub unmetered_network: bool,
    pub charging: bool,
}

const INITIAL_HISTORY_SEED: i64 = 100;
const ORDINARY_HISTORY_PAGE: i64 = 50;
const ACCELERATED_HISTORY_PAGE: i64 = 200;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmsReconciliationPlan {
    pub source_generation: String,
    pub subscription_id: String,
    pub cursor: Option<String>,
    pub upper_bound: String,
    /// Frozen at sweep start so a moving history baseline cannot change the scanned region.
    pub floor: Option<String>,
    /// Rows at or below the frozen history baseline are imported history, not live messages.
    pub history_upper_bound: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmsHistoryStatus {
    pub baseline_initialized: bool,
    pub scanning: bool,
    pub history_complete: bool,
    pub paused: bool,
    pub provider_error: bool,
    /// Publishable rows still awaiting completion (drafts and failed sends excluded).
    pub pending_count: u64,
    pub ambiguous_count: u64,
}
type DescendingCheckpoint = (String, bool, Option<String>, Option<String>, Option<String>);

struct Limits {
    pending: usize,
    pending_bytes: usize,
    recovery_records: u64,
    recovery_bytes: u64,
}
const LIMITS: Limits = Limits {
    pending: MAX_PENDING_SMS_ACQUISITIONS,
    pending_bytes: MAX_PENDING_SMS_ACQUISITION_BYTES,
    recovery_records: crate::RECOVERY_RECORD_BUDGET - RECOVERY_RECORD_RESERVE,
    recovery_bytes: crate::MAX_SNAPSHOT_BYTES - RECOVERY_BYTE_RESERVE,
};

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sms_live_gaps(
            source_generation TEXT NOT NULL,
            subscription_id TEXT NOT NULL,
            upper_bound TEXT NOT NULL,
            floor TEXT NOT NULL,
            cursor TEXT,
            token TEXT NOT NULL,
            PRIMARY KEY(source_generation, subscription_id)
         );",
    )?;
    crate::add_column_if_missing(
        conn,
        "sms_reconciliation_scans",
        "descending INTEGER NOT NULL DEFAULT 0",
    )?;
    crate::add_column_if_missing(conn, "sms_reconciliation_scans", "floor TEXT")?;
    Ok(())
}

impl Client {
    /// Persists a provider row before the host advances its scan checkpoint. Content the core
    /// cannot represent is durably recorded `unavailable` (reason `provider_row_invalid`) so the
    /// host can advance; only malformed scope metadata, storage failures and capacity are errors.
    pub fn begin_sms_acquisition(
        &self,
        input: SmsAcquisitionInput,
    ) -> Result<SmsAcquisition, Error> {
        self.begin_sms_acquisition_with(input, &LIMITS)
    }

    fn begin_sms_acquisition_with(
        &self,
        input: SmsAcquisitionInput,
        limits: &Limits,
    ) -> Result<SmsAcquisition, Error> {
        validate_metadata(&input)?;
        let input = normalized(input);
        let representable = validate_content(&input).is_ok();
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = by_source(&tx, &input)? {
            if existing.state == SmsAcquisitionState::Complete {
                let message: String = tx.query_row(
                    "SELECT message_id FROM sms_acquisitions WHERE acquisition_id=?",
                    params![existing.acquisition_id],
                    |row| row.get(0),
                )?;
                if crate::phone_read::observe_captured_read(
                    &tx,
                    &ctx,
                    crate::phone_read::CapturedRead {
                        transport: crate::phone_read::PhoneReadTransport::Sms,
                        provider_read: input.provider_read,
                        incoming: input.direction == Direction::Incoming,
                        imported: existing.input.imported,
                        message_id: crate::parse(&message)?,
                        message_exists: true,
                    },
                )? {
                    crate::seal_pending(&tx, &ctx, crate::MAX_SEAL_BATCH)?;
                }
                tx.commit()?;
                return Ok(existing);
            }
            if representable && existing.reason.as_deref() == Some(SMS_REASON_INVALID) {
                // A once-unrepresentable row (for example an over-long draft, since edited) has
                // no conversation or sequence yet: discard the placeholder and acquire it afresh.
                tx.execute(
                    "DELETE FROM sms_acquisitions WHERE acquisition_id=?",
                    params![existing.acquisition_id],
                )?;
            } else {
                let refreshed = refresh(&tx, existing, input, representable)?;
                tx.commit()?;
                return Ok(refreshed);
            }
        }
        supersede(&tx, &input)?;
        if !representable {
            let acquisition = SmsAcquisition {
                acquisition_id: uuid::Uuid::new_v4().to_string(),
                conversation_id: ConversationId::new(),
                input: sanitized(input),
                state: SmsAcquisitionState::Unavailable,
                reason: Some(SMS_REASON_INVALID.into()),
            };
            persist_acquisition(&tx, &acquisition, MessageId::new(), SourceSequence(0))?;
            tx.commit()?;
            return Ok(acquisition);
        }
        let reconciliation = reconcile_provider_sms(&tx, &ctx.device_id, &evidence(&input))?;
        if let SmsReconciliation::Matched(existing) = reconciliation {
            forget_carrier_wire_bodies_for_message(&tx, existing.message_id)?;
            publish_reconciliation_alias(&tx, &ctx, &existing.message_id, &input)?;
            link_matched_thread(
                &tx,
                &ctx.device_id.to_string(),
                &input,
                existing.conversation_id,
            )?;
            let acquisition = matched_acquisition(&input, existing.conversation_id);
            persist_acquisition(
                &tx,
                &acquisition,
                existing.message_id,
                existing.source_sequence,
            )?;
            persist_alias_for_id(&tx, &input, &existing.message_id.to_string())?;
            if crate::phone_read::observe_captured_read(
                &tx,
                &ctx,
                crate::phone_read::CapturedRead {
                    transport: crate::phone_read::PhoneReadTransport::Sms,
                    provider_read: input.provider_read,
                    incoming: input.direction == Direction::Incoming,
                    imported: input.imported,
                    message_id: existing.message_id,
                    message_exists: true,
                },
            )? {
                crate::seal_pending(&tx, &ctx, crate::MAX_SEAL_BATCH)?;
            }
            tx.commit()?;
            return Ok(acquisition);
        }
        if pending_queue_full(&tx, &input, limits)? {
            return Err(Error::SmsCapacity);
        }
        if input.imported && recovery_capacity_reached(&tx, limits)? {
            pause_history(&tx, &input.source.source_generation)?;
            tx.commit()?;
            return Err(Error::SmsCapacity);
        }
        let ambiguous = reconciliation == SmsReconciliation::Ambiguous;
        let thread = thread_conversation(&tx, &ctx.device_id.to_string(), &input.source)?;
        let address = match input.direction {
            Direction::Incoming => input.sender_address.as_deref(),
            Direction::Outgoing => input.recipients.first().map(String::as_str),
        };
        let resolved = if input.recipients.len() <= 1 {
            address
                .map(|address| {
                    crate::conversation_match::resolve_carrier_counterpart(&tx, address, thread)
                })
                .transpose()?
                .flatten()
        } else {
            None
        };
        let conversation_id = match resolved {
            Some(conversation) => conversation,
            None => thread
                .or(counterpart_conversation(&tx, &input)?)
                .unwrap_or_else(ConversationId::new),
        };
        if resolved.is_some() || thread.is_none() {
            link_thread(
                &tx,
                &ctx.device_id.to_string(),
                &input.source,
                conversation_id,
            )?;
        }
        let acquisition = SmsAcquisition {
            acquisition_id: uuid::Uuid::new_v4().to_string(),
            conversation_id,
            input,
            state: if ambiguous {
                SmsAcquisitionState::Unavailable
            } else {
                SmsAcquisitionState::Pending
            },
            reason: ambiguous.then_some(SMS_REASON_AMBIGUOUS.into()),
        };
        let sequence = crate::next_source_sequence(&tx, conversation_id, ctx.device_id)?;
        persist_acquisition(&tx, &acquisition, MessageId::new(), sequence)?;
        tx.commit()?;
        Ok(acquisition)
    }

    pub fn complete_sms_acquisition(&self, id: &str) -> Result<Captured, Error> {
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row: Option<(String, String, String, String, i64)> = tx.query_row("SELECT conversation_id,input,state,message_id,source_sequence FROM sms_acquisitions WHERE acquisition_id=?", params![id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        let Some((conversation, encoded, state, stored_message, sequence)) = row else {
            return Err(Error::NotFound);
        };
        let conversation_id: ConversationId = crate::parse(&conversation)?;
        match SmsAcquisitionState::parse(&state) {
            Some(SmsAcquisitionState::Complete) => {
                return Ok(Captured {
                    message_id: crate::parse(&stored_message)?,
                    conversation_id,
                    duplicate: true,
                });
            }
            Some(SmsAcquisitionState::Pending) => {}
            _ => return Err(Error::InvalidRequest("SMS acquisition unavailable")),
        }
        let input: SmsAcquisitionInput = crate::decode(encoded.as_bytes())?;
        match reconcile_provider_sms(&tx, &ctx.device_id, &evidence(&input))? {
            SmsReconciliation::Matched(existing)
                if existing.message_id.to_string() != stored_message =>
            {
                forget_carrier_wire_bodies_for_message(&tx, existing.message_id)?;
                publish_reconciliation_alias(&tx, &ctx, &existing.message_id, &input)?;
                link_matched_thread(
                    &tx,
                    &ctx.device_id.to_string(),
                    &input,
                    existing.conversation_id,
                )?;
                tx.execute("UPDATE sms_acquisitions SET conversation_id=?,source_sequence=?,state='complete',reason=NULL,message_id=? WHERE acquisition_id=?", params![existing.conversation_id.to_string(), crate::to_i64(existing.source_sequence.0)?, existing.message_id.to_string(), id])?;
                persist_alias_for_id(&tx, &input, &existing.message_id.to_string())?;
                if crate::phone_read::observe_captured_read(
                    &tx,
                    &ctx,
                    crate::phone_read::CapturedRead {
                        transport: crate::phone_read::PhoneReadTransport::Sms,
                        provider_read: input.provider_read,
                        incoming: input.direction == Direction::Incoming,
                        imported: input.imported,
                        message_id: existing.message_id,
                        message_exists: true,
                    },
                )? {
                    crate::seal_pending(&tx, &ctx, crate::MAX_SEAL_BATCH)?;
                }
                tx.commit()?;
                return Ok(Captured {
                    message_id: existing.message_id,
                    conversation_id: existing.conversation_id,
                    duplicate: true,
                });
            }
            SmsReconciliation::Ambiguous => {
                tx.execute("UPDATE sms_acquisitions SET state='unavailable',reason=? WHERE acquisition_id=?", params![SMS_REASON_AMBIGUOUS, id])?;
                tx.commit()?;
                return Err(Error::InvalidRequest("SMS reconciliation ambiguous"));
            }
            _ => {}
        }
        let provider_id = namespaced_id(&input);
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM messages WHERE source_device_id=? AND provider_message_id=?",
                params![ctx.device_id.to_string(), provider_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(message) = existing {
            tx.execute("UPDATE sms_acquisitions SET state='complete',reason=NULL,message_id=? WHERE acquisition_id=?", params![message,id])?;
            persist_alias_for_id(&tx, &input, &message)?;
            let message_id = crate::parse(&message)?;
            if crate::phone_read::observe_captured_read(
                &tx,
                &ctx,
                crate::phone_read::CapturedRead {
                    transport: crate::phone_read::PhoneReadTransport::Sms,
                    provider_read: input.provider_read,
                    incoming: input.direction == Direction::Incoming,
                    imported: input.imported,
                    message_id,
                    message_exists: true,
                },
            )? {
                crate::seal_pending(&tx, &ctx, crate::MAX_SEAL_BATCH)?;
            }
            tx.commit()?;
            return Ok(Captured {
                message_id,
                conversation_id,
                duplicate: true,
            });
        }
        let (sender_address, recipients) = match input.direction {
            Direction::Incoming => (input.sender_address.clone(), input.recipients.clone()),
            Direction::Outgoing => (None, input.recipients.clone()),
        };
        let message_id: MessageId = crate::parse(&stored_message)?;
        let payload = MessagePayload {
            reply_to: None,
            record: MessageRecord {
                message_id,
                conversation_id,
                source_sequence: SourceSequence(
                    u64::try_from(sequence).map_err(|_| Error::Database)?,
                ),
                attachments: Vec::new(),
            },
            source_device_id: ctx.device_id,
            provider_message_id: Some(provider_id),
            sender_address,
            recipients,
            subject: None,
            body: input.body.clone(),
            transport: Transport::Sms,
            direction: input.direction,
            imported: input.imported,
            sent_at_ms: display_time(&input),
            ordered_at_ms: display_time(&input)
                .is_none()
                .then(crate::notifications::now_ms),
            mms_context: None,
            rcs_capture: false,
        };
        if let Some(message_id) = crate::message_origin::claim(&tx, &payload, true)? {
            let (conversation_id, source_sequence) =
                crate::message_origin::message_identity(&tx, &message_id)?;
            link_matched_thread(&tx, &ctx.device_id.to_string(), &input, conversation_id)?;
            tx.execute("UPDATE sms_acquisitions SET state='complete',reason=NULL,message_id=?,conversation_id=?,source_sequence=? WHERE acquisition_id=?", params![message_id.to_string(), conversation_id.to_string(), crate::to_i64(source_sequence.0)?, id])?;
            persist_alias_for_id(&tx, &input, &message_id.to_string())?;
            if crate::phone_read::observe_captured_read(
                &tx,
                &ctx,
                crate::phone_read::CapturedRead {
                    transport: crate::phone_read::PhoneReadTransport::Sms,
                    provider_read: input.provider_read,
                    incoming: input.direction == Direction::Incoming,
                    imported: input.imported,
                    message_id,
                    message_exists: true,
                },
            )? {
                crate::seal_pending(&tx, &ctx, crate::MAX_SEAL_BATCH)?;
            }
            tx.commit()?;
            return Ok(Captured {
                message_id,
                conversation_id,
                duplicate: true,
            });
        }
        crate::phone_read::observe_captured_read(
            &tx,
            &ctx,
            crate::phone_read::CapturedRead {
                transport: crate::phone_read::PhoneReadTransport::Sms,
                provider_read: input.provider_read,
                incoming: input.direction == Direction::Incoming,
                imported: input.imported,
                message_id,
                message_exists: false,
            },
        )?;
        match crate::insert_message(&tx, &payload)? {
            Inserted::New => {}
            Inserted::Same => return Err(Error::Database),
            Inserted::Conflict => return Err(Error::Conflict),
        }
        crate::message_origin::mark_claimed(&tx, &message_id)?;
        crate::enqueue(
            &tx,
            &ctx,
            crate::EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::Message(payload),
        )?;
        tx.execute(
            "UPDATE sms_acquisitions SET state='complete',reason=NULL WHERE acquisition_id=?",
            params![id],
        )?;
        persist_alias_for_id(&tx, &input, &message_id.to_string())?;
        tx.commit()?;
        Ok(Captured {
            message_id,
            conversation_id,
            duplicate: false,
        })
    }

    /// Open (not complete) acquisitions in durable local order.
    pub fn sms_acquisitions(&self, limit: usize) -> Result<Vec<SmsAcquisition>, Error> {
        let guard = self.lock()?;
        let mut statement = guard.conn.prepare("SELECT acquisition_id,conversation_id,input,state,reason FROM sms_acquisitions WHERE state!='complete' ORDER BY local_order LIMIT ?")?;
        statement
            .query_map(
                params![limit.min(MAX_PENDING_SMS_ACQUISITIONS) as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(decode_row)
            .collect()
    }
    /// Pending acquisitions after a durable cursor, wrapping once so recheck rotates fairly.
    pub fn sms_acquisitions_after(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SmsAcquisition>, Error> {
        let guard = self.lock()?;
        let after = after.unwrap_or("");
        // Keep literal state='pending': a bound parameter would defeat the partial index.
        let mut statement = guard.conn.prepare("SELECT acquisition_id,conversation_id,input,state,reason FROM sms_acquisitions WHERE state='pending' AND acquisition_id>? ORDER BY acquisition_id LIMIT ?")?;
        let rows: Vec<_> = statement
            .query_map(
                params![after, limit.min(MAX_PENDING_SMS_ACQUISITIONS) as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?
            .collect::<Result<_, _>>()?;
        drop(statement);
        drop(guard);
        if rows.is_empty() && !after.is_empty() {
            return self.sms_acquisitions_after(None, limit);
        }
        rows.into_iter().map(decode_row).collect()
    }
    pub fn set_sms_acquisition_state(
        &self,
        id: &str,
        state: SmsAcquisitionState,
        reason: Option<&str>,
    ) -> Result<(), Error> {
        if state == SmsAcquisitionState::Complete {
            return Err(Error::InvalidRequest("SMS acquisition state"));
        }
        let guard = self.lock()?;
        let changed = guard.conn.execute("UPDATE sms_acquisitions SET state=?,reason=? WHERE acquisition_id=? AND state!='complete'", params![state.code(),reason,id])?;
        if changed == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }
    pub fn sms_scan_status(
        &self,
        generation: &str,
        subscription: &str,
        imported: bool,
    ) -> Result<SmsScanStatus, Error> {
        let guard = self.lock()?;
        guard.conn.query_row("SELECT provider_message_id,history_complete,paused_reason FROM sms_scan_checkpoints WHERE source_generation=? AND subscription_id=? AND imported=?",params![generation,subscription,imported],|r| Ok(SmsScanStatus { checkpoint:r.get::<_,String>(0).map(|v| (!v.is_empty()).then_some(v))?, history_complete:r.get(1)?, paused:r.get::<_,Option<String>>(2)?.is_some(), reason:r.get(2)? })).optional().map(|v| v.unwrap_or(SmsScanStatus { checkpoint:None,history_complete:false,paused:false,reason:None })).map_err(Into::into)
    }
    /// Advances one monotonic checkpoint. A capacity pause is not cleared here: only a plan that
    /// finds capacity available again resumes history.
    pub fn set_sms_scan_checkpoint(
        &self,
        generation: &str,
        subscription: &str,
        imported: bool,
        provider_id: &str,
    ) -> Result<(), Error> {
        validate_scope(generation, subscription)?;
        if crate::history_import::canonical_provider_id(provider_id).is_none() {
            return Err(Error::InvalidRequest("SMS checkpoint"));
        }
        let mut guard = self.lock()?;
        let row: Option<DescendingCheckpoint> = guard.conn.query_row("SELECT provider_message_id,descending,history_floor,descending_cursor,history_upper_bound FROM sms_scan_checkpoints WHERE source_generation=? AND subscription_id=? AND imported=?", params![generation, subscription, imported], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))).optional()?;
        match row {
            Some((_, true, floor, current, Some(upper))) => {
                if crate::history_import::advances_descending_cursor(
                    current.as_deref(),
                    floor.as_deref(),
                    &upper,
                    provider_id,
                )? {
                    let tx = guard
                        .conn
                        .transaction_with_behavior(TransactionBehavior::Immediate)?;
                    tx.execute("UPDATE sms_scan_checkpoints SET descending_cursor=?,history_seed_remaining=MAX(history_seed_remaining-1,0) WHERE source_generation=? AND subscription_id=? AND imported=1", params![provider_id, generation, subscription])?;
                    tx.commit()?;
                }
            }
            // A corrupt descending scope must fail closed; treating it as an old ascending scope
            // would move the legacy checkpoint into rows the history scan has not reached.
            Some((_, true, _, _, None)) => return Err(Error::Database),
            Some((current, false, _, _, _)) => {
                let candidate = crate::history_import::canonical_provider_id(provider_id)
                    .ok_or(Error::InvalidRequest("SMS checkpoint"))?;
                let advances = current.is_empty()
                    || candidate > crate::history_import::stored_provider_id(&current)?;
                if advances {
                    guard.conn.execute(
                        "UPDATE sms_scan_checkpoints SET provider_message_id=? WHERE source_generation=? AND subscription_id=? AND imported=?",
                        params![provider_id, generation, subscription, imported],
                    )?;
                }
            }
            None => {
                guard.conn.execute(
                    "INSERT INTO sms_scan_checkpoints(source_generation,subscription_id,imported,provider_message_id) VALUES(?,?,?,?)",
                    params![generation, subscription, imported, provider_id],
                )?;
            }
        }
        Ok(())
    }
    /// Returns the live and history plans. The first call atomically freezes the observed
    /// high-water mark as the history bound and seeds the live checkpoint at that same mark, so
    /// pre-existing rows are only ever imported history (read, no banners).
    pub fn plan_sms_scans(
        &self,
        generation: &str,
        scope: &str,
        observed_high_water: &str,
    ) -> Result<Vec<SmsScanPlan>, Error> {
        self.plan_sms_scans_with_history_conditions(
            generation,
            scope,
            observed_high_water,
            HistoryImportConditions::default(),
        )
    }

    pub fn plan_sms_scans_with_history_conditions(
        &self,
        generation: &str,
        scope: &str,
        observed_high_water: &str,
        conditions: HistoryImportConditions,
    ) -> Result<Vec<SmsScanPlan>, Error> {
        validate_scope(generation, scope)?;
        if crate::history_import::canonical_provider_id(observed_high_water).is_none() {
            return Err(Error::InvalidRequest("SMS high-water mark"));
        }
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        normalize_stored_history_state(&tx, generation, scope)?;
        tx.execute("INSERT INTO sms_scan_checkpoints(source_generation,subscription_id,imported,provider_message_id,history_upper_bound) VALUES(?1,?2,0,?3,NULL) ON CONFLICT(source_generation,subscription_id,imported) DO NOTHING", params![generation, scope, observed_high_water])?;
        tx.execute("INSERT INTO sms_scan_checkpoints(source_generation,subscription_id,imported,provider_message_id,history_upper_bound,descending) VALUES(?1,?2,1,'',?3,1) ON CONFLICT(source_generation,subscription_id,imported) DO NOTHING", params![generation, scope, observed_high_water])?;
        let paused: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sms_scan_checkpoints WHERE source_generation=? AND imported=1 AND paused_reason=?)", params![generation, PAUSE_RECOVERY_CAPACITY], |r| r.get(0))?;
        if paused && !recovery_capacity_reached(&tx, &LIMITS)? {
            tx.execute("UPDATE sms_scan_checkpoints SET paused_reason=NULL WHERE source_generation=? AND imported=1 AND paused_reason=?", params![generation, PAUSE_RECOVERY_CAPACITY])?;
        }
        let history_incomplete: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sms_scan_checkpoints WHERE source_generation=? AND subscription_id=? AND imported=1 AND history_complete=0)", params![generation, scope], |row| row.get(0))?;
        let gate_open = if history_incomplete {
            crate::initial_replay::caught_up(&tx)?
                && crate::history_import::history_import_gate_open(
                    &tx,
                    crate::notifications::now_ms(),
                )?
        } else {
            true
        };
        let outbox_backpressured =
            history_incomplete && crate::history_import::outbox_backpressures_history(&tx)?;
        let live_gap = open_live_gap(&tx, generation, scope, observed_high_water)?;
        let mut statement = tx.prepare("SELECT imported,provider_message_id,history_upper_bound,history_complete,paused_reason,pending_recheck_cursor,descending,history_floor,descending_cursor,history_seed_remaining FROM sms_scan_checkpoints WHERE source_generation=? AND subscription_id=? ORDER BY imported")?;
        let plans = statement
            .query_map(params![generation, scope], |r| {
                Ok(SmsScanPlan {
                    source_generation: generation.into(),
                    subscription_id: scope.into(),
                    imported: r.get(0)?,
                    checkpoint: {
                        let v: String = r.get(1)?;
                        if r.get::<_, bool>(6)? {
                            r.get(8)?
                        } else {
                            (!v.is_empty()).then_some(v)
                        }
                    },
                    descending: r.get(6)?,
                    history_floor: r.get(7)?,
                    history_upper_bound: r.get(2)?,
                    history_complete: r.get(3)?,
                    paused: r.get::<_, Option<String>>(4)?.is_some()
                        || (r.get::<_, bool>(0)? && (!gate_open || outbox_backpressured)),
                    pending_recheck_cursor: r.get(5)?,
                    live_gap: (!r.get::<_, bool>(0)?).then(|| live_gap.clone()).flatten(),
                    budget: if r.get::<_, bool>(0)? {
                        let seed_remaining: i64 = r.get(9)?;
                        if r.get::<_, bool>(6)? && seed_remaining > 0 {
                            seed_remaining.min(INITIAL_HISTORY_SEED) as u64
                        } else if conditions.unmetered_network || conditions.charging {
                            ACCELERATED_HISTORY_PAGE as u64
                        } else {
                            ORDINARY_HISTORY_PAGE as u64
                        }
                    } else {
                        ORDINARY_HISTORY_PAGE as u64
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        tx.commit()?;
        Ok(plans)
    }
    /// Commits one durable page of the independently fenced descending live-gap scan.
    pub fn commit_sms_live_gap_page(
        &self,
        generation: &str,
        scope: &str,
        token: &str,
        expected_cursor: Option<&str>,
        last_durable_id: Option<&str>,
        exhausted: bool,
    ) -> Result<bool, Error> {
        validate_scope(generation, scope)?;
        if token.is_empty() || token.len() > 256 {
            return Err(Error::InvalidRequest("SMS live gap token"));
        }
        for id in [expected_cursor, last_durable_id].into_iter().flatten() {
            if crate::history_import::canonical_provider_id(id).is_none() {
                return Err(Error::InvalidRequest("SMS live gap cursor"));
            }
        }
        if !exhausted && last_durable_id.is_none() {
            return Err(Error::InvalidRequest("SMS live gap page"));
        }
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(gap) = live_gap(&tx, generation, scope)? else {
            tx.commit()?;
            return Ok(false);
        };
        if gap.token != token || gap.cursor.as_deref() != expected_cursor {
            tx.commit()?;
            return Ok(false);
        }
        if let Some(last) = last_durable_id
            && !crate::history_import::advances_descending_cursor(
                gap.cursor.as_deref(),
                Some(&gap.floor),
                &gap.upper_bound,
                last,
            )?
        {
            tx.commit()?;
            return Ok(false);
        }
        if exhausted {
            tx.execute(
                "DELETE FROM sms_live_gaps WHERE source_generation=? AND subscription_id=? AND token=?",
                params![generation, scope, token],
            )?;
        } else {
            tx.execute(
                "UPDATE sms_live_gaps SET cursor=? WHERE source_generation=? AND subscription_id=? AND token=? AND cursor IS ?",
                params![last_durable_id, generation, scope, token, expected_cursor],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }
    pub fn set_sms_pending_recheck_cursor(
        &self,
        generation: &str,
        scope: &str,
        imported: bool,
        cursor: Option<&str>,
    ) -> Result<(), Error> {
        validate_scope(generation, scope)?;
        self.lock()?.conn.execute("UPDATE sms_scan_checkpoints SET pending_recheck_cursor=? WHERE source_generation=? AND subscription_id=? AND imported=?",params![cursor,generation,scope,imported])?;
        Ok(())
    }
    /// Returns the active bounded full-provider sweep, starting a new one only when the previous
    /// one completed at least [`SMS_RECONCILIATION_INTERVAL_MS`] ago. The first sweep for a scope
    /// is deferred by one interval because initial history capture already observes every row.
    pub fn plan_sms_reconciliation(
        &self,
        generation: &str,
        scope: &str,
        observed_high_water: &str,
        live_anchor_fingerprint: Option<&str>,
    ) -> Result<Option<SmsReconciliationPlan>, Error> {
        self.plan_sms_reconciliation_at(
            generation,
            scope,
            observed_high_water,
            live_anchor_fingerprint,
            crate::notifications::now_ms(),
        )
    }
    #[doc(hidden)]
    pub fn plan_sms_reconciliation_at(
        &self,
        generation: &str,
        scope: &str,
        observed_high_water: &str,
        live_anchor_fingerprint: Option<&str>,
        now_ms: i64,
    ) -> Result<Option<SmsReconciliationPlan>, Error> {
        validate_scope(generation, scope)?;
        if crate::history_import::canonical_provider_id(observed_high_water).is_none() {
            return Err(Error::InvalidRequest("SMS reconciliation bound"));
        }
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        normalize_stored_history_state(&tx, generation, scope)?;
        tx.execute(
            "INSERT INTO sms_reconciliation_scans(source_generation,subscription_id,scan_cursor,upper_bound,completed_at_ms,descending) VALUES(?,?,NULL,'',?,1) ON CONFLICT(source_generation,subscription_id) DO NOTHING",
            params![generation, scope, now_ms],
        )?;
        let (cursor, upper_bound, completed, descending): (Option<String>, String, Option<i64>, bool) = tx.query_row(
            "SELECT scan_cursor,upper_bound,completed_at_ms,descending FROM sms_reconciliation_scans WHERE source_generation=? AND subscription_id=?",
            params![generation, scope],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        if !upper_bound.is_empty() && !descending {
            // Legacy scans advanced upward. Re-observe their frozen region from the top instead.
            let floor: Option<String> = tx
                .query_row(
                    "SELECT history_upper_bound FROM sms_scan_checkpoints WHERE source_generation=? AND subscription_id=? AND imported=1 AND history_complete=0",
                    params![generation, scope],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            tx.execute(
                "UPDATE sms_reconciliation_scans SET scan_cursor=NULL,floor=?,descending=1 WHERE source_generation=? AND subscription_id=?",
                params![floor, generation, scope],
            )?;
        }
        let gap_floor = live_gap(&tx, generation, scope)?.map(|gap| gap.floor);
        let reused = provider_ids_reused(
            &tx,
            generation,
            scope,
            observed_high_water,
            live_anchor_fingerprint,
        )?;
        let clock_rebased = completed.is_some_and(|at| now_ms < at);
        let full_sweep_due = completed.is_none_or(|at| {
            !clock_rebased && now_ms.saturating_sub(at) >= SMS_RECONCILIATION_INTERVAL_MS
        });
        let (cursor, upper_bound, floor) = if !upper_bound.is_empty() {
            (
                if descending { cursor } else { None },
                upper_bound,
                reconciliation_floor(&tx, generation, scope)?,
            )
        } else if reused.is_none() && !full_sweep_due && !clock_rebased {
            tx.commit()?;
            return Ok(None);
        } else {
            let local_cursor = if full_sweep_due {
                None
            } else if clock_rebased {
                crate::history_import::canonical_provider_id(observed_high_water)
                    .map(|high_water| high_water.saturating_sub(LOCAL_REUSE_WINDOW))
            } else {
                match reused {
                    Some(ProviderIdReuse::AnchorMismatch { live }) => {
                        advance_live_checkpoint(&tx, generation, scope, observed_high_water, true)?;
                        Some(live.saturating_sub(LOCAL_REUSE_WINDOW))
                    }
                    Some(ProviderIdReuse::HighWaterRegression { live, high_water })
                        if live - high_water <= LOCAL_REUSE_WINDOW =>
                    {
                        advance_live_checkpoint(&tx, generation, scope, observed_high_water, true)?;
                        Some(high_water.saturating_sub(LOCAL_REUSE_WINDOW))
                    }
                    _ => None,
                }
            };
            if let Some(cursor) = local_cursor {
                let cursor = (cursor != 0).then(|| cursor.to_string());
                let floor = floor_covering_gap(cursor.clone(), gap_floor.clone())?;
                tx.execute(
                    "UPDATE sms_reconciliation_scans SET scan_cursor=NULL,upper_bound=?,floor=?,descending=1 WHERE source_generation=? AND subscription_id=?",
                    params![observed_high_water, floor, generation, scope],
                )?;
                (None, observed_high_water.to_owned(), floor)
            } else {
                let floor: Option<String> = tx
                    .query_row(
                        "SELECT history_upper_bound FROM sms_scan_checkpoints WHERE source_generation=? AND subscription_id=? AND imported=1 AND history_complete=0",
                        params![generation, scope],
                        |row| row.get(0),
                    )
                    .optional()?
                    .flatten();
                let floor = floor_covering_gap(floor, gap_floor)?;
                tx.execute(
                    "UPDATE sms_reconciliation_scans SET scan_cursor=NULL,upper_bound=?,floor=?,descending=1,completed_at_ms=NULL WHERE source_generation=? AND subscription_id=?",
                    params![observed_high_water, floor, generation, scope],
                )?;
                // After a high-water regression the sweep covers every row up to the observed mark;
                // the live scan resumes above it so later rows with reused lower IDs are not skipped.
                if let Some(ProviderIdReuse::HighWaterRegression { .. }) = reused {
                    advance_live_checkpoint(&tx, generation, scope, observed_high_water, true)?;
                }
                (None, observed_high_water.to_owned(), floor)
            }
        };
        let history: Option<(String, bool)> = tx
            .query_row(
                "SELECT history_upper_bound,history_complete FROM sms_scan_checkpoints WHERE source_generation=? AND subscription_id=? AND imported=1",
                params![generation, scope],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let history_upper_bound = history.as_ref().map(|(bound, _)| bound.clone());
        tx.commit()?;
        Ok(Some(SmsReconciliationPlan {
            source_generation: generation.into(),
            subscription_id: scope.into(),
            cursor,
            upper_bound,
            floor,
            history_upper_bound,
        }))
    }
    /// Acknowledges a successfully observed page. Exhaustion records the completion time; the
    /// next sweep starts only after the restart interval.
    pub fn complete_sms_reconciliation(
        &self,
        generation: &str,
        scope: &str,
        upper_bound: &str,
        cursor: Option<&str>,
        exhausted: bool,
    ) -> Result<(), Error> {
        validate_scope(generation, scope)?;
        if crate::history_import::canonical_provider_id(upper_bound).is_none() {
            return Err(Error::InvalidRequest("SMS reconciliation bound"));
        }
        if cursor.is_some_and(|value| crate::history_import::canonical_provider_id(value).is_none())
        {
            return Err(Error::InvalidRequest("SMS reconciliation cursor"));
        }
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        normalize_stored_history_state(&tx, generation, scope)?;
        if exhausted {
            tx.execute(
                "UPDATE sms_reconciliation_scans SET scan_cursor=NULL,upper_bound='',completed_at_ms=COALESCE(completed_at_ms,?) WHERE source_generation=? AND subscription_id=? AND upper_bound=?",
                params![crate::notifications::now_ms(), generation, scope, upper_bound],
            )?;
        } else {
            let floor: Option<String> = tx.query_row(
                "SELECT floor FROM sms_reconciliation_scans WHERE source_generation=? AND subscription_id=? AND upper_bound=?",
                params![generation, scope, upper_bound],
                |row| row.get(0),
            ).optional()?.flatten();
            let Some(cursor) = cursor else {
                return Err(Error::InvalidRequest("SMS reconciliation cursor"));
            };
            let stored: Option<String> = tx.query_row(
                "SELECT scan_cursor FROM sms_reconciliation_scans WHERE source_generation=? AND subscription_id=? AND upper_bound=?",
                params![generation, scope, upper_bound],
                |row| row.get(0),
            ).optional()?.flatten();
            if !crate::history_import::advances_descending_cursor(
                stored.as_deref(),
                floor.as_deref(),
                upper_bound,
                cursor,
            )? {
                return Err(Error::InvalidRequest("SMS reconciliation cursor"));
            }
            tx.execute(
                "UPDATE sms_reconciliation_scans SET scan_cursor=? WHERE source_generation=? AND subscription_id=? AND upper_bound=?",
                params![cursor, generation, scope, upper_bound],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn ack_sms_scan_exhausted(
        &self,
        generation: &str,
        scope: &str,
        imported: bool,
        bound: &str,
    ) -> Result<(), Error> {
        validate_scope(generation, scope)?;
        let bound_id = crate::history_import::canonical_provider_id(bound)
            .ok_or(Error::InvalidRequest("SMS scan bound"))?;
        let guard = self.lock()?;
        let stored: Option<String> = guard
            .conn
            .query_row(
                "SELECT history_upper_bound FROM sms_scan_checkpoints WHERE source_generation=? AND subscription_id=? AND imported=?",
                params![generation, scope, imported],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if stored
            .as_deref()
            .map(crate::history_import::stored_provider_id)
            .transpose()?
            == Some(bound_id)
        {
            guard.conn.execute(
                "UPDATE sms_scan_checkpoints SET history_complete=1 WHERE source_generation=? AND subscription_id=? AND imported=?",
                params![generation, scope, imported],
            )?;
        }
        Ok(())
    }
    /// Records only Android provider read/permission failures; core rejections and capacity
    /// pauses have their own states.
    pub fn set_sms_provider_error(
        &self,
        generation: &str,
        scope: &str,
        error: bool,
    ) -> Result<(), Error> {
        validate_scope(generation, scope)?;
        self.lock()?.conn.execute("UPDATE sms_scan_checkpoints SET provider_error=? WHERE source_generation=? AND subscription_id=?",params![error,generation,scope])?;
        Ok(())
    }
    pub fn sms_history_status(
        &self,
        generation: &str,
        scope: &str,
    ) -> Result<SmsHistoryStatus, Error> {
        validate_scope(generation, scope)?;
        let guard = self.lock()?;
        let history: Option<(bool, bool)> = guard.conn.query_row("SELECT history_complete,paused_reason IS NOT NULL FROM sms_scan_checkpoints WHERE source_generation=? AND subscription_id=? AND imported=1", params![generation, scope], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        let provider_error: bool = guard.conn.query_row("SELECT EXISTS(SELECT 1 FROM sms_scan_checkpoints WHERE source_generation=? AND subscription_id=? AND provider_error!=0)", params![generation, scope], |r| r.get(0))?;
        let (pending, ambiguous): (i64, i64) = guard.conn.query_row(
            &format!("SELECT COALESCE(SUM(state='pending' AND COALESCE(reason,'') NOT IN {PARKED}),0),COALESCE(SUM(state='unavailable' AND reason=?2),0) FROM sms_acquisitions WHERE state!='complete' AND source_generation=?1"),
            params![generation, SMS_REASON_AMBIGUOUS],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let (complete, paused) = history.unwrap_or((false, false));
        Ok(SmsHistoryStatus {
            baseline_initialized: history.is_some(),
            scanning: history.is_some() && !complete,
            history_complete: complete,
            paused,
            provider_error,
            pending_count: u64::try_from(pending).map_err(|_| Error::Database)?,
            ambiguous_count: u64::try_from(ambiguous).map_err(|_| Error::Database)?,
        })
    }
}

fn normalize_stored_history_state(
    conn: &Connection,
    generation: &str,
    scope: &str,
) -> Result<(), Error> {
    let mut statement = conn.prepare(
        "SELECT imported,provider_message_id,history_upper_bound,history_floor,descending_cursor FROM sms_scan_checkpoints WHERE source_generation=? AND subscription_id=?",
    )?;
    let rows = statement
        .query_map(params![generation, scope], |row| {
            Ok((
                row.get::<_, bool>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    let normalize_optional = |value: Option<String>| -> Result<Option<String>, Error> {
        value
            .map(|value| {
                crate::history_import::normalized_provider_id(&value).ok_or(Error::Database)
            })
            .transpose()
    };
    for (imported, checkpoint, upper, floor, cursor) in rows {
        let checkpoint = if checkpoint.is_empty() {
            checkpoint
        } else {
            crate::history_import::normalized_provider_id(&checkpoint).ok_or(Error::Database)?
        };
        conn.execute(
            "UPDATE sms_scan_checkpoints SET provider_message_id=?,history_upper_bound=?,history_floor=?,descending_cursor=? WHERE source_generation=? AND subscription_id=? AND imported=?",
            params![
                checkpoint,
                normalize_optional(upper)?,
                normalize_optional(floor)?,
                normalize_optional(cursor)?,
                generation,
                scope,
                imported
            ],
        )?;
    }

    let reconciliation: Option<(Option<String>, String, Option<String>)> = conn
        .query_row(
            "SELECT scan_cursor,upper_bound,floor FROM sms_reconciliation_scans WHERE source_generation=? AND subscription_id=?",
            params![generation, scope],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((cursor, upper, floor)) = reconciliation {
        let upper = if upper.is_empty() {
            upper
        } else {
            crate::history_import::normalized_provider_id(&upper).ok_or(Error::Database)?
        };
        conn.execute(
            "UPDATE sms_reconciliation_scans SET scan_cursor=?,upper_bound=?,floor=? WHERE source_generation=? AND subscription_id=?",
            params![normalize_optional(cursor)?, upper, normalize_optional(floor)?, generation, scope],
        )?;
    }
    if let Some(gap) = live_gap(conn, generation, scope)? {
        conn.execute(
            "UPDATE sms_live_gaps SET upper_bound=?,floor=?,cursor=? WHERE source_generation=? AND subscription_id=?",
            params![
                crate::history_import::normalized_provider_id(&gap.upper_bound).ok_or(Error::Database)?,
                crate::history_import::normalized_provider_id(&gap.floor).ok_or(Error::Database)?,
                normalize_optional(gap.cursor)?,
                generation,
                scope,
            ],
        )?;
    }
    Ok(())
}

fn reconciliation_floor(
    conn: &Connection,
    generation: &str,
    scope: &str,
) -> Result<Option<String>, Error> {
    conn.query_row(
        "SELECT floor FROM sms_reconciliation_scans WHERE source_generation=? AND subscription_id=?",
        params![generation, scope],
        |row| row.get(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(Into::into)
}

/// Extends a restricted sweep to cover an open gap. `None` is an unrestricted sweep floor,
/// while a missing gap is also `None`; an unrestricted sweep must never be narrowed by a gap.
fn floor_covering_gap(
    sweep_floor: Option<String>,
    gap_floor: Option<String>,
) -> Result<Option<String>, Error> {
    match (sweep_floor, gap_floor) {
        (Some(sweep), Some(gap)) => Ok((crate::history_import::stored_provider_id(&sweep)?
            <= crate::history_import::stored_provider_id(&gap)?)
        .then_some(sweep)
        .or(Some(gap))),
        (sweep_floor, None) => Ok(sweep_floor),
        (None, Some(_)) => Ok(None),
    }
}

fn live_gap(
    conn: &Connection,
    generation: &str,
    scope: &str,
) -> Result<Option<SmsLiveGapPlan>, Error> {
    conn.query_row(
        "SELECT upper_bound,floor,cursor,token FROM sms_live_gaps WHERE source_generation=? AND subscription_id=?",
        params![generation, scope],
        |row| {
            Ok(SmsLiveGapPlan {
                source_generation: generation.into(),
                subscription_id: scope.into(),
                upper_bound: row.get(0)?,
                floor: row.get(1)?,
                cursor: row.get(2)?,
                token: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn open_live_gap(
    conn: &Connection,
    generation: &str,
    scope: &str,
    observed_high_water: &str,
) -> Result<Option<SmsLiveGapPlan>, Error> {
    if let Some(gap) = live_gap(conn, generation, scope)? {
        return Ok(Some(gap));
    }
    let live: String = conn.query_row(
        "SELECT provider_message_id FROM sms_scan_checkpoints WHERE source_generation=? AND subscription_id=? AND imported=0",
        params![generation, scope],
        |row| row.get(0),
    )?;
    let live_id = crate::history_import::stored_provider_id(&live)?;
    let high_water = crate::history_import::canonical_provider_id(observed_high_water)
        .ok_or(Error::InvalidRequest("SMS high-water mark"))?;
    if high_water.saturating_sub(live_id) <= LIVE_GAP_THRESHOLD_IDS {
        return Ok(None);
    }
    let gap = SmsLiveGapPlan {
        source_generation: generation.into(),
        subscription_id: scope.into(),
        upper_bound: observed_high_water.into(),
        floor: live,
        cursor: None,
        token: uuid::Uuid::new_v4().to_string(),
    };
    conn.execute(
        "INSERT INTO sms_live_gaps(source_generation,subscription_id,upper_bound,floor,cursor,token) VALUES(?,?,?,?,NULL,?)",
        params![generation, scope, gap.upper_bound, gap.floor, gap.token],
    )?;
    advance_live_checkpoint(conn, generation, scope, observed_high_water, false)?;
    Ok(Some(gap))
}

fn advance_live_checkpoint(
    conn: &Connection,
    generation: &str,
    scope: &str,
    checkpoint: &str,
    close_gap: bool,
) -> Result<(), Error> {
    conn.execute(
        "UPDATE sms_scan_checkpoints SET provider_message_id=? WHERE source_generation=? AND subscription_id=? AND imported=0",
        params![checkpoint, generation, scope],
    )?;
    if close_gap {
        conn.execute(
            "DELETE FROM sms_live_gaps WHERE source_generation=? AND subscription_id=?",
            params![generation, scope],
        )?;
    }
    Ok(())
}

/// Reused provider IDs are evident when the high-water mark falls below the live checkpoint, or
/// when the row now at the live checkpoint carries evidence different from what was acquired for
/// that ID. A checkpoint row never acquired yet (a fresh baseline) is not evidence of reuse.
#[derive(Clone, Copy)]
enum ProviderIdReuse {
    AnchorMismatch { live: u64 },
    HighWaterRegression { live: u64, high_water: u64 },
}

fn provider_ids_reused(
    conn: &Connection,
    generation: &str,
    scope: &str,
    observed_high_water: &str,
    live_anchor_fingerprint: Option<&str>,
) -> Result<Option<ProviderIdReuse>, Error> {
    let Some(live) = conn
        .query_row(
            "SELECT provider_message_id FROM sms_scan_checkpoints WHERE source_generation=? AND subscription_id=? AND imported=0",
            params![generation, scope],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let live = crate::history_import::stored_provider_id(&live)?;
    let high_water = crate::history_import::canonical_provider_id(observed_high_water)
        .ok_or(Error::InvalidRequest("SMS reconciliation bound"))?;
    if high_water < live {
        return Ok(Some(ProviderIdReuse::HighWaterRegression {
            live,
            high_water,
        }));
    }
    let Some(anchor) = live_anchor_fingerprint else {
        return Ok(None);
    };
    let (known, seen): (bool, bool) = conn.query_row(
        "SELECT COALESCE(MAX(source_row_fingerprint=?3),0),COUNT(*)>0 FROM sms_acquisitions WHERE source_generation=?1 AND provider_message_id=?2",
        params![generation, live.to_string(), anchor],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((seen && !known).then_some(ProviderIdReuse::AnchorMismatch { live }))
}

/// Finds the message an `SMS_RECEIVED` broadcast duplicates: a redelivery (same broadcast ID),
/// or a provider row captured first with the same SMSC time, sender and exact body that no
/// broadcast has claimed yet. A provider-first match records the alias so redelivery stays a
/// duplicate.
pub(crate) fn broadcast_duplicate(
    conn: &Connection,
    device: &crate::DeviceId,
    broadcast_id: &str,
    sms: &IncomingSms,
    smsc_at_ms: Option<i64>,
) -> Result<Option<Captured>, Error> {
    let aliased: Option<String> = conn
        .query_row(
            "SELECT message_id FROM sms_broadcast_aliases WHERE broadcast_id=?",
            params![broadcast_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(message) = aliased {
        return captured_duplicate(conn, &message).map(Some);
    }
    let Some(smsc) = smsc_at_ms else {
        return Ok(None);
    };
    let sender = canonical_counterpart(conn, &sms.sender_address)?;
    let mut statement = conn.prepare(
        "SELECT a.message_id,m.payload FROM sms_acquisitions a JOIN messages m ON m.id=a.message_id
         WHERE a.evidence_time_ms=? AND a.direction='incoming' AND a.state='complete' AND m.source_device_id=?
           AND NOT EXISTS (SELECT 1 FROM sms_broadcast_aliases b WHERE b.message_id=a.message_id)
         ORDER BY m.local_order",
    )?;
    let rows = statement
        .query_map(params![smsc, device.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (message, encoded) in rows {
        let payload: MessagePayload = crate::decode(encoded.as_bytes())?;
        let same_sender = payload
            .sender_address
            .as_deref()
            .map(|address| canonical_counterpart(conn, address))
            .transpose()?
            .is_some_and(|address| address == sender);
        if payload.body == sms.body && same_sender {
            record_broadcast_alias(conn, broadcast_id, crate::parse(&message)?, Some(smsc))?;
            return captured_duplicate(conn, &message).map(Some);
        }
    }
    Ok(None)
}

pub(crate) fn record_broadcast_alias(
    conn: &Connection,
    broadcast_id: &str,
    message: MessageId,
    smsc_at_ms: Option<i64>,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO sms_broadcast_aliases(broadcast_id,message_id,smsc_at_ms) VALUES(?,?,?) ON CONFLICT(broadcast_id) DO NOTHING",
        params![broadcast_id, message.to_string(), smsc_at_ms],
    )?;
    Ok(())
}

fn captured_duplicate(conn: &Connection, message: &str) -> Result<Captured, Error> {
    let conversation: String = conn.query_row(
        "SELECT conversation_id FROM messages WHERE id=?",
        params![message],
        |r| r.get(0),
    )?;
    Ok(Captured {
        message_id: crate::parse(message)?,
        conversation_id: crate::parse(&conversation)?,
        duplicate: true,
    })
}

fn decode_row(
    (id, conversation, input, state, reason): (String, String, String, String, Option<String>),
) -> Result<SmsAcquisition, Error> {
    Ok(SmsAcquisition {
        acquisition_id: id,
        conversation_id: crate::parse(&conversation)?,
        input: crate::decode(input.as_bytes())?,
        state: SmsAcquisitionState::parse(&state).ok_or(Error::Database)?,
        reason,
    })
}
fn by_source(
    conn: &Connection,
    input: &SmsAcquisitionInput,
) -> Result<Option<SmsAcquisition>, Error> {
    conn.query_row("SELECT acquisition_id,conversation_id,input,state,reason FROM sms_acquisitions WHERE source_generation=? AND subscription_id=? AND source_row_fingerprint=? AND direction=?",params![input.source.source_generation,input.source.subscription_id,input.source_row_fingerprint,direction_code(input.direction)],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?.map(decode_row).transpose()
}
/// Re-observation of the same immutable provider evidence refreshes mutable fields (body,
/// `date_sent`, box-dependent addresses) of an open acquisition and makes it pending again so
/// the host reclassifies it. Completed acquisitions are immutable.
fn refresh(
    conn: &Connection,
    mut existing: SmsAcquisition,
    input: SmsAcquisitionInput,
    representable: bool,
) -> Result<SmsAcquisition, Error> {
    // Core-owned terminal outcomes survive re-observation of unchanged immutable evidence:
    // re-opening an ambiguous match would only re-derive the same ambiguity on every sweep.
    if existing.state == SmsAcquisitionState::Complete
        || existing.reason.as_deref() == Some(SMS_REASON_AMBIGUOUS)
    {
        return Ok(existing);
    }
    let (input, state, reason) = if representable {
        (input, SmsAcquisitionState::Pending, None)
    } else {
        (
            sanitized(input),
            SmsAcquisitionState::Unavailable,
            Some(SMS_REASON_INVALID.to_owned()),
        )
    };
    conn.execute(
        "UPDATE sms_acquisitions SET input=?,evidence_time_ms=?,state=?,reason=? WHERE acquisition_id=?",
        params![crate::json(&input)?, evidence_time(&input), state.code(), reason, existing.acquisition_id],
    )?;
    existing.input = input;
    existing.state = state;
    existing.reason = reason;
    Ok(existing)
}
/// A provider row whose immutable evidence changed (for example a failed send retried with a new
/// `date`) replaces any unpublished acquisition of the same provider ID instead of leaking it.
fn supersede(conn: &Connection, input: &SmsAcquisitionInput) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM sms_acquisitions WHERE source_generation=? AND provider_message_id=? AND source_row_fingerprint!=? AND state!='complete'",
        params![input.source.source_generation, input.source.provider_message_id, input.source_row_fingerprint],
    )?;
    Ok(())
}
fn pending_queue_full(
    conn: &Connection,
    input: &SmsAcquisitionInput,
    limits: &Limits,
) -> Result<bool, Error> {
    let (count, bytes): (i64, i64) = conn.query_row(
        &format!("SELECT COUNT(*),COALESCE(SUM(length(input)),0) FROM sms_acquisitions WHERE state='pending' AND COALESCE(reason,'') NOT IN {PARKED}"),
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let count = usize::try_from(count).map_err(|_| Error::Database)?;
    let bytes = usize::try_from(bytes).map_err(|_| Error::Database)?;
    Ok(count >= limits.pending || bytes + crate::json(input)?.len() > limits.pending_bytes)
}
/// Conservative, index-only estimate of what a recovery snapshot must carry: the message row-id
/// high-water mark bounds the retained message count from above, and the database page footprint
/// bounds every retained event, status, contact and notification byte.
fn recovery_capacity_reached(conn: &Connection, limits: &Limits) -> Result<bool, Error> {
    let records: i64 = conn.query_row(
        "SELECT COALESCE(MAX(local_order),0) FROM messages",
        [],
        |r| r.get(0),
    )?;
    let pages = pragma_integer(conn, "PRAGMA page_count")?;
    let page_size = pragma_integer(conn, "PRAGMA page_size")?;
    let records = u64::try_from(records).map_err(|_| Error::Database)?;
    let bytes = u64::try_from(pages.saturating_mul(page_size)).map_err(|_| Error::Database)?;
    Ok(records >= limits.recovery_records || bytes >= limits.recovery_bytes)
}
/// SQLCipher reports some pragmas (such as `page_size`, as `cipher_page_size`) as text.
fn pragma_integer(conn: &Connection, pragma: &str) -> Result<i64, Error> {
    match conn.query_row(pragma, [], |r| r.get::<_, rusqlite::types::Value>(0))? {
        rusqlite::types::Value::Integer(value) => Ok(value),
        rusqlite::types::Value::Text(value) => value.parse().map_err(|_| Error::Database),
        _ => Err(Error::Database),
    }
}
/// Pauses imported history for every provider scope of this source generation: history plans
/// read the provider scope while acquisitions carry SIM routes.
fn pause_history(conn: &Connection, generation: &str) -> Result<(), Error> {
    conn.execute(
        "UPDATE sms_scan_checkpoints SET paused_reason=? WHERE source_generation=? AND imported=1",
        params![PAUSE_RECOVERY_CAPACITY, generation],
    )?;
    Ok(())
}
fn normalized(mut input: SmsAcquisitionInput) -> SmsAcquisitionInput {
    input.sent_at_ms = input.sent_at_ms.filter(|time| *time > 0);
    input.date_sent_ms = input.date_sent_ms.filter(|time| *time > 0);
    input
}
/// Unrepresentable rows keep only their immutable source identity, never content.
fn sanitized(mut input: SmsAcquisitionInput) -> SmsAcquisitionInput {
    input.sender_address = None;
    input.recipients.clear();
    input.body.clear();
    input.source.broadcast_id = None;
    input
}
/// The time users see: provider `date` (local receipt or submission time, one clock for both
/// directions); `date_sent` only when `date` is unknown, because SMSC clocks can be skewed.
fn display_time(input: &SmsAcquisitionInput) -> Option<i64> {
    input.sent_at_ms.or(input.date_sent_ms)
}
fn evidence_time(input: &SmsAcquisitionInput) -> Option<i64> {
    (input.direction == Direction::Incoming)
        .then_some(input.date_sent_ms)
        .flatten()
}
fn validate_metadata(input: &SmsAcquisitionInput) -> Result<(), Error> {
    validate_scope(
        &input.source.source_generation,
        &input.source.subscription_id,
    )?;
    if input.source.provider_message_id.is_empty()
        || input.source.provider_message_id.len() > crate::MAX_PROVIDER_ID_BYTES
        || input.source_row_fingerprint.is_empty()
        || input.source_row_fingerprint.len() > crate::MAX_PROVIDER_ID_BYTES
        || input
            .source
            .provider_thread_id
            .as_ref()
            .is_some_and(|thread| thread.len() > crate::MAX_PROVIDER_ID_BYTES)
        || input
            .source
            .broadcast_id
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.len() > crate::MAX_PROVIDER_ID_BYTES)
    {
        return Err(Error::InvalidRequest("SMS provider metadata"));
    }
    Ok(())
}
fn validate_content(input: &SmsAcquisitionInput) -> Result<(), Error> {
    if input.body.len() > crate::MAX_BODY_BYTES {
        return Err(Error::InvalidRequest("SMS body"));
    }
    if input.direction == Direction::Incoming {
        crate::require_address(
            input
                .sender_address
                .as_deref()
                .ok_or(Error::InvalidRequest("SMS sender"))?,
        )?;
    } else if input.recipients.len() != 1 {
        return Err(Error::InvalidRequest("SMS recipients"));
    }
    for address in &input.recipients {
        crate::require_address(address)?;
    }
    Ok(())
}
fn evidence(input: &SmsAcquisitionInput) -> ProviderSmsEvidence {
    ProviderSmsEvidence {
        source_generation: input.source.source_generation.clone(),
        subscription_id: input.source.subscription_id.clone(),
        provider_message_id: input.source.provider_message_id.clone(),
        source_row_fingerprint: input.source_row_fingerprint.clone(),
        provider_thread_id: input.source.provider_thread_id.clone(),
        direction: input.direction,
        counterpart: match input.direction {
            Direction::Incoming => input.sender_address.clone().unwrap_or_default(),
            Direction::Outgoing => input.recipients.first().cloned().unwrap_or_default(),
        },
        body: input.body.clone(),
        creator: input.creator.clone(),
        date_ms: input.sent_at_ms,
        date_sent_ms: input.date_sent_ms,
        broadcast_id: input.source.broadcast_id.clone(),
    }
}
fn publish_reconciliation_alias(
    conn: &Connection,
    ctx: &crate::Ctx<'_>,
    message_id: &MessageId,
    input: &SmsAcquisitionInput,
) -> Result<(), Error> {
    let encoded: String = conn.query_row(
        "SELECT payload FROM messages WHERE id=?",
        params![message_id.to_string()],
        |row| row.get(0),
    )?;
    let mut matched: MessagePayload = crate::decode(encoded.as_bytes())?;
    if matched.direction == Direction::Outgoing && matched.sent_at_ms.is_none() {
        matched.sent_at_ms = display_time(input);
        if let Some(origin_key) = crate::message_origin::exact_key(conn, &matched)? {
            let already_published: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM message_origin_aliases WHERE origin_key=? AND message_id=?)",
                params![origin_key, message_id.to_string()],
                |row| row.get(0),
            )?;
            crate::message_origin::apply_alias(conn, message_id, &origin_key)?;
            if !already_published {
                crate::enqueue(
                    conn,
                    ctx,
                    crate::EnvelopePurpose::Event,
                    None,
                    None,
                    &PrivatePayload::MessageOriginAlias {
                        message_id: *message_id,
                        origin_key,
                    },
                )?;
            }
        }
    }
    Ok(())
}

fn matched_acquisition(
    input: &SmsAcquisitionInput,
    conversation_id: ConversationId,
) -> SmsAcquisition {
    SmsAcquisition {
        acquisition_id: uuid::Uuid::new_v4().to_string(),
        conversation_id,
        input: input.clone(),
        state: SmsAcquisitionState::Complete,
        reason: None,
    }
}
fn persist_acquisition(
    conn: &Connection,
    acquisition: &SmsAcquisition,
    message_id: MessageId,
    sequence: SourceSequence,
) -> Result<(), Error> {
    conn.execute("INSERT INTO sms_acquisitions(acquisition_id,source_generation,subscription_id,provider_message_id,source_row_fingerprint,direction,conversation_id,input,state,reason,message_id,source_sequence,evidence_time_ms,local_order) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,(SELECT COALESCE(MAX(local_order),0)+1 FROM sms_acquisitions))", params![acquisition.acquisition_id, acquisition.input.source.source_generation, acquisition.input.source.subscription_id, acquisition.input.source.provider_message_id, acquisition.input.source_row_fingerprint, direction_code(acquisition.input.direction), acquisition.conversation_id.to_string(), crate::json(&acquisition.input)?, acquisition.state.code(), acquisition.reason, message_id.to_string(), crate::to_i64(sequence.0)?, evidence_time(&acquisition.input)])?;
    Ok(())
}
fn persist_alias_for_id(
    conn: &Connection,
    input: &SmsAcquisitionInput,
    message_id: &str,
) -> Result<(), Error> {
    let Some(alias) = input.source.broadcast_id.as_deref() else {
        return Ok(());
    };
    conn.execute("INSERT INTO sms_provider_aliases(source_generation,subscription_id,alias,message_id) VALUES(?,?,?,?) ON CONFLICT(source_generation,subscription_id,alias) DO NOTHING", params![input.source.source_generation, input.source.subscription_id, alias, message_id])?;
    Ok(())
}
fn validate_scope(generation: &str, subscription: &str) -> Result<(), Error> {
    if generation.is_empty()
        || subscription.is_empty()
        || generation.len() > crate::MAX_PROVIDER_ID_BYTES
        || subscription.len() > crate::MAX_SUBSCRIPTION_BYTES
    {
        Err(Error::InvalidRequest("SMS source or subscription"))
    } else {
        Ok(())
    }
}
fn direction_code(direction: Direction) -> &'static str {
    match direction {
        Direction::Incoming => "incoming",
        Direction::Outgoing => "outgoing",
    }
}
fn namespaced_id(input: &SmsAcquisitionInput) -> String {
    format!(
        "sms:{}:{}:{}:{}",
        input.source.source_generation,
        input.source.subscription_id,
        input.source_row_fingerprint,
        direction_code(input.direction)
    )
}
fn counterpart_conversation(
    conn: &Connection,
    input: &SmsAcquisitionInput,
) -> Result<Option<ConversationId>, Error> {
    let address = match input.direction {
        Direction::Incoming => input.sender_address.as_deref(),
        Direction::Outgoing => input.recipients.first().map(String::as_str),
    };
    address
        .map(|address| crate::conversation_match::address_conversation(conn, address))
        .transpose()
        .map(Option::flatten)
}
fn thread_conversation(
    conn: &Connection,
    device: &str,
    source: &SmsSource,
) -> Result<Option<ConversationId>, Error> {
    let Some(thread) = source.provider_thread_id.as_deref() else {
        return Ok(None);
    };
    conn.query_row("SELECT conversation_id FROM mms_thread_conversations WHERE source_device_id=? AND source_generation=? AND subscription_id=? AND provider_thread_id=?",params![device,source.source_generation,source.subscription_id,thread],|r|r.get::<_,String>(0)).optional()?.map(|id|crate::parse(&id)).transpose()
}
fn link_thread(
    conn: &Connection,
    device: &str,
    source: &SmsSource,
    conversation: ConversationId,
) -> Result<(), Error> {
    let Some(thread) = source.provider_thread_id.as_deref() else {
        return Ok(());
    };
    conn.execute("INSERT INTO mms_thread_conversations(source_device_id,source_generation,subscription_id,provider_thread_id,conversation_id) VALUES(?,?,?,?,?) ON CONFLICT(source_device_id,source_generation,subscription_id,provider_thread_id) DO UPDATE SET conversation_id=excluded.conversation_id",params![device,source.source_generation,source.subscription_id,thread,conversation.to_string()])?;
    Ok(())
}

fn link_matched_thread(
    conn: &Connection,
    device: &str,
    input: &SmsAcquisitionInput,
    conversation: ConversationId,
) -> Result<(), Error> {
    if input.recipients.len() > 1 {
        return Ok(());
    }
    let address = match input.direction {
        Direction::Incoming => input.sender_address.as_deref(),
        Direction::Outgoing => input.recipients.first().map(String::as_str),
    };
    let Some(address) = address else {
        return Ok(());
    };
    if let Some(thread) = thread_conversation(conn, device, &input.source)?
        && !crate::conversation_match::carrier_conversation_eligible(conn, thread)?
    {
        return Ok(());
    }
    if crate::conversation_match::resolve_carrier_counterpart(conn, address, Some(conversation))?
        .is_some()
    {
        link_thread(conn, device, &input.source, conversation)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientConfig, Cursor, DatabaseKey, DeviceId, VaultId};

    fn client(dir: &tempfile::TempDir) -> Client {
        Client::open(
            ClientConfig {
                database_path: dir.path().join("capacity.db"),
                vault_id: VaultId::new(),
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[7; 32]).unwrap(),
        )
        .unwrap()
    }

    fn history_row(id: u32) -> SmsAcquisitionInput {
        SmsAcquisitionInput {
            source: SmsSource {
                source_generation: "install".into(),
                subscription_id: "sim-1".into(),
                provider_message_id: id.to_string(),
                provider_thread_id: None,
                broadcast_id: None,
            },
            direction: Direction::Outgoing,
            sender_address: None,
            recipients: vec!["+15555550100".into()],
            body: format!("history {id}"),
            imported: true,
            sent_at_ms: Some(1_600_000_000_000 + i64::from(id)),
            date_sent_ms: None,
            creator: None,
            provider_read: None,
            source_row_fingerprint: format!("fp-{id}"),
        }
    }

    #[test]
    fn recovery_capacity_pauses_history_in_the_provider_scope_until_capacity_returns() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        client.observe_server_high_water(Cursor(0)).unwrap();
        let tight = Limits {
            pending: MAX_PENDING_SMS_ACQUISITIONS,
            pending_bytes: MAX_PENDING_SMS_ACQUISITION_BYTES,
            recovery_records: 2,
            recovery_bytes: u64::MAX,
        };
        client
            .plan_sms_scans("install", "android-sms-provider", "10")
            .unwrap();
        for id in 1..=2 {
            let acquisition = client
                .begin_sms_acquisition_with(history_row(id), &tight)
                .unwrap();
            client
                .complete_sms_acquisition(&acquisition.acquisition_id)
                .unwrap();
        }
        assert_eq!(
            client.begin_sms_acquisition_with(history_row(3), &tight),
            Err(Error::SmsCapacity)
        );
        // Visible in the provider scope that plans and status read, and not cleared by progress.
        client
            .set_sms_scan_checkpoint("install", "android-sms-provider", true, "2")
            .unwrap();
        assert!(
            client
                .sms_history_status("install", "android-sms-provider")
                .unwrap()
                .paused
        );
        assert!(
            client
                .sms_scan_status("install", "android-sms-provider", true)
                .unwrap()
                .paused
        );
        // Live capture is not disabled by history capacity.
        let mut live = history_row(11);
        live.imported = false;
        client.begin_sms_acquisition_with(live, &tight).unwrap();
        // With production limits capacity is available again, so the next plan resumes history.
        client.open_history_import_gate("contacts_settled").unwrap();
        let plans = client
            .plan_sms_scans("install", "android-sms-provider", "10")
            .unwrap();
        assert!(plans.iter().all(|plan| !plan.paused));
    }

    #[test]
    fn full_pending_queue_backpressures_without_pausing_or_counting_completed_rows() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        let tight = Limits {
            pending: 1,
            pending_bytes: MAX_PENDING_SMS_ACQUISITION_BYTES,
            recovery_records: u64::MAX,
            recovery_bytes: u64::MAX,
        };
        client
            .plan_sms_scans("install", "android-sms-provider", "10")
            .unwrap();
        let first = client
            .begin_sms_acquisition_with(history_row(1), &tight)
            .unwrap();
        assert_eq!(
            client.begin_sms_acquisition_with(history_row(2), &tight),
            Err(Error::SmsCapacity)
        );
        assert!(
            !client
                .sms_history_status("install", "android-sms-provider")
                .unwrap()
                .paused
        );
        client
            .complete_sms_acquisition(&first.acquisition_id)
            .unwrap();
        client
            .begin_sms_acquisition_with(history_row(2), &tight)
            .unwrap();
    }

    #[test]
    fn malformed_descending_upper_bound_cannot_advance_the_ascending_checkpoint() {
        for stored in ["bad", "18446744073709551616"] {
            let dir = tempfile::tempdir().unwrap();
            let client = client(&dir);
            client.plan_sms_scans("install", "scope", "10").unwrap();
            {
                let guard = client.lock().unwrap();
                guard
                    .conn
                    .execute(
                        "UPDATE sms_scan_checkpoints SET history_upper_bound=? WHERE source_generation='install' AND subscription_id='scope' AND imported=1",
                        [stored],
                    )
                    .unwrap();
            }

            assert_eq!(
                client.set_sms_scan_checkpoint("install", "scope", true, "5"),
                Err(Error::Database),
                "stored value {stored:?} did not fail as database corruption"
            );
            let guard = client.lock().unwrap();
            let (checkpoint, upper): (String, String) = guard
                .conn
                .query_row(
                    "SELECT provider_message_id,history_upper_bound FROM sms_scan_checkpoints WHERE source_generation='install' AND subscription_id='scope' AND imported=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert!(checkpoint.is_empty());
            assert_eq!(upper, stored);
        }
    }

    #[test]
    fn initial_history_seed_is_durable_and_accelerates_only_with_host_facts() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);

        let first = client
            .plan_sms_scans_with_history_conditions(
                "install",
                "scope",
                "500",
                HistoryImportConditions::default(),
            )
            .unwrap();
        assert_eq!(first.iter().find(|plan| plan.imported).unwrap().budget, 100);

        for id in (401..=500).rev() {
            client
                .set_sms_scan_checkpoint("install", "scope", true, &id.to_string())
                .unwrap();
        }
        let resumed = client
            .plan_sms_scans_with_history_conditions(
                "install",
                "scope",
                "500",
                HistoryImportConditions::default(),
            )
            .unwrap();
        assert_eq!(
            resumed.iter().find(|plan| plan.imported).unwrap().budget,
            50
        );

        let accelerated = client
            .plan_sms_scans_with_history_conditions(
                "install",
                "scope",
                "500",
                HistoryImportConditions {
                    unmetered_network: false,
                    charging: true,
                },
            )
            .unwrap();
        assert_eq!(
            accelerated
                .iter()
                .find(|plan| plan.imported)
                .unwrap()
                .budget,
            200
        );
        assert_eq!(
            accelerated
                .iter()
                .find(|plan| !plan.imported)
                .unwrap()
                .budget,
            50
        );
    }

    #[test]
    fn legacy_ascending_history_uses_host_adaptive_budget_not_descending_seed() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        client.plan_sms_scans("install", "scope", "500").unwrap();
        client.lock().unwrap().conn.execute(
            "UPDATE sms_scan_checkpoints SET descending=0 WHERE source_generation='install' AND subscription_id='scope' AND imported=1",
            [],
        ).unwrap();

        let plans = client
            .plan_sms_scans_with_history_conditions(
                "install",
                "scope",
                "500",
                HistoryImportConditions {
                    unmetered_network: false,
                    charging: true,
                },
            )
            .unwrap();
        assert_eq!(plans.iter().find(|plan| plan.imported).unwrap().budget, 200);
    }

    #[test]
    fn reconciliation_candidate_lookups_use_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        let guard = client.lock().unwrap();
        for (sql, index) in [
            (
                "SELECT a.message_id FROM sms_acquisitions a JOIN messages m ON m.id=a.message_id WHERE a.evidence_time_ms=1 AND a.direction='incoming' AND a.state='complete'",
                "sms_acquisitions_incoming_evidence",
            ),
            (
                "SELECT b.message_id FROM sms_broadcast_aliases b WHERE b.smsc_at_ms=1",
                "sms_broadcast_aliases_time",
            ),
            (
                "SELECT 1 FROM sms_acquisitions x WHERE x.message_id='m' AND x.state='complete'",
                "sms_acquisitions_message",
            ),
            (
                "SELECT 1 FROM attempts a WHERE a.attempted_at_ms BETWEEN 1 AND 2",
                "attempts_time",
            ),
        ] {
            let plan: Vec<String> = guard
                .conn
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert!(
                plan.iter().any(|step| step.contains(index)),
                "{sql}: {plan:?}"
            );
        }
    }

    #[test]
    fn pending_rechecks_use_the_ordered_pending_index_and_wrap() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        let mut pending_ids = Vec::new();
        for id in 1..=5 {
            let acquisition = client.begin_sms_acquisition(history_row(id)).unwrap();
            if id <= 2 {
                client
                    .complete_sms_acquisition(&acquisition.acquisition_id)
                    .unwrap();
            } else {
                pending_ids.push(acquisition.acquisition_id);
            }
        }
        pending_ids.sort();

        let plan: Vec<String> = client
            .lock()
            .unwrap()
            .conn
            .prepare("EXPLAIN QUERY PLAN SELECT acquisition_id,conversation_id,input,state,reason FROM sms_acquisitions WHERE state='pending' AND acquisition_id>? ORDER BY acquisition_id LIMIT ?")
            .unwrap()
            .query_map(params!["", 10], |row| row.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|step| step.contains("sms_acquisitions_pending_order")),
            "{plan:?}"
        );

        let rows = client.sms_acquisitions_after(None, 10).unwrap();
        assert_eq!(
            rows.into_iter()
                .map(|acquisition| acquisition.acquisition_id)
                .collect::<Vec<_>>(),
            pending_ids
        );
        let wrapped = client.sms_acquisitions_after(Some("~"), 10).unwrap();
        assert_eq!(
            wrapped
                .into_iter()
                .map(|acquisition| acquisition.acquisition_id)
                .collect::<Vec<_>>(),
            pending_ids
        );
    }

    #[test]
    fn local_reconciliation_exhaustion_preserves_the_full_sweep_schedule() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        client
            .lock()
            .unwrap()
            .conn
            .execute(
                "INSERT INTO sms_reconciliation_scans(source_generation,subscription_id,scan_cursor,upper_bound,completed_at_ms) VALUES(?,?,NULL,?,?)",
                params!["install", "provider", "205", 1234],
            )
            .unwrap();

        client
            .complete_sms_reconciliation("install", "provider", "205", Some("5"), true)
            .unwrap();

        let completed_at_ms: Option<i64> = client
            .lock()
            .unwrap()
            .conn
            .query_row(
                "SELECT completed_at_ms FROM sms_reconciliation_scans WHERE source_generation=? AND subscription_id=?",
                params!["install", "provider"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(completed_at_ms, Some(1234));
    }

    fn prepare_overdue_reconciliation(client: &Client) {
        client.plan_sms_scans("install", "provider", "205").unwrap();
        client
            .set_sms_scan_checkpoint("install", "provider", false, "205")
            .unwrap();
        assert_eq!(
            client
                .plan_sms_reconciliation_at("install", "provider", "205", None, 0)
                .unwrap(),
            None
        );
    }

    #[test]
    fn overdue_anchor_mismatch_starts_and_stamps_a_full_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        prepare_overdue_reconciliation(&client);
        let mut row = history_row(205);
        row.imported = false;
        row.source.subscription_id = "provider".into();
        client.begin_sms_acquisition(row).unwrap();

        let plan = client
            .plan_sms_reconciliation_at(
                "install",
                "provider",
                "205",
                Some("replacement"),
                SMS_RECONCILIATION_INTERVAL_MS,
            )
            .unwrap()
            .unwrap();
        assert!(plan.cursor.is_none() || plan.cursor == plan.history_upper_bound);
        client
            .complete_sms_reconciliation(
                "install",
                "provider",
                &plan.upper_bound,
                plan.cursor.as_deref(),
                true,
            )
            .unwrap();

        let completed_at_ms: Option<i64> = client
            .lock()
            .unwrap()
            .conn
            .query_row(
                "SELECT completed_at_ms FROM sms_reconciliation_scans WHERE source_generation=? AND subscription_id=?",
                params!["install", "provider"],
                |row| row.get(0),
            )
            .unwrap();
        assert!(completed_at_ms.is_some());
    }

    #[test]
    fn overdue_small_regression_starts_a_full_sweep_and_resets_live_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        prepare_overdue_reconciliation(&client);

        let plan = client
            .plan_sms_reconciliation_at(
                "install",
                "provider",
                "204",
                None,
                SMS_RECONCILIATION_INTERVAL_MS,
            )
            .unwrap()
            .unwrap();
        assert!(plan.cursor.is_none() || plan.cursor == plan.history_upper_bound);
        let live = client.plan_sms_scans("install", "provider", "204").unwrap();
        assert_eq!(
            live.iter()
                .find(|plan| !plan.imported)
                .unwrap()
                .checkpoint
                .as_deref(),
            Some("204")
        );
    }

    #[test]
    fn backward_clock_starts_a_bounded_reconciliation_repair() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        client.plan_sms_scans("install", "provider", "205").unwrap();
        client
            .set_sms_scan_checkpoint("install", "provider", false, "205")
            .unwrap();
        assert_eq!(
            client
                .plan_sms_reconciliation_at("install", "provider", "205", None, 0)
                .unwrap(),
            None
        );
        client
            .lock()
            .unwrap()
            .conn
            .execute(
                "UPDATE sms_reconciliation_scans SET scan_cursor=NULL,upper_bound='',completed_at_ms=? WHERE source_generation=? AND subscription_id=?",
                params![10_000, "install", "provider"],
            )
            .unwrap();

        let plan = client
            .plan_sms_reconciliation_at("install", "provider", "205", None, 1_000)
            .unwrap()
            .expect("bounded repair");
        assert_eq!(plan.upper_bound, "205");
        assert_eq!(plan.cursor, None);
        assert_eq!(plan.floor.as_deref(), Some("5"));
    }

    #[test]
    fn live_gap_advances_the_live_checkpoint_and_fences_descending_progress() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        client.plan_sms_scans("install", "provider", "1").unwrap();
        let plans = client.plan_sms_scans("install", "provider", "600").unwrap();
        let gap = plans
            .iter()
            .find(|plan| !plan.imported)
            .and_then(|plan| plan.live_gap.clone())
            .expect("large live jump opens a gap");
        assert_eq!(gap.floor, "1");
        assert_eq!(gap.upper_bound, "600");
        assert_eq!(
            client
                .sms_scan_status("install", "provider", false)
                .unwrap()
                .checkpoint
                .as_deref(),
            Some("600")
        );
        assert!(
            client
                .commit_sms_live_gap_page(
                    "install",
                    "provider",
                    &gap.token,
                    None,
                    Some("550"),
                    false
                )
                .unwrap()
        );
        assert!(
            !client
                .commit_sms_live_gap_page(
                    "install",
                    "provider",
                    &gap.token,
                    None,
                    Some("550"),
                    false
                )
                .unwrap()
        );
        assert!(
            client
                .commit_sms_live_gap_page(
                    "install",
                    "provider",
                    &gap.token,
                    Some("550"),
                    Some("500"),
                    true
                )
                .unwrap()
        );
        assert!(
            client
                .plan_sms_scans("install", "provider", "600")
                .unwrap()
                .iter()
                .all(|plan| plan.live_gap.is_none())
        );
    }

    #[test]
    fn reuse_detection_closes_an_open_live_gap_and_reconciliation_covers_it() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        client.plan_sms_scans("install", "provider", "1").unwrap();
        let gap = client
            .plan_sms_scans("install", "provider", "600")
            .unwrap()
            .into_iter()
            .find(|plan| !plan.imported)
            .and_then(|plan| plan.live_gap)
            .unwrap();
        assert!(
            client
                .commit_sms_live_gap_page(
                    "install",
                    "provider",
                    &gap.token,
                    None,
                    Some("599"),
                    false,
                )
                .unwrap()
        );
        let mut anchor = history_row(600);
        anchor.imported = false;
        anchor.source.subscription_id = "provider".into();
        client.begin_sms_acquisition(anchor).unwrap();

        let plan = client
            .plan_sms_reconciliation_at("install", "provider", "600", Some("reused"), 1)
            .unwrap()
            .unwrap();

        assert_eq!(plan.upper_bound, "600");
        assert_eq!(plan.floor.as_deref(), Some("1"));
        assert_eq!(
            client
                .sms_scan_status("install", "provider", false)
                .unwrap()
                .checkpoint
                .as_deref(),
            Some("600")
        );
        assert!(
            client
                .plan_sms_scans("install", "provider", "600")
                .unwrap()
                .into_iter()
                .all(|plan| plan.live_gap.is_none())
        );
        assert!(
            !client
                .commit_sms_live_gap_page(
                    "install",
                    "provider",
                    &gap.token,
                    None,
                    Some("599"),
                    false,
                )
                .unwrap()
        );
    }

    #[test]
    fn small_regression_after_partial_gap_keeps_the_gap_floor_for_live_reconciliation() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        client.plan_sms_scans("install", "provider", "1").unwrap();
        let gap = client
            .plan_sms_scans("install", "provider", "1000")
            .unwrap()
            .into_iter()
            .find(|plan| !plan.imported)
            .and_then(|plan| plan.live_gap)
            .unwrap();
        client
            .commit_sms_live_gap_page("install", "provider", &gap.token, None, Some("950"), false)
            .unwrap();

        let replacement = client
            .plan_sms_reconciliation_at("install", "provider", "999", None, 1)
            .unwrap()
            .unwrap();

        assert_eq!(replacement.floor.as_deref(), Some("1"));
        assert_eq!(replacement.upper_bound, "999");
        assert_eq!(replacement.history_upper_bound.as_deref(), Some("1"));
        assert!(
            client
                .plan_sms_scans("install", "provider", "999")
                .unwrap()
                .into_iter()
                .all(|plan| plan.live_gap.is_none())
        );
    }

    #[test]
    fn unrestricted_reconciliation_stays_unrestricted_when_closing_a_gap_after_large_regression() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        client.plan_sms_scans("install", "provider", "100").unwrap();
        client
            .ack_sms_scan_exhausted("install", "provider", true, "100")
            .unwrap();
        client
            .plan_sms_scans("install", "provider", "1000")
            .unwrap();
        assert_eq!(
            client
                .plan_sms_reconciliation_at("install", "provider", "1000", None, 0)
                .unwrap(),
            None
        );

        let plan = client
            .plan_sms_reconciliation_at("install", "provider", "50", None, 1)
            .unwrap()
            .unwrap();

        assert_eq!(plan.upper_bound, "50");
        assert_eq!(plan.floor, None);
        assert!(
            client
                .plan_sms_scans("install", "provider", "50")
                .unwrap()
                .into_iter()
                .all(|scan| scan.live_gap.is_none())
        );
    }

    #[test]
    fn periodic_unrestricted_reconciliation_is_not_narrowed_by_an_open_gap() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        client.plan_sms_scans("install", "provider", "100").unwrap();
        client
            .ack_sms_scan_exhausted("install", "provider", true, "100")
            .unwrap();
        client
            .plan_sms_scans("install", "provider", "1000")
            .unwrap();
        assert_eq!(
            client
                .plan_sms_reconciliation_at("install", "provider", "1000", None, 0)
                .unwrap(),
            None
        );

        let plan = client
            .plan_sms_reconciliation_at(
                "install",
                "provider",
                "1000",
                None,
                SMS_RECONCILIATION_INTERVAL_MS,
            )
            .unwrap()
            .unwrap();
        assert_eq!(plan.floor, None);
    }

    #[test]
    fn local_reconciliation_floor_at_zero_stays_unrestricted_with_an_open_gap() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        client.plan_sms_scans("install", "provider", "1").unwrap();
        client
            .ack_sms_scan_exhausted("install", "provider", true, "1")
            .unwrap();
        client.plan_sms_scans("install", "provider", "600").unwrap();
        assert_eq!(
            client
                .plan_sms_reconciliation_at("install", "provider", "600", None, 1_000)
                .unwrap(),
            None
        );

        let plan = client
            .plan_sms_reconciliation_at("install", "provider", "200", None, 0)
            .unwrap()
            .unwrap();
        assert_eq!(plan.floor, None);
    }

    #[test]
    fn gap_remains_available_while_history_is_gated_or_backpressured() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        client.plan_sms_scans("install", "provider", "1").unwrap();

        let gated = client.plan_sms_scans("install", "provider", "600").unwrap();
        assert!(gated.iter().find(|plan| plan.imported).unwrap().paused);
        assert!(
            gated
                .iter()
                .find(|plan| !plan.imported)
                .unwrap()
                .live_gap
                .is_some()
        );

        client.open_history_import_gate("contacts_settled").unwrap();
        for id in 1..=500 {
            let acquisition = client.begin_sms_acquisition(history_row(id)).unwrap();
            client
                .complete_sms_acquisition(&acquisition.acquisition_id)
                .unwrap();
        }
        let backpressured = client.plan_sms_scans("install", "provider", "600").unwrap();
        assert!(
            backpressured
                .iter()
                .find(|plan| plan.imported)
                .unwrap()
                .paused
        );
        assert!(
            backpressured
                .iter()
                .find(|plan| !plan.imported)
                .unwrap()
                .live_gap
                .is_some()
        );
    }

    #[test]
    fn live_gap_threshold_and_open_bounds_are_stable() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        client.plan_sms_scans("install", "provider", "100").unwrap();
        assert!(
            client
                .plan_sms_scans("install", "provider", "600")
                .unwrap()
                .into_iter()
                .all(|plan| plan.live_gap.is_none())
        );

        let gap = client
            .plan_sms_scans("install", "provider", "601")
            .unwrap()
            .into_iter()
            .find(|plan| !plan.imported)
            .unwrap()
            .live_gap
            .unwrap();
        let repeated = client
            .plan_sms_scans("install", "provider", "900")
            .unwrap()
            .into_iter()
            .find(|plan| !plan.imported)
            .unwrap()
            .live_gap
            .unwrap();
        assert_eq!(
            (repeated.floor, repeated.upper_bound, repeated.cursor),
            ("100".into(), "601".into(), None)
        );
        assert_eq!(repeated.token, gap.token);
    }

    #[test]
    fn legacy_ascending_reconciliation_restarts_descending_with_the_current_history_floor() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        for (scope, history_complete, expected_floor) in
            [("incomplete", false, Some("100")), ("complete", true, None)]
        {
            client.plan_sms_scans("install", scope, "100").unwrap();
            if history_complete {
                client
                    .ack_sms_scan_exhausted("install", scope, true, "100")
                    .unwrap();
            }
            client.lock().unwrap().conn.execute(
                "INSERT INTO sms_reconciliation_scans(source_generation,subscription_id,scan_cursor,upper_bound,completed_at_ms,descending) VALUES(?,?,?, ?,NULL,0)",
                params!["install", scope, "125", "200"],
            ).unwrap();

            let plan = client
                .plan_sms_reconciliation_at("install", scope, "200", None, 1)
                .unwrap()
                .unwrap();
            assert_eq!(plan.cursor, None);
            assert_eq!(plan.upper_bound, "200");
            assert_eq!(plan.floor.as_deref(), expected_floor);
        }
    }

    #[test]
    fn descending_reconciliation_rejects_out_of_region_progress_and_resumes_below_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let client = client(&dir);
        client.lock().unwrap().conn.execute(
            "INSERT INTO sms_reconciliation_scans(source_generation,subscription_id,scan_cursor,upper_bound,floor,completed_at_ms,descending) VALUES(?,?,NULL,?,?,NULL,1)",
            params!["install", "provider", "200", "100"],
        ).unwrap();

        let first = client
            .plan_sms_reconciliation_at("install", "provider", "200", None, 1)
            .unwrap()
            .unwrap();
        assert_eq!(first.cursor, None);
        client
            .complete_sms_reconciliation("install", "provider", "200", Some("150"), false)
            .unwrap();
        assert_eq!(
            client.complete_sms_reconciliation("install", "provider", "200", Some("151"), false),
            Err(Error::InvalidRequest("SMS reconciliation cursor"))
        );
        assert_eq!(
            client.complete_sms_reconciliation("install", "provider", "200", Some("201"), false),
            Err(Error::InvalidRequest("SMS reconciliation cursor"))
        );
        assert_eq!(
            client.complete_sms_reconciliation("install", "provider", "200", Some("100"), false),
            Err(Error::InvalidRequest("SMS reconciliation cursor"))
        );
        let resumed = client
            .plan_sms_reconciliation_at("install", "provider", "200", None, 2)
            .unwrap()
            .unwrap();
        assert_eq!(resumed.cursor.as_deref(), Some("150"));
        client
            .complete_sms_reconciliation("install", "provider", "200", Some("101"), true)
            .unwrap();
        let completed: Option<i64> = client.lock().unwrap().conn.query_row(
            "SELECT completed_at_ms FROM sms_reconciliation_scans WHERE source_generation='install' AND subscription_id='provider'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert!(completed.is_some());
    }

    #[test]
    fn legacy_input_without_provider_read_decodes_as_unknown() {
        let input: SmsAcquisitionInput = serde_json::from_value(serde_json::json!({
            "source": {
                "source_generation": "generation",
                "subscription_id": "subscription",
                "provider_message_id": "message",
                "provider_thread_id": null
            },
            "direction": "incoming",
            "sender_address": "+15555550100",
            "recipients": [],
            "body": "body",
            "imported": false,
            "sent_at_ms": 1,
            "date_sent_ms": 1,
            "creator": null,
            "source_row_fingerprint": "fingerprint"
        }))
        .unwrap();

        assert_eq!(input.provider_read, None);
    }
}
