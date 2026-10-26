use crate::{ConversationId, Direction, Error, MessagePayload, Transport};
use phonenumber::{Mode, country, parse};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

pub(crate) fn canonical_address(address: &str, region: Option<&str>) -> String {
    let trimmed = address.trim();
    let parsed = if trimmed.starts_with('+') {
        parse(None, trimmed).ok()
    } else {
        region
            .and_then(|region| country::Id::from_str(region).ok())
            .and_then(|country| parse(Some(country), trimmed).ok())
    };
    parsed
        .filter(|number| number.is_valid())
        .map(|number| number.format().mode(Mode::E164).to_string())
        .unwrap_or_else(|| trimmed.to_owned())
}

pub(crate) fn identity_region(conn: &Connection) -> Result<String, Error> {
    let mut statement = conn.prepare("SELECT body FROM contact_books WHERE forgotten=0")?;
    let regions = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(|body| crate::contact_resolution::region(&body))
        .collect::<BTreeSet<_>>();
    Ok(if regions.len() == 1 {
        regions.into_iter().next().unwrap_or_else(|| "US".into())
    } else {
        "US".into()
    })
}

pub(crate) fn recipient_set(addresses: &[String], region: Option<&str>) -> BTreeSet<String> {
    addresses
        .iter()
        .map(|address| canonical_address(address, region))
        .collect()
}

pub(crate) type OwnAddressCache = BTreeMap<(String, String), Option<String>>;

pub(crate) fn message_recipient_set(
    conn: &Connection,
    payload: &MessagePayload,
    region: Option<&str>,
    own_addresses: &mut OwnAddressCache,
) -> Result<Option<BTreeSet<String>>, Error> {
    if payload.transport == Transport::Mms {
        let identity = payload
            .mms_context
            .as_ref()
            .map(|context| (&payload.source_device_id, context.subscription_id.as_str()));
        return mms_participant_set(
            conn,
            payload.direction,
            payload.sender_address.as_deref(),
            &payload.recipients,
            identity,
            region,
            own_addresses,
        );
    }
    let mut participants = payload.recipients.clone();
    if payload.direction == Direction::Incoming {
        participants.extend(payload.sender_address.clone());
    }
    Ok(Some(recipient_set(&participants, region)))
}

pub(crate) fn mms_participant_set(
    conn: &Connection,
    direction: Direction,
    sender: Option<&str>,
    recipients: &[String],
    identity: Option<(&crate::DeviceId, &str)>,
    region: Option<&str>,
    own_addresses: &mut OwnAddressCache,
) -> Result<Option<BTreeSet<String>>, Error> {
    let mut participants = recipients.to_vec();
    if direction == Direction::Incoming {
        if let Some(sender) = sender {
            participants.push(sender.to_owned());
        }
        if recipient_set(&participants, region).len() > 1 {
            let Some((device_id, subscription_id)) = identity else {
                return Ok(None);
            };
            let Some(own) = cached_own_address(conn, device_id, subscription_id, own_addresses)?
            else {
                return Ok(None);
            };
            if !participants
                .iter()
                .any(|address| confirmed_own_address(&own, address, region))
            {
                return Ok(None);
            }
            participants.retain(|address| !confirmed_own_address(&own, address, region));
        }
    }
    let participants = recipient_set(&participants, region);
    Ok((!participants.is_empty()).then_some(participants))
}

pub(crate) fn confirmed_own_address(own: &str, participant: &str, region: Option<&str>) -> bool {
    canonical_address(own, region) == canonical_address(participant, region)
        || crate::mms_identity::same_own_address(own, participant)
}

fn cached_own_address(
    conn: &Connection,
    device_id: &crate::DeviceId,
    subscription_id: &str,
    own_addresses: &mut OwnAddressCache,
) -> Result<Option<String>, Error> {
    let key = (device_id.to_string(), subscription_id.to_owned());
    if let Some(own) = own_addresses.get(&key) {
        return Ok(own.clone());
    }
    let own = conn
        .query_row(
            "SELECT address FROM mms_own_addresses WHERE source_device_id=? AND subscription_id=?",
            params![key.0, key.1],
            |row| row.get(0),
        )
        .optional()?;
    own_addresses.insert(key, own.clone());
    Ok(own)
}

pub(crate) fn conversation_for_recipients(
    conn: &Connection,
    recipients: &[String],
) -> Result<Option<ConversationId>, Error> {
    let region = identity_region(conn)?;
    let expected = recipient_set(recipients, Some(&region));
    if expected.is_empty() {
        return Ok(None);
    }
    let mut own_addresses = OwnAddressCache::new();
    let mut statement = conn.prepare(
        "SELECT conversation_id,payload FROM (SELECT conversation_id,payload,sort_at_ms,local_order,ROW_NUMBER() OVER (PARTITION BY conversation_id ORDER BY sort_at_ms DESC,local_order DESC) AS newest FROM messages) WHERE newest=1 ORDER BY sort_at_ms DESC,local_order DESC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (conversation_id, payload) = row?;
        let payload: MessagePayload = crate::decode(payload.as_bytes())?;
        let Some(participants) =
            message_recipient_set(conn, &payload, Some(&region), &mut own_addresses)?
        else {
            continue;
        };
        if participants == expected {
            return crate::parse(&conversation_id).map(Some);
        }
    }
    Ok(None)
}

pub(crate) fn address_conversation(
    conn: &Connection,
    address: &str,
) -> Result<Option<ConversationId>, Error> {
    let region = identity_region(conn)?;
    for candidate in [
        address.to_owned(),
        canonical_address(address, Some(&region)),
    ] {
        let conversation: Option<String> = conn
            .query_row(
                "SELECT conversation_id FROM addresses WHERE address=?",
                params![candidate],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(conversation) = conversation {
            return crate::parse(&conversation).map(Some);
        }
    }
    Ok(None)
}
