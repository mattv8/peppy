use crate::{
    Client, ConversationId, Ctx, DeviceId, Direction, EnvelopePurpose, Error, Inserted, MessageId,
    MessagePayload, MessageRecord, PrivatePayload, RcsMessageStatus, Transport, enqueue,
    insert_message, next_source_sequence,
};
use peppy_crypto::compaction_hmac;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::collections::BTreeSet;
use uuid::Uuid;

const MAX_DETECTORS_PER_CONVERSATION: i64 = 1_024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsReceiveOwner {
    Link,
    Provider,
}

impl RcsReceiveOwner {
    fn code(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Provider => "provider",
        }
    }

    fn from_code(value: &str) -> Result<Self, Error> {
        match value {
            "link" => Ok(Self::Link),
            "provider" => Ok(Self::Provider),
            _ => Err(Error::Database),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsOwnership {
    pub conversation_id: ConversationId,
    pub owner: RcsReceiveOwner,
    pub ownership_generation: u64,
    pub history_token: Option<String>,
    pub detector_floor: i64,
    pub proof_time_ms: i64,
    pub observed_detector_high_water: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsProviderDetector {
    pub provider_row_id: i64,
    pub provider_thread_id: i64,
    pub provider_type: i64,
    pub observed_at_ms: i64,
    pub observed_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsOwnerUnlinkCutoverCandidate {
    pub conversation_id: ConversationId,
    pub ownership_generation: u64,
    pub lifecycle_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsProviderRow {
    pub provider_row_id: i64,
    pub provider_thread_id: i64,
    pub provider_type: i64,
    pub observed_at_ms: i64,
    pub addresses: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsProviderRowRoute {
    Provider,
    DetectorRecorded,
    Alert,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsReceiveCode {
    OwnershipStale,
    ConversationUnproven,
    IdentityConflict,
    CursorInvalid,
    ProviderFloor,
    GatewayUnavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsProjectedRecord {
    pub google_message_id: String,
    pub google_conversation_id: String,
    pub direction: Direction,
    pub body: String,
    pub sent_at_ms: Option<i64>,
    pub tmp_id: Option<String>,
    pub rcs_reported: bool,
    pub e2ee_reported: Option<bool>,
    pub history_token: Option<String>,
}

/// The acquisition source of an RCS observation. Callers must select this explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsObservationProvenance {
    Live,
    Historical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RcsContextExpectation {
    pub ownership_generation: u64,
    pub link_generation: u64,
    pub session_generation: u64,
    pub context_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsReceiveOutcome {
    Inserted,
    Duplicate,
    UnmatchedOutgoing,
    OutgoingMatched,
    StatusApplied,
    StatusUnchanged,
    Rejected(RcsReceiveCode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsProjectedStatus {
    Sending,
    Sent,
    Failed,
    Delivered,
    Read,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RcsConversationTransport {
    Rcs,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct RcsConversationMetadata {
    pub conversation_id: ConversationId,
    pub transport: RcsConversationTransport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rcs_encryption: Option<bool>,
    #[serde(default)]
    pub rcs_history_gap: bool,
    pub logical_revision: u64,
    pub writer_device_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RcsMessageStatusProjection {
    pub status: RcsMessageStatus,
    pub rcs_reported: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsStatusApplication {
    pub google_message_id: String,
    pub google_conversation_id: String,
    pub tmp_id: Option<String>,
    pub status: RcsProjectedStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsIdentityOutcome {
    Recorded,
    Duplicate,
    Rejected(RcsReceiveCode),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsCutover {
    pub owner: RcsReceiveOwner,
    pub ownership_generation: u64,
    pub freeze_detector_floor: i64,
    pub ambiguous_detector_alert: bool,
}

enum CutoverAuthority {
    Legacy,
    LinkedGateway {
        expected_ownership_generation: u64,
    },
    OwnerUnlink {
        expected_ownership_generation: u64,
        expected_lifecycle_generation: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RcsReceiveAlert {
    None,
    AmbiguousProviderWindow,
    DetectorOverflow,
    GatewayUnavailable,
}

impl RcsReceiveAlert {
    fn from_code(value: Option<&str>) -> Result<Self, Error> {
        match value {
            None => Ok(Self::None),
            Some("ambiguous_provider_window") => Ok(Self::AmbiguousProviderWindow),
            Some("detector_overflow") => Ok(Self::DetectorOverflow),
            Some("gateway_unavailable") => Ok(Self::GatewayUnavailable),
            Some(_) => Err(Error::Database),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsReceiveState {
    pub owner: RcsReceiveOwner,
    pub ownership_generation: u64,
    pub history_token: Option<String>,
    pub detector_floor: i64,
    pub proof_time_ms: i64,
    pub observed_detector_high_water: i64,
    pub alert: RcsReceiveAlert,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RcsUnlinkCutoverGenerations {
    pub ownership_generation: u64,
    pub lifecycle_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RcsReceiveContext {
    pub conversation_id: ConversationId,
    pub google_conversation_id: String,
    pub ownership_generation: u64,
    pub lifecycle_generation: u64,
    pub owner: RcsReceiveOwner,
    pub history_token: Option<String>,
    pub expectation: RcsContextExpectation,
}

impl Client {
    pub fn publish_rcs_conversation_metadata(
        &self,
        conversation_id: ConversationId,
        ownership_generation: u64,
        rcs_encryption: Option<bool>,
    ) -> Result<bool, Error> {
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(ownership) = load_ownership(&tx, conversation_id)? else {
            return Err(Error::InvalidRequest(
                "stale RCS conversation metadata writer",
            ));
        };
        if ownership.owner != RcsReceiveOwner::Link
            || ownership.ownership_generation != ownership_generation
        {
            return Err(Error::InvalidRequest(
                "stale RCS conversation metadata writer",
            ));
        }
        let history_gap = load_receive_state(&tx, conversation_id)?
            .map(|state| history_gap(state.alert))
            .unwrap_or(false);
        publish_metadata(&tx, &ctx, conversation_id, rcs_encryption, history_gap)?;
        tx.commit()?;
        Ok(true)
    }

    pub fn rcs_conversation_metadata(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<RcsConversationMetadata>, Error> {
        self.lock()?
            .conn
            .query_row(
                "SELECT rcs_encryption,rcs_history_gap,logical_revision,writer_device_id FROM rcs_conversation_metadata WHERE conversation_id=?",
                [conversation_id.to_string()],
                |row| {
                    Ok(RcsConversationMetadata {
                        conversation_id,
                        transport: RcsConversationTransport::Rcs,
                        rcs_encryption: row.get::<_, Option<i64>>(0)?.map(|value| value != 0),
                        rcs_history_gap: row.get::<_, bool>(1)?,
                        logical_revision: u64::try_from(row.get::<_, i64>(2)?).map_err(|_| rusqlite::Error::InvalidQuery)?,
                        writer_device_id: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn rcs_message_status(
        &self,
        message_id: MessageId,
    ) -> Result<Option<RcsMessageStatusProjection>, Error> {
        self.lock()?
            .conn
            .query_row(
                "SELECT status_rank,rcs_reported FROM rcs_message_status WHERE message_id=? AND status_rank>0",
                [message_id.to_string()],
                |row| {
                    Ok(RcsMessageStatusProjection {
                        status: status_from_rank(row.get(0)?)?,
                        rcs_reported: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn rcs_message_reported(&self, message_id: MessageId) -> Result<Option<bool>, Error> {
        self.lock()?
            .conn
            .query_row(
                "SELECT rcs_reported FROM rcs_message_status WHERE message_id=?",
                [message_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Records proof-time link ownership without importing pre-proof provider history.
    pub fn begin_rcs_link_ownership(
        &self,
        conversation_id: ConversationId,
        initial_history_token: &str,
        provider_floor: i64,
    ) -> Result<RcsOwnership, Error> {
        if !valid_token(initial_history_token) || provider_floor < 0 {
            return Err(Error::InvalidRequest("invalid RCS ownership checkpoint"));
        }
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let generation = next_generation(&tx, conversation_id)?;
        tx.execute(
            "INSERT INTO rcs_lane_ownership(conversation_id,owner,ownership_generation,history_token,detector_floor,proof_time_ms,observed_detector_high_water,alert_code) VALUES(?,?,?,?,?,?,?,NULL)
             ON CONFLICT(conversation_id) DO UPDATE SET owner=excluded.owner,ownership_generation=excluded.ownership_generation,history_token=excluded.history_token,detector_floor=excluded.detector_floor,proof_time_ms=excluded.proof_time_ms,observed_detector_high_water=excluded.observed_detector_high_water,alert_code=NULL",
            params![conversation_id.to_string(), RcsReceiveOwner::Link.code(), generation, initial_history_token, provider_floor, crate::notifications::now_ms(), provider_floor],
        )?;
        publish_metadata(&tx, &ctx, conversation_id, None, false)?;
        tx.commit()?;
        Ok(RcsOwnership {
            conversation_id,
            owner: RcsReceiveOwner::Link,
            ownership_generation: generation as u64,
            history_token: Some(initial_history_token.into()),
            detector_floor: provider_floor,
            proof_time_ms: crate::notifications::now_ms(),
            observed_detector_high_water: provider_floor,
        })
    }

    /// Persists immutable link identity before R2 associates it with a vault message.
    pub fn record_rcs_link_identity(
        &self,
        conversation_id: ConversationId,
        ownership_generation: u64,
        google_message_id: &str,
        google_conversation_id: &str,
        history_token: Option<&str>,
    ) -> Result<RcsIdentityOutcome, Error> {
        if google_message_id.is_empty() || google_conversation_id.is_empty() {
            return Ok(RcsIdentityOutcome::Rejected(
                RcsReceiveCode::IdentityConflict,
            ));
        }
        if history_token.is_some_and(|token| !valid_token(token)) {
            return Ok(RcsIdentityOutcome::Rejected(RcsReceiveCode::CursorInvalid));
        }
        let mut store = self.lock()?;
        let (conn, _) = store.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(ownership) = load_ownership(&tx, conversation_id)? else {
            return Ok(RcsIdentityOutcome::Rejected(
                RcsReceiveCode::ConversationUnproven,
            ));
        };
        if ownership.owner != RcsReceiveOwner::Link
            || ownership.ownership_generation != ownership_generation
        {
            return Ok(RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale));
        }
        let outcome = record_identity(
            &tx,
            conversation_id,
            ownership_generation,
            google_message_id,
            google_conversation_id,
            None,
            false,
        )?;
        match outcome {
            RcsIdentityOutcome::Recorded => {}
            RcsIdentityOutcome::Duplicate => {
                checkpoint_record(&tx, conversation_id, ownership_generation, history_token)?;
                tx.commit()?;
                return Ok(outcome);
            }
            RcsIdentityOutcome::Rejected(_) => return Ok(outcome),
        }
        checkpoint_record(&tx, conversation_id, ownership_generation, history_token)?;
        tx.commit()?;
        Ok(RcsIdentityOutcome::Recorded)
    }

    pub fn ingest_rcs_projected_record(
        &self,
        conversation_id: ConversationId,
        ownership_generation: u64,
        record: RcsProjectedRecord,
    ) -> Result<RcsReceiveOutcome, Error> {
        let provenance = if record.history_token.is_some() {
            RcsObservationProvenance::Historical
        } else {
            RcsObservationProvenance::Live
        };
        self.ingest_rcs_projected_record_with_provenance(
            conversation_id,
            ownership_generation,
            record,
            provenance,
            true,
        )
    }

    pub fn ingest_rcs_projected_observation(
        &self,
        conversation_id: ConversationId,
        ownership_generation: u64,
        record: RcsProjectedRecord,
        provenance: RcsObservationProvenance,
    ) -> Result<RcsReceiveOutcome, Error> {
        if record.history_token.is_some() {
            return Err(Error::InvalidRequest(
                "RCS observations cannot carry a history checkpoint",
            ));
        }
        self.ingest_rcs_projected_record_with_provenance(
            conversation_id,
            ownership_generation,
            record,
            provenance,
            false,
        )
    }

    fn ingest_rcs_projected_record_with_provenance(
        &self,
        conversation_id: ConversationId,
        ownership_generation: u64,
        record: RcsProjectedRecord,
        provenance: RcsObservationProvenance,
        checkpoint_history_token: bool,
    ) -> Result<RcsReceiveOutcome, Error> {
        if record.google_message_id.is_empty()
            || record.google_conversation_id.is_empty()
            || record.body.len() > 16_384
            || record
                .history_token
                .as_deref()
                .is_some_and(|token| !valid_token(token))
        {
            return Ok(RcsReceiveOutcome::Rejected(
                RcsReceiveCode::IdentityConflict,
            ));
        }
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(ownership) = load_ownership(&tx, conversation_id)? else {
            return Ok(RcsReceiveOutcome::Rejected(
                RcsReceiveCode::ConversationUnproven,
            ));
        };
        if ownership.owner != RcsReceiveOwner::Link
            || ownership.ownership_generation != ownership_generation
        {
            return Ok(RcsReceiveOutcome::Rejected(RcsReceiveCode::OwnershipStale));
        }
        if !unique_current_gateway(&tx, ctx.device_id)? {
            tx.execute(
                "UPDATE rcs_lane_ownership SET alert_code='gateway_unavailable' WHERE conversation_id=?",
                [conversation_id.to_string()],
            )?;
            publish_metadata(&tx, &ctx, conversation_id, None, true)?;
            tx.commit()?;
            return Ok(RcsReceiveOutcome::Rejected(
                RcsReceiveCode::GatewayUnavailable,
            ));
        }
        let historical = provenance == RcsObservationProvenance::Historical;
        if historical
            && record
                .sent_at_ms
                .is_none_or(|sent_at_ms| sent_at_ms <= ownership.proof_time_ms)
        {
            return Ok(RcsReceiveOutcome::Rejected(RcsReceiveCode::ProviderFloor));
        }
        if record.direction == Direction::Outgoing
            && let Some(tmp_id) = record.tmp_id.as_deref()
            && let Some(message_id) = tx
                    .query_row(
                        "SELECT c.message_id FROM rcs_send_ledger l JOIN commands c ON c.command_id=l.command_id WHERE l.tmp_id=? AND l.google_conversation_id=?",
                        params![tmp_id, record.google_conversation_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
        {
                let message_id = message_id.parse().map_err(|_| Error::Database)?;
                return match record_identity(
                    &tx,
                    conversation_id,
                    ownership_generation,
                    &record.google_message_id,
                    &record.google_conversation_id,
                    Some(message_id),
                    true,
                )? {
                    RcsIdentityOutcome::Recorded => {
                        apply_reported_facts(
                            &tx,
                            &ctx,
                            conversation_id,
                            message_id,
                            record.rcs_reported,
                            record.e2ee_reported,
                        )?;
                        if checkpoint_history_token {
                            checkpoint_record(&tx, conversation_id, ownership_generation, record.history_token.as_deref())?;
                        }
                        tx.commit()?;
                        Ok(RcsReceiveOutcome::OutgoingMatched)
                    }
                    RcsIdentityOutcome::Duplicate => {
                        apply_reported_facts(
                            &tx,
                            &ctx,
                            conversation_id,
                            message_id,
                            record.rcs_reported,
                            record.e2ee_reported,
                        )?;
                        if checkpoint_history_token {
                            checkpoint_record(&tx, conversation_id, ownership_generation, record.history_token.as_deref())?;
                        }
                        tx.commit()?;
                        Ok(RcsReceiveOutcome::Duplicate)
                    }
                    RcsIdentityOutcome::Rejected(code) => Ok(RcsReceiveOutcome::Rejected(code)),
                };
        }
        let message_id = deterministic_message_id(
            &ctx,
            &record.google_conversation_id,
            &record.google_message_id,
        )?;
        let identity = record_identity(
            &tx,
            conversation_id,
            ownership_generation,
            &record.google_message_id,
            &record.google_conversation_id,
            Some(message_id),
            false,
        )?;
        let outcome = match identity {
            RcsIdentityOutcome::Recorded => {
                let unmatched_outgoing = record.direction == Direction::Outgoing;
                let payload = MessagePayload {
                    record: MessageRecord {
                        message_id,
                        conversation_id,
                        source_sequence: next_source_sequence(&tx, conversation_id, ctx.device_id)?,
                        attachments: Vec::new(),
                    },
                    source_device_id: ctx.device_id,
                    provider_message_id: None,
                    sender_address: None,
                    recipients: Vec::new(),
                    subject: None,
                    body: record.body,
                    transport: Transport::Rcs,
                    direction: record.direction,
                    imported: historical,
                    sent_at_ms: record.sent_at_ms,
                    ordered_at_ms: None,
                    mms_context: None,
                };
                if insert_message(&tx, &payload)? != Inserted::New {
                    return Err(Error::Database);
                }
                enqueue(
                    &tx,
                    &ctx,
                    EnvelopePurpose::Event,
                    None,
                    None,
                    &PrivatePayload::Message(payload),
                )?;
                if unmatched_outgoing {
                    let count: i64 = tx.query_row(
                        "SELECT COALESCE(CAST(v AS INTEGER),0) FROM metadata WHERE k='rcs_unmatched_outgoing_count'",
                        [],
                        |row| row.get(0),
                    ).optional()?.unwrap_or(0);
                    tx.execute(
                        "INSERT INTO metadata(k,v) VALUES('rcs_unmatched_outgoing_count',?) ON CONFLICT(k) DO UPDATE SET v=excluded.v",
                        [(count.checked_add(1).ok_or(Error::Database)?).to_string()],
                    )?;
                    RcsReceiveOutcome::UnmatchedOutgoing
                } else {
                    RcsReceiveOutcome::Inserted
                }
            }
            RcsIdentityOutcome::Duplicate => RcsReceiveOutcome::Duplicate,
            RcsIdentityOutcome::Rejected(code) => return Ok(RcsReceiveOutcome::Rejected(code)),
        };
        let reported_message_id = tx
            .query_row(
                "SELECT message_id FROM rcs_message_identities WHERE google_message_id=? AND google_conversation_id=? AND conversation_id=?",
                params![record.google_message_id, record.google_conversation_id, conversation_id.to_string()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .map(|id| id.parse().map_err(|_| Error::Database))
            .transpose()?;
        if let Some(reported_message_id) = reported_message_id {
            apply_reported_facts(
                &tx,
                &ctx,
                conversation_id,
                reported_message_id,
                record.rcs_reported,
                record.e2ee_reported,
            )?;
        }
        if checkpoint_history_token && let Some(token) = record.history_token {
            checkpoint_record(&tx, conversation_id, ownership_generation, Some(&token))?;
        }
        tx.commit()?;
        Ok(outcome)
    }

    pub fn apply_rcs_projected_status(
        &self,
        conversation_id: ConversationId,
        ownership_generation: u64,
        application: RcsStatusApplication,
    ) -> Result<RcsReceiveOutcome, Error> {
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(ownership) = load_ownership(&tx, conversation_id)? else {
            return Ok(RcsReceiveOutcome::Rejected(
                RcsReceiveCode::ConversationUnproven,
            ));
        };
        if ownership.owner != RcsReceiveOwner::Link
            || ownership.ownership_generation != ownership_generation
        {
            return Ok(RcsReceiveOutcome::Rejected(RcsReceiveCode::OwnershipStale));
        }
        if !unique_current_gateway(&tx, ctx.device_id)? {
            return Ok(RcsReceiveOutcome::Rejected(
                RcsReceiveCode::GatewayUnavailable,
            ));
        }
        let mut message_id: Option<String> = tx.query_row(
            "SELECT message_id FROM rcs_message_identities WHERE google_message_id=? AND google_conversation_id=? AND conversation_id=?",
            params![application.google_message_id, application.google_conversation_id, conversation_id.to_string()],
            |row| row.get(0),
        ).optional()?.flatten();
        if let Some(tmp_id) = application.tmp_id.as_deref() {
            let tmp_message_id: Option<String> = tx
                .query_row(
                    "SELECT c.message_id FROM rcs_send_ledger l JOIN commands c ON c.command_id=l.command_id WHERE l.tmp_id=? AND l.google_conversation_id=?",
                    params![tmp_id, application.google_conversation_id],
                    |row| row.get(0),
            )
            .optional()?;
            if let Some(tmp_message_id) = tmp_message_id.as_deref() {
                match record_identity(
                    &tx,
                    conversation_id,
                    ownership_generation,
                    &application.google_message_id,
                    &application.google_conversation_id,
                    Some(tmp_message_id.parse().map_err(|_| Error::Database)?),
                    true,
                )? {
                    RcsIdentityOutcome::Recorded => {}
                    RcsIdentityOutcome::Duplicate => {}
                    RcsIdentityOutcome::Rejected(code) => {
                        return Ok(RcsReceiveOutcome::Rejected(code));
                    }
                }
                if message_id.is_none() {
                    message_id = Some(tmp_message_id.to_owned());
                }
            }
        }
        let Some(message_id) = message_id else {
            return Ok(RcsReceiveOutcome::Rejected(
                RcsReceiveCode::IdentityConflict,
            ));
        };
        let message_id: MessageId = message_id.parse().map_err(|_| Error::Database)?;
        let status = match application.status {
            RcsProjectedStatus::Sending => RcsMessageStatus::Sending,
            RcsProjectedStatus::Sent => RcsMessageStatus::Sent,
            RcsProjectedStatus::Failed => RcsMessageStatus::Failed,
            RcsProjectedStatus::Delivered => RcsMessageStatus::Delivered,
            RcsProjectedStatus::Read => RcsMessageStatus::Read,
        };
        if let Some(tmp_id) = application.tmp_id.as_deref() {
            let evidence = match application.status {
                RcsProjectedStatus::Sending => None,
                RcsProjectedStatus::Sent => Some(crate::SendResult::Sent),
                RcsProjectedStatus::Failed => Some(crate::SendResult::FailedConfirmed),
                RcsProjectedStatus::Delivered | RcsProjectedStatus::Read => {
                    Some(crate::SendResult::Delivered)
                }
            };
            if let Some(evidence) = evidence {
                crate::reconcile_rcs_tmp_id_in_transaction(
                    &tx,
                    &ctx,
                    tmp_id,
                    &application.google_conversation_id,
                    evidence,
                )?;
            }
        }
        if !apply_status(&tx, message_id, status, true)? {
            tx.commit()?;
            return Ok(RcsReceiveOutcome::StatusUnchanged);
        }
        enqueue(
            &tx,
            &ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::RcsMessageStatus {
                message_id,
                status,
                rcs_reported: true,
            },
        )?;
        tx.commit()?;
        Ok(RcsReceiveOutcome::StatusApplied)
    }

    pub fn checkpoint_rcs_history(
        &self,
        conversation_id: ConversationId,
        ownership_generation: u64,
        history_token: &str,
    ) -> Result<RcsIdentityOutcome, Error> {
        if !valid_token(history_token) {
            return Ok(RcsIdentityOutcome::Rejected(RcsReceiveCode::CursorInvalid));
        }
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(ownership) = load_ownership(&tx, conversation_id)? else {
            return Ok(RcsIdentityOutcome::Rejected(
                RcsReceiveCode::ConversationUnproven,
            ));
        };
        if ownership.owner != RcsReceiveOwner::Link
            || ownership.ownership_generation != ownership_generation
        {
            return Ok(RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale));
        }
        if !unique_current_gateway(&tx, ctx.device_id)? {
            return Ok(RcsIdentityOutcome::Rejected(
                RcsReceiveCode::GatewayUnavailable,
            ));
        }
        tx.execute(
            "UPDATE rcs_lane_ownership SET history_token=? WHERE conversation_id=? AND ownership_generation=?",
            params![history_token, conversation_id.to_string(), ownership_generation],
        )?;
        tx.commit()?;
        Ok(RcsIdentityOutcome::Recorded)
    }

    pub fn rcs_receive_state(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<RcsReceiveState>, Error> {
        load_receive_state(&self.lock()?.conn, conversation_id)
    }

    pub fn rcs_unlink_cutover_generations(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<RcsUnlinkCutoverGenerations>, Error> {
        let store = self.lock()?;
        let Some(ownership) = load_ownership(&store.conn, conversation_id)? else {
            return Ok(None);
        };
        let Some(lifecycle) = crate::rcs_mapping::recorded_link_lifecycle_metadata(&store.conn)?
        else {
            return Ok(None);
        };
        Ok(Some(RcsUnlinkCutoverGenerations {
            ownership_generation: ownership.ownership_generation,
            lifecycle_generation: lifecycle.generation,
        }))
    }

    pub fn rcs_receive_context(
        &self,
        google_conversation_id: &str,
    ) -> Result<Option<RcsReceiveContext>, Error> {
        let mut store = self.lock()?;
        let device_id = store.config.device_id;
        receive_context_in_transaction(&mut store.conn, device_id, google_conversation_id, || {})
    }

    pub fn rcs_link_owned_conversations(&self) -> Result<Vec<RcsReceiveContext>, Error> {
        let mut store = self.lock()?;
        let device_id = store.config.device_id;
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        if !unique_current_gateway(&tx, device_id)? {
            return Err(Error::InvalidRequest("local RCS gateway is unavailable"));
        }
        let Some(lifecycle) = crate::rcs_mapping::recorded_link_lifecycle_metadata(&tx)? else {
            return Err(Error::InvalidRequest("local RCS gateway is unavailable"));
        };
        if lifecycle.state != crate::RcsCapabilityState::Linked {
            return Err(Error::InvalidRequest("local RCS gateway is unavailable"));
        }
        let rows = {
            let mut statement = tx.prepare(
                "SELECT o.conversation_id,m.google_conversation_id,o.ownership_generation,o.history_token FROM rcs_lane_ownership o JOIN rcs_conversation_mappings m ON m.peppy_conversation_id=o.conversation_id WHERE o.owner='link' AND m.invalidated_at_ms IS NULL ORDER BY o.conversation_id",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        if rows.is_empty() {
            tx.commit()?;
            return Ok(Vec::new());
        }
        let context_tuple = current_context_tuple(&tx)?
            .ok_or(Error::InvalidRequest("local RCS context is unavailable"))?;
        let contexts = rows
            .into_iter()
            .map(
                |(conversation_id, google_conversation_id, ownership_generation, history_token)| {
                    let ownership_generation = ownership_generation as u64;
                    Ok(RcsReceiveContext {
                        conversation_id: conversation_id.parse().map_err(|_| Error::Database)?,
                        google_conversation_id,
                        ownership_generation,
                        lifecycle_generation: lifecycle.generation,
                        owner: RcsReceiveOwner::Link,
                        history_token,
                        expectation: context_tuple.with_ownership(ownership_generation),
                    })
                },
            )
            .collect::<Result<Vec<_>, Error>>()?;
        tx.commit()?;
        Ok(contexts)
    }

    /// Enumerates terminal Link-owned lanes only after the current explicit unlink completed.
    pub fn rcs_owner_unlink_cutover_candidates(
        &self,
    ) -> Result<Vec<RcsOwnerUnlinkCutoverCandidate>, Error> {
        let mut store = self.lock()?;
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let lifecycle = crate::rcs_mapping::recorded_link_lifecycle_metadata(&tx)?
            .ok_or(Error::InvalidRequest("terminal RCS unlink is unavailable"))?;
        if lifecycle.state != crate::RcsCapabilityState::Unlinked
            || !crate::rcs_send::intentional_unlink_is_current(&tx)?
        {
            return Err(Error::InvalidRequest("terminal RCS unlink is unavailable"));
        }
        let candidates = {
            let mut statement = tx.prepare(
                "SELECT conversation_id,ownership_generation FROM rcs_lane_ownership WHERE owner='link' ORDER BY conversation_id LIMIT 100",
            )?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
                })?
                .map(|row| {
                    let (conversation_id, ownership_generation) = row?;
                    Ok(RcsOwnerUnlinkCutoverCandidate {
                        conversation_id: conversation_id.parse().map_err(|_| Error::Database)?,
                        ownership_generation,
                        lifecycle_generation: lifecycle.generation,
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?
        };
        tx.commit()?;
        Ok(candidates)
    }

    /// Routes a provider row without creating message, acquisition, or conversation state.
    pub fn route_rcs_provider_row(
        &self,
        row: RcsProviderRow,
    ) -> Result<RcsProviderRowRoute, Error> {
        validate_provider_row(&row)?;
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !local_gateway(&tx, ctx.device_id)? {
            return Err(Error::InvalidRequest("local RCS gateway is unavailable"));
        }
        let row_identities = row
            .addresses
            .iter()
            .filter_map(|address| crate::rcs_mapping::canonical_recipient(address))
            .collect::<BTreeSet<_>>();
        let (resolved, uncertain) =
            crate::rcs_mapping::peppy_conversations_for_identity_set(&tx, &row_identities)?;
        let mut conversations = resolved.into_iter().collect::<BTreeSet<_>>();
        let mut statement = tx.prepare(
            "SELECT m.peppy_conversation_id,p.canonical_participant,p.identity_region,p.own_identity FROM rcs_conversation_mappings m JOIN rcs_mapping_proofs p ON p.peppy_conversation_id=m.peppy_conversation_id AND p.mapping_revision=m.mapping_revision",
        )?;
        let proofs = statement
            .query_map([], |record| {
                Ok((
                    record.get::<_, String>(0)?,
                    record.get::<_, String>(1)?,
                    record.get::<_, String>(2)?,
                    record.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (conversation, participant, region, own) in proofs {
            let participants = row
                .addresses
                .iter()
                .filter(|address| {
                    !crate::conversation_match::confirmed_own_address(&own, address, Some(&region))
                })
                .filter_map(|address| crate::rcs_mapping::canonical_recipient(address))
                .collect::<BTreeSet<_>>();
            if participants == BTreeSet::from([participant]) {
                conversations.insert(conversation.parse().map_err(|_| Error::Database)?);
            }
        }
        if uncertain || conversations.len() > 1 {
            tx.commit()?;
            return Ok(RcsProviderRowRoute::Alert);
        }
        let Some(conversation_id) = conversations.pop_first() else {
            let has_link_lane: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM rcs_lane_ownership WHERE owner='link')",
                [],
                |record| record.get(0),
            )?;
            tx.commit()?;
            return Ok(if has_link_lane {
                RcsProviderRowRoute::Alert
            } else {
                RcsProviderRowRoute::Provider
            });
        };
        let Some(ownership) = load_ownership(&tx, conversation_id)? else {
            tx.commit()?;
            return Ok(RcsProviderRowRoute::Provider);
        };
        if ownership.owner != RcsReceiveOwner::Link {
            tx.commit()?;
            return Ok(RcsProviderRowRoute::Provider);
        }
        record_provider_detector_in_transaction(
            &tx,
            &ctx,
            conversation_id,
            RcsProviderDetector {
                provider_row_id: row.provider_row_id,
                provider_thread_id: row.provider_thread_id,
                provider_type: row.provider_type,
                observed_at_ms: row.observed_at_ms,
                observed_generation: ownership.ownership_generation,
            },
        )?;
        tx.commit()?;
        Ok(RcsProviderRowRoute::DetectorRecorded)
    }

    /// Records detector metadata only; this method never writes a vault message.
    pub fn record_rcs_provider_detector(
        &self,
        conversation_id: ConversationId,
        ownership_generation: u64,
        detector: RcsProviderDetector,
    ) -> Result<RcsIdentityOutcome, Error> {
        if detector.provider_row_id < 0
            || detector.provider_thread_id < 0
            || detector.provider_type < 0
        {
            return Err(Error::InvalidRequest("invalid RCS provider detector"));
        }
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(ownership) = load_ownership(&tx, conversation_id)? else {
            return Ok(RcsIdentityOutcome::Rejected(
                RcsReceiveCode::ConversationUnproven,
            ));
        };
        if ownership.owner != RcsReceiveOwner::Link
            || ownership.ownership_generation != ownership_generation
        {
            return Ok(RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale));
        }
        if !local_gateway(&tx, ctx.device_id)? {
            return Ok(RcsIdentityOutcome::Rejected(
                RcsReceiveCode::GatewayUnavailable,
            ));
        }
        let outcome =
            record_provider_detector_in_transaction(&tx, &ctx, conversation_id, detector)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Freezes link writing and makes provider ownership effective for rows after this floor.
    pub fn begin_rcs_unlink_cutover(
        &self,
        conversation_id: ConversationId,
    ) -> Result<RcsCutover, Error> {
        self.begin_rcs_unlink_cutover_inner(conversation_id, CutoverAuthority::Legacy)
    }

    pub fn begin_rcs_unlink_cutover_if_current(
        &self,
        conversation_id: ConversationId,
        expected_ownership_generation: u64,
    ) -> Result<RcsCutover, Error> {
        self.begin_rcs_unlink_cutover_inner(
            conversation_id,
            CutoverAuthority::LinkedGateway {
                expected_ownership_generation,
            },
        )
    }

    pub fn begin_rcs_owner_unlink_cutover(
        &self,
        conversation_id: ConversationId,
        expected_ownership_generation: u64,
        expected_lifecycle_generation: u64,
    ) -> Result<RcsCutover, Error> {
        self.begin_rcs_unlink_cutover_inner(
            conversation_id,
            CutoverAuthority::OwnerUnlink {
                expected_ownership_generation,
                expected_lifecycle_generation,
            },
        )
    }

    fn begin_rcs_unlink_cutover_inner(
        &self,
        conversation_id: ConversationId,
        authority: CutoverAuthority,
    ) -> Result<RcsCutover, Error> {
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(ownership) = load_ownership(&tx, conversation_id)? else {
            return Err(Error::NotFound);
        };
        if ownership.owner != RcsReceiveOwner::Link {
            return Err(Error::InvalidRequest("RCS link is not the current writer"));
        }
        match authority {
            CutoverAuthority::Legacy => {}
            CutoverAuthority::LinkedGateway {
                expected_ownership_generation,
            } => {
                if ownership.ownership_generation != expected_ownership_generation {
                    return Err(Error::InvalidRequest("RCS link is not the current writer"));
                }
                if !unique_current_gateway(&tx, ctx.device_id)? {
                    return Err(Error::InvalidRequest("RCS gateway is not currently linked"));
                }
            }
            CutoverAuthority::OwnerUnlink {
                expected_ownership_generation,
                expected_lifecycle_generation,
            } => {
                if ownership.ownership_generation != expected_ownership_generation {
                    return Err(Error::InvalidRequest("RCS link is not the current writer"));
                }
                let lifecycle = crate::rcs_mapping::recorded_link_lifecycle_metadata(&tx)?;
                if lifecycle.is_none_or(|lifecycle| {
                    lifecycle.state != crate::RcsCapabilityState::Unlinked
                        || lifecycle.generation != expected_lifecycle_generation
                }) || !crate::rcs_send::intentional_unlink_is_current(&tx)?
                {
                    return Err(Error::InvalidRequest("RCS owner unlink is not current"));
                }
            }
        }
        let freeze_detector_floor: i64 = tx.query_row(
            "SELECT MAX(?, ?, COALESCE((SELECT MAX(provider_row_id) FROM rcs_provider_detectors WHERE conversation_id=?), ?))",
            params![ownership.detector_floor, ownership.observed_detector_high_water, conversation_id.to_string(), ownership.detector_floor],
            |row| row.get(0),
        )?;
        let ambiguous_detector_alert: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM rcs_provider_detectors WHERE conversation_id=? AND provider_row_id<=? AND resolved=0)",
            params![conversation_id.to_string(), freeze_detector_floor],
            |row| row.get(0),
        )?;
        let generation = ownership
            .ownership_generation
            .checked_add(1)
            .ok_or(Error::Database)?;
        let alert = load_receive_state(&tx, conversation_id)?
            .ok_or(Error::Database)?
            .alert;
        let alert_code = match alert {
            RcsReceiveAlert::DetectorOverflow => Some("detector_overflow"),
            _ => ambiguous_detector_alert.then_some("ambiguous_provider_window"),
        };
        tx.execute(
            "UPDATE rcs_lane_ownership SET owner=?,ownership_generation=?,history_token=NULL,detector_floor=?,alert_code=? WHERE conversation_id=?",
            params![RcsReceiveOwner::Provider.code(), generation, freeze_detector_floor, alert_code, conversation_id.to_string()],
        )?;
        publish_metadata(
            &tx,
            &ctx,
            conversation_id,
            None,
            ambiguous_detector_alert || alert == RcsReceiveAlert::DetectorOverflow,
        )?;
        tx.commit()?;
        Ok(RcsCutover {
            owner: RcsReceiveOwner::Provider,
            ownership_generation: generation,
            freeze_detector_floor,
            ambiguous_detector_alert,
        })
    }

    pub fn invalidate_rcs_context_for_google_event(
        &self,
        conversation_id: ConversationId,
        google_conversation_id: &str,
        expected: RcsContextExpectation,
    ) -> Result<u64, Error> {
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(ownership) = load_ownership(&tx, conversation_id)? else {
            return Err(Error::NotFound);
        };
        if ownership.owner != RcsReceiveOwner::Link
            || ownership.ownership_generation != expected.ownership_generation
            || !unique_current_gateway(&tx, ctx.device_id)?
        {
            return Err(Error::InvalidRequest("RCS context is not current"));
        }
        let context: Option<String> = tx
            .query_row(
                "SELECT v FROM metadata WHERE k='rcs_mapping_context_v1'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(context) = context else {
            return Err(Error::InvalidRequest("RCS context is not current"));
        };
        let mut context: serde_json::Value =
            serde_json::from_str(&context).map_err(|_| Error::Database)?;
        let Some(object) = context.as_object_mut() else {
            return Err(Error::Database);
        };
        let matches_context = [
            ("link_generation", expected.link_generation),
            ("session_generation", expected.session_generation),
            ("context_revision", expected.context_revision),
        ]
        .into_iter()
        .all(|(key, expected)| {
            object.get(key).and_then(serde_json::Value::as_u64) == Some(expected)
        }) && object.get("link_state").and_then(serde_json::Value::as_u64)
            == Some(0);
        if !matches_context {
            return Err(Error::InvalidRequest("RCS context is not current"));
        }
        let targets_mapping: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM rcs_conversation_mappings WHERE peppy_conversation_id=? AND google_conversation_id=? AND invalidated_at_ms IS NULL)",
            params![conversation_id.to_string(), google_conversation_id],
            |row| row.get(0),
        )?;
        if !targets_mapping {
            return Err(Error::InvalidRequest("RCS mapping is not current"));
        }
        let revision = expected
            .context_revision
            .checked_add(1)
            .ok_or(Error::InvalidRequest("RCS context revision exhausted"))?;
        object.insert("context_revision".into(), serde_json::Value::from(revision));
        tx.execute(
            "UPDATE metadata SET v=? WHERE k='rcs_mapping_context_v1'",
            [serde_json::to_string(&context).map_err(|_| Error::Database)?],
        )?;
        crate::rcs_mapping::invalidate_mapping(&tx, conversation_id, "google_event")?;
        tx.commit()?;
        Ok(revision)
    }
}

#[derive(Clone, Copy)]
struct StoredRcsContextTuple {
    link_generation: u64,
    session_generation: u64,
    context_revision: u64,
}

impl StoredRcsContextTuple {
    fn with_ownership(self, ownership_generation: u64) -> RcsContextExpectation {
        RcsContextExpectation {
            ownership_generation,
            link_generation: self.link_generation,
            session_generation: self.session_generation,
            context_revision: self.context_revision,
        }
    }
}

fn current_context_tuple(conn: &Connection) -> Result<Option<StoredRcsContextTuple>, Error> {
    let context: Option<String> = conn
        .query_row(
            "SELECT v FROM metadata WHERE k='rcs_mapping_context_v1'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(context) = context else {
        return Ok(None);
    };
    let context: serde_json::Value = serde_json::from_str(&context).map_err(|_| Error::Database)?;
    let object = context.as_object().ok_or(Error::Database)?;
    if object.get("link_state").and_then(serde_json::Value::as_u64) != Some(0) {
        return Ok(None);
    }
    let value = |key| {
        object
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .ok_or(Error::Database)
    };
    Ok(Some(StoredRcsContextTuple {
        link_generation: value("link_generation")?,
        session_generation: value("session_generation")?,
        context_revision: value("context_revision")?,
    }))
}

fn receive_context_in_transaction(
    conn: &mut Connection,
    device_id: DeviceId,
    google_conversation_id: &str,
    after_authority_read: impl FnOnce(),
) -> Result<Option<RcsReceiveContext>, Error> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
    if !unique_current_gateway(&tx, device_id)? {
        return Err(Error::InvalidRequest("local RCS gateway is unavailable"));
    }
    let Some(lifecycle) = crate::rcs_mapping::recorded_link_lifecycle_metadata(&tx)? else {
        return Err(Error::InvalidRequest("local RCS gateway is unavailable"));
    };
    if lifecycle.state != crate::RcsCapabilityState::Linked {
        return Err(Error::InvalidRequest("local RCS gateway is unavailable"));
    }
    after_authority_read();
    let row: Option<(String, String, i64, Option<String>)> = tx
        .query_row(
            "SELECT o.conversation_id,m.google_conversation_id,o.ownership_generation,o.history_token FROM rcs_lane_ownership o JOIN rcs_conversation_mappings m ON m.peppy_conversation_id=o.conversation_id WHERE m.google_conversation_id=? AND m.invalidated_at_ms IS NULL AND o.owner='link'",
            [google_conversation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((conversation_id, google_conversation_id, ownership_generation, history_token)) = row
    else {
        tx.commit()?;
        return Ok(None);
    };
    let context_tuple = current_context_tuple(&tx)?
        .ok_or(Error::InvalidRequest("local RCS context is unavailable"))?;
    let ownership_generation = ownership_generation as u64;
    let context = RcsReceiveContext {
        conversation_id: conversation_id.parse().map_err(|_| Error::Database)?,
        google_conversation_id,
        ownership_generation,
        lifecycle_generation: lifecycle.generation,
        owner: RcsReceiveOwner::Link,
        history_token,
        expectation: context_tuple.with_ownership(ownership_generation),
    };
    tx.commit()?;
    Ok(Some(context))
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 4_096
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn validate_provider_row(row: &RcsProviderRow) -> Result<(), Error> {
    if row.provider_row_id < 0
        || row.provider_thread_id < 0
        || row.provider_type < 0
        || row.addresses.is_empty()
        || row.addresses.len() > 100
        || row
            .addresses
            .iter()
            .any(|address| address.trim().is_empty() || address.len() > 512)
    {
        return Err(Error::InvalidRequest("invalid RCS provider row"));
    }
    Ok(())
}

fn record_provider_detector_in_transaction(
    tx: &Connection,
    ctx: &Ctx<'_>,
    conversation_id: ConversationId,
    detector: RcsProviderDetector,
) -> Result<RcsIdentityOutcome, Error> {
    let conversation_key = conversation_id.to_string();
    tx.execute(
        "UPDATE rcs_lane_ownership SET observed_detector_high_water=MAX(observed_detector_high_water,?) WHERE conversation_id=?",
        params![detector.provider_row_id, conversation_key],
    )?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM rcs_provider_detectors WHERE conversation_id=? AND provider_row_id=?)",
        params![conversation_key, detector.provider_row_id],
        |record| record.get(0),
    )?;
    if exists {
        return Ok(RcsIdentityOutcome::Duplicate);
    }
    tx.execute(
        "DELETE FROM rcs_provider_detectors WHERE conversation_id=? AND provider_row_id IN (SELECT provider_row_id FROM rcs_provider_detectors WHERE conversation_id=? AND resolved=1 AND (SELECT COUNT(*) FROM rcs_provider_detectors WHERE conversation_id=?) >= ? ORDER BY observed_at_ms,provider_row_id LIMIT 1)",
        params![conversation_key, conversation_key, conversation_key, MAX_DETECTORS_PER_CONVERSATION],
    )?;
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM rcs_provider_detectors WHERE conversation_id=?",
        [conversation_key.as_str()],
        |record| record.get(0),
    )?;
    if count >= MAX_DETECTORS_PER_CONVERSATION {
        tx.execute(
            "UPDATE rcs_lane_ownership SET alert_code='detector_overflow' WHERE conversation_id=?",
            [conversation_key.as_str()],
        )?;
        publish_metadata(tx, ctx, conversation_id, None, true)?;
        return Ok(RcsIdentityOutcome::Recorded);
    }
    tx.execute(
        "INSERT INTO rcs_provider_detectors(conversation_id,provider_row_id,provider_thread_id,provider_type,observed_at_ms,observed_generation,resolved) VALUES(?,?,?,?,?,?,0)",
        params![conversation_key, detector.provider_row_id, detector.provider_thread_id, detector.provider_type, detector.observed_at_ms, detector.observed_generation],
    )?;
    Ok(RcsIdentityOutcome::Recorded)
}

fn unique_current_gateway(conn: &Connection, device_id: DeviceId) -> Result<bool, Error> {
    let now_ms = crate::notifications::now_ms();
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM rcs_link_status_projection WHERE state='linked' AND availability=1 AND health=1 AND phone_responsive=1 AND observed_at_ms<=? AND observed_at_ms>=?",
        params![now_ms, now_ms - crate::rcs_capability::RCS_CAPABILITY_STALE_MS],
        |row| row.get(0),
    )?;
    Ok(count == 1 && crate::rcs_capability::current_gateway(conn, device_id, now_ms)?)
}

fn local_gateway(conn: &Connection, device_id: DeviceId) -> Result<bool, Error> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM rcs_link_status_projection WHERE gateway_device_id=?)",
        [device_id.to_string()],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn checkpoint_record(
    conn: &Connection,
    conversation_id: ConversationId,
    ownership_generation: u64,
    history_token: Option<&str>,
) -> Result<(), Error> {
    if let Some(token) = history_token {
        conn.execute(
            "UPDATE rcs_lane_ownership SET history_token=? WHERE conversation_id=? AND ownership_generation=?",
            params![token, conversation_id.to_string(), ownership_generation],
        )?;
    }
    Ok(())
}

fn record_identity(
    conn: &Connection,
    conversation_id: ConversationId,
    ownership_generation: u64,
    google_message_id: &str,
    google_conversation_id: &str,
    message_id: Option<MessageId>,
    require_matching_message: bool,
) -> Result<RcsIdentityOutcome, Error> {
    let existing: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT conversation_id,message_id FROM rcs_message_identities WHERE google_message_id=? AND google_conversation_id=?",
            params![google_message_id, google_conversation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((existing_conversation, existing_message)) = existing {
        if existing_conversation != conversation_id.to_string() {
            return Ok(RcsIdentityOutcome::Rejected(
                RcsReceiveCode::IdentityConflict,
            ));
        }
        if existing_message.is_none() && message_id.is_some() {
            conn.execute(
                "UPDATE rcs_message_identities SET ownership_generation=?,message_id=? WHERE google_message_id=? AND google_conversation_id=?",
                params![ownership_generation, message_id.map(|id| id.to_string()), google_message_id, google_conversation_id],
            )?;
            return Ok(RcsIdentityOutcome::Recorded);
        }
        if require_matching_message
            && let (Some(existing_message), Some(message_id)) = (
                existing_message.as_deref(),
                message_id.map(|id| id.to_string()),
            )
            && existing_message != message_id
        {
            return Ok(RcsIdentityOutcome::Rejected(
                RcsReceiveCode::IdentityConflict,
            ));
        }
        return Ok(RcsIdentityOutcome::Duplicate);
    }
    conn.execute(
        "INSERT INTO rcs_message_identities(google_message_id,google_conversation_id,conversation_id,ownership_generation,message_id) VALUES(?,?,?,?,?)",
        params![google_message_id, google_conversation_id, conversation_id.to_string(), ownership_generation, message_id.map(|id| id.to_string())],
    )?;
    Ok(RcsIdentityOutcome::Recorded)
}

fn deterministic_message_id(
    ctx: &Ctx<'_>,
    google_conversation_id: &str,
    google_message_id: &str,
) -> Result<MessageId, Error> {
    let epoch = ctx.active_epoch.ok_or(Error::Crypto)?;
    let key = ctx.keys.get(&epoch).ok_or(Error::Crypto)?;
    let bytes = compaction_hmac(
        &key.event,
        b"rcs-message-id-v1",
        &[
            google_conversation_id.as_bytes(),
            google_message_id.as_bytes(),
        ],
    );
    let mut uuid_bytes: [u8; 16] = bytes[..16].try_into().map_err(|_| Error::Crypto)?;
    uuid_bytes[6] = (uuid_bytes[6] & 0x0f) | 0x80;
    uuid_bytes[8] = (uuid_bytes[8] & 0x3f) | 0x80;
    Ok(MessageId(Uuid::from_bytes(uuid_bytes)))
}

fn apply_status(
    conn: &Connection,
    message_id: MessageId,
    status: RcsMessageStatus,
    rcs_reported: bool,
) -> Result<bool, Error> {
    let rank = status_rank(status);
    Ok(conn.execute(
        "INSERT INTO rcs_message_status(message_id,status_rank,rcs_reported) VALUES(?,?,?) ON CONFLICT(message_id) DO UPDATE SET status_rank=MAX(rcs_message_status.status_rank,excluded.status_rank),rcs_reported=MAX(rcs_message_status.rcs_reported,excluded.rcs_reported) WHERE excluded.status_rank>rcs_message_status.status_rank OR excluded.rcs_reported>rcs_message_status.rcs_reported",
        params![message_id.to_string(), rank, rcs_reported],
    )? > 0)
}

pub(crate) fn status_rank(status: RcsMessageStatus) -> i64 {
    match status {
        RcsMessageStatus::Sending => 1,
        RcsMessageStatus::Sent => 2,
        RcsMessageStatus::Failed => 3,
        RcsMessageStatus::Delivered => 4,
        RcsMessageStatus::Read => 5,
    }
}

fn status_from_rank(rank: i64) -> Result<RcsMessageStatus, rusqlite::Error> {
    match rank {
        1 => Ok(RcsMessageStatus::Sending),
        2 => Ok(RcsMessageStatus::Sent),
        3 => Ok(RcsMessageStatus::Failed),
        4 => Ok(RcsMessageStatus::Delivered),
        5 => Ok(RcsMessageStatus::Read),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn apply_reported_facts(
    conn: &Connection,
    ctx: &Ctx<'_>,
    conversation_id: ConversationId,
    message_id: MessageId,
    rcs_reported: bool,
    e2ee_reported: Option<bool>,
) -> Result<(), Error> {
    if apply_replicated_reported(conn, message_id, rcs_reported)? {
        let rcs_reported = conn.query_row(
            "SELECT rcs_reported FROM rcs_message_status WHERE message_id=?",
            [message_id.to_string()],
            |row| row.get(0),
        )?;
        enqueue(
            conn,
            ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::RcsMessageReported {
                message_id,
                rcs_reported,
            },
        )?;
    }
    if e2ee_reported.is_some() {
        let history_gap = load_receive_state(conn, conversation_id)?
            .map(|state| history_gap(state.alert))
            .unwrap_or(false);
        publish_metadata(conn, ctx, conversation_id, e2ee_reported, history_gap)?;
    }
    Ok(())
}

pub(crate) fn apply_replicated_status(
    conn: &Connection,
    message_id: MessageId,
    status: RcsMessageStatus,
    rcs_reported: bool,
) -> Result<bool, Error> {
    apply_status(conn, message_id, status, rcs_reported)
}

pub(crate) fn apply_replicated_reported(
    conn: &Connection,
    message_id: MessageId,
    rcs_reported: bool,
) -> Result<bool, Error> {
    Ok(conn.execute(
        "INSERT INTO rcs_message_status(message_id,status_rank,rcs_reported) VALUES(?,?,?) ON CONFLICT(message_id) DO UPDATE SET rcs_reported=MAX(rcs_reported,excluded.rcs_reported) WHERE excluded.rcs_reported>rcs_message_status.rcs_reported",
        params![message_id.to_string(), 0, rcs_reported],
    )? > 0)
}

pub(crate) fn apply_replicated_metadata(
    conn: &Connection,
    metadata: &RcsConversationMetadata,
    producer: DeviceId,
    sequence: u64,
) -> Result<bool, Error> {
    apply_metadata(conn, metadata, producer, sequence)
}

fn metadata_clock(conn: &Connection, conversation_id: ConversationId) -> Result<u64, Error> {
    let current: Option<i64> = conn
        .query_row(
            "SELECT logical_revision FROM rcs_conversation_metadata WHERE conversation_id=?",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    current
        .map(u64::try_from)
        .transpose()
        .map_err(|_| Error::Database)
        .map(|value| value.unwrap_or(0))
}

fn apply_metadata(
    conn: &Connection,
    metadata: &RcsConversationMetadata,
    producer: DeviceId,
    sequence: u64,
) -> Result<bool, Error> {
    if !valid_metadata(metadata, producer) {
        return Ok(false);
    }
    let revision = i64::try_from(metadata.logical_revision).map_err(|_| Error::Database)?;
    let sequence = i64::try_from(sequence).map_err(|_| Error::Database)?;
    conn.execute(
        "INSERT INTO rcs_conversation_metadata(conversation_id,transport,rcs_encryption,rcs_history_gap,logical_revision,writer_device_id,producer_device_id,source_sequence) VALUES(?,'rcs',?,?,?,?,?,?) ON CONFLICT(conversation_id) DO UPDATE SET rcs_encryption=excluded.rcs_encryption,rcs_history_gap=excluded.rcs_history_gap,logical_revision=excluded.logical_revision,writer_device_id=excluded.writer_device_id,producer_device_id=excluded.producer_device_id,source_sequence=excluded.source_sequence WHERE excluded.logical_revision>rcs_conversation_metadata.logical_revision OR (excluded.logical_revision=rcs_conversation_metadata.logical_revision AND excluded.writer_device_id>rcs_conversation_metadata.writer_device_id)",
        params![metadata.conversation_id.to_string(), metadata.rcs_encryption.map(i64::from), metadata.rcs_history_gap, revision, metadata.writer_device_id, producer.to_string(), sequence],
    )?;
    Ok(true)
}

pub(crate) fn valid_metadata(metadata: &RcsConversationMetadata, producer: DeviceId) -> bool {
    metadata.logical_revision > 0
        && metadata.logical_revision < i64::MAX as u64
        && metadata.writer_device_id == producer.to_string()
        && metadata.writer_device_id.parse::<DeviceId>().is_ok()
}

fn history_gap(alert: RcsReceiveAlert) -> bool {
    alert != RcsReceiveAlert::None
}

fn publish_metadata(
    conn: &Connection,
    ctx: &Ctx<'_>,
    conversation_id: ConversationId,
    rcs_encryption: Option<bool>,
    rcs_history_gap: bool,
) -> Result<(), Error> {
    let existing: Option<(Option<i64>, bool)> = conn
        .query_row(
            "SELECT rcs_encryption,rcs_history_gap FROM rcs_conversation_metadata WHERE conversation_id=?",
            [conversation_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let rcs_encryption = rcs_encryption.or_else(|| {
        existing
            .as_ref()
            .and_then(|(value, _)| value.map(|value| value != 0))
    });
    if existing.as_ref().is_some_and(|(value, history_gap)| {
        value.map(|value| value != 0) == rcs_encryption && *history_gap == rcs_history_gap
    }) {
        return Ok(());
    }
    let metadata = RcsConversationMetadata {
        conversation_id,
        transport: RcsConversationTransport::Rcs,
        rcs_encryption,
        rcs_history_gap,
        logical_revision: metadata_clock(conn, conversation_id)?
            .checked_add(1)
            .ok_or(Error::Database)?,
        writer_device_id: ctx.device_id.to_string(),
    };
    let (_, sequence) = enqueue(
        conn,
        ctx,
        EnvelopePurpose::Event,
        None,
        None,
        &PrivatePayload::RcsConversationMetadata(metadata.clone()),
    )?;
    apply_metadata(conn, &metadata, ctx.device_id, sequence).map(|_| ())
}

fn next_generation(conn: &Connection, conversation_id: ConversationId) -> Result<i64, Error> {
    let previous: Option<i64> = conn
        .query_row(
            "SELECT ownership_generation FROM rcs_lane_ownership WHERE conversation_id=?",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    previous.unwrap_or(0).checked_add(1).ok_or(Error::Database)
}

fn load_ownership(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<Option<RcsOwnership>, Error> {
    let row: Option<(String, i64, Option<String>, i64, i64, i64)> = conn.query_row(
        "SELECT owner,ownership_generation,history_token,detector_floor,proof_time_ms,observed_detector_high_water FROM rcs_lane_ownership WHERE conversation_id=?",
        [conversation_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    )
    .optional()?;
    row.map(
        |(
            owner,
            ownership_generation,
            history_token,
            detector_floor,
            proof_time_ms,
            observed_detector_high_water,
        )| {
            Ok(RcsOwnership {
                conversation_id,
                owner: RcsReceiveOwner::from_code(&owner)?,
                ownership_generation: ownership_generation as u64,
                history_token,
                detector_floor,
                proof_time_ms,
                observed_detector_high_water,
            })
        },
    )
    .transpose()
}

fn load_receive_state(
    conn: &Connection,
    conversation_id: ConversationId,
) -> Result<Option<RcsReceiveState>, Error> {
    conn.query_row(
            "SELECT owner,ownership_generation,history_token,detector_floor,proof_time_ms,observed_detector_high_water,alert_code FROM rcs_lane_ownership WHERE conversation_id=?",
        [conversation_id.to_string()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get::<_, Option<String>>(6)?)),
    )
    .optional()?
    .map(|(owner, ownership_generation, history_token, detector_floor, proof_time_ms, observed_detector_high_water, alert_code)| {
        Ok(RcsReceiveState {
            owner: RcsReceiveOwner::from_code(&owner)?,
            ownership_generation: ownership_generation as u64,
            history_token,
            detector_floor,
            proof_time_ms,
            observed_detector_high_water,
            alert: RcsReceiveAlert::from_code(alert_code.as_deref())?,
        })
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientConfig, DatabaseKey, DeviceId, VaultId};

    fn config(directory: &tempfile::TempDir) -> ClientConfig {
        ClientConfig {
            database_path: directory.path().join("receive.db"),
            vault_id: VaultId::new(),
            device_id: DeviceId::new(),
        }
    }

    #[test]
    fn version_48_upgrade_preserves_rows_and_rejects_downgrade() {
        let directory = tempfile::TempDir::new().unwrap();
        let config = config(&directory);
        let client = Client::open(config.clone(), DatabaseKey::new(&[17; 32]).unwrap()).unwrap();
        {
            let store = client.lock().unwrap();
            store.conn.execute_batch(
                "INSERT INTO rcs_link_status_projection VALUES('gateway','linked',1,1,1,1,1,1,1);
                 DROP TABLE rcs_message_status;
                 DROP TABLE rcs_lane_ownership;
                 DROP TABLE rcs_conversation_metadata;
                 DROP TABLE rcs_message_identities;
                 DROP TABLE rcs_provider_detectors;
                 UPDATE schema_meta SET version=48;",
            ).unwrap();
        }
        drop(client);
        let upgraded = Client::open(config.clone(), DatabaseKey::new(&[17; 32]).unwrap()).unwrap();
        let store = upgraded.lock().unwrap();
        let state: String = store
            .conn
            .query_row(
                "SELECT state FROM rcs_link_status_projection WHERE gateway_device_id='gateway'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "linked");
        let table: String = store
            .conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='rcs_lane_ownership'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table, "rcs_lane_ownership");
        store
            .conn
            .execute(
                "UPDATE schema_meta SET version=?",
                [crate::SCHEMA_VERSION + 1],
            )
            .unwrap();
        drop(store);
        drop(upgraded);
        assert!(matches!(
            Client::open(config, DatabaseKey::new(&[17; 32]).unwrap()),
            Err(Error::UnsupportedSchema)
        ));
    }

    #[test]
    fn status_merge_keeps_reported_evidence_when_rank_does_not_improve() {
        let directory = tempfile::TempDir::new().unwrap();
        let client =
            Client::open(config(&directory), DatabaseKey::new(&[17; 32]).unwrap()).unwrap();
        let store = client.lock().unwrap();
        let equal_rank = MessageId::new();
        let higher_rank = MessageId::new();

        assert!(apply_status(&store.conn, equal_rank, RcsMessageStatus::Read, false).unwrap());
        assert!(apply_status(&store.conn, equal_rank, RcsMessageStatus::Sent, true).unwrap());
        assert!(apply_status(&store.conn, higher_rank, RcsMessageStatus::Sent, true).unwrap());
        assert!(apply_status(&store.conn, higher_rank, RcsMessageStatus::Read, false).unwrap());

        for message_id in [equal_rank, higher_rank] {
            assert_eq!(
                store
                    .conn
                    .query_row(
                        "SELECT status_rank,rcs_reported FROM rcs_message_status WHERE message_id=?",
                        [message_id.to_string()],
                        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?)),
                    )
                    .unwrap(),
                (5, true)
            );
        }
    }
}

#[cfg(test)]
mod receive_context_snapshot_tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn receive_context_uses_one_snapshot_across_authority_and_mapping_reads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("receive-context-snapshot.sqlite");
        let mut reader = Connection::open(&path).unwrap();
        reader.pragma_update(None, "journal_mode", "WAL").unwrap();
        reader
            .execute_batch(
                "CREATE TABLE metadata(k TEXT PRIMARY KEY,v TEXT NOT NULL);
                 CREATE TABLE rcs_link_status_projection(gateway_device_id TEXT PRIMARY KEY,state TEXT NOT NULL,availability INTEGER NOT NULL,health INTEGER NOT NULL,phone_responsive INTEGER NOT NULL,observed_at_ms INTEGER NOT NULL);
                 CREATE TABLE rcs_lane_ownership(conversation_id TEXT PRIMARY KEY,owner TEXT NOT NULL,ownership_generation INTEGER NOT NULL,history_token TEXT);
                 CREATE TABLE rcs_conversation_mappings(peppy_conversation_id TEXT PRIMARY KEY,google_conversation_id TEXT NOT NULL,invalidated_at_ms INTEGER);",
            )
            .unwrap();
        let device_id = DeviceId::new();
        let conversation_id = ConversationId::new();
        let now_ms = crate::notifications::now_ms();
        reader
            .execute(
                "INSERT INTO rcs_link_status_projection VALUES(?,?,?,?,?,?)",
                params![device_id.to_string(), "linked", true, true, true, now_ms],
            )
            .unwrap();
        for (key, value) in [
            ("rcs_link_lifecycle_v1", "\"linked\""),
            ("rcs_link_lifecycle_observed_at_ms_v1", &now_ms.to_string()),
            ("rcs_link_lifecycle_generation_v1", "1"),
        ] {
            reader
                .execute("INSERT INTO metadata(k,v) VALUES(?,?)", params![key, value])
                .unwrap();
        }
        reader
            .execute(
                "INSERT INTO metadata(k,v) VALUES('rcs_mapping_context_v1',?)",
                [serde_json::json!({
                    "link_generation": 3,
                    "session_generation": 4,
                    "context_revision": 5,
                    "link_state": 0
                })
                .to_string()],
            )
            .unwrap();
        reader
            .execute(
                "INSERT INTO rcs_lane_ownership VALUES(?,'link',7,'page-token')",
                [conversation_id.to_string()],
            )
            .unwrap();
        reader
            .execute(
                "INSERT INTO rcs_conversation_mappings VALUES(?,'google-conversation',NULL)",
                [conversation_id.to_string()],
            )
            .unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let writer_barrier = barrier.clone();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            let writer = Connection::open(writer_path).unwrap();
            writer_barrier.wait();
            writer
                .execute(
                    "UPDATE rcs_conversation_mappings SET invalidated_at_ms=?",
                    [now_ms + 1],
                )
                .unwrap();
            writer
                .execute(
                    "UPDATE metadata SET v='2' WHERE k='rcs_link_lifecycle_generation_v1'",
                    [],
                )
                .unwrap();
            writer
                .execute(
                    "UPDATE metadata SET v=? WHERE k='rcs_mapping_context_v1'",
                    [serde_json::json!({
                        "link_generation": 30,
                        "session_generation": 40,
                        "context_revision": 50,
                        "link_state": 0
                    })
                    .to_string()],
                )
                .unwrap();
            writer_barrier.wait();
        });
        let read_barrier = barrier.clone();
        let context = receive_context_in_transaction(
            &mut reader,
            device_id,
            "google-conversation",
            move || {
                read_barrier.wait();
                read_barrier.wait();
            },
        )
        .unwrap()
        .unwrap();
        writer.join().unwrap();

        assert_eq!(context.conversation_id, conversation_id);
        assert_eq!(context.google_conversation_id, "google-conversation");
        assert_eq!(context.ownership_generation, 7);
        assert_eq!(context.lifecycle_generation, 1);
        assert_eq!(context.history_token.as_deref(), Some("page-token"));
        assert_eq!(
            context.expectation,
            RcsContextExpectation {
                ownership_generation: 7,
                link_generation: 3,
                session_generation: 4,
                context_revision: 5,
            }
        );
    }
}
