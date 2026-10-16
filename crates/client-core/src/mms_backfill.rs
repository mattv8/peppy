//! Durable, vault-scoped MMS provider-history lifecycle.
//!
//! The Android reader owns provider I/O; this module owns the frozen baseline, progress, capacity
//! pauses, and bounded fair retries. It deliberately never changes a source generation: reused
//! provider IDs need scanner evidence, not a guessed destructive deduplication.

use super::*;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

pub const MAX_MMS_BACKFILL_RECHECK: usize = 32;
const PAUSE_RECOVERY_CAPACITY: &str = "recovery_capacity";
const PAUSE_LOCAL_CAPACITY: &str = "local_capacity";
const PAUSE_REMOTE_STORAGE: &str = "remote_storage";
const RECOVERY_RECORD_RESERVE: u64 = 10_000;
const RECOVERY_BYTE_RESERVE: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MmsBackfillPhase {
    Live,
    History,
    Recheck,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MmsBackfillPause {
    RecoveryCapacity,
    LocalCapacity,
    RemoteStorage,
}

impl MmsBackfillPause {
    fn code(self) -> &'static str {
        match self {
            Self::RecoveryCapacity => PAUSE_RECOVERY_CAPACITY,
            Self::LocalCapacity => PAUSE_LOCAL_CAPACITY,
            Self::RemoteStorage => PAUSE_REMOTE_STORAGE,
        }
    }

    fn parse(value: Option<String>) -> Result<Option<Self>, Error> {
        value
            .map(|value| match value.as_str() {
                PAUSE_RECOVERY_CAPACITY => Ok(Self::RecoveryCapacity),
                PAUSE_LOCAL_CAPACITY => Ok(Self::LocalCapacity),
                PAUSE_REMOTE_STORAGE => Ok(Self::RemoteStorage),
                _ => Err(Error::Database),
            })
            .transpose()
    }
}

/// Install-scoped preference data imported once before the first Rust-driven plan.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MmsLegacyBackfillState {
    /// `None` is an absent preference; `Some("0")` is an explicit provider baseline.
    pub history_baseline: Option<String>,
    pub import_history: Option<bool>,
    pub recheck_cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MmsBackfillScanPlan {
    pub phase: MmsBackfillPhase,
    pub source_generation: String,
    pub subscription_id: String,
    /// Last acknowledged provider ID, if a page is resumable.
    pub cursor: Option<String>,
    /// Frozen maximum ID for history/recheck classification; absent only before initialization.
    pub history_upper_bound: Option<String>,
    pub history_complete: bool,
    pub pause: Option<MmsBackfillPause>,
    /// Hosts must never exceed this request count for a recheck pass.
    pub budget: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MmsBackfillRecheckRequest {
    pub provider_message_id: String,
    pub direction: Direction,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MmsBackfillRecheck {
    pub requests: Vec<MmsBackfillRecheckRequest>,
    /// Opaque durable fairness cursor. `None` means the next call begins a fresh round.
    pub cursor: Option<String>,
    pub budget: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MmsBackfillStatus {
    pub baseline_initialized: bool,
    pub history_upper_bound: Option<String>,
    pub history_complete: bool,
    pub pause: Option<MmsBackfillPause>,
    pub pending_count: u64,
    pub recheck_cursor: Option<String>,
}

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mms_backfill_scopes(
           source_generation TEXT NOT NULL,
           subscription_id TEXT NOT NULL,
           history_upper_bound TEXT,
           history_complete INTEGER NOT NULL DEFAULT 0,
           pause_reason TEXT,
           pending_recheck_cursor TEXT,
           migration_done INTEGER NOT NULL DEFAULT 0,
           PRIMARY KEY(source_generation, subscription_id)
         );",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mms_backfill_recheck_cursors(
           source_generation TEXT NOT NULL,
           subscription_id TEXT NOT NULL,
           pending_cursor INTEGER NOT NULL DEFAULT 0,
           completed_cursor INTEGER NOT NULL DEFAULT 0,
           PRIMARY KEY(source_generation, subscription_id)
         );",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mms_provider_identity_conflicts(
           source_generation TEXT NOT NULL,
           subscription_id TEXT NOT NULL,
           provider_message_id TEXT NOT NULL,
           direction TEXT NOT NULL,
           reason TEXT NOT NULL,
           PRIMARY KEY(source_generation, subscription_id, provider_message_id, direction)
         );",
    )?;
    Ok(())
}

