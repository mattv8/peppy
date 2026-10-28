//! Encrypted, bounded carrier-history control records.  This module deliberately owns only the
//! wire ledger; attempt scheduling remains in `message_history_requests`.
//!
//! Wire shapes are strict: every control kind has an exact key set, generations and revisions are
//! canonical decimal strings, and key epochs and expiry are JSON integers. Authenticated
//! producers may only speak for themselves (source, requester, cancel) or for their own results.
use crate::{Ctx, Error, PrivatePayload, enqueue};
use peppy_protocol::EnvelopePurpose;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const REQUEST_TTL: i64 = 10 * 60;
const RETENTION: i64 = 24 * 60 * 60;
const MAX_ACTIVE: i64 = 8;
const MAX_ORPHANS: i64 = 256;
const MAX_RECEIPTS: usize = 200;
const SMS_BUDGET: i64 = 100;
const MMS_BUDGET: i64 = 100;
const SOURCE_KEYS: [&str; 9] = [
    "schema_version",
    "producer",
    "generation",
    "revision",
    "key_epoch",
    "capabilities",
    "permission",
    "policy",
    "exhausted",
];
const REQUEST_KEYS: [&str; 9] = [
    "schema_version",
    "request_id",
    "requester",
    "target",
    "generation",
    "key_epoch",
    "expires_at",
    "sms_budget",
    "mms_budget",
];
const RESULT_KEYS: [&str; 10] = [
    "schema_version",
    "request_id",
    "target",
    "generation",
    "key_epoch",
    "state",
    "reason",
    "receipts",
    "source_exhausted",
    "next_progress_token",
];
const CANCEL_KEYS: [&str; 6] = [
    "schema_version",
    "request_id",
    "requester",
    "target",
    "generation",
    "key_epoch",
];

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn invalid() -> Error {
    Error::InvalidRequest("invalid message history control")
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, Error> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty() && v.len() <= 1024)
        .ok_or_else(invalid)
}
/// Canonical decimal string (no sign, no leading zeros) such as a generation or revision.
fn decimal(v: &Value, key: &str) -> Result<i64, Error> {
    let raw = v.get(key).and_then(Value::as_str).ok_or_else(invalid)?;
    let canonical = raw == "0" || !raw.starts_with('0');
    if !canonical || raw.is_empty() || raw.len() > 18 || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    raw.parse().map_err(|_| invalid())
}
fn json_u32(v: &Value, key: &str) -> Result<u32, Error> {
    v.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(invalid)
}
fn json_i64(v: &Value, key: &str) -> Result<i64, Error> {
    v.get(key)
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0)
        .ok_or_else(invalid)
}
fn flag(v: &Value, key: &str) -> Result<bool, Error> {
    v.get(key).and_then(Value::as_bool).ok_or_else(invalid)
}
fn optional_text<'a>(v: &'a Value, key: &str) -> Result<Option<&'a str>, Error> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() && s.len() <= 128 => Ok(Some(s)),
        _ => Err(invalid()),
    }
}
fn uuid<'a>(v: &'a Value, key: &str) -> Result<&'a str, Error> {
    let id = text(v, key)?;
    (uuid::Uuid::parse_str(id)
        .ok()
        .as_ref()
        .is_some_and(|parsed| parsed.to_string() == id))
    .then_some(id)
    .ok_or_else(invalid)
}
/// Rejects any unknown or missing key of a control object.
fn exact(value: &Value, keys: &[&str]) -> Result<(), Error> {
    let fields = value.as_object().ok_or_else(invalid)?;
    (fields.len() == keys.len() && keys.iter().all(|key| fields.contains_key(*key)))
        .then_some(())
        .ok_or_else(invalid)
}
fn encoded(v: &Value) -> Result<String, Error> {
    serde_json::to_string(v).map_err(|_| Error::Database)
}
fn schema(v: &Value) -> Result<(), Error> {
    (v.get("schema_version").and_then(Value::as_i64) == Some(1))
        .then_some(())
        .ok_or_else(invalid)
}
fn terminal(state: &str) -> bool {
    matches!(
        state,
        "completed" | "partial" | "blocked" | "failed" | "expired" | "cancelled"
    )
}
fn valid_state(state: &str) -> bool {
    matches!(
        state,
        "queued"
            | "waiting"
            | "running"
            | "completed"
            | "partial"
            | "blocked"
            | "failed"
            | "expired"
            | "cancelled"
    )
}
/// Bounded reason allowlist per state. Terminal non-success states require one listed reason;
/// intermediate and success states must carry none.
fn reason_ok(state: &str, reason: Option<&str>) -> bool {
    let allowed: &[&str] = match state {
        "partial" => &["budget_exhausted", "source_unavailable"],
        "blocked" => &[
            "permission_denied",
            "policy_denied",
            "generation_changed",
            "source_revoked",
            "source_exhausted",
            "key_epoch_changed",
            "capacity",
        ],
        "failed" => &["provider_error", "storage_error", "internal_error"],
        "expired" => &["expired"],
        "cancelled" => &["cancelled"],
        _ => &[],
    };
    match reason {
        Some(reason) => allowed.contains(&reason),
        None => !matches!(
            state,
            "partial" | "blocked" | "failed" | "expired" | "cancelled"
        ),
    }
}
fn has_forbidden_field(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            key == "query" || key.contains("provider_id") || has_forbidden_field(value)
        }),
        Value::Array(values) => values.iter().any(has_forbidden_field),
        _ => false,
    }
}

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS message_history_sources(producer TEXT PRIMARY KEY,generation INTEGER NOT NULL,revision INTEGER NOT NULL,key_epoch INTEGER NOT NULL,body TEXT NOT NULL,updated_at INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS message_history_requests(request_id TEXT PRIMARY KEY,requester TEXT NOT NULL,target TEXT NOT NULL,generation INTEGER NOT NULL,key_epoch INTEGER NOT NULL,body TEXT NOT NULL,status TEXT NOT NULL,reason TEXT,expires_at INTEGER NOT NULL,historical INTEGER NOT NULL DEFAULT 0); CREATE TABLE IF NOT EXISTS message_history_orphans(request_id TEXT PRIMARY KEY,producer TEXT NOT NULL,key_epoch INTEGER NOT NULL,body TEXT NOT NULL,created_at INTEGER NOT NULL);")?;
    add_column_if_missing(conn, "message_history_requests", "result", "TEXT")?;
    add_column_if_missing(conn, "message_history_requests", "envelope_id", "TEXT")?;
    // Local ledger only: the source revision captured when the request was made. NULL marks a
    // request from before this column, which coverage treats as unproven.
    add_column_if_missing(
        conn,
        "message_history_requests",
        "source_revision",
        "INTEGER",
    )?;
    Ok(())
}
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<(), Error> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?) WHERE name=?)",
        params![table, column],
        |row| row.get(0),
    )?;
    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}"),
            [],
        )?;
    }
    Ok(())
}

