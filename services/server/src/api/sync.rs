//! Durable sync surfaces: retained replay, immutable ciphertext snapshots,
//! pending command references and the in-process outbox/retention maintenance.
//!
//! The server only stores and orders opaque envelopes. Snapshot records are
//! `cursor + Envelope`; clients stage-import them and never execute historical
//! commands from a snapshot.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Duration;

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeStruct};
use serde_json::{Value, json, value::RawValue};
use sqlx::{PgPool, Postgres, Row, Transaction};
use tokio::sync::broadcast;
use uuid::Uuid;

use super::{
    ApiError, ApiResult, ApiState, Operation, Principal, auth_with_mode, authorize,
    database_unavailable, still_active,
};
use crate::storage::Storage;

pub(super) const MAX_PAGE: u16 = 200;
const MAX_MANIFEST_PAGE: u16 = 5_000;
/// Serialized-envelope byte budget for one replay, snapshot or WebSocket page.
/// The first row is always included, so a page always advances; a single
/// envelope is at most ~1.4 MB of JSON.
pub(super) const PAGE_BYTE_BUDGET: i64 = 8 * 1024 * 1024;
const DEFAULT_PAGE: u16 = 100;
/// Upper bound on transport-log rows removed for one vault per maintenance pass.
const PRUNE_BATCH_ROWS: i64 = 10_000;
/// Upper bound on vaults examined per maintenance pass.
const PRUNE_BATCH_VAULTS: i64 = 100;
/// Clamp for interval arithmetic so configured durations cannot overflow PostgreSQL.
const MAX_INTERVAL_MILLIS: i64 = 10 * 365 * 86_400 * 1_000;

/// Transport timing and retention policy. `router` derives it from the
/// environment; tests and embedders can pass explicit values.
#[derive(Clone, Debug)]
pub struct TransportOptions {
    /// Transport-log replay retention (default 30 days): the longest history is kept. Cursors below the replay floor receive `resync_required`.
    pub replay_retention: Duration,
    /// How often a live WebSocket rescans committed rows without any notice.
    pub durable_replay_interval: Duration,
    /// Time allowed for the client hello after the upgrade.
    pub hello_timeout: Duration,
    /// Time allowed for one frame write before the socket is closed for backpressure.
    pub send_timeout: Duration,
    /// Cadence for WebSocket control Pings while a peer is present.
    pub heartbeat_interval: Duration,
    /// Cadence for adaptive WebSocket control Pings while a device is alone.
    pub idle_heartbeat_interval: Duration,
    /// Outbox drain cadence.
    pub outbox_drain_interval: Duration,
    /// Maximum outbox jobs claimed or deleted per drain pass.
    pub outbox_batch_size: i64,
    /// How long delivered outbox references are kept before deletion.
    pub outbox_delivered_retention: Duration,
    /// Transport-log pruning cadence.
    pub replay_prune_interval: Duration,
    /// Storage cleanup cadence (expired reservations/copies and queued deletes).
    pub storage_cleanup_interval: Duration,
    /// Maximum rows retired and objects deleted per storage cleanup pass.
    pub storage_cleanup_batch: i64,
}

impl Default for TransportOptions {
    fn default() -> Self {
        Self {
            replay_retention: Duration::from_secs(
                u64::from(crate::config::DEFAULT_REPLAY_RETENTION_DAYS) * 86_400,
            ),
            durable_replay_interval: Duration::from_secs(5),
            hello_timeout: Duration::from_secs(10),
            send_timeout: Duration::from_secs(10),
            heartbeat_interval: Duration::from_secs(20),
            idle_heartbeat_interval: Duration::from_secs(50),
            outbox_drain_interval: Duration::from_secs(1),
            outbox_batch_size: 256,
            outbox_delivered_retention: Duration::from_secs(3_600),
            replay_prune_interval: Duration::from_secs(300),
            storage_cleanup_interval: Duration::from_secs(30),
            storage_cleanup_batch: 16,
        }
    }
}

fn interval_millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis())
        .unwrap_or(i64::MAX)
        .min(MAX_INTERVAL_MILLIS)
}

/// Parses a decimal cursor string. Cursors are non-negative JSON strings.
pub(super) fn parse_cursor(value: &str) -> ApiResult<i64> {
    value
        .parse::<i64>()
        .ok()
        .filter(|cursor| *cursor >= 0)
        .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_cursor"))
}

fn page_limit(limit: Option<u16>) -> i64 {
    i64::from(limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE))
}

/// A cursor the transport log cannot serve. Clients recover with a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Resync {
    pub reason: ResyncReason,
    pub high_water: i64,
    pub replay_floor: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ResyncReason {
    /// The cursor is older than retained transport history.
    CursorExpired,
    /// The cursor is beyond current server state, for example after a restore/rollback.
    CursorAhead,
    /// Physical compaction changed the retained snapshot set since the cut began.
    CompactionGenerationChanged,
}

impl ResyncReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::CursorExpired => "cursor_expired",
            Self::CursorAhead => "cursor_ahead",
            Self::CompactionGenerationChanged => "compaction_generation_changed",
        }
    }
}

impl Resync {
    fn classify(cursor: i64, high_water: i64, replay_floor: i64) -> Option<Self> {
        let reason = if cursor > high_water {
            ResyncReason::CursorAhead
        } else if cursor < replay_floor {
            ResyncReason::CursorExpired
        } else {
            return None;
        };
        Some(Self {
            reason,
            high_water,
            replay_floor,
        })
    }

    fn fields(&self) -> serde_json::Map<String, Value> {
        let mut fields = serde_json::Map::new();
        fields.insert("reason".into(), json!(self.reason.as_str()));
        fields.insert(
            "high_water_cursor".into(),
            json!(self.high_water.to_string()),
        );
        fields.insert(
            "replay_floor_cursor".into(),
            json!(self.replay_floor.to_string()),
        );
        fields.insert("snapshot_path".into(), json!("/v1/snapshot"));
        fields
    }

    /// WebSocket frame sent immediately before close code 4409.
    pub(super) fn ws_frame(&self) -> String {
        let mut fields = self.fields();
        fields.insert("type".into(), json!("resync_required"));
        Value::Object(fields).to_string()
    }
}

impl IntoResponse for Resync {
    fn into_response(self) -> Response {
        let mut fields = self.fields();
        fields.insert("code".into(), json!("resync_required"));
        (StatusCode::CONFLICT, Json(Value::Object(fields))).into_response()
    }
}

/// Current vault watermarks read inside the caller's transaction.
struct Watermarks {
    high_water: i64,
    replay_floor: i64,
    compaction_generation: i64,
}

async fn watermarks(
    tx: &mut Transaction<'_, Postgres>,
    vault: Uuid,
) -> Result<Watermarks, sqlx::Error> {
    let row = sqlx::query("SELECT next_cursor,replay_floor_cursor,compaction_generation FROM vaults WHERE vault_id=$1")
        .bind(vault)
        .fetch_one(&mut **tx)
        .await?;
    Ok(Watermarks {
        high_water: row.get("next_cursor"),
        replay_floor: row.get("replay_floor_cursor"),
        compaction_generation: row.get("compaction_generation"),
    })
}

async fn read_only_snapshot(
    db: &PgPool,
    vault: Uuid,
) -> Result<Transaction<'static, Postgres>, sqlx::Error> {
    crate::scope::begin_read_only_repeatable(db, vault).await
}

/// One committed envelope, kept as the database's serialized JSON so pages
/// are moved into the response without building or cloning a `Value` tree.
pub(super) struct CursorRecord {
    pub cursor: i64,
    pub envelope: Box<RawValue>,
}

impl Serialize for CursorRecord {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut record = serializer.serialize_struct("CursorRecord", 2)?;
        record.serialize_field("cursor", &self.cursor.to_string())?;
        record.serialize_field("envelope", &self.envelope)?;
        record.end()
    }
}

/// WebSocket `event` frame.
pub(super) struct EventFrame(CursorRecord);

impl EventFrame {
    pub(super) fn new(record: CursorRecord) -> Self {
        Self(record)
    }
}

impl Serialize for EventFrame {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut frame = serializer.serialize_struct("EventFrame", 3)?;
        frame.serialize_field("type", "event")?;
        frame.serialize_field("cursor", &self.0.cursor.to_string())?;
        frame.serialize_field("envelope", &self.0.envelope)?;
        frame.end()
    }
}

/// Source table for a budgeted page. Both are trusted constants.
#[derive(Clone, Copy)]
enum PageSource {
    TransportLog,
    ImmutableRecords,
}

/// Reads rows in `(after, upper]` in cursor order, at most `limit` rows and at
/// most `PAGE_BYTE_BUDGET` serialized bytes (always at least one row). The
/// budget is applied in SQL, so oversized pages are never transferred.
async fn fetch_budgeted(
    tx: &mut Transaction<'_, Postgres>,
    source: PageSource,
    vault: Uuid,
    after: i64,
    upper: i64,
    limit: i64,
) -> Result<Vec<CursorRecord>, sqlx::Error> {
    let table = match source {
        PageSource::TransportLog => "event_log",
        PageSource::ImmutableRecords => "encrypted_records",
    };
    let rows = sqlx::query(&format!("SELECT cursor,body FROM (SELECT cursor,body,row_number() OVER w AS position,sum(octet_length(body)) OVER w AS running FROM (SELECT cursor,envelope::text AS body FROM {table} WHERE vault_id=$1 AND cursor>$2 AND cursor<=$3 ORDER BY cursor LIMIT $4) page WINDOW w AS (ORDER BY cursor)) sized WHERE position=1 OR running<=$5 ORDER BY cursor"))
        .bind(vault)
        .bind(after)
        .bind(upper)
        .bind(limit)
        .bind(PAGE_BYTE_BUDGET)
        .fetch_all(&mut **tx)
        .await?;
    rows.into_iter()
        .map(|row| {
            let body: String = row.get("body");
            Ok(CursorRecord {
                cursor: row.get("cursor"),
                envelope: RawValue::from_string(body)
                    .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
            })
        })
        .collect()
}

/// Cursor to request next, or `None` when `records` reached `upper`.
fn next_after(records: &[CursorRecord], upper: i64) -> Option<String> {
    next_after_cursor(records.last().map(|record| record.cursor), upper)
}

fn next_after_cursor(last: Option<i64>, upper: i64) -> Option<String> {
    last.filter(|cursor| *cursor < upper)
        .map(|cursor| cursor.to_string())
}

