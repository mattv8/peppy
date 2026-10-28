//! Durable, query-free coordination for one bounded history-search attempt.
//!
//! Hosts only execute returned work. `RequestPhone` is an already-enqueued protocol event,
//! and status reads never write. Every transition is derived from core-owned state: the
//! protocol request projection, receipt rows, and journal, snapshot, projection, key and
//! search-backfill readiness. Hosts supply only a replay proof, never receipt or index flags.
use crate::{Client, Ctx, Error, compaction_readiness, message_history_protocol};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

const ATTEMPT_MS: i64 = 60_000;
const CATCH_UP_MS: i64 = 20_000;
const RETENTION_MS: i64 = 24 * 60 * 60 * 1000;
const MAX_ACTIVE_ATTEMPTS: i64 = 8;
const MAX_SOURCES: usize = 4;
const INDEX_WORK_LIMIT: usize = 100;
/// Mirrors the protocol's per-target active-request cap. Protocol refusals are classified as
/// capacity only when this many requests are live; any other refusal is unsupported.
const PROTOCOL_ACTIVE_REQUEST_CAP: i64 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySearchState {
    Idle,
    CatchingUp,
    WaitingForPhone,
    Indexing,
    Partial,
    Completed,
    Blocked,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistorySearchReason {
    Offline,
    Permission,
    Capacity,
    NeedsUnlock,
    Unsupported,
    Timeout,
}

/// Captured immediately before a replay HTTP request, from trusted worker/native code only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySearchSyncContext {
    pub started_at_ms: i64,
    pub snapshot_generation: i64,
}

/// Proof that one replay page was fetched after the attempt started. `next_after_present`
/// distinguishes an old server (field absent) from an explicit terminal `next_after: null`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySearchReplayProof {
    pub context: HistorySearchSyncContext,
    /// Canonical decimal cursor in `0..=i64::MAX`, without leading zeros. `0` is the valid
    /// high water of an empty server.
    pub high_water_cursor: String,
    pub next_after_present: bool,
    pub terminal_next_after: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySearchSyncFacts {
    /// `None` refreshes local progress against the previously accepted proof; it never
    /// fabricates a new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<HistorySearchReplayProof>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistorySearchStatus {
    pub attempt_id: Option<Uuid>,
    pub state: HistorySearchState,
    pub reason: Option<HistorySearchReason>,
    pub can_search_further: bool,
    pub progress_version: String,
    pub index_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistorySearchWork {
    WakeSync,
    AdvanceIndex {
        limit: usize,
    },
    /// The request has already been written to the encrypted outbox. Hosts wake their
    /// normal phone/sync path; they must not enqueue another request.
    RequestPhone {
        request_id: String,
        target_device_id: String,
    },
    Wait,
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}

fn parse_state(value: &str) -> HistorySearchState {
    match value {
        "catching_up" => HistorySearchState::CatchingUp,
        "waiting_for_phone" => HistorySearchState::WaitingForPhone,
        "indexing" => HistorySearchState::Indexing,
        "partial" => HistorySearchState::Partial,
        "completed" => HistorySearchState::Completed,
        "blocked" => HistorySearchState::Blocked,
        "failed" => HistorySearchState::Failed,
        "cancelled" => HistorySearchState::Cancelled,
        _ => HistorySearchState::Idle,
    }
}

fn reason_name(value: Option<&str>) -> Option<&'static str> {
    match value? {
        "offline" => Some("offline"),
        "permission" => Some("permission"),
        "capacity" => Some("capacity"),
        "needs_unlock" => Some("needs_unlock"),
        "unsupported" => Some("unsupported"),
        "timeout" => Some("timeout"),
        _ => None,
    }
}

/// Boundary mapping from protocol wire reasons (the `reason_ok` vocabulary) to coordinator
/// reasons. Distinct from `reason_name`, which parses only stored canonical reasons. Wire
/// values without a coordinator reason map to `None`, so the failure stays generic.
fn wire_reason(wire: Option<&str>) -> Option<&'static str> {
    match wire? {
        "permission_denied" => Some("permission"),
        "policy_denied" => Some("capacity"),
        "capacity" => Some("capacity"),
        "key_epoch_changed" => Some("needs_unlock"),
        "generation_changed" | "source_revoked" => Some("unsupported"),
        "expired" => Some("timeout"),
        // Budget, source unavailability, source exhaustion, and provider, storage, or internal
        // failures carry no coordinator reason.
        _ => None,
    }
}

fn parse_reason(value: &str) -> Option<HistorySearchReason> {
    match value {
        "offline" => Some(HistorySearchReason::Offline),
        "permission" => Some(HistorySearchReason::Permission),
        "capacity" => Some(HistorySearchReason::Capacity),
        "needs_unlock" => Some(HistorySearchReason::NeedsUnlock),
        "unsupported" => Some(HistorySearchReason::Unsupported),
        "timeout" => Some(HistorySearchReason::Timeout),
        _ => None,
    }
}

fn terminal(state: &str) -> bool {
    matches!(
        state,
        "completed" | "partial" | "blocked" | "failed" | "cancelled"
    )
}

fn canonical_cursor(value: &str) -> Option<i64> {
    let parsed: u64 = value.parse().ok()?;
    (parsed.to_string() == value && (0..=i64::MAX as u64).contains(&parsed))
        .then_some(parsed as i64)
}

/// Core-derived environment for one call. `keys_ready` means the active epoch is unlocked.
struct Env {
    requester: String,
    epoch: Option<u32>,
    keys_ready: bool,
}

impl Env {
    fn new(ctx: &Ctx<'_>) -> Self {
        Self {
            requester: ctx.device_id.to_string(),
            epoch: ctx.active_epoch,
            keys_ready: ctx
                .active_epoch
                .is_some_and(|epoch| ctx.keys.contains_key(&epoch)),
        }
    }
}

/// One retained advertisement, read through the protocol projection (never raw rows).
struct SourceView {
    producer: String,
    capable: bool,
    current_epoch: bool,
    permitted: bool,
    exhausted: bool,
    generation: i64,
    key_epoch: u32,
    revision: i64,
}

impl SourceView {
    fn capable_now(&self) -> bool {
        self.capable && self.current_epoch
    }
    fn permitted_now(&self) -> bool {
        self.capable_now() && self.permitted
    }
    fn open(&self) -> bool {
        self.permitted_now() && !self.exhausted
    }
}

fn source_views(conn: &Connection, env: &Env) -> Result<Vec<SourceView>, Error> {
    let value: Value = serde_json::from_str(&message_history_protocol::sources(conn, "{}")?)
        .map_err(|_| Error::Database)?;
    Ok(value["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|source| {
            let caps = &source["capabilities"];
            Some(SourceView {
                producer: source["producer"].as_str()?.to_owned(),
                capable: caps["sms"].as_bool() == Some(true) || caps["mms"].as_bool() == Some(true),
                current_epoch: env
                    .epoch
                    .is_some_and(|epoch| source["keyEpoch"].as_u64() == Some(u64::from(epoch))),
                permitted: source["permission"].as_bool() == Some(true)
                    && source["policy"].as_bool() == Some(true),
                exhausted: source["exhausted"].as_bool() == Some(true),
                generation: source["generation"]
                    .as_str()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(-1),
                key_epoch: source["keyEpoch"]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(u32::MAX),
                revision: source["revision"]
                    .as_str()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(-1),
            })
        })
        .collect())
}

/// Stable device order, rotated by the attempt's durable rotation.
fn selection(views: &[SourceView], rotation: i64) -> Vec<String> {
    let mut ids: Vec<String> = views
        .iter()
        .filter(|view| view.open())
        .map(|view| view.producer.clone())
        .collect();
    ids.sort();
    if !ids.is_empty() {
        let offset = usize::try_from(rotation.rem_euclid(ids.len() as i64)).unwrap_or(0);
        ids.rotate_left(offset);
    }
    ids
}

struct AttemptRow {
    id: String,
    state: String,
    reason: Option<&'static str>,
    started_at_ms: i64,
    deadline_ms: i64,
    catchup_deadline_ms: i64,
    rotation: i64,
    progress_version: i64,
    proof_high_water: Option<i64>,
    proof_generation: Option<i64>,
}

fn load(conn: &Connection, id: Uuid) -> Result<Option<AttemptRow>, Error> {
    Ok(conn
        .query_row(
            "SELECT state,reason,started_at_ms,deadline_ms,catchup_deadline_ms,rotation,progress_version,proof_high_water,proof_generation FROM message_history_attempts WHERE attempt_id=?",
            [id.to_string()],
            |row| {
                Ok(AttemptRow {
                    id: id.to_string(),
                    state: row.get(0)?,
                    reason: reason_name(row.get::<_, Option<String>>(1)?.as_deref()),
                    started_at_ms: row.get(2)?,
                    deadline_ms: row.get(3)?,
                    catchup_deadline_ms: row.get(4)?,
                    rotation: row.get(5)?,
                    progress_version: row.get(6)?,
                    proof_high_water: row.get(7)?,
                    proof_generation: row.get(8)?,
                })
            },
        )
        .optional()?)
}

