//! Vault-wide deletion of a contact book, applied on every device.
//!
//! A device may delete the synced copy of a removed owner's address book for the whole vault.
//! The deletion replicates as one book fence (`ContactBookDeleted`, retained for the life of the
//! book) and one terminal record per contact (`ContactBookDeletedMember`) so each contact's
//! history can be pruned. The fence lives in `deleted_contact_books`. Every later record of the
//! same book at or below the fenced generation is a no-op, so replays and delayed records never
//! resurrect deleted data, while higher generations are unaffected.
use crate::contact_media;
use crate::contacts::{canonical_decimal, invalid, require_unlocked};
use crate::{
    Ctx, EnvelopePurpose, Error, MAX_SEAL_BATCH, PrivatePayload, enqueue_without_sealing, get_meta,
    receive_cursor, seal_pending, set_meta, to_i64,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

/// Refusal reasons of `delete`, carried by `Error::InvalidRequest`. Hosts map them to copy.
pub(crate) const DELETE_INPUT: &str = "contact book deletion input is invalid";
pub(crate) const DELETE_NOT_FOUND: &str = "contact book is not on this device";
pub(crate) const DELETE_ALREADY_DELETED: &str = "contact book was already deleted for all devices";
pub(crate) const DELETE_OWNER_NOT_REMOVED: &str = "contact book owner is not a removed device";
pub(crate) const DELETE_OWNER_IS_THIS_DEVICE: &str = "contact book belongs to this device";
pub(crate) const DELETE_OWNER_SUCCEEDED: &str =
    "contact book owner was succeeded by another device";
pub(crate) const DELETE_NOT_CURRENT: &str = "contact book deletion needs an up-to-date sync";

/// Newest server replay head a host reported after committing a page (see `observe_replay_head`).
const REPLAY_HEAD_META: &str = "contact_completion_replay_head";

/// Book-keyed local state removed with a deleted book. Scan children are deleted before their
/// scans. Every statement binds the book id as `?1`.
const BOOK_STATE_SQL: &[&str] = &[
    "DELETE FROM contact_scan_members WHERE scan_id IN (SELECT scan_id FROM contact_scans WHERE book_id=?1)",
    "DELETE FROM contact_scan_baseline WHERE scan_id IN (SELECT scan_id FROM contact_scans WHERE book_id=?1)",
    "DELETE FROM contact_scan_baselines WHERE scan_id IN (SELECT scan_id FROM contact_scans WHERE book_id=?1)",
    "DELETE FROM contact_scans WHERE book_id=?1",
    "DELETE FROM contact_scan_holds WHERE book_id=?1",
    "DELETE FROM contact_scan_state WHERE book_id=?1",
    "DELETE FROM contact_scan_requests WHERE book_id=?1",
    "DELETE FROM contact_edit_ledger WHERE book_id=?1",
    "DELETE FROM contact_restore_claims WHERE book_id=?1",
    "DELETE FROM contact_book_sources WHERE book_id=?1",
    "DELETE FROM contact_sources WHERE book_id=?1",
    "DELETE FROM contact_source_field_map WHERE book_id=?1",
    "DELETE FROM contact_source_map WHERE book_id=?1",
    "DELETE FROM contact_name_pins WHERE book_id=?1",
    "DELETE FROM contact_book_lookup_exclusions WHERE book_id=?1",
    "DELETE FROM contact_address_index WHERE book_id=?1",
    "DELETE FROM contact_resolution_projection WHERE book_id=?1",
    "DELETE FROM contact_resolution_state WHERE book_id=?1",
    "DELETE FROM inherited_contact_books WHERE book_id=?1",
];

/// Whether a deletion names `own_device` as the book owner. Such a deletion is quarantined: this
/// device's own address book is local truth and no replicated event removes it.
pub(crate) fn targets_own_book(payload: &PrivatePayload, own_device: &str) -> bool {
    matches!(
        payload,
        PrivatePayload::ContactBookDeleted { owner_device_id, .. }
            | PrivatePayload::ContactBookDeletedMember { owner_device_id, .. }
            if owner_device_id == own_device
    )
}

/// Applies a book fence or a per-contact record. `Ok(false)` quarantines the record, which is
/// what a malformed generation (`canonical_decimal` rejects it) becomes through `apply_event`.
pub(crate) fn apply(conn: &Connection, payload: &PrivatePayload) -> Result<bool, Error> {
    match payload {
        PrivatePayload::ContactBookDeleted {
            book_id,
            owner_device_id,
            generation,
            deleted_at_ms,
        } => {
            let generation = canonical_decimal(generation)?;
            check_ids(book_id, owner_device_id)?;
            // The owner was succeeded, so its successor now owns the book: nothing to delete.
            if has_successor(conn, owner_device_id)? {
                return Ok(true);
            }
            // A stored book of another owner under this id is not this owner's to delete. The
            // fence is not stored at all, so a later book of that owner is not suppressed.
            if stored_under_other_owner(conn, book_id, owner_device_id)? {
                return Ok(true);
            }
            store_fence(conn, book_id, owner_device_id, generation, *deleted_at_ms)?;
            // Captured before the member rows below go: a terminal member already received is what
            // settles a contact, so it must still be visible here.
            capture_completion(conn, book_id, owner_device_id, generation, None)?;
            conn.execute(
                "DELETE FROM deleted_contact_members WHERE book_id=? AND owner_device_id=? AND generation<=?",
                params![book_id, owner_device_id, generation],
            )?;
            remove_covered_contacts(conn, book_id, owner_device_id, generation)?;
            if stored_at_or_below(conn, book_id, owner_device_id, generation)?
                && contact_ids(conn, book_id)?.is_empty()
            {
                remove_local_book(conn, book_id)?;
            }
            Ok(true)
        }
        PrivatePayload::ContactBookDeletedMember {
            book_id,
            contact_id,
            owner_device_id,
            generation,
        } => {
            let generation = canonical_decimal(generation)?;
            check_ids(book_id, owner_device_id)?;
            if has_successor(conn, owner_device_id)? {
                return Ok(true);
            }
            if stored_under_other_owner(conn, book_id, owner_device_id)? {
                return Ok(true);
            }
            // A terminal record from elsewhere settles this device's own obligation for the contact.
            cancel_completion(conn, book_id, owner_device_id, contact_id, generation)?;
            if !covers(conn, book_id, owner_device_id, Some(generation))? {
                conn.execute(
                    "INSERT INTO deleted_contact_members(book_id,contact_id,owner_device_id,generation) VALUES(?,?,?,?) ON CONFLICT(book_id,contact_id,owner_device_id) DO UPDATE SET generation=MAX(generation,excluded.generation)",
                    params![book_id, contact_id, owner_device_id, generation],
                )?;
            }
            let covered: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM contacts c JOIN contact_books b ON b.id=c.book_id WHERE c.book_id=? AND c.id=? AND COALESCE(c.publication_owner,b.owner_device_id)=? AND COALESCE(c.publication_generation,b.generation)<=?)",
                params![book_id, contact_id, owner_device_id, generation],
                |row| row.get(0),
            )?;
            if covered {
                remove_contact(conn, book_id, contact_id)?;
            }
            Ok(true)
        }
        _ => Err(invalid()),
    }
}