pub(super) enum ReplayPage {
    Rows {
        high_water: i64,
        replay_floor: i64,
        rows: Vec<CursorRecord>,
    },
    Resync(Resync),
}

/// Reads one budgeted replay page after `after` from a single consistent
/// snapshot of the vault watermarks and transport log. Pruning moves the floor
/// and deletes rows atomically, so a page never silently skips a pruned cursor.
pub(super) async fn replay_page(
    db: &PgPool,
    vault: Uuid,
    after: i64,
    limit: i64,
) -> Result<ReplayPage, sqlx::Error> {
    let mut tx = read_only_snapshot(db, vault).await?;
    let marks = watermarks(&mut tx, vault).await?;
    if let Some(resync) = Resync::classify(after, marks.high_water, marks.replay_floor) {
        tx.commit().await?;
        return Ok(ReplayPage::Resync(resync));
    }
    let rows = fetch_budgeted(
        &mut tx,
        PageSource::TransportLog,
        vault,
        after,
        marks.high_water,
        limit,
    )
    .await?;
    tx.commit().await?;
    Ok(ReplayPage::Rows {
        high_water: marks.high_water,
        replay_floor: marks.replay_floor,
        rows,
    })
}

/// Watermarks used by the WebSocket handshake before replay begins.
pub(super) async fn position(
    db: &PgPool,
    vault: Uuid,
    cursor: i64,
) -> Result<Result<(i64, i64), Resync>, sqlx::Error> {
    let mut tx = read_only_snapshot(db, vault).await?;
    let marks = watermarks(&mut tx, vault).await?;
    tx.commit().await?;
    Ok(
        match Resync::classify(cursor, marks.high_water, marks.replay_floor) {
            Some(resync) => Err(resync),
            None => Ok((marks.high_water, marks.replay_floor)),
        },
    )
}

#[derive(Serialize)]
struct EventsPage {
    high_water_cursor: String,
    replay_floor_cursor: String,
    next_after: Option<String>,
    events: Vec<CursorRecord>,
}

#[derive(Serialize)]
struct SnapshotPage {
    high_water_cursor: String,
    compaction_generation: String,
    next_after: Option<String>,
    records: Vec<CursorRecord>,
}

#[derive(Deserialize)]
pub(super) struct EventsQuery {
    after: Option<String>,
    limit: Option<u16>,
}

/// `GET /v1/events?after=&limit=` — retained transport replay.
pub(super) async fn events(
    State(s): State<ApiState>,
    h: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> Result<Response, ApiError> {
    let p = auth_with_mode(&s, &h).await?;
    authorize(&s, &p, Operation::Sync).await?;
    if !still_active(&s, &p)
        .await
        .map_err(|error| database_unavailable(&error, "events_replay_revalidate"))?
    {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_bearer"));
    }
    let after = parse_cursor(q.after.as_deref().unwrap_or("0"))?;
    match replay_page(&s.db, p.vault, after, page_limit(q.limit))
        .await
        .map_err(|error| database_unavailable(&error, "events_replay"))?
    {
        ReplayPage::Resync(resync) => Ok(resync.into_response()),
        ReplayPage::Rows {
            high_water,
            replay_floor,
            rows,
        } => {
            let next = next_after(&rows, high_water);
            record_device_cursor(&s.db, p.vault, p.device, after, next.is_none(), false).await;
            Ok(Json(EventsPage {
                high_water_cursor: high_water.to_string(),
                replay_floor_cursor: replay_floor.to_string(),
                next_after: next,
                events: rows,
            })
            .into_response())
        }
    }
}

/// Best-effort durable replay progress. A failed report never changes the
/// replay response or WebSocket handshake outcome.
///
/// A report that loses the race with pruning is simply not recorded. Its
/// device's next request is classified by `Resync::classify` against the real
/// floor, so the device either continues (its cursor is at or above the floor)
/// or is told to resync. Recorded cursors bound how far the floor moves; they
/// are not what makes replay correct.
///
/// Lock order: the vault row first, then `device_cursors`. Pruning and
/// revocation take the vault row first too, so no path waits for the vault
/// row while holding a `device_cursors` row. The decision read takes no lock; the
/// share lock on the vault row is taken only when a write is due. It waits for
/// the pruner's `FOR UPDATE`, so the floor read after it is the committed floor.
pub(super) async fn record_device_cursor(
    db: &PgPool,
    vault: Uuid,
    device: Uuid,
    cursor: i64,
    last_page: bool,
    hello: bool,
) {
    let result = async {
        let mut tx = crate::scope::begin(db, vault).await?;
        // Mirrors the upsert's WHERE below; the upsert stays the final arbiter.
        let stored: Option<(i64, bool)> = sqlx::query_as("SELECT receive_cursor, reported_at < now()-interval '5 minutes' FROM device_cursors WHERE vault_id=$1 AND device_id=$2")
            .bind(vault)
            .bind(device)
            .fetch_optional(&mut *tx)
            .await?;
        let due = match stored {
            None => true,
            Some((stored, stale)) => {
                cursor < stored || stale || (cursor != stored && (last_page || hello))
            }
        };
        if !due {
            return tx.commit().await;
        }
        // A cursor below the floor read under the share lock was pruned: not recorded.
        let floor: i64 = sqlx::query_scalar("SELECT replay_floor_cursor FROM vaults WHERE vault_id=$1 FOR SHARE")
            .bind(vault)
            .fetch_one(&mut *tx)
            .await?;
        if cursor < floor {
            return tx.commit().await;
        }
        sqlx::query("INSERT INTO device_cursors(vault_id,device_id,receive_cursor,reported_at) VALUES($1,$2,$3,now()) ON CONFLICT(vault_id,device_id) DO UPDATE SET receive_cursor=EXCLUDED.receive_cursor,reported_at=now() WHERE EXCLUDED.receive_cursor < device_cursors.receive_cursor OR device_cursors.reported_at < now()-interval '5 minutes' OR (EXCLUDED.receive_cursor <> device_cursors.receive_cursor AND ($4 OR $5))")
            .bind(vault)
            .bind(device)
            .bind(cursor)
            .bind(last_page)
            .bind(hello)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(error_kind = %error, "device cursor report failed");
    }
}

/// `GET /v1/snapshot` — fixes a cut for immutable ciphertext record paging.
pub(super) async fn snapshot_start(
    State(s): State<ApiState>,
    h: HeaderMap,
) -> ApiResult<Json<Value>> {
    let p = auth_with_mode(&s, &h).await?;
    authorize(&s, &p, Operation::Export).await?;
    let read = async {
        let mut tx = read_only_snapshot(&s.db, p.vault).await?;
        let marks = watermarks(&mut tx, p.vault).await?;
        let row = sqlx::query("SELECT key_epoch,profile_fingerprint FROM vaults WHERE vault_id=$1")
            .bind(p.vault)
            .fetch_one(&mut *tx)
            .await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM encrypted_records WHERE vault_id=$1 AND cursor<=$2",
        )
        .bind(p.vault)
        .bind(marks.high_water)
        .fetch_one(&mut *tx)
        .await?;
        let compaction_active: bool = sqlx::query_scalar(ROSTER_FENCED)
            .bind(p.vault)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok::<_, sqlx::Error>((marks, row, count, compaction_active))
    };
    let (marks, row, count, compaction_active) = read
        .await
        .map_err(|error| database_unavailable(&error, "snapshot_start"))?;
    // `compaction_supported` means this server stores compaction metadata and
    // fences snapshot pages by generation, so clients always send the fence.
    // Physical deletion is separately gated on every non-revoked device having
    // declared that it fences (`compaction_active`).
    Ok(Json(json!({
        "snapshot_version": 1,
        "compaction_supported": true,
        "origin_tags_supported": true,
        "compaction_active": compaction_active,
        "compaction_generation": marks.compaction_generation.to_string(),
        "vault_id": p.vault,
        "high_water_cursor": marks.high_water.to_string(),
        "record_count": count.to_string(),
        "replay_floor_cursor": marks.replay_floor.to_string(),
        "key_epoch": row.get::<i32, _>("key_epoch"),
        "profile_fingerprint": row.get::<String, _>("profile_fingerprint"),
        "max_page_size": MAX_PAGE,
        "manifest_supported": true,
        "max_manifest_page_size": MAX_MANIFEST_PAGE,
    })))
}

#[derive(Deserialize)]
pub(super) struct SnapshotPageQuery {
    high_water: String,
    compaction_generation: Option<String>,
    after: Option<String>,
    limit: Option<u16>,
}

struct SnapshotPageRequest {
    high_water: i64,
    compaction_generation: Option<i64>,
    after: i64,
}

async fn snapshot_preamble(
    state: &ApiState,
    principal: &Principal,
    query: &SnapshotPageQuery,
    operation: &'static str,
) -> Result<Result<(SnapshotPageRequest, Transaction<'static, Postgres>), Resync>, ApiError> {
    let high_water = parse_cursor(&query.high_water)?;
    let compaction_generation = query
        .compaction_generation
        .as_deref()
        .map(parse_cursor)
        .transpose()?;
    let after = parse_cursor(query.after.as_deref().unwrap_or("0"))?;
    if after > high_water {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_cursor"));
    }
    if compaction_generation.is_some() {
        // Sending the fence is itself the device's declaration that it restarts
        // a cut when the generation changes.
        declare_compaction_fence(&state.db, principal.vault, principal.device)
            .await
            .map_err(|error| database_unavailable(&error, "snapshot_compaction_declare"))?;
    }
    let mut tx = read_only_snapshot(&state.db, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, operation))?;
    let marks = watermarks(&mut tx, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, operation))?;
    // Pre-compaction clients remain compatible until physical deletion. At
    // that point a missing generation fails closed instead of returning a
    // partial fixed-cut import.
    let generation_changed = compaction_generation
        .map_or(marks.compaction_generation != 0, |generation| {
            generation != marks.compaction_generation
        });
    if high_water > marks.high_water || generation_changed {
        tx.commit()
            .await
            .map_err(|error| database_unavailable(&error, operation))?;
        return Ok(Err(Resync {
            reason: if high_water > marks.high_water {
                ResyncReason::CursorAhead
            } else {
                ResyncReason::CompactionGenerationChanged
            },
            high_water: marks.high_water,
            replay_floor: marks.replay_floor,
        }));
    }
    Ok(Ok((
        SnapshotPageRequest {
            high_water,
            compaction_generation,
            after,
        },
        tx,
    )))
}