fn snapshot_generation(conn: &Connection) -> Result<i64, Error> {
    Ok(conn
        .query_row(
            "SELECT v FROM metadata WHERE k='snapshot_generation'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|value| value.parse().ok())
        .unwrap_or(0))
}

fn search_remaining(conn: &Connection) -> Result<i64, Error> {
    Ok(conn
        .query_row(
            "SELECT CAST(v AS INTEGER) FROM metadata WHERE k='search_remaining'",
            [],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// Accepts a replay proof only when it was captured after this attempt started and against
/// the current snapshot generation. Returns the stored `(high_water, generation)`.
fn accept_proof(
    conn: &Connection,
    row: &AttemptRow,
    proof: &HistorySearchReplayProof,
) -> Result<Option<(i64, i64)>, Error> {
    if !(proof.next_after_present && proof.terminal_next_after) {
        return Ok(None);
    }
    let Some(high) = canonical_cursor(&proof.high_water_cursor) else {
        return Ok(None);
    };
    if proof.context.started_at_ms < row.started_at_ms {
        return Ok(None);
    }
    let generation = snapshot_generation(conn)?;
    if proof.context.snapshot_generation != generation {
        return Ok(None);
    }
    Ok(Some((high, generation)))
}

/// Core-side re-verification of a stored proof, run on every observation.
fn proof_holds(conn: &Connection, env: &Env, row: &AttemptRow) -> Result<bool, Error> {
    let (Some(high), Some(generation)) = (row.proof_high_water, row.proof_generation) else {
        return Ok(false);
    };
    if !env.keys_ready
        || snapshot_generation(conn)? != generation
        || i64::try_from(crate::receive_cursor(conn)?).unwrap_or(i64::MAX) < high
    {
        return Ok(false);
    }
    let blocked: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM journal WHERE status='pending' AND cursor<=?1) OR EXISTS(SELECT 1 FROM snapshot_generations WHERE state='staging' OR (state='published' AND received>drained)) OR EXISTS(SELECT 1 FROM projection_generations WHERE state IN ('draining','staging'))",
        [high],
        |row| row.get(0),
    )?;
    Ok(!blocked && search_remaining(conn)? == 0)
}

enum Receipt {
    Verified,
    Pending,
    Mismatch,
}

/// A receipt is verified only against local rows: canonical message ID, the request target as
/// source device, the decimal revision as source sequence, and an indexed FTS row.
fn receipt(
    conn: &Connection,
    target: &str,
    message_id: &str,
    revision: &str,
) -> Result<Receipt, Error> {
    let row: Option<(i64, String, i64)> = conn
        .query_row(
            "SELECT local_order,source_device_id,source_sequence FROM messages WHERE id=?",
            [message_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((order, device, sequence)) = row else {
        return Ok(Receipt::Pending);
    };
    if device != target || revision != sequence.to_string() {
        return Ok(Receipt::Mismatch);
    }
    let indexed: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_search_fts WHERE rowid=?)",
        [order],
        |row| row.get(0),
    )?;
    Ok(if indexed {
        Receipt::Verified
    } else {
        Receipt::Pending
    })
}

/// The source identity a request was made against: generation, key epoch, and the revision
/// captured from the source record. `revision` is `None` for requests made before capture.
#[derive(Clone, Copy)]
struct Version {
    generation: i64,
    key_epoch: u32,
    revision: Option<i64>,
}

/// One admitted source as seen through the protocol projection, or a local refusal row.
struct Admitted {
    target: String,
    state: String,
    reason: Option<&'static str>,
    exhausted: bool,
    receipts: Vec<(String, String)>,
    /// True only once a phone-originated record exists. A server acknowledgement alone is not
    /// a response and never extends the attempt.
    phone_responded: bool,
    version: Option<Version>,
}

fn admitted(conn: &Connection, env: &Env, attempt_id: &str) -> Result<Vec<Admitted>, Error> {
    let rows: Vec<(String, Option<String>, Option<String>)> = {
        let mut statement = conn.prepare(
            "SELECT target_device_id,request_id,reason FROM message_history_attempt_sources WHERE attempt_id=? ORDER BY target_device_id",
        )?;
        statement
            .query_map([attempt_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<_, _>>()?
    };
    rows.into_iter()
        .map(|(target, request_id, reason)| {
            let Some(request_id) = request_id else {
                return Ok(Admitted {
                    target,
                    state: "blocked".into(),
                    reason: reason_name(reason.as_deref()).or(Some("capacity")),
                    exhausted: false,
                    receipts: Vec::new(),
                    phone_responded: false,
                    version: None,
                });
            };
            let value: Value = serde_json::from_str(&message_history_protocol::status(
                conn,
                &env.requester,
                &json!({"request_id": request_id}).to_string(),
            )?)
            .map_err(|_| Error::Database)?;
            if value.is_null() {
                return Ok(Admitted {
                    target,
                    state: "expired".into(),
                    reason: Some("offline"),
                    exhausted: false,
                    receipts: Vec::new(),
                    phone_responded: false,
                    version: None,
                });
            }
            let version = value["generation"]
                .as_str()
                .and_then(|value| value.parse::<i64>().ok())
                .zip(
                    value["keyEpoch"]
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok()),
                )
                .map(|(generation, key_epoch)| Version {
                    generation,
                    key_epoch,
                    revision: value["sourceRevision"]
                        .as_str()
                        .and_then(|value| value.parse::<i64>().ok()),
                });
            let state = match value["state"].as_str() {
                Some(
                    state @ ("queued" | "waiting" | "running" | "completed" | "partial" | "blocked"
                    | "failed" | "expired"),
                ) => state.to_owned(),
                _ => "failed".to_owned(),
            };
            let receipts = value["receipts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| {
                    Some((
                        item["messageId"].as_str()?.to_owned(),
                        item["revision"].as_str()?.to_owned(),
                    ))
                })
                .collect();
            Ok(Admitted {
                target,
                state,
                reason: wire_reason(value["reason"].as_str()),
                exhausted: value["sourceExhausted"].as_bool() == Some(true),
                receipts,
                phone_responded: value["phoneResponded"].as_bool() == Some(true),
                version,
            })
        })
        .collect()
}

/// True when this attempt's own completed, exhausted request covers the source's exact version:
/// same target, generation, key epoch and captured revision. Advertised exhaustion covers the
/// current version on its own. A coverage record of unknown revision never covers.
fn covers(rows: &[Admitted], view: &SourceView) -> bool {
    view.exhausted
        || rows.iter().any(|row| {
            row.target == view.producer
                && row.state == "completed"
                && row.exhausted
                && row.version.is_some_and(|version| {
                    version.generation == view.generation
                        && version.key_epoch == view.key_epoch
                        && version.revision == Some(view.revision)
                })
        })
}

/// The result of evaluating one attempt against core state. Read-only to compute.
#[derive(Clone)]
struct Projection {
    state: String,
    reason: Option<&'static str>,
    /// Terminal transitions withdraw only unshared, still-active interest.
    withdraw: bool,
    proof_ok: bool,
    admitted: usize,
    unused: Vec<String>,
}

fn project(conn: &Connection, env: &Env, row: &AttemptRow, now: i64) -> Result<Projection, Error> {
    if terminal(&row.state) {
        return Ok(Projection {
            state: row.state.clone(),
            reason: row.reason,
            withdraw: false,
            proof_ok: false,
            admitted: 0,
            unused: Vec::new(),
        });
    }
    let views = source_views(conn, env)?;
    let rows = admitted(conn, env, &row.id)?;
    let open = selection(&views, row.rotation);
    let unused: Vec<String> = open
        .iter()
        .filter(|target| !rows.iter().any(|row| &row.target == *target))
        .cloned()
        .collect();
    let proof_ok = proof_holds(conn, env, row)?;
    let more = !unused.is_empty() && rows.len() < MAX_SOURCES;
    let shape = |state: &str, reason: Option<&'static str>, withdraw: bool| Projection {
        state: state.to_owned(),
        reason,
        withdraw,
        proof_ok,
        admitted: rows.len(),
        unused: unused.clone(),
    };

    let responded = rows.iter().any(|row| row.phone_responded);
    // Coverage is known only when every capable advertisement is current-epoch, permitted, and
    // covered by this attempt at its exact version (see `covers`).
    let covered = views
        .iter()
        .filter(|view| view.capable)
        .all(|view| view.permitted_now() && covers(&rows, view));
    // A wall clock set before the persisted start would never reach the deadline. Treat it
    // as expired instead of extending the wait; forward jumps expire through the deadline.
    if now < row.started_at_ms {
        return Ok(shape("partial", Some("timeout"), true));
    }
    if now >= row.deadline_ms {
        return Ok(if responded {
            shape("partial", Some("timeout"), true)
        } else {
            shape("failed", Some("offline"), true)
        });
    }
    if !env.keys_ready {
        return Ok(shape("blocked", Some("needs_unlock"), true));
    }
    if row.state == "catching_up"
        && row.proof_high_water.is_none()
        && now >= row.catchup_deadline_ms
    {
        return Ok(shape("partial", Some("timeout"), true));
    }
    if rows.is_empty() {
        if !views.iter().any(SourceView::capable_now) {
            return Ok(shape("blocked", Some("unsupported"), false));
        }
        if !views.iter().any(SourceView::permitted_now) {
            return Ok(shape("blocked", Some("permission"), false));
        }
        if open.is_empty() {
            if covered {
                // Completion needs a fresh replay proof; without one the attempt keeps catching up.
                return Ok(shape(
                    if proof_ok { "completed" } else { "catching_up" },
                    None,
                    false,
                ));
            }
            let denied = views.iter().any(|view| view.capable && !view.permitted);
            let reason = if denied { "permission" } else { "unsupported" };
            return Ok(shape("blocked", Some(reason), false));
        }
    }
    if rows
        .iter()
        .any(|row| matches!(row.state.as_str(), "queued" | "waiting" | "running"))
    {
        return Ok(shape("waiting_for_phone", None, false));
    }

    let mut receipts_pending = false;
    let mut mismatch = false;
    for source in rows
        .iter()
        .filter(|row| matches!(row.state.as_str(), "completed" | "partial"))
    {
        for (message_id, revision) in &source.receipts {
            match receipt(conn, &source.target, message_id, revision)? {
                Receipt::Verified => {}
                Receipt::Pending => receipts_pending = true,
                Receipt::Mismatch => mismatch = true,
            }
        }
    }
    if receipts_pending {
        return Ok(shape("indexing", None, false));
    }
    if more {
        return Ok(shape(
            if proof_ok {
                "waiting_for_phone"
            } else {
                "catching_up"
            },
            None,
            false,
        ));
    }

    let all_complete = !rows.is_empty()
        && unused.is_empty()
        && !mismatch
        && covered
        && rows
            .iter()
            .all(|row| row.state == "completed" && row.exhausted);
    if all_complete {
        return Ok(shape(
            if proof_ok { "completed" } else { "catching_up" },
            None,
            false,
        ));
    }
    if mismatch
        || rows
            .iter()
            .any(|row| matches!(row.state.as_str(), "completed" | "partial"))
    {
        return Ok(shape("partial", None, false));
    }
    if !rows.is_empty() && rows.iter().all(|row| row.state == "blocked") {
        let reason = rows
            .iter()
            .filter_map(|row| row.reason)
            .min_by_key(|reason| match *reason {
                "permission" => 0,
                "needs_unlock" => 1,
                "capacity" => 2,
                _ => 3,
            });
        if let Some(reason) = reason {
            return Ok(shape("blocked", Some(reason), false));
        }
        // Blocked without a mappable reason (for example an exhausted source) is a generic
        // failure rather than a permission or capacity claim.
    }
    let reason = rows
        .iter()
        .filter(|row| row.state != "blocked")
        .find_map(|row| row.reason)
        // Offline is reserved for the zero-response path. A phone that responded and failed
        // generically stays a generic failure.
        .or((!responded).then_some("offline"));
    Ok(shape("failed", reason, false))
}

/// Persists a projection and withdraws unshared interest on deadline or unlock transitions.
fn persist(
    conn: &Connection,
    ctx: &Ctx<'_>,
    row: &AttemptRow,
    projection: &Projection,
    now: i64,
) -> Result<(), Error> {
    if projection.state != row.state || projection.reason != row.reason {
        conn.execute(
            "UPDATE message_history_attempts SET state=?,reason=?,progress_version=progress_version+1,updated_at=? WHERE attempt_id=? AND state=?",
            params![projection.state, projection.reason, now, row.id, row.state],
        )?;
    }
    if projection.withdraw {
        withdraw(conn, ctx, &row.id)?;
    }
    Ok(())
}

/// True when a non-terminal attempt other than `except` still references the request.
fn live_elsewhere(conn: &Connection, request_id: &str, except: &str) -> Result<bool, Error> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM message_history_attempt_sources s JOIN message_history_attempts t ON t.attempt_id=s.attempt_id WHERE s.request_id=? AND s.attempt_id<>? AND t.state IN ('catching_up','waiting_for_phone','indexing'))",
        params![request_id, except],
        |row| row.get(0),
    )?)
}

/// Only refusals that a later retry can clear are deferred: keys that are not unlocked, or
/// compaction that is unsupported or still upgrading. Every other error propagates.
fn deferrable_refusal(error: &Error) -> bool {
    matches!(error, Error::KeysUnavailable | Error::InvalidRequest(_))
}

