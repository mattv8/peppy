//! Durable encrypted requests for an owner phone to perform a fresh full contact scan.
use crate::{Ctx, Error, PrivatePayload, enqueue};
use peppy_protocol::EnvelopePurpose;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const TTL_SECONDS: i64 = 7 * 24 * 60 * 60;
const EXPIRY_SKEW_SECONDS: i64 = 5 * 60;
const RETENTION_SECONDS: i64 = 24 * 60 * 60;
const MAX_ACTIVE_PER_BOOK_REQUESTER: i64 = 8;
const MAX_ORPHAN_RESULTS: i64 = 256;
const TERMINAL_STATES: &[&str] = &["completed", "blocked", "failed", "expired"];
const REASONS: &[&str] = &[
    "permission_denied",
    "limited_access",
    "sync_disabled",
    "source_revoked",
    "source_retired",
    "unsupported",
    "needs_unlock",
    "stale_epoch",
    "superseded_generation",
    "incomplete_scan",
    "cancelled",
    "expired",
];

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn invalid() -> Error {
    Error::InvalidRequest("invalid contact scan request")
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty() && v.len() <= 1024)
        .ok_or_else(invalid)
}
fn revision(value: &Value, key: &str) -> Result<i64, Error> {
    crate::contacts::revision(value, key)
}
fn encoded(value: &Value) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|_| Error::Database)
}

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS contact_scan_requests(request_id TEXT PRIMARY KEY, book_id TEXT NOT NULL, requester TEXT NOT NULL, target_owner TEXT NOT NULL, generation INTEGER NOT NULL, key_epoch INTEGER NOT NULL, body TEXT NOT NULL, status TEXT NOT NULL, reason TEXT, expires_at INTEGER NOT NULL, scan_id TEXT, historical INTEGER NOT NULL DEFAULT 0, envelope_id TEXT); CREATE TABLE IF NOT EXISTS contact_scan_orphan_results(request_id TEXT PRIMARY KEY, producer TEXT NOT NULL, key_epoch INTEGER NOT NULL, body TEXT NOT NULL, created_at INTEGER NOT NULL DEFAULT 0);")?;
    let has_orphan_created_at: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('contact_scan_orphan_results') WHERE name='created_at')", [], |row| row.get(0))?;
    if !has_orphan_created_at {
        conn.execute("ALTER TABLE contact_scan_orphan_results ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0", [])?;
    }
    let has_envelope_id: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('contact_scan_requests') WHERE name='envelope_id')", [], |row| row.get(0))?;
    if !has_envelope_id {
        conn.execute(
            "ALTER TABLE contact_scan_requests ADD COLUMN envelope_id TEXT",
            [],
        )?;
    }
    Ok(())
}

fn book(conn: &Connection, id: &str) -> Result<(String, i64, Value), Error> {
    let (owner, generation, body): (String, i64, String) = conn
        .query_row(
            "SELECT owner_device_id,generation,body FROM contact_books WHERE id=? AND forgotten=0",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    Ok((
        owner,
        generation,
        serde_json::from_str(&body).map_err(|_| Error::Database)?,
    ))
}
fn accepts_scan_requests(body: &Value) -> bool {
    body.get("state").and_then(Value::as_str) == Some("active")
        && body
            .pointer("/capabilities/scan_request")
            .and_then(Value::as_bool)
            == Some(true)
}
fn expire(conn: &Connection, requester: &str) -> Result<(), Error> {
    conn.execute(
        "UPDATE contact_scan_requests SET status='expired',reason='expired' WHERE requester=? AND status IN ('queued','waiting','scanning') AND expires_at<?",
        params![requester, now()],
    )?;
    Ok(())
}

fn canonical_id<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    let id = text(value, key)?;
    (uuid::Uuid::parse_str(id)
        .ok()
        .as_ref()
        .is_some_and(|parsed| parsed.to_string() == id))
    .then_some(id)
    .ok_or_else(invalid)
}

fn schema(value: &Value) -> Result<(), Error> {
    (value.get("schema_version").and_then(Value::as_i64) == Some(1))
        .then_some(())
        .ok_or_else(invalid)
}

fn valid_reason(state: &str, reason: Option<&str>) -> bool {
    match state {
        "completed" => reason.is_none(),
        "blocked" | "failed" | "expired" => reason.is_some_and(|reason| REASONS.contains(&reason)),
        "waiting" | "scanning" | "queued" => reason.is_none(),
        _ => false,
    }
}