pub(crate) fn member_covers(
    conn: &Connection,
    book_id: &str,
    contact_id: &str,
    owner: &str,
    generation: i64,
) -> Result<bool, Error> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM deleted_contact_members WHERE book_id=? AND contact_id=? AND owner_device_id=? AND generation>=?)",
        params![book_id, contact_id, owner, generation],
        |row| row.get(0),
    )?)
}

pub(crate) fn remove_covered_contacts(
    conn: &Connection,
    book_id: &str,
    owner: &str,
    generation: i64,
) -> Result<(), Error> {
    let ids: Vec<String> = conn.prepare(
        "SELECT c.id FROM contacts c JOIN contact_books b ON b.id=c.book_id WHERE c.book_id=? AND COALESCE(c.publication_owner,b.owner_device_id)=? AND COALESCE(c.publication_generation,b.generation)<=?"
    )?.query_map(params![book_id, owner, generation], |row| row.get(0))?.collect::<Result<_, _>>()?;
    for id in ids {
        remove_contact(conn, book_id, &id)?;
    }
    Ok(())
}

/// Whether this device knows its synced state is not current, so a deletion must wait. Work that
/// `apply_pending` would apply now counts: a pending journal row (not parked as a snapshot isolate)
/// under an unlocked epoch. Rows of locked epochs wait for their key and do not block. Also not
/// current: a published snapshot not yet drained, a projection draining, staging or failed, and a
/// latched contact repair.
fn sync_not_current(conn: &Connection, ctx: &Ctx<'_>) -> Result<bool, Error> {
    let epochs = ctx
        .keys
        .keys()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let pending_inbound: bool = conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM journal INDEXED BY journal_apply_eligible WHERE status='pending' AND reason IS NOT 'snapshot_rcs_isolated' AND key_epoch IN ({epochs}))"
        ),
        [],
        |row| row.get(0),
    )?;
    if pending_inbound {
        return Ok(true);
    }
    let undrained: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM snapshot_generations WHERE state='published' AND received>drained)",
        [],
        |row| row.get(0),
    )?;
    if undrained {
        return Ok(true);
    }
    let projection: Option<String> = conn
        .query_row(
            "SELECT state FROM projection_generations WHERE state!='superseded' ORDER BY generation DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if matches!(
        projection.as_deref(),
        Some("draining" | "staging" | "failed")
    ) {
        return Ok(true);
    }
    Ok(get_meta(conn, crate::snapshot_projection::REPAIR_LATCH)?.is_some())
}