/// Cancels one live remote request inside a savepoint. Returns `Ok(true)` when no remote work
/// remains (cancelled, already terminal, or purged) and `Ok(false)` when the cancel was
/// deferred. A deferred cancel rolls back only its own protocol writes.
fn cancel_remote(conn: &Connection, ctx: &Ctx<'_>, request_id: &str) -> Result<bool, Error> {
    let input = json!({"request_id": request_id}).to_string();
    let status: Value = serde_json::from_str(&message_history_protocol::status(
        conn,
        &ctx.device_id.to_string(),
        &input,
    )?)
    .map_err(|_| Error::Database)?;
    if status.is_null() {
        return Ok(true);
    }
    conn.execute_batch("SAVEPOINT history_remote_cancel")?;
    match message_history_protocol::cancel_in_transaction(conn, ctx, &input) {
        Ok(_) => {
            conn.execute_batch("RELEASE history_remote_cancel")?;
            Ok(true)
        }
        Err(error) => {
            conn.execute_batch("ROLLBACK TO history_remote_cancel; RELEASE history_remote_cancel")?;
            if deferrable_refusal(&error) {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }
}

fn set_cancel_pending(conn: &Connection, request_id: &str, pending: bool) -> Result<(), Error> {
    conn.execute(
        "UPDATE message_history_attempt_sources SET cancel_pending=? WHERE request_id=?",
        params![i64::from(pending), request_id],
    )?;
    Ok(())
}

/// Ends this attempt's interest in its admitted requests. Shared requests stay alive for the
/// other live attempts. A remote cancel that cannot be enqueued yet is recorded for a later
/// flush, so the local end always completes.
fn withdraw(conn: &Connection, ctx: &Ctx<'_>, attempt_id: &str) -> Result<(), Error> {
    let requests: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT request_id FROM message_history_attempt_sources WHERE attempt_id=? AND request_id IS NOT NULL",
        )?;
        statement
            .query_map([attempt_id], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    for request_id in requests {
        if live_elsewhere(conn, &request_id, attempt_id)? {
            continue;
        }
        let cancelled = cancel_remote(conn, ctx, &request_id)?;
        set_cancel_pending(conn, &request_id, !cancelled)?;
    }
    Ok(())
}

/// Retries deferred remote cancels, rechecking for other live interest before each one.
fn flush_deferred_cancels(conn: &Connection, ctx: &Ctx<'_>) -> Result<(), Error> {
    let pending: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT DISTINCT request_id FROM message_history_attempt_sources WHERE cancel_pending=1 AND request_id IS NOT NULL",
        )?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    for request_id in pending {
        if live_elsewhere(conn, &request_id, "")? {
            continue;
        }
        if cancel_remote(conn, ctx, &request_id)? {
            set_cancel_pending(conn, &request_id, false)?;
        }
    }
    Ok(())
}

/// What the local store can offer right now, computed read-only from core state.
struct Availability {
    /// An open source exists: current epoch, permitted, and not exhausted.
    open: bool,
    /// `open`, and the server can admit a request now (compaction ready, keys unlocked).
    admissible: bool,
    /// `admissible`, and some open source is not covered by this attempt at its version.
    uncovered: bool,
}

fn can_search_further(
    state: HistorySearchState,
    reason: Option<HistorySearchReason>,
    availability: &Availability,
) -> bool {
    match state {
        HistorySearchState::Idle => availability.open,
        HistorySearchState::CatchingUp
        | HistorySearchState::WaitingForPhone
        | HistorySearchState::Indexing => false,
        // Terminal coverage is never a dead end: a new source, generation, or revision is
        // offered, but the just-completed version is not.
        HistorySearchState::Completed => availability.uncovered,
        HistorySearchState::Blocked => match reason {
            Some(HistorySearchReason::NeedsUnlock) => true,
            // A stored permission block is never terminal for the offer: retry is offered
            // exactly while a current eligible source exists. Retry itself is an explicit
            // new start; no status read mutates state.
            Some(HistorySearchReason::Capacity) | Some(HistorySearchReason::Permission) => {
                availability.open
            }
            // Unsupported rearms only once the server can actually admit the request.
            Some(HistorySearchReason::Unsupported) => availability.admissible,
            _ => false,
        },
        HistorySearchState::Partial
        | HistorySearchState::Failed
        | HistorySearchState::Cancelled => availability.open,
    }
}

fn idle(conn: &Connection, open: bool) -> Result<HistorySearchStatus, Error> {
    Ok(HistorySearchStatus {
        attempt_id: None,
        state: HistorySearchState::Idle,
        reason: None,
        can_search_further: open,
        progress_version: "0".into(),
        index_version: crate::local_search::search_generation(conn)?.to_string(),
    })
}

/// Read-only projection of one attempt. Writes nothing; a changed state bumps the projected
/// progress version so status and the persisted version agree after the next transition.
fn status_for(
    conn: &Connection,
    env: &Env,
    ctx: &Ctx<'_>,
    attempt: Option<Uuid>,
    now: i64,
) -> Result<HistorySearchStatus, Error> {
    let views = source_views(conn, env)?;
    let open = views.iter().any(SourceView::open);
    let Some(id) = attempt else {
        return idle(conn, open);
    };
    let Some(row) = load(conn, id)? else {
        return idle(conn, open);
    };
    let projection = project(conn, env, &row, now)?;
    let state = parse_state(&projection.state);
    let reason = projection.reason.and_then(parse_reason);
    let changed = projection.state != row.state || projection.reason != row.reason;
    let admissible = open && compaction_readiness(conn, ctx)?["state"] == "ready";
    let uncovered = admissible && {
        let rows = admitted(conn, env, &row.id)?;
        views.iter().any(|view| view.open() && !covers(&rows, view))
    };
    let availability = Availability {
        open,
        admissible,
        uncovered,
    };
    Ok(HistorySearchStatus {
        attempt_id: Some(id),
        state,
        reason,
        can_search_further: can_search_further(state, reason, &availability),
        progress_version: (row.progress_version + i64::from(changed)).to_string(),
        index_version: crate::local_search::search_generation(conn)?.to_string(),
    })
}

/// Expires overdue active attempts and prunes terminal ones older than the retention window.
fn prune_and_sweep(conn: &Connection, ctx: &Ctx<'_>, env: &Env, now: i64) -> Result<(), Error> {
    let cutoff = now - RETENTION_MS;
    conn.execute(
        "DELETE FROM message_history_attempt_sources WHERE attempt_id IN (SELECT attempt_id FROM message_history_attempts WHERE state IN ('completed','partial','blocked','failed','cancelled') AND updated_at<?)",
        [cutoff],
    )?;
    conn.execute(
        "DELETE FROM message_history_attempts WHERE state IN ('completed','partial','blocked','failed','cancelled') AND updated_at<?",
        [cutoff],
    )?;
    let active: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT attempt_id FROM message_history_attempts WHERE state NOT IN ('completed','partial','blocked','failed','cancelled')",
        )?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    for id in active {
        let id = Uuid::parse_str(&id).map_err(|_| Error::Database)?;
        if let Some(row) = load(conn, id)? {
            let projection = project(conn, env, &row, now)?;
            persist(conn, ctx, &row, &projection, now)?;
        }
    }
    Ok(())
}

/// Live protocol requests to one target, counted exactly as the protocol's own cap counts them.
fn active_requests(conn: &Connection, target: &str) -> Result<i64, Error> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM message_history_requests WHERE target=? AND status IN ('queued','waiting','running')",
        [target],
        |row| row.get(0),
    )?)
}