impl Client {
    /// Imports preference state exactly once. Preferences remain untouched for rollback safety.
    pub fn migrate_mms_backfill_state(
        &self,
        generation: &str,
        scope: &str,
        legacy: MmsLegacyBackfillState,
    ) -> Result<(), Error> {
        validate_scope(generation, scope)?;
        validate_optional_id(legacy.history_baseline.as_deref(), "MMS baseline")?;
        if legacy
            .recheck_cursor
            .as_deref()
            .is_some_and(|cursor| cursor.is_empty() || cursor.len() > MAX_PROVIDER_ID_BYTES)
        {
            return Err(Error::InvalidRequest("MMS recheck cursor"));
        }
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM mms_backfill_scopes WHERE source_generation=? AND subscription_id=?)",
            params![generation, scope],
            |row| row.get(0),
        )?;
        if !exists {
            let recheck_cursor =
                resolve_legacy_recheck_cursor(&tx, generation, legacy.recheck_cursor.as_deref())?;
            // Android's old preference defaults false both before an import is requested and after
            // it finishes. It is not completion evidence; only an exhausted Rust-owned scan may
            // mark history complete.
            let complete = false;
            tx.execute(
                "INSERT INTO mms_backfill_scopes(source_generation,subscription_id,history_upper_bound,history_complete,pending_recheck_cursor,migration_done) VALUES(?,?,?,?,?,1)",
                params![generation, scope, legacy.history_baseline, complete, recheck_cursor],
            )?;
            if let Some(bound) = legacy.history_baseline {
                ensure_checkpoint(&tx, generation, scope, false, &bound)?;
                ensure_checkpoint(&tx, generation, scope, true, "")?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Atomically freezes an absent baseline and returns every bounded scan phase.
    pub fn plan_mms_backfill_scans(
        &self,
        generation: &str,
        scope: &str,
        observed_max_id: &str,
    ) -> Result<Vec<MmsBackfillScanPlan>, Error> {
        validate_scope(generation, scope)?;
        validate_id(observed_max_id, "MMS observed maximum")?;
        let pending_bytes = self.mms_pending_media_bytes()?;
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_scope(&tx, generation, scope, observed_max_id)?;
        let (baseline, complete, pause, recheck_cursor) = scope_row(&tx, generation, scope)?;
        let baseline = match baseline {
            Some(baseline) => baseline,
            None => {
                tx.execute(
                    "UPDATE mms_backfill_scopes SET history_upper_bound=?,history_complete=0 WHERE source_generation=? AND subscription_id=?",
                    params![observed_max_id, generation, scope],
                )?;
                ensure_checkpoint(&tx, generation, scope, false, observed_max_id)?;
                ensure_checkpoint(&tx, generation, scope, true, "")?;
                observed_max_id.to_owned()
            }
        };
        rewind_live_checkpoint_on_regression(&tx, generation, scope, observed_max_id)?;
        let pause = refresh_local_pause(&tx, generation, scope, pending_bytes, pause)?;
        let live = checkpoint(&tx, generation, scope, false)?;
        let history = checkpoint(&tx, generation, scope, true)?;
        tx.commit()?;
        Ok(vec![
            MmsBackfillScanPlan {
                phase: MmsBackfillPhase::Live,
                source_generation: generation.into(),
                subscription_id: scope.into(),
                cursor: live,
                history_upper_bound: Some(baseline.clone()),
                history_complete: complete,
                pause,
                budget: MAX_MMS_BACKFILL_RECHECK,
            },
            MmsBackfillScanPlan {
                phase: MmsBackfillPhase::History,
                source_generation: generation.into(),
                subscription_id: scope.into(),
                cursor: history,
                history_upper_bound: Some(baseline),
                history_complete: complete,
                pause,
                budget: MAX_MMS_BACKFILL_RECHECK,
            },
            MmsBackfillScanPlan {
                phase: MmsBackfillPhase::Recheck,
                source_generation: generation.into(),
                subscription_id: scope.into(),
                cursor: recheck_cursor,
                history_upper_bound: None,
                history_complete: complete,
                pause,
                budget: MAX_MMS_BACKFILL_RECHECK,
            },
        ])
    }

    /// Acknowledges a provider page after its acquisitions are durable. Recheck uses its own fair
    /// cursor and cannot be advanced through this method.
    pub fn advance_mms_backfill_page(
        &self,
        generation: &str,
        scope: &str,
        phase: MmsBackfillPhase,
        provider_id: &str,
    ) -> Result<(), Error> {
        validate_scope(generation, scope)?;
        validate_id(provider_id, "MMS checkpoint")?;
        let imported = match phase {
            MmsBackfillPhase::Live => false,
            MmsBackfillPhase::History => true,
            MmsBackfillPhase::Recheck => {
                return Err(Error::InvalidRequest("MMS recheck checkpoint"));
            }
        };
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = checkpoint(&tx, generation, scope, imported)?;
        if current
            .as_deref()
            .is_none_or(|current| id_number(provider_id) > id_number(current))
        {
            tx.execute(
                "INSERT INTO mms_scan_checkpoints(source_generation,subscription_id,imported,provider_message_id) VALUES(?,?,?,?) ON CONFLICT(source_generation,subscription_id,imported) DO UPDATE SET provider_message_id=excluded.provider_message_id",
                params![generation, scope, imported, provider_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Records page exhaustion. History only completes after every pending/blocked acquisition in
    /// the scope drains, preventing a false-complete state after a capacity pause.
    pub fn finish_mms_backfill_phase(
        &self,
        generation: &str,
        scope: &str,
        phase: MmsBackfillPhase,
        bound: &str,
        exhausted: bool,
    ) -> Result<bool, Error> {
        validate_scope(generation, scope)?;
        validate_id(bound, "MMS scan bound")?;
        if phase != MmsBackfillPhase::History || !exhausted {
            return Ok(false);
        }
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (baseline, _, _, _) = scope_row(&tx, generation, scope)?;
        if baseline.as_deref() != Some(bound) || pending_acquisitions(&tx, generation, scope)? {
            tx.commit()?;
            return Ok(false);
        }
        tx.execute(
            "UPDATE mms_backfill_scopes SET history_complete=1 WHERE source_generation=? AND subscription_id=?",
            params![generation, scope],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Moves a bounded cursor around pending work before returning requests, so a persistent head
    /// failure cannot monopolize repeated passes or create more acquisition rows.
    pub fn mms_backfill_recheck_requests(
        &self,
        generation: &str,
        scope: &str,
    ) -> Result<MmsBackfillRecheck, Error> {
        validate_scope(generation, scope)?;
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (_, _, _, legacy_cursor) = scope_row(&tx, generation, scope)?;
        let legacy_cursor = legacy_cursor
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or_default();
        let (pending_cursor, completed_cursor) =
            recheck_cursors(&tx, generation, scope, legacy_cursor)?;
        let pending_budget = MAX_MMS_BACKFILL_RECHECK / 2;
        let (mut requests, pending_next) = recheck_partition(
            &tx,
            generation,
            "state IN ('pending','blocked')",
            pending_cursor,
            pending_budget,
        )?;
        let (completed, completed_next) = recheck_partition(
            &tx,
            generation,
            "state='complete'",
            completed_cursor,
            MAX_MMS_BACKFILL_RECHECK - requests.len(),
        )?;
        requests.extend(completed);
        let next = pending_next.or(completed_next);
        store_recheck_cursors(
            &tx,
            generation,
            scope,
            pending_next.unwrap_or(0),
            completed_next.unwrap_or(0),
        )?;
        tx.execute(
            "UPDATE mms_backfill_scopes SET pending_recheck_cursor=? WHERE source_generation=? AND subscription_id=?",
            params![next.map(|value| value.to_string()), generation, scope],
        )?;
        tx.commit()?;
        Ok(MmsBackfillRecheck {
            requests: requests
                .into_iter()
                .map(
                    |(_, provider_message_id, direction)| MmsBackfillRecheckRequest {
                        provider_message_id,
                        direction,
                    },
                )
                .collect(),
            cursor: next.map(|value| value.to_string()),
            budget: MAX_MMS_BACKFILL_RECHECK,
        })
    }

    pub fn pause_mms_backfill_for_local_capacity(
        &self,
        generation: &str,
        scope: &str,
    ) -> Result<(), Error> {
        self.set_mms_backfill_pause(generation, scope, Some(MmsBackfillPause::LocalCapacity))
    }

    /// Call only for the server's canonical `vault_quota_exceeded` response.
    pub fn report_mms_backfill_vault_quota_exceeded(
        &self,
        generation: &str,
        scope: &str,
    ) -> Result<(), Error> {
        self.set_mms_backfill_pause(generation, scope, Some(MmsBackfillPause::RemoteStorage))
    }

    /// A successful reserve or refreshed usage result clears a remote-full pause; it never clears
    /// local/recovery pauses whose own measurements are still authoritative.
    pub fn report_mms_backfill_remote_capacity(
        &self,
        generation: &str,
        scope: &str,
        has_capacity: bool,
    ) -> Result<(), Error> {
        validate_scope(generation, scope)?;
        let guard = self.lock()?;
        if has_capacity {
            guard.conn.execute(
                "UPDATE mms_backfill_scopes SET pause_reason=NULL WHERE source_generation=? AND subscription_id=? AND pause_reason=?",
                params![generation, scope, PAUSE_REMOTE_STORAGE],
            )?;
        } else {
            guard.conn.execute(
                "UPDATE mms_backfill_scopes SET pause_reason=? WHERE source_generation=? AND subscription_id=?",
                params![PAUSE_REMOTE_STORAGE, generation, scope],
            )?;
        }
        Ok(())
    }

    pub fn mms_backfill_status(
        &self,
        generation: &str,
        scope: &str,
    ) -> Result<MmsBackfillStatus, Error> {
        validate_scope(generation, scope)?;
        let guard = self.lock()?;
        let row = guard.conn.query_row(
            "SELECT history_upper_bound,history_complete,pause_reason,pending_recheck_cursor FROM mms_backfill_scopes WHERE source_generation=? AND subscription_id=?",
            params![generation, scope],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).optional()?;
        let Some((baseline, complete, pause, cursor)) = row else {
            return Ok(MmsBackfillStatus {
                baseline_initialized: false,
                history_upper_bound: None,
                history_complete: false,
                pause: None,
                pending_count: 0,
                recheck_cursor: None,
            });
        };
        let pending: i64 = guard.conn.query_row(
            "SELECT (SELECT COUNT(*) FROM mms_acquisitions WHERE source_generation=? AND state IN ('pending','blocked')) + (SELECT COUNT(*) FROM mms_provider_identity_conflicts WHERE source_generation=?)",
            params![generation, generation],
            |row| row.get(0),
        )?;
        Ok(MmsBackfillStatus {
            baseline_initialized: baseline.is_some(),
            history_upper_bound: baseline,
            history_complete: complete,
            pause: MmsBackfillPause::parse(pause)?,
            pending_count: pending.try_into().map_err(|_| Error::Database)?,
            recheck_cursor: cursor,
        })
    }

    fn set_mms_backfill_pause(
        &self,
        generation: &str,
        scope: &str,
        pause: Option<MmsBackfillPause>,
    ) -> Result<(), Error> {
        validate_scope(generation, scope)?;
        let guard = self.lock()?;
        guard.conn.execute(
            "UPDATE mms_backfill_scopes SET pause_reason=? WHERE source_generation=? AND subscription_id=?",
            params![pause.map(MmsBackfillPause::code), generation, scope],
        )?;
        Ok(())
    }
}

fn ensure_scope(
    conn: &Connection,
    generation: &str,
    scope: &str,
    observed_max_id: &str,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO mms_backfill_scopes(source_generation,subscription_id,migration_done) VALUES(?,?,1) ON CONFLICT(source_generation,subscription_id) DO NOTHING",
        params![generation, scope],
    )?;
    let missing: bool = conn.query_row(
        "SELECT history_upper_bound IS NULL FROM mms_backfill_scopes WHERE source_generation=? AND subscription_id=?",
        params![generation, scope],
        |row| row.get(0),
    )?;
    if missing {
        conn.execute("UPDATE mms_backfill_scopes SET history_upper_bound=? WHERE source_generation=? AND subscription_id=?", params![observed_max_id, generation, scope])?;
        ensure_checkpoint(conn, generation, scope, false, observed_max_id)?;
        ensure_checkpoint(conn, generation, scope, true, "")?;
    }
    Ok(())
}

/// Legacy Android stored an acquisition UUID here, while current core stores a local-order cursor.
/// Resolve only a known acquisition in this source generation; an expired/pruned UUID safely starts
/// a new bounded pass instead of blocking migration.
fn resolve_legacy_recheck_cursor(
    conn: &Connection,
    generation: &str,
    cursor: Option<&str>,
) -> Result<Option<String>, Error> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    if cursor.parse::<i64>().is_ok() {
        return Ok(Some(cursor.into()));
    }
    let order = conn
        .query_row(
        "SELECT local_order FROM mms_acquisitions WHERE acquisition_id=? AND source_generation=?",
        params![cursor, generation],
        |row| row.get::<_, i64>(0),
    )
        .optional()?;
    Ok(order.map(|order| order.to_string()))
}

/// A lower observed provider maximum means IDs may have been recycled. Keep the immutable history
/// baseline but rewind the live cursor under the same generation so the recreated range is read.
fn rewind_live_checkpoint_on_regression(
    conn: &Connection,
    generation: &str,
    scope: &str,
    observed_max_id: &str,
) -> Result<(), Error> {
    let Some(live) = checkpoint(conn, generation, scope, false)? else {
        return Ok(());
    };
    if id_number(observed_max_id) < id_number(&live) {
        conn.execute(
            "UPDATE mms_scan_checkpoints SET provider_message_id=? WHERE source_generation=? AND subscription_id=? AND imported=0",
            params![observed_max_id, generation, scope],
        )?;
    }
    Ok(())
}

fn ensure_checkpoint(
    conn: &Connection,
    generation: &str,
    scope: &str,
    imported: bool,
    value: &str,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO mms_scan_checkpoints(source_generation,subscription_id,imported,provider_message_id) VALUES(?,?,?,?) ON CONFLICT(source_generation,subscription_id,imported) DO NOTHING",
        params![generation, scope, imported, value],
    )?;
    Ok(())
}

fn scope_row(
    conn: &Connection,
    generation: &str,
    scope: &str,
) -> Result<
    (
        Option<String>,
        bool,
        Option<MmsBackfillPause>,
        Option<String>,
    ),
    Error,
> {
    let row = conn.query_row(
        "SELECT history_upper_bound,history_complete,pause_reason,pending_recheck_cursor FROM mms_backfill_scopes WHERE source_generation=? AND subscription_id=?",
        params![generation, scope],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).optional()?.ok_or(Error::Database)?;
    Ok((row.0, row.1, MmsBackfillPause::parse(row.2)?, row.3))
}

fn checkpoint(
    conn: &Connection,
    generation: &str,
    scope: &str,
    imported: bool,
) -> Result<Option<String>, Error> {
    conn.query_row(
        "SELECT provider_message_id FROM mms_scan_checkpoints WHERE source_generation=? AND subscription_id=? AND imported=?",
        params![generation, scope, imported],
        |row| row.get::<_, String>(0),
    ).optional().map(|value| value.filter(|value| !value.is_empty())).map_err(Into::into)
}

fn refresh_local_pause(
    conn: &Connection,
    generation: &str,
    scope: &str,
    pending_bytes: u64,
    pause: Option<MmsBackfillPause>,
) -> Result<Option<MmsBackfillPause>, Error> {
    let capacity = recovery_capacity_reached(conn)?
        || pending_bytes >= MAX_PENDING_MMS_MEDIA_BYTES
        || local_acquisition_capacity_reached(conn, generation, scope)?;
    match (pause, capacity) {
        (Some(MmsBackfillPause::RemoteStorage), _) => Ok(Some(MmsBackfillPause::RemoteStorage)),
        (_, true) => {
            let pause = if recovery_capacity_reached(conn)? {
                MmsBackfillPause::RecoveryCapacity
            } else {
                MmsBackfillPause::LocalCapacity
            };
            conn.execute("UPDATE mms_backfill_scopes SET pause_reason=? WHERE source_generation=? AND subscription_id=?", params![pause.code(), generation, scope])?;
            Ok(Some(pause))
        }
        (Some(MmsBackfillPause::RecoveryCapacity | MmsBackfillPause::LocalCapacity), false) => {
            conn.execute("UPDATE mms_backfill_scopes SET pause_reason=NULL WHERE source_generation=? AND subscription_id=?", params![generation, scope])?;
            Ok(None)
        }
        (pause, false) => Ok(pause),
    }
}

fn pending_acquisitions(conn: &Connection, generation: &str, _scope: &str) -> Result<bool, Error> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM mms_acquisitions WHERE source_generation=? AND state IN ('pending','blocked') UNION SELECT 1 FROM mms_provider_identity_conflicts WHERE source_generation=?)", params![generation, generation], |row| row.get(0)).map_err(Into::into)
}

fn local_acquisition_capacity_reached(
    conn: &Connection,
    generation: &str,
    _scope: &str,
) -> Result<bool, Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM mms_acquisitions WHERE source_generation=? AND state IN ('pending','blocked')",
        params![generation],
        |row| row.get(0),
    )?;
    Ok(usize::try_from(count).map_err(|_| Error::Database)? >= MAX_MMS_ACQUISITIONS)
}