/// Records the contacts this device holds that a fence of `owner` covers, before their rows are
/// removed. A contact without a known terminal member becomes owed work, so this device publishes
/// the member itself. Known means a member at or above `generation` in `deleted_contact_members`,
/// or, while promoting snapshot generation `staged`, one staged in `projection_deleted_members`
/// (those reach the live table only after this capture). Covered contacts include tombstones.
pub(crate) fn capture_completion(
    conn: &Connection,
    book_id: &str,
    owner: &str,
    generation: i64,
    staged: Option<i64>,
) -> Result<(), Error> {
    let gate = to_i64(receive_cursor(conn)?)?;
    conn.execute(
        "INSERT INTO contact_book_deletion_completion(book_id,owner_device_id,generation,gate_cursor) VALUES(?,?,?,?) ON CONFLICT(book_id,owner_device_id) DO UPDATE SET generation=MAX(generation,excluded.generation),gate_cursor=MAX(gate_cursor,excluded.gate_cursor)",
        params![book_id, owner, generation, gate],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO contact_book_deletion_completion_contacts(book_id,owner_device_id,contact_id) SELECT c.book_id,?2,c.id FROM contacts c JOIN contact_books b ON b.id=c.book_id WHERE c.book_id=?1 AND COALESCE(c.publication_owner,b.owner_device_id)=?2 AND COALESCE(c.publication_generation,b.generation)<=?3 AND NOT EXISTS(SELECT 1 FROM deleted_contact_members m WHERE m.book_id=c.book_id AND m.contact_id=c.id AND m.owner_device_id=?2 AND m.generation>=?3)",
        params![book_id, owner, generation],
    )?;
    // A member staged by the snapshot being promoted is not in `deleted_contact_members` yet, so
    // it settles its contact here. Only promotion has a stage, so the live path never reads it.
    if let Some(staged) = staged {
        conn.execute(
            "DELETE FROM contact_book_deletion_completion_contacts WHERE book_id=?1 AND owner_device_id=?2 AND EXISTS(SELECT 1 FROM projection_deleted_members p WHERE p.generation=?3 AND p.book_id=contact_book_deletion_completion_contacts.book_id AND p.contact_id=contact_book_deletion_completion_contacts.contact_id AND p.owner_device_id=contact_book_deletion_completion_contacts.owner_device_id AND p.book_generation>=?4)",
            params![book_id, owner, staged, generation],
        )?;
    }
    Ok(())
}

/// Records the owed members of a promotion's covered contacts that live only in the stage: staged
/// publications (`projection_contacts`) and covered records staging dropped
/// (`projection_covered_contacts`). Deduplicated against terminal members known in the stage and
/// live. Called inside the promotion transaction, after this stage's fences reach
/// `deleted_contact_books` and before the stage rows are removed, so a failed promotion owes nothing.
pub(crate) fn capture_staged_completion(conn: &Connection, generation: i64) -> Result<(), Error> {
    let gate = to_i64(receive_cursor(conn)?)?;
    const STAGED: &str = "WITH staged(book_id,contact_id,owner,gen) AS (SELECT book_id,contact_id,publication_owner,publication_generation FROM projection_contacts WHERE generation=?1 AND publication_owner IS NOT NULL UNION ALL SELECT book_id,contact_id,owner_device_id,book_generation FROM projection_covered_contacts WHERE generation=?1)";
    let uncovered = "s.gen<=d.generation AND NOT EXISTS(SELECT 1 FROM projection_deleted_members m WHERE m.generation=?1 AND m.book_id=s.book_id AND m.contact_id=s.contact_id AND m.owner_device_id=s.owner AND m.book_generation>=d.generation) AND NOT EXISTS(SELECT 1 FROM deleted_contact_members m WHERE m.book_id=s.book_id AND m.contact_id=s.contact_id AND m.owner_device_id=s.owner AND m.generation>=d.generation)";
    conn.execute(
        &format!("{STAGED} INSERT INTO contact_book_deletion_completion(book_id,owner_device_id,generation,gate_cursor) SELECT DISTINCT s.book_id,s.owner,d.generation,?2 FROM staged s JOIN deleted_contact_books d ON d.book_id=s.book_id AND d.owner_device_id=s.owner WHERE {uncovered} ON CONFLICT(book_id,owner_device_id) DO UPDATE SET generation=MAX(generation,excluded.generation),gate_cursor=MAX(gate_cursor,excluded.gate_cursor)"),
        params![generation, gate],
    )?;
    conn.execute(
        &format!("{STAGED} INSERT OR IGNORE INTO contact_book_deletion_completion_contacts(book_id,owner_device_id,contact_id) SELECT DISTINCT s.book_id,s.owner,s.contact_id FROM staged s JOIN deleted_contact_books d ON d.book_id=s.book_id AND d.owner_device_id=s.owner WHERE {uncovered}"),
        params![generation],
    )?;
    Ok(())
}

