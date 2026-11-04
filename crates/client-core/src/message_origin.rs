use crate::{
    ConversationId, Direction, Error, MessagePayload, SourceSequence, Transport, get_meta, set_meta,
};
use peppy_domain::MessageId;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

const BACKFILL_BATCH: usize = 128;
const BACKFILL_ROW_BUDGET: usize = 1_024;
const BACKFILL_CURSOR_META: &str = "message_origin_backfill_cursor";
const BACKFILL_SKIPPED_META: &str = "message_origin_backfill_skipped";

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS message_origins(message_id TEXT PRIMARY KEY, coarse_key TEXT NOT NULL, sent_at_ms INTEGER, claimed INTEGER NOT NULL DEFAULT 0);
         CREATE INDEX IF NOT EXISTS message_origins_coarse_time ON message_origins(coarse_key, sent_at_ms);
         CREATE TABLE IF NOT EXISTS message_origin_aliases(origin_key TEXT PRIMARY KEY, message_id TEXT NOT NULL);",
    )?;
    Ok(())
}

pub(crate) fn apply_alias(
    conn: &Connection,
    message_id: &MessageId,
    origin_key: &str,
) -> Result<(), Error> {
    conn.execute(
        "INSERT OR IGNORE INTO message_origin_aliases(origin_key,message_id) VALUES(?,?)",
        params![origin_key, message_id.to_string()],
    )?;
    Ok(())
}

/// This deliberately uses the region-free canonicalizer: origin identity must not depend on a
/// device's configured identity region. National-format numbers may therefore miss across stores.
fn address(value: &str) -> String {
    crate::conversation_match::canonical_address(value, None)
}

fn framed(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

pub(crate) fn coarse_key(
    conn: &Connection,
    payload: &MessagePayload,
) -> Result<Option<String>, Error> {
    let transport = match payload.transport {
        Transport::Sms => b"sms".as_slice(),
        Transport::Mms => b"mms".as_slice(),
        Transport::Rcs => return Ok(None),
    };
    let mut counterparts = match payload.direction {
        Direction::Incoming => payload
            .sender_address
            .iter()
            .map(|v| address(v))
            .collect::<Vec<_>>(),
        Direction::Outgoing => payload
            .recipients
            .iter()
            .map(|v| address(v))
            .collect::<Vec<_>>(),
    };
    counterparts.sort();
    counterparts.dedup();
    let mut hasher = Sha256::new();
    framed(&mut hasher, b"peppy-origin-v1");
    framed(&mut hasher, transport);
    framed(
        &mut hasher,
        match payload.direction {
            Direction::Incoming => b"incoming",
            Direction::Outgoing => b"outgoing",
        },
    );
    for counterpart in counterparts {
        framed(&mut hasher, counterpart.as_bytes());
    }
    framed(&mut hasher, &Sha256::digest(payload.body.as_bytes()));
    if payload.transport == Transport::Mms {
        framed(
            &mut hasher,
            payload.subject.as_deref().unwrap_or("").as_bytes(),
        );
        framed(
            &mut hasher,
            &(payload.record.attachments.len() as u64).to_be_bytes(),
        );
        for attachment_id in &payload.record.attachments {
            let part: Option<(String, i64)> = conn
                .query_row(
                    "SELECT media_type,plaintext_bytes FROM attachments WHERE attachment_id=?",
                    params![attachment_id.attachment_id.to_string()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((media_type, bytes)) = part else {
                return Ok(None);
            };
            framed(&mut hasher, media_type.as_bytes());
            framed(&mut hasher, &bytes.to_be_bytes());
        }
    }
    Ok(Some(crate::media::hex(&hasher.finalize())))
}

pub(crate) fn exact_evidence(
    conn: &Connection,
    payload: &MessagePayload,
) -> Result<Option<Vec<u8>>, Error> {
    let Some(coarse_key) = coarse_key(conn, payload)? else {
        return Ok(None);
    };
    Ok(exact_evidence_for(&coarse_key, payload.sent_at_ms))
}

fn exact_evidence_for(coarse_key: &str, sent_at_ms: Option<i64>) -> Option<Vec<u8>> {
    let sent_at_ms = sent_at_ms.filter(|time| *time > 0)?;
    let mut evidence = Vec::with_capacity(72);
    evidence.extend_from_slice(&Sha256::digest(coarse_key.as_bytes()));
    evidence.extend_from_slice(&sent_at_ms.to_be_bytes());
    Some(evidence)
}

pub(crate) fn exact_key(
    conn: &Connection,
    payload: &MessagePayload,
) -> Result<Option<String>, Error> {
    Ok(exact_evidence(conn, payload)?.map(|evidence| crate::media::hex(&evidence)))
}

pub(crate) fn decode_hex(value: &str) -> Result<Vec<u8>, Error> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::Database);
    }
    (0..value.len() / 2)
        .map(|index| {
            u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).map_err(|_| Error::Database)
        })
        .collect()
}

pub(crate) fn decode_hex_32(value: &str) -> Result<[u8; 32], Error> {
    let bytes = decode_hex(value)?;
    bytes.try_into().map_err(|_| Error::Database)
}

