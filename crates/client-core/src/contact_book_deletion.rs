//! Vault-wide deletion of a contact book, applied on every device.
//!
//! A device may delete the synced copy of a removed owner's address book for the whole vault.
//! The deletion replicates as one book fence (`ContactBookDeleted`, retained for the life of the
//! book) and one terminal record per contact (`ContactBookDeletedMember`) so each contact's
//! history can be pruned. The fence lives in `deleted_contact_books`. Every later record of the
//! same book at or below the fenced generation is a no-op, so replays and delayed records never
//! resurrect deleted data, while higher generations are unaffected.
use crate::Error;
use crate::PrivatePayload;
use crate::contact_media;
use crate::contacts::{canonical_decimal, invalid};
use rusqlite::{Connection, OptionalExtension, params};

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

/// Applies a book fence or a per-contact record. `Ok(false)` quarantines the record.
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
            store_fence(conn, book_id, owner_device_id, generation, *deleted_at_ms)?;
            if stored_at_or_below(conn, book_id, owner_device_id, generation)? {
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
            if stored_at_or_below(conn, book_id, owner_device_id, generation)?
                || covers(conn, book_id, Some(generation))?
            {
                remove_contact(conn, book_id, contact_id)?;
            }
            Ok(true)
        }
        _ => Err(invalid()),
    }
}

/// Highest generation fenced for `book_id` by a vault-wide deletion, if any.
pub(crate) fn fence_generation(conn: &Connection, book_id: &str) -> Result<Option<i64>, Error> {
    Ok(conn
        .query_row(
            "SELECT generation FROM deleted_contact_books WHERE book_id=?",
            [book_id],
            |row| row.get(0),
        )
        .optional()?)
}

/// Whether a record for `book_id` is covered by the book's fence. `generation` is the record's
/// own generation. A record without one is judged by the stored book's generation, and an absent
/// book counts as covered. Covered records are no-ops: they neither quarantine nor resurrect.
pub(crate) fn covers(
    conn: &Connection,
    book_id: &str,
    generation: Option<i64>,
) -> Result<bool, Error> {
    let Some(fenced) = fence_generation(conn, book_id)? else {
        return Ok(false);
    };
    let generation = match generation {
        Some(generation) => generation,
        None => stored_book(conn, book_id)?.map_or(0, |(_, stored)| stored),
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
        "INSERT INTO deleted_contact_books(book_id,owner_device_id,generation,deleted_at_ms) VALUES(?,?,?,?) ON CONFLICT(book_id) DO UPDATE SET owner_device_id=excluded.owner_device_id,generation=excluded.generation,deleted_at_ms=excluded.deleted_at_ms WHERE excluded.generation > deleted_contact_books.generation",
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

fn check_ids(book_id: &str, owner: &str) -> Result<(), Error> {
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
        assert_eq!(fence_generation(&conn, "b1").unwrap(), Some(1));
    }
}
