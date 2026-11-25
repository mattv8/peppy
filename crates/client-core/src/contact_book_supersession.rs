//! Automatic retirement of a successor phone's own superseded contact book.
//!
//! A re-enrolled phone inherits its predecessor's book and takes it over with a full scan. The
//! book it started on its own before the succession is then superseded. Retirement removes all
//! of that book's contacts (the tombstones keep the usual restore data on other devices), closes
//! its open work and publishes it retired with `superseded_by`, in one transaction. Unlike an
//! ordinary scan removal it needs no approval: the user accepted that for this case.
use crate::contacts;
use crate::device_succession;
use crate::{Ctx, Error, MAX_SEAL_BATCH, initial_replay, seal_pending};
use rusqlite::{Connection, TransactionBehavior};
use serde_json::{Value, json};

/// Reason on edit and restore requests closed because their book was replaced.
pub(crate) const BOOK_REPLACED: &str = "book_replaced";

/// Retires every superseded book of this device. Requires unlocked keys. Returns
/// `{"schema_version":1,"retired":[{"book_id","replaced_by","contacts_removed"}],"reopened":[book_id]}`;
/// both lists are empty when nothing is eligible. `reopened` names inherited books whose takeover
/// is open again so their contact identities get reconciled (`contact_takeover`): hosts run this
/// at the start of every contacts pass, before they select the book.
pub(crate) fn retire(conn: &mut Connection, ctx: &Ctx<'_>) -> Result<String, Error> {
    contacts::require_unlocked(ctx)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut retired = Vec::new();
    if let Some((replaced_by, superseded)) = eligible(&tx, ctx)? {
        for book_id in superseded {
            let removed = retire_book(&tx, ctx, &book_id, &replaced_by)?;
            retired.push(json!({
                "book_id": book_id,
                "replaced_by": replaced_by,
                "contacts_removed": removed,
            }));
        }
    }
    // Requests can still reach a book after it was replaced; they are closed the same way. Applying
    // a peer record has no context to publish a result, so this pass does it.
    for book_id in replaced_books(&tx, ctx)? {
        close_requests(&tx, ctx, &book_id)?;
        crate::contact_sync_requests::end_for_retired_book(&tx, ctx, &book_id)?;
    }
    loop {
        if seal_pending(&tx, ctx, MAX_SEAL_BATCH)? < MAX_SEAL_BATCH {
            break;
        }
    }
    let reopened = crate::contact_takeover::reopen_unreconciled(&tx, ctx)?;
    tx.commit()?;
    Ok(json!({"schema_version": 1, "retired": retired, "reopened": reopened}).to_string())
}

/// The selected inherited book and the books it supersedes, when retirement may run: the same
/// readiness `contact_book_selection` waits for, this device is a recorded successor, and the
/// selected inherited book's takeover scan completed.
fn eligible(conn: &Connection, ctx: &Ctx<'_>) -> Result<Option<(String, Vec<String>)>, Error> {
    if !initial_replay::caught_up(conn)? || !device_succession::local_hint_registered(conn)? {
        return Ok(None);
    }
    let successor: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM device_successions WHERE successor=?)",
        [ctx.device_id.to_string()],
        |row| row.get(0),
    )?;
    if !successor {
        return Ok(None);
    }
    let Some(selection) = device_succession::inherited_selection(conn, &ctx.device_id)? else {
        return Ok(None);
    };
    if !selection.takeover_complete || selection.retire_book_ids.is_empty() {
        return Ok(None);
    }
    Ok(Some((selection.book_id, selection.retire_book_ids)))
}

/// This device's replaced books.
fn replaced_books(conn: &Connection, ctx: &Ctx<'_>) -> Result<Vec<String>, Error> {
    Ok(conn
        .prepare(&format!(
            "SELECT id FROM contact_books WHERE owner_device_id=? AND forgotten=0 AND {} ORDER BY id",
            contacts::REPLACED_BOOK_SQL
        ))?
        .query_map([ctx.device_id.to_string()], |row| row.get(0))?
        .collect::<Result<_, _>>()?)
}

/// Retires one superseded book and returns how many live contacts it removed. The retired body
/// is stored first, so every tombstone embeds it.
fn retire_book(
    tx: &Connection,
    ctx: &Ctx<'_>,
    book_id: &str,
    replaced_by: &str,
) -> Result<usize, Error> {
    let (mut body, _) = contacts::owned_book(tx, ctx, book_id)?;
    body["state"] = json!("retired");
    body["superseded_by"] = json!(replaced_by);
    if !body.get("capabilities").is_some_and(Value::is_object) {
        body["capabilities"] = json!({});
    }
    body["capabilities"]["read"] = json!(false);
    body["capabilities"]["write"] = json!(false);
    let state_revision = contacts::book_state_revision(&body)?
        .checked_add(1)
        .ok_or(Error::Database)?;
    body["state_revision"] = json!(state_revision.to_string());
    tx.execute(
        "UPDATE contact_books SET state='retired',body=? WHERE id=?",
        rusqlite::params![
            serde_json::to_string(&body).map_err(|_| Error::Database)?,
            book_id
        ],
    )?;
    tx.execute(
        "UPDATE contact_scan_holds SET status='stale' WHERE book_id=? AND status='awaiting_approval'",
        [book_id],
    )?;
    close_requests(tx, ctx, book_id)?;
    crate::contact_sync_requests::end_for_retired_book(tx, ctx, book_id)?;
    let live: Vec<String> = tx
        .prepare("SELECT id FROM contacts WHERE book_id=? AND deleted_at IS NULL ORDER BY id")?
        .query_map([book_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let deleted_at = contacts::now();
    for contact_id in &live {
        contacts::tombstone_owned_unsealed(tx, ctx, book_id, contact_id, deleted_at)?;
    }
    contacts::publish_book_state(tx, ctx, book_id)?;
    Ok(live.len())
}

/// Closes the book's open edit and restore requests. A request never issued is rejected. An issued
/// write that never reconciled is resolved as failed, as the owner's Dismiss resolves an unknown
/// write: the phone keeps whatever it has, the taken-over book's next full scan publishes it, and
/// nothing is retried. An unresolved restore claim is released as a failed reconciliation would.
fn close_requests(tx: &Connection, ctx: &Ctx<'_>, book_id: &str) -> Result<(), Error> {
    let open: Vec<(String, String, bool)> = tx
        .prepare("SELECT request_id,status,permit_issued FROM contact_edit_ledger WHERE book_id=? AND status IN ('requested','approved','awaiting_approval','applying','outcome_unknown') ORDER BY request_id")?
        .query_map([book_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<_, _>>()?;
    for (request_id, status, issued) in open {
        let unresolved_write = issued || matches!(status.as_str(), "applying" | "outcome_unknown");
        let outcome = if unresolved_write {
            "failed"
        } else {
            "rejected"
        };
        contacts::finalize(
            tx,
            ctx,
            &request_id,
            book_id,
            outcome,
            Some(BOOK_REPLACED),
            None,
        )?;
    }
    tx.execute(
        "UPDATE contact_restore_claims SET status='failed' WHERE book_id=? AND status='claimed'",
        [book_id],
    )?;
    Ok(())
}