/// Settles this device's owed member for one contact, once a terminal member at or above the
/// fence generation is known (received, or enqueued by this device). A lower-generation member
/// does not settle it.
fn cancel_completion(
    conn: &Connection,
    book_id: &str,
    owner: &str,
    contact_id: &str,
    generation: i64,
) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM contact_book_deletion_completion_contacts WHERE book_id=?1 AND owner_device_id=?2 AND contact_id=?3 AND ?4>=(SELECT generation FROM contact_book_deletion_completion w WHERE w.book_id=?1 AND w.owner_device_id=?2)",
        params![book_id, owner, contact_id, generation],
    )?;
    Ok(())
}

/// Drops every owed member of one fence.
fn forget_completion(conn: &Connection, book_id: &str, owner: &str) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM contact_book_deletion_completion_contacts WHERE book_id=? AND owner_device_id=?",
        params![book_id, owner],
    )?;
    conn.execute(
        "DELETE FROM contact_book_deletion_completion WHERE book_id=? AND owner_device_id=?",
        params![book_id, owner],
    )?;
    Ok(())
}

/// Whether `drain_completion` can enqueue now. It needs an unlocked vault and the active epoch's
/// compaction key. Compaction must also be supported, complete and fully readable: the conditions
/// `compaction_plan` checks before any contact record may start. Work waits otherwise, and it is
/// not reported as pending either.
pub(crate) fn completion_ready(conn: &Connection, ctx: &Ctx<'_>) -> Result<bool, Error> {
    if require_unlocked(ctx).is_err() {
        return Ok(false);
    }
    let compaction_key = ctx
        .active_epoch
        .and_then(|epoch| ctx.keys.get(&epoch))
        .is_some_and(|keys| keys.compaction.is_some());
    Ok(compaction_key
        && get_meta(conn, "server_compaction_supported")?.as_deref() == Some("1")
        && get_meta(conn, "compaction_backfill_complete")?.as_deref() == Some("1")
        && crate::backfill_unreadable(conn, ctx)? == 0)
}

/// Replay head that owed members may drain against, or `None` to defer. Deferral holds while
/// (a) no host has reported a head, (b) receipt has not reached the newest reported head, or
/// (c) `sync_not_current` finds inbound work, a snapshot or projection in progress, or a repair.
/// (b) and (c) let the deleter's members, which follow its fence on the server, be received and
/// applied first. A row is eligible only when its gate (set at capture) is at or below the head.
fn completion_head(conn: &Connection, ctx: &Ctx<'_>) -> Result<Option<u64>, Error> {
    if !completion_ready(conn, ctx)? {
        return Ok(None);
    }
    let Some(head) = get_meta(conn, REPLAY_HEAD_META)? else {
        return Ok(None);
    };
    let head: u64 = head.parse().map_err(|_| Error::Database)?;
    if receive_cursor(conn)? < head || sync_not_current(conn, ctx)? {
        return Ok(None);
    }
    Ok(Some(head))
}

/// Whether `drain_completion` would enqueue an owed member now. `sync_work_status` reports owed work
/// only then, so hosts that loop on pending apply cannot spin on work that is still waiting.
pub(crate) fn completion_pending(conn: &Connection, ctx: &Ctx<'_>) -> Result<bool, Error> {
    let owed: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM contact_book_deletion_completion_contacts LIMIT 1)",
        [],
        |row| row.get(0),
    )?;
    if !owed {
        return Ok(false);
    }
    let Some(head) = completion_head(conn, ctx)? else {
        return Ok(false);
    };
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM contact_book_deletion_completion_contacts p JOIN contact_book_deletion_completion w ON w.book_id=p.book_id AND w.owner_device_id=p.owner_device_id WHERE w.gate_cursor<=?1)",
        params![to_i64(head)?],
        |row| row.get(0),
    )?)
}

/// Records a server replay head that a host observed after committing a page. The highest head
/// wins. Owed members publish only after receipt reaches it (see `completion_head`).
pub(crate) fn observe_replay_head(conn: &Connection, cursor: u64) -> Result<(), Error> {
    let newer = match get_meta(conn, REPLAY_HEAD_META)? {
        Some(head) => cursor > head.parse::<u64>().map_err(|_| Error::Database)?,
        None => true,
    };
    if newer {
        set_meta(conn, REPLAY_HEAD_META, &cursor.to_string())?;
    }
    Ok(())
}

