use crate::{
    Client, ConversationId, DeviceId, Error, MAX_ADDRESS_BYTES, MmsReplyContext, PrivatePayload,
    Transport, enqueue, require_address, to_i64,
};
use peppy_protocol::EnvelopePurpose;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::collections::BTreeSet;

const LATEST_MMS_SQL: &str = "SELECT payload FROM messages WHERE conversation_id=? AND json_extract(payload,'$.transport')='mms' ORDER BY sort_at_ms DESC,local_order DESC LIMIT 1";
const LATEST_CONTEXTUAL_MMS_SQL: &str = "SELECT payload FROM messages WHERE conversation_id=? AND json_extract(payload,'$.mms_context') IS NOT NULL ORDER BY sort_at_ms DESC,local_order DESC LIMIT 1";

pub(crate) fn install_query_indexes(conn: &rusqlite::Connection) -> Result<(), Error> {
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS messages_latest_mms
           ON messages(conversation_id,sort_at_ms DESC,local_order DESC)
           WHERE json_extract(payload,'$.transport')='mms';
         CREATE INDEX IF NOT EXISTS messages_latest_mms_context
           ON messages(conversation_id,sort_at_ms DESC,local_order DESC)
           WHERE json_extract(payload,'$.mms_context') IS NOT NULL;",
    )?;
    Ok(())
}