fn recheck_cursors(
    conn: &Connection,
    generation: &str,
    scope: &str,
    legacy_cursor: i64,
) -> Result<(i64, i64), Error> {
    conn.query_row(
        "SELECT pending_cursor,completed_cursor FROM mms_backfill_recheck_cursors WHERE source_generation=? AND subscription_id=?",
        params![generation, scope],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()?
    .map_or(Ok((legacy_cursor, 0)), Ok)
}

fn store_recheck_cursors(
    conn: &Connection,
    generation: &str,
    scope: &str,
    pending: i64,
    completed: i64,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO mms_backfill_recheck_cursors(source_generation,subscription_id,pending_cursor,completed_cursor) VALUES(?,?,?,?) ON CONFLICT(source_generation,subscription_id) DO UPDATE SET pending_cursor=excluded.pending_cursor,completed_cursor=excluded.completed_cursor",
        params![generation, scope, pending, completed],
    )?;
    Ok(())
}

fn recheck_partition(
    conn: &Connection,
    generation: &str,
    state_predicate: &str,
    cursor: i64,
    limit: usize,
) -> Result<(Vec<(i64, String, Direction)>, Option<i64>), Error> {
    if limit == 0 {
        return Ok((Vec::new(), Some(cursor)));
    }
    let query = format!(
        "SELECT local_order,provider_message_id,direction FROM mms_acquisitions WHERE source_generation=? AND {state_predicate} AND local_order>? ORDER BY local_order LIMIT ?"
    );
    let mut statement = conn.prepare(&query)?;
    let mut rows = statement
        .query_map(params![generation, cursor, limit], decode_recheck_row)?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.is_empty() && cursor != 0 {
        rows = statement
            .query_map(params![generation, 0, limit], decode_recheck_row)?
            .collect::<Result<Vec<_>, _>>()?;
    }
    let next = rows.last().map(|row| row.0);
    Ok((rows, next))
}

