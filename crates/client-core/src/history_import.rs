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
     AND NOT EXISTS(SELECT 1 FROM outbox_attachments oa JOIN attachments a ON a.attachment_id=oa.attachment_id WHERE oa.envelope_id=outbox.envelope_id AND a.state!='uploaded') \
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
    let candidate = id(candidate)?;
    if candidate == 0
        || candidate > id(upper)?
        || floor
            .map(id)
            .transpose()?
            .is_some_and(|value| candidate <= value)
    {
        return Ok(false);
    }
    Ok(match current.map(id).transpose()? {
        Some(value) => candidate < value,
        None => true,
    })
}

fn id(value: &str) -> Result<u64, Error> {
    value
        .parse()
        .map_err(|_| Error::InvalidRequest("history checkpoint"))
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
pub(crate) fn outbox_backpressures_history(conn: &Connection) -> Result<bool, Error> {
    let query = format!(
        "SELECT COUNT(*) FROM (SELECT 1 FROM outbox WHERE purpose='event' \
         AND state IN ('unsealed','queued') AND {} LIMIT ?)",
        outbox_upload_eligible_predicate()
    );
    let count: i64 = conn.query_row(&query, params![MAX_PENDING_HISTORY_OUTBOX_EVENTS], |row| {
        row.get(0)
    })?;
    Ok(count >= MAX_PENDING_HISTORY_OUTBOX_EVENTS)
}