/// `GET /v1/snapshot/records?high_water=&after=&limit=` — one stable page under a fixed cut.
///
/// Records are immutable and cursors are allocated in commit order under the
/// vault lock, so every page at or below `high_water` is stable without holding
/// a server-side snapshot session.
pub(super) async fn snapshot_records(
    State(s): State<ApiState>,
    h: HeaderMap,
    Query(q): Query<SnapshotPageQuery>,
) -> Result<Response, ApiError> {
    let p = auth_with_mode(&s, &h).await?;
    authorize(&s, &p, Operation::Export).await?;
    let limit = page_limit(q.limit);
    let (request, mut tx) = match snapshot_preamble(&s, &p, &q, "snapshot_records").await? {
        Ok(context) => context,
        Err(resync) => return Ok(resync.into_response()),
    };
    let rows = fetch_budgeted(
        &mut tx,
        PageSource::ImmutableRecords,
        p.vault,
        request.after,
        request.high_water,
        limit,
    )
    .await
    .map_err(|error| database_unavailable(&error, "snapshot_records"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "snapshot_records"))?;
    Ok(Json(SnapshotPage {
        high_water_cursor: request.high_water.to_string(),
        compaction_generation: request
            .compaction_generation
            .unwrap_or_default()
            .to_string(),
        next_after: next_after(&rows, request.high_water),
        records: rows,
    })
    .into_response())
}

#[derive(Serialize)]
struct SnapshotManifestEntry {
    cursor: String,
    digest: String,
    flags: u8,
}

#[derive(Serialize)]
struct SnapshotManifestPage {
    high_water_cursor: String,
    compaction_generation: String,
    next_after: Option<String>,
    entries: Vec<SnapshotManifestEntry>,
}

async fn fetch_manifest(
    tx: &mut Transaction<'_, Postgres>,
    vault: Uuid,
    after: i64,
    upper: i64,
    limit: i64,
) -> Result<Vec<SnapshotManifestEntry>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT cursor, cipher_digest,
                COALESCE(jsonb_typeof(envelope->'compaction') <> 'null', FALSE) AS has_compaction,
                COALESCE(jsonb_typeof(envelope->'origin_tag') <> 'null', FALSE) AS has_origin_tag
         FROM encrypted_records
         WHERE vault_id=$1 AND cursor>$2 AND cursor<=$3
         ORDER BY cursor
         LIMIT $4",
    )
    .bind(vault)
    .bind(after)
    .bind(upper)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| SnapshotManifestEntry {
            cursor: row.get::<i64, _>("cursor").to_string(),
            digest: hex::encode(row.get::<Vec<u8>, _>("cipher_digest")),
            flags: u8::from(row.get::<bool, _>("has_compaction"))
                | (u8::from(row.get::<bool, _>("has_origin_tag")) << 1),
        })
        .collect())
}

/// `GET /v1/snapshot/manifest?high_water=&after=&limit=` — record digests for verified resync.
pub(super) async fn snapshot_manifest(
    State(s): State<ApiState>,
    h: HeaderMap,
    Query(q): Query<SnapshotPageQuery>,
) -> Result<Response, ApiError> {
    let p = auth_with_mode(&s, &h).await?;
    authorize(&s, &p, Operation::Export).await?;
    let limit = i64::from(q.limit.unwrap_or(1_000).clamp(1, MAX_MANIFEST_PAGE));
    let (request, mut tx) = match snapshot_preamble(&s, &p, &q, "snapshot_manifest").await? {
        Ok(context) => context,
        Err(resync) => return Ok(resync.into_response()),
    };
    let entries = fetch_manifest(&mut tx, p.vault, request.after, request.high_water, limit)
        .await
        .map_err(|error| database_unavailable(&error, "snapshot_manifest"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "snapshot_manifest"))?;
    Ok(Json(SnapshotManifestPage {
        high_water_cursor: request.high_water.to_string(),
        compaction_generation: request
            .compaction_generation
            .unwrap_or_default()
            .to_string(),
        next_after: next_after_cursor(
            entries
                .last()
                .and_then(|entry| entry.cursor.parse::<i64>().ok()),
            request.high_water,
        ),
        entries,
    })
    .into_response())
}

#[derive(Deserialize)]
pub(super) struct PendingQuery {
    after: Option<String>,
    limit: Option<u16>,
}

/// `GET /v1/commands/pending` — references to commands without a receipt from
/// their original target gateway. Gateways see only commands targeted to them.
/// This is read-only reconciliation data; it never retargets or re-identifies.
pub(super) async fn pending_commands(
    State(s): State<ApiState>,
    h: HeaderMap,
    Query(q): Query<PendingQuery>,
) -> ApiResult<Json<Value>> {
    let p = auth_with_mode(&s, &h).await?;
    authorize(&s, &p, Operation::Sync).await?;
    let after = parse_cursor(q.after.as_deref().unwrap_or("0"))?;
    let mut tx = crate::scope::begin(&s.db, p.vault)
        .await
        .map_err(|error| database_unavailable(&error, "pending_commands_begin"))?;
    let is_executor = p.role == "gateway"
        || (p.role == "owner"
            && sqlx::query_scalar::<_, i32>(
                "SELECT 1 FROM device_capabilities WHERE vault_id=$1 AND device_id=$2",
            )
            .bind(p.vault)
            .bind(p.device)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| database_unavailable(&error, "pending_commands_capability_lookup"))?
            .is_some());
    let gateway_filter = is_executor.then_some(p.device);
    let rows = sqlx::query("SELECT c.command_id,c.producer_device_id,c.gateway_device_id,c.cursor FROM commands c WHERE c.vault_id=$1 AND ($2::uuid IS NULL OR c.gateway_device_id=$2) AND c.cursor>$3 AND NOT EXISTS (SELECT 1 FROM command_receipts r WHERE r.vault_id=c.vault_id AND r.command_id=c.command_id AND r.gateway_device_id=c.gateway_device_id) AND NOT EXISTS (SELECT 1 FROM devices d WHERE d.vault_id=c.vault_id AND d.device_id=c.producer_device_id AND d.revoked_at IS NOT NULL) ORDER BY c.cursor LIMIT $4")
        .bind(p.vault)
        .bind(gateway_filter)
        .bind(after)
        .bind(page_limit(q.limit))
        .fetch_all(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "pending_commands"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "pending_commands_commit"))?;
    Ok(Json(json!({
        "commands": rows.into_iter().map(|row| json!({
            "command_id": row.get::<Uuid, _>("command_id"),
            "producer_device_id": row.get::<Uuid, _>("producer_device_id"),
            "gateway_device_id": row.get::<Uuid, _>("gateway_device_id"),
            "cursor": row.get::<i64, _>("cursor").to_string(),
        })).collect::<Vec<_>>()
    })))
}

/// Result of one outbox drain pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrainStats {
    pub published: u64,
    pub deleted: u64,
}

/// Claims due outbox references, publishes their committed cursor as an
/// in-process latency hint, marks them delivered, and deletes delivered
/// references older than the retention. Delivery is at-least-once: a crash
/// before commit republishes, which receivers ignore by cursor. Hints are never
/// authoritative; sockets also rescan committed rows on a timer.
pub async fn drain_outbox(
    db: &PgPool,
    hints: &broadcast::Sender<(Uuid, i64)>,
    batch_size: i64,
    delivered_retention: Duration,
) -> Result<DrainStats, sqlx::Error> {
    let batch_size = batch_size.max(1);
    let mut tx = db.begin().await?;
    let due = sqlx::query("SELECT vault_id,cursor,kind FROM outbox_jobs WHERE delivered_at IS NULL AND available_at<=now() ORDER BY available_at,vault_id,cursor LIMIT $1 FOR UPDATE SKIP LOCKED")
        .bind(batch_size)
        .fetch_all(&mut *tx)
        .await?;
    let mut vaults = Vec::with_capacity(due.len());
    let mut cursors = Vec::with_capacity(due.len());
    let mut kinds = Vec::with_capacity(due.len());
    for row in &due {
        vaults.push(row.get::<Uuid, _>("vault_id"));
        cursors.push(row.get::<i64, _>("cursor"));
        kinds.push(row.get::<String, _>("kind"));
    }
    if !due.is_empty() {
        sqlx::query("UPDATE outbox_jobs o SET delivered_at=now(),attempts=o.attempts+1 FROM UNNEST($1::uuid[],$2::bigint[],$3::text[]) AS d(vault_id,cursor,kind) WHERE o.vault_id=d.vault_id AND o.cursor=d.cursor AND o.kind=d.kind")
            .bind(&vaults)
            .bind(&cursors)
            .bind(&kinds)
            .execute(&mut *tx)
            .await?;
    }
    // Publish while the claim is held; if commit fails the jobs are retried.
    for (vault, cursor) in vaults.iter().zip(&cursors) {
        let _ = hints.send((*vault, *cursor));
    }
    tx.commit().await?;
    let deleted = sqlx::query("DELETE FROM outbox_jobs WHERE ctid IN (SELECT ctid FROM outbox_jobs WHERE delivered_at IS NOT NULL AND delivered_at < now() - ($1::bigint * interval '1 millisecond') LIMIT $2)")
        .bind(interval_millis(delivered_retention))
        .bind(batch_size)
        .execute(db)
        .await?
        .rows_affected();
    Ok(DrainStats {
        published: cursors.len() as u64,
        deleted,
    })
}

/// Progress of the devices that still count toward a vault's replay floor, as
/// the lowest cursor among them (NULL when none counts). A device counts when it
/// is not revoked and either reported within the retention (its stored cursor)
/// or has no report and was paired within the retention (cursor 0). Expects
/// `$1` = retention in milliseconds, `d` = `devices` and `c` = `device_cursors`.
const COUNTED_PROGRESS: &str = "MIN(CASE WHEN d.revoked_at IS NULL AND c.reported_at >= now() - ($1::bigint * interval '1 millisecond') THEN c.receive_cursor WHEN d.revoked_at IS NULL AND c.device_id IS NULL AND d.created_at >= now() - ($1::bigint * interval '1 millisecond') THEN 0 END)";

