//! Vault-wide deletion of a contact book, applied on every device.
//!
//! A device may delete the synced copy of a removed owner's address book for the whole vault.
//! The deletion replicates as one book fence (`ContactBookDeleted`, retained for the life of the
//! book) and one terminal record per contact (`ContactBookDeletedMember`) so each contact's
//! history can be pruned. The fence lives in `deleted_contact_books`. Every later record of the
//! same book at or below the fenced generation is a no-op, so replays and delayed records never
//! resurrect deleted data, while higher generations are unaffected.
use crate::contact_media;
use crate::contacts::{canonical_decimal, invalid};
use crate::{
    Ctx, EnvelopePurpose, Error, MAX_SEAL_BATCH, PrivatePayload, enqueue_without_sealing,
    seal_pending,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
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
            if stored_book(conn, book_id)?.is_some_and(|(owner, _)| owner != *owner_device_id) {
                return Ok(true);
            }
            store_fence(conn, book_id, owner_device_id, generation, *deleted_at_ms)?;
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
            if stored_book(conn, book_id)?.is_some_and(|(owner, _)| owner != *owner_device_id) {
                return Ok(true);
            }
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

/// Deletes a removed device's contact book for the whole vault. `input` is
/// `{"book_id", "revoked_device_ids"}`, where the revoked list is the host's trusted fact. One
/// transaction enqueues the book fence and one member record per stored contact (compaction
/// metadata is captured before any local projection), seals them, then applies the fence here.
pub(crate) fn delete(conn: &mut Connection, ctx: &Ctx<'_>, input: &str) -> Result<String, Error> {
    crate::contacts::require_unlocked(ctx)?;
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
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
    let generation = generation.to_string();
    let fence = PrivatePayload::ContactBookDeleted {
        book_id: book_id.to_owned(),
        owner_device_id: owner.clone(),
        generation: generation.clone(),
        deleted_at_ms: now_ms(),
    };
    enqueue_without_sealing(&tx, ctx, EnvelopePurpose::Event, None, None, &fence)?;
    for contact_id in members {
        let member = PrivatePayload::ContactBookDeletedMember {
            book_id: book_id.to_owned(),
            contact_id,
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
    fn historical_copies_still_fence_later_records() {
        let conn = Connection::open_in_memory().unwrap();
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
