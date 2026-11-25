//! Contact identities of an inherited book.
//!
//! A re-enrolled phone inherits its predecessor's contact book, but not the predecessor's
//! owner-local source mapping (`contact_source`): provider ids never leave a device. Without
//! that mapping the successor's takeover scan would mint a second identity for every contact
//! the predecessor already published, and the deletion policy would hold the removal of the
//! first one. This module recognises the predecessor's contacts by content instead
//! (`contact_identity`):
//! - `adopt`: while the takeover is open, an unmapped provider contact takes over the identity
//!   of the predecessor contact with the same content.
//! - `reconcile`: the first complete authoritative scan merges what adoption did not prevent.
//!   A predecessor contact that an observed contact covers is removed and linked to it, without
//!   the deletion policy's approval: nothing the phone holds disappears. Predecessor contacts
//!   that an approved scan hold already removed are linked the same way. Any other removed
//!   contact is left alone: it may be a contact of its own that the user deleted.
//! - `reopen_unreconciled`: a takeover that an earlier build completed with both identities in
//!   place is opened once more, so the host runs that scan again.
//!
//! A predecessor contact is a contact of the book without a source mapping on this device. Every
//! contact the successor itself observed or wrote has one.
use crate::contact_identity::{self, Content};
use crate::{
    Ctx, Error, MAX_SEAL_BATCH, contact_queries, contact_resolution, contacts, initial_replay,
    seal_pending,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

const LISTS: [&str; 3] = ["phones", "emails", "addresses"];
/// Condition on a contact row aliased `c`: this device has no provider mapping for it.
const UNMAPPED: &str = "NOT EXISTS(SELECT 1 FROM contact_source_map s WHERE s.book_id=c.book_id AND s.contact_id=c.id)";
/// Condition on a contact row aliased `c`: an approved scan hold of its book removed it, it is
/// unchanged since and still offered for restore. `?2` is the restore cutoff. A scan holds
/// removals only when many contacts went missing at once, which is what a takeover that minted a
/// second identity for every contact produced.
const REMOVED_BY_HOLD: &str = "c.deleted_at>?2 AND json_type(c.body,'$.restored_to') IS NULL AND EXISTS(SELECT 1 FROM contact_scan_holds h, json_each(h.members) m WHERE h.book_id=c.book_id AND h.status='applied' AND json_extract(m.value,'$.id')=c.id AND CAST(json_extract(m.value,'$.revision') AS INTEGER)+1=c.revision)";

/// What the reconciling scan did with the predecessor's contacts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reconciled {
    /// Live predecessor contacts removed and linked to the observed contact that covers them.
    pub merged: usize,
    /// Predecessor contacts an approved hold had removed, linked to the observed contact that
    /// covers them.
    pub relinked: usize,
}

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    // Owner-local, like the source mapping: the predecessor contacts of an open takeover that
    // no provider contact has claimed yet, by content key. Never projected or enqueued.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS contact_takeover_candidates(book_id TEXT NOT NULL, contact_id TEXT NOT NULL, revision INTEGER NOT NULL, exact_key TEXT NOT NULL, identity_key TEXT, PRIMARY KEY(book_id,contact_id));
         CREATE INDEX IF NOT EXISTS contact_takeover_candidates_exact ON contact_takeover_candidates(book_id,exact_key);
         CREATE INDEX IF NOT EXISTS contact_takeover_candidates_identity ON contact_takeover_candidates(book_id,identity_key);
         CREATE TABLE IF NOT EXISTS contact_takeover_indexed(book_id TEXT PRIMARY KEY);",
    )?;
    Ok(())
}