/// Whether the contact's current publication is not covered by `owner`'s fence at `generation`: a
/// higher generation, or another owner. Such a contact continued after the fence, so its owed
/// member would supersede a newer frontier. An absent contact is not superseded.
fn superseded_publication(
    conn: &Connection,
    book_id: &str,
    owner: &str,
    contact_id: &str,
    generation: i64,
) -> Result<bool, Error> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM contacts c JOIN contact_books b ON b.id=c.book_id WHERE c.book_id=?1 AND c.id=?2 AND NOT(COALESCE(c.publication_owner,b.owner_device_id)=?3 AND COALESCE(c.publication_generation,b.generation)<=?4))",
        params![book_id, contact_id, owner, generation],
        |row| row.get(0),
    )?)
}

/// Drops one owed member without emitting it.
fn settle_owed(
    conn: &Connection,
    book_id: &str,
    owner: &str,
    contact_id: &str,
) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM contact_book_deletion_completion_contacts WHERE book_id=?1 AND owner_device_id=?2 AND contact_id=?3",
        params![book_id, owner, contact_id],
    )?;
    Ok(())
}

/// Publishes the member records still owed by fences this device applied, at most `MAX_SEAL_BATCH`
/// per pass. The caller runs it at the end of `apply_pending`, inside the batch transaction and
/// after every row of the batch and any snapshot promotion, so in-batch order does not matter.
/// Only an unlocked device emits, and only when `completion_head` allows it. Work for a book this
/// device owns, or whose owner was succeeded, is dropped because those records are never applied.
/// A contact that a newer or foreign publication now covers is settled without a member. A failure
/// other than a compaction deferral fails the batch.
pub(crate) fn drain_completion(tx: &mut Transaction<'_>, ctx: &Ctx<'_>) -> Result<(), Error> {
    // Work with nothing left owed is finished, whatever the lock or compaction state.
    tx.execute(
        "DELETE FROM contact_book_deletion_completion WHERE NOT EXISTS(SELECT 1 FROM contact_book_deletion_completion_contacts p WHERE p.book_id=contact_book_deletion_completion.book_id AND p.owner_device_id=contact_book_deletion_completion.owner_device_id)",
        [],
    )?;
    // The readiness check reads compaction state, so it runs only when something is owed.
    let owed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM contact_book_deletion_completion_contacts)",
        [],
        |row| row.get(0),
    )?;
    if !owed {
        return Ok(());
    }
    let Some(head) = completion_head(tx, ctx)? else {
        return Ok(());
    };
    let me = ctx.device_id.to_string();
    let owners: Vec<(String, String)> = tx
        .prepare("SELECT book_id,owner_device_id FROM contact_book_deletion_completion")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (book_id, owner) in owners {
        if owner == me || has_successor(tx, &owner)? {
            forget_completion(tx, &book_id, &owner)?;
        }
    }
    let batch: Vec<(String, String, i64, String)> = tx
        .prepare(
            "SELECT p.book_id,p.owner_device_id,w.generation,p.contact_id FROM contact_book_deletion_completion_contacts p JOIN contact_book_deletion_completion w ON w.book_id=p.book_id AND w.owner_device_id=p.owner_device_id WHERE w.gate_cursor<=?2 ORDER BY p.rowid LIMIT ?1",
        )?
        .query_map(params![MAX_SEAL_BATCH as i64, to_i64(head)?], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    let mut enqueued = false;
    for (book_id, owner, generation, contact_id) in batch {
        if member_covers(tx, &book_id, &contact_id, &owner, generation)? {
            cancel_completion(tx, &book_id, &owner, &contact_id, generation)?;
            continue;
        }
        if superseded_publication(tx, &book_id, &owner, &contact_id, generation)? {
            settle_owed(tx, &book_id, &owner, &contact_id)?;
            continue;
        }
        let member = PrivatePayload::ContactBookDeletedMember {
            book_id: book_id.clone(),
            contact_id: contact_id.clone(),
            owner_device_id: owner.clone(),
            generation: generation.to_string(),
        };
        let savepoint = tx.savepoint()?;
        match enqueue_without_sealing(&savepoint, ctx, EnvelopePurpose::Event, None, None, &member)
        {
            Ok(_) => savepoint.commit()?,
            // Compaction is not ready to take a compactable record (`compaction_plan`): keep the
            // rest for a later pass. Any other failure is unexpected and fails the batch.
            Err(Error::KeysUnavailable | Error::InvalidRequest(_)) => break,
            Err(error) => return Err(error),
        }
        enqueued = true;
        cancel_completion(tx, &book_id, &owner, &contact_id, generation)?;
    }
    if enqueued {
        seal_pending(tx, ctx, MAX_SEAL_BATCH)?;
    }
    Ok(())
}