/// Admits the next unused source. Protocol refusals (per-source active cap) become a
/// capacity outcome for that source; other errors propagate.
fn admit(
    conn: &mut Connection,
    ctx: &Ctx<'_>,
    attempt_id: &str,
    projection: &Projection,
) -> Result<HistorySearchWork, Error> {
    let mut admitted = projection.admitted;
    for target in &projection.unused {
        if admitted >= MAX_SOURCES {
            break;
        }
        match message_history_protocol::request(
            conn,
            ctx,
            &json!({"target_device_id": target}).to_string(),
        ) {
            Ok(response) => {
                let value: Value = serde_json::from_str(&response).map_err(|_| Error::Database)?;
                let request_id = value["requestId"]
                    .as_str()
                    .ok_or(Error::Database)?
                    .to_owned();
                conn.execute(
                    "INSERT INTO message_history_attempt_sources(attempt_id,target_device_id,request_id,reason) VALUES(?,?,?,NULL)",
                    params![attempt_id, target, request_id],
                )?;
                return Ok(HistorySearchWork::RequestPhone {
                    request_id,
                    target_device_id: target.clone(),
                });
            }
            Err(Error::InvalidRequest(_)) => {
                // Only the protocol's own active cap is capacity. Any other refusal (a server
                // without compaction, or one still upgrading) is unsupported for this attempt,
                // and the source is never retried within it.
                let reason = if active_requests(conn, target)? >= PROTOCOL_ACTIVE_REQUEST_CAP {
                    "capacity"
                } else {
                    "unsupported"
                };
                conn.execute(
                    "INSERT INTO message_history_attempt_sources(attempt_id,target_device_id,request_id,reason) VALUES(?,?,NULL,?)",
                    params![attempt_id, target, reason],
                )?;
                admitted += 1;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(HistorySearchWork::Wait)
}

impl Client {
    pub fn start_history_search(&self, operation_id: Uuid) -> Result<HistorySearchStatus, Error> {
        if operation_id.is_nil() {
            return Err(Error::InvalidRequest("history operation id"));
        }
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let env = Env::new(&ctx);
        let now = now_ms();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        flush_deferred_cancels(&tx, &ctx)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT attempt_id FROM message_history_attempts WHERE operation_id=?",
                [operation_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let id = match existing {
            Some(value) => Uuid::parse_str(value.as_str()).map_err(|_| Error::Database)?,
            None => {
                prune_and_sweep(&tx, &ctx, &env, now)?;
                let active: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM message_history_attempts WHERE state NOT IN ('completed','partial','blocked','failed','cancelled')",
                    [],
                    |row| row.get(0),
                )?;
                if active >= MAX_ACTIVE_ATTEMPTS {
                    return Err(Error::InvalidRequest("history search capacity"));
                }
                let id = Uuid::new_v4();
                let rotation: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(rotation),-1)+1 FROM message_history_attempts",
                    [],
                    |row| row.get(0),
                )?;
                tx.execute(
                    "INSERT INTO message_history_attempts(attempt_id,operation_id,state,reason,started_at_ms,deadline_ms,catchup_deadline_ms,rotation,progress_version,updated_at) VALUES(?,?,'catching_up',NULL,?,?,?,?,1,?)",
                    params![
                        id.to_string(),
                        operation_id.to_string(),
                        now,
                        now + ATTEMPT_MS,
                        now + CATCH_UP_MS,
                        rotation,
                        now
                    ],
                )?;
                id
            }
        };
        tx.commit()?;
        status_for(conn, &env, &ctx, Some(id), now)
    }

    /// Read-only. `None`, or an unknown ID, returns idle availability without inventing an attempt.
    pub fn history_search_status(
        &self,
        attempt_id: Option<Uuid>,
    ) -> Result<HistorySearchStatus, Error> {
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let env = Env::new(&ctx);
        status_for(conn, &env, &ctx, attempt_id, now_ms())
    }

    /// Captured immediately before a replay HTTP request. Trusted worker/native code only.
    pub fn history_search_sync_context(&self) -> Result<HistorySearchSyncContext, Error> {
        let store = self.lock()?;
        Ok(HistorySearchSyncContext {
            started_at_ms: now_ms(),
            snapshot_generation: snapshot_generation(&store.conn)?,
        })
    }

    /// Cancels this attempt in one transaction. Terminal attempts are returned unchanged.
    pub fn cancel_history_search(&self, attempt_id: Uuid) -> Result<HistorySearchStatus, Error> {
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let env = Env::new(&ctx);
        let now = now_ms();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = load(&tx, attempt_id)?.ok_or(Error::NotFound)?;
        if !terminal(&row.state) {
            tx.execute(
                "UPDATE message_history_attempts SET state='cancelled',reason=NULL,progress_version=progress_version+1,updated_at=? WHERE attempt_id=? AND state NOT IN ('completed','partial','blocked','failed','cancelled')",
                params![now, row.id],
            )?;
            withdraw(&tx, &ctx, &row.id)?;
        }
        tx.commit()?;
        status_for(conn, &env, &ctx, Some(attempt_id), now)
    }

    /// Host-only observation after replay, apply, snapshot, or index work. The proof is
    /// validated and stored by core; every transition is re-derived from core state.
    pub fn observe_history_search_sync(
        &self,
        attempt_id: Uuid,
        facts: HistorySearchSyncFacts,
    ) -> Result<HistorySearchStatus, Error> {
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let env = Env::new(&ctx);
        let now = now_ms();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        flush_deferred_cancels(&tx, &ctx)?;
        let row = load(&tx, attempt_id)?.ok_or(Error::NotFound)?;
        if let Some(proof) = &facts.proof
            && let Some((high, generation)) = accept_proof(&tx, &row, proof)?
        {
            tx.execute(
                "UPDATE message_history_attempts SET proof_high_water=?,proof_generation=? WHERE attempt_id=?",
                params![high, generation, row.id],
            )?;
        }
        let row = load(&tx, attempt_id)?.ok_or(Error::NotFound)?;
        let projection = project(&tx, &env, &row, now)?;
        persist(&tx, &ctx, &row, &projection, now)?;
        tx.commit()?;
        status_for(conn, &env, &ctx, Some(attempt_id), now)
    }

    /// Work for the host loop. Persists transitions; admits at most one phone request per call.
    pub fn history_search_work(&self, attempt_id: Uuid) -> Result<HistorySearchWork, Error> {
        let mut store = self.lock()?;
        let (conn, ctx) = store.parts();
        let env = Env::new(&ctx);
        let now = now_ms();
        let projection = {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            flush_deferred_cancels(&tx, &ctx)?;
            let row = load(&tx, attempt_id)?.ok_or(Error::NotFound)?;
            let projection = project(&tx, &env, &row, now)?;
            persist(&tx, &ctx, &row, &projection, now)?;
            tx.commit()?;
            projection
        };
        match projection.state.as_str() {
            "catching_up" | "indexing" => {
                if env.keys_ready && search_remaining(conn)? > 0 {
                    Ok(HistorySearchWork::AdvanceIndex {
                        limit: INDEX_WORK_LIMIT,
                    })
                } else {
                    Ok(HistorySearchWork::WakeSync)
                }
            }
            "waiting_for_phone"
                if projection.proof_ok
                    && projection.admitted < MAX_SOURCES
                    && !projection.unused.is_empty() =>
            {
                admit(conn, &ctx, &attempt_id.to_string(), &projection)
            }
            _ => Ok(HistorySearchWork::Wait),
        }
    }
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), Error> {
    let present: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
        params![table, column],
        |row| row.get(0),
    )?;
    if !present {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

fn drop_legacy_column(conn: &Connection, table: &str, column: &str) -> Result<(), Error> {
    let present: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
        params![table, column],
        |row| row.get(0),
    )?;
    if present {
        conn.execute_batch(&format!("ALTER TABLE {table} DROP COLUMN {column}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientConfig, DatabaseKey, DeviceId, VaultId};
    use serde_json::json;
    use std::collections::BTreeMap;

    const STARTED: i64 = 1_000_000;
    const TARGET: &str = "phone-a";

    fn client() -> (tempfile::TempDir, Client) {
        let dir = tempfile::TempDir::new().unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("history.db"),
                vault_id: VaultId::new(),
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[3; 32]).unwrap(),
        )
        .unwrap();
        (dir, client)
    }

    fn env() -> Env {
        Env {
            requester: "desktop".into(),
            epoch: Some(1),
            keys_ready: true,
        }
    }

    fn insert_attempt(conn: &Connection, id: Uuid, started: i64) {
        conn.execute(
            "INSERT INTO message_history_attempts(attempt_id,operation_id,state,reason,started_at_ms,deadline_ms,catchup_deadline_ms,rotation,progress_version,updated_at) VALUES(?,?,'catching_up',NULL,?,?,?,0,1,?)",
            params![
                id.to_string(),
                Uuid::new_v4().to_string(),
                started,
                started + ATTEMPT_MS,
                started + CATCH_UP_MS,
                started
            ],
        )
        .unwrap();
    }

    fn insert_source(conn: &Connection, producer: &str, body: Value) {
        conn.execute(
            "INSERT INTO message_history_sources(producer,generation,revision,key_epoch,body,updated_at) VALUES(?,1,1,1,?,0)",
            params![producer, body.to_string()],
        )
        .unwrap();
    }

    fn capable(exhausted: bool) -> Value {
        json!({"capabilities":{"sms":true,"mms":false},"permission":true,"policy":true,"exhausted":exhausted})
    }

    /// Inserts a protocol request owned by "desktop" and links it to an attempt. The protocol
    /// validates request IDs as canonical UUIDs, so each row gets a fresh one.
    fn insert_request(
        conn: &Connection,
        attempt: Uuid,
        target: &str,
        status: &str,
        reason: Option<&str>,
        result: Value,
    ) {
        let request_id = Uuid::new_v4().to_string();
        let expires = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap()
            + 3600;
        conn.execute(
            "INSERT INTO message_history_requests(request_id,requester,target,generation,key_epoch,body,status,reason,expires_at,result,source_revision) VALUES(?,?,?,1,1,'{}',?,?,?,?,1)",
            params![
                request_id,
                "desktop",
                target,
                status,
                reason,
                expires,
                (!result.is_null()).then(|| result.to_string())
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message_history_attempt_sources(attempt_id,target_device_id,request_id,reason) VALUES(?,?,?,NULL)",
            params![attempt.to_string(), target, request_id],
        )
        .unwrap();
    }

    fn insert_message(conn: &Connection, id: &str, device: &str, sequence: i64) -> i64 {
        conn.execute(
            "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen) VALUES(?,?,?,?,?,0)",
            params![id, "conversation", device, sequence, r#"{"body":"hello"}"#],
        )
        .unwrap();
        conn.query_row("SELECT local_order FROM messages WHERE id=?", [id], |row| {
            row.get(0)
        })
        .unwrap()
    }

    fn row_for(conn: &Connection, id: Uuid) -> AttemptRow {
        load(conn, id).unwrap().unwrap()
    }

    fn receipts_result(exhausted: bool, receipts: Value) -> Value {
        json!({"receipts": receipts, "sourceExhausted": exhausted, "nextProgressToken": null})
    }

    /// Stores a replay proof at high water 0 for the current (empty) snapshot generation.
    fn prove(conn: &Connection, id: Uuid) {
        conn.execute(
            "UPDATE message_history_attempts SET proof_high_water=0,proof_generation=0 WHERE attempt_id=?",
            [id.to_string()],
        )
        .unwrap();
    }

    #[test]
    fn capable_source_without_replay_proof_stays_catching_up_until_deadline() {
        let (_dir, client) = client();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        let id = Uuid::new_v4();
        insert_attempt(conn, id, STARTED);
        insert_source(conn, TARGET, capable(false));
        let row = row_for(conn, id);
        let before = project(conn, &env(), &row, STARTED + CATCH_UP_MS - 1).unwrap();
        assert_eq!(before.state, "catching_up");
        let after = project(conn, &env(), &row, STARTED + CATCH_UP_MS).unwrap();
        assert_eq!(
            (after.state.as_str(), after.reason),
            ("partial", Some("timeout"))
        );
    }

    #[test]
    fn phone_that_responded_before_deadline_is_partial_timeout_not_offline() {
        let (_dir, client) = client();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        let id = Uuid::new_v4();
        insert_attempt(conn, id, STARTED);
        insert_source(conn, TARGET, capable(false));
        insert_request(
            conn,
            id,
            TARGET,
            "waiting",
            None,
            receipts_result(false, json!([])),
        );
        let projection = project(conn, &env(), &row_for(conn, id), STARTED + ATTEMPT_MS).unwrap();
        assert_eq!(
            (projection.state.as_str(), projection.reason),
            ("partial", Some("timeout"))
        );
    }

    #[test]
    fn unanswered_request_at_deadline_is_failed_offline() {
        let (_dir, client) = client();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        let id = Uuid::new_v4();
        insert_attempt(conn, id, STARTED);
        insert_source(conn, TARGET, capable(false));
        // Server acknowledgement alone: no phone-originated record exists.
        insert_request(conn, id, TARGET, "queued", None, Value::Null);
        let row = row_for(conn, id);
        let projection = project(conn, &env(), &row, STARTED + ATTEMPT_MS).unwrap();
        assert_eq!(
            (projection.state.as_str(), projection.reason),
            ("failed", Some("offline"))
        );
    }

    #[test]
    fn locked_epoch_blocks_with_needs_unlock() {
        let (_dir, client) = client();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        let id = Uuid::new_v4();
        insert_attempt(conn, id, STARTED);
        let locked = Env {
            keys_ready: false,
            ..env()
        };
        let projection = project(conn, &locked, &row_for(conn, id), STARTED + 1).unwrap();
        assert_eq!(
            (projection.state.as_str(), projection.reason),
            ("blocked", Some("needs_unlock"))
        );
    }

    #[test]
    fn replay_proof_must_be_post_start_terminal_and_current_generation() {
        let (_dir, client) = client();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        let id = Uuid::new_v4();
        insert_attempt(conn, id, STARTED);
        let row = row_for(conn, id);
        let proof = |started_at_ms: i64, generation: i64, next_present: bool, terminal: bool| {
            HistorySearchReplayProof {
                context: HistorySearchSyncContext {
                    started_at_ms,
                    snapshot_generation: generation,
                },
                high_water_cursor: "42".into(),
                next_after_present: next_present,
                terminal_next_after: terminal,
            }
        };
        assert_eq!(
            accept_proof(conn, &row, &proof(STARTED - 1, 0, true, true)).unwrap(),
            None
        );
        assert_eq!(
            accept_proof(conn, &row, &proof(STARTED, 9, true, true)).unwrap(),
            None
        );
        assert_eq!(
            accept_proof(conn, &row, &proof(STARTED, 0, false, false)).unwrap(),
            None
        );
        assert_eq!(
            accept_proof(conn, &row, &proof(STARTED, 0, true, false)).unwrap(),
            None
        );
        assert_eq!(
            accept_proof(conn, &row, &proof(STARTED, 0, true, true)).unwrap(),
            Some((42, 0))
        );
        assert!(canonical_cursor("042").is_none());
        assert_eq!(
            canonical_cursor("0"),
            Some(0),
            "an empty server's high water is valid"
        );
        assert!(canonical_cursor("00").is_none());
        assert!(canonical_cursor("9223372036854775808").is_none());
    }

    #[test]
    fn stored_proof_is_reverified_against_receive_cursor_journal_and_generation() {
        let (_dir, client) = client();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        let id = Uuid::new_v4();
        insert_attempt(conn, id, STARTED);
        conn.execute(
            "UPDATE message_history_attempts SET proof_high_water=42,proof_generation=0 WHERE attempt_id=?",
            [id.to_string()],
        )
        .unwrap();
        let row = row_for(conn, id);
        assert!(
            !proof_holds(conn, &env(), &row).unwrap(),
            "receive cursor 0 is behind the high water"
        );
        conn.execute(
            "INSERT OR REPLACE INTO metadata(k,v) VALUES('receive_cursor','42')",
            [],
        )
        .unwrap();
        assert!(proof_holds(conn, &env(), &row).unwrap());
        conn.execute(
            "INSERT INTO journal(cursor,envelope_id,key_epoch,wire,status,reason) VALUES(7,'pending-envelope',1,X'00','pending',NULL)",
            [],
        )
        .unwrap();
        assert!(
            !proof_holds(conn, &env(), &row).unwrap(),
            "a pending journal row at or below the high water blocks proof"
        );
        conn.execute("DELETE FROM journal WHERE cursor=7", [])
            .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO metadata(k,v) VALUES('snapshot_generation','5')",
            [],
        )
        .unwrap();
        assert!(
            !proof_holds(conn, &env(), &row).unwrap(),
            "a generation change invalidates the stored proof"
        );
    }

    #[test]
    fn receipt_must_match_source_revision_and_index_before_completion() {
        let (_dir, client) = client();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        insert_source(conn, TARGET, capable(false));
        let message = Uuid::new_v4().to_string();
        let receipt = json!([{"messageId": message, "revision": "7"}]);

        let wrong_source = Uuid::new_v4();
        insert_attempt(conn, wrong_source, STARTED);
        insert_message(conn, &message, "someone-else", 7);
        insert_request(
            conn,
            wrong_source,
            TARGET,
            "completed",
            None,
            receipts_result(true, receipt.clone()),
        );
        assert_eq!(
            project(conn, &env(), &row_for(conn, wrong_source), STARTED + 1)
                .unwrap()
                .state,
            "partial"
        );
        conn.execute("DELETE FROM messages WHERE id=?", [&message])
            .unwrap();

        let wrong_revision = Uuid::new_v4();
        insert_attempt(conn, wrong_revision, STARTED);
        insert_message(conn, &message, TARGET, 8);
        insert_request(
            conn,
            wrong_revision,
            TARGET,
            "completed",
            None,
            receipts_result(true, receipt.clone()),
        );
        assert_eq!(
            project(conn, &env(), &row_for(conn, wrong_revision), STARTED + 1)
                .unwrap()
                .state,
            "partial"
        );
        conn.execute("DELETE FROM messages WHERE id=?", [&message])
            .unwrap();

        let not_indexed = Uuid::new_v4();
        insert_attempt(conn, not_indexed, STARTED);
        let order = insert_message(conn, &message, TARGET, 7);
        conn.execute("DELETE FROM local_search_fts WHERE rowid=?", [order])
            .unwrap();
        insert_request(
            conn,
            not_indexed,
            TARGET,
            "completed",
            None,
            receipts_result(true, receipt.clone()),
        );
        assert_eq!(
            project(conn, &env(), &row_for(conn, not_indexed), STARTED + 1)
                .unwrap()
                .state,
            "indexing"
        );
        conn.execute("DELETE FROM messages WHERE id=?", [&message])
            .unwrap();

        let missing = Uuid::new_v4();
        insert_attempt(conn, missing, STARTED);
        insert_request(
            conn,
            missing,
            TARGET,
            "completed",
            None,
            receipts_result(true, receipt.clone()),
        );
        assert_eq!(
            project(conn, &env(), &row_for(conn, missing), STARTED + 1)
                .unwrap()
                .state,
            "indexing"
        );

        let verified = Uuid::new_v4();
        insert_attempt(conn, verified, STARTED);
        insert_message(conn, &message, TARGET, 7);
        insert_request(
            conn,
            verified,
            TARGET,
            "completed",
            None,
            receipts_result(true, receipt),
        );
        prove(conn, verified);
        assert_eq!(
            project(conn, &env(), &row_for(conn, verified), STARTED + 1)
                .unwrap()
                .state,
            "completed"
        );
    }

    #[test]
    fn one_failed_source_does_not_erase_a_healthy_source_and_success_needs_coverage() {
        let (_dir, client) = client();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        insert_source(conn, "phone-a", capable(false));
        insert_source(conn, "phone-b", capable(false));

        let mixed = Uuid::new_v4();
        insert_attempt(conn, mixed, STARTED);
        insert_request(
            conn,
            mixed,
            "phone-a",
            "failed",
            Some("provider_error"),
            receipts_result(false, json!([])),
        );
        insert_request(
            conn,
            mixed,
            "phone-b",
            "completed",
            None,
            receipts_result(true, json!([])),
        );
        let projection = project(conn, &env(), &row_for(conn, mixed), STARTED + 1).unwrap();
        assert_eq!(
            (projection.state.as_str(), projection.reason),
            ("partial", None)
        );

        let all_failed = Uuid::new_v4();
        insert_attempt(conn, all_failed, STARTED);
        insert_request(
            conn,
            all_failed,
            "phone-a",
            "failed",
            Some("provider_error"),
            receipts_result(false, json!([])),
        );
        insert_request(
            conn,
            all_failed,
            "phone-b",
            "blocked",
            Some("permission_denied"),
            receipts_result(false, json!([])),
        );
        let projection = project(conn, &env(), &row_for(conn, all_failed), STARTED + 1).unwrap();
        // Both phones responded, so the generic failure is not an offline claim.
        assert_eq!(
            (projection.state.as_str(), projection.reason),
            ("failed", None)
        );

        let unknown_coverage = Uuid::new_v4();
        insert_attempt(conn, unknown_coverage, STARTED);
        insert_request(
            conn,
            unknown_coverage,
            "phone-a",
            "completed",
            None,
            receipts_result(false, json!([])),
        );
        insert_request(
            conn,
            unknown_coverage,
            "phone-b",
            "completed",
            None,
            receipts_result(true, json!([])),
        );
        let projection =
            project(conn, &env(), &row_for(conn, unknown_coverage), STARTED + 1).unwrap();
        assert_eq!(
            projection.state, "partial",
            "a source without exhaustion keeps coverage unknown"
        );
    }

    #[test]
    fn unsupported_and_permission_block_without_offering_a_dead_button() {
        let (_dir, client) = client();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        let none = Uuid::new_v4();
        insert_attempt(conn, none, STARTED);
        let projection = project(conn, &env(), &row_for(conn, none), STARTED + 1).unwrap();
        assert_eq!(
            (projection.state.as_str(), projection.reason),
            ("blocked", Some("unsupported"))
        );
        let denied = json!({"capabilities":{"sms":true,"mms":false},"permission":false,"policy":true,"exhausted":false});
        insert_source(conn, TARGET, denied);
        let projection = project(conn, &env(), &row_for(conn, none), STARTED + 1).unwrap();
        assert_eq!(
            (projection.state.as_str(), projection.reason),
            ("blocked", Some("permission"))
        );
        // Unsupported stays hidden while the server cannot admit, even with an open source.
        assert!(!can_search_further(
            HistorySearchState::Blocked,
            Some(HistorySearchReason::Unsupported),
            &avail(true, false, false)
        ));
        assert!(can_search_further(
            HistorySearchState::Blocked,
            Some(HistorySearchReason::Unsupported),
            &avail(true, true, false)
        ));
        assert!(!can_search_further(
            HistorySearchState::Blocked,
            Some(HistorySearchReason::Permission),
            &avail(false, false, false)
        ));
        assert!(can_search_further(
            HistorySearchState::Blocked,
            Some(HistorySearchReason::Permission),
            &avail(true, true, false)
        ));
        assert!(can_search_further(
            HistorySearchState::Blocked,
            Some(HistorySearchReason::NeedsUnlock),
            &avail(false, false, false)
        ));
        assert!(!can_search_further(
            HistorySearchState::Completed,
            None,
            &avail(true, true, false)
        ));
        assert!(can_search_further(
            HistorySearchState::Completed,
            None,
            &avail(true, true, true)
        ));
        assert!(!can_search_further(
            HistorySearchState::WaitingForPhone,
            None,
            &avail(true, true, true)
        ));
    }

    fn avail(open: bool, admissible: bool, uncovered: bool) -> Availability {
        Availability {
            open,
            admissible,
            uncovered,
        }
    }

    #[test]
    fn legacy_epoch_and_exhausted_sources_are_not_offered() {
        let (_dir, client) = client();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        insert_source(conn, TARGET, capable(true));
        let id = Uuid::new_v4();
        insert_attempt(conn, id, STARTED);
        let projection = project(conn, &env(), &row_for(conn, id), STARTED + 1).unwrap();
        assert_eq!(
            projection.state, "catching_up",
            "exhaustion alone is not completion without a fresh replay proof"
        );
        prove(conn, id);
        let projection = project(conn, &env(), &row_for(conn, id), STARTED + 1).unwrap();
        assert_eq!(
            projection.state, "completed",
            "all capable sources exhausted is known coverage once proven"
        );
        let other_epoch = Env {
            epoch: Some(2),
            ..env()
        };
        let projection = project(conn, &other_epoch, &row_for(conn, id), STARTED + 1).unwrap();
        assert_eq!(
            (projection.state.as_str(), projection.reason),
            ("blocked", Some("unsupported"))
        );
    }

    #[test]
    fn terminal_attempts_are_pruned_after_retention_and_active_children_are_kept() {
        let (_dir, client) = client();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        let old = Uuid::new_v4();
        insert_attempt(conn, old, STARTED);
        conn.execute(
            "UPDATE message_history_attempts SET state='completed',updated_at=0 WHERE attempt_id=?",
            [old.to_string()],
        )
        .unwrap();
        conn.execute("INSERT INTO message_history_attempt_sources(attempt_id,target_device_id,request_id,reason) VALUES(?,?,NULL,'capacity')", params![old.to_string(), TARGET]).unwrap();
        let recent = Uuid::new_v4();
        insert_attempt(conn, recent, STARTED);
        conn.execute(
            "UPDATE message_history_attempts SET state='failed',updated_at=? WHERE attempt_id=?",
            params![STARTED + RETENTION_MS, recent.to_string()],
        )
        .unwrap();
        let ctx = Ctx {
            vault_id: VaultId::new(),
            device_id: DeviceId::new(),
            keys: &BTreeMap::new(),
            active_epoch: None,
        };
        prune_and_sweep(conn, &ctx, &env(), STARTED + RETENTION_MS + 1).unwrap();
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM message_history_attempts WHERE attempt_id=?",
                [recent.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 1, "an attempt inside the window is kept");
        let pruned: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM message_history_attempts WHERE attempt_id=?",
                [old.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pruned, 0, "an attempt beyond the window is removed");
        let children: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM message_history_attempt_sources WHERE attempt_id=?",
                [old.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            children, 0,
            "pruned attempts take their source children with them"
        );
    }

    #[test]
    fn backward_clock_before_persisted_start_expires_the_attempt_instead_of_extending_it() {
        let (_dir, client) = client();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        insert_source(conn, TARGET, capable(false));
        let id = Uuid::new_v4();
        insert_attempt(conn, id, STARTED);
        let row = row_for(conn, id);

        let at_start = project(conn, &env(), &row, STARTED).unwrap();
        assert_eq!(
            at_start.state, "catching_up",
            "the attempt is live at its own start"
        );

        let regressed = project(conn, &env(), &row, STARTED - 1).unwrap();
        assert_eq!(
            (
                regressed.state.as_str(),
                regressed.reason,
                regressed.withdraw
            ),
            ("partial", Some("timeout"), true)
        );

        let ctx = Ctx {
            vault_id: VaultId::new(),
            device_id: DeviceId::new(),
            keys: &BTreeMap::new(),
            active_epoch: None,
        };
        persist(conn, &ctx, &row, &regressed, STARTED - 1).unwrap();
        let stored = row_for(conn, id);
        assert_eq!(
            stored.state, "partial",
            "the regression is persisted, not only projected"
        );
        assert_eq!(stored.reason, Some("timeout"));
    }

    #[test]
    fn selection_is_stable_device_order_rotated_per_attempt_and_skips_exhausted() {
        let views: Vec<SourceView> = ["d", "b", "e", "a", "c"]
            .iter()
            .map(|producer| SourceView {
                producer: (*producer).into(),
                capable: true,
                current_epoch: true,
                permitted: true,
                exhausted: false,
                generation: 1,
                key_epoch: 1,
                revision: 1,
            })
            .collect();
        let joined = |selected: Vec<String>| selected.join(",");
        assert_eq!(joined(selection(&views, 0)), "a,b,c,d,e");
        assert_eq!(joined(selection(&views, 1)), "b,c,d,e,a");
        assert_eq!(joined(selection(&views, 6)), "b,c,d,e,a");
        let mut exhausted = views;
        exhausted[0].exhausted = true;
        assert_eq!(joined(selection(&exhausted, 0)), "a,b,c,e");
        assert_eq!(MAX_SOURCES, 4);
    }

    #[test]
    fn coalesced_request_is_kept_while_another_live_attempt_references_it() {
        let (_dir, client) = client();
        let store = client.lock().unwrap();
        let conn = &store.conn;
        insert_source(conn, TARGET, capable(false));
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        insert_attempt(conn, first, STARTED);
        insert_attempt(conn, second, STARTED);
        insert_request(
            conn,
            first,
            TARGET,
            "waiting",
            None,
            receipts_result(false, json!([])),
        );
        let request_id: String = conn
            .query_row(
                "SELECT request_id FROM message_history_attempt_sources WHERE attempt_id=?",
                [first.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO message_history_attempt_sources(attempt_id,target_device_id,request_id,reason) VALUES(?,?,?,NULL)",
            params![second.to_string(), TARGET, request_id],
        )
        .unwrap();
        let ctx = Ctx {
            vault_id: VaultId::new(),
            device_id: DeviceId::new(),
            keys: &BTreeMap::new(),
            active_epoch: None,
        };
        withdraw(conn, &ctx, &first.to_string()).unwrap();
        let status: String = conn
            .query_row(
                "SELECT status FROM message_history_requests WHERE request_id=?",
                [&request_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            status, "waiting",
            "the second attempt still needs the shared request"
        );
    }

    #[test]
    fn pre_release_unique_request_table_is_rebuilt_without_losing_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE message_history_attempts(attempt_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL UNIQUE,state TEXT NOT NULL,reason TEXT,deadline INTEGER NOT NULL,catchup_deadline INTEGER NOT NULL,index_version TEXT NOT NULL,progress_version INTEGER NOT NULL DEFAULT 0,rotation INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE message_history_attempt_sources(attempt_id TEXT NOT NULL,target_device_id TEXT NOT NULL,request_id TEXT NOT NULL UNIQUE,PRIMARY KEY(attempt_id,target_device_id));
             INSERT INTO message_history_attempt_sources VALUES('attempt-1','phone-a','request-1');",
        )
        .unwrap();
        initialize(&conn).unwrap();
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name='message_history_attempt_sources'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!sql.contains("UNIQUE"));
        let preserved: String = conn
            .query_row("SELECT request_id FROM message_history_attempt_sources WHERE attempt_id='attempt-1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(preserved, "request-1");
        let legacy: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('message_history_attempts') WHERE name IN ('deadline','catchup_deadline','index_version')", [], |row| row.get(0))
            .unwrap();
        assert_eq!(legacy, 0);
        initialize(&conn).unwrap();
    }
}

/// Real admission: unlocked keys, compaction-capable server, backfill ready, remote
/// advertisements and phone results applied through the protocol's own apply path.
#[cfg(test)]
mod admission {
    use super::*;
    use crate::{ClientConfig, DatabaseKey, DeviceId, KeyProfile, PrivatePayload, VaultId};
    use peppy_crypto::{create_vault_check_header, derive_root_key};
    use serde_json::json;
    use std::collections::BTreeMap;

    const PASSPHRASE: &str = "history-admission-passphrase";

    struct Fixture {
        _dir: tempfile::TempDir,
        client: Client,
        device: String,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let device = DeviceId::new();
        let device_text = device.to_string();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("admission.db"),
                vault_id,
                device_id: device,
            },
            DatabaseKey::new(&[5; 32]).unwrap(),
        )
        .unwrap();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key(PASSPHRASE, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        client.unlock(&profile, &header, PASSPHRASE).unwrap();
        client.set_server_compaction_state(true, true).unwrap();
        let mut ready = false;
        for _ in 0..16 {
            let report: Value =
                serde_json::from_str(&client.compaction_backfill_step_json(100).unwrap()).unwrap();
            if report["state"] == "ready" {
                ready = true;
                break;
            }
        }
        assert!(
            ready,
            "compaction backfill must reach ready before admission"
        );
        Fixture {
            _dir: dir,
            client,
            device: device_text,
        }
    }

    fn advertisement(
        producer: &str,
        generation: i64,
        epoch: u32,
        exhausted: bool,
        permission: bool,
    ) -> Value {
        advertisement_at(producer, generation, "1", epoch, exhausted, permission)
    }

    fn advertisement_at(
        producer: &str,
        generation: i64,
        revision: &str,
        epoch: u32,
        exhausted: bool,
        permission: bool,
    ) -> Value {
        json!({"schema_version":1,"producer":producer,"generation":generation.to_string(),"revision":revision,"key_epoch":epoch,"capabilities":{"sms":true,"mms":false},"permission":permission,"policy":true,"exhausted":exhausted})
    }

    /// Applies a remote advertisement through the protocol's apply path.
    fn advertise(fixture: &Fixture, producer: &str, source: Value, envelope_epoch: u32) {
        let store = fixture.client.lock().unwrap();
        assert!(
            message_history_protocol::apply_event(
                &store.conn,
                producer,
                false,
                envelope_epoch,
                &PrivatePayload::MessageHistorySource { source },
            )
            .unwrap()
        );
    }

    /// Applies a phone-originated progress or result record through the protocol apply path.
    fn phone_record(
        fixture: &Fixture,
        producer: &str,
        request_id: &str,
        generation: i64,
        state: &str,
        exhausted: bool,
    ) {
        phone_record_with(
            fixture,
            producer,
            request_id,
            generation,
            state,
            exhausted,
            json!([]),
        );
    }

    fn phone_record_with(
        fixture: &Fixture,
        producer: &str,
        request_id: &str,
        generation: i64,
        state: &str,
        exhausted: bool,
        receipts: Value,
    ) {
        // The protocol requires the exact ten result keys.
        let result = json!({"schema_version":1,"request_id":request_id,"target":producer,"generation":generation.to_string(),"key_epoch":1,"state":state,"reason":null,"receipts":receipts,"source_exhausted":exhausted,"next_progress_token":null});
        let store = fixture.client.lock().unwrap();
        assert!(
            message_history_protocol::apply_event(
                &store.conn,
                producer,
                false,
                1,
                &PrivatePayload::MessageHistoryResult { result },
            )
            .unwrap()
        );
    }

    fn start(fixture: &Fixture) -> Uuid {
        fixture
            .client
            .start_history_search(Uuid::new_v4())
            .unwrap()
            .attempt_id
            .unwrap()
    }

    /// Replay proof with the empty-server high water `0`, captured after the attempt started.
    fn observe_empty_server(fixture: &Fixture, attempt: Uuid) -> HistorySearchStatus {
        let context = fixture.client.history_search_sync_context().unwrap();
        fixture
            .client
            .observe_history_search_sync(
                attempt,
                HistorySearchSyncFacts {
                    proof: Some(HistorySearchReplayProof {
                        context,
                        high_water_cursor: "0".into(),
                        next_after_present: true,
                        terminal_next_after: true,
                    }),
                },
            )
            .unwrap()
    }

    /// Repeats work until it stops admitting, returning each admitted `(request, target)`.
    fn drain(fixture: &Fixture, attempt: Uuid) -> Vec<(String, String)> {
        let mut admitted = Vec::new();
        for _ in 0..10 {
            match fixture.client.history_search_work(attempt).unwrap() {
                HistorySearchWork::RequestPhone {
                    request_id,
                    target_device_id,
                } => admitted.push((request_id, target_device_id)),
                _ => break,
            }
        }
        admitted
    }

    fn request_state(fixture: &Fixture, request_id: &str) -> String {
        let value: Value = serde_json::from_str(
            &fixture
                .client
                .message_history_request_status(&json!({"request_id": request_id}).to_string())
                .unwrap(),
        )
        .unwrap();
        value["state"].as_str().unwrap().to_owned()
    }

    fn projection_at(fixture: &Fixture, attempt: Uuid, now: i64) -> Projection {
        let store = fixture.client.lock().unwrap();
        let row = load(&store.conn, attempt).unwrap().unwrap();
        let env = Env {
            requester: fixture.device.clone(),
            epoch: Some(1),
            keys_ready: true,
        };
        project(&store.conn, &env, &row, now).unwrap()
    }

    #[test]
    fn zero_high_water_proof_admits_a_real_phone_request_with_compaction_enabled() {
        let fixture = fixture();
        advertise(
            &fixture,
            "phone-a",
            advertisement("phone-a", 1, 1, false, true),
            1,
        );
        let attempt = start(&fixture);
        observe_empty_server(&fixture, attempt);
        let admitted = drain(&fixture, attempt);
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].1, "phone-a");
        assert_eq!(request_state(&fixture, &admitted[0].0), "queued");
    }

    #[test]
    fn admission_fans_out_to_four_sources_and_coalesces_across_attempts() {
        let fixture = fixture();
        for producer in ["phone-a", "phone-b", "phone-c", "phone-d", "phone-e"] {
            advertise(
                &fixture,
                producer,
                advertisement(producer, 1, 1, false, true),
                1,
            );
        }
        let first = start(&fixture);
        observe_empty_server(&fixture, first);
        let first_admitted = drain(&fixture, first);
        let targets: Vec<&str> = first_admitted
            .iter()
            .map(|(_, target)| target.as_str())
            .collect();
        assert_eq!(
            targets,
            ["phone-a", "phone-b", "phone-c", "phone-d"],
            "four sources in stable order, the fifth is never admitted"
        );

        // A second attempt rotates to start at phone-b; its request coalesces with the first.
        let second = start(&fixture);
        observe_empty_server(&fixture, second);
        let HistorySearchWork::RequestPhone {
            request_id,
            target_device_id,
        } = fixture.client.history_search_work(second).unwrap()
        else {
            panic!("second attempt must admit a phone request");
        };
        assert_eq!(target_device_id, "phone-b");
        let shared = first_admitted
            .iter()
            .find(|(_, target)| target == "phone-b")
            .unwrap();
        assert_eq!(
            request_id, shared.0,
            "the existing active request is reused"
        );

        // Cancelling the second attempt keeps the request the first attempt still needs.
        fixture.client.cancel_history_search(second).unwrap();
        assert_eq!(request_state(&fixture, &request_id), "queued");

        // Cancelling the last live interest cancels the remote requests.
        fixture.client.cancel_history_search(first).unwrap();
        assert_eq!(request_state(&fixture, &request_id), "cancelled");
        assert_eq!(request_state(&fixture, &first_admitted[0].0), "cancelled");
    }

    #[test]
    fn five_open_sources_with_four_admitted_results_are_partial_never_completed() {
        let fixture = fixture();
        for producer in ["phone-a", "phone-b", "phone-c", "phone-d", "phone-e"] {
            advertise(
                &fixture,
                producer,
                advertisement(producer, 1, 1, false, true),
                1,
            );
        }
        let attempt = start(&fixture);
        observe_empty_server(&fixture, attempt);
        for (request_id, target) in drain(&fixture, attempt) {
            phone_record(&fixture, &target, &request_id, 1, "completed", true);
        }
        let status = fixture.client.history_search_status(Some(attempt)).unwrap();
        assert_eq!(status.state, HistorySearchState::Partial);
        assert_eq!(status.reason, None);
    }

    #[test]
    fn denied_capable_and_old_epoch_sources_prevent_completed_coverage() {
        let fixture = fixture();
        advertise(
            &fixture,
            "phone-a",
            advertisement("phone-a", 1, 1, true, true),
            1,
        );
        advertise(
            &fixture,
            "phone-b",
            advertisement("phone-b", 1, 1, false, false),
            1,
        );
        advertise(
            &fixture,
            "phone-old",
            advertisement("phone-old", 1, 0, false, true),
            0,
        );
        let attempt = start(&fixture);
        observe_empty_server(&fixture, attempt);
        let status = fixture.client.history_search_status(Some(attempt)).unwrap();
        assert_eq!(status.state, HistorySearchState::Blocked);
        assert_eq!(status.reason, Some(HistorySearchReason::Permission));
        assert!(!status.can_search_further);
    }

    #[test]
    fn old_epoch_capable_source_next_to_exhausted_one_is_unsupported_not_completed() {
        let fixture = fixture();
        advertise(
            &fixture,
            "phone-a",
            advertisement("phone-a", 1, 1, true, true),
            1,
        );
        advertise(
            &fixture,
            "phone-old",
            advertisement("phone-old", 1, 0, false, true),
            0,
        );
        let attempt = start(&fixture);
        observe_empty_server(&fixture, attempt);
        let status = fixture.client.history_search_status(Some(attempt)).unwrap();
        assert_eq!(status.state, HistorySearchState::Blocked);
        assert_eq!(status.reason, Some(HistorySearchReason::Unsupported));
        assert!(!status.can_search_further);
    }

    #[test]
    fn acknowledged_silent_phone_fails_offline_at_deadline_and_progress_times_out_partial() {
        let fixture = fixture();
        advertise(
            &fixture,
            "phone-a",
            advertisement("phone-a", 1, 1, false, true),
            1,
        );
        let attempt = start(&fixture);
        observe_empty_server(&fixture, attempt);
        let admitted = drain(&fixture, attempt);
        let (request_id, _) = admitted[0].clone();
        let deadline = {
            let store = fixture.client.lock().unwrap();
            load(&store.conn, attempt).unwrap().unwrap().deadline_ms
        };
        let silent = projection_at(&fixture, attempt, deadline);
        assert_eq!(
            (silent.state.as_str(), silent.reason),
            ("failed", Some("offline"))
        );

        phone_record(&fixture, "phone-a", &request_id, 1, "waiting", false);
        let progressed = projection_at(&fixture, attempt, deadline);
        assert_eq!(
            (progressed.state.as_str(), progressed.reason),
            ("partial", Some("timeout"))
        );
    }

    #[test]
    fn eighth_active_request_is_capacity_and_other_refusals_are_unsupported() {
        let fixture = fixture();
        for generation in 1..=8 {
            advertise(
                &fixture,
                "phone-a",
                advertisement("phone-a", generation, 1, false, true),
                1,
            );
            let mut store = fixture.client.lock().unwrap();
            let (conn, ctx) = store.parts();
            message_history_protocol::request(
                conn,
                &ctx,
                &json!({"target_device_id":"phone-a"}).to_string(),
            )
            .unwrap();
        }
        advertise(
            &fixture,
            "phone-a",
            advertisement("phone-a", 9, 1, false, true),
            1,
        );
        let attempt = start(&fixture);
        observe_empty_server(&fixture, attempt);
        assert!(
            drain(&fixture, attempt).is_empty(),
            "a refused source yields no phone request"
        );
        let status = fixture.client.history_search_status(Some(attempt)).unwrap();
        assert_eq!(status.state, HistorySearchState::Blocked);
        assert_eq!(status.reason, Some(HistorySearchReason::Capacity));
        assert!(
            status.can_search_further,
            "capacity is retryable while the source stays open"
        );
    }

    #[test]
    fn compaction_refusal_is_unsupported_and_is_never_retried_within_the_attempt() {
        let fixture = fixture();
        fixture
            .client
            .set_server_compaction_state(false, false)
            .unwrap();
        advertise(
            &fixture,
            "phone-b",
            advertisement("phone-b", 1, 1, false, true),
            1,
        );
        let attempt = start(&fixture);
        observe_empty_server(&fixture, attempt);
        assert!(drain(&fixture, attempt).is_empty());
        let status = fixture.client.history_search_status(Some(attempt)).unwrap();
        assert_eq!(status.state, HistorySearchState::Blocked);
        assert_eq!(status.reason, Some(HistorySearchReason::Unsupported));
        assert!(
            !status.can_search_further,
            "no dead retry offer for an unsupported server"
        );
    }

    #[test]
    fn locked_cancel_ends_local_interest_and_a_later_flush_cancels_the_remote_request() {
        let fixture = fixture();
        advertise(
            &fixture,
            "phone-a",
            advertisement("phone-a", 1, 1, false, true),
            1,
        );
        let attempt = start(&fixture);
        observe_empty_server(&fixture, attempt);
        let (request_id, _) = drain(&fixture, attempt).remove(0);
        let later = start(&fixture);
        let now = now_ms();
        {
            let mut store = fixture.client.lock().unwrap();
            let locked = Ctx {
                vault_id: store.config.vault_id,
                device_id: store.config.device_id,
                keys: &BTreeMap::new(),
                active_epoch: None,
            };
            let tx = store.conn.transaction().unwrap();
            tx.execute(
                "UPDATE message_history_attempts SET state='cancelled',reason=NULL,progress_version=progress_version+1,updated_at=? WHERE attempt_id=?",
                params![now, attempt.to_string()],
            )
            .unwrap();
            withdraw(&tx, &locked, &attempt.to_string()).unwrap();
            tx.commit().unwrap();
        }
        let local = fixture.client.history_search_status(Some(attempt)).unwrap();
        assert_eq!(
            local.state,
            HistorySearchState::Cancelled,
            "local interest ends even though the cancel was deferred"
        );
        assert_eq!(
            request_state(&fixture, &request_id),
            "queued",
            "the remote cancel was deferred, not lost silently"
        );
        let pending: i64 = {
            let store = fixture.client.lock().unwrap();
            store
                .conn
                .query_row(
                    "SELECT cancel_pending FROM message_history_attempt_sources WHERE request_id=?",
                    [&request_id],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(pending, 1);

        // Any unlocked call flushes the deferred cancel once no live attempt still needs it.
        fixture.client.history_search_work(later).unwrap();
        assert_eq!(request_state(&fixture, &request_id), "cancelled");
    }

    /// Publishes this device's own advertisement. Its requests are then answered through the
    /// real local progress path, because the requester is also the target.
    fn publish_self(fixture: &Fixture, permission: bool, generation: i64) {
        fixture
            .client
            .publish_message_history_source(
                &json!({"generation": generation.to_string(), "capabilities": {"sms": true, "mms": false}, "permission": permission, "policy": true, "exhausted": false}).to_string(),
            )
            .unwrap();
    }

    fn self_progress(fixture: &Fixture, request_id: &str, state: &str, reason: &str) {
        let mut record = json!({"schema_version":1,"request_id":request_id,"target":fixture.device,"generation":"1","key_epoch":1,"state":state,"receipts":[]});
        record["reason"] = json!(reason);
        fixture
            .client
            .record_message_history_progress(&record.to_string())
            .unwrap();
    }

    fn status_of(fixture: &Fixture, attempt: Uuid) -> HistorySearchStatus {
        fixture.client.history_search_status(Some(attempt)).unwrap()
    }

    fn request_rows(fixture: &Fixture) -> i64 {
        let store = fixture.client.lock().unwrap();
        store
            .conn
            .query_row("SELECT COUNT(*) FROM message_history_requests", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn protocol_wire_reasons_map_at_the_boundary_through_real_progress_and_status() {
        let fixture = fixture();
        publish_self(&fixture, true, 1);
        // Each row: real progress state and wire reason, then the bridge state and reason the
        // status projection must report. Only the zero-response path may ever be `Offline`.
        let cases = [
            (
                "blocked",
                "permission_denied",
                HistorySearchState::Blocked,
                Some(HistorySearchReason::Permission),
            ),
            (
                "blocked",
                "policy_denied",
                HistorySearchState::Blocked,
                Some(HistorySearchReason::Capacity),
            ),
            (
                "blocked",
                "key_epoch_changed",
                HistorySearchState::Blocked,
                Some(HistorySearchReason::NeedsUnlock),
            ),
            (
                "blocked",
                "generation_changed",
                HistorySearchState::Blocked,
                Some(HistorySearchReason::Unsupported),
            ),
            (
                "blocked",
                "source_revoked",
                HistorySearchState::Blocked,
                Some(HistorySearchReason::Unsupported),
            ),
            (
                "blocked",
                "source_exhausted",
                HistorySearchState::Failed,
                None,
            ),
            ("failed", "provider_error", HistorySearchState::Failed, None),
            ("failed", "storage_error", HistorySearchState::Failed, None),
            ("failed", "internal_error", HistorySearchState::Failed, None),
            (
                "partial",
                "budget_exhausted",
                HistorySearchState::Partial,
                None,
            ),
            (
                "partial",
                "source_unavailable",
                HistorySearchState::Partial,
                None,
            ),
            (
                "expired",
                "expired",
                HistorySearchState::Failed,
                Some(HistorySearchReason::Timeout),
            ),
        ];
        for (state, wire, expected_state, expected_reason) in cases {
            let attempt = start(&fixture);
            observe_empty_server(&fixture, attempt);
            let admitted = drain(&fixture, attempt);
            assert_eq!(
                admitted.len(),
                1,
                "{state}/{wire}: one self request is admitted"
            );
            self_progress(&fixture, &admitted[0].0, state, wire);
            let status = status_of(&fixture, attempt);
            assert_eq!(
                (status.state, status.reason),
                (expected_state, expected_reason),
                "{state}/{wire}"
            );
            assert_ne!(
                status.reason,
                Some(HistorySearchReason::Offline),
                "{state}/{wire}"
            );
            fixture.client.cancel_history_search(attempt).unwrap();
        }
    }

    #[test]
    fn stored_permission_block_offers_retry_only_while_a_current_eligible_source_exists() {
        let fixture = fixture();
        publish_self(&fixture, false, 1);
        let blocked = start(&fixture);
        observe_empty_server(&fixture, blocked);
        let stored = status_of(&fixture, blocked);
        assert_eq!(
            (stored.state, stored.reason),
            (
                HistorySearchState::Blocked,
                Some(HistorySearchReason::Permission)
            )
        );
        assert!(
            !stored.can_search_further,
            "no eligible source yet, so no retry offer"
        );
        let requests_before = request_rows(&fixture);

        // A newer advertisement restores permission. The stored block remains, but the offer
        // now reflects the current eligible source, and reading status creates no request.
        publish_self(&fixture, true, 2);
        let restored = status_of(&fixture, blocked);
        assert_eq!(
            (restored.state, restored.reason),
            (
                HistorySearchState::Blocked,
                Some(HistorySearchReason::Permission)
            )
        );
        assert!(restored.can_search_further);
        assert_eq!(
            request_rows(&fixture),
            requests_before,
            "status reads never enqueue requests"
        );

        // Retry is an explicit new start, which admits a real request.
        let retry = start(&fixture);
        observe_empty_server(&fixture, retry);
        let admitted = drain(&fixture, retry);
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].1, fixture.device);
    }

    fn insert_indexed_message(fixture: &Fixture, id: &str, device: &str, sequence: i64) {
        let store = fixture.client.lock().unwrap();
        store
            .conn
            .execute(
                "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen) VALUES(?,?,?,?,?,0)",
                params![id, "conversation", device, sequence, r#"{"body":"hello"}"#],
            )
            .unwrap();
    }

    /// One phone, admitted, completed and exhausted, with the terminal state persisted.
    fn complete_phone_a(fixture: &Fixture) -> Uuid {
        advertise(
            fixture,
            "phone-a",
            advertisement("phone-a", 1, 1, false, true),
            1,
        );
        let attempt = start(fixture);
        observe_empty_server(fixture, attempt);
        let admitted = drain(fixture, attempt);
        assert_eq!(admitted.len(), 1);
        phone_record(fixture, "phone-a", &admitted[0].0, 1, "completed", true);
        fixture.client.history_search_work(attempt).unwrap();
        let status = status_of(fixture, attempt);
        assert_eq!(status.state, HistorySearchState::Completed);
        attempt
    }

    #[test]
    fn completion_waits_for_index_readiness_and_the_current_snapshot_generation() {
        let fixture = fixture();
        advertise(
            &fixture,
            "phone-a",
            advertisement("phone-a", 1, 1, false, true),
            1,
        );
        let attempt = start(&fixture);
        observe_empty_server(&fixture, attempt);
        let (request_id, _) = drain(&fixture, attempt).remove(0);
        let message = Uuid::new_v4().to_string();
        let receipts = json!([{"messageId": message, "revision": "1"}]);
        phone_record_with(
            &fixture,
            "phone-a",
            &request_id,
            1,
            "completed",
            true,
            receipts,
        );
        assert_eq!(
            status_of(&fixture, attempt).state,
            HistorySearchState::Indexing,
            "a receipt that is not yet indexed keeps the attempt indexing"
        );

        insert_indexed_message(&fixture, &message, "phone-a", 1);
        assert_eq!(
            status_of(&fixture, attempt).state,
            HistorySearchState::Completed,
            "the indexed receipt and the proof complete the attempt"
        );

        {
            let store = fixture.client.lock().unwrap();
            store
                .conn
                .execute(
                    "INSERT OR REPLACE INTO metadata(k,v) VALUES('snapshot_generation','1')",
                    [],
                )
                .unwrap();
        }
        let stale = status_of(&fixture, attempt);
        assert_eq!(
            stale.state,
            HistorySearchState::CatchingUp,
            "a snapshot generation change after admission invalidates the proof"
        );
        assert!(!stale.can_search_further);
    }

    #[test]
    fn completed_reoffers_only_a_source_version_this_attempt_does_not_cover() {
        let fixture = fixture();
        let attempt = complete_phone_a(&fixture);
        let covered = status_of(&fixture, attempt);
        assert!(
            !covered.can_search_further,
            "the just-completed source is covered even though its exhaustion advertisement lags"
        );
        let requests = request_rows(&fixture);

        advertise(
            &fixture,
            "phone-b",
            advertisement("phone-b", 1, 1, false, true),
            1,
        );
        let new_source = status_of(&fixture, attempt);
        assert_eq!(new_source.state, HistorySearchState::Completed);
        assert!(new_source.can_search_further, "a new source is offered");
        assert_eq!(
            request_rows(&fixture),
            requests,
            "status reads never enqueue requests"
        );
    }

    #[test]
    fn completed_reoffers_on_generation_rotation_and_on_a_newer_revision() {
        let rotated = fixture();
        let attempt = complete_phone_a(&rotated);
        advertise(
            &rotated,
            "phone-a",
            advertisement_at("phone-a", 2, "1", 1, false, true),
            1,
        );
        assert!(status_of(&rotated, attempt).can_search_further);

        let revised = fixture();
        let attempt = complete_phone_a(&revised);
        advertise(
            &revised,
            "phone-a",
            advertisement_at("phone-a", 1, "2", 1, false, true),
            1,
        );
        assert!(status_of(&revised, attempt).can_search_further);
    }

    #[test]
    fn completed_without_a_captured_revision_is_not_treated_as_covered() {
        let fixture = fixture();
        let attempt = complete_phone_a(&fixture);
        {
            let store = fixture.client.lock().unwrap();
            store
                .conn
                .execute(
                    "UPDATE message_history_requests SET source_revision=NULL",
                    [],
                )
                .unwrap();
        }
        assert!(
            status_of(&fixture, attempt).can_search_further,
            "an unknown coverage version is offered again, never assumed complete"
        );
    }

    #[test]
    fn unsupported_rearms_only_when_the_server_can_admit_an_open_source() {
        let fixture = fixture();
        fixture
            .client
            .set_server_compaction_state(false, false)
            .unwrap();
        advertise(
            &fixture,
            "phone-a",
            advertisement("phone-a", 1, 1, false, true),
            1,
        );
        let refused_attempt = start(&fixture);
        observe_empty_server(&fixture, refused_attempt);
        assert!(drain(&fixture, refused_attempt).is_empty());
        fixture.client.history_search_work(refused_attempt).unwrap();
        let refused = status_of(&fixture, refused_attempt);
        assert_eq!(
            (refused.state, refused.reason),
            (
                HistorySearchState::Blocked,
                Some(HistorySearchReason::Unsupported)
            )
        );
        assert!(
            !refused.can_search_further,
            "an unsupported server is not offered"
        );

        fixture
            .client
            .set_server_compaction_state(true, true)
            .unwrap();
        let recovered = status_of(&fixture, refused_attempt);
        assert_eq!(
            (recovered.state, recovered.reason),
            (
                HistorySearchState::Blocked,
                Some(HistorySearchReason::Unsupported)
            ),
            "the stored attempt stays terminal; only the offer changes"
        );
        assert!(
            recovered.can_search_further,
            "recovery re-offers the search"
        );

        let retry = start(&fixture);
        observe_empty_server(&fixture, retry);
        assert_eq!(
            drain(&fixture, retry).len(),
            1,
            "the explicit retry admits a real request"
        );
    }
}

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS message_history_attempts(attempt_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL UNIQUE,state TEXT NOT NULL,reason TEXT,started_at_ms INTEGER NOT NULL DEFAULT 0,deadline_ms INTEGER NOT NULL DEFAULT 0,catchup_deadline_ms INTEGER NOT NULL DEFAULT 0,rotation INTEGER NOT NULL DEFAULT 0,progress_version INTEGER NOT NULL DEFAULT 0,proof_high_water INTEGER,proof_generation INTEGER,updated_at INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE IF NOT EXISTS message_history_attempt_sources(attempt_id TEXT NOT NULL,target_device_id TEXT NOT NULL,request_id TEXT,reason TEXT,PRIMARY KEY(attempt_id,target_device_id));",
    )?;
    let unique_request: bool = conn.query_row(
        "SELECT COALESCE((SELECT sql LIKE '%UNIQUE%' FROM sqlite_master WHERE type='table' AND name='message_history_attempt_sources'),0)",
        [],
        |row| row.get(0),
    )?;
    if unique_request {
        // Pre-release scratch shape: one request ID could not serve two attempts. Rebuild it
        // atomically so existing rows survive while the per-attempt primary key takes over.
        conn.execute_batch("SAVEPOINT history_attempt_sources_rebuild")?;
        let rebuilt = conn.execute_batch(
            "CREATE TABLE message_history_attempt_sources_next(attempt_id TEXT NOT NULL,target_device_id TEXT NOT NULL,request_id TEXT,reason TEXT,PRIMARY KEY(attempt_id,target_device_id));
             INSERT INTO message_history_attempt_sources_next(attempt_id,target_device_id,request_id,reason) SELECT attempt_id,target_device_id,request_id,NULL FROM message_history_attempt_sources;
             DROP TABLE message_history_attempt_sources;
             ALTER TABLE message_history_attempt_sources_next RENAME TO message_history_attempt_sources;",
        );
        match rebuilt {
            Ok(()) => conn.execute_batch("RELEASE history_attempt_sources_rebuild")?,
            Err(error) => {
                conn.execute_batch(
                    "ROLLBACK TO history_attempt_sources_rebuild; RELEASE history_attempt_sources_rebuild",
                )?;
                return Err(error.into());
            }
        }
    }
    ensure_column(
        conn,
        "message_history_attempts",
        "started_at_ms",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "message_history_attempts",
        "deadline_ms",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "message_history_attempts",
        "catchup_deadline_ms",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "message_history_attempts",
        "rotation",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "message_history_attempts",
        "progress_version",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "message_history_attempts",
        "proof_high_water",
        "INTEGER",
    )?;
    ensure_column(
        conn,
        "message_history_attempts",
        "proof_generation",
        "INTEGER",
    )?;
    ensure_column(
        conn,
        "message_history_attempts",
        "updated_at",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "message_history_attempt_sources", "reason", "TEXT")?;
    ensure_column(
        conn,
        "message_history_attempt_sources",
        "cancel_pending",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    drop_legacy_column(conn, "message_history_attempts", "deadline")?;
    drop_legacy_column(conn, "message_history_attempts", "catchup_deadline")?;
    drop_legacy_column(conn, "message_history_attempts", "index_version")?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS message_history_attempt_sources_request ON message_history_attempt_sources(request_id);",
    )?;
    Ok(())
}