/// Maps an unmapped `source_key` to the predecessor contact that holds the content of `fields`,
/// when `book_id` is an inherited book whose takeover is open. The caller has checked that this
/// device owns the book. Without a match nothing is mapped and the caller mints an identity.
///
/// Equal content matches first. Otherwise the match must be the only unclaimed predecessor
/// contact with the same name, phone numbers and emails, and `fields` must cover it, so that
/// treating the provider contact as its newer version discards nothing.
pub(crate) fn adopt(
    tx: &Connection,
    book_id: &str,
    source_key: &str,
    fields: &Value,
) -> Result<(), Error> {
    if !takeover_open(tx, book_id)? || mapped(tx, book_id, source_key)? {
        return Ok(());
    }
    let region = book_region(tx, book_id)?;
    index_candidates(tx, book_id, region.as_deref())?;
    let incoming = Content::of(fields, region.as_deref());
    let Some((contact_id, stored)) = matching_candidate(tx, book_id, &incoming, region.as_deref())?
    else {
        return Ok(());
    };
    tx.execute(
        "INSERT INTO contact_source_map(book_id,source_key,contact_id) VALUES(?,?,?)",
        params![book_id, source_key, contact_id],
    )?;
    tx.execute(
        "DELETE FROM contact_takeover_candidates WHERE book_id=? AND contact_id=?",
        params![book_id, contact_id],
    )?;
    adopt_item_ids(tx, book_id, &contact_id, fields, &stored)
}

/// Merges the predecessor's copies of contacts that scan `scan_id` observed. Runs once per
/// inherited book, in the transaction of its first complete authoritative scan. Merged ids
/// leave `missing`, so the deletion policy only judges contacts the phone no longer holds.
pub(crate) fn reconcile(
    tx: &Connection,
    ctx: &Ctx<'_>,
    book_id: &str,
    scan_id: &str,
    missing: &mut Vec<String>,
) -> Result<Reconciled, Error> {
    let pending: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM inherited_contact_books WHERE book_id=? AND reconciled=0)",
        [book_id],
        |row| row.get(0),
    )?;
    if !pending {
        return Ok(Reconciled::default());
    }
    let region = book_region(tx, book_id)?;
    let region = region.as_deref();
    let unobserved: HashSet<&str> = missing.iter().map(String::as_str).collect();
    let mut live = contents(
        tx,
        &format!(
            "SELECT c.id,c.body FROM contacts c WHERE c.book_id=?1 AND c.deleted_at IS NULL AND {UNMAPPED}"
        ),
        params![book_id],
        region,
    )?;
    live.retain(|id, _| unobserved.contains(id.as_str()));
    let removed = contents(
        tx,
        &format!(
            "SELECT c.id,c.body FROM contacts c WHERE c.book_id=?1 AND {UNMAPPED} AND {REMOVED_BY_HOLD}"
        ),
        params![book_id, restore_cutoff()],
        region,
    )?;
    let mut reconciled = Reconciled::default();
    if !(live.is_empty() && removed.is_empty()) {
        // Only a contact this scan observed can take a predecessor contact's place.
        let mut observed = contents(
            tx,
            "SELECT c.id,c.body FROM contacts c JOIN contact_scan_members m ON m.scan_id=?2 AND m.contact_id=c.id WHERE c.book_id=?1 AND c.deleted_at IS NULL AND EXISTS(SELECT 1 FROM contact_source_map s WHERE s.book_id=c.book_id AND s.contact_id=c.id)",
            params![book_id, scan_id],
            region,
        )?;
        let deleted_at = contacts::now();
        let mut merged = HashSet::new();
        for (predecessor, kept) in contact_identity::pair(&live, &observed) {
            contacts::tombstone_owned_merged_unsealed(
                tx,
                ctx,
                book_id,
                &predecessor,
                &kept,
                deleted_at,
            )?;
            observed.remove(&kept);
            merged.insert(predecessor);
        }
        for (predecessor, kept) in contact_identity::pair(&removed, &observed) {
            contacts::mark_merged(tx, ctx, book_id, &predecessor, &kept)?;
            reconciled.relinked += 1;
        }
        reconciled.merged = merged.len();
        missing.retain(|id| !merged.contains(id));
        while seal_pending(tx, ctx, MAX_SEAL_BATCH)? == MAX_SEAL_BATCH {}
    }
    tx.execute(
        "UPDATE inherited_contact_books SET reconciled=1 WHERE book_id=?",
        [book_id],
    )?;
    forget_candidates(tx, book_id)?;
    Ok(reconciled)
}

