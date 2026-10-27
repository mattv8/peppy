//! Durable, vault-scoped MMS provider-history lifecycle.
//!
//! The Android reader owns provider I/O; this module owns the frozen baseline, progress, capacity
//! pauses, and bounded fair retries. It deliberately never changes a source generation: reused
//! provider IDs need scanner evidence, not a guessed destructive deduplication.

use super::*;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params, params_from_iter};
use serde::{Deserialize, Serialize};

pub const MAX_MMS_BACKFILL_RECHECK: usize = 32;
pub const MMS_COMPLETED_RECHECK_REST_MS: i64 = 24 * 60 * 60 * 1000;
const MMS_RECHECK_BACKOFF_MS: i64 = 15 * 60 * 1000;
const MMS_RECHECK_MAX_BACKOFF_MS: i64 = 24 * 60 * 60 * 1000;
const MMS_AWAITING_PROVIDER_BACKOFF_MS: i64 = 15 * 60 * 1000;
const MMS_AWAITING_PROVIDER_IMMEDIATE_OBSERVATIONS: i64 = 2;
// Pending candidates are the newest rows by local order; the fair sort then runs over at most this
// many rows, so completed history never enters the recheck sort.
// One bounded live provider page can add a message, preview, binding and demand records for each
// attachment. Keep additional room for SMS/status records that core cannot classify here.
const LIVE_SNAPSHOT_RECORD_HEADROOM: u64 =
    (MAX_MMS_BACKFILL_RECHECK as u64) * ((MAX_MMS_ATTACHMENTS as u64) * 2 + 3) + 512;
const SNAPSHOT_COUNT_META: &str = "mms_recovery_snapshot_record_count";
const SNAPSHOT_HIGH_WATER_META: &str = "mms_recovery_snapshot_high_water";
const SNAPSHOT_ACKS_META: &str = "mms_recovery_acks_since_cut";
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
    /// The snapshot hard limit is reached. No acquisition can safely be admitted.
    AbsoluteRecoveryCapacity,
    RecoveryCapacity,
    LocalCapacity,
    RemoteStorage,
}

impl MmsBackfillPause {
    fn code(self) -> &'static str {
        match self {
            Self::AbsoluteRecoveryCapacity => "absolute_recovery_capacity",
            Self::RecoveryCapacity => PAUSE_RECOVERY_CAPACITY,
            Self::LocalCapacity => PAUSE_LOCAL_CAPACITY,
            Self::RemoteStorage => PAUSE_REMOTE_STORAGE,
        }
    }

    fn parse(value: Option<String>) -> Result<Option<Self>, Error> {
        value
            .map(|value| match value.as_str() {
                "absolute_recovery_capacity" => Ok(Self::AbsoluteRecoveryCapacity),
                PAUSE_RECOVERY_CAPACITY => Ok(Self::RecoveryCapacity),
                PAUSE_LOCAL_CAPACITY => Ok(Self::LocalCapacity),
                PAUSE_REMOTE_STORAGE => Ok(Self::RemoteStorage),
                _ => Err(Error::Database),
            })
            .transpose()
    }
}

/// Host evidence for one provider recheck attempt. The core deliberately does not infer this from
/// a planned request: provider reads and their result belong to the Android host.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MmsBackfillRecheckOutcome {
    /// The host reread a completed provider row and confirmed it remains complete.
    Verified,
    Progress,
    UnchangedUnavailable,
    UnchangedPartUnavailable,
    TransientFailure,
    ProviderChanged,
    /// Carrier content is still arriving. The first observations stay immediately due, then the
    /// row rests on a fixed interval so the provider is not re-read on every pass.
    AwaitingProvider,
}

/// Conditions that affect whether maintenance work may run. They are host facts, not guesses.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct MmsBackfillRecheckConditions {
    pub charging: bool,
    pub capacity_changed: bool,
    pub explicit_refresh: bool,
    /// A debounced provider observer hint. This resets at most one pending recheck budget.
    #[serde(default)]
    pub provider_changed: bool,
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
    #[serde(default)]
    pub acquisition_id: String,
    pub provider_message_id: String,
    pub direction: Direction,
    #[serde(default = "default_recheck_state")]
    pub state: MmsAcquisitionState,
}

