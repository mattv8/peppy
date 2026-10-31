use crate::Error;
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) const HISTORY_IMPORT_GATE_TIMEOUT_MS: i64 = 10 * 60 * 1000;
/// History pages may overshoot this by at most one already-planned host page.
pub(crate) const MAX_PENDING_HISTORY_OUTBOX_EVENTS: i64 = 500;

/// SQL predicate for an outbox row eligible for eventual publication.
/// Callers query `outbox` directly, so the predicate deliberately uses that table name.
pub(crate) fn outbox_upload_eligible_predicate() -> &'static str {
    "outbox.envelope_id NOT IN (SELECT envelope_id FROM outbox_conflicts) \
     AND outbox.envelope_id NOT IN (SELECT envelope_id FROM contact_photo_registrations WHERE acknowledged=0) \
     AND NOT EXISTS(SELECT 1 FROM outbox_attachments oa JOIN attachments a ON a.attachment_id=oa.attachment_id WHERE oa.envelope_id=outbox.envelope_id AND NOT (a.state='uploaded' OR (a.state='available' AND a.remote_object_id IS NOT NULL))) \
     AND (outbox.compaction IS NULL OR EXISTS(SELECT 1 FROM metadata WHERE k='server_compaction_supported' AND v='1'))"
}

/// Returns whether a descending cursor may replace the current cursor. This is shared by SMS and
/// MMS so their durable-prefix boundary checks cannot drift.
pub(crate) fn advances_descending_cursor(
    current: Option<&str>,
    floor: Option<&str>,
    upper: &str,
    candidate: &str,
) -> Result<bool, Error> {
    let candidate =
        canonical_provider_id(candidate).ok_or(Error::InvalidRequest("history checkpoint"))?;
    if candidate == 0
        || candidate > stored_provider_id(upper)?
        || floor
            .map(stored_provider_id)
            .transpose()?
            .is_some_and(|value| candidate <= value)
    {
        return Ok(false);
    }
    Ok(match current.map(stored_provider_id).transpose()? {
        Some(value) => candidate < value,
        None => true,
    })
}

/// Parses a canonical provider ID. Provider IDs are stored as text and retain the full `u64`
/// range; signed SQLite local-order columns use the separate helpers below.
pub(crate) fn canonical_provider_id(value: &str) -> Option<u64> {
    let parsed = value.parse::<u64>().ok()?;
    (parsed.to_string() == value).then_some(parsed)
}

pub(crate) fn normalized_provider_id(value: &str) -> Option<String> {
    value.parse::<u64>().ok().map(|parsed| parsed.to_string())
}

pub(crate) fn stored_provider_id(value: &str) -> Result<u64, Error> {
    value.parse::<u64>().map_err(|_| Error::Database)
}

pub(crate) fn normalized_local_order(value: &str) -> Option<String> {
    value.parse::<i64>().ok().map(|parsed| parsed.to_string())
}

pub(crate) fn stored_local_order(value: &str) -> Result<i64, Error> {
    value.parse::<i64>().map_err(|_| Error::Database)
}