/// Opens the takeover of each inherited book of this device again when an earlier build
/// completed it without reconciling and predecessor contacts remain. The host then selects the
/// book as inherited and runs the takeover full scan, which reconciles it. A book without
/// leftovers is marked reconciled, so each book is opened at most once. Returns the opened books.
pub(crate) fn reopen_unreconciled(tx: &Connection, ctx: &Ctx<'_>) -> Result<Vec<String>, Error> {
    if !initial_replay::caught_up(tx)? {
        return Ok(Vec::new());
    }
    let unreconciled: Vec<String> = tx
        .prepare("SELECT i.book_id FROM inherited_contact_books i JOIN contact_books b ON b.id=i.book_id WHERE b.owner_device_id=? AND b.forgotten=0 AND b.state!='retired' AND i.takeover_complete=1 AND i.reconciled=0 ORDER BY i.book_id")?
        .query_map([ctx.device_id.to_string()], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let mut reopened = Vec::new();
    for book_id in unreconciled {
        let leftovers: bool = tx.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM contacts c WHERE c.book_id=?1 AND {UNMAPPED} AND (c.deleted_at IS NULL OR ({REMOVED_BY_HOLD})))"),
            params![book_id, restore_cutoff()],
            |row| row.get(0),
        )?;
        if leftovers {
            tx.execute(
                "UPDATE inherited_contact_books SET takeover_complete=0 WHERE book_id=?",
                [&book_id],
            )?;
            forget_candidates(tx, &book_id)?;
            reopened.push(book_id);
        } else {
            tx.execute(
                "UPDATE inherited_contact_books SET reconciled=1 WHERE book_id=?",
                [&book_id],
            )?;
        }
    }
    Ok(reopened)
}

fn takeover_open(tx: &Connection, book_id: &str) -> Result<bool, Error> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM inherited_contact_books WHERE book_id=? AND takeover_complete=0)",
        [book_id],
        |row| row.get(0),
    )?)
}

fn mapped(tx: &Connection, book_id: &str, source_key: &str) -> Result<bool, Error> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM contact_source_map WHERE book_id=? AND source_key=?)",
        params![book_id, source_key],
        |row| row.get(0),
    )?)
}