/// Deletes a removed device's contact book for the whole vault. `input` is
/// `{"book_id", "revoked_device_ids", "sync_current"}`. The revoked list and `sync_current` are the
/// host's trusted facts; a missing `sync_current` is false. Refusals are checked in this order:
/// invalid input, locked vault (`KeysUnavailable`), then `DELETE_NOT_CURRENT` unless
/// `sync_current` is true and `sync_not_current` finds nothing outstanding, then the book checks.
/// One transaction enqueues the book fence and one member record per stored contact (compaction
/// metadata is captured before any local projection), seals them, then applies the fence here.
pub(crate) fn delete(conn: &mut Connection, ctx: &Ctx<'_>, input: &str) -> Result<String, Error> {
    let input: Value =
        serde_json::from_str(input).map_err(|_| Error::InvalidRequest(DELETE_INPUT))?;
    let book_id = text(&input, "book_id")?;
    let revoked = input
        .get("revoked_device_ids")
        .and_then(Value::as_array)
        .ok_or(Error::InvalidRequest(DELETE_INPUT))?
        .iter()
        .map(|id| id.as_str().ok_or(Error::InvalidRequest(DELETE_INPUT)))
        .collect::<Result<Vec<&str>, _>>()?;
    let sync_current = input
        .get("sync_current")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    require_unlocked(ctx)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !sync_current || sync_not_current(&tx, ctx)? {
        return Err(Error::InvalidRequest(DELETE_NOT_CURRENT));
    }
    let Some((owner, generation)) = stored_book(&tx, book_id)? else {
        // The local book is gone. A fence for this id means the deletion already ran (a re-run).
        let refusal = if has_fence(&tx, book_id)? {
            DELETE_ALREADY_DELETED
        } else {
            DELETE_NOT_FOUND
        };
        return Err(Error::InvalidRequest(refusal));
    };
    if !revoked.contains(&owner.as_str()) {
        return Err(Error::InvalidRequest(DELETE_OWNER_NOT_REMOVED));
    }
    if owner == ctx.device_id.to_string() {
        return Err(Error::InvalidRequest(DELETE_OWNER_IS_THIS_DEVICE));
    }
    if has_successor(&tx, &owner)? {
        return Err(Error::InvalidRequest(DELETE_OWNER_SUCCEEDED));
    }
    // Refused only when the fence already covers the book as stored. A continued book (a higher
    // generation after an earlier deletion) is deleted again under a higher fence.
    if fence_generation(&tx, book_id, &owner)?.is_some_and(|fenced| fenced >= generation) {
        return Err(Error::InvalidRequest(DELETE_ALREADY_DELETED));
    }
    // Every stored contact gets a member record, tombstones included, so each contact's history
    // is superseded and can be pruned. The reported count is the live contacts.
    let members = contact_ids(&tx, book_id)?;
    let live: i64 = tx.query_row(
        "SELECT COUNT(*) FROM contacts WHERE book_id=? AND deleted_at IS NULL",
        [book_id],
        |row| row.get(0),
    )?;
    let book_generation = generation;
    let generation = generation.to_string();
    let fence = PrivatePayload::ContactBookDeleted {
        book_id: book_id.to_owned(),
        owner_device_id: owner.clone(),
        generation: generation.clone(),
        deleted_at_ms: now_ms(),
    };
    enqueue_without_sealing(&tx, ctx, EnvelopePurpose::Event, None, None, &fence)?;
    for contact_id in &members {
        let member = PrivatePayload::ContactBookDeletedMember {
            book_id: book_id.to_owned(),
            contact_id: contact_id.clone(),
            owner_device_id: owner.clone(),
            generation: generation.clone(),
        };
        enqueue_without_sealing(&tx, ctx, EnvelopePurpose::Event, None, None, &member)?;
    }
    loop {
        if seal_pending(&tx, ctx, MAX_SEAL_BATCH)? < MAX_SEAL_BATCH {
            break;
        }
    }
    apply(&tx, &fence)?;
    // Every stored contact already has this device's member above, so the fence's capture of
    // them is settled here and must not emit a second record.
    for contact_id in &members {
        cancel_completion(&tx, book_id, &owner, contact_id, book_generation)?;
    }
    tx.commit()?;
    Ok(json!({"status": "deleted", "book_id": book_id, "contacts": live}).to_string())
}

/// Whether records of `owner` are ignored on this device: its own book, or a book whose owner
/// was succeeded. Staging applies the same rule as live apply.
pub(crate) fn owner_ignored(conn: &Connection, me: &str, owner: &str) -> Result<bool, Error> {
    Ok(owner == me || has_successor(conn, owner)?)
}

fn text<'a>(input: &'a Value, key: &str) -> Result<&'a str, Error> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(Error::InvalidRequest(DELETE_INPUT))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Highest generation fenced for `book_id` by `owner`'s vault-wide deletion, if any.
pub(crate) fn fence_generation(
    conn: &Connection,
    book_id: &str,
    owner: &str,
) -> Result<Option<i64>, Error> {
    Ok(conn
        .query_row(
            "SELECT generation FROM deleted_contact_books WHERE book_id=? AND owner_device_id=?",
            params![book_id, owner],
            |row| row.get(0),
        )
        .optional()?)
}

