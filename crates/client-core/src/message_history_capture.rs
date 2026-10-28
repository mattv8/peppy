//! Durable Android admission for bounded carrier-history pages.
//!
//! Provider generations are opaque UUIDs that never leave this module: encrypted controls carry
//! only the decimal control generation allocated here. A page covers one transport for the
//! pending requests that lack that transport's claim when the page is begun. At most one page
//! lease is open per control generation and transport. An expired lease settles its claims as
//! partial without granting new budget, so a crashed worker can never re-admit the same request.

use crate::{Ctx, Error, message_history_protocol};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

const BUDGET: i64 = 100;
const LEASE_SECS: i64 = 30;
const RETENTION_SECS: i64 = 24 * 60 * 60;
const TRANSPORTS: [&str; 2] = ["sms", "mms"];
const REASONS: [&str; 6] = [
    "permission_denied",
    "policy_denied",
    "capacity",
    "provider_error",
    "storage_error",
    "generation_changed",
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceInput {
    provider_generation: String,
    sms: bool,
    mms: bool,
    permission: bool,
    policy: bool,
    #[serde(default)]
    force_republish: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FinishInput {
    provider_generation: String,
    token: String,
    observations: i64,
    exhausted: bool,
    reason: Option<String>,
}

/// One open page lease, read while settling expired or rotated leases.
struct Page {
    token: String,
    generation: i64,
    transport: String,
    baseline: i64,
    claimed_at: i64,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn invalid() -> Error {
    Error::InvalidRequest("history capture")
}
fn transport_name(value: &str) -> Result<&'static str, Error> {
    TRANSPORTS
        .into_iter()
        .find(|transport| *transport == value)
        .ok_or_else(invalid)
}
/// Caller-supplied finish reasons. Coverage outcomes (budget, source) are derived, never supplied.
fn supplied_reason(value: Option<&str>) -> Result<Option<&'static str>, Error> {
    match value {
        None => Ok(None),
        Some(value) => REASONS
            .into_iter()
            .find(|reason| *reason == value)
            .map(Some)
            .ok_or_else(invalid),
    }
}
fn parse_provider(provider_generation: &str) -> Result<&str, Error> {
    Uuid::parse_str(provider_generation)
        .map_err(|_| Error::InvalidRequest("provider generation"))?;
    Ok(provider_generation)
}
fn live(status: &str) -> bool {
    matches!(status, "waiting" | "running")
}
/// Higher rank wins aggregation: blocked reasons outrank failures, which outrank partial coverage.
fn precedence(state: &str) -> u8 {
    match state {
        "blocked" => 4,
        "failed" => 3,
        "partial" => 2,
        _ => 1,
    }
}

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS message_history_capture_mappings(
            control_generation INTEGER PRIMARY KEY CHECK(control_generation > 0),
            provider_generation TEXT NOT NULL,
            active INTEGER NOT NULL CHECK(active IN (0,1)));
         CREATE UNIQUE INDEX IF NOT EXISTS message_history_capture_one_active_mapping
            ON message_history_capture_mappings(active) WHERE active=1;
         CREATE TABLE IF NOT EXISTS message_history_capture_pages(
            token TEXT PRIMARY KEY,
            control_generation INTEGER NOT NULL,
            transport TEXT NOT NULL CHECK(transport IN ('sms','mms')),
            budget INTEGER NOT NULL CHECK(budget=100),
            baseline INTEGER NOT NULL,
            claimed_at INTEGER NOT NULL,
            state TEXT NOT NULL CHECK(state IN ('claimed','closed')));
         CREATE INDEX IF NOT EXISTS message_history_capture_pages_lease
            ON message_history_capture_pages(control_generation,transport,state);
         CREATE TABLE IF NOT EXISTS message_history_capture_page_claims(
            request_id TEXT NOT NULL,
            transport TEXT NOT NULL CHECK(transport IN ('sms','mms')),
            token TEXT NOT NULL REFERENCES message_history_capture_pages(token),
            state TEXT NOT NULL CHECK(state IN ('claimed','completed','partial','blocked','failed')),
            reason TEXT,
            exhausted INTEGER NOT NULL DEFAULT 0,
            observations INTEGER NOT NULL DEFAULT 0,
            receipts TEXT,
            finished_at INTEGER,
            PRIMARY KEY(request_id,transport));
         CREATE INDEX IF NOT EXISTS message_history_capture_page_claims_token
            ON message_history_capture_page_claims(token);
         CREATE TABLE IF NOT EXISTS message_history_capture_publications(
            request_id TEXT PRIMARY KEY,
            result TEXT NOT NULL,
            published INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL);
         CREATE INDEX IF NOT EXISTS message_history_capture_publications_pending
            ON message_history_capture_publications(published,created_at);",
    )?;
    Ok(())
}

/// The single admitted provider generation and its decimal control generation, if any.
fn active_mapping(conn: &Connection) -> Result<Option<(i64, String)>, Error> {
    conn.query_row(
        "SELECT control_generation,provider_generation FROM message_history_capture_mappings WHERE active=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(Into::into)
}
/// The active decimal generation when `provider_generation` is the currently published provider.
/// Any other value, including legacy or malformed strings, maps to nothing so optional history
/// stays inert. Only publication validates the UUID.
fn mapped_generation(conn: &Connection, provider_generation: &str) -> Result<Option<i64>, Error> {
    Ok(active_mapping(conn)?
        .and_then(|(generation, active)| (active == provider_generation).then_some(generation)))
}
/// Generation the owner's retained source already advertises, or zero when none is retained.
fn retained_generation(conn: &Connection, owner: &str) -> Result<i64, Error> {
    conn.query_row(
        "SELECT generation FROM message_history_sources WHERE producer=?",
        [owner],
        |row| row.get(0),
    )
    .optional()
    .map(|generation| generation.unwrap_or(0))
    .map_err(Into::into)
}
/// Returns the active decimal generation for `provider_generation`. A UUID that is not the active
/// one, or whose active number would sit behind the retained source, receives a fresh higher
/// number; numbers are never reused, so a returning UUID cannot reactivate old claims.
fn activate_generation(
    conn: &mut Connection,
    owner: &str,
    provider_generation: &str,
) -> Result<i64, Error> {
    let provider = parse_provider(provider_generation)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let retained = retained_generation(&tx, owner)?;
    if let Some((generation, active)) = active_mapping(&tx)?
        && active == provider
        && generation >= retained
    {
        tx.commit()?;
        return Ok(generation);
    }
    let highest: i64 = tx.query_row(
        "SELECT COALESCE(MAX(control_generation),0) FROM message_history_capture_mappings",
        [],
        |row| row.get(0),
    )?;
    let generation = highest.max(retained) + 1;
    tx.execute(
        "UPDATE message_history_capture_mappings SET active=0 WHERE active=1",
        [],
    )?;
    tx.execute(
        "INSERT INTO message_history_capture_mappings(control_generation,provider_generation,active) VALUES(?,?,1)",
        params![generation, provider],
    )?;
    tx.commit()?;
    Ok(generation)
}

/// Publishes this device's capture source. Exhaustion is derived from durable history evidence for
/// each enabled transport; the host never states it. With both transports disabled, a retained
/// source is withdrawn (policy and permission false, capabilities kept, never an error) and no
/// retained source makes the call inert.
pub(crate) fn publish_source(
    conn: &mut Connection,
    ctx: &Ctx<'_>,
    input: &str,
) -> Result<String, Error> {
    let input: SourceInput =
        serde_json::from_str(input).map_err(|_| Error::InvalidRequest("history capture source"))?;
    let owner = ctx.device_id.to_string();
    let (sms, mms, permission, policy) = if input.sms || input.mms {
        (input.sms, input.mms, input.permission, input.policy)
    } else {
        match retained_capabilities(conn, &owner)? {
            Some((sms, mms)) => (sms, mms, false, false),
            None => return Ok(json!({"producer":owner,"published":false}).to_string()),
        }
    };
    let generation = activate_generation(conn, &owner, &input.provider_generation)?;
    let exhausted = exhausted_for(conn, &input.provider_generation, sms, mms)?;
    message_history_protocol::publish_source(
        conn,
        ctx,
        &json!({"generation":generation.to_string(),"capabilities":{"sms":sms,"mms":mms},"permission":permission,"policy":policy,"exhausted":exhausted,"force_republish":input.force_republish}).to_string(),
    )
}

/// Capabilities of this device's retained source, if one was ever published.
fn retained_capabilities(conn: &Connection, owner: &str) -> Result<Option<(bool, bool)>, Error> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM message_history_sources WHERE producer=?",
            [owner],
            |row| row.get(0),
        )
        .optional()?;
    body.map(|body| -> Result<(bool, bool), Error> {
        let body: Value = serde_json::from_str(&body).map_err(|_| Error::Database)?;
        Ok((
            body["capabilities"]["sms"] == json!(true),
            body["capabilities"]["mms"] == json!(true),
        ))
    })
    .transpose()
}