fn default_recheck_state() -> MmsAcquisitionState {
    MmsAcquisitionState::Pending
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct MmsBackfillRecheck {
    pub requests: Vec<MmsBackfillRecheckRequest>,
    /// Opaque durable fairness cursor. `None` means the next call begins a fresh round.
    pub cursor: Option<String>,
    pub budget: usize,
    /// A host may schedule one more bounded pass only for actual due work, never housekeeping.
    #[serde(default)]
    pub continuation_due: bool,
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
    // Kept outside the main schema migrations so a restored database gains the additive lifecycle
    // state during normal core initialization without rewriting acquisition rows.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mms_backfill_recheck_state(
           acquisition_id TEXT PRIMARY KEY,
           next_due_at_ms INTEGER NOT NULL DEFAULT 0,
           failure_count INTEGER NOT NULL DEFAULT 0,
           last_reason TEXT
         );
         CREATE TABLE IF NOT EXISTS mms_backfill_recheck_attempts(
           acquisition_id TEXT PRIMARY KEY,
           attempted_at_ms INTEGER NOT NULL
         );",
    )?;
    // Completed-sweep keyset order is (length, provider id, local order), descending. The exact
    // scope and provider-wide queries each use their own index so neither needs a temp sort.
    conn.execute_batch(
        "DROP INDEX IF EXISTS mms_acquisitions_completed_provider_numeric;
         DROP INDEX IF EXISTS mms_acquisitions_completed_provider_numeric_wide;
         CREATE INDEX IF NOT EXISTS mms_acquisitions_completed_sweep
           ON mms_acquisitions(source_generation,subscription_id,state,length(provider_message_id) DESC,provider_message_id DESC,local_order DESC);
         CREATE INDEX IF NOT EXISTS mms_acquisitions_completed_sweep_wide
           ON mms_acquisitions(source_generation,state,length(provider_message_id) DESC,provider_message_id DESC,local_order DESC);",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS mms_backfill_completed_sweeps(
           source_generation TEXT NOT NULL,
           subscription_id TEXT NOT NULL,
           started_at_ms INTEGER NOT NULL DEFAULT 0,
           completed_at_ms INTEGER,
           cursor_len INTEGER,
           cursor_provider TEXT,
           cursor_order INTEGER,
           batch_len INTEGER,
           batch_provider TEXT,
           batch_order INTEGER,
           PRIMARY KEY(source_generation, subscription_id)
         );
         CREATE TABLE IF NOT EXISTS mms_backfill_completed_claims(
           source_generation TEXT NOT NULL,
           subscription_id TEXT NOT NULL,
           acquisition_id TEXT NOT NULL,
           PRIMARY KEY(source_generation, subscription_id, acquisition_id)
         );
         CREATE INDEX IF NOT EXISTS mms_backfill_completed_claims_acquisition
           ON mms_backfill_completed_claims(acquisition_id);",
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
    /// Authenticated snapshot cut observed by the host. Historical local journal/outbox rows are
    /// not a server retention inventory: compacted records may remain locally forever.
    pub fn record_recovery_snapshot_capacity(
        &self,
        high_water: Cursor,
        record_count: u64,
    ) -> Result<(), Error> {
        if record_count > high_water.0 {
            return Err(Error::InvalidRequest("snapshot record count"));
        }
        to_i64(high_water.0)?;
        to_i64(record_count)?;
        let mut store = self.lock()?;
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        record_snapshot_capacity(&tx, high_water.0, record_count)?;
        tx.commit()?;
        Ok(())
    }
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
                // The reserve protects history recovery only. Live delivery may consume it until
                // the absolute snapshot ceiling, while genuine local media pressure still stops it.
                paused: matches!(
                    pause,
                    Some(
                        MmsBackfillPause::AbsoluteRecoveryCapacity
                            | MmsBackfillPause::LocalCapacity
                    )
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
                paused: matches!(pause, Some(MmsBackfillPause::AbsoluteRecoveryCapacity)),
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
        let continuation_due = !requests.is_empty();
        Ok(MmsBackfillRecheck {
            requests: requests
                .into_iter()
                .map(
                    |(acquisition_id, _, provider_message_id, direction, state)| {
                        MmsBackfillRecheckRequest {
                            acquisition_id,
                            provider_message_id,
                            direction,
                            state,
                        }
                    },
                )
                .collect(),
            cursor: next.map(|value| value.to_string()),
            budget: MAX_MMS_BACKFILL_RECHECK,
            continuation_due,
        })
    }

    /// Returns due pending work plus a charging-only (or explicit-refresh), newest-first sweep.
    /// Unlike the compatibility method above, unchanged failures are durably delayed and completed
    /// maintenance rests for 24 hours after an exhausted pass.
    pub fn mms_backfill_due_rechecks(
        &self,
        generation: &str,
        scope: &str,
        now_ms: i64,
        conditions: MmsBackfillRecheckConditions,
    ) -> Result<MmsBackfillRecheck, Error> {
        validate_scope(generation, scope)?;
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if conditions.capacity_changed || conditions.explicit_refresh {
            clear_recheck_due(&tx, generation, scope)?;
        }
        if conditions.provider_changed {
            reset_pending_recheck_due(&tx, generation, scope, MAX_MMS_BACKFILL_RECHECK)?;
        }
        let pending =
            due_recheck_partition(&tx, generation, scope, now_ms, MAX_MMS_BACKFILL_RECHECK)?;
        let mut requests = pending;
        if (conditions.charging || conditions.explicit_refresh)
            && requests.len() < MAX_MMS_BACKFILL_RECHECK
        {
            requests.extend(completed_recheck_partition(
                &tx,
                generation,
                scope,
                now_ms,
                conditions.explicit_refresh,
                MAX_MMS_BACKFILL_RECHECK - requests.len(),
            )?);
        }
        let continuation_due = !requests.is_empty();
        tx.commit()?;
        Ok(MmsBackfillRecheck {
            requests: requests
                .into_iter()
                .map(
                    |(acquisition_id, _, provider_message_id, direction, state)| {
                        MmsBackfillRecheckRequest {
                            acquisition_id,
                            provider_message_id,
                            direction,
                            state,
                        }
                    },
                )
                .collect(),
            cursor: None,
            budget: MAX_MMS_BACKFILL_RECHECK,
            continuation_due,
        })
    }

    /// Records actual host feedback for a recheck attempt. A provider-content change clears the
    /// delay immediately; unchanged non-actionable results back off durably.
    pub fn record_mms_backfill_recheck_outcome(
        &self,
        acquisition_id: &str,
        now_ms: i64,
        outcome: MmsBackfillRecheckOutcome,
        conditions: MmsBackfillRecheckConditions,
    ) -> Result<(), Error> {
        if acquisition_id.is_empty() || acquisition_id.len() > 256 {
            return Err(Error::InvalidRequest("MMS acquisition"));
        }
        let mut guard = self.lock()?;
        let tx = guard
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: MmsAcquisitionState = tx
            .query_row(
                "SELECT state FROM mms_acquisitions WHERE acquisition_id=?",
                [acquisition_id],
                |row| {
                    MmsAcquisitionState::parse(&row.get::<_, String>(0)?)
                        .ok_or(rusqlite::Error::InvalidQuery)
                },
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        if outcome == MmsBackfillRecheckOutcome::Verified && state != MmsAcquisitionState::Complete
        {
            return Err(Error::InvalidRequest("MMS completed verification"));
        }
        let reset = conditions.capacity_changed
            || conditions.explicit_refresh
            || matches!(
                outcome,
                MmsBackfillRecheckOutcome::Progress | MmsBackfillRecheckOutcome::ProviderChanged
            );
        if outcome == MmsBackfillRecheckOutcome::Verified {
            // Completed sweep rows may carry stale backoff state from before they were completed.
            // Verification resolves their claim without treating a successful provider reread as a
            // failure or creating a future pending-work deadline.
            tx.execute(
                "DELETE FROM mms_backfill_recheck_state WHERE acquisition_id=?",
                [acquisition_id],
            )?;
        } else {
            let (due, failures, reason) = if reset {
                (0, 0, None)
            } else {
                let previous: i64 = tx
                    .query_row(
                        "SELECT failure_count FROM mms_backfill_recheck_state WHERE acquisition_id=?",
                        [acquisition_id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .unwrap_or(0);
                let failures = previous.saturating_add(1).min(8);
                let exponential_due = || {
                    let delay = MMS_RECHECK_BACKOFF_MS
                        .saturating_mul(1_i64 << failures.saturating_sub(1))
                        .min(MMS_RECHECK_MAX_BACKOFF_MS);
                    now_ms.saturating_add(delay)
                };
                let (due, reason) = match outcome {
                    MmsBackfillRecheckOutcome::AwaitingProvider => (
                        if failures <= MMS_AWAITING_PROVIDER_IMMEDIATE_OBSERVATIONS {
                            0
                        } else {
                            now_ms.saturating_add(MMS_AWAITING_PROVIDER_BACKOFF_MS)
                        },
                        "awaiting_provider",
                    ),
                    MmsBackfillRecheckOutcome::UnchangedUnavailable => {
                        (exponential_due(), "unavailable")
                    }
                    MmsBackfillRecheckOutcome::UnchangedPartUnavailable => {
                        (exponential_due(), "part_unavailable")
                    }
                    MmsBackfillRecheckOutcome::TransientFailure => (exponential_due(), "transient"),
                    MmsBackfillRecheckOutcome::Progress
                    | MmsBackfillRecheckOutcome::ProviderChanged => {
                        unreachable!()
                    }
                    MmsBackfillRecheckOutcome::Verified => unreachable!(),
                };
                (due, failures, Some(reason))
            };
            tx.execute(
                "INSERT INTO mms_backfill_recheck_state(acquisition_id,next_due_at_ms,failure_count,last_reason)
                 VALUES(?,?,?,?)
                 ON CONFLICT(acquisition_id) DO UPDATE SET next_due_at_ms=excluded.next_due_at_ms,failure_count=excluded.failure_count,last_reason=excluded.last_reason",
                params![acquisition_id, due, failures, reason],
            )?;
        }
        tx.execute(
            "INSERT INTO mms_backfill_recheck_attempts(acquisition_id,attempted_at_ms) VALUES(?,?)
             ON CONFLICT(acquisition_id) DO UPDATE SET attempted_at_ms=excluded.attempted_at_ms",
            params![acquisition_id, now_ms],
        )?;
        // The attempt is resolved, so its completed-sweep claim no longer blocks the next batch.
        let claimed = claimed_sweep_scopes(&tx, acquisition_id)?;
        tx.execute(
            "DELETE FROM mms_backfill_completed_claims WHERE acquisition_id=?",
            [acquisition_id],
        )?;
        for (generation, scope) in claimed {
            fold_resolved_completed_batch(&tx, &generation, &scope)?;
        }
        tx.commit()?;
        Ok(())
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
        let pending_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM mms_acquisitions WHERE state='pending'",
            [],
            |row| row.get(0),
        )?;
        let limit = limit.min(
            MAX_MMS_ACQUISITIONS
                .saturating_sub(usize::try_from(pending_count).map_err(|_| Error::Database)?),
        );
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
        clear_recheck_due(&tx, generation, scope)?;
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
        let was_full = remote_storage_full(&tx, generation, scope)?;
        tx.execute(
            "INSERT INTO mms_backfill_remote_capacity(source_generation,subscription_id,storage_full) VALUES(?,?,?) ON CONFLICT(source_generation,subscription_id) DO UPDATE SET storage_full=excluded.storage_full",
            params![generation, scope, !has_capacity],
        )?;
        if has_capacity {
            if was_full {
                clear_recheck_due(&tx, generation, scope)?;
            }
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
    previous_pause: Option<MmsBackfillPause>,
) -> Result<Option<MmsBackfillPause>, Error> {
    let recovery = recovery_capacity(conn)?;
    let local_full = pending_bytes >= MAX_PENDING_MMS_MEDIA_BYTES
        || crate::mms::pending_acquisition_capacity_reached(conn)?;
    let remote_full = remote_storage_full(conn, generation, scope)?;
    let pause = effective_pause(recovery, local_full, remote_full);
    if matches!(
        previous_pause,
        Some(MmsBackfillPause::LocalCapacity | MmsBackfillPause::AbsoluteRecoveryCapacity)
    ) && !matches!(
        pause,
        Some(MmsBackfillPause::LocalCapacity | MmsBackfillPause::AbsoluteRecoveryCapacity)
    ) {
        clear_recheck_due(conn, generation, scope)?;
    }
    conn.execute(
        "UPDATE mms_backfill_scopes SET pause_reason=? WHERE source_generation=? AND subscription_id=?",
        params![pause.map(MmsBackfillPause::code), generation, scope],
    )?;
    Ok(pause)
}

fn effective_pause(
    recovery: RecoveryCapacity,
    local_full: bool,
    remote_full: bool,
) -> Option<MmsBackfillPause> {
    if recovery == RecoveryCapacity::Absolute {
        Some(MmsBackfillPause::AbsoluteRecoveryCapacity)
    } else if local_full {
        Some(MmsBackfillPause::LocalCapacity)
    } else if recovery == RecoveryCapacity::Reserve {
        Some(MmsBackfillPause::RecoveryCapacity)
    } else if remote_full {
        Some(MmsBackfillPause::RemoteStorage)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecoveryCapacity {
    Available,
    Reserve,
    Absolute,
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

type RecheckRow = (String, i64, String, Direction, MmsAcquisitionState);

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
        "SELECT acquisition_id,local_order,provider_message_id,direction,state FROM mms_acquisitions WHERE source_generation=? AND {state_predicate} AND local_order>? ORDER BY local_order LIMIT ?"
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
    let next = rows.last().map(|row| row.1);
    Ok((rows, next))
}

fn clear_recheck_due(conn: &Connection, generation: &str, scope: &str) -> Result<(), Error> {
    conn.execute(
        "UPDATE mms_backfill_recheck_state SET next_due_at_ms=0,failure_count=0,last_reason=NULL
         WHERE acquisition_id IN (
           SELECT acquisition_id FROM mms_acquisitions
            WHERE source_generation=? AND (subscription_id=? OR ?='android-mms-provider') AND state='pending'
         )",
        params![generation, scope, scope],
    )?;
    Ok(())
}

/// Pending rows of one scope in numeric provider chronology, newest first. The provider hint reset
/// and the fair due selection both start from this bounded candidate set.
fn newest_pending_candidates(provider_wide: bool, limit_sql: &str) -> String {
    let scope_predicate = if provider_wide {
        ""
    } else {
        "AND subscription_id=?"
    };
    format!(
        "SELECT acquisition_id,local_order,provider_message_id,direction,state FROM mms_acquisitions
         WHERE source_generation=? {scope_predicate} AND state='pending'
         ORDER BY length(provider_message_id) DESC,provider_message_id DESC,local_order DESC
         LIMIT {limit_sql}"
    )
}

/// Binds the scope placeholders used by `newest_pending_candidates`, in order.
fn scope_values(generation: &str, scope: &str, provider_wide: bool) -> Vec<Value> {
    let mut values = vec![Value::Text(generation.into())];
    if !provider_wide {
        values.push(Value::Text(scope.into()));
    }
    values
}

fn reset_pending_recheck_due(
    conn: &Connection,
    generation: &str,
    scope: &str,
    limit: usize,
) -> Result<(), Error> {
    let provider_wide = scope == PROVIDER_WIDE_SCOPE;
    let selected = format!(
        "SELECT acquisition_id FROM ({})",
        newest_pending_candidates(provider_wide, "?")
    );
    let mut values = scope_values(generation, scope, provider_wide);
    values.push(Value::Integer(limit as i64));
    conn.execute(
        &format!(
            "UPDATE mms_backfill_recheck_state SET next_due_at_ms=0,failure_count=0,last_reason=NULL
             WHERE acquisition_id IN ({selected})"
        ),
        params_from_iter(values.iter()),
    )?;
    // The same newest rows also lose their attempt stamp, so a fresh provider change outranks older
    // never-ready rows in the fair due order.
    conn.execute(
        &format!(
            "UPDATE mms_backfill_recheck_attempts SET attempted_at_ms=0
             WHERE acquisition_id IN ({selected})"
        ),
        params_from_iter(values.iter()),
    )?;
    Ok(())
}

/// Due pending work, fairly ordered: least recently attempted first (never attempted is 0), then
/// newest numeric provider chronology, then newest local order. Only the capped pending partition
/// participates; include legacy over-cap rows too so they can drain instead of starving forever.
fn due_recheck_partition(
    conn: &Connection,
    generation: &str,
    scope: &str,
    now_ms: i64,
    limit: usize,
) -> Result<Vec<RecheckRow>, Error> {
    let provider_wide = scope == PROVIDER_WIDE_SCOPE;
    let query = format!(
        "SELECT c.acquisition_id,c.local_order,c.provider_message_id,c.direction,c.state FROM mms_acquisitions c
          LEFT JOIN mms_backfill_recheck_state r ON r.acquisition_id=c.acquisition_id
          LEFT JOIN mms_backfill_recheck_attempts t ON t.acquisition_id=c.acquisition_id
          WHERE c.source_generation=? {scope_predicate} AND c.state='pending' AND (COALESCE(r.next_due_at_ms,0)<=? OR ?<t.attempted_at_ms)
          ORDER BY COALESCE(t.attempted_at_ms,0) ASC,length(c.provider_message_id) DESC,c.provider_message_id DESC,c.local_order DESC
          LIMIT ?",
        scope_predicate = if provider_wide { "" } else { "AND c.subscription_id=?" },
    );
    let mut statement = conn.prepare(&query)?;
    let mut values = scope_values(generation, scope, provider_wide);
    values.extend([
        Value::Integer(now_ms),
        Value::Integer(now_ms),
        Value::Integer(limit as i64),
    ]);
    statement
        .query_map(params_from_iter(values.iter()), decode_recheck_row)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

const PROVIDER_WIDE_SCOPE: &str = "android-mms-provider";
const COMPLETED_SWEEP_ORDER: &str =
    "length(provider_message_id) DESC,provider_message_id DESC,local_order DESC";
// The keyset cursor is split into disjoint index ranges (same provider ID and earlier local order,
// same length and smaller provider ID, shorter length). Each range is one seek on the expression
// index, so already-swept history is never rescanned.
const SEGMENT_CURSOR_ROW: &str =
    "AND length(provider_message_id)=? AND provider_message_id=? AND local_order<?";
const SEGMENT_CURSOR_LENGTH: &str = "AND length(provider_message_id)=? AND provider_message_id<?";
const SEGMENT_SHORTER: &str = "AND length(provider_message_id)<?";
const SEGMENT_ALL: &str = "";
const ORDER_SAME_PROVIDER_ID: &str = "local_order DESC";
const ORDER_SAME_LENGTH: &str = "provider_message_id DESC,local_order DESC";

/// Numeric provider chronology plus local order: the durable completed-sweep position.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SweepPosition {
    length: i64,
    provider: String,
    order: i64,
}

impl SweepPosition {
    fn of(row: &RecheckRow) -> Self {
        Self {
            length: i64::try_from(row.2.chars().count()).unwrap_or(i64::MAX),
            provider: row.2.clone(),
            order: row.1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompletedSweep {
    started_at_ms: i64,
    completed_at_ms: Option<i64>,
    cursor: Option<SweepPosition>,
    /// End of the batch whose claims are still unresolved. It becomes the cursor only then.
    batch_end: Option<SweepPosition>,
}

impl CompletedSweep {
    fn restart(&mut self, now_ms: i64) {
        self.started_at_ms = now_ms;
        self.completed_at_ms = None;
        self.cursor = None;
    }

    fn advance_past_resolved_batch(&mut self) {
        if let Some(end) = self.batch_end.take() {
            self.cursor = Some(end);
        }
    }
}

fn sweep_position(
    length: Option<i64>,
    provider: Option<String>,
    order: Option<i64>,
) -> Option<SweepPosition> {
    Some(SweepPosition {
        length: length?,
        provider: provider?,
        order: order?,
    })
}

fn load_completed_sweep(
    conn: &Connection,
    generation: &str,
    scope: &str,
) -> Result<Option<CompletedSweep>, Error> {
    conn.query_row(
        "SELECT started_at_ms,completed_at_ms,cursor_len,cursor_provider,cursor_order,batch_len,batch_provider,batch_order
         FROM mms_backfill_completed_sweeps WHERE source_generation=? AND subscription_id=?",
        params![generation, scope],
        |row| {
            Ok(CompletedSweep {
                started_at_ms: row.get(0)?,
                completed_at_ms: row.get(1)?,
                cursor: sweep_position(row.get(2)?, row.get(3)?, row.get(4)?),
                batch_end: sweep_position(row.get(5)?, row.get(6)?, row.get(7)?),
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn store_completed_sweep(
    conn: &Connection,
    generation: &str,
    scope: &str,
    sweep: &CompletedSweep,
) -> Result<(), Error> {
    let cursor = sweep.cursor.as_ref();
    let batch = sweep.batch_end.as_ref();
    conn.execute(
        "INSERT INTO mms_backfill_completed_sweeps(source_generation,subscription_id,started_at_ms,completed_at_ms,cursor_len,cursor_provider,cursor_order,batch_len,batch_provider,batch_order)
         VALUES(?,?,?,?,?,?,?,?,?,?)
         ON CONFLICT(source_generation,subscription_id) DO UPDATE SET started_at_ms=excluded.started_at_ms,completed_at_ms=excluded.completed_at_ms,cursor_len=excluded.cursor_len,cursor_provider=excluded.cursor_provider,cursor_order=excluded.cursor_order,batch_len=excluded.batch_len,batch_provider=excluded.batch_provider,batch_order=excluded.batch_order",
        params![
            generation,
            scope,
            sweep.started_at_ms,
            sweep.completed_at_ms,
            cursor.map(|position| position.length),
            cursor.map(|position| position.provider.clone()),
            cursor.map(|position| position.order),
            batch.map(|position| position.length),
            batch.map(|position| position.provider.clone()),
            batch.map(|position| position.order),
        ],
    )?;
    Ok(())
}

/// Scopes whose sweep claimed this acquisition, read before the claim is removed.
fn claimed_sweep_scopes(
    conn: &Connection,
    acquisition_id: &str,
) -> Result<Vec<(String, String)>, Error> {
    let mut statement = conn.prepare(
        "SELECT source_generation,subscription_id FROM mms_backfill_completed_claims WHERE acquisition_id=?",
    )?;
    statement
        .query_map([acquisition_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Moves the cursor to a claimed batch's end once none of its claims remain.
fn fold_resolved_completed_batch(
    conn: &Connection,
    generation: &str,
    scope: &str,
) -> Result<(), Error> {
    conn.execute(
        "UPDATE mms_backfill_completed_sweeps
         SET cursor_len=batch_len,cursor_provider=batch_provider,cursor_order=batch_order,batch_len=NULL,batch_provider=NULL,batch_order=NULL
         WHERE source_generation=? AND subscription_id=? AND batch_provider IS NOT NULL
           AND NOT EXISTS(SELECT 1 FROM mms_backfill_completed_claims WHERE source_generation=? AND subscription_id=?)",
        params![generation, scope, generation, scope],
    )?;
    Ok(())
}

/// Returns unresolved claims first, so an interrupted or budget-limited batch is never discarded.
fn claimed_completed_rows(
    conn: &Connection,
    generation: &str,
    scope: &str,
    limit: usize,
) -> Result<Vec<RecheckRow>, Error> {
    conn.execute(
        "DELETE FROM mms_backfill_completed_claims WHERE source_generation=? AND subscription_id=?
           AND NOT EXISTS(SELECT 1 FROM mms_acquisitions a WHERE a.acquisition_id=mms_backfill_completed_claims.acquisition_id)",
        params![generation, scope],
    )?;
    let mut statement = conn.prepare(
        "SELECT a.acquisition_id,a.local_order,a.provider_message_id,a.direction,a.state
         FROM mms_backfill_completed_claims c JOIN mms_acquisitions a ON a.acquisition_id=c.acquisition_id
         WHERE c.source_generation=? AND c.subscription_id=?
         ORDER BY length(a.provider_message_id) DESC,a.provider_message_id DESC,a.local_order DESC LIMIT ?",
    )?;
    statement
        .query_map(params![generation, scope, limit], decode_recheck_row)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn claim_completed_rows(
    conn: &Connection,
    generation: &str,
    scope: &str,
    rows: &[RecheckRow],
) -> Result<(), Error> {
    let mut statement = conn.prepare(
        "INSERT OR IGNORE INTO mms_backfill_completed_claims(source_generation,subscription_id,acquisition_id) VALUES(?,?,?)",
    )?;
    for row in rows {
        statement.execute(params![generation, scope, row.0])?;
    }
    Ok(())
}

fn completed_segment_sql(provider_wide: bool, filter: &str, order: &str) -> String {
    let scope = if provider_wide {
        ""
    } else {
        " AND subscription_id=?"
    };
    format!(
        "SELECT acquisition_id,local_order,provider_message_id,direction,state FROM mms_acquisitions
         WHERE source_generation=?{scope} AND state='complete' {filter}
         ORDER BY {order} LIMIT ?"
    )
}

/// The exact-scope and provider-wide queries are separate statements so neither uses an OR that
/// would prevent a single ordered index walk.
/// Keyset statements for one batch: each is a disjoint ordered index range, and the caller appends
/// the remaining limit as its last parameter.
fn completed_segments(
    generation: &str,
    scope: &str,
    cursor: Option<&SweepPosition>,
) -> Vec<(String, Vec<Value>)> {
    let provider_wide = scope == PROVIDER_WIDE_SCOPE;
    let filters: Vec<(&str, &str, Vec<Value>)> = match cursor {
        None => vec![(SEGMENT_ALL, COMPLETED_SWEEP_ORDER, Vec::new())],
        Some(position) => vec![
            (
                SEGMENT_CURSOR_ROW,
                // Equal length and provider ID are constant here, so only the local order remains.
                ORDER_SAME_PROVIDER_ID,
                vec![
                    Value::Integer(position.length),
                    Value::Text(position.provider.clone()),
                    Value::Integer(position.order),
                ],
            ),
            (
                SEGMENT_CURSOR_LENGTH,
                ORDER_SAME_LENGTH,
                vec![
                    Value::Integer(position.length),
                    Value::Text(position.provider.clone()),
                ],
            ),
            (
                SEGMENT_SHORTER,
                COMPLETED_SWEEP_ORDER,
                vec![Value::Integer(position.length)],
            ),
        ],
    };
    filters
        .into_iter()
        .map(|(filter, order, filter_values)| {
            let mut values = vec![Value::Text(generation.into())];
            if !provider_wide {
                values.push(Value::Text(scope.into()));
            }
            values.extend(filter_values);
            (completed_segment_sql(provider_wide, filter, order), values)
        })
        .collect()
}

fn completed_batch_rows(
    conn: &Connection,
    generation: &str,
    scope: &str,
    cursor: Option<&SweepPosition>,
    limit: usize,
) -> Result<Vec<RecheckRow>, Error> {
    let mut rows = Vec::new();
    for (sql, mut values) in completed_segments(generation, scope, cursor) {
        let remaining = limit.saturating_sub(rows.len());
        if remaining == 0 {
            break;
        }
        values.push(Value::Integer(
            i64::try_from(remaining).map_err(|_| Error::Database)?,
        ));
        let mut statement = conn.prepare(&sql)?;
        rows.extend(
            statement
                .query_map(params_from_iter(values), decode_recheck_row)?
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    Ok(rows)
}

/// Completed maintenance is a persisted, newest-first sweep. Claimed rows from an unresolved batch
/// return first. A new batch claims rows without moving the cursor; the cursor moves only after its
/// outcomes are recorded. The 24-hour rest starts once a sweep finds no rows after its cursor.
/// Only an explicit refresh may interrupt a resting sweep, and it starts one bounded round.
fn completed_recheck_partition(
    conn: &Connection,
    generation: &str,
    scope: &str,
    now_ms: i64,
    explicit_refresh: bool,
    limit: usize,
) -> Result<Vec<RecheckRow>, Error> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let claimed = claimed_completed_rows(conn, generation, scope, limit)?;
    if !claimed.is_empty() {
        return Ok(claimed);
    }
    let mut sweep = load_completed_sweep(conn, generation, scope)?.unwrap_or(CompletedSweep {
        started_at_ms: now_ms,
        completed_at_ms: None,
        cursor: None,
        batch_end: None,
    });
    if sweep.started_at_ms > now_ms || sweep.completed_at_ms.is_some_and(|done| done > now_ms) {
        // A rolled-back clock must not freeze maintenance behind a future rest or start time.
        sweep.restart(now_ms);
    }
    sweep.advance_past_resolved_batch();
    let resting = sweep
        .completed_at_ms
        .is_some_and(|done| done.saturating_add(MMS_COMPLETED_RECHECK_REST_MS) > now_ms);
    let rows = if resting && !explicit_refresh {
        Vec::new()
    } else {
        if sweep.completed_at_ms.is_some() {
            sweep.restart(now_ms);
        }
        let rows = completed_batch_rows(conn, generation, scope, sweep.cursor.as_ref(), limit)?;
        match rows.last() {
            Some(last) => {
                claim_completed_rows(conn, generation, scope, &rows)?;
                sweep.batch_end = Some(SweepPosition::of(last));
            }
            None => {
                sweep.completed_at_ms = Some(now_ms);
                sweep.cursor = None;
            }
        }
        rows
    };
    store_completed_sweep(conn, generation, scope, &sweep)?;
    Ok(rows)
}

fn decode_recheck_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecheckRow> {
    let direction = match row.get::<_, String>(3)?.as_str() {
        "incoming" => Direction::Incoming,
        "outgoing" => Direction::Outgoing,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let state = MmsAcquisitionState::parse(&row.get::<_, String>(4)?)
        .ok_or(rusqlite::Error::InvalidQuery)?;
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, direction, state))
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

fn recovery_capacity(conn: &Connection) -> Result<RecoveryCapacity, Error> {
    let records: i64 = conn.query_row(
        "SELECT COALESCE(MAX(local_order),0) FROM messages",
        [],
        |row| row.get(0),
    )?;
    let pages = pragma_integer(conn, "PRAGMA page_count")?;
    let page_size = pragma_integer(conn, "PRAGMA page_size")?;
    let records: u64 = records.try_into().map_err(|_| Error::Database)?;
    let transport_records = known_transport_record_count(conn)?;
    let bytes: u64 = pages
        .saturating_mul(page_size)
        .try_into()
        .map_err(|_| Error::Database)?;
    let capacity = classify_recovery_capacity(records, transport_records, bytes);
    if capacity == RecoveryCapacity::Reserve && get_meta(conn, SNAPSHOT_HIGH_WATER_META)?.is_none()
    {
        // Older/unreachable servers have not supplied a retained-record inventory. Preserve the
        // prior admission reserve until the existing authenticated probe establishes that budget.
        return Ok(RecoveryCapacity::Absolute);
    }
    Ok(capacity)
}

fn classify_recovery_capacity(
    records: u64,
    transport_records: u64,
    bytes: u64,
) -> RecoveryCapacity {
    // Database pages include indexes, old outbox copies and superseded journal records. They are
    // an import-reserve warning, not proof that the retained server snapshot hit its byte limit.
    if records.max(transport_records)
        >= MAX_SNAPSHOT_RECORDS.saturating_sub(LIVE_SNAPSHOT_RECORD_HEADROOM)
    {
        RecoveryCapacity::Absolute
    } else if records >= MAX_SNAPSHOT_RECORDS - RECOVERY_RECORD_RESERVE
        || bytes >= MAX_SNAPSHOT_BYTES - RECOVERY_BYTE_RESERVE
    {
        RecoveryCapacity::Reserve
    } else {
        RecoveryCapacity::Available
    }
}

/// Only a trusted server cut describes retained records. Add indexed post-cut journal work and
/// unpublished local work; never count lifetime notifications that the server already compacted.
fn known_transport_record_count(conn: &Connection) -> Result<u64, Error> {
    let Some(high) = get_meta(conn, SNAPSHOT_HIGH_WATER_META)? else {
        return Ok(0);
    };
    let high: i64 = high.parse().map_err(|_| Error::Database)?;
    let count: u64 = get_meta(conn, SNAPSHOT_COUNT_META)?
        .ok_or(Error::Database)?
        .parse()
        .map_err(|_| Error::Database)?;
    let tail: i64 = conn.query_row(
        "SELECT COUNT(*) FROM (SELECT 1 FROM journal WHERE cursor>? LIMIT ?)",
        params![high, MAX_SNAPSHOT_RECORDS],
        |r| r.get(0),
    )?;
    let pending:i64=conn.query_row("SELECT COUNT(*) FROM (SELECT 1 FROM outbox WHERE state='unsealed' UNION ALL SELECT 1 FROM outbox WHERE state='queued' LIMIT ?)",[MAX_SNAPSHOT_RECORDS],|r|r.get(0))?;
    let acknowledgements: u64 = get_meta(conn, SNAPSHOT_ACKS_META)?
        .unwrap_or_else(|| "0".into())
        .parse()
        .map_err(|_| Error::Database)?;
    Ok(count
        .saturating_add(tail as u64)
        .saturating_add(pending as u64)
        .saturating_add(acknowledgements))
}

pub(crate) fn record_snapshot_capacity(
    conn: &Connection,
    high: u64,
    count: u64,
) -> Result<(), Error> {
    let old = get_meta(conn, SNAPSHOT_HIGH_WATER_META)?.and_then(|v| v.parse::<u64>().ok());
    if old.is_some_and(|old| old > high) {
        return Ok(());
    }
    for (key, value) in [
        (SNAPSHOT_HIGH_WATER_META, high.to_string()),
        (SNAPSHOT_COUNT_META, count.to_string()),
        (SNAPSHOT_ACKS_META, "0".into()),
    ] {
        conn.execute("INSERT INTO metadata(k,v) VALUES(?,?) ON CONFLICT(k) DO UPDATE SET v=excluded.v WHERE metadata.v!=excluded.v",params![key,value])?;
    }
    Ok(())
}

pub(crate) fn record_recovery_ack(conn: &Connection) -> Result<(), Error> {
    let count: u64 = get_meta(conn, SNAPSHOT_ACKS_META)?
        .unwrap_or_else(|| "0".into())
        .parse()
        .map_err(|_| Error::Database)?;
    set_meta(
        conn,
        SNAPSHOT_ACKS_META,
        &count.saturating_add(1).to_string(),
    )
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
    use peppy_crypto::{create_vault_check_header, derive_root_key};
    use tempfile::TempDir;

    fn outcome_client() -> (TempDir, Client) {
        let directory = TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("correct horse battery staple", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let client = Client::open(
            ClientConfig {
                database_path: directory.path().join("outcome.db"),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[9; 32]).unwrap(),
        )
        .unwrap();
        client
            .unlock(&profile, &header, "correct horse battery staple")
            .unwrap();
        (directory, client)
    }

    #[test]
    fn local_database_history_and_indexes_are_not_an_absolute_snapshot_byte_measure() {
        assert_eq!(
            classify_recovery_capacity(100, 100, MAX_SNAPSHOT_BYTES + 4096),
            RecoveryCapacity::Reserve
        );
        assert_eq!(
            classify_recovery_capacity(100, MAX_SNAPSHOT_RECORDS - 1, 4096),
            RecoveryCapacity::Absolute
        );
    }
    use rusqlite::StatementStatus;

    #[test]
    fn history_reserve_never_overrides_a_real_local_capacity_block() {
        assert_eq!(
            effective_pause(RecoveryCapacity::Reserve, true, false),
            Some(MmsBackfillPause::LocalCapacity)
        );
        assert_eq!(
            effective_pause(RecoveryCapacity::Reserve, false, false),
            Some(MmsBackfillPause::RecoveryCapacity)
        );
        assert_eq!(
            effective_pause(RecoveryCapacity::Absolute, true, false),
            Some(MmsBackfillPause::AbsoluteRecoveryCapacity)
        );
    }

    fn capacity_connection() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE messages(local_order INTEGER NOT NULL);
             CREATE TABLE metadata(k TEXT PRIMARY KEY,v TEXT NOT NULL);
             CREATE TABLE journal(cursor INTEGER PRIMARY KEY,envelope_id TEXT);
             CREATE TABLE outbox(seq INTEGER PRIMARY KEY,envelope_id TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'acknowledged');
             CREATE TABLE mms_acquisitions(state TEXT NOT NULL, acquisition_id TEXT, source_generation TEXT, subscription_id TEXT);
             CREATE TABLE mms_backfill_recheck_state(acquisition_id TEXT PRIMARY KEY,next_due_at_ms INTEGER,failure_count INTEGER,last_reason TEXT);
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
        record_snapshot_capacity(&conn, 0, 0).unwrap();
        conn
    }

    #[test]
    fn missing_snapshot_probe_keeps_the_old_live_reserve_until_a_cut_arrives() {
        let conn = capacity_connection();
        conn.execute("DELETE FROM metadata WHERE k=?", [SNAPSHOT_HIGH_WATER_META])
            .unwrap();
        conn.execute(
            "INSERT INTO messages(local_order) VALUES(?)",
            [MAX_SNAPSHOT_RECORDS - RECOVERY_RECORD_RESERVE],
        )
        .unwrap();
        assert_eq!(
            recovery_capacity(&conn).unwrap(),
            RecoveryCapacity::Absolute
        );
        record_snapshot_capacity(&conn, 90000, 90000).unwrap();
        assert_eq!(recovery_capacity(&conn).unwrap(), RecoveryCapacity::Reserve);
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
            refresh_local_pause(
                &conn,
                "generation",
                "scope",
                0,
                Some(MmsBackfillPause::LocalCapacity)
            )
            .unwrap(),
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
                0,
                Some(MmsBackfillPause::RemoteStorage),
            )
            .unwrap(),
            Some(MmsBackfillPause::RecoveryCapacity)
        );
    }

    #[test]
    fn absolute_recovery_capacity_is_distinct_from_history_reserve() {
        let conn = capacity_connection();
        conn.execute(
            "INSERT INTO messages(local_order) VALUES(?)",
            [MAX_SNAPSHOT_RECORDS],
        )
        .unwrap();

        assert_eq!(
            refresh_local_pause(&conn, "generation", "scope", 0, None).unwrap(),
            Some(MmsBackfillPause::AbsoluteRecoveryCapacity)
        );
    }

    #[test]
    fn known_transport_records_close_live_headroom_before_message_limit() {
        let conn = capacity_connection();
        conn.execute("INSERT INTO messages(local_order) VALUES(?)", [100])
            .unwrap();
        record_snapshot_capacity(&conn, MAX_SNAPSHOT_RECORDS, MAX_SNAPSHOT_RECORDS - 10).unwrap();
        assert_eq!(
            recovery_capacity(&conn).unwrap(),
            RecoveryCapacity::Absolute
        );

        let lower = capacity_connection();
        lower
            .execute(
                "INSERT INTO messages(local_order) VALUES(?)",
                [MAX_SNAPSHOT_RECORDS - RECOVERY_RECORD_RESERVE],
            )
            .unwrap();
        assert_eq!(
            recovery_capacity(&lower).unwrap(),
            RecoveryCapacity::Reserve
        );
    }

    #[test]
    fn superseded_local_notification_history_does_not_exhaust_live_snapshot_capacity() {
        let conn = capacity_connection();
        conn.execute("INSERT INTO messages(local_order) VALUES(100)", [])
            .unwrap();
        conn.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<110000) INSERT INTO journal(cursor,envelope_id) SELECT i,'notification-'||i FROM n;").unwrap();
        record_snapshot_capacity(&conn, 110000, 100).unwrap();
        assert_eq!(known_transport_record_count(&conn).unwrap(), 100);
        assert_eq!(
            recovery_capacity(&conn).unwrap(),
            RecoveryCapacity::Available
        );
        conn.execute(
            "INSERT INTO journal(cursor,envelope_id) VALUES(110001,'new-event')",
            [],
        )
        .unwrap();
        record_recovery_ack(&conn).unwrap();
        assert_eq!(known_transport_record_count(&conn).unwrap(), 102);
        record_snapshot_capacity(&conn, 110002, 101).unwrap();
        assert_eq!(known_transport_record_count(&conn).unwrap(), 101);
    }

    #[test]
    fn due_pending_rechecks_wait_for_the_durable_deadline() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mms_acquisitions(
               acquisition_id TEXT PRIMARY KEY, source_generation TEXT NOT NULL,
               subscription_id TEXT NOT NULL, provider_message_id TEXT NOT NULL,
               direction TEXT NOT NULL, state TEXT NOT NULL, local_order INTEGER NOT NULL
             );
              CREATE TABLE mms_backfill_recheck_state(
                acquisition_id TEXT PRIMARY KEY, next_due_at_ms INTEGER NOT NULL,
                failure_count INTEGER NOT NULL, last_reason TEXT, completed_recheck_at_ms INTEGER NOT NULL
              );
              CREATE TABLE mms_backfill_recheck_attempts(
                acquisition_id TEXT PRIMARY KEY, attempted_at_ms INTEGER NOT NULL
              );
              INSERT INTO mms_acquisitions VALUES('due','generation','scope','7','incoming','pending',1);
             INSERT INTO mms_backfill_recheck_state VALUES('due',100,1,'part_unavailable',0);",
        )
        .unwrap();

        assert!(
            due_recheck_partition(&conn, "generation", "scope", 99, 32)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            due_recheck_partition(&conn, "generation", "scope", 100, 32)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn verified_completed_outcome_removes_backoff_and_resolves_the_claim() {
        let (_directory, client) = outcome_client();
        let acquisition = client
            .begin_mms_acquisition(MmsAcquisitionInput {
                source: MmsSource {
                    source_generation: "generation".into(),
                    subscription_id: "scope".into(),
                    provider_message_id: "1".into(),
                    provider_thread_id: None,
                },
                direction: Direction::Incoming,
                sender_address: Some("+15555550100".into()),
                recipients: Vec::new(),
                subject: None,
                body: "complete".into(),
                imported: false,
                observed_at_ms: 1,
                transaction_id: None,
            })
            .unwrap();
        client
            .complete_mms_acquisition(&acquisition.acquisition_id)
            .unwrap();
        let conditions = MmsBackfillRecheckConditions {
            charging: true,
            ..Default::default()
        };
        client
            .record_mms_backfill_recheck_outcome(
                &acquisition.acquisition_id,
                1,
                MmsBackfillRecheckOutcome::TransientFailure,
                conditions,
            )
            .unwrap();
        assert_eq!(
            client
                .mms_backfill_due_rechecks("generation", "scope", 2, conditions)
                .unwrap()
                .requests
                .len(),
            1
        );
        client
            .record_mms_backfill_recheck_outcome(
                &acquisition.acquisition_id,
                3,
                MmsBackfillRecheckOutcome::Verified,
                conditions,
            )
            .unwrap();

        assert!(
            client
                .mms_backfill_due_rechecks("generation", "scope", 4, conditions)
                .unwrap()
                .requests
                .is_empty(),
            "resolving the claim advances the completed sweep"
        );
        assert_eq!(
            client
                .mms_backfill_due_rechecks(
                    "generation",
                    "scope",
                    5,
                    MmsBackfillRecheckConditions {
                        explicit_refresh: true,
                        ..conditions
                    },
                )
                .unwrap()
                .requests
                .len(),
            1
        );
        client
            .record_mms_backfill_recheck_outcome(
                &acquisition.acquisition_id,
                6,
                MmsBackfillRecheckOutcome::Verified,
                conditions,
            )
            .unwrap();

        let store = client.lock().unwrap();
        let (backoff_rows, claim_rows): (i64, i64) = store
            .conn
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM mms_backfill_recheck_state WHERE acquisition_id=?),
                    (SELECT COUNT(*) FROM mms_backfill_completed_claims WHERE acquisition_id=?)",
                [&acquisition.acquisition_id, &acquisition.acquisition_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((backoff_rows, claim_rows), (0, 0));
    }

    fn sweep_connection() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mms_acquisitions(
               acquisition_id TEXT PRIMARY KEY, source_generation TEXT NOT NULL,
               subscription_id TEXT NOT NULL, provider_message_id TEXT NOT NULL,
               direction TEXT NOT NULL, state TEXT NOT NULL, local_order INTEGER NOT NULL UNIQUE
             );",
        )
        .unwrap();
        initialize(&conn).unwrap();
        conn
    }

    fn insert_complete(conn: &Connection, id: &str, provider: &str, direction: &str) {
        conn.execute(
            "INSERT INTO mms_acquisitions(acquisition_id,source_generation,subscription_id,provider_message_id,direction,state,local_order)
             VALUES(?,'generation','scope',?,?,'complete',(SELECT COALESCE(MAX(local_order),0)+1 FROM mms_acquisitions))",
            params![id, provider, direction],
        )
        .unwrap();
    }

    /// Stands in for the host outcome path: resolves claims, then folds the finished batch.
    fn resolve_batch(conn: &Connection, ids: &[&str]) {
        for id in ids {
            conn.execute(
                "DELETE FROM mms_backfill_completed_claims WHERE acquisition_id=?",
                [id],
            )
            .unwrap();
        }
        fold_resolved_completed_batch(conn, "generation", "scope").unwrap();
    }

    fn ids(rows: &[RecheckRow]) -> Vec<&str> {
        rows.iter().map(|row| row.0.as_str()).collect()
    }

    #[test]
    fn completed_sweep_walks_numeric_chronology_with_equal_provider_id_tie() {
        let conn = sweep_connection();
        insert_complete(&conn, "a", "2", "incoming");
        insert_complete(&conn, "b", "10", "incoming");
        insert_complete(&conn, "c", "10", "outgoing");
        insert_complete(&conn, "d", "9", "incoming");

        let first = completed_recheck_partition(&conn, "generation", "scope", 0, false, 2).unwrap();
        assert_eq!(
            ids(&first),
            ["c", "b"],
            "equal provider ID breaks ties by newest local order"
        );
        resolve_batch(&conn, &["c", "b"]);

        let second =
            completed_recheck_partition(&conn, "generation", "scope", 0, false, 2).unwrap();
        assert_eq!(ids(&second), ["d", "a"]);
        resolve_batch(&conn, &["d", "a"]);

        assert!(
            completed_recheck_partition(&conn, "generation", "scope", 0, false, 2)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn unresolved_claims_return_first_and_cursor_waits_for_the_batch() {
        let conn = sweep_connection();
        insert_complete(&conn, "a", "1", "incoming");
        insert_complete(&conn, "b", "2", "incoming");
        insert_complete(&conn, "c", "3", "incoming");

        let batch = completed_recheck_partition(&conn, "generation", "scope", 0, false, 2).unwrap();
        assert_eq!(ids(&batch), ["c", "b"]);
        // One outcome resolves only its own claim; the cursor must not skip the other row.
        conn.execute(
            "DELETE FROM mms_backfill_completed_claims WHERE acquisition_id='c'",
            [],
        )
        .unwrap();
        let remaining =
            completed_recheck_partition(&conn, "generation", "scope", 0, false, 2).unwrap();
        assert_eq!(
            ids(&remaining),
            ["b"],
            "unattempted claims are never discarded"
        );
        resolve_batch(&conn, &["b"]);
        assert_eq!(
            ids(&completed_recheck_partition(&conn, "generation", "scope", 0, false, 2).unwrap()),
            ["a"]
        );
    }

    #[test]
    fn completed_keyset_segments_are_index_ordered_and_do_not_rescan_history() {
        let conn = sweep_connection();
        let tx = conn.unchecked_transaction().unwrap();
        for id in 1..=90_000_i64 {
            insert_complete(&tx, &id.to_string(), &id.to_string(), "incoming");
        }
        tx.commit().unwrap();
        let (deep_provider, deep_order): (String, i64) = conn
            .query_row(
                "SELECT provider_message_id,local_order FROM mms_acquisitions
                 WHERE source_generation='generation' AND subscription_id='scope' AND state='complete'
                 ORDER BY length(provider_message_id) DESC,provider_message_id DESC,local_order DESC
                 LIMIT 1 OFFSET 80000",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let deep = SweepPosition {
            length: i64::try_from(deep_provider.len()).unwrap(),
            provider: deep_provider,
            order: deep_order,
        };

        for (scope, cursor) in ["scope", PROVIDER_WIDE_SCOPE]
            .into_iter()
            .flat_map(|scope| [(scope, None), (scope, Some(&deep))])
        {
            for (sql, mut values) in completed_segments("generation", scope, cursor) {
                values.push(Value::Integer(MAX_MMS_BACKFILL_RECHECK as i64));
                let mut plan = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
                let details = plan
                    .query_map(params_from_iter(values.iter()), |row| {
                        row.get::<_, String>(3)
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                assert!(
                    details.iter().all(|detail| !detail.contains("TEMP B-TREE")),
                    "{details:?}"
                );
                assert!(
                    details
                        .iter()
                        .any(|detail| detail.contains("mms_acquisitions_completed_sweep")),
                    "{details:?}"
                );

                let mut statement = conn.prepare(&sql).unwrap();
                let rows = statement
                    .query_map(params_from_iter(values.iter()), decode_recheck_row)
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                assert!(rows.len() <= MAX_MMS_BACKFILL_RECHECK);
                let steps = statement.get_status(StatementStatus::VmStep);
                // Measured ~335 steps at the top and ~340 at offset 80k; a rescan costs far more.
                assert!(steps < 2_000, "steps={steps} cursor={cursor:?}");
            }
        }
    }
}