/// Whether any owner's vault-wide deletion fenced `book_id`.
fn has_fence(conn: &Connection, book_id: &str) -> Result<bool, Error> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM deleted_contact_books WHERE book_id=?)",
        [book_id],
        |row| row.get(0),
    )?)
}

/// Whether a record of `book_id` owned by `owner` is covered by that owner's fence. `generation`
/// is the record's own generation. A record without one is judged by the stored book of the same
/// owner, an absent book counts as covered, and a stored book of another owner is never covered.
/// Covered records are no-ops: they neither quarantine nor resurrect.
pub(crate) fn covers(
    conn: &Connection,
    book_id: &str,
    owner: &str,
    generation: Option<i64>,
) -> Result<bool, Error> {
    let Some(fenced) = fence_generation(conn, book_id, owner)? else {
        return Ok(false);
    };
    let generation = match generation {
        Some(generation) => generation,
        None => match stored_book(conn, book_id)? {
            Some((stored_owner, stored)) if stored_owner == owner => stored,
            Some(_) => return Ok(false),
            None => 0,
        },
    };
    Ok(generation <= fenced)
}

/// Removes every local trace of a book: its contacts, then all book-keyed state. Photo holders
/// are released only after the contact rows are gone, because `sync_contact` reads the row.
/// Callers run this inside the surrounding transaction.
pub(crate) fn remove_local_book(conn: &Connection, book_id: &str) -> Result<(), Error> {
    let contact_ids = contact_ids(conn, book_id)?;
    for sql in BOOK_STATE_SQL {
        conn.execute(sql, [book_id])?;
    }
    conn.execute("DELETE FROM contacts WHERE book_id=?", [book_id])?;
    conn.execute("DELETE FROM contact_books WHERE id=?", [book_id])?;
    for contact_id in &contact_ids {
        contact_media::sync_contact(conn, book_id, contact_id)?;
    }
    contact_media::release_book_edit_holders(conn, book_id)
}

fn remove_contact(conn: &Connection, book_id: &str, contact_id: &str) -> Result<(), Error> {
    conn.execute(
        "DELETE FROM contacts WHERE book_id=? AND id=?",
        params![book_id, contact_id],
    )?;
    contact_media::sync_contact(conn, book_id, contact_id)
}

fn store_fence(
    conn: &Connection,
    book_id: &str,
    owner: &str,
    generation: i64,
    deleted_at_ms: i64,
) -> Result<(), Error> {
    conn.execute(
        "INSERT INTO deleted_contact_books(book_id,owner_device_id,generation,deleted_at_ms) VALUES(?,?,?,?) ON CONFLICT(book_id,owner_device_id) DO UPDATE SET generation=excluded.generation,deleted_at_ms=excluded.deleted_at_ms WHERE excluded.generation > deleted_contact_books.generation",
        params![book_id, owner, generation, deleted_at_ms],
    )?;
    Ok(())
}

/// Whether the book stored under `book_id` belongs to another owner. The stored book is normalised
/// to its terminal owner first, so a legacy book still named by a succeeded owner is judged by the
/// owner that now holds it, not consumed as a foreign-owner no-op.
fn stored_under_other_owner(conn: &Connection, book_id: &str, owner: &str) -> Result<bool, Error> {
    crate::device_succession::inherit_stored_book(conn, book_id)?;
    Ok(stored_book(conn, book_id)?.is_some_and(|(stored, _)| stored != owner))
}

/// Whether the stored book belongs to `owner` at a generation not above `generation`.
fn stored_at_or_below(
    conn: &Connection,
    book_id: &str,
    owner: &str,
    generation: i64,
) -> Result<bool, Error> {
    Ok(stored_book(conn, book_id)?
        .is_some_and(|(stored_owner, stored)| stored_owner == owner && stored <= generation))
}

/// Owner and generation of the stored book, if this device holds it.
fn stored_book(conn: &Connection, book_id: &str) -> Result<Option<(String, i64)>, Error> {
    Ok(conn
        .query_row(
            "SELECT owner_device_id,generation FROM contact_books WHERE id=?",
            [book_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?)
}

fn contact_ids(conn: &Connection, book_id: &str) -> Result<Vec<String>, Error> {
    let mut statement = conn.prepare("SELECT id FROM contacts WHERE book_id=?")?;
    let ids = statement
        .query_map([book_id], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(ids)
}

fn has_successor(conn: &Connection, owner: &str) -> Result<bool, Error> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM device_successions WHERE predecessor=?)",
        [owner],
        |row| row.get(0),
    )?)
}