/// Prunes the eligible contiguous prefix of each vault's transport log and
/// advances `replay_floor_cursor` in the same transaction. Immutable
/// `encrypted_records` are never touched. Returns the number of rows removed.
///
/// A row is eligible when it is older than the retention (the cap) or when every
/// counted device has already received it (progress). The floor moves to the
/// greater of the age cut (rows older than the retention) and the progress cut
/// (the lowest cursor of the counted devices).
/// Candidate vaults are the union of two bounded sets: vaults with rows older
/// than the retention (`event_log_created_at`), and vaults whose progress cut is
/// above their floor, largest distance first (a full pass over `devices` and
/// `device_cursors`). Each set is capped at `PRUNE_BATCH_VAULTS`; a pruned vault
/// leaves its set, so the others rotate in.
pub async fn prune_replay_log(db: &PgPool, retention: Duration) -> Result<u64, sqlx::Error> {
    let retention = interval_millis(retention);
    let aged: Vec<Uuid> = sqlx::query_scalar("SELECT DISTINCT vault_id FROM event_log WHERE created_at < now() - ($1::bigint * interval '1 millisecond') LIMIT $2")
        .bind(retention)
        .bind(PRUNE_BATCH_VAULTS)
        .fetch_all(db)
        .await?;
    let progressed: Vec<Uuid> = sqlx::query_scalar(&format!(
        "SELECT p.vault_id FROM (SELECT d.vault_id, {COUNTED_PROGRESS} AS progress FROM devices d LEFT JOIN device_cursors c ON c.vault_id=d.vault_id AND c.device_id=d.device_id GROUP BY d.vault_id) p JOIN vaults v ON v.vault_id=p.vault_id WHERE p.progress > v.replay_floor_cursor AND EXISTS (SELECT 1 FROM event_log e WHERE e.vault_id=v.vault_id) ORDER BY p.progress - v.replay_floor_cursor DESC LIMIT $2"
    ))
    .bind(retention)
    .bind(PRUNE_BATCH_VAULTS)
    .fetch_all(db)
    .await?;
    let mut vaults = aged;
    for vault in progressed {
        if !vaults.contains(&vault) {
            vaults.push(vault);
        }
    }
    let mut removed = 0;
    for vault in vaults {
        let mut tx = db.begin().await?;
        // Ingest allocates cursors under this lock, so no cursor can appear
        // below the computed cut while it is held.
        let floor: i64 = sqlx::query_scalar(
            "SELECT replay_floor_cursor FROM vaults WHERE vault_id=$1 FOR UPDATE",
        )
        .bind(vault)
        .fetch_one(&mut *tx)
        .await?;
        let bounds = sqlx::query("SELECT (SELECT MIN(cursor) FROM event_log WHERE vault_id=$1 AND created_at >= now() - ($2::bigint * interval '1 millisecond')) AS youngest, (SELECT MAX(cursor) FROM event_log WHERE vault_id=$1) AS newest")
            .bind(vault)
            .bind(retention)
            .fetch_one(&mut *tx)
            .await?;
        let youngest: Option<i64> = bounds.get("youngest");
        let newest: Option<i64> = bounds.get("newest");
        let Some(newest) = newest else {
            tx.commit().await?;
            continue;
        };
        let age_cut = youngest.map_or(newest, |cursor| cursor - 1);
        // NULL (no counted device) means no progress cut. Never pass the newest
        // cursor here: that would prune rows nobody has received.
        let progress_cut: Option<i64> = sqlx::query_scalar(&format!(
            "SELECT {COUNTED_PROGRESS} FROM devices d LEFT JOIN device_cursors c ON c.vault_id=d.vault_id AND c.device_id=d.device_id WHERE d.vault_id=$2"
        ))
        .bind(retention)
        .bind(vault)
        .fetch_one(&mut *tx)
        .await?;
        let cut = age_cut
            .max(progress_cut.map_or(age_cut, |cursor| cursor.min(newest)))
            .min(floor.saturating_add(PRUNE_BATCH_ROWS));
        if cut <= floor {
            tx.commit().await?;
            continue;
        }
        removed += sqlx::query("DELETE FROM event_log WHERE vault_id=$1 AND cursor<=$2")
            .bind(vault)
            .bind(cut)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        sqlx::query("UPDATE vaults SET replay_floor_cursor=$2 WHERE vault_id=$1")
            .bind(vault)
            .bind(cut)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(removed)
}

/// True when every non-revoked device of the vault has declared that it fences
/// snapshot pages by compaction generation. New pairings start undeclared, so
/// physical compaction pauses until they declare (fail closed).
pub(super) const ROSTER_FENCED: &str = "SELECT NOT EXISTS (SELECT 1 FROM devices WHERE vault_id=$1 AND revoked_at IS NULL AND NOT compaction_generation_fence)";

/// Idempotently records that `device` fences snapshot pages by generation.
pub(super) async fn declare_compaction_fence(
    db: &PgPool,
    vault: Uuid,
    device: Uuid,
) -> Result<(), sqlx::Error> {
    let mut tx = crate::scope::begin(db, vault).await?;
    sqlx::query("UPDATE devices SET compaction_generation_fence=TRUE WHERE vault_id=$1 AND device_id=$2 AND revoked_at IS NULL AND NOT compaction_generation_fence")
        .bind(vault)
        .bind(device)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Minimum spacing between physical passes for one vault. Each pass that
/// deletes rows bumps the generation and restarts in-flight snapshot imports,
/// so slow (background-only) clients need long stable windows.
const COMPACTION_INTERVAL: &str = "24 hours";
/// Terminal records (removals/tombstones) stay retained this long after
/// arrival even when superseded, so offline readers still observe them.
const TERMINAL_RETENTION: &str = "90 days";
/// Supersession rows examined per vault pass; larger graphs finish on later passes.
const COMPACTION_EDGE_LIMIT: i64 = 250_000;
/// Records removed per vault pass. Matches the 100k snapshot ceiling, so one
/// pass can retire a whole snapshot's worth of superseded state.
const COMPACTION_BATCH_ROWS: usize = 100_000;
/// Expired unsuperseded terminal roots considered per vault pass.
const COMPACTION_ROOT_LIMIT: i64 = 100_000;
/// A saturated pass (batch, edge or root limit reached) is retried after this
/// much stable time instead of a full `COMPACTION_INTERVAL`, so a busy vault
/// drains its backlog while snapshot imports still get hour-long windows.
const COMPACTION_BACKLOG_RETRY: &str = "1 hour";
/// Supersession edges the expired-root ancestry walks may follow per pass, summed
/// over all roots: four edge pages. One root's walk follows each edge at most
/// once, so only roots whose ancestries overlap can reach this bound.
const MAX_ANCESTRY_EDGES: usize = 4 * COMPACTION_EDGE_LIMIT as usize;

/// Retained records sharing a root's grouping key, read for the expiry guard.
const COMPACTION_SAME_KEY_LIMIT: i64 = 1_024;

/// A replay-pruned terminal record at least `TERMINAL_RETENTION` old.
struct ExpiredRoot {
    cursor: i64,
    /// Other retained records with the same grouping key (bounded).
    same_key: Vec<i64>,
    /// The bound was reached, so survivors may be unknown.
    same_key_truncated: bool,
}

/// One supersession row: `replacement` (by cursor) names a target identity.
struct Edge {
    replacement: i64,
    target: Target,
}

enum Target {
    /// A retained record that this reference may legitimately remove.
    Valid { cursor: i64, deletable: bool },
    /// A retained record this reference may not remove (command, foreign
    /// legacy row or self reference). It neither authorizes nor blocks.
    Protected,
    /// Already physically compacted; recorded in the identity ledger.
    Resolved,
    /// Not uploaded yet. Its replacement must stay until it arrives, or the
    /// late arrival would have nothing superseding it.
    Pending,
}

/// Chooses records to delete from one vault's supersession graph.
///
/// A target is removed only when (a) it is replay-pruned and not a young
/// terminal, its own rows are complete, and no pending reference holds it;
/// (b) a valid supersession path from it reaches a record that no valid
/// supersession targets (a retained representative), at any distance; and
/// (c) every valid target it supersedes is itself removed in the same pass, so
/// no ancestor is stranded without its superseder. Rule (c) keeps the greatest
/// set closed under supersession. Cycles have no representative and remain.
///
/// There is no depth limit. Rules (a) and (b) take one reverse walk from the
/// representatives, rule (c) a worklist, and root safety one forward
/// propagation. Each follows every supersession reference at most a constant
/// number of times; the chosen set is ordered, so rule (c) is O((V + E) log V)
/// with V records and E references.
///
/// `expired_roots` are replay-pruned terminal records at least
/// `TERMINAL_RETENTION` old. One that no valid reference targets expires
/// together with its whole ancestry, but only when every reference in that
/// ancestry is valid-and-deletable or already resolved: a pending, protected
/// or not-yet-deletable ancestor keeps the whole root. As defence in depth, a
/// retained record with the same grouping key outside that ancestry also keeps
/// the root (the key is a hint for this guard, never deletion authority).
/// Non-terminal roots (the latest state) never expire.
///
/// Ancestries are collected with a visited set, and ancestries of different
/// roots can overlap. Together the walks of one pass follow at most
/// `MAX_ANCESTRY_EDGES` supersession edges. When that bound is reached, the
/// root being walked and every later root are kept for this pass, so an
/// incomplete walk never expires anything.
fn select_compaction(
    edges: &[Edge],
    expired_roots: &[ExpiredRoot],
    incomplete_from: Option<i64>,
    batch: usize,
) -> Vec<i64> {
    select_compaction_within(
        edges,
        expired_roots,
        incomplete_from,
        batch,
        MAX_ANCESTRY_EDGES,
    )
}

/// `select_compaction` with an explicit ancestry bound; tests use small bounds.
fn select_compaction_within(
    edges: &[Edge],
    expired_roots: &[ExpiredRoot],
    incomplete_from: Option<i64>,
    batch: usize,
    ancestry_edges: usize,
) -> Vec<i64> {
    let graph = SupersessionGraph::new(edges);
    // A node whose own outgoing rows may be truncated is never removed.
    let complete = |cursor: i64| incomplete_from.is_none_or(|cut| cursor < cut);
    let reaching = graph.reaching_representative();
    let mut chosen: BTreeSet<i64> = graph
        .deletable
        .iter()
        .filter(|&(&cursor, &ok)| {
            ok && complete(cursor) && !graph.held.contains(&cursor) && reaching.contains(&cursor)
        })
        .map(|(&cursor, _)| cursor)
        .collect();
    chosen.extend(graph.expired_ancestries(expired_roots, &complete, ancestry_edges));
    graph.close(&mut chosen);
    if chosen.len() > batch {
        chosen = chosen.into_iter().take(batch).collect();
        graph.close(&mut chosen);
    }
    chosen.into_iter().collect()
}

/// One pass's supersession edges, indexed in both directions.
#[derive(Default)]
struct SupersessionGraph {
    /// Valid target -> replacements that supersede it.
    successors: HashMap<i64, Vec<i64>>,
    /// Replacement -> valid targets it supersedes.
    supersedes: HashMap<i64, Vec<i64>>,
    /// Valid target -> whether it may be removed (replay-pruned, not a young terminal).
    deletable: HashMap<i64, bool>,
    /// Replacements whose own pending reference holds them.
    held: HashSet<i64>,
    /// Replacements with a pending or protected reference; their roots never expire.
    unresolved: HashSet<i64>,
}

impl SupersessionGraph {
    fn new(edges: &[Edge]) -> Self {
        let mut graph = Self::default();
        for edge in edges {
            match edge.target {
                Target::Valid {
                    cursor,
                    deletable: ok,
                } => {
                    graph
                        .successors
                        .entry(cursor)
                        .or_default()
                        .push(edge.replacement);
                    graph
                        .supersedes
                        .entry(edge.replacement)
                        .or_default()
                        .push(cursor);
                    graph.deletable.insert(cursor, ok);
                }
                Target::Pending => {
                    graph.held.insert(edge.replacement);
                    graph.unresolved.insert(edge.replacement);
                }
                Target::Protected => {
                    graph.unresolved.insert(edge.replacement);
                }
                Target::Resolved => {}
            }
        }
        graph
    }

    /// Targets with a valid supersession path to a retained representative: a
    /// reverse walk from the representatives, O(V + E).
    fn reaching_representative(&self) -> HashSet<i64> {
        let mut reaching = HashSet::new();
        let mut stack: Vec<i64> = self
            .supersedes
            .keys()
            .copied()
            .filter(|record| !self.successors.contains_key(record))
            .collect();
        while let Some(record) = stack.pop() {
            for &target in self.supersedes.get(&record).into_iter().flatten() {
                if reaching.insert(target) {
                    stack.push(target);
                }
            }
        }
        reaching
    }

    /// Records with an ancestor that cannot expire (not deletable, incomplete or
    /// unresolved): one forward propagation along successors, O(V + E).
    fn tainted(&self, complete: &impl Fn(i64) -> bool) -> HashSet<i64> {
        let mut tainted = HashSet::new();
        let mut stack: Vec<i64> = self
            .deletable
            .keys()
            .copied()
            .filter(|&target| {
                !(self.deletable[&target] && complete(target) && !self.unresolved.contains(&target))
            })
            .collect();
        while let Some(record) = stack.pop() {
            for &replacement in self.successors.get(&record).into_iter().flatten() {
                if tainted.insert(replacement) {
                    stack.push(replacement);
                }
            }
        }
        tainted
    }

    /// `root` and every record with a supersession path to it. `None` when the
    /// walk would follow more than `budget` supersession edges in total.
    fn ancestry(&self, root: i64, followed: &mut usize, budget: usize) -> Option<HashSet<i64>> {
        let mut seen = HashSet::from([root]);
        let mut stack = vec![root];
        while let Some(record) = stack.pop() {
            for &target in self.supersedes.get(&record).into_iter().flatten() {
                *followed += 1;
                if *followed > budget {
                    return None;
                }
                if seen.insert(target) {
                    stack.push(target);
                }
            }
        }
        Some(seen)
    }

    /// Ancestries of the expired roots that may expire, in the given order.
    /// Stops at the first walk that exceeds the budget: that root and the rest are kept.
    fn expired_ancestries(
        &self,
        roots: &[ExpiredRoot],
        complete: &impl Fn(i64) -> bool,
        budget: usize,
    ) -> Vec<i64> {
        let tainted = self.tainted(complete);
        let mut followed = 0;
        let mut expired = Vec::new();
        for root in roots {
            let cursor = root.cursor;
            if root.same_key_truncated
                || self.successors.contains_key(&cursor)
                || !complete(cursor)
                || self.unresolved.contains(&cursor)
                || tainted.contains(&cursor)
            {
                continue;
            }
            let Some(ancestry) = self.ancestry(cursor, &mut followed, budget) else {
                break;
            };
            if root.same_key.iter().all(|key| ancestry.contains(key)) {
                expired.extend(ancestry);
            }
        }
        expired
    }

    /// Rule (c) as a greatest fixed point: drops each chosen record that
    /// supersedes a valid target outside the set, then whatever that strands.
    /// A dropped record re-examines only the chosen records that supersede it.
    fn close(&self, chosen: &mut BTreeSet<i64>) {
        let mut work: Vec<i64> = chosen
            .iter()
            .copied()
            .filter(|&record| {
                self.supersedes
                    .get(&record)
                    .into_iter()
                    .flatten()
                    .any(|target| !chosen.contains(target))
            })
            .collect();
        while let Some(record) = work.pop() {
            if !chosen.remove(&record) {
                continue;
            }
            for &replacement in self.successors.get(&record).into_iter().flatten() {
                if chosen.contains(&replacement) {
                    work.push(replacement);
                }
            }
        }
    }
}

/// Physically removes superseded, replay-pruned records (see
/// `select_compaction`). Each vault is processed at most once per
/// `COMPACTION_INTERVAL`, least recently processed first, and only while its
/// whole device roster fences snapshot pages. Before deletion, each record's
/// retry identity is copied to `compacted_records`, which ingest consults so a
/// late retry stays a duplicate and a reused sequence stays a conflict.
pub async fn compact_records(db: &PgPool) -> Result<u64, sqlx::Error> {
    let vaults: Vec<Uuid> = sqlx::query_scalar(&format!("SELECT v.vault_id FROM vaults v WHERE v.replay_floor_cursor > 0 AND (v.last_compacted_at IS NULL OR v.last_compacted_at <= now()-interval '{COMPACTION_INTERVAL}') AND NOT EXISTS (SELECT 1 FROM devices d WHERE d.vault_id=v.vault_id AND d.revoked_at IS NULL AND NOT d.compaction_generation_fence) ORDER BY v.last_compacted_at NULLS FIRST, v.vault_id LIMIT $1"))
        .bind(PRUNE_BATCH_VAULTS).fetch_all(db).await?;
    let mut removed = 0;
    for vault in vaults {
        match compact_vault(db, vault).await {
            Ok(count) => removed += count,
            Err(error) => {
                // One vault's failure must not starve the others: its transaction rolled
                // back, so back it off for a full interval and continue with the next.
                tracing::warn!(%vault, error_kind = %error, "vault compaction failed; backing off");
                if let Err(error) =
                    sqlx::query("UPDATE vaults SET last_compacted_at=now() WHERE vault_id=$1")
                        .bind(vault)
                        .execute(db)
                        .await
                {
                    tracing::warn!(%vault, error_kind = %error, "vault compaction backoff failed");
                }
            }
        }
    }
    Ok(removed)
}

/// One vault's pass in a single transaction (see `compact_records`).
async fn compact_vault(db: &PgPool, vault: Uuid) -> Result<u64, sqlx::Error> {
    let mut removed = 0;
    let mut tx = db.begin().await?;
    // Ingest allocates cursors under this lock, so the floor and the graph
    // read below are stable for this pass. The roster is re-checked here.
    let floor: i64 =
        sqlx::query_scalar("SELECT replay_floor_cursor FROM vaults WHERE vault_id=$1 FOR UPDATE")
            .bind(vault)
            .fetch_one(&mut *tx)
            .await?;
    let fenced: bool = sqlx::query_scalar(ROSTER_FENCED)
        .bind(vault)
        .fetch_one(&mut *tx)
        .await?;
    if !fenced {
        tx.commit().await?;
        return Ok(0);
    }
    let rows = sqlx::query(&format!("SELECT s.by_cursor, t.cursor AS target_cursor, (t.purpose='event' AND t.cursor<>s.by_cursor AND (tm.cursor IS NOT NULL OR t.producer_device_id=r.producer_device_id)) AS valid, (t.cursor<=$2 AND (tm.terminal IS DISTINCT FROM TRUE OR tm.created_at <= now()-interval '{TERMINAL_RETENTION}')) AS deletable, c.producer_sequence IS NOT NULL AS resolved FROM record_supersessions s JOIN encrypted_records r ON r.vault_id=s.vault_id AND r.cursor=s.by_cursor LEFT JOIN encrypted_records t ON t.vault_id=s.vault_id AND t.producer_device_id=s.target_producer_device_id AND t.producer_sequence=s.target_producer_sequence LEFT JOIN record_compaction tm ON tm.vault_id=t.vault_id AND tm.cursor=t.cursor LEFT JOIN compacted_records c ON c.vault_id=s.vault_id AND c.producer_device_id=s.target_producer_device_id AND c.producer_sequence=s.target_producer_sequence WHERE s.vault_id=$1 ORDER BY s.by_cursor LIMIT $3"))
        .bind(vault).bind(floor).bind(COMPACTION_EDGE_LIMIT).fetch_all(&mut *tx).await?;
    let incomplete_from = (rows.len() as i64 == COMPACTION_EDGE_LIMIT)
        .then(|| rows.last().map(|row| row.get::<i64, _>("by_cursor")))
        .flatten();
    let edges: Vec<Edge> = rows
        .iter()
        .map(|row| {
            let target = match row.get::<Option<i64>, _>("target_cursor") {
                Some(cursor) if row.get::<Option<bool>, _>("valid") == Some(true) => {
                    Target::Valid {
                        cursor,
                        deletable: row.get::<Option<bool>, _>("deletable") == Some(true),
                    }
                }
                Some(_) => Target::Protected,
                None if row.get::<bool, _>("resolved") => Target::Resolved,
                None => Target::Pending,
            };
            Edge {
                replacement: row.get("by_cursor"),
                target,
            }
        })
        .collect();
    let expired_roots: Vec<ExpiredRoot> = sqlx::query(&format!("SELECT m.cursor, ARRAY(SELECT o.cursor FROM record_compaction o WHERE o.vault_id=m.vault_id AND o.compaction_key=m.compaction_key AND o.cursor<>m.cursor ORDER BY o.cursor LIMIT $4) AS same_key FROM record_compaction m JOIN encrypted_records e ON e.vault_id=m.vault_id AND e.cursor=m.cursor WHERE m.vault_id=$1 AND m.terminal AND e.purpose='event' AND m.cursor<=$2 AND m.created_at <= now()-interval '{TERMINAL_RETENTION}' ORDER BY m.cursor LIMIT $3"))
        .bind(vault)
        .bind(floor)
        .bind(COMPACTION_ROOT_LIMIT)
        .bind(COMPACTION_SAME_KEY_LIMIT)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|row| {
            let same_key: Vec<i64> = row.get("same_key");
            ExpiredRoot {
                cursor: row.get("cursor"),
                same_key_truncated: same_key.len() as i64 == COMPACTION_SAME_KEY_LIMIT,
                same_key,
            }
        })
        .collect();
    let cursors = select_compaction(
        &edges,
        &expired_roots,
        incomplete_from,
        COMPACTION_BATCH_ROWS,
    );
    let saturated = incomplete_from.is_some()
        || cursors.len() == COMPACTION_BATCH_ROWS
        || expired_roots.len() as i64 == COMPACTION_ROOT_LIMIT;
    if !cursors.is_empty() {
        sqlx::query("INSERT INTO compacted_records(vault_id,producer_device_id,producer_sequence,envelope_id,command_id,cipher_digest,original_cursor,origin_tag) SELECT vault_id,producer_device_id,producer_sequence,envelope_id,command_id,cipher_digest,cursor,origin_tag FROM encrypted_records WHERE vault_id=$1 AND cursor=ANY($2)")
            .bind(vault).bind(&cursors).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM record_supersessions WHERE vault_id=$1 AND by_cursor = ANY($2)")
            .bind(vault)
            .bind(&cursors)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM record_compaction WHERE vault_id=$1 AND cursor = ANY($2)")
            .bind(vault)
            .bind(&cursors)
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL peppy.compaction = 'on'")
            .execute(&mut *tx)
            .await?;
        let count =
            sqlx::query("DELETE FROM encrypted_records WHERE vault_id=$1 AND cursor = ANY($2)")
                .bind(vault)
                .bind(&cursors)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        if count != cursors.len() as u64 {
            // The vault lock makes this impossible; never commit a partial ledger.
            return Err(sqlx::Error::Protocol(
                "compaction deleted an unexpected row count".into(),
            ));
        }
        sqlx::query(
            "UPDATE vaults SET compaction_generation=compaction_generation+1 WHERE vault_id=$1",
        )
        .bind(vault)
        .execute(&mut *tx)
        .await?;
        removed = count;
    }
    // Processed (even when nothing was eligible) so other vaults rotate in.
    // A saturated vault becomes due again after the backlog retry window.
    sqlx::query(&format!("UPDATE vaults SET last_compacted_at=CASE WHEN $2 THEN now()-interval '{COMPACTION_INTERVAL}'+interval '{COMPACTION_BACKLOG_RETRY}' ELSE now() END WHERE vault_id=$1"))
        .bind(vault)
        .bind(saturated)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(removed)
}

/// Starts the bounded in-process maintenance loop. It stops when the router's
/// hint sender is dropped or the pool closes; there is no separate worker.
pub(super) fn spawn_maintenance(
    db: PgPool,
    hints: &broadcast::Sender<(Uuid, i64)>,
    storage: Option<Storage>,
    options: TransportOptions,
) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        tracing::warn!("no tokio runtime; outbox drain and replay pruning are disabled");
        return;
    };
    let weak = hints.downgrade();
    runtime.spawn(async move {
        let mut drain = tokio::time::interval(options.outbox_drain_interval);
        drain.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut prune = tokio::time::interval(options.replay_prune_interval);
        prune.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut cleanup = tokio::time::interval(options.storage_cleanup_interval);
        cleanup.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = drain.tick() => {
                    let Some(hints) = weak.upgrade() else { return };
                    if db.is_closed() { return; }
                    if let Err(error) = drain_outbox(&db, &hints, options.outbox_batch_size, options.outbox_delivered_retention).await {
                        tracing::warn!(error_kind = %error, "outbox drain failed");
                    }
                }
                _ = prune.tick() => {
                    if weak.strong_count() == 0 || db.is_closed() { return; }
                    match prune_replay_log(&db, options.replay_retention).await {
                        Ok(0) => {}
                        Ok(rows) => tracing::info!(rows, "pruned replay log rows"),
                        Err(error) => tracing::warn!(error_kind = %error, "replay pruning failed"),
                    }
                    match compact_records(&db).await {
                        Ok(0) => {}
                        Ok(rows) => tracing::info!(rows, "physically compacted superseded records"),
                        Err(error) => tracing::warn!(error_kind = %error, "record compaction failed"),
                    }
                }
                _ = cleanup.tick() => {
                    if weak.strong_count() == 0 || db.is_closed() { return; }
                    if let Err(error) = super::attachments::storage_maintenance(&db, storage.as_ref(), options.storage_cleanup_batch).await {
                        tracing::warn!(error_kind = %error, "storage cleanup failed");
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn native_golden(name: &str, actual: &str) {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/contracts/fixtures/server")
            .join(name);
        if std::env::var_os("UPDATE_GOLDENS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, actual).unwrap();
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!("{}: {error}; regenerate with UPDATE_GOLDENS=1 cargo test -p peppy-server native_sync_response_goldens", path.display())
        });
        let actual: Value = serde_json::from_str(actual).unwrap();
        let expected: Value = serde_json::from_str(&expected).unwrap();
        assert_eq!(
            actual,
            expected,
            "{} differs; regenerate with UPDATE_GOLDENS=1 cargo test -p peppy-server native_sync_response_goldens",
            path.display()
        );
    }

    #[tokio::test]
    async fn native_sync_response_goldens() {
        let record = || CursorRecord {
            cursor: 1,
            envelope: RawValue::from_string(
                json!({
                    "protocol_version": 1,
                    "envelope_id": "22222222-2222-4222-8222-222222222222",
                    "command_id": null,
                    "vault_id": "11111111-1111-4111-8111-111111111111",
                    "producer_device_id": "33333333-3333-4333-8333-333333333333",
                    "producer_sequence": "1",
                    "key_epoch": 1,
                    "crypto_suite": 1,
                    "profile_fingerprint": "00".repeat(32),
                    "purpose": "event",
                    "route": null,
                    "ciphertext": "AA==",
                })
                .to_string(),
            )
            .unwrap(),
        };
        let samples = [
            (
                "events.200.json",
                serde_json::to_value(EventsPage {
                    high_water_cursor: "1".into(),
                    replay_floor_cursor: "0".into(),
                    next_after: None,
                    events: vec![record()],
                })
                .unwrap(),
            ),
            (
                "snapshot-records.200.json",
                serde_json::to_value(SnapshotPage {
                    high_water_cursor: "1".into(),
                    compaction_generation: "1".into(),
                    next_after: None,
                    records: vec![record()],
                })
                .unwrap(),
            ),
            (
                "events-more.200.json",
                serde_json::to_value(EventsPage {
                    high_water_cursor: "2".into(),
                    replay_floor_cursor: "0".into(),
                    next_after: Some("1".into()),
                    events: vec![record()],
                })
                .unwrap(),
            ),
            (
                "snapshot-records-more.200.json",
                serde_json::to_value(SnapshotPage {
                    high_water_cursor: "2".into(),
                    compaction_generation: "1".into(),
                    next_after: Some("1".into()),
                    records: vec![record()],
                })
                .unwrap(),
            ),
            (
                "snapshot-manifest.200.json",
                serde_json::to_value(SnapshotManifestPage {
                    high_water_cursor: "1".into(),
                    compaction_generation: "1".into(),
                    next_after: None,
                    entries: vec![SnapshotManifestEntry {
                        cursor: "1".into(),
                        digest: "00".repeat(32),
                        flags: 0,
                    }],
                })
                .unwrap(),
            ),
        ];
        for (name, value) in samples {
            native_golden(
                name,
                &(serde_json::to_string_pretty(&value).unwrap() + "\n"),
            );
        }
        let response = Resync {
            reason: ResyncReason::CursorExpired,
            high_water: 10,
            replay_floor: 5,
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        native_golden("events.409.json", std::str::from_utf8(&body).unwrap());
        let response = ApiError(StatusCode::FORBIDDEN, "owner_required").into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        native_golden(
            "owner-required.403.json",
            std::str::from_utf8(&body).unwrap(),
        );
    }

    fn roots(cursors: &[i64]) -> Vec<ExpiredRoot> {
        cursors
            .iter()
            .map(|&cursor| ExpiredRoot {
                cursor,
                same_key: Vec::new(),
                same_key_truncated: false,
            })
            .collect()
    }

    fn valid(replacement: i64, cursor: i64) -> Edge {
        Edge {
            replacement,
            target: Target::Valid {
                cursor,
                deletable: true,
            },
        }
    }

    #[test]
    fn compaction_selection_collapses_chains_and_keeps_cycles_and_ancestors() {
        // 1 <- 2 <- 3: both ancestors go together.
        assert_eq!(
            select_compaction(&[valid(2, 1), valid(3, 2)], &[], None, 100),
            vec![1, 2]
        );
        // A pure cycle has no representative.
        assert!(select_compaction(&[valid(2, 1), valid(1, 2)], &[], None, 100).is_empty());
        // A cycle with an exit collapses into the exit.
        assert_eq!(
            select_compaction(&[valid(2, 1), valid(1, 2), valid(3, 1)], &[], None, 100),
            vec![1, 2]
        );
        // 2 cannot go while its own target 1 is not deletable (young terminal):
        // removing 2 would strand 1 without its superseder.
        let young = Edge {
            replacement: 2,
            target: Target::Valid {
                cursor: 1,
                deletable: false,
            },
        };
        assert!(select_compaction(&[young, valid(3, 2)], &[], None, 100).is_empty());
        // Batch truncation re-applies the ancestor rule: {1} alone is fine, {2} alone is not.
        assert_eq!(
            select_compaction(&[valid(2, 1), valid(3, 2)], &[], None, 1),
            vec![1]
        );
        // Pending targets hold their replacement; protected ones neither authorize nor block.
        let pending = Edge {
            replacement: 2,
            target: Target::Pending,
        };
        assert_eq!(
            select_compaction(&[valid(2, 1), pending, valid(3, 2)], &[], None, 100),
            vec![1]
        );
        let protected = Edge {
            replacement: 2,
            target: Target::Protected,
        };
        assert_eq!(
            select_compaction(&[protected, valid(3, 2)], &[], None, 100),
            vec![2]
        );
        // Rows at/after a truncated edge page may be incomplete and are never removed.
        assert_eq!(
            select_compaction(&[valid(2, 1), valid(3, 2)], &[], Some(2), 100),
            vec![1]
        );

        // Expired terminal roots expire with their whole ancestry.
        assert_eq!(
            select_compaction(&[valid(2, 1)], &roots(&[2]), None, 100),
            vec![1, 2]
        );
        assert_eq!(select_compaction(&[], &roots(&[5]), None, 100), vec![5]);
        let resolved = Edge {
            replacement: 2,
            target: Target::Resolved,
        };
        assert_eq!(
            select_compaction(&[resolved], &roots(&[2]), None, 100),
            vec![2]
        );
        // ... but not while any reference in its ancestry is unsafe or not yet
        // deletable: the root stays (its superseded post still goes normally).
        let pending = Edge {
            replacement: 2,
            target: Target::Pending,
        };
        assert_eq!(
            select_compaction(&[valid(2, 1), pending], &roots(&[2]), None, 100),
            vec![1]
        );
        let protected = Edge {
            replacement: 2,
            target: Target::Protected,
        };
        assert_eq!(
            select_compaction(&[valid(2, 1), protected], &roots(&[2]), None, 100),
            vec![1]
        );
        let young = Edge {
            replacement: 2,
            target: Target::Valid {
                cursor: 1,
                deletable: false,
            },
        };
        assert!(select_compaction(&[young], &roots(&[2]), None, 100).is_empty());
        let deep_pending = Edge {
            replacement: 1,
            target: Target::Pending,
        };
        assert!(
            select_compaction(&[valid(2, 1), deep_pending], &roots(&[2]), None, 100).is_empty()
        );
        // A superseded terminal is not a root (handled by the representative rule),
        // and a root whose rows may be truncated never expires.
        assert_eq!(
            select_compaction(&[valid(3, 2)], &roots(&[2]), None, 100),
            vec![2]
        );
        assert!(select_compaction(&[], &roots(&[5]), Some(5), 100).is_empty());
        // Same-key survivors outside the ancestry keep the root; members don't.
        let guarded = |same_key: Vec<i64>, truncated: bool| {
            vec![ExpiredRoot {
                cursor: 2,
                same_key,
                same_key_truncated: truncated,
            }]
        };
        assert_eq!(
            select_compaction(&[valid(2, 1)], &guarded(vec![1, 9], false), None, 100),
            vec![1]
        );
        assert_eq!(
            select_compaction(&[valid(2, 1)], &guarded(vec![1], false), None, 100),
            vec![1, 2]
        );
        assert!(select_compaction(&[], &guarded(vec![], true), None, 100).is_empty());
    }

    /// `1 <- 2 <- ... <- len`: each record supersedes its predecessor.
    fn chain(len: i64) -> Vec<Edge> {
        (1..len).map(|cursor| valid(cursor + 1, cursor)).collect()
    }

    #[test]
    fn chains_of_any_length_keep_only_the_newest_record() {
        for len in [2, 64, 65, 66, 67, 200, 5_201] {
            assert_eq!(
                select_compaction(&chain(len), &[], None, COMPACTION_BATCH_ROWS),
                (1..len).collect::<Vec<_>>(),
                "chain of {len} records"
            );
        }
    }

    #[test]
    fn long_chain_keeps_the_record_at_a_young_held_or_incomplete_position_and_newer_ones() {
        const LEN: i64 = 200;
        const AT: i64 = 100;
        let older: Vec<i64> = (1..AT).collect();
        // Young terminal AT: its superseder names a target that may not be removed yet.
        let young: Vec<Edge> = (1..LEN)
            .map(|cursor| Edge {
                replacement: cursor + 1,
                target: Target::Valid {
                    cursor,
                    deletable: cursor != AT,
                },
            })
            .collect();
        assert_eq!(
            select_compaction(&young, &[], None, COMPACTION_BATCH_ROWS),
            older,
            "young terminal at {AT}"
        );
        // AT itself holds a pending reference.
        let mut held = chain(LEN);
        held.push(Edge {
            replacement: AT,
            target: Target::Pending,
        });
        assert_eq!(
            select_compaction(&held, &[], None, COMPACTION_BATCH_ROWS),
            older,
            "held replacement at {AT}"
        );
        // Rows from AT on may be incomplete.
        assert_eq!(
            select_compaction(&chain(LEN), &[], Some(AT), COMPACTION_BATCH_ROWS),
            older,
            "incomplete rows from {AT}"
        );
    }

    #[test]
    fn long_cycles_remain_without_an_exit_and_collapse_into_an_exit() {
        const LEN: i64 = 300;
        let mut edges = chain(LEN);
        edges.push(valid(1, LEN));
        assert!(select_compaction(&edges, &[], None, COMPACTION_BATCH_ROWS).is_empty());
        edges.push(valid(LEN + 1, 1));
        assert_eq!(
            select_compaction(&edges, &[], None, COMPACTION_BATCH_ROWS),
            (1..=LEN).collect::<Vec<_>>()
        );
    }

    #[test]
    fn batch_truncation_on_a_long_chain_removes_the_oldest_records() {
        assert_eq!(
            select_compaction(&chain(500), &[], None, 10),
            (1..=10).collect::<Vec<_>>()
        );
        assert_eq!(select_compaction(&chain(500), &[], None, 1), vec![1]);
    }

    #[test]
    fn expired_root_under_a_deep_chain_expires_with_its_whole_ancestry() {
        for len in [65, 200] {
            assert_eq!(
                select_compaction(&chain(len), &roots(&[len]), None, COMPACTION_BATCH_ROWS),
                (1..=len).collect::<Vec<_>>(),
                "root under a chain of {len}"
            );
        }
    }

    #[test]
    fn deep_unsafe_ancestor_keeps_the_expired_root_and_the_ordinary_rule_still_applies() {
        const LEN: i64 = 200;
        // Ancestor 10 is 190 supersessions below the root 200.
        const DEEP: i64 = 10;
        let mut not_deletable = chain(LEN);
        not_deletable.retain(|edge| edge.replacement != DEEP + 1);
        not_deletable.push(Edge {
            replacement: DEEP + 1,
            target: Target::Valid {
                cursor: DEEP,
                deletable: false,
            },
        });
        assert_eq!(
            select_compaction(&not_deletable, &roots(&[LEN]), None, COMPACTION_BATCH_ROWS),
            (1..DEEP).collect::<Vec<_>>(),
            "not deletable ancestor"
        );
        let mut pending = chain(LEN);
        pending.push(Edge {
            replacement: DEEP,
            target: Target::Pending,
        });
        assert_eq!(
            select_compaction(&pending, &roots(&[LEN]), None, COMPACTION_BATCH_ROWS),
            (1..DEEP).collect::<Vec<_>>(),
            "pending ancestor"
        );
        let mut protected = chain(LEN);
        protected.push(Edge {
            replacement: DEEP,
            target: Target::Protected,
        });
        assert_eq!(
            select_compaction(&protected, &roots(&[LEN]), None, COMPACTION_BATCH_ROWS),
            (1..LEN).collect::<Vec<_>>(),
            "protected ancestor: the ordinary rule still removes 1..=199"
        );
    }

    #[test]
    fn ancestry_budget_keeps_the_walked_root_and_every_later_root() {
        // 2001 <- ... <- 2020: the root 2020 needs 19 supersession edges.
        let edges: Vec<Edge> = (2001..2020)
            .map(|cursor| valid(cursor + 1, cursor))
            .collect();
        let expired = roots(&[1000, 2020, 3000]);
        // Budget 10: the walk of 2020 stops, so 2020 and the later root 3000 are kept.
        assert_eq!(
            select_compaction_within(&edges, &expired, None, COMPACTION_BATCH_ROWS, 10),
            [vec![1000], (2001..2020).collect()].concat()
        );
        // Budget 19: exactly enough for the walk of 2020, so every root expires.
        assert_eq!(
            select_compaction_within(&edges, &expired, None, COMPACTION_BATCH_ROWS, 19),
            [vec![1000], (2001..=2020).collect(), vec![3000]].concat()
        );
    }

    #[test]
    fn quarter_million_record_chains_complete_in_one_pass() {
        const LEN: i64 = 250_000;
        assert_eq!(
            select_compaction(&chain(LEN), &[], None, usize::MAX),
            (1..LEN).collect::<Vec<_>>()
        );
        // The oldest target is not deletable: every superseder is stranded in turn.
        let mut young = chain(LEN);
        young[0] = Edge {
            replacement: 2,
            target: Target::Valid {
                cursor: 1,
                deletable: false,
            },
        };
        assert!(select_compaction(&young, &[], None, usize::MAX).is_empty());
    }

    #[test]
    fn one_target_with_thousands_of_expired_replacements_completes() {
        const REPLACEMENTS: i64 = 5_000;
        let edges: Vec<Edge> = (2..=REPLACEMENTS + 1)
            .map(|replacement| valid(replacement, 1))
            .collect();
        let expired: Vec<i64> = (2..=REPLACEMENTS + 1).collect();
        assert_eq!(
            select_compaction(&edges, &roots(&expired), None, usize::MAX),
            (1..=REPLACEMENTS + 1).collect::<Vec<_>>()
        );
    }

    /// Records reachable from `start` through `next`, excluding `start` unless it is reached again.
    fn reachable(start: i64, next: impl Fn(i64) -> Vec<i64>) -> BTreeSet<i64> {
        let mut seen = BTreeSet::new();
        let mut frontier = vec![start];
        while let Some(node) = frontier.pop() {
            for reached in next(node) {
                if seen.insert(reached) {
                    frontier.push(reached);
                }
            }
        }
        seen
    }

    /// Brute-force reference written from the documented rules only: naive
    /// reachability per record, a naive fixed point, a naive ancestry per root,
    /// no depth cap. It shares no code with `select_compaction`.
    fn reference_selection(
        edges: &[Edge],
        roots: &[ExpiredRoot],
        incomplete_from: Option<i64>,
        batch: usize,
    ) -> Vec<i64> {
        // (replacement, target) of every valid reference.
        let valid_refs: Vec<(i64, i64)> = edges
            .iter()
            .filter_map(|edge| match edge.target {
                Target::Valid { cursor, .. } => Some((edge.replacement, cursor)),
                _ => None,
            })
            .collect();
        let targets: BTreeSet<i64> = valid_refs.iter().map(|&(_, target)| target).collect();
        let deletable = |x: i64| {
            edges.iter().any(|edge| {
                matches!(edge.target, Target::Valid { cursor, deletable: true } if cursor == x)
            })
        };
        let held = |x: i64| {
            edges
                .iter()
                .any(|edge| edge.replacement == x && matches!(edge.target, Target::Pending))
        };
        let unresolved = |x: i64| {
            edges.iter().any(|edge| {
                edge.replacement == x && matches!(edge.target, Target::Pending | Target::Protected)
            })
        };
        let complete = |x: i64| incomplete_from.is_none_or(|cut| x < cut);
        // Records that supersede `x`, directly or through a chain of valid references.
        let superseders = |x: i64| {
            reachable(x, |node| {
                valid_refs
                    .iter()
                    .filter(|&&(_, target)| target == node)
                    .map(|&(by, _)| by)
                    .collect()
            })
        };
        // Records that `x` supersedes, directly or through a chain of valid references.
        let superseded = |x: i64| {
            reachable(x, |node| {
                valid_refs
                    .iter()
                    .filter(|&&(by, _)| by == node)
                    .map(|&(_, target)| target)
                    .collect()
            })
        };

        // Rule (a) and (b): deletable, complete, not held, and a representative is reachable.
        let mut chosen: BTreeSet<i64> = targets
            .iter()
            .copied()
            .filter(|&target| {
                deletable(target)
                    && complete(target)
                    && !held(target)
                    && superseders(target)
                        .iter()
                        .any(|record| !targets.contains(record))
            })
            .collect();
        // Expired roots: whole ancestry, when every part of it may go.
        for root in roots {
            let cursor = root.cursor;
            if root.same_key_truncated
                || targets.contains(&cursor)
                || !complete(cursor)
                || unresolved(cursor)
            {
                continue;
            }
            let ancestry = superseded(cursor);
            let safe = ancestry
                .iter()
                .all(|&record| deletable(record) && complete(record) && !unresolved(record));
            let same_key_inside = root.same_key.iter().all(|key| ancestry.contains(key));
            if safe && same_key_inside {
                chosen.insert(cursor);
                chosen.extend(ancestry);
            }
        }
        // Rule (c): a record stays only while every valid target it supersedes stays.
        let close = |mut kept: BTreeSet<i64>| loop {
            let next: BTreeSet<i64> = kept
                .iter()
                .copied()
                .filter(|&record| {
                    valid_refs
                        .iter()
                        .all(|&(by, target)| by != record || kept.contains(&target))
                })
                .collect();
            if next.len() == kept.len() {
                return next;
            }
            kept = next;
        };
        let mut chosen = close(chosen);
        if chosen.len() > batch {
            chosen = close(chosen.into_iter().take(batch).collect());
        }
        chosen.into_iter().collect()
    }

    /// Small seeded xorshift generator: the tests need no randomness crate.
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
        }

        fn next_u64(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        /// A value in `0..bound`; `bound` must be positive.
        fn below(&mut self, bound: u64) -> u64 {
            self.next_u64() % bound
        }

        fn chance(&mut self, percent: u64) -> bool {
            self.below(100) < percent
        }

        fn record(&mut self, nodes: i64) -> i64 {
            1 + self.below(nodes as u64) as i64
        }
    }

    /// A random graph for `seed`: valid, not-deletable, pending, protected and
    /// resolved references, cycles, truncated rows, batch limits and expired
    /// roots with same-key lists. Every eighth graph holds a chain deeper than
    /// the old walk cap; the reference has no cap.
    fn random_graph(seed: u64) -> (Vec<Edge>, Vec<ExpiredRoot>, Option<i64>, usize) {
        let mut rng = Rng::new(seed);
        let deep = seed.is_multiple_of(8);
        let nodes = (if deep {
            66 + rng.below(25)
        } else {
            1 + rng.below(12)
        }) as i64;
        let deletable: Vec<bool> = (0..=nodes).map(|_| rng.chance(80)).collect();
        let incomplete_from = rng.chance(25).then(|| rng.record(nodes));
        let mut edges = Vec::new();
        if deep {
            // Mostly one long supersession chain, deeper than the old walk cap.
            for cursor in 1..nodes {
                if rng.chance(98) {
                    edges.push(Edge {
                        replacement: cursor + 1,
                        target: Target::Valid {
                            cursor,
                            deletable: deletable[cursor as usize],
                        },
                    });
                }
            }
        }
        for _ in 0..rng.below(3 * nodes as u64 + 1) {
            let replacement = rng.record(nodes);
            let target = match rng.below(100) {
                0..=69 => {
                    let cursor = if replacement > 1 && rng.chance(60) {
                        replacement - 1
                    } else {
                        rng.record(nodes)
                    };
                    if cursor == replacement {
                        Target::Protected
                    } else {
                        Target::Valid {
                            cursor,
                            deletable: deletable[cursor as usize],
                        }
                    }
                }
                70..=79 => Target::Pending,
                80..=89 => Target::Protected,
                _ => Target::Resolved,
            };
            edges.push(Edge {
                replacement,
                target,
            });
        }
        let mut expired = Vec::new();
        for cursor in 1..=nodes {
            if !rng.chance(35) {
                continue;
            }
            let mut same_key: Vec<i64> = (0..rng.below(3))
                .map(|_| rng.record(nodes))
                .filter(|&key| key != cursor)
                .collect();
            same_key.sort_unstable();
            same_key.dedup();
            expired.push(ExpiredRoot {
                cursor,
                same_key,
                same_key_truncated: rng.chance(5),
            });
        }
        let batch = if rng.chance(40) {
            1 + rng.below(nodes as u64 + 1) as usize
        } else {
            COMPACTION_BATCH_ROWS
        };
        (edges, expired, incomplete_from, batch)
    }

    #[test]
    fn selection_matches_a_brute_force_reference_on_random_graphs() {
        for seed in 1..=6_000 {
            let (edges, expired, incomplete_from, batch) = random_graph(seed);
            assert_eq!(
                select_compaction(&edges, &expired, incomplete_from, batch),
                reference_selection(&edges, &expired, incomplete_from, batch),
                "selection differs from the reference at seed {seed}"
            );
        }
    }

    /// Edges, expired roots, `incomplete_from`, batch, and the expected selection.
    type DocumentedCase = (Vec<Edge>, Vec<ExpiredRoot>, Option<i64>, usize, Vec<i64>);

    #[test]
    fn reference_agrees_with_every_documented_example() {
        let guarded = |same_key: Vec<i64>, same_key_truncated: bool| {
            vec![ExpiredRoot {
                cursor: 2,
                same_key,
                same_key_truncated,
            }]
        };
        let young_1 = || Edge {
            replacement: 2,
            target: Target::Valid {
                cursor: 1,
                deletable: false,
            },
        };
        let pending_at = |replacement| Edge {
            replacement,
            target: Target::Pending,
        };
        let protected_at = |replacement| Edge {
            replacement,
            target: Target::Protected,
        };
        let resolved_2 = || Edge {
            replacement: 2,
            target: Target::Resolved,
        };
        // (edges, roots, incomplete_from, batch, expected)
        let cases: Vec<DocumentedCase> = vec![
            (
                vec![valid(2, 1), valid(3, 2)],
                vec![],
                None,
                100,
                vec![1, 2],
            ),
            (vec![valid(2, 1), valid(1, 2)], vec![], None, 100, vec![]),
            (
                vec![valid(2, 1), valid(1, 2), valid(3, 1)],
                vec![],
                None,
                100,
                vec![1, 2],
            ),
            (vec![young_1(), valid(3, 2)], vec![], None, 100, vec![]),
            (vec![valid(2, 1), valid(3, 2)], vec![], None, 1, vec![1]),
            (
                vec![valid(2, 1), pending_at(2), valid(3, 2)],
                vec![],
                None,
                100,
                vec![1],
            ),
            (
                vec![protected_at(2), valid(3, 2)],
                vec![],
                None,
                100,
                vec![2],
            ),
            (
                vec![valid(2, 1), valid(3, 2)],
                vec![],
                Some(2),
                100,
                vec![1],
            ),
            (vec![valid(2, 1)], roots(&[2]), None, 100, vec![1, 2]),
            (vec![], roots(&[5]), None, 100, vec![5]),
            (vec![resolved_2()], roots(&[2]), None, 100, vec![2]),
            (
                vec![valid(2, 1), pending_at(2)],
                roots(&[2]),
                None,
                100,
                vec![1],
            ),
            (
                vec![valid(2, 1), protected_at(2)],
                roots(&[2]),
                None,
                100,
                vec![1],
            ),
            (vec![young_1()], roots(&[2]), None, 100, vec![]),
            (
                vec![valid(2, 1), pending_at(1)],
                roots(&[2]),
                None,
                100,
                vec![],
            ),
            (vec![valid(3, 2)], roots(&[2]), None, 100, vec![2]),
            (vec![], roots(&[5]), Some(5), 100, vec![]),
            (
                vec![valid(2, 1)],
                guarded(vec![1, 9], false),
                None,
                100,
                vec![1],
            ),
            (
                vec![valid(2, 1)],
                guarded(vec![1], false),
                None,
                100,
                vec![1, 2],
            ),
            (vec![], guarded(vec![], true), None, 100, vec![]),
        ];
        for (index, (edges, expired, incomplete_from, batch, expected)) in
            cases.into_iter().enumerate()
        {
            assert_eq!(
                reference_selection(&edges, &expired, incomplete_from, batch),
                expected,
                "reference, documented example {index}"
            );
            assert_eq!(
                select_compaction(&edges, &expired, incomplete_from, batch),
                expected,
                "production, documented example {index}"
            );
        }
    }

    #[test]
    fn resync_classifies_expired_and_ahead_cursors() {
        assert_eq!(Resync::classify(5, 10, 5), None);
        assert_eq!(Resync::classify(10, 10, 0), None);
        assert_eq!(
            Resync::classify(4, 10, 5).map(|r| r.reason),
            Some(ResyncReason::CursorExpired)
        );
        assert_eq!(
            Resync::classify(11, 10, 0).map(|r| r.reason),
            Some(ResyncReason::CursorAhead)
        );
    }

    #[test]
    fn cursor_and_page_parsing_are_bounded() {
        assert_eq!(parse_cursor("0").unwrap(), 0);
        assert!(parse_cursor("-1").is_err());
        assert!(parse_cursor("1.5").is_err());
        assert_eq!(page_limit(Some(0)), 1);
        assert_eq!(page_limit(Some(5_000)), i64::from(MAX_PAGE));
        assert_eq!(page_limit(None), i64::from(DEFAULT_PAGE));
        assert_eq!(interval_millis(Duration::MAX), MAX_INTERVAL_MILLIS);
    }
}
