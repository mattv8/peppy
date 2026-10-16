//! Provider evidence reconciliation without provider-invented command identities.

use crate::{
    ConversationId, DeviceId, Direction, Error, MessageId, MessagePayload, SourceSequence,
    Transport,
};
use rusqlite::{Connection, OptionalExtension, params};

const PEPPY_ANDROID_PACKAGE: &str = "dev.peppy.mobile";
const PEPPY_CLOCK_SKEW_MS: i64 = 120_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSmsEvidence {
    pub source_generation: String,
    pub subscription_id: String,
    pub provider_message_id: String,
    pub source_row_fingerprint: String,
    pub provider_thread_id: Option<String>,
    pub direction: Direction,
    /// Sender for incoming rows and the sole recipient for outgoing rows.
    pub counterpart: String,
    pub body: String,
    pub creator: Option<String>,
    pub date_ms: Option<i64>,
    pub date_sent_ms: Option<i64>,
    /// Identifier retained from a prior incoming broadcast, when one exists.
    pub broadcast_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmsReconciledMessage {
    pub message_id: MessageId,
    pub conversation_id: ConversationId,
    pub source_sequence: SourceSequence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SmsReconciliation {
    Matched(SmsReconciledMessage),
    NoMatch,
    Ambiguous,
}

/// Finds the one existing message that actual provider evidence can safely identify.
/// Callers persist the returned mapping; this function deliberately has no side effects.
pub fn reconcile_provider_sms(
    conn: &Connection,
    gateway_device_id: &DeviceId,
    evidence: &ProviderSmsEvidence,
) -> Result<SmsReconciliation, Error> {
    if let Some(existing) = existing_source(conn, evidence)? {
        return Ok(SmsReconciliation::Matched(existing));
    }
    if evidence.direction == Direction::Incoming {
        if let Some(alias) = evidence.broadcast_id.as_deref() {
            // An alias claimed by another provider row identifies a distinct repeated message.
            if let Some(existing) = alias_message(
                conn,
                &evidence.source_generation,
                &evidence.subscription_id,
                alias,
            )? && unclaimed(conn, &existing.message_id.to_string())?
            {
                return Ok(SmsReconciliation::Matched(existing));
            }
        }
        return incoming_match(conn, gateway_device_id, evidence);
    }
    outgoing_match(conn, gateway_device_id, evidence)
}

fn existing_source(
    conn: &Connection,
    evidence: &ProviderSmsEvidence,
) -> Result<Option<SmsReconciledMessage>, Error> {
    let direction = direction_code(evidence.direction);
    let message_id = conn
        .query_row(
            "SELECT message_id FROM sms_acquisitions WHERE source_generation=? AND subscription_id=? AND provider_message_id=? AND source_row_fingerprint=? AND direction=? AND state='complete'",
            params![evidence.source_generation, evidence.subscription_id, evidence.provider_message_id, evidence.source_row_fingerprint, direction],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    message_id.map(|id| message_by_id(conn, &id)).transpose()
}

fn alias_message(
    conn: &Connection,
    generation: &str,
    subscription: &str,
    alias: &str,
) -> Result<Option<SmsReconciledMessage>, Error> {
    let message_id = conn
        .query_row(
            "SELECT message_id FROM sms_provider_aliases WHERE source_generation=? AND subscription_id=? AND alias=?",
            params![generation, subscription, alias],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    message_id.map(|id| message_by_id(conn, &id)).transpose()
}

/// Incoming provider rows reuse a broadcast capture of the same message: first by the
/// deterministic broadcast identity the host derives from the row, then by indexed evidence
/// (SMSC time, canonical sender and exact body). Each broadcast message is consumed by at most
/// one provider row; equal candidates are interchangeable, so the oldest unclaimed one is used.
fn incoming_match(
    conn: &Connection,
    gateway_device_id: &DeviceId,
    evidence: &ProviderSmsEvidence,
) -> Result<SmsReconciliation, Error> {
    if let Some(broadcast_id) = evidence.broadcast_id.as_deref() {
        let candidate: Option<String> = conn
            .query_row(
                "SELECT message_id FROM sms_broadcast_aliases WHERE broadcast_id=?
                 UNION ALL SELECT id FROM messages WHERE source_device_id=? AND provider_message_id=?
                 LIMIT 1",
                params![broadcast_id, gateway_device_id.to_string(), broadcast_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(message) = candidate
            && unclaimed(conn, &message)?
        {
            return message_by_id(conn, &message).map(SmsReconciliation::Matched);
        }
    }
    let Some(smsc) = evidence.date_sent_ms.filter(|time| *time > 0) else {
        return Ok(SmsReconciliation::NoMatch);
    };
    let counterpart = canonical_counterpart(conn, &evidence.counterpart)?;
    let mut statement = conn.prepare(
        "SELECT b.message_id,m.payload FROM sms_broadcast_aliases b JOIN messages m ON m.id=b.message_id
         WHERE b.smsc_at_ms=? AND m.source_device_id=?
           AND NOT EXISTS (SELECT 1 FROM sms_acquisitions x WHERE x.message_id=b.message_id AND x.state='complete')
         ORDER BY m.local_order",
    )?;
    let rows = statement
        .query_map(params![smsc, gateway_device_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (message, encoded) in rows {
        let payload: MessagePayload = crate::decode(encoded.as_bytes())?;
        let same_sender = payload
            .sender_address
            .as_deref()
            .map(|sender| canonical_counterpart(conn, sender))
            .transpose()?
            .is_some_and(|sender| sender == counterpart);
        if payload.direction == Direction::Incoming && payload.body == evidence.body && same_sender
        {
            return message_by_id(conn, &message).map(SmsReconciliation::Matched);
        }
    }
    Ok(SmsReconciliation::NoMatch)
}

fn unclaimed(conn: &Connection, message_id: &str) -> Result<bool, Error> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sms_acquisitions WHERE message_id=? AND state='complete' LIMIT 1",
            params![message_id],
            |_| Ok(()),
        )
        .optional()?
        .is_none())
}

/// Outgoing provider rows reconcile to one unconsumed gateway send attempt on the same route with
/// the same canonical recipient and exact body. Android writes `date_sent=0` for sends, so the
/// provider `date` is compared with the gateway's durable attempt time. A Peppy-created row may
/// also claim a legacy attempt recorded before attempt times existed; a row created by another
/// app never reconciles, because that app really sent its own message.
fn outgoing_match(
    conn: &Connection,
    gateway_device_id: &DeviceId,
    evidence: &ProviderSmsEvidence,
) -> Result<SmsReconciliation, Error> {
    let peppy_created = match evidence.creator.as_deref() {
        Some(PEPPY_ANDROID_PACKAGE) => true,
        None => false,
        Some(_) => return Ok(SmsReconciliation::NoMatch),
    };
    let provider_time = provider_time(evidence);
    if provider_time.is_none() && !peppy_created {
        return Ok(SmsReconciliation::NoMatch);
    }
    let counterpart = canonical_counterpart(conn, &evidence.counterpart)?;
    let mut statement = conn.prepare(
        "SELECT m.id,m.payload
         FROM attempts a JOIN commands c ON c.command_id=a.command_id
         JOIN command_ledger l ON l.command_id=a.command_id JOIN messages m ON m.id=c.message_id
         WHERE c.gateway_device_id=?1 AND l.subscription_id=?2
           AND a.state NOT IN ('failed_before_submission','failed_confirmed')
           AND ((?3 IS NOT NULL AND a.attempted_at_ms BETWEEN ?3-?4 AND ?3+?4)
                OR (?5 AND (a.attempted_at_ms IS NULL OR ?3 IS NULL)))
           AND NOT EXISTS (SELECT 1 FROM sms_acquisitions x WHERE x.message_id=m.id AND x.state='complete')",
    )?;
    let rows = statement
        .query_map(
            params![
                gateway_device_id.to_string(),
                evidence.subscription_id,
                provider_time,
                PEPPY_CLOCK_SKEW_MS,
                peppy_created
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let mut candidates = Vec::new();
    for (id, encoded) in rows {
        let payload: MessagePayload = crate::decode(encoded.as_bytes())?;
        let same_counterpart = payload.recipients.len() == 1
            && canonical_counterpart(conn, &payload.recipients[0])? == counterpart;
        if payload.transport == Transport::Sms
            && payload.direction == Direction::Outgoing
            && payload.body == evidence.body
            && same_counterpart
        {
            candidates.push(message_by_id(conn, &id)?);
        }
    }
    unique(candidates)
}

/// Provider `date` is the local submission time for sends; `date_sent` is 0 there and only a
/// positive value is evidence.
fn provider_time(evidence: &ProviderSmsEvidence) -> Option<i64> {
    evidence
        .date_ms
        .filter(|time| *time > 0)
        .or(evidence.date_sent_ms.filter(|time| *time > 0))
}

pub(crate) fn canonical_counterpart(conn: &Connection, counterpart: &str) -> Result<String, Error> {
    Ok(crate::conversation_match::canonical_address(
        counterpart,
        Some(&crate::conversation_match::identity_region(conn)?),
    ))
}

fn message_by_id(conn: &Connection, message_id: &str) -> Result<SmsReconciledMessage, Error> {
    conn.query_row(
        "SELECT id,conversation_id,source_sequence FROM messages WHERE id=?",
        params![message_id],
        |row| {
            let sequence: i64 = row.get(2)?;
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, sequence))
        },
    )
    .map_err(Into::into)
    .and_then(|(id, conversation, sequence)| {
        Ok(SmsReconciledMessage {
            message_id: crate::parse(&id)?,
            conversation_id: crate::parse(&conversation)?,
            source_sequence: SourceSequence(u64::try_from(sequence).map_err(|_| Error::Database)?),
        })
    })
}

fn unique(mut candidates: Vec<SmsReconciledMessage>) -> Result<SmsReconciliation, Error> {
    match candidates.len() {
        0 => Ok(SmsReconciliation::NoMatch),
        1 => Ok(SmsReconciliation::Matched(candidates.remove(0))),
        _ => Ok(SmsReconciliation::Ambiguous),
    }
}

fn direction_code(direction: Direction) -> &'static str {
    match direction {
        Direction::Incoming => "incoming",
        Direction::Outgoing => "outgoing",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_alias_returns_the_original_message_identity() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE contact_books(body TEXT NOT NULL, forgotten INTEGER NOT NULL); CREATE TABLE messages(id TEXT PRIMARY KEY,conversation_id TEXT NOT NULL,source_sequence INTEGER NOT NULL); CREATE TABLE sms_acquisitions(source_generation TEXT,subscription_id TEXT,provider_message_id TEXT,source_row_fingerprint TEXT,direction TEXT,state TEXT,message_id TEXT,input TEXT); CREATE TABLE sms_provider_aliases(source_generation TEXT,subscription_id TEXT,alias TEXT,message_id TEXT);").unwrap();
        let message = MessageId::new();
        let conversation = ConversationId::new();
        conn.execute(
            "INSERT INTO messages VALUES(?,?,7)",
            params![message.to_string(), conversation.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sms_provider_aliases VALUES('install','sim','broadcast',?)",
            params![message.to_string()],
        )
        .unwrap();
        let evidence = ProviderSmsEvidence {
            source_generation: "install".into(),
            subscription_id: "sim".into(),
            provider_message_id: "provider-row".into(),
            source_row_fingerprint: "fingerprint".into(),
            provider_thread_id: None,
            direction: Direction::Incoming,
            counterpart: "+14155552671".into(),
            body: "same body".into(),
            creator: None,
            date_ms: Some(10),
            date_sent_ms: None,
            broadcast_id: Some("broadcast".into()),
        };

        let result = reconcile_provider_sms(&conn, &DeviceId::new(), &evidence).unwrap();
        assert_eq!(
            result,
            SmsReconciliation::Matched(SmsReconciledMessage {
                message_id: message,
                conversation_id: conversation,
                source_sequence: SourceSequence(7)
            })
        );
    }
}