pub(crate) fn looks_numeric(value: &str) -> bool {
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

pub(crate) fn history_import_gate_open(conn: &Connection, now_ms: i64) -> Result<bool, Error> {
    let state: Option<(Option<i64>, Option<i64>)> = conn
        .query_row(
            "SELECT opened_at_ms,first_closed_at_ms FROM history_import_gate WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((opened, first_closed)) = state else {
        conn.execute(
            "INSERT INTO history_import_gate(id,opened_at_ms,first_closed_at_ms,reason) VALUES(1,NULL,?,NULL)",
            params![now_ms],
        )?;
        return Ok(false);
    };
    if opened.is_some() {
        return Ok(true);
    }
    // Wall clocks can move backward after the first plan. Resetting a future timestamp makes the
    // timeout a real ten-minute bound from this observation instead of waiting for that clock to
    // catch up.
    let first_closed = first_closed.filter(|at| *at <= now_ms).unwrap_or(now_ms);
    if now_ms.saturating_sub(first_closed) >= HISTORY_IMPORT_GATE_TIMEOUT_MS {
        conn.execute(
            "UPDATE history_import_gate SET opened_at_ms=?,reason='timeout' WHERE id=1 AND opened_at_ms IS NULL",
            params![now_ms],
        )?;
        return Ok(true);
    }
    conn.execute(
        "UPDATE history_import_gate SET first_closed_at_ms=? WHERE id=1 AND (first_closed_at_ms IS NULL OR first_closed_at_ms>?)",
        params![first_closed, now_ms],
    )?;
    Ok(false)
}

/// Returns whether imported history must yield to locally outstanding event publication. This is
/// intentionally a plan-time decision: a page admitted immediately before the threshold may
/// finish, but no durable history pause is recorded and live/recheck work remains available.
fn outbox_backpressure_query() -> String {
    format!(
        "SELECT COUNT(*) FROM (SELECT 1 FROM outbox WHERE purpose='event' \
         AND state IN ('unsealed','queued') AND {} LIMIT ?)",
        outbox_upload_eligible_predicate()
    )
}

pub(crate) fn outbox_backpressures_history(conn: &Connection) -> Result<bool, Error> {
    let query = outbox_backpressure_query();
    let count: i64 = conn.query_row(&query, params![MAX_PENDING_HISTORY_OUTBOX_EVENTS], |row| {
        row.get(0)
    })?;
    Ok(count >= MAX_PENDING_HISTORY_OUTBOX_EVENTS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Client, ClientConfig, DatabaseKey, DeviceId, VaultId};
    use rusqlite::{Connection, StatementStatus};

    fn cache_misses(conn: &Connection, reset: bool) -> i32 {
        let mut current = 0;
        let mut high_water = 0;
        let status = unsafe {
            rusqlite::ffi::sqlite3_db_status(
                conn.handle(),
                rusqlite::ffi::SQLITE_DBSTATUS_CACHE_MISS,
                &mut current,
                &mut high_water,
                i32::from(reset),
            )
        };
        assert_eq!(status, rusqlite::ffi::SQLITE_OK);
        current
    }

    #[test]
    fn history_backpressure_query_uses_covering_indexes_for_large_ineligible_backlog() {
        let directory = tempfile::TempDir::new().unwrap();
        let config = ClientConfig {
            database_path: directory.path().join("history-backpressure.db"),
            vault_id: VaultId::new(),
            device_id: DeviceId::new(),
        };
        drop(Client::open(config.clone(), DatabaseKey::new(&[9; 32]).unwrap()).unwrap());

        let conn = Connection::open(&config.database_path).unwrap();
        conn.execute_batch(
            "PRAGMA key = \"x'0909090909090909090909090909090909090909090909090909090909090909'\"; \
             PRAGMA cache_size = -64; \
             WITH RECURSIVE series(value) AS (SELECT 1 UNION ALL SELECT value + 1 FROM series WHERE value < 5000) \
             INSERT INTO outbox(seq,envelope_id,purpose,state,plain,wire) \
             SELECT value, 'pending-' || value, 'event', 'queued', zeroblob(16384), zeroblob(32768) FROM series; \
             WITH RECURSIVE series(value) AS (SELECT 1 UNION ALL SELECT value + 1 FROM series WHERE value < 4500) \
             INSERT INTO outbox_conflicts(envelope_id,cursor) \
             SELECT 'pending-' || value, value FROM series; \
             WITH RECURSIVE series(value) AS (SELECT 4501 UNION ALL SELECT value + 1 FROM series WHERE value < 4700) \
             INSERT INTO contact_photo_registrations(envelope_id,attachment_id,remote_object_id,producer_sequence,acknowledged) \
             SELECT 'pending-' || value, 'attachment-' || value, 'remote-' || value, value, 1 FROM series; \
             WITH RECURSIVE series(value) AS (SELECT 5001 UNION ALL SELECT value + 1 FROM series WHERE value < 15000) \
             INSERT INTO outbox(seq,envelope_id,purpose,state,plain,wire) \
             SELECT value, 'acknowledged-' || value, 'event', 'acknowledged', zeroblob(16384), randomblob(32768) FROM series; \
             PRAGMA shrink_memory;",
        )
        .unwrap();

        let acknowledged_history_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM outbox WHERE purpose='event' AND state='acknowledged' AND length(wire)=32768",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            acknowledged_history_rows >= 10_000,
            "fixture must retain acknowledged event history with 32KB wire blobs: {acknowledged_history_rows}"
        );

        let query = outbox_backpressure_query();
        let plan = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
            .unwrap()
            .query_map([MAX_PENDING_HISTORY_OUTBOX_EVENTS], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        assert!(
            plan.contains("USING COVERING INDEX outbox_pending_events_history"),
            "{plan}"
        );
        assert!(
            plan.contains("USING COVERING INDEX contact_photo_registrations_unacknowledged"),
            "{plan}"
        );

        cache_misses(&conn, true);
        let mut statement = conn.prepare(&query).unwrap();
        let count: i64 = statement
            .query_row([MAX_PENDING_HISTORY_OUTBOX_EVENTS], |row| row.get(0))
            .unwrap();
        let steps = statement.get_status(StatementStatus::VmStep);
        assert_eq!(count, MAX_PENDING_HISTORY_OUTBOX_EVENTS);
        assert!(steps <= 40_000, "history backpressure VM steps: {steps}");
        assert!(
            cache_misses(&conn, false) <= 128,
            "history backpressure cache misses exceeded covering-index budget"
        );
        assert!(outbox_backpressures_history(&conn).unwrap());
    }

    #[test]
    fn descending_cursor_inputs_must_be_canonical_while_zero_stays_a_caller_rule() {
        assert!(advances_descending_cursor(None, None, "10", "5").unwrap());
        for bad in ["+5", "05", "-5", "", "18446744073709551616"] {
            assert!(
                advances_descending_cursor(None, None, "10", bad).is_err(),
                "candidate {bad:?} accepted"
            );
        }
        assert!(advances_descending_cursor(None, None, "+10", "5").unwrap());
        assert!(advances_descending_cursor(Some("07"), None, "10", "5").unwrap());
        assert!(
            advances_descending_cursor(
                None,
                None,
                u64::MAX.to_string().as_str(),
                "9223372036854775808"
            )
            .unwrap()
        );
        assert!(!advances_descending_cursor(None, None, "10", "0").unwrap());
    }
}