fn book_region(tx: &Connection, book_id: &str) -> Result<Option<String>, Error> {
    let body: Option<String> = tx
        .query_row(
            "SELECT body FROM contact_books WHERE id=?",
            [book_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(body.as_deref().and_then(contact_resolution::region))
}

/// Tombstones deleted before this time are past their restore window.
fn restore_cutoff() -> i64 {
    contacts::now() - contact_queries::TOMBSTONE_TTL_SECONDS
}

fn contents(
    tx: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
    region: Option<&str>,
) -> Result<BTreeMap<String, Content>, Error> {
    let rows: Vec<(String, String)> = tx
        .prepare(sql)?
        .query_map(params, |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    rows.into_iter()
        .map(|(id, body)| Ok((id, Content::of(&parsed(&body)?, region))))
        .collect()
}

fn parsed(body: &str) -> Result<Value, Error> {
    serde_json::from_str(body).map_err(|_| Error::Database)
}

/// Indexes the book's predecessor contacts once per takeover. Replay has caught up before a
/// host scans, and a succeeded predecessor publishes nothing more, so the set is complete.
fn index_candidates(tx: &Connection, book_id: &str, region: Option<&str>) -> Result<(), Error> {
    let indexed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM contact_takeover_indexed WHERE book_id=?)",
        [book_id],
        |row| row.get(0),
    )?;
    if indexed {
        return Ok(());
    }
    let predecessors: Vec<(String, i64, String)> = tx
        .prepare(&format!("SELECT c.id,c.revision,c.body FROM contacts c WHERE c.book_id=? AND c.deleted_at IS NULL AND {UNMAPPED}"))?
        .query_map([book_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<Result<_, _>>()?;
    for (contact_id, revision, body) in predecessors {
        let content = Content::of(&parsed(&body)?, region);
        tx.execute(
            "INSERT OR REPLACE INTO contact_takeover_candidates(book_id,contact_id,revision,exact_key,identity_key) VALUES(?,?,?,?,?)",
            params![book_id, contact_id, revision, content.exact, content.identity],
        )?;
    }
    tx.execute(
        "INSERT INTO contact_takeover_indexed(book_id) VALUES(?)",
        [book_id],
    )?;
    Ok(())
}

fn forget_candidates(tx: &Connection, book_id: &str) -> Result<(), Error> {
    tx.execute(
        "DELETE FROM contact_takeover_candidates WHERE book_id=?",
        [book_id],
    )?;
    tx.execute(
        "DELETE FROM contact_takeover_indexed WHERE book_id=?",
        [book_id],
    )?;
    Ok(())
}

/// The predecessor contact `incoming` continues, with its stored body.
fn matching_candidate(
    tx: &Connection,
    book_id: &str,
    incoming: &Content,
    region: Option<&str>,
) -> Result<Option<(String, Value)>, Error> {
    let equal = current_candidates(
        tx,
        book_id,
        "SELECT contact_id FROM contact_takeover_candidates WHERE book_id=? AND exact_key=? ORDER BY contact_id",
        &incoming.exact,
    )?;
    if let Some(first) = equal.into_iter().next() {
        return Ok(Some(first));
    }
    let Some(identity) = &incoming.identity else {
        return Ok(None);
    };
    let mut alike = current_candidates(
        tx,
        book_id,
        "SELECT contact_id FROM contact_takeover_candidates WHERE book_id=? AND identity_key=? ORDER BY contact_id",
        identity,
    )?;
    let only = match alike.len() {
        1 => alike.remove(0),
        _ => return Ok(None),
    };
    Ok(incoming
        .covers(&Content::of(&only.1, region))
        .then_some(only))
}

/// The indexed candidates with `key` that are still unclaimed predecessor contacts at their
/// indexed revision. A candidate that changed since is dropped from the index.
fn current_candidates(
    tx: &Connection,
    book_id: &str,
    candidates_sql: &str,
    key: &str,
) -> Result<Vec<(String, Value)>, Error> {
    let ids: Vec<String> = tx
        .prepare(candidates_sql)?
        .query_map(params![book_id, key], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let mut current = Vec::new();
    for contact_id in ids {
        let body: Option<String> = tx
            .query_row(
                &format!("SELECT c.body FROM contacts c JOIN contact_takeover_candidates k ON k.book_id=c.book_id AND k.contact_id=c.id AND k.revision=c.revision WHERE c.book_id=? AND c.id=? AND c.deleted_at IS NULL AND {UNMAPPED}"),
                params![book_id, contact_id],
                |row| row.get(0),
            )
            .optional()?;
        match body {
            Some(body) => current.push((contact_id, parsed(&body)?)),
            None => {
                tx.execute(
                    "DELETE FROM contact_takeover_candidates WHERE book_id=? AND contact_id=?",
                    params![book_id, contact_id],
                )?;
            }
        }
    }
    Ok(current)
}

/// Binds each provider list item to the stored item with the same content, before the source
/// mapping mints item ids. An unchanged contact then keeps its digest, so it is not republished.
fn adopt_item_ids(
    tx: &Connection,
    book_id: &str,
    contact_id: &str,
    fields: &Value,
    stored: &Value,
) -> Result<(), Error> {
    for list in LISTS {
        let mut unbound: Vec<(String, &str)> = items(stored, list)
            .filter_map(|item| Some((contact_identity::item_key(item), item.get("id")?.as_str()?)))
            .collect();
        for item in items(fields, list) {
            let Some(provider_item_id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            let key = contact_identity::item_key(item);
            let Some(position) = unbound
                .iter()
                .position(|(stored_key, _)| *stored_key == key)
            else {
                continue;
            };
            let (_, field_id) = unbound.remove(position);
            tx.execute(
                "INSERT OR IGNORE INTO contact_source_field_map(book_id,contact_id,field,provider_item_id,field_id) VALUES(?,?,?,?,?)",
                params![book_id, contact_id, list, provider_item_id, field_id],
            )?;
        }
    }
    Ok(())
}

fn items<'a>(contact: &'a Value, list: &str) -> impl Iterator<Item = &'a Value> {
    contact
        .get(list)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contact_source::{Adoption, platform_contact};
    use serde_json::json;

    const BOOK: &str = "inherited-book";
    const PREDECESSOR_CONTACT: &str = "predecessor-contact";

    /// An inherited book with one predecessor contact and an open takeover.
    fn inherited() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE metadata(k TEXT PRIMARY KEY, v TEXT NOT NULL);")
            .unwrap();
        contacts::initialize(&conn).unwrap();
        crate::contact_source::initialize(&conn).unwrap();
        initialize(&conn).unwrap();
        let stored = json!({
            "id": PREDECESSOR_CONTACT, "book_id": BOOK, "revision": "1", "digest": "d",
            "display_name": "Ada", "name": {"given": "Ada"},
            "phones": [{"id": "stored-phone", "label": "mobile", "value": "+12025550100"}],
            "emails": [], "addresses": []
        });
        conn.execute(
            "INSERT INTO contact_books(id,owner_device_id,generation,state,body) VALUES(?,'successor',1,'active',?)",
            params![BOOK, json!({"id": BOOK, "region": "US"}).to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO contacts(id,book_id,revision,body) VALUES(?,?,1,?)",
            params![PREDECESSOR_CONTACT, BOOK, stored.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO inherited_contact_books(book_id,predecessor) VALUES(?,'predecessor')",
            [BOOK],
        )
        .unwrap();
        conn
    }

    fn entry() -> Value {
        json!({"source_key": "raw:7", "fields": {
            "display_name": "Ada", "name": {"given": "Ada"},
            "phones": [{"id": "provider-phone", "label": "mobile", "value": "+12025550100"}],
            "emails": [], "addresses": []
        }})
    }

    fn mapped_id(conn: &Connection, adoption: Adoption) -> String {
        let contact = platform_contact(conn, BOOK, &entry(), adoption).unwrap().1;
        contact["id"].as_str().unwrap().to_owned()
    }

    #[test]
    fn an_observed_provider_contact_adopts_the_predecessor_identity_and_item_ids() {
        let conn = inherited();
        let contact = platform_contact(&conn, BOOK, &entry(), Adoption::Allow)
            .unwrap()
            .1;
        assert_eq!(contact["id"], PREDECESSOR_CONTACT);
        assert_eq!(contact["phones"][0]["id"], "stored-phone");
    }

    #[test]
    fn the_outcome_of_a_requested_write_never_adopts() {
        let conn = inherited();
        assert_ne!(mapped_id(&conn, Adoption::Never), PREDECESSOR_CONTACT);
    }

    #[test]
    fn nothing_is_adopted_once_the_takeover_is_complete() {
        let conn = inherited();
        conn.execute("UPDATE inherited_contact_books SET takeover_complete=1", [])
            .unwrap();
        assert_ne!(mapped_id(&conn, Adoption::Allow), PREDECESSOR_CONTACT);
    }

    #[test]
    fn a_predecessor_contact_that_changed_after_indexing_is_not_adopted() {
        let conn = inherited();
        index_candidates(&conn, BOOK, Some("US")).unwrap();
        conn.execute("UPDATE contacts SET revision=2", []).unwrap();
        assert_ne!(mapped_id(&conn, Adoption::Allow), PREDECESSOR_CONTACT);
    }

    #[test]
    fn a_book_that_was_not_inherited_never_adopts() {
        let conn = inherited();
        conn.execute("DELETE FROM inherited_contact_books", [])
            .unwrap();
        assert_ne!(mapped_id(&conn, Adoption::Allow), PREDECESSOR_CONTACT);
    }
}