pub(crate) fn check_ids(book_id: &str, owner: &str) -> Result<(), Error> {
    if book_id.is_empty() || owner.is_empty() {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_device_deletions_are_detected_for_both_kinds() {
        let fence = PrivatePayload::ContactBookDeleted {
            book_id: "b1".into(),
            owner_device_id: "me".into(),
            generation: "1".into(),
            deleted_at_ms: 1,
        };
        let member = PrivatePayload::ContactBookDeletedMember {
            book_id: "b1".into(),
            contact_id: "c1".into(),
            owner_device_id: "me".into(),
            generation: "1".into(),
        };
        assert!(targets_own_book(&fence, "me"));
        assert!(targets_own_book(&member, "me"));
        assert!(!targets_own_book(&fence, "other"));
    }

    #[test]
    fn a_member_record_keys_its_compaction_identity_by_book_and_contact() {
        let member = PrivatePayload::ContactBookDeletedMember {
            book_id: "b1".into(),
            contact_id: "c2".into(),
            owner_device_id: "owner".into(),
            generation: "1".into(),
        };
        assert_eq!(
            crate::compaction_identity(&member).as_deref(),
            Some("contact:b1:c2")
        );
    }

    #[test]
    fn historical_copies_still_fence_later_records() {
        let conn = Connection::open_in_memory().unwrap();
        // The completion capture reads the receive frontier from metadata.
        conn.execute_batch("CREATE TABLE metadata(k TEXT PRIMARY KEY, v TEXT NOT NULL)")
            .unwrap();
        crate::contacts::initialize(&conn).unwrap();
        let fence = PrivatePayload::ContactBookDeleted {
            book_id: "b1".into(),
            owner_device_id: "owner".into(),
            generation: "1".into(),
            deleted_at_ms: 1,
        };
        assert!(crate::contacts::apply_event(&conn, "deleter", true, 1, &fence).unwrap());
        let upsert = PrivatePayload::ContactUpserted {
            book: serde_json::json!({"id":"b1","owner_device_id":"owner","generation":"1","state":"active"}),
            contact: serde_json::json!({"id":"c1","book_id":"b1","revision":"3","display_name":"Ada"}),
        };
        assert!(crate::contacts::apply_event(&conn, "owner", true, 1, &upsert).unwrap());
        let contacts: i64 = conn
            .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
            .unwrap();
        let books: i64 = conn
            .query_row("SELECT COUNT(*) FROM contact_books", [], |row| row.get(0))
            .unwrap();
        assert_eq!((contacts, books), (0, 0));
        assert_eq!(fence_generation(&conn, "b1", "owner").unwrap(), Some(1));
    }

    #[test]
    fn fenced_photo_edit_request_holds_no_photo() {
        let conn = Connection::open_in_memory().unwrap();
        crate::contacts::initialize(&conn).unwrap();
        crate::contact_media::initialize(&conn).unwrap();
        let photo = "00000000-0000-4000-8000-0000000000aa";
        let edit = PrivatePayload::ContactEditRequest {
            request: json!({
                "request_id": "r1",
                "target_owner": "owner",
                "book_id": "b1",
                "photo_op": {"op": "set", "attachment_id": photo}
            }),
        };
        let holders = || -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM contact_photo_references WHERE holder_id='r1'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };

        crate::contact_media::after_apply(&conn, &edit).unwrap();
        assert_eq!(holders(), 1, "control: an unfenced edit holds its photo");

        store_fence(&conn, "b1", "owner", 1, 1).unwrap();
        conn.execute("DELETE FROM contact_photo_references", [])
            .unwrap();
        crate::contact_media::after_apply(&conn, &edit).unwrap();
        assert_eq!(holders(), 0, "a fenced edit holds nothing");
    }

    #[test]
    fn book_fence_is_never_in_a_book_states_frontier() {
        let fence = PrivatePayload::ContactBookDeleted {
            book_id: "b1".into(),
            owner_device_id: "owner".into(),
            generation: "1".into(),
            deleted_at_ms: 1,
        };
        let state = PrivatePayload::ContactBookState {
            book: json!({"id": "b1", "owner_device_id": "owner", "generation": "2", "state": "active"}),
        };
        assert_eq!(
            crate::compaction_identity(&fence).as_deref(),
            Some("contact-book-deleted:b1:owner")
        );
        assert_ne!(
            crate::compaction_identity(&fence),
            crate::compaction_identity(&state)
        );
        let group = |payload: &PrivatePayload| {
            crate::compaction_group(payload)
                .unwrap()
                .map(|(name, parts, _)| (name, parts))
        };
        assert_ne!(group(&fence), group(&state));
        let other_fence = PrivatePayload::ContactBookDeleted {
            book_id: "b1".into(),
            owner_device_id: "other".into(),
            generation: "1".into(),
            deleted_at_ms: 1,
        };
        assert_ne!(
            crate::compaction_identity(&fence),
            crate::compaction_identity(&other_fence)
        );
        assert_ne!(group(&fence), group(&other_fence));
    }
}