fn publish_terminal(
    conn: &Connection,
    ctx: &Ctx<'_>,
    request_id: &str,
    book_id: &str,
    generation: i64,
    request_epoch: u32,
    state: &str,
    reason: &str,
) -> Result<(), Error> {
    let changed = conn.execute(
        "UPDATE contact_scan_requests SET status=?,reason=? WHERE request_id=? AND status NOT IN ('completed','blocked','failed','expired')",
        params![state, reason, request_id],
    )?;
    if changed == 0 {
        return Ok(());
    }
    enqueue(
        conn,
        ctx,
        EnvelopePurpose::Event,
        None,
        None,
        &PrivatePayload::ContactScanResult {
            result: json!({"schema_version":1,"request_id":request_id,"book_id":book_id,"target_owner":ctx.device_id.to_string(),"generation":generation.to_string(),"key_epoch":request_epoch,"state":state,"reason":reason}),
        },
    )?;
    Ok(())
}

fn apply_result(
    conn: &Connection,
    request_id: &str,
    owner: &str,
    book_id: &str,
    generation: i64,
    request_epoch: u32,
    producer: &str,
    envelope_epoch: u32,
    result: &Value,
) -> Result<bool, Error> {
    if owner != producer
        || result.get("target_owner").and_then(Value::as_str) != Some(owner)
        || result.get("key_epoch").and_then(Value::as_u64) != Some(request_epoch as u64)
        || result.get("book_id").and_then(Value::as_str) != Some(book_id)
        || revision(result, "generation")? != generation
        || envelope_epoch < request_epoch
    {
        return Ok(false);
    }
    let next = text(result, "state")?;
    if !valid_reason(next, result.get("reason").and_then(Value::as_str)) {
        return Ok(false);
    }
    let current: (String, Option<String>) = conn.query_row(
        "SELECT status,reason FROM contact_scan_requests WHERE request_id=?",
        [request_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let placeholder = current.0 == "expired" && current.1.as_deref() == Some("historical");
    let advances = placeholder
        || (!TERMINAL_STATES.contains(&current.0.as_str())
            && match next {
                "waiting" => current.0 == "queued" || current.0 == "waiting",
                "scanning" => matches!(current.0.as_str(), "queued" | "waiting" | "scanning"),
                terminal => TERMINAL_STATES.contains(&terminal),
            });
    if advances {
        conn.execute(
            "UPDATE contact_scan_requests SET status=?,reason=? WHERE request_id=?",
            params![
                next,
                result.get("reason").and_then(Value::as_str),
                request_id
            ],
        )?;
    }
    Ok(true)
}

fn prune(conn: &Connection) -> Result<(), Error> {
    let cutoff = now() - RETENTION_SECONDS;
    conn.execute("DELETE FROM contact_scan_requests WHERE status IN ('completed','blocked','failed','expired') AND expires_at<? AND request_id NOT IN (SELECT request_id FROM (SELECT request_id, ROW_NUMBER() OVER (PARTITION BY book_id,requester ORDER BY expires_at DESC,rowid DESC) AS row_number FROM contact_scan_requests) WHERE row_number=1)", [cutoff])?;
    Ok(())
}

pub(crate) fn request(conn: &mut Connection, ctx: &Ctx<'_>, input: &str) -> Result<String, Error> {
    crate::contacts::require_unlocked(ctx)?;
    let input: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    let book_id = text(&input, "book_id")?.to_owned();
    let (owner, generation, body) = book(conn, &book_id)?;
    if owner == ctx.device_id.to_string() || !accepts_scan_requests(&body) {
        return Err(invalid());
    }
    let tx = conn.transaction()?;
    expire(&tx, &ctx.device_id.to_string())?;
    prune(&tx)?;
    let existing: Option<(String, String, i64, Option<String>)> = tx
        .query_row(
            "SELECT request_id,status,expires_at,reason FROM contact_scan_requests WHERE book_id=? AND requester=? AND generation=? AND key_epoch=? AND historical=0 AND status IN ('queued','waiting','scanning') ORDER BY expires_at DESC,rowid DESC LIMIT 1",
            params![book_id, ctx.device_id.to_string(), generation, ctx.active_epoch],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if let Some((request_id, status, expires_at, reason)) = existing {
        tx.commit()?;
        return Ok(json!({"requestId":request_id,"bookId":book_id,"state":status,"reason":reason,"expiresAt":expires_at,"duplicate":true}).to_string());
    }
    let active_count: i64 = tx.query_row("SELECT COUNT(*) FROM contact_scan_requests WHERE book_id=? AND requester=? AND status IN ('queued','waiting','scanning')", params![book_id,ctx.device_id.to_string()], |row| row.get(0))?;
    if active_count >= MAX_ACTIVE_PER_BOOK_REQUESTER {
        return Err(invalid());
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    let expires_at = now() + TTL_SECONDS;
    let request = json!({"schema_version":1,"request_id":request_id,"book_id":book_id,"target_owner":owner,"generation":generation.to_string(),"requester":ctx.device_id.to_string(),"key_epoch":ctx.active_epoch,"expires_at":expires_at});
    tx.execute(
        "INSERT INTO contact_scan_requests(request_id,book_id,requester,target_owner,generation,key_epoch,body,status,expires_at) VALUES(?,?,?,?,?,?,?,'queued',?)",
        params![
            request_id,
            book_id,
            ctx.device_id.to_string(),
            owner,
            generation,
            ctx.active_epoch,
            encoded(&request)?,
            expires_at
        ],
    )?;
    let (envelope_id, _) = enqueue(
        &tx,
        ctx,
        EnvelopePurpose::Event,
        None,
        None,
        &PrivatePayload::ContactScanRequest { request },
    )?;
    tx.execute(
        "UPDATE contact_scan_requests SET envelope_id=? WHERE request_id=?",
        params![envelope_id.to_string(), request_id],
    )?;
    tx.commit()?;
    Ok(
        json!({"requestId":request_id,"bookId":book_id,"state":"queued","expiresAt":expires_at})
            .to_string(),
    )
}

pub(crate) fn status(conn: &Connection, requester: &str, input: &str) -> Result<String, Error> {
    let input: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    let book_id = text(&input, "book_id")?;
    let timestamp = now();
    let row: Option<(String,String,Option<String>,i64)> = conn.query_row("SELECT request_id, CASE WHEN status IN ('queued','waiting','scanning') AND expires_at<?3 THEN 'expired' WHEN status='queued' AND envelope_id IS NOT NULL AND EXISTS(SELECT 1 FROM outbox WHERE envelope_id=contact_scan_requests.envelope_id AND state='acknowledged') THEN 'waiting' ELSE status END, CASE WHEN status IN ('queued','waiting','scanning') AND expires_at<?3 THEN 'expired' ELSE reason END, expires_at FROM contact_scan_requests WHERE book_id=?1 AND requester=?2 ORDER BY expires_at DESC,rowid DESC LIMIT 1", params![book_id, requester, timestamp], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    Ok(row.map(|(request_id,state,reason,expires_at)| json!({"requestId":request_id,"bookId":book_id,"state":state,"reason":reason,"expiresAt":expires_at})).unwrap_or(Value::Null).to_string())
}

pub(crate) fn pending(conn: &mut Connection, ctx: &Ctx<'_>, input: &str) -> Result<String, Error> {
    let input: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    let book_filter = input.get("book_id").and_then(Value::as_str);
    let active_epoch = ctx.active_epoch.ok_or(Error::KeysUnavailable)?;
    let owner = ctx.device_id.to_string();
    let tx = conn.transaction()?;
    let candidates = tx.prepare(
        "SELECT r.request_id,r.book_id,r.generation,r.key_epoch,r.expires_at,b.generation,b.forgotten,b.body
         FROM contact_scan_requests r LEFT JOIN contact_books b ON b.id=r.book_id
         WHERE r.target_owner=? AND r.historical=0 AND r.status IN ('waiting','scanning')",
    )?.query_map([&owner], |row| {
        Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,i64>(2)?,
            row.get::<_,u32>(3)?, row.get::<_,i64>(4)?, row.get::<_,Option<i64>>(5)?,
            row.get::<_,Option<bool>>(6)?, row.get::<_,Option<String>>(7)?))
    })?.collect::<Result<Vec<_>,_>>()?;
    for (id, book_id, generation, request_epoch, expires_at, live_generation, forgotten, body) in
        candidates
    {
        let parsed = body
            .as_deref()
            .map(serde_json::from_str::<Value>)
            .transpose()
            .map_err(|_| Error::Database)?;
        let reason = if expires_at < now() {
            Some("expired")
        } else if request_epoch != active_epoch {
            Some("stale_epoch")
        } else if forgotten == Some(true) || live_generation.is_none() {
            Some("source_revoked")
        } else if live_generation != Some(generation) {
            Some("superseded_generation")
        } else if parsed
            .as_ref()
            .and_then(|v| v.get("state"))
            .and_then(Value::as_str)
            == Some("retired")
        {
            Some("source_retired")
        } else if parsed.as_ref().is_none_or(|v| !accepts_scan_requests(v)) {
            Some("unsupported")
        } else {
            None
        };
        if let Some(reason) = reason {
            let state = if reason == "expired" {
                "expired"
            } else {
                "blocked"
            };
            publish_terminal(
                &tx,
                ctx,
                &id,
                &book_id,
                generation,
                request_epoch,
                state,
                reason,
            )?;
        }
    }
    let rows = tx
        .prepare(
            "SELECT r.request_id,r.book_id,r.generation,r.expires_at,r.status,r.scan_id
         FROM contact_scan_requests r
         WHERE r.target_owner=?1 AND r.key_epoch=?2 AND r.historical=0
           AND (?3 IS NULL OR r.book_id=?3)
           AND (r.status='waiting' OR (r.status='scanning' AND EXISTS(
               SELECT 1 FROM contact_scans s WHERE s.scan_id=r.scan_id AND s.complete=0)))
         ORDER BY r.expires_at,r.rowid",
        )?
        .query_map(params![owner, active_epoch, book_filter], |row| {
            Ok(
                json!({"requestId":row.get::<_,String>(0)?,"bookId":row.get::<_,String>(1)?,
            "generation":row.get::<_,i64>(2)?.to_string(),"expiresAt":row.get::<_,i64>(3)?,
            "state":row.get::<_,String>(4)?,"scanId":row.get::<_,Option<String>>(5)?}),
            )
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let result = json!({"requests":rows}).to_string();
    tx.commit()?;
    Ok(result)
}

pub(crate) fn report_blocked(
    conn: &mut Connection,
    ctx: &Ctx<'_>,
    input: &str,
) -> Result<String, Error> {
    crate::contacts::require_unlocked(ctx)?;
    let input: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    let book_id = text(&input, "book_id")?;
    let reason = text(&input, "reason")?;
    if !REASONS.contains(&reason) {
        return Err(invalid());
    }
    let tx = conn.transaction()?;
    let (_, generation) = crate::contacts::owned_book(&tx, ctx, book_id)?;
    let ids = tx.prepare("SELECT request_id,key_epoch FROM contact_scan_requests WHERE book_id=? AND target_owner=? AND generation=? AND key_epoch=? AND status IN ('waiting','blocked')")?.query_map(params![book_id,ctx.device_id.to_string(),generation,ctx.active_epoch], |r| Ok((r.get::<_,String>(0)?,r.get::<_,u32>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
    for (request_id, epoch) in &ids {
        publish_terminal(
            &tx, ctx, request_id, book_id, generation, *epoch, "blocked", reason,
        )?;
    }
    tx.commit()?;
    Ok(json!({"bookId":book_id,"state":"blocked","reason":reason,"count":ids.len()}).to_string())
}

pub(crate) fn set_capability(
    conn: &mut Connection,
    ctx: &Ctx<'_>,
    input: &str,
) -> Result<String, Error> {
    crate::contacts::require_unlocked(ctx)?;
    let input: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    let book_id = text(&input, "book_id")?;
    let enabled = input
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(invalid)?;
    let tx = conn.transaction()?;
    let (mut body, _) = crate::contacts::owned_book(&tx, ctx, book_id)?;
    body["capabilities"]["scan_request"] = json!(enabled);
    tx.execute(
        "UPDATE contact_books SET body=? WHERE id=?",
        params![encoded(&body)?, book_id],
    )?;
    if !enabled {
        let generation = crate::contacts::owned_book(&tx, ctx, book_id)?.1;
        let requests = tx.prepare("SELECT request_id,key_epoch FROM contact_scan_requests WHERE book_id=? AND target_owner=? AND generation=? AND status IN ('waiting','scanning')")?.query_map(params![book_id,ctx.device_id.to_string(),generation], |row| Ok((row.get::<_,String>(0)?, row.get::<_,u32>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
        for (request_id, request_epoch) in requests {
            publish_terminal(
                &tx,
                ctx,
                &request_id,
                book_id,
                generation,
                request_epoch,
                "blocked",
                "sync_disabled",
            )?;
        }
    }
    crate::contacts::publish_book_state(&tx, ctx, book_id)?;
    tx.commit()?;
    Ok(json!({"bookId":book_id,"canSync":enabled}).to_string())
}

pub(crate) fn on_scan_started(
    conn: &Connection,
    ctx: &Ctx<'_>,
    scan_id: &str,
    book_id: &str,
    generation: i64,
    full: bool,
) -> Result<(), Error> {
    let (body, live_generation) = crate::contacts::owned_book(conn, ctx, book_id)?;
    let active_epoch = ctx.active_epoch.ok_or(Error::KeysUnavailable)?;
    let waiting = conn.prepare(
        "SELECT request_id,generation,key_epoch,expires_at FROM contact_scan_requests WHERE book_id=? AND target_owner=? AND historical=0 AND status IN ('waiting','scanning')",
    )?.query_map(params![book_id,ctx.device_id.to_string()], |row| {
        Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,u32>(2)?,row.get::<_,i64>(3)?))
    })?.collect::<Result<Vec<_>,_>>()?;
    for (request_id, request_generation, request_epoch, expires_at) in waiting {
        let reason = if expires_at < now() {
            Some("expired")
        } else if request_epoch != active_epoch {
            Some("stale_epoch")
        } else if request_generation != live_generation {
            Some("superseded_generation")
        } else if body.get("state").and_then(Value::as_str) == Some("retired") {
            Some("source_retired")
        } else if !accepts_scan_requests(&body) {
            Some("unsupported")
        } else {
            None
        };
        if let Some(reason) = reason {
            publish_terminal(
                conn,
                ctx,
                &request_id,
                book_id,
                request_generation,
                request_epoch,
                if reason == "expired" {
                    "expired"
                } else {
                    "blocked"
                },
                reason,
            )?;
        }
    }
    if !full || live_generation != generation || !accepts_scan_requests(&body) {
        return Ok(());
    }
    let rows = conn.prepare(
        "SELECT request_id,key_epoch FROM contact_scan_requests
         WHERE book_id=?1 AND target_owner=?2 AND generation=?3 AND key_epoch=?4
           AND historical=0 AND expires_at>=?5
           AND (status='waiting' OR (status='scanning' AND EXISTS(
               SELECT 1 FROM contact_scans old WHERE old.scan_id=contact_scan_requests.scan_id AND old.complete=0)))",
    )?.query_map(params![book_id,ctx.device_id.to_string(),generation,active_epoch,now()], |row| {
        Ok((row.get::<_,String>(0)?, row.get::<_,u32>(1)?))
    })?.collect::<Result<Vec<_>,_>>()?;
    for (request_id, request_epoch) in rows {
        conn.execute("UPDATE contact_scan_requests SET status='scanning',reason=NULL,scan_id=? WHERE request_id=?", params![scan_id,request_id])?;
        enqueue(
            conn,
            ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::ContactScanResult {
                result: json!({"schema_version":1,"request_id":request_id,"book_id":book_id,
                "target_owner":ctx.device_id.to_string(),"generation":generation.to_string(),
                "key_epoch":request_epoch,"state":"scanning","reason":Value::Null}),
            },
        )?;
    }
    Ok(())
}

pub(crate) fn on_scan_finished(
    conn: &Connection,
    ctx: &Ctx<'_>,
    scan_id: &str,
    successful: bool,
    reason: Option<&str>,
) -> Result<usize, Error> {
    let mut stmt = conn.prepare("SELECT request_id,book_id,generation,key_epoch FROM contact_scan_requests WHERE scan_id=? AND status='scanning'")?;
    let rows = stmt
        .query_map([scan_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, u32>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (request_id, book_id, generation, request_epoch) in &rows {
        let state = if successful { "completed" } else { "blocked" };
        conn.execute(
            "UPDATE contact_scan_requests SET status=?,reason=? WHERE request_id=?",
            params![state, reason, request_id],
        )?;
        let result = json!({"schema_version":1,"request_id":request_id,"book_id":book_id,"target_owner":ctx.device_id.to_string(),"generation":generation.to_string(),"key_epoch":request_epoch,"state":state,"reason":if successful { None } else { reason }});
        enqueue(
            conn,
            ctx,
            EnvelopePurpose::Event,
            None,
            None,
            &PrivatePayload::ContactScanResult { result },
        )?;
    }
    Ok(rows.len())
}

pub(crate) fn apply_event(
    conn: &Connection,
    producer: &str,
    historical: bool,
    envelope_epoch: u32,
    payload: &PrivatePayload,
) -> Result<bool, Error> {
    match payload {
        PrivatePayload::ContactScanRequest { request } => {
            schema(request)?;
            let request_id = canonical_id(request, "request_id")?;
            let book_id = text(request, "book_id")?;
            let target = text(request, "target_owner")?;
            let generation = revision(request, "generation")?;
            let requester = text(request, "requester")?;
            let request_epoch = request
                .get("key_epoch")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(invalid)?;
            let expires = request
                .get("expires_at")
                .and_then(Value::as_i64)
                .ok_or_else(invalid)?;
            if requester != producer
                || request_epoch > envelope_epoch
                || expires > now() + TTL_SECONDS + EXPIRY_SKEW_SECONDS
            {
                return Ok(false);
            }
            let bytes = encoded(request)?;
            if let Some((old, old_requester)) = conn
                .query_row(
                    "SELECT body,requester FROM contact_scan_requests WHERE request_id=?",
                    [request_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?
            {
                return Ok(old == bytes && old_requester == producer);
            }
            let Some((owner, _current_generation)) = conn.query_row(
                "SELECT owner_device_id,generation FROM contact_books WHERE id=? AND forgotten=0",
                [book_id], |row| Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?)),
            ).optional()? else {
                return Ok(true);
            };
            if owner != target {
                return Ok(false);
            }
            let active_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM contact_scan_requests WHERE book_id=? AND requester=? AND historical=0 AND status IN ('queued','waiting','scanning')",
                params![book_id,producer], |row| row.get(0),
            )?;
            if !historical && active_count >= MAX_ACTIVE_PER_BOOK_REQUESTER {
                return Ok(true);
            }
            let (status, reason) = if historical {
                ("expired", Some("historical"))
            } else {
                ("waiting", None)
            };
            conn.execute(
                "INSERT INTO contact_scan_requests(request_id,book_id,requester,target_owner,generation,key_epoch,body,status,reason,expires_at,historical) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                params![request_id,book_id,producer,target,generation,request_epoch,bytes,status,reason,expires,historical],
            )?;
            if let Some((result_producer, result_epoch, result)) = conn.query_row(
                "SELECT producer,key_epoch,body FROM contact_scan_orphan_results WHERE request_id=?", [request_id],
                |row| Ok((row.get::<_,String>(0)?,row.get::<_,u32>(1)?,row.get::<_,String>(2)?)),
            ).optional()? {
                let result: Value = serde_json::from_str(&result).map_err(|_| Error::Database)?;
                apply_result(conn, request_id, &owner, book_id, generation, request_epoch,
                    &result_producer, result_epoch, &result)?;
                conn.execute("DELETE FROM contact_scan_orphan_results WHERE request_id=?", [request_id])?;
            }
            Ok(true)
        }
        PrivatePayload::ContactScanResult { result } => {
            schema(result)?;
            let request_id = canonical_id(result, "request_id")?;
            let book_id = text(result, "book_id")?;
            let target = text(result, "target_owner")?;
            let generation = revision(result, "generation")?;
            let request_epoch = result
                .get("key_epoch")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(invalid)?;
            let state = text(result, "state")?;
            let reason = result.get("reason").and_then(Value::as_str);
            if !valid_reason(state, reason) || envelope_epoch < request_epoch {
                return Ok(false);
            }
            let row: Option<(String,String,i64,u32)> = conn.query_row(
                "SELECT target_owner,book_id,generation,key_epoch FROM contact_scan_requests WHERE request_id=?",
                [request_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
            ).optional()?;
            let Some((owner, stored_book, stored_generation, stored_epoch)) = row else {
                let known_owner: Option<String> = conn
                    .query_row(
                        "SELECT owner_device_id FROM contact_books WHERE id=?",
                        [book_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if known_owner.as_deref() != Some(producer) || target != producer {
                    return Ok(false);
                }
                conn.execute(
                    "DELETE FROM contact_scan_orphan_results WHERE created_at<?",
                    [now() - RETENTION_SECONDS],
                )?;
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM contact_scan_orphan_results",
                    [],
                    |row| row.get(0),
                )?;
                if count >= MAX_ORPHAN_RESULTS {
                    conn.execute("DELETE FROM contact_scan_orphan_results WHERE request_id IN (SELECT request_id FROM contact_scan_orphan_results ORDER BY created_at,rowid LIMIT ?)", [count-MAX_ORPHAN_RESULTS+1])?;
                }
                conn.execute(
                    "INSERT INTO contact_scan_orphan_results(request_id,producer,key_epoch,body,created_at) VALUES(?,?,?,?,?) ON CONFLICT(request_id) DO NOTHING",
                    params![request_id,producer,envelope_epoch,encoded(result)?,now()],
                )?;
                return Ok(true);
            };
            if stored_book != book_id
                || stored_generation != generation
                || owner != target
                || stored_epoch != request_epoch
            {
                return Ok(false);
            }
            apply_result(
                conn,
                request_id,
                &owner,
                &stored_book,
                stored_generation,
                stored_epoch,
                producer,
                envelope_epoch,
                result,
            )
        }
        _ => Ok(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE contact_books(id TEXT PRIMARY KEY, owner_device_id TEXT NOT NULL, generation INTEGER NOT NULL, state TEXT NOT NULL, body TEXT NOT NULL, forgotten INTEGER NOT NULL DEFAULT 0); CREATE TABLE outbox(envelope_id TEXT PRIMARY KEY,state TEXT NOT NULL);").unwrap();
        initialize(&conn).unwrap();
        conn
    }

    fn book(conn: &Connection, owner: &str, generation: i64, state: &str, enabled: bool) {
        let body = json!({"id":"book","owner_device_id":owner,"generation":generation.to_string(),"state":state,"capabilities":{"scan_request":enabled}});
        conn.execute(
            "INSERT INTO contact_books VALUES('book',?,?,?, ?,0)",
            params![owner, generation, state, encoded(&body).unwrap()],
        )
        .unwrap();
    }

    fn request(id: &str, requester: &str, owner: &str, generation: i64, epoch: u32) -> Value {
        json!({"schema_version":1,"request_id":id,"book_id":"book","target_owner":owner,"generation":generation.to_string(),"requester":requester,"key_epoch":epoch,"expires_at":now()+60})
    }

    fn result(
        id: &str,
        owner: &str,
        generation: i64,
        epoch: u32,
        state: &str,
        reason: Option<&str>,
    ) -> Value {
        json!({"schema_version":1,"request_id":id,"book_id":"book","target_owner":owner,"generation":generation.to_string(),"key_epoch":epoch,"state":state,"reason":reason})
    }

    #[test]
    fn stale_generation_request_preserves_payload_epoch_for_owner_rejection() {
        let conn = database();
        book(&conn, "owner", 2, "active", true);
        let id = uuid::Uuid::new_v4().to_string();
        let payload = PrivatePayload::ContactScanRequest {
            request: request(&id, "requester", "owner", 1, 1),
        };
        assert!(apply_event(&conn, "requester", false, 2, &payload).unwrap());
        let row: (i64, u32) = conn
            .query_row(
                "SELECT generation,key_epoch FROM contact_scan_requests WHERE request_id=?",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (1, 1));
    }

    #[test]
    fn nonterminal_progress_is_accepted_but_terminal_state_never_regresses() {
        let conn = database();
        book(&conn, "owner", 1, "active", true);
        let id = uuid::Uuid::new_v4().to_string();
        assert!(
            apply_event(
                &conn,
                "requester",
                true,
                1,
                &PrivatePayload::ContactScanRequest {
                    request: request(&id, "requester", "owner", 1, 1)
                }
            )
            .unwrap()
        );
        assert!(
            apply_event(
                &conn,
                "owner",
                true,
                1,
                &PrivatePayload::ContactScanResult {
                    result: result(&id, "owner", 1, 1, "scanning", None)
                }
            )
            .unwrap()
        );
        assert!(
            apply_event(
                &conn,
                "owner",
                true,
                1,
                &PrivatePayload::ContactScanResult {
                    result: result(&id, "owner", 1, 1, "completed", None)
                }
            )
            .unwrap()
        );
        assert!(
            apply_event(
                &conn,
                "owner",
                true,
                1,
                &PrivatePayload::ContactScanResult {
                    result: result(&id, "owner", 1, 1, "blocked", Some("cancelled"))
                }
            )
            .unwrap()
        );
        let status: String = conn
            .query_row(
                "SELECT status FROM contact_scan_requests WHERE request_id=?",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "completed");
    }

    #[test]
    fn unknown_result_is_cached_only_for_the_known_book_owner() {
        let conn = database();
        book(&conn, "owner", 1, "active", true);
        let id = uuid::Uuid::new_v4().to_string();
        let payload = PrivatePayload::ContactScanResult {
            result: result(&id, "owner", 1, 1, "completed", None),
        };
        assert!(!apply_event(&conn, "attacker", false, 1, &payload).unwrap());
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM contact_scan_orphan_results",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn status_selects_new_request_when_expiry_timestamps_tie() {
        let conn = database();
        let expiry = now() + 60;
        for (id, state) in [("old", "completed"), ("new", "waiting")] {
            conn.execute("INSERT INTO contact_scan_requests(request_id,book_id,requester,target_owner,generation,key_epoch,body,status,expires_at) VALUES(?, 'book','requester','owner',1,1,'{}',?,?)", params![id,state,expiry]).unwrap();
        }
        let changes = conn.total_changes();
        let value: Value =
            serde_json::from_str(&status(&conn, "requester", r#"{"book_id":"book"}"#).unwrap())
                .unwrap();
        assert_eq!(value["requestId"], "new");
        assert_eq!(
            conn.total_changes(),
            changes,
            "status must remain SQL read-only"
        );
    }

    #[test]
    fn request_identity_schema_and_duplicate_bytes_are_strict() {
        let conn = database();
        book(&conn, "owner", 1, "active", true);
        let id = uuid::Uuid::new_v4().to_string();
        let valid = request(&id, "requester", "owner", 1, 1);
        let mut spoofed_requester = valid.clone();
        spoofed_requester["requester"] = json!("other");
        assert!(
            !apply_event(
                &conn,
                "requester",
                false,
                1,
                &PrivatePayload::ContactScanRequest {
                    request: spoofed_requester
                }
            )
            .unwrap()
        );
        let mut spoofed_target = valid.clone();
        spoofed_target["target_owner"] = json!("attacker");
        assert!(
            !apply_event(
                &conn,
                "requester",
                false,
                1,
                &PrivatePayload::ContactScanRequest {
                    request: spoofed_target
                }
            )
            .unwrap()
        );
        let mut bad_schema = valid.clone();
        bad_schema["schema_version"] = json!(2);
        assert!(
            apply_event(
                &conn,
                "requester",
                false,
                1,
                &PrivatePayload::ContactScanRequest {
                    request: bad_schema
                }
            )
            .is_err()
        );
        let mut bad_id = valid.clone();
        bad_id["request_id"] = json!("AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA");
        assert!(
            apply_event(
                &conn,
                "requester",
                false,
                1,
                &PrivatePayload::ContactScanRequest { request: bad_id }
            )
            .is_err()
        );
        assert!(
            apply_event(
                &conn,
                "requester",
                false,
                1,
                &PrivatePayload::ContactScanRequest {
                    request: valid.clone()
                }
            )
            .unwrap()
        );
        assert!(
            !apply_event(
                &conn,
                "attacker",
                false,
                1,
                &PrivatePayload::ContactScanResult {
                    result: result(&id, "owner", 1, 1, "completed", None)
                }
            )
            .unwrap()
        );
        assert!(
            !apply_event(
                &conn,
                "owner",
                false,
                1,
                &PrivatePayload::ContactScanResult {
                    result: result(&id, "owner", 1, 1, "failed", Some("unbounded_peer_reason"))
                }
            )
            .unwrap()
        );
        let mut conflict = valid;
        conflict["expires_at"] = json!(now() + 30);
        assert!(
            !apply_event(
                &conn,
                "requester",
                false,
                1,
                &PrivatePayload::ContactScanRequest { request: conflict }
            )
            .unwrap()
        );
    }

    #[test]
    fn historical_result_before_request_restores_terminal_without_execution() {
        let conn = database();
        book(&conn, "owner", 1, "active", true);
        let id = uuid::Uuid::new_v4().to_string();
        assert!(
            apply_event(
                &conn,
                "owner",
                true,
                1,
                &PrivatePayload::ContactScanResult {
                    result: result(&id, "owner", 1, 1, "blocked", Some("permission_denied"))
                }
            )
            .unwrap()
        );
        assert!(
            apply_event(
                &conn,
                "requester",
                true,
                1,
                &PrivatePayload::ContactScanRequest {
                    request: request(&id, "requester", "owner", 1, 1)
                }
            )
            .unwrap()
        );
        let row: (String, Option<String>, bool) = conn
            .query_row(
                "SELECT status,reason,historical FROM contact_scan_requests WHERE request_id=?",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            ("blocked".into(), Some("permission_denied".into()), true)
        );
    }

    #[test]
    fn orphan_result_cache_is_bounded() {
        let conn = database();
        book(&conn, "owner", 1, "active", true);
        for _ in 0..(MAX_ORPHAN_RESULTS + 20) {
            let id = uuid::Uuid::new_v4().to_string();
            assert!(
                apply_event(
                    &conn,
                    "owner",
                    false,
                    1,
                    &PrivatePayload::ContactScanResult {
                        result: result(&id, "owner", 1, 1, "completed", None)
                    }
                )
                .unwrap()
            );
        }
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM contact_scan_orphan_results",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, MAX_ORPHAN_RESULTS);
    }

    #[test]
    fn book_lookup_does_not_translate_database_failures_to_not_found() {
        let conn = database();
        conn.execute("DROP TABLE contact_books", []).unwrap();
        assert_eq!(super::book(&conn, "book"), Err(Error::Database));
    }

    #[test]
    fn requester_expiry_cleanup_does_not_consume_owner_rejection_work() {
        let conn = database();
        conn.execute("INSERT INTO contact_scan_requests(request_id,book_id,requester,target_owner,generation,key_epoch,body,status,expires_at) VALUES('remote','book','peer','local',1,1,'{}','waiting',0)", []).unwrap();
        expire(&conn, "local").unwrap();
        let state: String = conn
            .query_row(
                "SELECT status FROM contact_scan_requests WHERE request_id='remote'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "waiting");
    }

    #[test]
    fn status_derives_expiry_without_mutating_the_ledger() {
        let conn = database();
        conn.execute("INSERT INTO contact_scan_requests(request_id,book_id,requester,target_owner,generation,key_epoch,body,status,expires_at) VALUES('expired','book','requester','owner',1,1,'{}','waiting',0)", []).unwrap();
        let changes = conn.total_changes();
        let value: Value =
            serde_json::from_str(&status(&conn, "requester", r#"{"book_id":"book"}"#).unwrap())
                .unwrap();
        assert_eq!(
            (value["state"].as_str(), value["reason"].as_str()),
            (Some("expired"), Some("expired"))
        );
        assert_eq!(conn.total_changes(), changes);
        let stored: String = conn
            .query_row(
                "SELECT status FROM contact_scan_requests WHERE request_id='expired'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "waiting");
    }

    #[test]
    fn unknown_book_request_is_consumed_without_creating_work() {
        let conn = database();
        let id = uuid::Uuid::new_v4().to_string();
        assert!(
            apply_event(
                &conn,
                "requester",
                false,
                1,
                &PrivatePayload::ContactScanRequest {
                    request: request(&id, "requester", "owner", 1, 1)
                }
            )
            .unwrap()
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM contact_scan_requests", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn incoming_request_flood_is_bounded_per_book_and_requester() {
        let conn = database();
        book(&conn, "owner", 1, "active", true);
        for _ in 0..(MAX_ACTIVE_PER_BOOK_REQUESTER + 5) {
            let id = uuid::Uuid::new_v4().to_string();
            assert!(
                apply_event(
                    &conn,
                    "requester",
                    false,
                    1,
                    &PrivatePayload::ContactScanRequest {
                        request: request(&id, "requester", "owner", 1, 1)
                    }
                )
                .unwrap()
            );
        }
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM contact_scan_requests", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, MAX_ACTIVE_PER_BOOK_REQUESTER);
    }
}