pub(crate) fn index_message(conn: &Connection, payload: &MessagePayload) -> Result<(), Error> {
    let Some(coarse_key) = coarse_key(conn, payload)? else {
        return Ok(());
    };
    conn.execute(
        "INSERT OR IGNORE INTO message_origins(message_id,coarse_key,sent_at_ms) VALUES(?,?,?)",
        params![
            payload.record.message_id.to_string(),
            coarse_key,
            payload.sent_at_ms.filter(|time| *time > 0)
        ],
    )?;
    Ok(())
}

pub(crate) fn mark_claimed(conn: &Connection, message_id: &MessageId) -> Result<(), Error> {
    conn.execute(
        "UPDATE message_origins SET claimed=1 WHERE message_id=?",
        params![message_id.to_string()],
    )?;
    Ok(())
}

pub(crate) fn message_identity(
    conn: &Connection,
    message_id: &MessageId,
) -> Result<(ConversationId, SourceSequence), Error> {
    conn.query_row(
        "SELECT conversation_id,source_sequence FROM messages WHERE id=?",
        params![message_id.to_string()],
        |row| {
            let sequence: i64 = row.get(1)?;
            Ok((
                crate::parse(&row.get::<_, String>(0)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                SourceSequence(u64::try_from(sequence).map_err(|_| rusqlite::Error::InvalidQuery)?),
            ))
        },
    )
    .map_err(Into::into)
}

pub(crate) fn claim(
    conn: &Connection,
    payload: &MessagePayload,
    sms_window: bool,
) -> Result<Option<MessageId>, Error> {
    let Some(coarse) = coarse_key(conn, payload)? else {
        return Ok(None);
    };
    let Some(exact) = exact_evidence_for(&coarse, payload.sent_at_ms)
        .map(|evidence| crate::media::hex(&evidence))
    else {
        return Ok(None);
    };
    if let Some(id) = conn
        .query_row(
            "SELECT a.message_id FROM message_origin_aliases a JOIN messages m ON m.id=a.message_id WHERE a.origin_key=?",
            params![exact],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(Some(crate::parse(&id)?));
    }
    let time = payload
        .sent_at_ms
        .filter(|time| *time > 0)
        .unwrap_or_default();
    let sql = if sms_window {
        "SELECT o.message_id FROM message_origins o WHERE o.coarse_key=? AND o.claimed=0 AND o.sent_at_ms IS NOT NULL AND ABS(o.sent_at_ms-?)<=? AND NOT EXISTS(SELECT 1 FROM sms_acquisitions a WHERE a.message_id=o.message_id AND a.state='complete') AND NOT EXISTS(SELECT 1 FROM mms_acquisitions a WHERE a.message_id=o.message_id AND a.state='complete') ORDER BY o.sent_at_ms<>?, ABS(o.sent_at_ms-?), o.sent_at_ms, o.message_id LIMIT 1"
    } else {
        "SELECT o.message_id FROM message_origins o WHERE o.coarse_key=? AND o.claimed=0 AND o.sent_at_ms=? AND NOT EXISTS(SELECT 1 FROM sms_acquisitions a WHERE a.message_id=o.message_id AND a.state='complete') AND NOT EXISTS(SELECT 1 FROM mms_acquisitions a WHERE a.message_id=o.message_id AND a.state='complete') LIMIT 1"
    };
    let id: Option<String> = if sms_window {
        conn.query_row(
            sql,
            params![
                coarse,
                time,
                crate::sms_reconciliation::PEPPY_CLOCK_SKEW_MS,
                time,
                time
            ],
            |r| r.get(0),
        )
        .optional()?
    } else {
        conn.query_row(sql, params![coarse, time], |r| r.get(0))
            .optional()?
    };
    if let Some(id) = id {
        conn.execute(
            "UPDATE message_origins SET claimed=1 WHERE message_id=?",
            params![id],
        )?;
        return Ok(Some(crate::parse(&id)?));
    }
    Ok(None)
}

pub(crate) fn backfill_step(conn: &Connection) -> Result<(), Error> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='message_origins')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(());
    }
    if get_meta(conn, "message_origin_backfill_complete")?.as_deref() == Some("1") {
        return Ok(());
    }
    let mut cursor = get_meta(conn, BACKFILL_CURSOR_META)?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or_default();
    let mut processed = 0;
    while processed < BACKFILL_ROW_BUDGET {
        let rows = {
            let mut statement = conn.prepare(
                "SELECT local_order,payload FROM messages WHERE local_order>? ORDER BY local_order LIMIT ?",
            )?;
            statement
                .query_map(params![cursor, BACKFILL_BATCH as i64], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        if rows.is_empty() {
            set_meta(conn, "message_origin_backfill_complete", "1")?;
            break;
        }
        for (local_order, payload) in &rows {
            cursor = *local_order;
            match crate::decode(payload.as_bytes()) {
                Ok(payload) => index_message(conn, &payload)?,
                Err(_) => {
                    let skipped = get_meta(conn, BACKFILL_SKIPPED_META)?
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or_default()
                        + 1;
                    set_meta(conn, BACKFILL_SKIPPED_META, &skipped.to_string())?;
                }
            }
        }
        set_meta(conn, BACKFILL_CURSOR_META, &cursor.to_string())?;
        processed += rows.len();
        if rows.len() < BACKFILL_BATCH {
            set_meta(conn, "message_origin_backfill_complete", "1")?;
            break;
        }
    }
    Ok(())
}