/// True only when every enabled transport has durable exhaustion evidence for `provider`.
fn exhausted_for(conn: &Connection, provider: &str, sms: bool, mms: bool) -> Result<bool, Error> {
    for (enabled, transport) in [(sms, "sms"), (mms, "mms")] {
        if enabled && !history_proven(conn, provider, transport)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Admitted requests for the published provider generation, plus whether each transport has
/// unclaimed eligible work and no open lease. Claimed requests never make a transport ready.
pub(crate) fn pending(
    conn: &mut Connection,
    ctx: &Ctx<'_>,
    provider_generation: &str,
) -> Result<String, Error> {
    let Some(generation) = mapped_generation(conn, provider_generation)? else {
        return Ok(json!({"requests":[],"needsSms":false,"needsMms":false}).to_string());
    };
    let owner = ctx.device_id.to_string();
    let mut body: Value =
        serde_json::from_str(&message_history_protocol::pending(conn, ctx, "{}")?)
            .map_err(|_| Error::Database)?;
    body["requests"]
        .as_array_mut()
        .ok_or(Error::Database)?
        .retain(|request| request["generation"].as_str() == Some(&generation.to_string()));
    let candidates = request_ids(&body)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    settle(&tx, &owner, Some(generation), None)?;
    let needs_sms = needs_transport(&tx, &owner, generation, "sms", &candidates)?;
    let needs_mms = needs_transport(&tx, &owner, generation, "mms", &candidates)?;
    tx.commit()?;
    body["needsSms"] = json!(needs_sms);
    body["needsMms"] = json!(needs_mms);
    Ok(body.to_string())
}

fn request_ids(body: &Value) -> Result<Vec<String>, Error> {
    body["requests"]
        .as_array()
        .ok_or(Error::Database)?
        .iter()
        .map(|request| {
            request["requestId"]
                .as_str()
                .map(str::to_owned)
                .ok_or(Error::Database)
        })
        .collect()
}

fn leased(conn: &Connection, generation: i64, transport: &str) -> Result<bool, Error> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM message_history_capture_pages WHERE control_generation=? AND transport=? AND state='claimed')",
        params![generation, transport],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// Whether `transport` has work to wake for now: advertised, no open lease, and at least one
/// admissible pending request lacking this transport's claim.
fn needs_transport(
    conn: &Connection,
    owner: &str,
    generation: i64,
    transport: &str,
    candidates: &[String],
) -> Result<bool, Error> {
    if !advertised(conn, owner, generation, transport)? || leased(conn, generation, transport)? {
        return Ok(false);
    }
    Ok(!eligible_requests(conn, owner, generation, transport, candidates)?.is_empty())
}

/// Candidates that are still admissible on `generation` and lack a claim for `transport`.
fn eligible_requests(
    conn: &Connection,
    owner: &str,
    generation: i64,
    transport: &str,
    candidates: &[String],
) -> Result<Vec<String>, Error> {
    let now = now();
    let mut eligible = Vec::new();
    for request_id in candidates {
        let admissible: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM message_history_requests WHERE request_id=? AND target=? AND generation=? AND historical=0 AND status IN ('waiting','running') AND expires_at>=?)",
            params![request_id, owner, generation, now],
            |row| row.get(0),
        )?;
        let claimed: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM message_history_capture_page_claims WHERE request_id=? AND transport=?)",
            params![request_id, transport],
            |row| row.get(0),
        )?;
        if admissible && !claimed {
            eligible.push(request_id.clone());
        }
    }
    Ok(eligible)
}

fn pending_request_ids(
    conn: &mut Connection,
    ctx: &Ctx<'_>,
    provider_generation: &str,
) -> Result<Vec<String>, Error> {
    let body: Value = serde_json::from_str(&pending(conn, ctx, provider_generation)?)
        .map_err(|_| Error::Database)?;
    body["requests"]
        .as_array()
        .ok_or(Error::Database)?
        .iter()
        .map(|request| {
            request["requestId"]
                .as_str()
                .map(str::to_owned)
                .ok_or(Error::Database)
        })
        .collect()
}

fn source_body(conn: &Connection, owner: &str, generation: i64) -> Result<Option<Value>, Error> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM message_history_sources WHERE producer=? AND generation=?",
            params![owner, generation],
            |row| row.get(0),
        )
        .optional()?;
    body.map(|body| serde_json::from_str(&body).map_err(|_| Error::Database))
        .transpose()
}
fn advertised(
    conn: &Connection,
    owner: &str,
    generation: i64,
    transport: &str,
) -> Result<bool, Error> {
    Ok(source_body(conn, owner, generation)?
        .is_some_and(|body| body["capabilities"][transport] == json!(true)))
}

/// Imported, local-device, matching-transport messages after `baseline`. The filters run in SQL
/// before the page limit, so other transports and live rows cannot hide matches. Returns at most
/// `BUDGET` receipts and whether more matched. Malformed payloads and SQL failures propagate.
fn receipts(
    conn: &Connection,
    owner: &str,
    baseline: i64,
    transport: &str,
) -> Result<(Vec<Value>, bool), Error> {
    let mut statement = conn.prepare(
        "SELECT id,source_sequence FROM messages WHERE local_order>? AND source_device_id=? AND json_extract(payload,'$.imported')=1 AND json_extract(payload,'$.transport')=? ORDER BY local_order LIMIT ?",
    )?;
    let rows = statement
        .query_map(params![baseline, owner, transport, BUDGET + 1], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let truncated = rows.len() as i64 > BUDGET;
    let receipts = rows
        .into_iter()
        .take(BUDGET as usize)
        .map(|(id, sequence)| json!({"messageId":id,"revision":sequence.to_string()}))
        .collect();
    Ok((receipts, truncated))
}

/// Durable history evidence that exhaustion may be published as completed for `transport`. Every
/// imported acquisition of the provider generation must be complete, no imported checkpoint may be
/// open, paused or errored, and at least one checkpoint must have acknowledged exhaustion. Rows of
/// other generations, live messages and other transports are never evidence here.
fn history_proven(conn: &Connection, provider: &str, transport: &str) -> Result<bool, Error> {
    let (open, pending, acknowledged): (bool, bool, bool) = match transport {
        "sms" => (
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sms_scan_checkpoints WHERE source_generation=? AND imported=1 AND (history_complete=0 OR paused_reason IS NOT NULL OR provider_error<>0))",
                [provider],
                |row| row.get(0),
            )?,
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sms_acquisitions WHERE source_generation=? AND json_extract(input,'$.imported')=1 AND state!='complete')",
                [provider],
                |row| row.get(0),
            )?,
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sms_scan_checkpoints WHERE source_generation=? AND imported=1 AND history_complete=1 AND paused_reason IS NULL AND provider_error=0)",
                [provider],
                |row| row.get(0),
            )?,
        ),
        "mms" => (
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM mms_backfill_scopes WHERE source_generation=? AND (history_complete=0 OR pause_reason IS NOT NULL))",
                [provider],
                |row| row.get(0),
            )?,
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM mms_acquisitions WHERE source_generation=? AND json_extract(input,'$.imported')=1 AND state!='complete')",
                [provider],
                |row| row.get(0),
            )?,
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM mms_backfill_scopes WHERE source_generation=? AND history_complete=1 AND pause_reason IS NULL)",
                [provider],
                |row| row.get(0),
            )?,
        ),
        _ => return Ok(false),
    };
    Ok(!open && !pending && acknowledged)
}

