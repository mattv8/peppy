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
    pub descending: bool,
    pub history_floor: Option<String>,
    /// Frozen maximum ID for history/recheck classification; absent only before initialization.
    pub history_upper_bound: Option<String>,
    pub history_complete: bool,
    pub pause: Option<MmsBackfillPause>,
    /// Phase-specific execution decision. `pause` remains the diagnostic reason.
    pub paused: bool,
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
    pub quarantined_count: u64,
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
            tx.execute(
                "INSERT INTO mms_backfill_scopes(source_generation,subscription_id,history_upper_bound,history_complete,pending_recheck_cursor,migration_done,history_descending) VALUES(?,?,?,?,?,1,1)",
                params![generation, scope, legacy.history_baseline, false, recheck_cursor],
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
        let (baseline, complete, pause, recheck_cursor, descending, floor, history_cursor) =
            scope_row(&tx, generation, scope)?;
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
        let gate_open = if complete {
            true
        } else {
            crate::history_import::history_import_gate_open(&tx, crate::notifications::now_ms())?
        };
        let outbox_backpressured =
            !complete && crate::history_import::outbox_backpressures_history(&tx)?;
        let live = checkpoint(&tx, generation, scope, false)?;
        let history = checkpoint(&tx, generation, scope, true)?;
        tx.commit()?;
        Ok(vec![
            MmsBackfillScanPlan {
                phase: MmsBackfillPhase::Live,
                source_generation: generation.into(),
                subscription_id: scope.into(),
                cursor: live,
                descending: false,
                history_floor: None,
                history_upper_bound: Some(baseline.clone()),
                history_complete: complete,
                pause,
                paused: matches!(
                    pause,
                    Some(MmsBackfillPause::RecoveryCapacity | MmsBackfillPause::LocalCapacity)
                ),
                budget: MAX_MMS_BACKFILL_RECHECK,
            },
            MmsBackfillScanPlan {
                phase: MmsBackfillPhase::History,
                source_generation: generation.into(),
                subscription_id: scope.into(),
                cursor: if descending { history_cursor } else { history },
                descending,
                history_floor: floor,
                history_upper_bound: Some(baseline),
                history_complete: complete,
                pause,
                paused: pause.is_some() || !gate_open || outbox_backpressured,
                budget: MAX_MMS_BACKFILL_RECHECK,
            },
            MmsBackfillScanPlan {
                phase: MmsBackfillPhase::Recheck,
                source_generation: generation.into(),
                subscription_id: scope.into(),
                cursor: recheck_cursor,
                descending: false,
                history_floor: None,
                history_upper_bound: None,
                history_complete: complete,
                pause,
                paused: false,
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
        let (baseline, _, _, _, descending, floor, cursor) = scope_row(&tx, generation, scope)?;
        let current = if phase == MmsBackfillPhase::History && descending {
            cursor
        } else {
            checkpoint(&tx, generation, scope, imported)?
        };
        let advances = if phase == MmsBackfillPhase::History && descending {
            crate::history_import::advances_descending_cursor(
                current.as_deref(),
                floor.as_deref(),
                baseline.as_deref().ok_or(Error::Database)?,
                provider_id,
            )?
        } else {
            match current.as_deref() {
                Some(current) => id_number(provider_id)? > id_number(current)?,
                None => true,
            }
        };
        if advances {
            if phase == MmsBackfillPhase::History && descending {
                tx.execute("UPDATE mms_backfill_scopes SET history_cursor=? WHERE source_generation=? AND subscription_id=?", params![provider_id, generation, scope])?;
            } else {
                tx.execute(
                "INSERT INTO mms_scan_checkpoints(source_generation,subscription_id,imported,provider_message_id) VALUES(?,?,?,?) ON CONFLICT(source_generation,subscription_id,imported) DO UPDATE SET provider_message_id=excluded.provider_message_id",
                params![generation, scope, imported, provider_id],
            )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Records page exhaustion. History only completes after every actionable pending acquisition
    /// in the scope drains; blocked and unavailable rows are terminal bookkeeping.
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
        let (baseline, _, _, _, _, _, _) = scope_row(&tx, generation, scope)?;
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
        let (_, _, _, legacy_cursor, _, _, _) = scope_row(&tx, generation, scope)?;
        let legacy_cursor = legacy_cursor
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or_default();
        let (pending_cursor, completed_cursor) =
            recheck_cursors(&tx, generation, scope, legacy_cursor)?;
        let pending_budget = MAX_MMS_BACKFILL_RECHECK / 2;
        let (mut requests, pending_next) = recheck_partition(
            &tx,
            generation,
            "state='pending'",
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

    /// Reopens only bounded terminal local work for a user-requested provider recheck. Completed
    /// messages remain immutable and this API does not authorize any carrier action.
    pub fn request_mms_recheck(
        &self,
        generation: &str,
        scope: &str,
        limit: usize,
    ) -> Result<usize, Error> {
        validate_scope(generation, scope)?;
        if limit == 0 || limit > MAX_MMS_BACKFILL_RECHECK {
            return Err(Error::InvalidRequest("MMS recheck limit"));
        }
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE mms_acquisitions SET state='pending',reason='manual recheck' WHERE acquisition_id IN (
                SELECT acquisition_id FROM mms_acquisitions
                WHERE source_generation=? AND (subscription_id=? OR ?='android-mms-provider')
                  AND state IN ('blocked','unavailable')
                  AND reason IS NOT 'unknown_subscription'
                  AND subscription_id <> 'android-mms-unknown-subscription'
                ORDER BY local_order LIMIT ?
            )",
            params![generation, scope, scope, limit],
        )?;
        tx.commit()?;
        Ok(changed)
    }

    /// Records a transient local-capacity diagnostic for additive compatibility. Scan planning
    /// immediately recomputes the effective pause from measured recovery, media, acquisition, and
    /// persisted remote-capacity facts; this marker is not independent pause authority.
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
        self.report_mms_backfill_remote_capacity(generation, scope, false)
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
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO mms_backfill_remote_capacity(source_generation,subscription_id,storage_full) VALUES(?,?,?) ON CONFLICT(source_generation,subscription_id) DO UPDATE SET storage_full=excluded.storage_full",
            params![generation, scope, !has_capacity],
        )?;
        if has_capacity {
            tx.execute(
                "UPDATE mms_backfill_scopes SET pause_reason=NULL WHERE source_generation=? AND subscription_id=? AND pause_reason=?",
                params![generation, scope, PAUSE_REMOTE_STORAGE],
            )?;
        } else {
            tx.execute(
                "UPDATE mms_backfill_scopes SET pause_reason=? WHERE source_generation=? AND subscription_id=? AND (pause_reason IS NULL OR pause_reason=?)",
                params![PAUSE_REMOTE_STORAGE, generation, scope, PAUSE_REMOTE_STORAGE],
            )?;
        }
        tx.commit()?;
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
                quarantined_count: quarantine_count(&guard.conn, generation, scope)?,
                recheck_cursor: None,
            });
        };
        let pending: i64 = guard.conn.query_row(
            "SELECT COUNT(*) FROM mms_acquisitions WHERE source_generation=? AND (subscription_id=? OR ?='android-mms-provider') AND state='pending'",
            params![generation, scope, scope],
            |row| row.get(0),
        )?;
        Ok(MmsBackfillStatus {
            baseline_initialized: baseline.is_some(),
            history_upper_bound: baseline,
            history_complete: complete,
            pause: MmsBackfillPause::parse(pause)?,
            pending_count: pending.try_into().map_err(|_| Error::Database)?,
            quarantined_count: quarantine_count(&guard.conn, generation, scope)?,
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
        "INSERT INTO mms_backfill_scopes(source_generation,subscription_id,migration_done,history_descending) VALUES(?,?,1,1) ON CONFLICT(source_generation,subscription_id) DO NOTHING",
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
    if id_number(observed_max_id)? < id_number(&live)? {
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

type ScopeRow = (
    Option<String>,
    bool,
    Option<MmsBackfillPause>,
    Option<String>,
    bool,
    Option<String>,
    Option<String>,
);

fn scope_row(conn: &Connection, generation: &str, scope: &str) -> Result<ScopeRow, Error> {
    let row = conn.query_row(
        "SELECT history_upper_bound,history_complete,pause_reason,pending_recheck_cursor,history_descending,history_floor,history_cursor FROM mms_backfill_scopes WHERE source_generation=? AND subscription_id=?",
        params![generation, scope],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
    ).optional()?.ok_or(Error::Database)?;
    Ok((
        row.0,
        row.1,
        MmsBackfillPause::parse(row.2)?,
        row.3,
        row.4,
        row.5,
        row.6,
    ))
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
    _pause: Option<MmsBackfillPause>,
) -> Result<Option<MmsBackfillPause>, Error> {
    let recovery_full = recovery_capacity_reached(conn)?;
    let local_full = pending_bytes >= MAX_PENDING_MMS_MEDIA_BYTES
        || crate::mms::pending_acquisition_capacity_reached(conn)?;
    let remote_full = remote_storage_full(conn, generation, scope)?;
    let pause = effective_pause(recovery_full, local_full, remote_full);
    conn.execute(
        "UPDATE mms_backfill_scopes SET pause_reason=? WHERE source_generation=? AND subscription_id=?",
        params![pause.map(MmsBackfillPause::code), generation, scope],
    )?;
    Ok(pause)
}

fn effective_pause(
    recovery_full: bool,
    local_full: bool,
    remote_full: bool,
) -> Option<MmsBackfillPause> {
    if recovery_full {
        Some(MmsBackfillPause::RecoveryCapacity)
    } else if local_full {
        Some(MmsBackfillPause::LocalCapacity)
    } else if remote_full {
        Some(MmsBackfillPause::RemoteStorage)
    } else {
        None
    }
}

fn remote_storage_full(conn: &Connection, generation: &str, scope: &str) -> Result<bool, Error> {
    conn.query_row(
        "SELECT storage_full FROM mms_backfill_remote_capacity WHERE source_generation=? AND subscription_id=?",
        params![generation, scope],
        |row| row.get(0),
    )
    .optional()
    .map(|value| value.unwrap_or(false))
    .map_err(Into::into)
}

fn pending_acquisitions(conn: &Connection, generation: &str, scope: &str) -> Result<bool, Error> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM mms_acquisitions WHERE source_generation=? AND (subscription_id=? OR ?='android-mms-provider') AND state='pending')", params![generation, scope, scope], |row| row.get(0)).map_err(Into::into)
}

fn quarantine_count(conn: &Connection, generation: &str, scope: &str) -> Result<u64, Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM mms_provider_identity_quarantines WHERE source_generation=? AND (subscription_id=? OR ?='android-mms-provider')",
        params![generation, scope, scope],
        |row| row.get(0),
    )?;
    count.try_into().map_err(|_| Error::Database)
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

type RecheckRow = (i64, String, Direction);

fn recheck_partition(
    conn: &Connection,
    generation: &str,
    state_predicate: &str,
    cursor: i64,
    limit: usize,
) -> Result<(Vec<RecheckRow>, Option<i64>), Error> {
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

pub(crate) fn record_provider_identity_quarantine(
    conn: &Connection,
    input: &MmsAcquisitionInput,
    reason: &str,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO mms_provider_identity_quarantines(source_generation,subscription_id,provider_message_id,direction,evidence_json,recorded_at_ms) VALUES(?,?,?,?,?,?) ON CONFLICT(source_generation,subscription_id,provider_message_id,direction) DO UPDATE SET evidence_json=excluded.evidence_json,recorded_at_ms=excluded.recorded_at_ms",
        params![
            input.source.source_generation,
            input.source.subscription_id,
            input.source.provider_message_id,
            crate::mms::direction_code(input.direction),
            super::json(&serde_json::json!({"reason": reason, "input": input}))?,
            crate::notifications::now_ms(),
        ],
    )?;
    conn.execute(
        "DELETE FROM mms_provider_identity_quarantines WHERE rowid IN (
            SELECT rowid FROM mms_provider_identity_quarantines
            ORDER BY recorded_at_ms DESC, rowid DESC LIMIT -1 OFFSET ?
        )",
        params![MAX_MMS_ACQUISITIONS as i64],
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

fn id_number(value: &str) -> Result<u64, Error> {
    value
        .parse()
        .map_err(|_| Error::InvalidRequest("MMS checkpoint"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capacity_connection() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE messages(local_order INTEGER NOT NULL);
             CREATE TABLE mms_acquisitions(state TEXT NOT NULL);
             CREATE TABLE mms_backfill_scopes(
               source_generation TEXT NOT NULL,
               subscription_id TEXT NOT NULL,
               pause_reason TEXT,
               PRIMARY KEY(source_generation,subscription_id)
             );
             CREATE TABLE mms_backfill_remote_capacity(
               source_generation TEXT NOT NULL,
               subscription_id TEXT NOT NULL,
               storage_full INTEGER NOT NULL,
               PRIMARY KEY(source_generation,subscription_id)
             );
             INSERT INTO mms_backfill_scopes(source_generation,subscription_id) VALUES('generation','scope');
             INSERT INTO mms_backfill_remote_capacity(source_generation,subscription_id,storage_full) VALUES('generation','scope',1);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn remote_full_with_local_media_full_prefers_local_then_restores_remote() {
        let conn = capacity_connection();

        assert_eq!(
            refresh_local_pause(
                &conn,
                "generation",
                "scope",
                MAX_PENDING_MMS_MEDIA_BYTES,
                Some(MmsBackfillPause::RemoteStorage),
            )
            .unwrap(),
            Some(MmsBackfillPause::LocalCapacity)
        );
        assert_eq!(
            refresh_local_pause(&conn, "generation", "scope", 0, None).unwrap(),
            Some(MmsBackfillPause::RemoteStorage)
        );
    }

    #[test]
    fn remote_full_with_recovery_full_prefers_recovery() {
        let conn = capacity_connection();
        conn.execute(
            "INSERT INTO messages(local_order) VALUES(?)",
            [MAX_SNAPSHOT_RECORDS - RECOVERY_RECORD_RESERVE],
        )
        .unwrap();

        assert_eq!(
            refresh_local_pause(
                &conn,
                "generation",
                "scope",
                MAX_PENDING_MMS_MEDIA_BYTES,
                Some(MmsBackfillPause::RemoteStorage),
            )
            .unwrap(),
            Some(MmsBackfillPause::RecoveryCapacity)
        );
    }
}
