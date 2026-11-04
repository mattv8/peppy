use crate::{Ctx, EnvelopePurpose, Error, PrivatePayload, enqueue, initial_replay};
use peppy_domain::DeviceId;
use rusqlite::{Connection, OptionalExtension, params};

const MAX_HINT_BYTES: usize = 512;

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS device_hints(device_id TEXT NOT NULL, platform TEXT NOT NULL, hint_tag TEXT NOT NULL, key_epoch INTEGER NOT NULL, PRIMARY KEY(device_id, key_epoch));
         CREATE TABLE IF NOT EXISTS device_successions(predecessor TEXT PRIMARY KEY, successor TEXT NOT NULL, platform TEXT NOT NULL, hint_tag TEXT NOT NULL, key_epoch INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS device_local_hint_tags(key_epoch INTEGER NOT NULL, platform TEXT NOT NULL, hint_tag TEXT NOT NULL, PRIMARY KEY(key_epoch, platform, hint_tag));
         CREATE TABLE IF NOT EXISTS pending_device_successions(predecessor TEXT NOT NULL, successor TEXT NOT NULL, platform TEXT NOT NULL, hint_tag TEXT NOT NULL, key_epoch INTEGER NOT NULL, producer TEXT NOT NULL, PRIMARY KEY(predecessor, successor, platform, hint_tag, key_epoch, producer));
         CREATE TABLE IF NOT EXISTS inherited_contact_books(book_id TEXT PRIMARY KEY, predecessor TEXT NOT NULL, takeover_complete INTEGER NOT NULL DEFAULT 0);",
    )?;
    let has_takeover_complete = conn
        .prepare("PRAGMA table_info(inherited_contact_books)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == "takeover_complete");
    if !has_takeover_complete {
        conn.execute(
            "ALTER TABLE inherited_contact_books ADD COLUMN takeover_complete INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    Ok(())
}

pub(crate) fn apply_hint(
    conn: &Connection,
    ctx: &Ctx,
    device_id: &DeviceId,
    platform: &str,
    hint_tag: &str,
    key_epoch: u32,
) -> Result<(), Error> {
    conn.execute(
        "INSERT OR IGNORE INTO device_hints(device_id,platform,hint_tag,key_epoch) VALUES(?,?,?,?)",
        params![device_id.to_string(), platform, hint_tag, key_epoch],
    )?;
    evaluate_succession(conn, ctx)?;
    apply_pending(conn)?;
    Ok(())
}

pub(crate) fn register_hint(
    conn: &Connection,
    ctx: &Ctx,
    platform: &str,
    hint: &[u8],
) -> Result<(), Error> {
    if !matches!(platform, "android" | "ios") || hint.is_empty() || hint.len() > MAX_HINT_BYTES {
        return Err(Error::InvalidRequest("invalid device hint"));
    }
    let active = ctx.active_epoch.ok_or(Error::KeysUnavailable)?;
    conn.execute("DELETE FROM metadata WHERE k='device_hint_unavailable'", [])?;
    let mut active_tag = None;
    for (epoch, keys) in ctx.keys {
        let Some(key) = keys.compaction.as_ref() else {
            continue;
        };
        let tag = hint_tag(key, platform, hint);
        conn.execute(
            "INSERT OR IGNORE INTO device_local_hint_tags(key_epoch,platform,hint_tag) VALUES(?,?,?)",
            params![epoch, platform, tag],
        )?;
        if *epoch == active {
            active_tag = Some(tag);
        }
    }
    let tag = active_tag.ok_or(Error::KeysUnavailable)?;
    let own = ctx.device_id.to_string();
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM device_hints WHERE device_id=? AND key_epoch=?",
            params![own, active],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        enqueue(
            conn,
            ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::DeviceHintRegistered {
                device_id: ctx.device_id,
                platform: platform.to_owned(),
                hint_tag: tag.clone(),
                key_epoch: active,
            },
        )?;
        apply_hint(conn, ctx, &ctx.device_id, platform, &tag, active)?;
    }
    evaluate_succession(conn, ctx)
}

/// Marks this installation as unable to provide a reinstall-stable hint without publishing one.
pub(crate) fn register_no_hint(conn: &Connection) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO metadata(k,v) VALUES('device_hint_unavailable','1') ON CONFLICT(k) DO UPDATE SET v='1'",
        [],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // Mirrors the authenticated succession wire payload.
pub(crate) fn apply_succession(
    conn: &Connection,
    producer: DeviceId,
    predecessor: DeviceId,
    successor: DeviceId,
    platform: &str,
    hint_tag: &str,
    key_epoch: u32,
) -> Result<bool, Error> {
    if producer != successor || predecessor == successor || !matches!(platform, "android" | "ios") {
        return Ok(false);
    }
    if !hints_match(
        conn,
        &predecessor,
        &successor,
        platform,
        hint_tag,
        key_epoch,
    )? {
        if can_be_pending(conn, &predecessor, &successor)? {
            conn.execute("INSERT OR IGNORE INTO pending_device_successions(predecessor,successor,platform,hint_tag,key_epoch,producer) VALUES(?,?,?,?,?,?)", params![predecessor.to_string(), successor.to_string(), platform, hint_tag, key_epoch, producer.to_string()])?;
        }
        return Ok(false);
    }
    accept(
        conn,
        &predecessor,
        &successor,
        platform,
        hint_tag,
        key_epoch,
    )
}

pub(crate) fn evaluate_succession(conn: &Connection, ctx: &Ctx) -> Result<(), Error> {
    if !initial_replay::caught_up(conn)? {
        return Ok(());
    }
    let own = ctx.device_id.to_string();
    if conn
        .query_row(
            "SELECT 1 FROM device_successions WHERE successor=? LIMIT 1",
            [&own],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        return Ok(());
    }
    // The largest rowid is the most recently projected journal hint; it breaks equal-tag ties.
    let candidate: Option<(String, String, String, i64)> = conn.query_row(
        "SELECT h.device_id,h.platform,h.hint_tag,h.key_epoch FROM device_hints h JOIN device_local_hint_tags l ON l.key_epoch=h.key_epoch AND l.platform=h.platform AND l.hint_tag=h.hint_tag WHERE h.device_id!=? AND NOT EXISTS(SELECT 1 FROM device_successions s WHERE s.predecessor=h.device_id) ORDER BY h.rowid DESC LIMIT 1",
        [&own], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).optional()?;
    let Some((predecessor, platform, hint_tag, epoch)) = candidate else {
        return Ok(());
    };
    let predecessor: DeviceId = predecessor.parse().map_err(|_| Error::Database)?;
    let epoch = u32::try_from(epoch).map_err(|_| Error::Database)?;
    let own_hint_exists: Option<i64> = conn.query_row(
        "SELECT 1 FROM device_hints WHERE device_id=? AND platform=? AND hint_tag=? AND key_epoch=?",
        params![own, platform, hint_tag, epoch],
        |row| row.get(0),
    ).optional()?;
    if own_hint_exists.is_none() {
        enqueue(
            conn,
            ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::DeviceHintRegistered {
                device_id: ctx.device_id,
                platform: platform.clone(),
                hint_tag: hint_tag.clone(),
                key_epoch: epoch,
            },
        )?;
        apply_hint(conn, ctx, &ctx.device_id, &platform, &hint_tag, epoch)?;
    }
    if apply_succession(
        conn,
        ctx.device_id,
        predecessor,
        ctx.device_id,
        &platform,
        &hint_tag,
        epoch,
    )? {
        enqueue(
            conn,
            ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::DeviceSucceeded {
                predecessor,
                successor: ctx.device_id,
                platform,
                hint_tag,
                key_epoch: epoch,
            },
        )?;
    }
    Ok(())
}

fn apply_pending(conn: &Connection) -> Result<(), Error> {
    let mut statement = conn.prepare("SELECT predecessor,successor,platform,hint_tag,key_epoch,producer FROM pending_device_successions ORDER BY rowid")?;
    let rows: Vec<(String, String, String, String, i64, String)> = statement
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    for (predecessor, successor, platform, tag, epoch, producer) in rows {
        let (Ok(predecessor), Ok(successor), Ok(producer), Ok(epoch)) = (
            predecessor.parse(),
            successor.parse(),
            producer.parse(),
            u32::try_from(epoch),
        ) else {
            continue;
        };
        if apply_succession(
            conn,
            producer,
            predecessor,
            successor,
            &platform,
            &tag,
            epoch,
        )? {
            conn.execute("DELETE FROM pending_device_successions WHERE predecessor=? AND successor=? AND platform=? AND hint_tag=? AND key_epoch=? AND producer=?", params![predecessor.to_string(), successor.to_string(), platform, tag, epoch, producer.to_string()])?;
        }
    }
    Ok(())
}

fn hints_match(
    conn: &Connection,
    predecessor: &DeviceId,
    successor: &DeviceId,
    platform: &str,
    tag: &str,
    epoch: u32,
) -> Result<bool, Error> {
    for device in [predecessor, successor] {
        let found: Option<i64> = conn.query_row("SELECT 1 FROM device_hints WHERE device_id=? AND platform=? AND hint_tag=? AND key_epoch=?", params![device.to_string(), platform, tag, epoch], |r| r.get(0)).optional()?;
        if found.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn accept(
    conn: &Connection,
    predecessor: &DeviceId,
    successor: &DeviceId,
    platform: &str,
    tag: &str,
    epoch: u32,
) -> Result<bool, Error> {
    if conn
        .query_row(
            "SELECT 1 FROM device_successions WHERE predecessor=? OR successor=? LIMIT 1",
            params![predecessor.to_string(), successor.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        return Ok(false);
    }
    let mut cursor = successor.to_string();
    while let Some(next) = conn
        .query_row(
            "SELECT successor FROM device_successions WHERE predecessor=?",
            [&cursor],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        if next == predecessor.to_string() {
            return Ok(false);
        }
        cursor = next;
    }
    conn.execute("INSERT OR IGNORE INTO device_successions(predecessor,successor,platform,hint_tag,key_epoch) VALUES(?,?,?,?,?)", params![predecessor.to_string(), successor.to_string(), platform, tag, epoch])?;
    if conn.changes() == 0 {
        return Ok(false);
    }
    let books: Vec<(String, String)> = conn
        .prepare("SELECT id,body FROM contact_books WHERE owner_device_id=?")?
        .query_map([predecessor.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<Result<_, _>>()?;
    for (book_id, body) in books {
        let mut body: serde_json::Value =
            serde_json::from_str(&body).map_err(|_| Error::Database)?;
        body["owner_device_id"] = serde_json::Value::String(successor.to_string());
        conn.execute(
            "INSERT OR IGNORE INTO inherited_contact_books(book_id,predecessor) VALUES(?,?)",
            params![book_id, predecessor.to_string()],
        )?;
        conn.execute(
            "UPDATE contact_books SET owner_device_id=?,body=? WHERE id=?",
            params![
                successor.to_string(),
                serde_json::to_string(&body).map_err(|_| Error::Database)?,
                book_id
            ],
        )?;
    }
    crate::requeue_successor_invalid_payload(conn, successor)?;
    Ok(true)
}

pub(crate) fn complete_takeover_scan(conn: &Connection, book_id: &str) -> Result<(), Error> {
    conn.execute(
        "UPDATE inherited_contact_books SET takeover_complete=1 WHERE book_id=?",
        [book_id],
    )?;
    Ok(())
}

fn can_be_pending(
    conn: &Connection,
    predecessor: &DeviceId,
    successor: &DeviceId,
) -> Result<bool, Error> {
    for device in [predecessor, successor] {
        if conn
            .query_row(
                "SELECT 1 FROM device_hints WHERE device_id=? LIMIT 1",
                [device.to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_none()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn local_hint_registered(conn: &Connection) -> Result<bool, Error> {
    let registered = conn
        .query_row("SELECT 1 FROM device_local_hint_tags LIMIT 1", [], |_| {
            Ok(())
        })
        .optional()?
        .is_some();
    Ok(registered
        || conn
            .query_row(
                "SELECT 1 FROM metadata WHERE k='device_hint_unavailable' AND v='1'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
}

pub(crate) fn inherited_books(conn: &Connection, owner: &DeviceId) -> Result<Vec<String>, Error> {
    let mut statement = conn.prepare("SELECT b.id FROM contact_books b JOIN inherited_contact_books i ON i.book_id=b.id WHERE b.owner_device_id=? AND b.forgotten=0 AND b.state!='retired' ORDER BY i.takeover_complete,b.id")?;
    Ok(statement
        .query_map([owner.to_string()], |r| r.get(0))?
        .collect::<Result<_, _>>()?)
}

pub(crate) fn takeover_complete(conn: &Connection, book_id: &str) -> Result<bool, Error> {
    Ok(conn
        .query_row(
            "SELECT takeover_complete FROM inherited_contact_books WHERE book_id=?",
            [book_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .is_some_and(|complete| complete != 0))
}

fn hint_tag(key: &peppy_crypto::PurposeKey, platform: &str, hint: &[u8]) -> String {
    let tag = peppy_crypto::compaction_hmac(key, b"device-hint-v1", &[platform.as_bytes(), hint]);
    tag.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_hint_unblocks_local_selection_gate() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE metadata(k TEXT PRIMARY KEY, v TEXT NOT NULL);")
            .unwrap();
        initialize(&conn).unwrap();
        assert!(!local_hint_registered(&conn).unwrap());
        register_no_hint(&conn).unwrap();
        assert!(local_hint_registered(&conn).unwrap());
    }
}
