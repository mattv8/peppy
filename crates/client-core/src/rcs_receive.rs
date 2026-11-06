use crate::{
    Client, ConversationId, Ctx, DeviceId, Direction, EnvelopePurpose, Error, Inserted, MessageId,
    MessagePayload, MessageRecord, PrivatePayload, RcsMessageStatus, Transport, enqueue,
    insert_message, next_source_sequence,
};
use peppy_crypto::compaction_hmac;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
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
        )?;
        if outcome != RcsIdentityOutcome::Recorded {
            return Ok(outcome);
        }
        if let Some(token) = history_token {
            tx.execute(
                "UPDATE rcs_lane_ownership SET history_token=? WHERE conversation_id=? AND ownership_generation=?",
                params![token, conversation_id.to_string(), ownership_generation],
            )?;
        }
        tx.commit()?;
        Ok(RcsIdentityOutcome::Recorded)
    }

    pub fn ingest_rcs_projected_record(
        &self,
        conversation_id: ConversationId,
        ownership_generation: u64,
        record: RcsProjectedRecord,
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
        if record.history_token.is_some()
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
                        checkpoint_record(&tx, conversation_id, ownership_generation, record.history_token.as_deref())?;
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
                        checkpoint_record(&tx, conversation_id, ownership_generation, record.history_token.as_deref())?;
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
                    imported: record.history_token.is_some(),
                    sent_at_ms: record.sent_at_ms,
                    ordered_at_ms: None,
                    mms_context: None,
                };
                if insert_message(&tx, &payload)? != Inserted::New {
                    return Err(Error::Database);
                }
                tx.execute(
                    "INSERT INTO rcs_message_status(message_id,status_rank,rcs_reported) VALUES(?,?,?)",
                    params![message_id.to_string(), 0, record.rcs_reported],
                )?;
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
        if let Some(token) = record.history_token {
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
        let mut identity_recorded = false;
        let mut message_id: Option<String> = tx.query_row(
            "SELECT message_id FROM rcs_message_identities WHERE google_message_id=? AND google_conversation_id=? AND conversation_id=?",
            params![application.google_message_id, application.google_conversation_id, conversation_id.to_string()],
            |row| row.get(0),
        ).optional()?.flatten();
        if message_id.is_none()
            && let Some(tmp_id) = application.tmp_id.as_deref()
        {
            message_id = tx
                .query_row(
                    "SELECT c.message_id FROM rcs_send_ledger l JOIN commands c ON c.command_id=l.command_id WHERE l.tmp_id=? AND l.google_conversation_id=?",
                    params![tmp_id, application.google_conversation_id],
                    |row| row.get(0),
            )
            .optional()?;
            if let Some(message_id) = message_id.as_deref() {
                match record_identity(
                    &tx,
                    conversation_id,
                    ownership_generation,
                    &application.google_message_id,
                    &application.google_conversation_id,
                    Some(message_id.parse().map_err(|_| Error::Database)?),
                )? {
                    RcsIdentityOutcome::Recorded => identity_recorded = true,
                    RcsIdentityOutcome::Duplicate => {}
                    RcsIdentityOutcome::Rejected(code) => {
                        return Ok(RcsReceiveOutcome::Rejected(code));
                    }
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
        if !apply_status(&tx, message_id, status, true)? {
            if identity_recorded {
                tx.commit()?;
            }
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
        if !unique_current_gateway(&tx, ctx.device_id)? {
            return Ok(RcsIdentityOutcome::Rejected(
                RcsReceiveCode::GatewayUnavailable,
            ));
        }
        let conversation_key = conversation_id.to_string();
        tx.execute(
            "UPDATE rcs_lane_ownership SET observed_detector_high_water=MAX(observed_detector_high_water,?) WHERE conversation_id=?",
            params![detector.provider_row_id, conversation_key],
        )?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM rcs_provider_detectors WHERE conversation_id=? AND provider_row_id=?)",
            params![conversation_key, detector.provider_row_id],
            |row| row.get(0),
        )?;
        if exists {
            tx.commit()?;
            return Ok(RcsIdentityOutcome::Duplicate);
        }
        tx.execute(
            "DELETE FROM rcs_provider_detectors WHERE conversation_id=? AND provider_row_id IN (
                SELECT provider_row_id FROM rcs_provider_detectors
                WHERE conversation_id=? AND resolved=1
                  AND (SELECT COUNT(*) FROM rcs_provider_detectors WHERE conversation_id=?) >= ?
                ORDER BY observed_at_ms,provider_row_id
                LIMIT 1
            )",
            params![
                conversation_key,
                conversation_key,
                conversation_key,
                MAX_DETECTORS_PER_CONVERSATION
            ],
        )?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM rcs_provider_detectors WHERE conversation_id=?",
            [conversation_key.as_str()],
            |row| row.get(0),
        )?;
        if count >= MAX_DETECTORS_PER_CONVERSATION {
            tx.execute(
                "UPDATE rcs_lane_ownership SET alert_code='detector_overflow' WHERE conversation_id=?",
                [conversation_key.as_str()],
            )?;
            publish_metadata(&tx, &ctx, conversation_id, None, true)?;
            tx.commit()?;
            return Ok(RcsIdentityOutcome::Recorded);
        }
        tx.execute(
            "INSERT INTO rcs_provider_detectors(conversation_id,provider_row_id,provider_thread_id,provider_type,observed_at_ms,observed_generation,resolved) VALUES(?,?,?,?,?,?,0)",
            params![conversation_key, detector.provider_row_id, detector.provider_thread_id, detector.provider_type, detector.observed_at_ms, detector.observed_generation],
        )?;
        tx.commit()?;
        Ok(RcsIdentityOutcome::Recorded)
    }

    /// Freezes link writing and makes provider ownership effective for rows after this floor.
    pub fn begin_rcs_unlink_cutover(
        &self,
        conversation_id: ConversationId,
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
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 4_096
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
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
    let rank = |status| match status {
        RcsMessageStatus::Sending => 1,
        RcsMessageStatus::Sent => 2,
        RcsMessageStatus::Failed => 3,
        RcsMessageStatus::Delivered => 4,
        RcsMessageStatus::Read => 5,
    };
    let current: Option<i64> = conn
        .query_row(
            "SELECT status_rank FROM rcs_message_status WHERE message_id=?",
            [message_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if current.is_some_and(|current| current >= rank(status)) {
        return Ok(false);
    }
    conn.execute("INSERT INTO rcs_message_status(message_id,status_rank,rcs_reported) VALUES(?,?,?) ON CONFLICT(message_id) DO UPDATE SET status_rank=excluded.status_rank,rcs_reported=excluded.rcs_reported", params![message_id.to_string(), rank(status), rcs_reported])?;
    Ok(true)
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
    apply_replicated_reported(conn, message_id, rcs_reported)?;
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
    conn.execute(
        "INSERT INTO rcs_message_status(message_id,status_rank,rcs_reported) VALUES(?,?,?) ON CONFLICT(message_id) DO UPDATE SET rcs_reported=MAX(rcs_reported,excluded.rcs_reported)",
        params![message_id.to_string(), 0, rcs_reported],
    )?;
    Ok(true)
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
    if metadata.logical_revision == 0
        || metadata.logical_revision >= i64::MAX as u64
        || metadata.writer_device_id != producer.to_string()
        || metadata.writer_device_id.parse::<DeviceId>().is_err()
    {
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
    let existing: Option<Option<i64>> = conn
        .query_row(
            "SELECT rcs_encryption FROM rcs_conversation_metadata WHERE conversation_id=?",
            [conversation_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let metadata = RcsConversationMetadata {
        conversation_id,
        transport: RcsConversationTransport::Rcs,
        rcs_encryption: rcs_encryption.or_else(|| existing.flatten().map(|value| value != 0)),
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
            .execute("UPDATE schema_meta SET version=50", [])
            .unwrap();
        drop(store);
        drop(upgraded);
        assert!(matches!(
            Client::open(config, DatabaseKey::new(&[17; 32]).unwrap()),
            Err(Error::UnsupportedSchema)
        ));
    }
}