fn decode_recheck_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, String, Direction)> {
    let direction = match row.get::<_, String>(2)?.as_str() {
        "incoming" => Direction::Incoming,
        "outgoing" => Direction::Outgoing,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    Ok((row.get(0)?, row.get(1)?, direction))
}

pub(crate) fn record_provider_identity_conflict(
    conn: &Connection,
    input: &MmsAcquisitionInput,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO mms_provider_identity_conflicts(source_generation,subscription_id,provider_message_id,direction,reason) VALUES(?,?,?,?,?) ON CONFLICT(source_generation,subscription_id,provider_message_id,direction) DO UPDATE SET reason=excluded.reason",
        params![
            input.source.source_generation,
            input.source.subscription_id,
            input.source.provider_message_id,
            crate::mms::direction_code(input.direction),
            "provider_identity_changed",
        ],
    )?;
    Ok(())
}

fn recovery_capacity_reached(conn: &Connection) -> Result<bool, Error> {
    let records: i64 = conn.query_row(
        "SELECT COALESCE(MAX(local_order),0) FROM messages",
        [],
        |row| row.get(0),
    )?;
    let pages = pragma_integer(conn, "PRAGMA page_count")?;
    let page_size = pragma_integer(conn, "PRAGMA page_size")?;
    let records: u64 = records.try_into().map_err(|_| Error::Database)?;
    let bytes: u64 = pages
        .saturating_mul(page_size)
        .try_into()
        .map_err(|_| Error::Database)?;
    Ok(records >= MAX_SNAPSHOT_RECORDS - RECOVERY_RECORD_RESERVE
        || bytes >= MAX_SNAPSHOT_BYTES - RECOVERY_BYTE_RESERVE)
}

fn pragma_integer(conn: &Connection, pragma: &str) -> Result<i64, Error> {
    match conn.query_row(pragma, [], |row| row.get::<_, rusqlite::types::Value>(0))? {
        rusqlite::types::Value::Integer(value) => Ok(value),
        rusqlite::types::Value::Text(value) => value.parse().map_err(|_| Error::Database),
        _ => Err(Error::Database),
    }
}

fn validate_scope(generation: &str, scope: &str) -> Result<(), Error> {
    if generation.is_empty()
        || generation.len() > MAX_PROVIDER_ID_BYTES
        || scope.is_empty()
        || scope.len() > MAX_SUBSCRIPTION_BYTES
    {
        return Err(Error::InvalidRequest("MMS backfill scope"));
    }
    Ok(())
}

fn validate_optional_id(value: Option<&str>, name: &'static str) -> Result<(), Error> {
    value
        .map(|value| validate_id(value, name))
        .transpose()
        .map(|_| ())
}

fn validate_id(value: &str, name: &'static str) -> Result<(), Error> {
    if value.parse::<u64>().is_err() {
        return Err(Error::InvalidRequest(name));
    }
    Ok(())
}

fn id_number(value: &str) -> u64 {
    value.parse().expect("validated provider identifier")
}