impl Client {
    /// Stores an accepted detected or manually confirmed own address for a SIM and publishes one encrypted metadata
    /// event. It never changes carrier state or existing conversation identity.
    pub fn set_mms_own_address(&self, subscription_id: &str, address: &str) -> Result<(), Error> {
        validate_own_address(address)?;
        validate_subscription_id(subscription_id)?;
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT address,revision FROM mms_own_addresses WHERE source_device_id=? AND subscription_id=?",
                params![ctx.device_id.to_string(), subscription_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        if existing
            .as_ref()
            .is_some_and(|(stored, _)| stored == address)
        {
            return Ok(tx.rollback()?);
        }
        let revision = existing
            .map(|(_, value)| u64::try_from(value).map_err(|_| Error::Database))
            .transpose()?
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(Error::Database)?;
        apply_mms_own_address(&tx, ctx.device_id, subscription_id, address, revision)?;
        enqueue(
            &tx,
            &ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::MmsOwnAddress {
                source_device_id: ctx.device_id,
                subscription_id: subscription_id.to_owned(),
                address: address.to_owned(),
                revision,
            },
        )?;
        Ok(tx.commit()?)
    }

    /// Returns this device's stored own address for a SIM route.
    pub fn mms_own_address(&self, subscription_id: &str) -> Result<Option<String>, Error> {
        validate_subscription_id(subscription_id)?;
        let mut guard = self.lock()?;
        let (conn, ctx) = guard.parts();
        Ok(conn
            .query_row(
                "SELECT address FROM mms_own_addresses WHERE source_device_id=? AND subscription_id=?",
                params![ctx.device_id.to_string(), subscription_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Reply recipients for the latest MMS in a conversation.
    pub fn mms_reply_context(
        &self,
        conversation_id: ConversationId,
    ) -> Result<MmsReplyContext, Error> {
        let s = self.lock()?;
        let payload: Option<crate::MessagePayload> = s
            .conn
            .query_row(
                LATEST_MMS_SQL,
                params![conversation_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|json| crate::decode(json.as_bytes()))
            .transpose()?;
        let Some(payload) = payload.filter(|payload| payload.transport == Transport::Mms) else {
            return Err(Error::NotFound);
        };
        // Locally created outgoing replies have no provider source; retain the latest immutable
        // incoming/contextual source in this conversation for own-address confirmation.
        let contextual: Option<crate::MessagePayload> = s
            .conn
            .query_row(
                LATEST_CONTEXTUAL_MMS_SQL,
                params![conversation_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|json| crate::decode(json.as_bytes()))
            .transpose()?;
        let identity = payload
            .mms_context
            .as_ref()
            .map(|context| (&payload, context))
            .or_else(|| {
                contextual.as_ref().and_then(|message| {
                    message
                        .mms_context
                        .as_ref()
                        .map(|context| (message, context))
                })
            });
        let own = match identity {
            Some((message, context)) => s
                .conn
                .query_row(
                    "SELECT address FROM mms_own_addresses WHERE source_device_id=? AND subscription_id=?",
                    params![message.source_device_id.to_string(), context.subscription_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?,
            None => None,
        };
        let sender = payload.sender_address.clone();
        let candidates = match payload.direction {
            crate::Direction::Incoming => payload
                .sender_address
                .into_iter()
                .chain(payload.recipients)
                .collect::<Vec<_>>(),
            // The provider sender of an outgoing MMS is self (and may be the literal AOSP
            // insert-address-token), never a reply recipient.
            crate::Direction::Outgoing => payload.recipients,
        };
        let region = crate::conversation_match::identity_region(&s.conn)?;
        let mut unique_keys = BTreeSet::new();
        let unique = candidates
            .into_iter()
            .filter(|address| {
                unique_keys.insert(crate::conversation_match::canonical_address(
                    address,
                    Some(&region),
                ))
            })
            .collect::<Vec<_>>();
        let group = unique.len() > 1;
        let confirmed_own = own.as_deref().filter(|own| {
            unique.iter().any(|address| {
                crate::conversation_match::confirmed_own_address(own, address, Some(&region))
            })
        });
        let recipients = match confirmed_own {
            Some(own) => unique
                .into_iter()
                .filter(|address| {
                    !crate::conversation_match::confirmed_own_address(own, address, Some(&region))
                })
                .collect(),
            None if payload.direction == crate::Direction::Incoming
                && group
                && unique.len() == 2
                && sender.is_some() =>
            {
                vec![sender.unwrap()]
            }
            None => unique,
        };
        Ok(MmsReplyContext {
            recipients,
            subject: payload.subject,
        })
    }
}

pub(crate) fn valid_mms_own_address(subscription_id: &str, address: &str, revision: u64) -> bool {
    validate_own_address(address).is_ok()
        && validate_subscription_id(subscription_id).is_ok()
        && i64::try_from(revision).is_ok()
}

pub(crate) fn apply_mms_own_address(
    conn: &rusqlite::Connection,
    source_device_id: DeviceId,
    subscription_id: &str,
    address: &str,
    revision: u64,
) -> Result<bool, Error> {
    validate_own_address(address)?;
    validate_subscription_id(subscription_id)?;
    let revision = to_i64(revision)?;
    let changed = conn.execute(
        "INSERT INTO mms_own_addresses(source_device_id,subscription_id,address,revision) VALUES(?,?,?,?)
         ON CONFLICT(source_device_id,subscription_id) DO UPDATE SET address=excluded.address,revision=excluded.revision
         WHERE excluded.revision > mms_own_addresses.revision",
        params![source_device_id.to_string(), subscription_id, address, revision],
    )?;
    Ok(changed != 0)
}

fn validate_own_address(address: &str) -> Result<(), Error> {
    require_address(address)?;
    if address.len() > MAX_ADDRESS_BYTES || address.trim() != address {
        return Err(Error::InvalidRequest("own MMS address"));
    }
    let digits = address.strip_prefix('+').unwrap_or(address);
    if !(7..=15).contains(&digits.len()) || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::InvalidRequest("own MMS address"));
    }
    Ok(())
}

fn validate_subscription_id(subscription_id: &str) -> Result<(), Error> {
    if subscription_id.is_empty() || subscription_id.len() > crate::MAX_SUBSCRIPTION_BYTES {
        return Err(Error::InvalidRequest("subscription id"));
    }
    Ok(())
}

pub(crate) fn same_own_address(own: &str, participant: &str) -> bool {
    let own = own.trim();
    let participant = participant.trim();
    if own.starts_with('+') && participant.starts_with('+') {
        return false;
    }
    let Some(own_digits) = phone_digits(own) else {
        return false;
    };
    let Some(participant_digits) = phone_digits(participant) else {
        return false;
    };
    if own_digits.len() < 10 || participant_digits.len() < 10 {
        return false;
    }
    // Directionality: if exactly one side has '+', that side's digit count must be >= the other's.
    let own_has_plus = own.starts_with('+');
    let participant_has_plus = participant.starts_with('+');
    if own_has_plus != participant_has_plus {
        if own_has_plus && own_digits.len() < participant_digits.len() {
            return false;
        }
        if participant_has_plus && participant_digits.len() < own_digits.len() {
            return false;
        }
    }
    let (longer, shorter) = if own_digits.len() >= participant_digits.len() {
        (own_digits, participant_digits)
    } else {
        (participant_digits, own_digits)
    };
    longer.len() - shorter.len() <= 3 && longer.ends_with(&shorter)
}

fn phone_digits(address: &str) -> Option<String> {
    if address.is_empty()
        || !address
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '(' | ')' | '.'))
    {
        return None;
    }
    Some(address.chars().filter(|c| c.is_ascii_digit()).collect())
}

#[cfg(test)]
#[path = "mms_reply_query_budget.rs"]
mod query_budget_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientConfig, DatabaseKey, MessageId, MmsContext, SourceSequence, VaultId};
    use peppy_domain::MessageRecord;

    #[test]
    fn incoming_group_without_sender_replies_to_all_participants() {
        let directory = tempfile::TempDir::new().unwrap();
        let conversation_id = ConversationId::new();
        let client = Client::open(
            ClientConfig {
                database_path: directory.path().join("reply.db"),
                vault_id: VaultId::new(),
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[31; 32]).unwrap(),
        )
        .unwrap();
        let payload = crate::MessagePayload {
            record: MessageRecord {
                message_id: MessageId::new(),
                conversation_id,
                source_sequence: SourceSequence(1),
                attachments: vec![],
            },
            source_device_id: DeviceId::new(),
            provider_message_id: Some("provider-1".into()),
            sender_address: None,
            recipients: vec!["+15555550100".into(), "+15555550102".into()],
            subject: None,
            body: "Group body".into(),
            transport: Transport::Mms,
            direction: crate::Direction::Incoming,
            imported: false,
            sent_at_ms: Some(10),
            ordered_at_ms: None,
            mms_context: Some(MmsContext {
                source_generation: "generation-1".into(),
                subscription_id: "sim-1".into(),
                provider_thread_id: Some("thread-1".into()),
            }),
        };
        let store = client.lock().unwrap();
        store
            .conn
            .execute(
                "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen,sort_at_ms) VALUES(?,?,?,?,?,0,10)",
                params![
                    payload.record.message_id.to_string(),
                    conversation_id.to_string(),
                    payload.source_device_id.to_string(),
                    payload.record.source_sequence.0,
                    serde_json::to_string(&payload).unwrap(),
                ],
            )
            .unwrap();
        drop(store);

        assert_eq!(
            client
                .mms_reply_context(conversation_id)
                .unwrap()
                .recipients,
            vec!["+15555550100", "+15555550102"]
        );
    }
}