/// Removes terminal requests and orphan results older than the 24-hour retention window. Runs on
/// every write path (request, pending, apply) so requester, phone and observer roles all stay
/// bounded.
fn prune(conn: &Connection) -> Result<(), Error> {
    let cutoff = now() - RETENTION;
    conn.execute(
        "DELETE FROM message_history_requests WHERE status IN ('completed','partial','blocked','failed','expired','cancelled') AND expires_at<?",
        [cutoff],
    )?;
    conn.execute(
        "DELETE FROM message_history_orphans WHERE created_at<?",
        [cutoff],
    )?;
    Ok(())
}
/// Moves every live request past its deadline to `expired`, globally, before any coalescing or
/// cap decision, so stale rows (own or other observers') never fill a live slot.
fn expire_stale(conn: &Connection) -> Result<(), Error> {
    conn.execute(
        "UPDATE message_history_requests SET status='expired',reason='expired' WHERE status IN ('queued','waiting','running') AND expires_at<?",
        [now()],
    )?;
    Ok(())
}

fn receipt(value: &Value) -> Result<Value, Error> {
    exact(value, &["messageId", "revision"])?;
    let message_id = uuid(value, "messageId")?;
    let revision = value
        .get("revision")
        .and_then(Value::as_str)
        .filter(|revision| !revision.is_empty() && revision.len() <= 128)
        .ok_or_else(invalid)?;
    Ok(json!({"messageId":message_id,"revision":revision}))
}
/// Builds the canonical receipt/progress projection; extra fields are rejected, not copied.
fn result_details(value: &Value) -> Result<Value, Error> {
    if has_forbidden_field(value) {
        return Err(invalid());
    }
    let receipts = match value.get("receipts") {
        None => Vec::new(),
        Some(Value::Array(items)) if items.len() <= MAX_RECEIPTS => {
            items.iter().map(receipt).collect::<Result<Vec<_>, _>>()?
        }
        _ => return Err(invalid()),
    };
    let exhausted = match value.get("source_exhausted") {
        None => false,
        Some(flag) => flag.as_bool().ok_or_else(invalid)?,
    };
    let token = match value.get("next_progress_token") {
        None | Some(Value::Null) => None,
        Some(Value::String(token)) if !token.is_empty() && token.len() <= 1024 => {
            Some(token.clone())
        }
        _ => return Err(invalid()),
    };
    Ok(json!({"receipts":receipts,"sourceExhausted":exhausted,"nextProgressToken":token}))
}
fn result_wire(
    request_id: &str,
    target: &str,
    generation: i64,
    key_epoch: u32,
    state: &str,
    reason: Option<&str>,
    details: &Value,
) -> Value {
    json!({"schema_version":1,"request_id":request_id,"target":target,"generation":generation.to_string(),"key_epoch":key_epoch,"state":state,"reason":reason,"receipts":details["receipts"],"source_exhausted":details["sourceExhausted"],"next_progress_token":details["nextProgressToken"]})
}
type StoredSource = (i64, i64, u32, Value);
/// `status, reason, expires_at, result, envelope_id, generation, key_epoch, source_revision` of
/// one requester row.
type StatusRow = (
    String,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
    i64,
    u32,
    Option<i64>,
);
/// The retained source row of `producer`, parsed. Read-only.
fn stored_source(conn: &Connection, producer: &str) -> Result<Option<StoredSource>, Error> {
    let row: Option<(i64, i64, u32, String)> = conn
        .query_row(
            "SELECT generation,revision,key_epoch,body FROM message_history_sources WHERE producer=?",
            [producer],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    row.map(|(generation, revision, epoch, body)| {
        Ok((
            generation,
            revision,
            epoch,
            serde_json::from_str(&body).map_err(|_| Error::Database)?,
        ))
    })
    .transpose()
}

/// Publishes this authenticated device's retained carrier-history capability.
///
/// Input: `{generation, capabilities:{sms,mms}, permission, policy, exhausted}` with an optional
/// `revision` (canonical decimal string) and an optional local-only `force_republish` flag. The
/// flag never reaches the wire.
///
/// Revision ownership: when `revision` is omitted, Rust derives `previous + 1` inside the same
/// transaction that stores the source, so the host never supplies a wall clock or preference
/// counter. An explicit `revision` must still increase `(generation, revision)` lexically.
///
/// Coalescing: when the generation, key epoch, capabilities, permission, policy and exhaustion all
/// match the stored source and `force_republish` is false, the call succeeds with
/// `published:false`, keeps the stored revision and writes nothing to the outbox. An explicit
/// `revision` that differs from the stored one is rejected even then. Any change, or a forced
/// call, stores the new revision and enqueues exactly one `MessageHistorySource` event.
pub(crate) fn publish_source(
    conn: &mut Connection,
    ctx: &Ctx<'_>,
    input: &str,
) -> Result<String, Error> {
    let input: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    let epoch = ctx.active_epoch.ok_or(Error::KeysUnavailable)?;
    let generation = decimal(&input, "generation")?;
    let explicit = match input.get("revision") {
        None => None,
        Some(_) => Some(decimal(&input, "revision")?),
    };
    let caps = input.get("capabilities").ok_or_else(invalid)?;
    let (sms, mms) = (flag(caps, "sms")?, flag(caps, "mms")?);
    if !sms && !mms {
        return Err(invalid());
    }
    let permission = flag(&input, "permission")?;
    let policy = flag(&input, "policy")?;
    let exhausted = flag(&input, "exhausted")?;
    let force = match input.get("force_republish") {
        None => false,
        Some(_) => flag(&input, "force_republish")?,
    };
    let producer = ctx.device_id.to_string();
    let tx = conn.transaction()?;
    let current = stored_source(&tx, &producer)?;
    if let Some((old_generation, old_revision, old_epoch, stored)) = &current {
        let unchanged = *old_generation == generation
            && *old_epoch == epoch
            && stored["capabilities"] == json!({"sms":sms,"mms":mms})
            && stored["permission"] == json!(permission)
            && stored["policy"] == json!(policy)
            && stored["exhausted"] == json!(exhausted);
        if unchanged && !force {
            if explicit.is_some_and(|revision| revision != *old_revision) {
                return Err(invalid());
            }
            tx.commit()?;
            return Ok(json!({"producer":&producer,"generation":old_generation.to_string(),"revision":old_revision.to_string(),"published":false}).to_string());
        }
    }
    let revision = explicit.unwrap_or_else(|| {
        current
            .as_ref()
            .map_or(1, |(_, old_revision, _, _)| old_revision + 1)
    });
    if current
        .as_ref()
        .is_some_and(|(old_generation, old_revision, _, _)| {
            generation < *old_generation
                || (generation == *old_generation && revision <= *old_revision)
        })
    {
        return Err(invalid());
    }
    let source = json!({"schema_version":1,"producer":&producer,"generation":generation.to_string(),"revision":revision.to_string(),"key_epoch":epoch,"capabilities":{"sms":sms,"mms":mms},"permission":permission,"policy":policy,"exhausted":exhausted});
    let body = encoded(&source)?;
    tx.execute("INSERT INTO message_history_sources(producer,generation,revision,key_epoch,body,updated_at) VALUES(?,?,?,?,?,?) ON CONFLICT(producer) DO UPDATE SET generation=excluded.generation,revision=excluded.revision,key_epoch=excluded.key_epoch,body=excluded.body,updated_at=excluded.updated_at", params![producer, generation, revision, epoch, body, now()])?;
    enqueue(
        &tx,
        ctx,
        EnvelopePurpose::Event,
        None,
        None,
        &PrivatePayload::MessageHistorySource { source },
    )?;
    tx.commit()?;
    Ok(json!({"producer":&producer,"generation":generation.to_string(),"revision":revision.to_string(),"published":true}).to_string())
}

/// Requester-side coordinator entry: `{target_device_id}` only. Coalesces a live duplicate,
/// enforces the eight-request cap per target, and enqueues the request atomically.
#[allow(dead_code)] // Consumed by the coordinator lane after this protocol lands.
pub(crate) fn request(conn: &mut Connection, ctx: &Ctx<'_>, input: &str) -> Result<String, Error> {
    let input: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    exact(&input, &["target_device_id"])?;
    let target = text(&input, "target_device_id")?;
    let epoch = ctx.active_epoch.ok_or(Error::KeysUnavailable)?;
    let (generation, revision, source_epoch, source) =
        stored_source(conn, target)?.ok_or_else(invalid)?;
    if source_epoch != epoch
        || !(source["capabilities"]["sms"] == json!(true)
            || source["capabilities"]["mms"] == json!(true))
        || source["permission"] != json!(true)
        || source["policy"] != json!(true)
        || source["exhausted"] != json!(false)
    {
        return Err(invalid());
    }
    let owner = ctx.device_id.to_string();
    let tx = conn.transaction()?;
    prune(&tx)?;
    expire_stale(&tx)?;
    let existing: Option<(String, String, i64)> = tx
        .query_row(
            "SELECT request_id,status,expires_at FROM message_history_requests WHERE requester=? AND target=? AND generation=? AND key_epoch=? AND historical=0 AND status IN ('queued','waiting','running') ORDER BY rowid DESC LIMIT 1",
            params![owner, target, generation, epoch],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((request_id, state, expires_at)) = existing {
        tx.commit()?;
        return Ok(json!({"requestId":request_id,"targetDeviceId":target,"state":state,"expiresAt":expires_at,"duplicate":true}).to_string());
    }
    let active: i64 = tx.query_row(
        "SELECT COUNT(*) FROM message_history_requests WHERE target=? AND historical=0 AND status IN ('queued','waiting','running')",
        [target],
        |r| r.get(0),
    )?;
    if active >= MAX_ACTIVE {
        return Err(invalid());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let expires = now() + REQUEST_TTL;
    let body = json!({"schema_version":1,"request_id":id,"requester":&owner,"target":target,"generation":generation.to_string(),"key_epoch":epoch,"expires_at":expires,"sms_budget":SMS_BUDGET,"mms_budget":MMS_BUDGET});
    tx.execute("INSERT INTO message_history_requests(request_id,requester,target,generation,key_epoch,body,status,expires_at,source_revision) VALUES(?,?,?,?,?,?,'queued',?,?)", params![id, owner, target, generation, epoch, encoded(&body)?, expires, revision])?;
    let (envelope_id, _) = enqueue(
        &tx,
        ctx,
        EnvelopePurpose::Event,
        None,
        None,
        &PrivatePayload::MessageHistoryRequest { request: body },
    )?;
    tx.execute(
        "UPDATE message_history_requests SET envelope_id=? WHERE request_id=?",
        params![envelope_id.to_string(), id],
    )?;
    tx.commit()?;
    Ok(
        json!({"requestId":id,"targetDeviceId":target,"state":"queued","expiresAt":expires})
            .to_string(),
    )
}

/// Why a waiting request must not run on this phone now, or `None` when it is admissible.
fn revocation(
    source: Option<&StoredSource>,
    generation: i64,
    request_epoch: u32,
    epoch: u32,
) -> Option<&'static str> {
    let Some((source_generation, _, source_epoch, source)) = source else {
        return Some("source_revoked");
    };
    if *source_generation != generation {
        return Some("generation_changed");
    }
    if *source_epoch != epoch || request_epoch != epoch {
        return Some("key_epoch_changed");
    }
    if source["permission"] != json!(true) {
        return Some("permission_denied");
    }
    if source["policy"] != json!(true) {
        return Some("policy_denied");
    }
    if source["exhausted"] != json!(false) {
        return Some("source_exhausted");
    }
    None
}

/// Phone-side admission. Expires stale rows, then revalidates every waiting request against the
/// current local source. Revoked requests become terminal `blocked` with a published result in
/// the same transaction; only still-admissible work is returned.
pub(crate) fn pending(conn: &mut Connection, ctx: &Ctx<'_>, _input: &str) -> Result<String, Error> {
    let epoch = ctx.active_epoch.ok_or(Error::KeysUnavailable)?;
    let owner = ctx.device_id.to_string();
    let tx = conn.transaction()?;
    prune(&tx)?;
    expire_stale(&tx)?;
    let source = stored_source(&tx, &owner)?;
    let candidates: Vec<(String, String, i64, u32, i64, String)> = {
        let mut rows = tx.prepare("SELECT request_id,requester,generation,key_epoch,expires_at,status FROM message_history_requests WHERE target=? AND historical=0 AND status IN ('waiting','running') ORDER BY expires_at,rowid")?;
        let mapped = rows.query_map([&owner], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?;
        mapped.collect::<Result<Vec<_>, _>>()?
    };
    let mut admitted = Vec::new();
    for (id, requester, generation, request_epoch, expires_at, state) in candidates {
        if let Some(reason) = revocation(source.as_ref(), generation, request_epoch, epoch) {
            tx.execute(
                "UPDATE message_history_requests SET status='blocked',reason=? WHERE request_id=? AND status IN ('waiting','running')",
                params![reason, id],
            )?;
            let details = json!({"receipts":[],"sourceExhausted":false,"nextProgressToken":null});
            let result = result_wire(
                &id,
                &owner,
                generation,
                request_epoch,
                "blocked",
                Some(reason),
                &details,
            );
            enqueue(
                &tx,
                ctx,
                EnvelopePurpose::Event,
                None,
                None,
                &PrivatePayload::MessageHistoryResult { result },
            )?;
            continue;
        }
        admitted.push(json!({"schemaVersion":1,"requestId":id,"requester":requester,"targetDeviceId":owner,"generation":generation.to_string(),"keyEpoch":request_epoch,"expiresAt":expires_at,"state":state,"smsBudget":SMS_BUDGET,"mmsBudget":MMS_BUDGET}));
    }
    tx.commit()?;
    Ok(json!({"requests":admitted}).to_string())
}

/// Coordinator projection of retained source capabilities. Read-only.
#[allow(dead_code)] // Consumed by the coordinator lane after this protocol lands.
pub(crate) fn sources(conn: &Connection, _input: &str) -> Result<String, Error> {
    let mut statement = conn.prepare("SELECT producer,generation,revision,key_epoch,body FROM message_history_sources ORDER BY producer")?;
    let sources = statement
        .query_map([], |row| {
            let body: String = row.get(4)?;
            let source: Value =
                serde_json::from_str(&body).map_err(|_| rusqlite::Error::InvalidQuery)?;
            Ok(json!({"producer":row.get::<_,String>(0)?,"generation":row.get::<_,i64>(1)?.to_string(),"revision":row.get::<_,i64>(2)?.to_string(),"keyEpoch":row.get::<_,u32>(3)?,"capabilities":source["capabilities"],"permission":source["permission"],"policy":source["policy"],"exhausted":source["exhausted"]}))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({"sources":sources}).to_string())
}

/// Requester-only read of a request. Queued work becomes `waiting` once the server acknowledged
/// its envelope; a live deadline past expiry is reported as `expired`.
pub(crate) fn status(conn: &Connection, requester: &str, input: &str) -> Result<String, Error> {
    let input: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    let id = uuid(&input, "request_id")?;
    let row: Option<StatusRow> = conn
        .query_row(
            "SELECT status,reason,expires_at,result,envelope_id,generation,key_epoch,source_revision FROM message_history_requests WHERE request_id=? AND requester=?",
            params![id, requester],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()?;
    let Some((stored, reason, expires, result, envelope, generation, key_epoch, revision)) = row
    else {
        return Ok(Value::Null.to_string());
    };
    let acknowledged = match envelope {
        Some(envelope) => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM outbox WHERE envelope_id=? AND state='acknowledged')",
            [envelope],
            |r| r.get::<_, bool>(0),
        )?,
        None => false,
    };
    let (state, reason) = if !terminal(&stored) && expires < now() {
        ("expired".to_owned(), Some("expired".to_owned()))
    } else if stored == "queued" && acknowledged {
        ("waiting".to_owned(), reason)
    } else {
        (stored, reason)
    };
    // A phone-originated record exists only once the target stored progress or a result. A
    // server acknowledgement alone never sets it, so it cannot count as a response.
    let phone_responded = result.is_some();
    let result = result
        .and_then(|result| serde_json::from_str::<Value>(&result).ok())
        .unwrap_or_else(|| json!({"receipts":[],"sourceExhausted":false,"nextProgressToken":null}));
    // The version fields are the request's own captured identity, never a fresh read of the
    // source: coverage compares them with the source's current advertisement.
    Ok(json!({"requestId":id,"state":state,"reason":reason,"expiresAt":expires,"phoneResponded":phone_responded,"receipts":result["receipts"],"sourceExhausted":result["sourceExhausted"],"nextProgressToken":result["nextProgressToken"],"generation":generation.to_string(),"keyEpoch":key_epoch,"sourceRevision":revision.map(|revision| revision.to_string())}).to_string())
}

/// Atomic cancel for callers that already own a transaction. Terminal requests return their stored
/// state without enqueueing; a live request is cancelled and one cancel event is enqueued. Errors
/// roll back with the caller's transaction, so the request never becomes cancelled without its
/// event.
pub(crate) fn cancel_in_transaction(
    conn: &Connection,
    ctx: &Ctx<'_>,
    input: &str,
) -> Result<String, Error> {
    let input: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    exact(&input, &["request_id"])?;
    let id = uuid(&input, "request_id")?;
    let owner = ctx.device_id.to_string();
    let (target, generation, key_epoch, status): (String, i64, u32, String) = conn
        .query_row(
            "SELECT target,generation,key_epoch,status FROM message_history_requests WHERE request_id=? AND requester=?",
            params![id, owner],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    if terminal(&status) {
        return Ok(json!({"requestId":id,"state":status}).to_string());
    }
    conn.execute(
        "UPDATE message_history_requests SET status='cancelled',reason='cancelled' WHERE request_id=? AND status NOT IN ('completed','partial','blocked','failed','expired','cancelled')",
        [id],
    )?;
    let cancel = json!({"schema_version":1,"request_id":id,"requester":&owner,"target":target,"generation":generation.to_string(),"key_epoch":key_epoch});
    enqueue(
        conn,
        ctx,
        EnvelopePurpose::Event,
        None,
        None,
        &PrivatePayload::MessageHistoryCancel { cancel },
    )?;
    Ok(json!({"requestId":id,"state":"cancelled"}).to_string())
}

pub(crate) fn cancel(conn: &mut Connection, ctx: &Ctx<'_>, input: &str) -> Result<String, Error> {
    let tx = conn.transaction()?;
    let output = cancel_in_transaction(&tx, ctx, input)?;
    tx.commit()?;
    Ok(output)
}

/// Phone-side progress. Atomic with its result event; terminal rows and no-op intermediate
/// progress (`running -> waiting`, repeats) return the stored state without enqueueing.
pub(crate) fn record_progress(
    conn: &mut Connection,
    ctx: &Ctx<'_>,
    input: &str,
) -> Result<String, Error> {
    let result: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    schema(&result)?;
    if has_forbidden_field(&result) {
        return Err(invalid());
    }
    let id = uuid(&result, "request_id")?;
    let state = text(&result, "state")?;
    if !valid_state(state) || state == "queued" {
        return Err(invalid());
    }
    let reason = optional_text(&result, "reason")?;
    if !reason_ok(state, reason) {
        return Err(invalid());
    }
    let details = result_details(&result)?;
    let owner = ctx.device_id.to_string();
    if text(&result, "target")? != owner {
        return Err(invalid());
    }
    let generation = decimal(&result, "generation")?;
    let epoch = json_u32(&result, "key_epoch")?;
    let tx = conn.transaction()?;
    prune(&tx)?;
    let row: Option<(String, i64, u32, String)> = tx
        .query_row(
            "SELECT target,generation,key_epoch,status FROM message_history_requests WHERE request_id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let (stored_target, stored_generation, stored_epoch, status) = row.ok_or_else(invalid)?;
    if stored_target != owner || stored_generation != generation || stored_epoch != epoch {
        return Err(invalid());
    }
    if terminal(&status) || status == state || (status == "running" && state == "waiting") {
        tx.commit()?;
        return Ok(json!({"requestId":id,"state":status}).to_string());
    }
    tx.execute(
        "UPDATE message_history_requests SET status=?,reason=?,result=? WHERE request_id=?",
        params![state, reason, encoded(&details)?, id],
    )?;
    let wire = result_wire(id, &owner, generation, epoch, state, reason, &details);
    enqueue(
        &tx,
        ctx,
        EnvelopePurpose::Event,
        None,
        None,
        &PrivatePayload::MessageHistoryResult { result: wire },
    )?;
    tx.commit()?;
    Ok(json!({"requestId":id,"state":state}).to_string())
}

/// Applies one authenticated history control. Validation failures become `Ok(false)`
/// (quarantine); SQL failures propagate.
pub(crate) fn apply_event(
    conn: &Connection,
    producer: &str,
    historical: bool,
    envelope_epoch: u32,
    payload: &PrivatePayload,
) -> Result<bool, Error> {
    prune(conn)?;
    match payload {
        PrivatePayload::MessageHistorySource { source } => {
            apply_source(conn, producer, envelope_epoch, source)
        }
        PrivatePayload::MessageHistoryRequest { request } => {
            apply_request(conn, producer, historical, envelope_epoch, request)
        }
        PrivatePayload::MessageHistoryResult { result } => {
            apply_result(conn, producer, historical, envelope_epoch, result)
        }
        PrivatePayload::MessageHistoryCancel { cancel } => {
            apply_cancel(conn, producer, envelope_epoch, cancel)
        }
        _ => Ok(true),
    }
}

fn apply_source(
    conn: &Connection,
    producer: &str,
    envelope_epoch: u32,
    source: &Value,
) -> Result<bool, Error> {
    exact(source, &SOURCE_KEYS)?;
    schema(source)?;
    let caps = source.get("capabilities").ok_or_else(invalid)?;
    exact(caps, &["sms", "mms"])?;
    if text(source, "producer")? != producer || json_u32(source, "key_epoch")? != envelope_epoch {
        return Ok(false);
    }
    let generation = decimal(source, "generation")?;
    let revision = decimal(source, "revision")?;
    let (sms, mms) = (flag(caps, "sms")?, flag(caps, "mms")?);
    if (!sms && !mms)
        || !source.get("permission").is_some_and(Value::is_boolean)
        || !source.get("policy").is_some_and(Value::is_boolean)
        || !source.get("exhausted").is_some_and(Value::is_boolean)
    {
        return Ok(false);
    }
    let body = encoded(source)?;
    let old: Option<(i64, i64, String)> = conn
        .query_row(
            "SELECT generation,revision,body FROM message_history_sources WHERE producer=?",
            [producer],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((old_generation, old_revision, old_body)) = old {
        if generation == old_generation && revision == old_revision {
            return Ok(old_body == body);
        }
        if generation < old_generation || (generation == old_generation && revision < old_revision)
        {
            return Ok(true);
        }
    }
    conn.execute("INSERT INTO message_history_sources(producer,generation,revision,key_epoch,body,updated_at) VALUES(?,?,?,?,?,?) ON CONFLICT(producer) DO UPDATE SET generation=excluded.generation,revision=excluded.revision,key_epoch=excluded.key_epoch,body=excluded.body,updated_at=excluded.updated_at", params![producer, generation, revision, envelope_epoch, body, now()])?;
    Ok(true)
}

fn apply_request(
    conn: &Connection,
    producer: &str,
    historical: bool,
    envelope_epoch: u32,
    request: &Value,
) -> Result<bool, Error> {
    exact(request, &REQUEST_KEYS)?;
    schema(request)?;
    let id = uuid(request, "request_id")?;
    let requester = text(request, "requester")?;
    let target = text(request, "target")?;
    let generation = decimal(request, "generation")?;
    let epoch = json_u32(request, "key_epoch")?;
    let expires = json_i64(request, "expires_at")?;
    if requester != producer
        || epoch > envelope_epoch
        || expires > now() + REQUEST_TTL + 60
        || json_i64(request, "sms_budget")? != SMS_BUDGET
        || json_i64(request, "mms_budget")? != MMS_BUDGET
    {
        return Ok(false);
    }
    // Gating first: a replay after the source changed is quarantined, never a silent no-op.
    let Some((source_generation, _, source_epoch, source)) = stored_source(conn, target)? else {
        return Ok(false);
    };
    if source_generation != generation
        || source_epoch != epoch
        || source["permission"] != json!(true)
        || source["policy"] != json!(true)
        || source["exhausted"] != json!(false)
    {
        return Ok(false);
    }
    let body = encoded(request)?;
    if let Some(old) = conn
        .query_row(
            "SELECT body FROM message_history_requests WHERE request_id=?",
            [id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    {
        return Ok(old == body);
    }
    expire_stale(conn)?;
    let (status, reason) = if historical {
        ("expired", Some("historical"))
    } else if expires < now() {
        ("expired", Some("expired"))
    } else {
        ("waiting", None)
    };
    if status == "waiting" {
        let active: i64 = conn.query_row(
            "SELECT COUNT(*) FROM message_history_requests WHERE target=? AND historical=0 AND status IN ('waiting','running')",
            [target],
            |r| r.get(0),
        )?;
        if active >= MAX_ACTIVE {
            return Ok(true);
        }
    }
    conn.execute("INSERT INTO message_history_requests(request_id,requester,target,generation,key_epoch,body,status,reason,expires_at,historical) VALUES(?,?,?,?,?,?,?,?,?,?)", params![id, requester, target, generation, epoch, body, status, reason, expires, historical])?;
    reconcile_orphan(conn, id, target)?;
    Ok(true)
}

/// Applies a result that arrived before its request. Only the authenticated target may produce
/// a result for its own request, so the orphan is claimed by exactly that producer.
fn reconcile_orphan(conn: &Connection, request_id: &str, target: &str) -> Result<(), Error> {
    let orphan: Option<(String, u32, String)> = conn
        .query_row(
            "SELECT producer,key_epoch,body FROM message_history_orphans WHERE request_id=?",
            [request_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((producer, epoch, body)) = orphan else {
        return Ok(());
    };
    if producer != target {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM message_history_orphans WHERE request_id=?",
        [request_id],
    )?;
    let result: Value = serde_json::from_str(&body).map_err(|_| Error::Database)?;
    apply_result(conn, &producer, false, epoch, &result)?;
    Ok(())
}

fn store_orphan(
    conn: &Connection,
    request_id: &str,
    producer: &str,
    envelope_epoch: u32,
    result: &Value,
) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM message_history_orphans WHERE created_at<?",
        [now() - RETENTION],
    )?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM message_history_orphans", [], |r| {
        r.get(0)
    })?;
    if count >= MAX_ORPHANS {
        conn.execute(
            "DELETE FROM message_history_orphans WHERE request_id IN (SELECT request_id FROM message_history_orphans ORDER BY created_at,rowid LIMIT ?)",
            [count - MAX_ORPHANS + 1],
        )?;
    }
    conn.execute(
        "INSERT INTO message_history_orphans(request_id,producer,key_epoch,body,created_at) VALUES(?,?,?,?,?) ON CONFLICT(request_id) DO NOTHING",
        params![request_id, producer, envelope_epoch, encoded(result)?, now()],
    )?;
    Ok(())
}

fn apply_result(
    conn: &Connection,
    producer: &str,
    historical: bool,
    envelope_epoch: u32,
    result: &Value,
) -> Result<bool, Error> {
    exact(result, &RESULT_KEYS)?;
    schema(result)?;
    let id = uuid(result, "request_id")?;
    let target = text(result, "target")?;
    let generation = decimal(result, "generation")?;
    let epoch = json_u32(result, "key_epoch")?;
    let state = text(result, "state")?;
    let reason = optional_text(result, "reason")?;
    if target != producer
        || epoch > envelope_epoch
        || state == "queued"
        || !valid_state(state)
        || !reason_ok(state, reason)
    {
        return Ok(false);
    }
    let details = result_details(result)?;
    let row: Option<(String, i64, u32, String)> = conn
        .query_row(
            "SELECT target,generation,key_epoch,status FROM message_history_requests WHERE request_id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((stored_target, stored_generation, stored_epoch, status)) = row else {
        // Historical replays never resurrect unknown requests; live orphans wait for the request.
        if !historical {
            store_orphan(conn, id, producer, envelope_epoch, result)?;
        }
        return Ok(true);
    };
    if stored_target != producer || stored_generation != generation || stored_epoch != epoch {
        return Ok(false);
    }
    if terminal(&status) || (status == "running" && state == "waiting") || status == state {
        return Ok(true);
    }
    conn.execute(
        "UPDATE message_history_requests SET status=?,reason=?,result=? WHERE request_id=?",
        params![state, reason, encoded(&details)?, id],
    )?;
    Ok(true)
}

/// Requester-authenticated cancel. Unknown rows are consumed (the request never reached this
/// device); a mismatching requester, target, generation or epoch is quarantined.
fn apply_cancel(
    conn: &Connection,
    producer: &str,
    envelope_epoch: u32,
    cancel: &Value,
) -> Result<bool, Error> {
    exact(cancel, &CANCEL_KEYS)?;
    schema(cancel)?;
    let id = uuid(cancel, "request_id")?;
    let requester = text(cancel, "requester")?;
    let target = text(cancel, "target")?;
    let generation = decimal(cancel, "generation")?;
    let epoch = json_u32(cancel, "key_epoch")?;
    if requester != producer || epoch > envelope_epoch {
        return Ok(false);
    }
    let row: Option<(String, String, i64, u32, String)> = conn
        .query_row(
            "SELECT requester,target,generation,key_epoch,status FROM message_history_requests WHERE request_id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((stored_requester, stored_target, stored_generation, stored_epoch, status)) = row
    else {
        return Ok(true);
    };
    if stored_requester != producer
        || stored_target != target
        || stored_generation != generation
        || stored_epoch != epoch
    {
        return Ok(false);
    }
    if !terminal(&status) {
        conn.execute(
            "UPDATE message_history_requests SET status='cancelled',reason='cancelled' WHERE request_id=?",
            [id],
        )?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(producer: &str, generation: i64, revision: i64, epoch: u32) -> Value {
        json!({"schema_version":1,"producer":producer,"generation":generation.to_string(),"revision":revision.to_string(),"key_epoch":epoch,"capabilities":{"sms":true,"mms":false},"permission":true,"policy":true,"exhausted":false})
    }

    #[test]
    fn source_identity_rejects_same_revision_with_different_bytes() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        let advertisement = source("phone", 1, 1, 1);
        assert!(
            apply_event(
                &conn,
                "phone",
                false,
                1,
                &PrivatePayload::MessageHistorySource {
                    source: advertisement.clone()
                }
            )
            .unwrap()
        );
        assert!(
            apply_event(
                &conn,
                "phone",
                false,
                1,
                &PrivatePayload::MessageHistorySource {
                    source: advertisement
                }
            )
            .unwrap()
        );
        let mut conflict = source("phone", 1, 1, 1);
        conflict["permission"] = json!(false);
        assert!(
            !apply_event(
                &conn,
                "phone",
                false,
                1,
                &PrivatePayload::MessageHistorySource { source: conflict }
            )
            .unwrap()
        );
    }

    #[test]
    fn historical_request_is_retained_expired_and_never_admitted() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        let source = source("phone", 4, 1, 2);
        apply_event(
            &conn,
            "phone",
            false,
            2,
            &PrivatePayload::MessageHistorySource { source },
        )
        .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let request = json!({"schema_version":1,"request_id":id,"requester":"desktop","target":"phone","generation":"4","key_epoch":2,"expires_at":now()+60,"sms_budget":100,"mms_budget":100});
        assert!(
            apply_event(
                &conn,
                "desktop",
                true,
                2,
                &PrivatePayload::MessageHistoryRequest { request }
            )
            .unwrap()
        );
        let state: (String, bool) = conn
            .query_row(
                "SELECT status,historical FROM message_history_requests WHERE request_id=?",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, ("expired".into(), true));
    }

    #[test]
    fn requester_status_projects_only_bounded_validated_receipts() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let receipt = json!({"messageId":uuid::Uuid::new_v4().to_string(),"revision":"17"});
        conn.execute("INSERT INTO message_history_requests(request_id,requester,target,generation,key_epoch,body,status,expires_at,result) VALUES(?,?,?,?,?,'{}','completed',?,?)", params![id, "desktop", "phone", 1, 1, now() + 60, json!({"receipts":[receipt],"sourceExhausted":true,"nextProgressToken":"next"}).to_string()]).unwrap();
        let status: Value = serde_json::from_str(
            &status(&conn, "desktop", &json!({"request_id":id}).to_string()).unwrap(),
        )
        .unwrap();
        assert_eq!(status["receipts"].as_array().unwrap().len(), 1);
        assert_eq!(status["sourceExhausted"], true);
        assert_eq!(status["nextProgressToken"], "next");
    }

    #[test]
    fn receipts_and_reasons_are_strict() {
        let uppercase = uuid::Uuid::new_v4().to_string().to_uppercase();
        assert!(
            result_details(&json!({"receipts":[{"messageId":uppercase,"revision":"1"}]})).is_err()
        );
        assert!(result_details(&json!({"receipts":[{"messageId":uuid::Uuid::new_v4().to_string(),"revision":"1","body":"leak"}]})).is_err());
        assert!(reason_ok("blocked", Some("permission_denied")));
        assert!(!reason_ok("blocked", Some("free text")));
        assert!(!reason_ok("completed", Some("permission_denied")));
        assert!(reason_ok("running", None));
    }

    #[test]
    fn decimal_wire_values_are_canonical() {
        assert!(decimal(&json!({"g":"01"}), "g").is_err());
        assert!(decimal(&json!({"g":"+1"}), "g").is_err());
        assert!(decimal(&json!({"g":1}), "g").is_err());
        assert_eq!(decimal(&json!({"g":"0"}), "g").unwrap(), 0);
        assert_eq!(decimal(&json!({"g":"12"}), "g").unwrap(), 12);
    }
}

/// Encrypted end-to-end protocol tests. Each member is a real SQLCipher client; envelopes move
/// through `drain`/`ship`, so every assertion runs against sealed payloads and the real apply path.
#[cfg(test)]
mod client_tests {
    use super::*;
    use crate::{
        ApplyReport, Client, ClientConfig, Cursor, DatabaseKey, DeviceId, Envelope, IngestResult,
        KeyProfile, VaultCheckHeader, VaultId,
    };
    use peppy_crypto::{create_vault_check_header, derive_root_key};
    use tempfile::TempDir;

    const PASSPHRASE: &str = "correct horse battery staple";

    struct Member {
        client: Client,
        device: DeviceId,
    }

    /// One vault profile and header shared by every member, exactly as real devices share them.
    struct Vault {
        id: VaultId,
        profile: KeyProfile,
        header: VaultCheckHeader,
    }

    fn vault() -> Vault {
        let id = VaultId::new();
        let profile = KeyProfile::new(id.0, 1).unwrap();
        let root = derive_root_key(PASSPHRASE, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        Vault {
            id,
            profile,
            header,
        }
    }

    fn member(dir: &TempDir, name: &str, vault: &Vault) -> Member {
        let device = DeviceId::new();
        let config = ClientConfig {
            database_path: dir.path().join(format!("{name}.db")),
            vault_id: vault.id,
            device_id: device,
        };
        let client = Client::open(config, DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
        client
            .unlock(&vault.profile, &vault.header, PASSPHRASE)
            .unwrap();
        client.set_server_compaction_state(true, true).unwrap();
        Member { client, device }
    }

    /// Takes every unacknowledged envelope of `from` and acknowledges it, so nothing ships twice.
    fn drain(from: &Member) -> Vec<Envelope> {
        let envelopes = from.client.pending_outbox().unwrap();
        for envelope in &envelopes {
            from.client.ack_outbox(envelope.envelope_id).unwrap();
        }
        envelopes
    }

    /// Ingests `envelopes` into `to` in order with contiguous cursors, then applies them.
    fn ship(envelopes: &[Envelope], to: &Member) -> ApplyReport {
        for envelope in envelopes {
            let cursor = Cursor(to.client.receive_cursor().unwrap().0 + 1);
            assert!(matches!(
                to.client.ingest(envelope, cursor).unwrap(),
                IngestResult::Journaled | IngestResult::Duplicate
            ));
        }
        to.client.apply_pending(100).unwrap()
    }

    fn with_protocol<T>(member: &Member, run: impl FnOnce(&mut Connection, &Ctx<'_>) -> T) -> T {
        let mut store = member.client.lock().unwrap();
        let (conn, ctx) = store.parts();
        run(conn, &ctx)
    }

    fn value(result: Result<String, Error>) -> Value {
        serde_json::from_str(&result.unwrap()).unwrap()
    }

    fn publish(member: &Member, generation: &str, permission: bool) -> Value {
        value(member.client.publish_message_history_source(
            &json!({"generation":generation,"capabilities":{"sms":true,"mms":false},"permission":permission,"policy":true,"exhausted":false}).to_string(),
        ))
    }

    fn request_from(desktop: &Member, phone: &Member) -> Value {
        let input = json!({"target_device_id": phone.device.to_string()}).to_string();
        with_protocol(desktop, |conn, ctx| value(request(conn, ctx, &input)))
    }

    fn admitted(phone: &Member) -> Vec<Value> {
        let output = with_protocol(phone, |conn, ctx| pending(conn, ctx, "{}"));
        value(output)["requests"].as_array().unwrap().clone()
    }

    fn progress(phone: &Member, request: &Value, state: &str, receipts: Value) -> Value {
        let input = json!({"schema_version":1,"request_id":request["requestId"],"target":phone.device.to_string(),"generation":request["generation"],"key_epoch":request["keyEpoch"],"state":state,"receipts":receipts}).to_string();
        with_protocol(phone, |conn, ctx| value(record_progress(conn, ctx, &input)))
    }

    fn status_of(desktop: &Member, id: &str) -> Value {
        let requester = desktop.device.to_string();
        let input = json!({"request_id": id}).to_string();
        with_protocol(desktop, |conn, _| value(status(conn, &requester, &input)))
    }

    fn request_state(member: &Member, id: &str) -> String {
        with_protocol(member, |conn, _| {
            conn.query_row(
                "SELECT status FROM message_history_requests WHERE request_id=?",
                [id],
                |row| row.get(0),
            )
            .unwrap()
        })
    }

    #[test]
    fn malformed_known_request_is_quarantined_and_a_later_valid_event_applies() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        let observer = member(&dir, "observer", &vault);
        with_protocol(&desktop, |conn, ctx| {
            enqueue(
                conn,
                ctx,
                EnvelopePurpose::Event,
                None,
                None,
                &PrivatePayload::MessageHistoryRequest {
                    request: json!({"schema_version":1,"request_id":uuid::Uuid::new_v4().to_string(),"requester":desktop.device.to_string(),"target":phone.device.to_string(),"generation":"1","key_epoch":1,"expires_at":now()+60}),
                },
            )
            .unwrap();
        });
        let report = ship(&drain(&desktop), &observer);
        assert_eq!(report.quarantined, 1);
        publish(&phone, "1", true);
        let report = ship(&drain(&phone), &observer);
        assert_eq!(report.quarantined, 0);
        let sources = with_protocol(&observer, |conn, _| sources(conn, "{}").unwrap());
        assert_eq!(value(Ok(sources))["sources"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn spoofed_requester_and_canceller_are_quarantined() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let observer = member(&dir, "observer", &vault);
        with_protocol(&phone, |conn, ctx| {
            enqueue(
                conn,
                ctx,
                EnvelopePurpose::Event,
                None,
                None,
                &PrivatePayload::MessageHistoryCancel {
                    cancel: json!({"schema_version":1,"request_id":uuid::Uuid::new_v4().to_string(),"requester":"someone-else","target":phone.device.to_string(),"generation":"1","key_epoch":1}),
                },
            )
            .unwrap();
        });
        assert_eq!(ship(&drain(&phone), &observer).quarantined, 1);
    }

    #[test]
    fn duplicate_requests_coalesce_and_expired_rows_are_replaced() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, "1", true);
        ship(&drain(&phone), &desktop);
        let first = request_from(&desktop, &phone);
        let again = request_from(&desktop, &phone);
        assert_eq!(first["requestId"], again["requestId"]);
        assert_eq!(again["duplicate"], true);
        let id = first["requestId"].as_str().unwrap().to_owned();
        with_protocol(&desktop, |conn, _| {
            conn.execute(
                "UPDATE message_history_requests SET expires_at=0 WHERE request_id=?",
                [&id],
            )
            .unwrap();
        });
        assert_eq!(status_of(&desktop, &id)["state"], "expired");
        let fresh = request_from(&desktop, &phone);
        assert_ne!(fresh["requestId"], first["requestId"]);
    }

    #[test]
    fn revocation_terminally_blocks_admitted_work_and_publishes_the_reason() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, "1", true);
        ship(&drain(&phone), &desktop);
        let request = request_from(&desktop, &phone);
        ship(&drain(&desktop), &phone);
        assert_eq!(admitted(&phone).len(), 1);
        publish(&phone, "1", false);
        assert!(admitted(&phone).is_empty());
        ship(&drain(&phone), &desktop);
        let status = status_of(&desktop, request["requestId"].as_str().unwrap());
        assert_eq!(
            (status["state"].as_str(), status["reason"].as_str()),
            (Some("blocked"), Some("permission_denied"))
        );
    }

    #[test]
    fn generation_change_blocks_and_identical_replay_is_quarantined() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, "1", true);
        ship(&drain(&phone), &desktop);
        let request = request_from(&desktop, &phone);
        ship(&drain(&desktop), &phone);
        publish(&phone, "2", true);
        assert!(admitted(&phone).is_empty());
        ship(&drain(&phone), &desktop);
        let status = status_of(&desktop, request["requestId"].as_str().unwrap());
        assert_eq!(status["reason"], "generation_changed");
        let replay = json!({"schema_version":1,"request_id":request["requestId"],"requester":desktop.device.to_string(),"target":phone.device.to_string(),"generation":"1","key_epoch":1,"expires_at":request["expiresAt"],"sms_budget":100,"mms_budget":100});
        with_protocol(&desktop, |conn, ctx| {
            enqueue(
                conn,
                ctx,
                EnvelopePurpose::Event,
                None,
                None,
                &PrivatePayload::MessageHistoryRequest { request: replay },
            )
            .unwrap();
        });
        assert_eq!(ship(&drain(&desktop), &phone).quarantined, 1);
    }

    #[test]
    fn result_before_request_is_reconciled_for_the_observer() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        let observer = member(&dir, "observer", &vault);
        publish(&phone, "1", true);
        let source = drain(&phone);
        ship(&source, &desktop);
        let request = request_from(&desktop, &phone);
        let request_envelopes = drain(&desktop);
        ship(&request_envelopes, &phone);
        let phone_request = admitted(&phone).remove(0);
        progress(&phone, &phone_request, "completed", json!([]));
        let result = drain(&phone);
        ship(&source, &observer);
        ship(&result, &observer);
        let orphans: i64 = with_protocol(&observer, |conn, _| {
            conn.query_row("SELECT COUNT(*) FROM message_history_orphans", [], |r| {
                r.get(0)
            })
            .unwrap()
        });
        assert_eq!(orphans, 1);
        ship(&request_envelopes, &observer);
        let id = request["requestId"].as_str().unwrap();
        assert_eq!(request_state(&observer, id), "completed");
        let orphans: i64 = with_protocol(&observer, |conn, _| {
            conn.query_row("SELECT COUNT(*) FROM message_history_orphans", [], |r| {
                r.get(0)
            })
            .unwrap()
        });
        assert_eq!(orphans, 0);
    }

    #[test]
    fn late_result_after_cancel_is_consumed_without_regressing_the_terminal_state() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, "1", true);
        ship(&drain(&phone), &desktop);
        let request = request_from(&desktop, &phone);
        ship(&drain(&desktop), &phone);
        let phone_request = admitted(&phone).remove(0);
        progress(&phone, &phone_request, "completed", json!([]));
        let result = drain(&phone);
        let id = request["requestId"].as_str().unwrap().to_owned();
        let cancelled = with_protocol(&desktop, |conn, ctx| {
            value(cancel(conn, ctx, &json!({"request_id": id}).to_string()))
        });
        assert_eq!(cancelled["state"], "cancelled");
        let report = ship(&result, &desktop);
        assert_eq!(report.quarantined, 0);
        assert_eq!(status_of(&desktop, &id)["state"], "cancelled");
        let cancel_envelopes = drain(&desktop);
        assert_eq!(ship(&cancel_envelopes, &phone).quarantined, 0);
        let again = with_protocol(&desktop, |conn, ctx| {
            value(cancel(conn, ctx, &json!({"request_id": id}).to_string()))
        });
        assert_eq!(again["state"], "cancelled");
        assert!(drain(&desktop).is_empty());
    }

    #[test]
    fn enqueue_failure_rolls_back_the_request_row() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, "1", true);
        ship(&drain(&phone), &desktop);
        desktop
            .client
            .set_server_compaction_state(false, false)
            .unwrap();
        let input = json!({"target_device_id": phone.device.to_string()}).to_string();
        assert!(with_protocol(&desktop, |conn, ctx| request(conn, ctx, &input)).is_err());
        let rows: i64 = with_protocol(&desktop, |conn, _| {
            conn.query_row("SELECT COUNT(*) FROM message_history_requests", [], |r| {
                r.get(0)
            })
            .unwrap()
        });
        assert_eq!(rows, 0);
        assert!(drain(&desktop).is_empty());
        desktop
            .client
            .set_server_compaction_state(true, true)
            .unwrap();
    }

    #[test]
    fn cancel_helper_is_atomic_with_the_callers_transaction() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, "1", true);
        ship(&drain(&phone), &desktop);
        let request = request_from(&desktop, &phone);
        let id = request["requestId"].as_str().unwrap().to_owned();
        let input = json!({"request_id": id}).to_string();
        with_protocol(&desktop, |conn, ctx| {
            let tx = conn.transaction().unwrap();
            cancel_in_transaction(&tx, ctx, &input).unwrap();
            drop(tx);
        });
        assert_eq!(request_state(&desktop, &id), "queued");
        assert!(drain(&desktop).len() == 1);
    }

    #[test]
    fn retention_prunes_terminal_rows_after_twenty_four_hours() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let observer = member(&dir, "observer", &vault);
        with_protocol(&observer, |conn, _| {
            conn.execute(
                "INSERT INTO message_history_requests(request_id,requester,target,generation,key_epoch,body,status,expires_at) VALUES('old','a','b',1,1,'{}','completed',?)",
                [now() - RETENTION - 1],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message_history_requests(request_id,requester,target,generation,key_epoch,body,status,expires_at) VALUES('new','a','b',1,1,'{}','completed',?)",
                [now() + 60],
            )
            .unwrap();
            prune(conn).unwrap();
            let remaining: Vec<String> = conn
                .prepare("SELECT request_id FROM message_history_requests ORDER BY request_id")
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(remaining, vec!["new".to_owned()]);
        });
    }
}