/// Terminal claim state for one finished page. Blocking reasons come first, then truncation or a
/// full page (never exhausted), then exhaustion. Everything else is conservative partial coverage.
fn outcome(
    supplied: Option<&'static str>,
    exhausted: bool,
    truncated: bool,
    observations: i64,
) -> (&'static str, Option<&'static str>) {
    match supplied {
        Some(
            reason @ ("permission_denied" | "policy_denied" | "capacity" | "generation_changed"),
        ) => ("blocked", Some(reason)),
        Some(reason) => ("failed", Some(reason)),
        None if truncated || observations == BUDGET => ("partial", Some("budget_exhausted")),
        None if exhausted => ("completed", None),
        None => ("partial", Some("source_unavailable")),
    }
}

fn claimed_requests(conn: &Connection, token: &str) -> Result<Vec<String>, Error> {
    let mut statement = conn.prepare(
        "SELECT request_id FROM message_history_capture_page_claims WHERE token=? AND state='claimed' ORDER BY request_id",
    )?;
    statement
        .query_map([token], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Settles every open page that can no longer produce a result, then prunes retained rows.
/// Pages of another control generation are blocked as `generation_changed`. A page whose lease
/// expired is recovered as partial with its bounded receipts. `keep` skips the page being
/// finished, so an executor that finishes late still reports its real observations.
fn settle(
    tx: &Transaction<'_>,
    owner: &str,
    current: Option<i64>,
    keep: Option<&str>,
) -> Result<(), Error> {
    let now = now();
    let pages = {
        let mut statement = tx.prepare(
            "SELECT token,control_generation,transport,baseline,claimed_at FROM message_history_capture_pages WHERE state='claimed'",
        )?;
        statement
            .query_map([], |row| {
                Ok(Page {
                    token: row.get(0)?,
                    generation: row.get(1)?,
                    transport: row.get(2)?,
                    baseline: row.get(3)?,
                    claimed_at: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for page in pages {
        if keep == Some(page.token.as_str()) {
            continue;
        }
        if current != Some(page.generation) {
            tx.execute(
                "UPDATE message_history_capture_page_claims SET state='blocked',reason='generation_changed',receipts='[]',finished_at=? WHERE token=? AND state='claimed'",
                params![now, page.token],
            )?;
            tx.execute(
                "UPDATE message_history_capture_pages SET state='closed' WHERE token=?",
                [&page.token],
            )?;
        } else if page.claimed_at + LEASE_SECS <= now {
            recover(tx, owner, &page, now)?;
        }
    }
    prune(tx, now)
}

/// Abandoned lease: the executor is gone. Its claims become partial/source_unavailable with the
/// receipts actually imported since the page baseline. Those requests keep their claim, so no
/// new budget can be granted for them.
fn recover(tx: &Transaction<'_>, owner: &str, page: &Page, now: i64) -> Result<(), Error> {
    let requests = claimed_requests(tx, &page.token)?;
    let (captured, _) = receipts(tx, owner, page.baseline, &page.transport)?;
    tx.execute(
        "UPDATE message_history_capture_page_claims SET state='partial',reason='source_unavailable',exhausted=0,observations=0,receipts=?,finished_at=? WHERE token=? AND state='claimed'",
        params![
            serde_json::to_string(&captured).map_err(|_| Error::Database)?,
            now,
            page.token
        ],
    )?;
    tx.execute(
        "UPDATE message_history_capture_pages SET state='closed' WHERE token=?",
        [&page.token],
    )?;
    for request_id in requests {
        aggregate(tx, owner, page.generation, &request_id)?;
    }
    Ok(())
}

fn prune(tx: &Transaction<'_>, now: i64) -> Result<(), Error> {
    let cutoff = now - RETENTION_SECS;
    tx.execute(
        "DELETE FROM message_history_capture_page_claims WHERE (state!='claimed' AND finished_at<?1) OR (state='claimed' AND request_id NOT IN (SELECT request_id FROM message_history_requests))",
        [cutoff],
    )?;
    tx.execute(
        "DELETE FROM message_history_capture_pages WHERE state='closed' AND claimed_at<? AND token NOT IN (SELECT token FROM message_history_capture_page_claims)",
        [cutoff],
    )?;
    tx.execute(
        "DELETE FROM message_history_capture_publications WHERE created_at<?",
        [cutoff],
    )?;
    Ok(())
}

/// Queues one terminal result once every advertised transport of the request has a settled claim.
/// Requests that are no longer live on this generation (expired, terminal, rotated, missing) are
/// skipped; the protocol owns their terminal state.
fn aggregate(
    tx: &Transaction<'_>,
    owner: &str,
    generation: i64,
    request_id: &str,
) -> Result<(), Error> {
    let now = now();
    let request: Option<(i64, u32, i64, String)> = tx
        .query_row(
            "SELECT generation,key_epoch,expires_at,status FROM message_history_requests WHERE request_id=? AND target=?",
            params![request_id, owner],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((request_generation, epoch, expires_at, status)) = request else {
        return Ok(());
    };
    if request_generation != generation || expires_at < now || !live(&status) {
        return Ok(());
    }
    let Some(source) = source_body(tx, owner, generation)? else {
        return Ok(());
    };
    let mut claims = Vec::new();
    for transport in TRANSPORTS
        .into_iter()
        .filter(|transport| source["capabilities"][*transport] == json!(true))
    {
        let claim: Option<(String, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT state,reason,receipts FROM message_history_capture_page_claims WHERE request_id=? AND transport=?",
                params![request_id, transport],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        match claim {
            Some(claim) if claim.0 != "claimed" => claims.push(claim),
            _ => return Ok(()),
        }
    }
    let mut worst: Option<(u8, String, Option<String>)> = None;
    let mut receipts = Vec::new();
    for (state, reason, raw) in claims {
        if let Some(raw) = raw {
            receipts.extend(serde_json::from_str::<Vec<Value>>(&raw).map_err(|_| Error::Database)?);
        }
        let rank = precedence(&state);
        if worst.as_ref().is_none_or(|(current, _, _)| rank > *current) {
            worst = Some((rank, state, reason));
        }
    }
    let Some((_, state, reason)) = worst else {
        return Ok(());
    };
    let result = json!({"schema_version":1,"request_id":request_id,"target":owner,"generation":generation.to_string(),"key_epoch":epoch,"state":state,"reason":reason,"receipts":receipts,"source_exhausted":state == "completed","next_progress_token":null});
    tx.execute(
        "INSERT INTO message_history_capture_publications(request_id,result,published,created_at) VALUES(?,?,0,?) ON CONFLICT(request_id) DO NOTHING",
        params![request_id, result.to_string(), now],
    )?;
    Ok(())
}

/// Admits one page for `transport`: the caller's pending requests that lack this transport's
/// claim, bounded to one shared 100-observation budget. Returns `null` when the transport is not
/// advertised, a page for it is still leased, or no unclaimed request is pending.
pub(crate) fn begin(
    conn: &mut Connection,
    ctx: &Ctx<'_>,
    provider_generation: &str,
    transport: &str,
) -> Result<String, Error> {
    let transport = transport_name(transport)?;
    let owner = ctx.device_id.to_string();
    let Some(generation) = mapped_generation(conn, provider_generation)? else {
        return Ok(Value::Null.to_string());
    };
    if !advertised(conn, &owner, generation, transport)? {
        return Ok(Value::Null.to_string());
    }
    let candidates = pending_request_ids(conn, ctx, provider_generation)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    settle(&tx, &owner, Some(generation), None)?;
    if leased(&tx, generation, transport)? {
        tx.commit()?;
        return Ok(Value::Null.to_string());
    }
    let eligible = eligible_requests(&tx, &owner, generation, transport, &candidates)?;
    if eligible.is_empty() {
        tx.commit()?;
        return Ok(Value::Null.to_string());
    }
    let now = now();
    let token = Uuid::new_v4().to_string();
    let baseline: i64 = tx.query_row(
        "SELECT COALESCE(MAX(local_order),0) FROM messages",
        [],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO message_history_capture_pages(token,control_generation,transport,budget,baseline,claimed_at,state) VALUES(?,?,?,?,?,?,'claimed')",
        params![token, generation, transport, BUDGET, baseline, now],
    )?;
    for request_id in &eligible {
        tx.execute(
            "INSERT INTO message_history_capture_page_claims(request_id,transport,token,state) VALUES(?,?,?,'claimed')",
            params![request_id, transport, token],
        )?;
    }
    tx.commit()?;
    Ok(json!({"token":token,"budget":BUDGET}).to_string())
}

/// Finishes one page with its actual observations and records each claimed request's coverage.
/// A repeated finish of a closed page is a no-op. Requests are aggregated only when every
/// advertised transport has settled, then the durable results are published.
pub(crate) fn finish(conn: &mut Connection, ctx: &Ctx<'_>, input: &str) -> Result<String, Error> {
    let input: FinishInput = serde_json::from_str(input).map_err(|_| invalid())?;
    if !(0..=BUDGET).contains(&input.observations) {
        return Err(invalid());
    }
    let supplied = supplied_reason(input.reason.as_deref())?;
    let owner = ctx.device_id.to_string();
    let current = active_mapping(conn)?.map(|(generation, _)| generation);
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    settle(&tx, &owner, current, Some(&input.token))?;
    tx.commit()?;
    let generation = mapped_generation(conn, &input.provider_generation)?.ok_or_else(invalid)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let page: Option<(i64, String, i64, String)> = tx
        .query_row(
            "SELECT control_generation,transport,baseline,state FROM message_history_capture_pages WHERE token=?",
            [&input.token],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((page_generation, transport, baseline, state)) = page else {
        return Err(invalid());
    };
    if page_generation != generation {
        return Err(invalid());
    }
    if state == "closed" {
        tx.commit()?;
        return Ok(json!({"token":input.token,"state":"finished"}).to_string());
    }
    let (captured, truncated) = receipts(&tx, &owner, baseline, &transport)?;
    // The caller's `exhausted` is necessary but never sufficient: completion also needs durable
    // history evidence for this provider generation and transport.
    let exhausted = input.exhausted && history_proven(&tx, &input.provider_generation, &transport)?;
    let (claim_state, claim_reason) = outcome(supplied, exhausted, truncated, input.observations);
    let requests = claimed_requests(&tx, &input.token)?;
    tx.execute(
        "UPDATE message_history_capture_page_claims SET state=?1,reason=?2,exhausted=?3,observations=?4,receipts=?5,finished_at=?6 WHERE token=?7 AND state='claimed'",
        params![
            claim_state,
            claim_reason,
            exhausted as i64,
            input.observations,
            serde_json::to_string(&captured).map_err(|_| Error::Database)?,
            now(),
            input.token
        ],
    )?;
    tx.execute(
        "UPDATE message_history_capture_pages SET state='closed' WHERE token=?",
        [&input.token],
    )?;
    for request_id in &requests {
        aggregate(&tx, &owner, generation, request_id)?;
    }
    tx.commit()?;
    drain(conn, ctx, &input.provider_generation)?;
    Ok(json!({"token":input.token,"state":"finished"}).to_string())
}

/// Protocol errors that mean "not yet publishable" rather than "invalid": keys are locked, or the
/// server cannot yet accept compactable control records. The row stays pending for a later drain.
fn retryable(error: &Error) -> bool {
    matches!(
        error,
        Error::KeysUnavailable
            | Error::InvalidRequest(
                "retained control requires a compaction-capable server"
                    | "contact sync is finishing its compaction upgrade"
            )
    )
}

/// Publishes one durable result. Rows whose request is missing, rotated, or past its deadline
/// while still live are dropped. Returns whether the row was published.
fn publish_one(
    conn: &mut Connection,
    ctx: &Ctx<'_>,
    generation: i64,
    request_id: &str,
    result: &str,
) -> Result<bool, Error> {
    let request: Option<(i64, i64, String)> = conn
        .query_row(
            "SELECT generation,expires_at,status FROM message_history_requests WHERE request_id=?",
            [request_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let usable = request.is_some_and(|(request_generation, expires_at, status)| {
        request_generation == generation && (!live(&status) || expires_at >= now())
    });
    if !usable {
        conn.execute(
            "DELETE FROM message_history_capture_publications WHERE request_id=?",
            [request_id],
        )?;
        return Ok(false);
    }
    match message_history_protocol::record_progress(conn, ctx, result) {
        Ok(_) => {
            conn.execute(
                "UPDATE message_history_capture_publications SET published=1 WHERE request_id=?",
                [request_id],
            )?;
            Ok(true)
        }
        Err(error) if retryable(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Retries durable terminal publication without provider access or rescanning. Every row is
/// attempted; the first unexpected failure is returned after the others are processed.
pub(crate) fn drain(
    conn: &mut Connection,
    ctx: &Ctx<'_>,
    provider_generation: &str,
) -> Result<String, Error> {
    let Some(generation) = mapped_generation(conn, provider_generation)? else {
        return Ok(json!({"published":0,"pending":0}).to_string());
    };
    let owner = ctx.device_id.to_string();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    settle(&tx, &owner, Some(generation), None)?;
    tx.commit()?;
    let publications = {
        let mut statement = conn.prepare(
            "SELECT request_id,result FROM message_history_capture_publications WHERE published=0 ORDER BY created_at,request_id",
        )?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut published = 0;
    let mut first_error = None;
    for (request_id, result) in publications {
        match publish_one(conn, ctx, generation, &request_id, &result) {
            Ok(true) => published += 1,
            Ok(false) => {}
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM message_history_capture_publications WHERE published=0",
        [],
        |row| row.get(0),
    )?;
    Ok(json!({"published":published,"pending":pending}).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    const FIRST: &str = "18d0a04f-98e9-4cc2-8faa-e8893e6d00aa";
    const SECOND: &str = "0cc94dc4-ea5e-4df7-9cf6-5cfb0f10cf23";

    #[test]
    fn returning_uuid_receives_a_fresh_generation_and_never_reactivates_the_old_one() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::message_history_protocol::initialize(&conn).unwrap();
        initialize(&conn).unwrap();
        let owner = "phone";
        assert_eq!(activate_generation(&mut conn, owner, FIRST).unwrap(), 1);
        assert_eq!(activate_generation(&mut conn, owner, FIRST).unwrap(), 1);
        assert_eq!(activate_generation(&mut conn, owner, SECOND).unwrap(), 2);
        assert_eq!(mapped_generation(&conn, FIRST).unwrap(), None);
        assert_eq!(mapped_generation(&conn, SECOND).unwrap(), Some(2));
        assert_eq!(activate_generation(&mut conn, owner, FIRST).unwrap(), 3);
        assert_eq!(mapped_generation(&conn, FIRST).unwrap(), Some(3));
    }

    #[test]
    fn retained_source_ahead_of_the_mapping_forces_a_higher_generation() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::message_history_protocol::initialize(&conn).unwrap();
        initialize(&conn).unwrap();
        conn.execute(
            "INSERT INTO message_history_sources(producer,generation,revision,key_epoch,body,updated_at) VALUES('phone',5,1,1,'{}',0)",
            [],
        )
        .unwrap();
        assert_eq!(activate_generation(&mut conn, "phone", FIRST).unwrap(), 6);
    }

    #[test]
    fn non_uuid_provider_is_inert_for_mapping_and_rejected_for_publication() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::message_history_protocol::initialize(&conn).unwrap();
        initialize(&conn).unwrap();
        assert_eq!(mapped_generation(&conn, "42"), Ok(None));
        assert_eq!(
            activate_generation(&mut conn, "phone", "42"),
            Err(Error::InvalidRequest("provider generation"))
        );
    }

    #[test]
    fn finish_reasons_are_allowlisted_and_coverage_is_never_supplied() {
        assert_eq!(supplied_reason(None), Ok(None));
        assert_eq!(
            supplied_reason(Some("permission_denied")),
            Ok(Some("permission_denied"))
        );
        assert!(supplied_reason(Some("budget_exhausted")).is_err());
        assert!(supplied_reason(Some("source_unavailable")).is_err());
    }

    #[test]
    fn outcome_blocks_before_coverage_and_never_exhausts_truncated_or_full_pages() {
        assert_eq!(
            outcome(Some("permission_denied"), true, false, 0),
            ("blocked", Some("permission_denied"))
        );
        assert_eq!(
            outcome(Some("provider_error"), true, false, 0),
            ("failed", Some("provider_error"))
        );
        assert_eq!(
            outcome(None, true, true, 1),
            ("partial", Some("budget_exhausted"))
        );
        assert_eq!(
            outcome(None, true, false, BUDGET),
            ("partial", Some("budget_exhausted"))
        );
        assert_eq!(outcome(None, true, false, 3), ("completed", None));
        assert_eq!(
            outcome(None, false, false, 3),
            ("partial", Some("source_unavailable"))
        );
    }

    #[test]
    fn precedence_prefers_blocked_then_failed_then_partial() {
        assert!(precedence("blocked") > precedence("failed"));
        assert!(precedence("failed") > precedence("partial"));
        assert!(precedence("partial") > precedence("completed"));
    }

    #[test]
    fn receipts_include_only_imported_current_device_matching_transport_after_baseline() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE messages(local_order INTEGER PRIMARY KEY,id TEXT NOT NULL,source_device_id TEXT NOT NULL,source_sequence INTEGER NOT NULL,payload TEXT NOT NULL);",
        )
        .unwrap();
        let local = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let other = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        let rows = [
            (
                1,
                "11111111-1111-4111-8111-111111111111",
                local,
                1,
                r#"{"imported":true,"transport":"sms"}"#,
            ),
            (
                2,
                "22222222-2222-4222-8222-222222222222",
                local,
                2,
                r#"{"imported":false,"transport":"sms"}"#,
            ),
            (
                3,
                "33333333-3333-4333-8333-333333333333",
                local,
                3,
                r#"{"imported":true,"transport":"mms"}"#,
            ),
            (
                4,
                "44444444-4444-4444-8444-444444444444",
                other,
                4,
                r#"{"imported":true,"transport":"sms"}"#,
            ),
            (
                5,
                "55555555-5555-4555-8555-555555555555",
                local,
                5,
                r#"{"imported":true,"transport":"sms"}"#,
            ),
        ];
        for row in rows {
            conn.execute(
                "INSERT INTO messages VALUES(?,?,?,?,?)",
                params![row.0, row.1, row.2, row.3, row.4],
            )
            .unwrap();
        }
        let (captured, truncated) = receipts(&conn, local, 1, "sms").unwrap();
        assert!(!truncated);
        assert_eq!(
            captured,
            vec![json!({"messageId":"55555555-5555-4555-8555-555555555555","revision":"5"})]
        );
    }

    #[test]
    fn receipts_propagate_malformed_payloads_instead_of_hiding_them() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE messages(local_order INTEGER PRIMARY KEY,id TEXT NOT NULL,source_device_id TEXT NOT NULL,source_sequence INTEGER NOT NULL,payload TEXT NOT NULL);
             INSERT INTO messages VALUES(1,'11111111-1111-4111-8111-111111111111','local',1,'not json');",
        )
        .unwrap();
        assert_eq!(receipts(&conn, "local", 0, "sms"), Err(Error::Database));
    }
}

/// Client-level tests: each member is a real SQLCipher client, and every control moves through
/// encrypted envelopes and the real apply path. Admission, claims and publication are asserted
/// through the public begin/finish/drain methods and the durable rows they produce.
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
    const ONE: &str = "18d0a04f-98e9-4cc2-8faa-e8893e6d00aa";
    const TWO: &str = "0cc94dc4-ea5e-4df7-9cf6-5cfb0f10cf23";

    struct Member {
        client: Client,
        device: DeviceId,
    }

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

    fn drain_outbox(from: &Member) -> Vec<Envelope> {
        let envelopes = from.client.pending_outbox().unwrap();
        for envelope in &envelopes {
            from.client.ack_outbox(envelope.envelope_id).unwrap();
        }
        envelopes
    }

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

    fn with_store<T>(member: &Member, run: impl FnOnce(&mut Connection, &Ctx<'_>) -> T) -> T {
        let mut store = member.client.lock().unwrap();
        let (conn, ctx) = store.parts();
        run(conn, &ctx)
    }

    fn value(result: Result<String, Error>) -> Value {
        serde_json::from_str(&result.unwrap()).unwrap()
    }

    fn publish(member: &Member, provider: &str, sms: bool, mms: bool) -> Value {
        value(member.client.publish_message_history_capture_source(
            &json!({"providerGeneration":provider,"sms":sms,"mms":mms,"permission":true,"policy":true,"forceRepublish":false}).to_string(),
        ))
    }

    fn begin(member: &Member, provider: &str, transport: &str) -> Value {
        value(
            member
                .client
                .begin_message_history_capture(provider, transport),
        )
    }

    fn token_of(begun: &Value) -> String {
        begun["token"].as_str().unwrap().to_owned()
    }

    fn finish(
        member: &Member,
        provider: &str,
        token: &str,
        observations: i64,
        exhausted: bool,
        reason: Option<&str>,
    ) -> Result<Value, Error> {
        let input = json!({"providerGeneration":provider,"token":token,"observations":observations,"exhausted":exhausted,"reason":reason}).to_string();
        member
            .client
            .finish_message_history_capture(&input)
            .map(|output| serde_json::from_str(&output).unwrap())
    }

    /// Source reaches the requester, the requester enqueues, and the phone admits the request.
    fn admit(desktop: &Member, phone: &Member) -> String {
        ship(&drain_outbox(phone), desktop);
        let input = json!({"target_device_id": phone.device.to_string()}).to_string();
        let request = with_store(desktop, |conn, ctx| {
            value(message_history_protocol::request(conn, ctx, &input))
        });
        ship(&drain_outbox(desktop), phone);
        request["requestId"].as_str().unwrap().to_owned()
    }

    fn insert_message(
        member: &Member,
        device: &str,
        transport: &str,
        imported: bool,
        payload: Option<&str>,
    ) {
        let payload = payload.map_or_else(
            || json!({"imported":imported,"transport":transport}).to_string(),
            str::to_owned,
        );
        with_store(member, |conn, _| {
            conn.execute(
                "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen) VALUES(?,?,?,1,?,0)",
                params![Uuid::new_v4().to_string(), Uuid::new_v4().to_string(), device, payload],
            )
            .unwrap();
        });
    }

    fn request_status(member: &Member, id: &str) -> String {
        with_store(member, |conn, _| {
            conn.query_row(
                "SELECT status FROM message_history_requests WHERE request_id=?",
                [id],
                |row| row.get(0),
            )
            .unwrap()
        })
    }

    /// Receipt projection stored by the protocol: `{receipts, sourceExhausted, nextProgressToken}`.
    fn request_result(member: &Member, id: &str) -> Value {
        with_store(member, |conn, _| {
            let raw: Option<String> = conn
                .query_row(
                    "SELECT result FROM message_history_requests WHERE request_id=?",
                    [id],
                    |row| row.get(0),
                )
                .unwrap();
            serde_json::from_str(&raw.expect("terminal result stored")).unwrap()
        })
    }

    fn request_reason(member: &Member, id: &str) -> Option<String> {
        with_store(member, |conn, _| {
            conn.query_row(
                "SELECT reason FROM message_history_requests WHERE request_id=?",
                [id],
                |row| row.get(0),
            )
            .unwrap()
        })
    }

    fn claim(
        member: &Member,
        id: &str,
        transport: &str,
    ) -> (String, Option<String>, Option<String>) {
        with_store(member, |conn, _| {
            conn.query_row(
                "SELECT state,reason,token FROM message_history_capture_page_claims WHERE request_id=? AND transport=?",
                params![id, transport],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
        })
    }

    fn count(member: &Member, sql: &str) -> i64 {
        with_store(member, |conn, _| {
            conn.query_row(sql, [], |row| row.get(0)).unwrap()
        })
    }

    fn set_page_lease(member: &Member, token: &str, claimed_at: i64) {
        with_store(member, |conn, _| {
            conn.execute(
                "UPDATE message_history_capture_pages SET claimed_at=? WHERE token=?",
                params![claimed_at, token],
            )
            .unwrap();
        });
    }

    #[test]
    fn two_transports_each_claim_once_and_publish_one_completed_result() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, ONE, true, true);
        let id = admit(&desktop, &phone);
        complete_history(&phone, ONE, "sms");
        complete_history(&phone, ONE, "mms");
        let sms = begin(&phone, ONE, "sms");
        assert_eq!(sms["budget"], BUDGET);
        let sms_token = token_of(&sms);
        insert_message(&phone, &phone.device.to_string(), "sms", true, None);
        finish(&phone, ONE, &sms_token, 1, true, None).unwrap();
        assert_eq!(request_status(&phone, &id), "waiting");
        let mms = begin(&phone, ONE, "mms");
        let mms_token = token_of(&mms);
        insert_message(&phone, &phone.device.to_string(), "mms", true, None);
        finish(&phone, ONE, &mms_token, 0, true, None).unwrap();
        assert_eq!(request_status(&phone, &id), "completed");
        assert_eq!(request_reason(&phone, &id), None);
        let result = request_result(&phone, &id);
        assert_eq!(result["sourceExhausted"], true);
        assert_eq!(result["receipts"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_partial_transport_keeps_the_request_partial_even_when_the_other_is_exhausted() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, ONE, true, true);
        let id = admit(&desktop, &phone);
        let sms_token = token_of(&begin(&phone, ONE, "sms"));
        finish(&phone, ONE, &sms_token, BUDGET, false, None).unwrap();
        let mms_token = token_of(&begin(&phone, ONE, "mms"));
        finish(&phone, ONE, &mms_token, 0, true, None).unwrap();
        assert_eq!(request_status(&phone, &id), "partial");
        assert_eq!(
            request_reason(&phone, &id).as_deref(),
            Some("budget_exhausted")
        );
        assert_eq!(request_result(&phone, &id)["sourceExhausted"], false);
    }

    #[test]
    fn blocked_permission_outranks_a_provider_failure_in_either_order() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, ONE, true, true);
        let id = admit(&desktop, &phone);
        let mms_token = token_of(&begin(&phone, ONE, "mms"));
        finish(&phone, ONE, &mms_token, 0, false, Some("provider_error")).unwrap();
        let sms_token = token_of(&begin(&phone, ONE, "sms"));
        finish(&phone, ONE, &sms_token, 0, false, Some("permission_denied")).unwrap();
        assert_eq!(request_status(&phone, &id), "blocked");
        assert_eq!(
            request_reason(&phone, &id).as_deref(),
            Some("permission_denied")
        );
        assert_eq!(request_result(&phone, &id)["sourceExhausted"], false);
    }

    #[test]
    fn concurrent_begin_while_leased_returns_null_and_duplicate_finish_regrants_nothing() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, ONE, true, false);
        let id = admit(&desktop, &phone);
        let token = token_of(&begin(&phone, ONE, "sms"));
        assert_eq!(begin(&phone, ONE, "sms"), Value::Null);
        finish(&phone, ONE, &token, 2, false, None).unwrap();
        let before = claim(&phone, &id, "sms");
        assert_eq!(
            finish(&phone, ONE, &token, 2, false, None).unwrap()["state"],
            "finished"
        );
        assert_eq!(claim(&phone, &id, "sms"), before);
        assert_eq!(begin(&phone, ONE, "sms"), Value::Null);
        assert_eq!(
            count(&phone, "SELECT COUNT(*) FROM message_history_capture_pages"),
            1
        );
    }

    #[test]
    fn later_request_gets_its_own_page_while_earlier_claims_stay_bound_to_theirs() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, ONE, true, false);
        let first = admit(&desktop, &phone);
        let first_token = token_of(&begin(&phone, ONE, "sms"));
        // A requester coalesces its own live request, so the later request comes from another device.
        let other = member(&dir, "other", &vault);
        value(phone.client.publish_message_history_capture_source(
            &json!({"providerGeneration":ONE,"sms":true,"mms":false,"permission":true,"policy":true,"forceRepublish":true}).to_string(),
        ));
        let second = admit(&other, &phone);
        assert_eq!(begin(&phone, ONE, "sms"), Value::Null);
        finish(&phone, ONE, &first_token, 0, true, None).unwrap();
        let second_token = token_of(&begin(&phone, ONE, "sms"));
        assert_ne!(second_token, first_token);
        assert_eq!(
            claim(&phone, &first, "sms").2.as_deref(),
            Some(first_token.as_str())
        );
        assert_eq!(
            claim(&phone, &second, "sms").2.as_deref(),
            Some(second_token.as_str())
        );
    }

    #[test]
    fn abandoned_lease_settles_partial_and_grants_no_new_allowance() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, ONE, true, false);
        let id = admit(&desktop, &phone);
        let token = token_of(&begin(&phone, ONE, "sms"));
        set_page_lease(&phone, &token, 0);
        assert_eq!(begin(&phone, ONE, "sms"), Value::Null);
        assert_eq!(
            claim(&phone, &id, "sms"),
            (
                "partial".into(),
                Some("source_unavailable".into()),
                Some(token.clone())
            )
        );
        assert_eq!(begin(&phone, ONE, "sms"), Value::Null);
        assert_eq!(request_status(&phone, &id), "waiting");
        assert_eq!(
            value(phone.client.drain_message_history_capture_results(ONE)),
            json!({"published":1,"pending":0})
        );
        assert_eq!(request_status(&phone, &id), "partial");
        assert_eq!(
            finish(&phone, ONE, &token, 5, true, None).unwrap()["state"],
            "finished"
        );
        assert_eq!(
            request_reason(&phone, &id).as_deref(),
            Some("source_unavailable")
        );
    }

    #[test]
    fn sql_filters_run_before_the_page_limit_so_other_transports_cannot_hide_matches() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, ONE, true, false);
        let id = admit(&desktop, &phone);
        let token = token_of(&begin(&phone, ONE, "sms"));
        let local = phone.device.to_string();
        complete_history(&phone, ONE, "sms");
        for _ in 0..120 {
            insert_message(&phone, &local, "mms", true, None);
        }
        for _ in 0..3 {
            insert_message(&phone, &local, "sms", false, None);
        }
        for _ in 0..5 {
            insert_message(&phone, &local, "sms", true, None);
        }
        insert_message(&phone, &DeviceId::new().to_string(), "sms", true, None);
        finish(&phone, ONE, &token, 0, true, None).unwrap();
        assert_eq!(request_status(&phone, &id), "completed");
        assert_eq!(
            request_result(&phone, &id)["receipts"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
    }

    #[test]
    fn more_than_one_budget_of_matching_receipts_is_truncated_partial_never_complete() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, ONE, true, false);
        let id = admit(&desktop, &phone);
        let token = token_of(&begin(&phone, ONE, "sms"));
        let local = phone.device.to_string();
        complete_history(&phone, ONE, "sms");
        for _ in 0..150 {
            insert_message(&phone, &local, "sms", true, None);
        }
        finish(&phone, ONE, &token, 0, true, None).unwrap();
        let result = request_result(&phone, &id);
        assert_eq!(request_status(&phone, &id), "partial");
        assert_eq!(
            request_reason(&phone, &id).as_deref(),
            Some("budget_exhausted")
        );
        assert_eq!(
            result["receipts"].as_array().unwrap().len(),
            BUDGET as usize
        );
    }

    #[test]
    fn expired_request_is_never_completed_over_its_deadline() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, ONE, true, false);
        let id = admit(&desktop, &phone);
        let token = token_of(&begin(&phone, ONE, "sms"));
        with_store(&phone, |conn, _| {
            conn.execute(
                "UPDATE message_history_requests SET expires_at=0 WHERE request_id=?",
                [&id],
            )
            .unwrap();
        });
        finish(&phone, ONE, &token, 0, true, None).unwrap();
        assert_eq!(request_status(&phone, &id), "waiting");
        assert_eq!(
            count(
                &phone,
                "SELECT COUNT(*) FROM message_history_capture_publications"
            ),
            0
        );
    }

    #[test]
    fn cancelled_request_publishes_nothing_on_finish() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, ONE, true, false);
        let id = admit(&desktop, &phone);
        let token = token_of(&begin(&phone, ONE, "sms"));
        with_store(&phone, |conn, _| {
            conn.execute("UPDATE message_history_requests SET status='cancelled',reason='cancelled' WHERE request_id=?", [&id])
                .unwrap();
        });
        finish(&phone, ONE, &token, 0, true, None).unwrap();
        assert_eq!(request_status(&phone, &id), "cancelled");
        assert_eq!(
            count(
                &phone,
                "SELECT COUNT(*) FROM message_history_capture_publications"
            ),
            0
        );
    }

    #[test]
    fn policy_pending_publication_is_retained_then_drains_without_rescanning() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, ONE, true, false);
        let id = admit(&desktop, &phone);
        let token = token_of(&begin(&phone, ONE, "sms"));
        complete_history(&phone, ONE, "sms");
        phone
            .client
            .set_server_compaction_state(false, true)
            .unwrap();
        finish(&phone, ONE, &token, 0, true, None).unwrap();
        assert_eq!(request_status(&phone, &id), "waiting");
        let pages = count(&phone, "SELECT COUNT(*) FROM message_history_capture_pages");
        let claims = count(
            &phone,
            "SELECT COUNT(*) FROM message_history_capture_page_claims",
        );
        phone
            .client
            .set_server_compaction_state(true, true)
            .unwrap();
        let drained = value(phone.client.drain_message_history_capture_results(ONE));
        assert_eq!(drained, json!({"published":1,"pending":0}));
        assert_eq!(request_status(&phone, &id), "completed");
        assert_eq!(
            count(&phone, "SELECT COUNT(*) FROM message_history_capture_pages"),
            pages
        );
        assert_eq!(
            count(
                &phone,
                "SELECT COUNT(*) FROM message_history_capture_page_claims"
            ),
            claims
        );
    }

    #[test]
    fn unexpected_publication_failure_propagates_then_retries_after_repair() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, ONE, true, false);
        let id = admit(&desktop, &phone);
        let broken = json!({"schema_version":1,"request_id":id,"target":phone.device.to_string(),"generation":"1","key_epoch":1,"state":"bogus","reason":null,"receipts":[],"source_exhausted":false,"next_progress_token":null});
        with_store(&phone, |conn, _| {
            conn.execute(
                "INSERT INTO message_history_capture_publications(request_id,result,published,created_at) VALUES(?,?,0,?)",
                params![id, broken.to_string(), now()],
            )
            .unwrap();
        });
        assert!(
            phone
                .client
                .drain_message_history_capture_results(ONE)
                .is_err()
        );
        assert_eq!(
            count(
                &phone,
                "SELECT COUNT(*) FROM message_history_capture_publications WHERE published=0"
            ),
            1
        );
        let repaired = json!({"schema_version":1,"request_id":id,"target":phone.device.to_string(),"generation":"1","key_epoch":1,"state":"partial","reason":"source_unavailable","receipts":[],"source_exhausted":false,"next_progress_token":null});
        with_store(&phone, |conn, _| {
            conn.execute(
                "UPDATE message_history_capture_publications SET result=? WHERE request_id=?",
                params![repaired.to_string(), id],
            )
            .unwrap();
        });
        assert_eq!(
            value(phone.client.drain_message_history_capture_results(ONE)),
            json!({"published":1,"pending":0})
        );
        assert_eq!(request_status(&phone, &id), "partial");
    }

    #[test]
    fn publication_for_a_missing_request_is_dropped_without_failing_drain() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        publish(&phone, ONE, true, false);
        with_store(&phone, |conn, _| {
            conn.execute(
                "INSERT INTO message_history_capture_publications(request_id,result,published,created_at) VALUES(?,'{}',0,?)",
                params![Uuid::new_v4().to_string(), now()],
            )
            .unwrap();
        });
        assert_eq!(
            value(phone.client.drain_message_history_capture_results(ONE)),
            json!({"published":0,"pending":0})
        );
    }

    fn pending_view(member: &Member, provider: &str) -> Value {
        value(
            member
                .client
                .pending_message_history_capture_requests(provider),
        )
    }

    fn needs(member: &Member, provider: &str) -> (bool, bool) {
        let view = pending_view(member, provider);
        (
            view["needsSms"] == json!(true),
            view["needsMms"] == json!(true),
        )
    }

    /// Republishes the same facts with `forceRepublish`, so the new requester receives the source.
    fn republish(member: &Member, provider: &str) -> Value {
        value(member.client.publish_message_history_capture_source(
            &json!({"providerGeneration":provider,"sms":true,"mms":true,"permission":true,"policy":true,"forceRepublish":true}).to_string(),
        ))
    }

    /// This device's retained source body, as the protocol stores it.
    fn stored_source(member: &Member) -> Option<Value> {
        with_store(member, |conn, _| {
            conn.query_row(
                "SELECT body FROM message_history_sources WHERE producer=?",
                [member.device.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .unwrap()
            .map(|body| serde_json::from_str(&body).unwrap())
        })
    }

    #[test]
    fn absent_or_legacy_provider_mapping_is_inert_and_finish_still_rejects_tokens() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let idle = json!({"requests":[],"needsSms":false,"needsMms":false});
        for provider in [ONE, "legacy-provider-string"] {
            assert_eq!(begin(&phone, provider, "sms"), Value::Null);
            assert_eq!(begin(&phone, provider, "mms"), Value::Null);
            assert_eq!(pending_view(&phone, provider), idle);
            assert_eq!(
                value(phone.client.drain_message_history_capture_results(provider)),
                json!({"published":0,"pending":0})
            );
        }
        assert!(finish(&phone, ONE, &Uuid::new_v4().to_string(), 0, true, None).is_err());
        assert!(phone
            .client
            .publish_message_history_capture_source(
                &json!({"providerGeneration":"legacy-provider-string","sms":true,"mms":false,"permission":true,"policy":true,"forceRepublish":false}).to_string(),
            )
            .is_err());
        publish(&phone, TWO, true, false);
        assert_eq!(begin(&phone, ONE, "sms"), Value::Null);
        assert_eq!(pending_view(&phone, ONE), idle);
    }

    #[test]
    fn pending_needs_follow_unclaimed_eligibility_and_leases_not_live_requests() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        let other = member(&dir, "other", &vault);
        publish(&phone, ONE, true, true);
        assert_eq!(needs(&phone, ONE), (false, false));
        let first = admit(&desktop, &phone);
        assert_eq!(needs(&phone, ONE), (true, true));
        let sms_token = token_of(&begin(&phone, ONE, "sms"));
        assert_eq!(needs(&phone, ONE), (false, true), "sms lease is open");
        finish(&phone, ONE, &sms_token, 0, false, None).unwrap();
        assert_eq!(
            needs(&phone, ONE),
            (false, true),
            "claimed live request is not sms-ready"
        );
        assert_eq!(claim(&phone, &first, "sms").0, "partial");
        republish(&phone, ONE);
        let second = admit(&other, &phone);
        assert_eq!(
            needs(&phone, ONE),
            (true, true),
            "new request is ready on both"
        );
        let second_sms = token_of(&begin(&phone, ONE, "sms"));
        assert_eq!(
            claim(&phone, &second, "sms").2.as_deref(),
            Some(second_sms.as_str())
        );
        assert_eq!(needs(&phone, ONE), (false, true));
        let mms_token = token_of(&begin(&phone, ONE, "mms"));
        assert_eq!(needs(&phone, ONE), (false, false));
        finish(&phone, ONE, &mms_token, 0, false, None).unwrap();
        assert_eq!(claim(&phone, &first, "mms").0, "partial");
    }

    #[test]
    fn exhaustion_in_the_source_is_derived_from_history_metadata_not_the_host() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        publish(&phone, ONE, true, true);
        assert_eq!(stored_source(&phone).unwrap()["exhausted"], false);
        complete_history(&phone, ONE, "sms");
        assert_eq!(
            publish(&phone, ONE, true, true)["published"],
            false,
            "one open transport keeps the source unexhausted"
        );
        complete_history(&phone, ONE, "mms");
        assert_eq!(publish(&phone, ONE, true, true)["published"], true);
        assert_eq!(stored_source(&phone).unwrap()["exhausted"], true);
    }

    #[test]
    fn withdrawing_every_transport_keeps_capabilities_and_denies_admitted_work() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        let fresh = member(&dir, "fresh", &vault);
        publish(&phone, ONE, true, true);
        let id = admit(&desktop, &phone);
        let withdrawn = value(phone.client.publish_message_history_capture_source(
            &json!({"providerGeneration":ONE,"sms":false,"mms":false,"permission":true,"policy":true,"forceRepublish":false}).to_string(),
        ));
        assert_eq!(withdrawn["published"], true);
        let source = stored_source(&phone).unwrap();
        assert_eq!(source["capabilities"], json!({"sms":true,"mms":true}));
        assert_eq!(source["permission"], false);
        assert_eq!(source["policy"], false);
        assert_eq!(pending_view(&phone, ONE)["requests"], json!([]));
        assert_eq!(request_status(&phone, &id), "blocked");
        let inert = value(fresh.client.publish_message_history_capture_source(
            &json!({"providerGeneration":ONE,"sms":false,"mms":false,"permission":true,"policy":true,"forceRepublish":false}).to_string(),
        ));
        assert_eq!(
            inert,
            json!({"producer":fresh.device.to_string(),"published":false})
        );
        assert!(stored_source(&fresh).is_none());
    }

    /// Durable evidence that `transport` history for `provider` is exhausted (the Rust-owned
    /// checkpoint an exhausted finish must match).
    fn complete_history(member: &Member, provider: &str, transport: &str) {
        with_store(member, |conn, _| {
            match transport {
            "sms" => conn
                .execute(
                    "INSERT INTO sms_scan_checkpoints(source_generation,subscription_id,imported,provider_message_id,history_complete,paused_reason,provider_error) VALUES(?,'sub',1,'',1,NULL,0)",
                    [provider],
                )
                .unwrap(),
            _ => conn
                .execute(
                    "INSERT INTO mms_backfill_scopes(source_generation,subscription_id,history_complete,pause_reason,migration_done) VALUES(?,'sub',1,NULL,1)",
                    [provider],
                )
                .unwrap(),
        }
        });
    }

    /// Imported acquisition of `provider` in the given state, as the SMS or MMS history path writes it.
    fn imported_acquisition(member: &Member, provider: &str, transport: &str, state: &str) {
        with_store(member, |conn, _| {
            let sql = match transport {
                "sms" => {
                    "INSERT INTO sms_acquisitions(acquisition_id,source_generation,subscription_id,provider_message_id,source_row_fingerprint,direction,conversation_id,input,state,reason,message_id,source_sequence,local_order) VALUES(?,?,'sub','p','fp','incoming',?,'{\"imported\":true}',?,NULL,?,1,1)"
                }
                _ => {
                    "INSERT INTO mms_acquisitions(acquisition_id,source_generation,subscription_id,provider_message_id,direction,conversation_id,input,state,reason,message_id,source_sequence,local_order) VALUES(?,?,'sub','p','incoming',?,'{\"imported\":true}',?,NULL,?,1,1)"
                }
            };
            conn.execute(
                sql,
                params![
                    Uuid::new_v4().to_string(),
                    provider,
                    Uuid::new_v4().to_string(),
                    state,
                    Uuid::new_v4().to_string()
                ],
            )
            .unwrap();
        });
    }

    /// Finishes one exhausted page after `setup` shapes the durable history evidence, and returns the
    /// request's status and reason once the finish has drained.
    fn exhausted_outcome(transport: &str, setup: impl Fn(&Member)) -> (String, Option<String>) {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        if transport == "sms" {
            publish(&phone, ONE, true, false);
        } else {
            publish(&phone, ONE, false, true);
        }
        let id = admit(&desktop, &phone);
        let token = token_of(&begin(&phone, ONE, transport));
        setup(&phone);
        finish(&phone, ONE, &token, 0, true, None).unwrap();
        (request_status(&phone, &id), request_reason(&phone, &id))
    }

    #[test]
    fn exhaustion_publishes_completed_only_with_durable_history_evidence() {
        let sms_proven = |member: &Member| {
            complete_history(member, ONE, "sms");
        };
        // The caller's `exhausted:true` is necessary but not sufficient: each negative case keeps
        // one piece of durable evidence open and must publish partial coverage instead.
        for (label, setup) in [
            (
                "no checkpoint",
                Box::new(|_: &Member| {}) as Box<dyn Fn(&Member)>,
            ),
            (
                "open checkpoint",
                Box::new(|member: &Member| {
                    complete_history(member, ONE, "sms");
                    with_store(member, |conn, _| {
                        conn.execute("UPDATE sms_scan_checkpoints SET history_complete=0", [])
                            .unwrap();
                    });
                }),
            ),
            (
                "paused checkpoint",
                Box::new(|member: &Member| {
                    complete_history(member, ONE, "sms");
                    with_store(member, |conn, _| {
                        conn.execute(
                            "UPDATE sms_scan_checkpoints SET paused_reason='recovery_capacity'",
                            [],
                        )
                        .unwrap();
                    });
                }),
            ),
            (
                "errored checkpoint",
                Box::new(|member: &Member| {
                    complete_history(member, ONE, "sms");
                    with_store(member, |conn, _| {
                        conn.execute("UPDATE sms_scan_checkpoints SET provider_error=1", [])
                            .unwrap();
                    });
                }),
            ),
            (
                "pending imported acquisition",
                Box::new(|member: &Member| {
                    complete_history(member, ONE, "sms");
                    imported_acquisition(member, ONE, "sms", "pending");
                }),
            ),
            (
                "unavailable imported acquisition",
                Box::new(|member: &Member| {
                    complete_history(member, ONE, "sms");
                    imported_acquisition(member, ONE, "sms", "unavailable");
                }),
            ),
        ] {
            assert_eq!(
                exhausted_outcome("sms", setup),
                ("partial".into(), Some("source_unavailable".into())),
                "sms case: {label}"
            );
        }
        assert_eq!(
            exhausted_outcome("sms", sms_proven),
            ("completed".into(), None),
            "sms proven"
        );
        for (label, setup) in [
            (
                "mms no scope",
                Box::new(|_: &Member| {}) as Box<dyn Fn(&Member)>,
            ),
            (
                "mms open scope",
                Box::new(|member: &Member| {
                    complete_history(member, ONE, "mms");
                    with_store(member, |conn, _| {
                        conn.execute("UPDATE mms_backfill_scopes SET history_complete=0", [])
                            .unwrap();
                    });
                }),
            ),
            (
                "mms pending imported acquisition",
                Box::new(|member: &Member| {
                    complete_history(member, ONE, "mms");
                    imported_acquisition(member, ONE, "mms", "pending");
                }),
            ),
        ] {
            assert_eq!(
                exhausted_outcome("mms", setup),
                ("partial".into(), Some("source_unavailable".into())),
                "mms case: {label}"
            );
        }
        assert_eq!(
            exhausted_outcome("mms", |member| complete_history(member, ONE, "mms")),
            ("completed".into(), None),
            "mms proven"
        );
    }

    #[test]
    fn rotation_blocks_open_pages_rejects_the_old_token_and_returning_uuid_gets_a_fresh_number() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        let first = publish(&phone, ONE, true, false);
        assert_eq!(first["generation"], "1");
        let id = admit(&desktop, &phone);
        let old_token = token_of(&begin(&phone, ONE, "sms"));
        let second = publish(&phone, TWO, true, false);
        assert_eq!(second["generation"], "2");
        assert_eq!(begin(&phone, TWO, "sms"), Value::Null);
        assert_eq!(
            claim(&phone, &id, "sms"),
            (
                "blocked".into(),
                Some("generation_changed".into()),
                Some(old_token.clone())
            )
        );
        assert!(finish(&phone, ONE, &old_token, 0, true, None).is_err());
        let returned = publish(&phone, ONE, true, false);
        assert_eq!(returned["generation"], "3");
        assert_eq!(returned["published"], true);
    }

    #[test]
    fn missing_mapping_with_retained_source_publishes_a_higher_generation() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        assert_eq!(publish(&phone, ONE, true, false)["generation"], "1");
        with_store(&phone, |conn, _| {
            conn.execute("DELETE FROM message_history_capture_mappings", [])
                .unwrap();
        });
        let republished = publish(&phone, ONE, true, false);
        assert_eq!(republished["generation"], "2");
        assert_eq!(republished["published"], true);
    }

    #[test]
    fn unadvertised_transport_is_never_admitted() {
        let dir = TempDir::new().unwrap();
        let vault = vault();
        let phone = member(&dir, "phone", &vault);
        let desktop = member(&dir, "desktop", &vault);
        publish(&phone, ONE, true, false);
        admit(&desktop, &phone);
        assert_eq!(begin(&phone, ONE, "mms"), Value::Null);
        assert_eq!(
            count(&phone, "SELECT COUNT(*) FROM message_history_capture_pages"),
            0
        );
    }
}
