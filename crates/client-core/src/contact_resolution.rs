//! Bounded, durable address-to-contact lookup projection.

use crate::{Error, contacts};
use phonenumber::{Mode, country, parse};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

const BATCH: i64 = 200;
const MAX_DISPLAY_REFRESH_PAGES: usize = 64;
const MAX_ADDRESSES: usize = 100;
const MAX_ADDRESS_BYTES: usize = 256;
const ORIGINAL: i64 = 0;
const E164: i64 = 1;

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(MIGRATION)?;
    Ok(())
}

const MIGRATION: &str = "
CREATE TABLE IF NOT EXISTS contact_resolution_state(book_id TEXT PRIMARY KEY, book_digest TEXT NOT NULL, cursor TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS contact_resolution_projection(book_id TEXT NOT NULL, contact_id TEXT NOT NULL, revision INTEGER NOT NULL, PRIMARY KEY(book_id,contact_id));
CREATE TABLE IF NOT EXISTS contact_address_index(key TEXT NOT NULL, key_kind INTEGER NOT NULL, book_id TEXT NOT NULL, contact_id TEXT NOT NULL, PRIMARY KEY(key,key_kind,book_id,contact_id));
CREATE INDEX IF NOT EXISTS contact_address_index_contact ON contact_address_index(book_id,contact_id);
";

fn invalid() -> Error {
    Error::InvalidRequest("invalid contact resolution request")
}

pub(crate) fn region(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()?
        .get("region")?
        .as_str()
        .filter(|v| v.len() == 2 && v.bytes().all(|b| b.is_ascii_alphabetic()))
        .map(|v| v.to_ascii_uppercase())
}

/// Only values that affect address normalization belong in the invalidation fingerprint.
/// Book status, policy, and scan timestamps must not force a 20k-contact reindex.
fn normalization_fingerprint(body: &str) -> String {
    let value = serde_json::from_str::<Value>(body).unwrap_or(Value::Null);
    let schema = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .map_or_else(String::new, |value| value.to_string());
    format!("{schema}:{}", region(body).unwrap_or_default())
}

pub(crate) fn normalized(address: &str, region: Option<&str>) -> Option<String> {
    let address = address.trim();
    if address.is_empty()
        || !address.bytes().all(|byte| {
            byte.is_ascii_digit() || matches!(byte, b'+' | b' ' | b'(' | b')' | b'.' | b'-')
        })
    {
        return None;
    }
    let parsed = if address.starts_with('+') {
        parse(None, address).ok()?
    } else {
        let region = region?;
        let id = country::Id::from_str(region).ok()?;
        parse(Some(id), address).ok()?
    };
    if !parsed.is_valid() {
        return None;
    }
    Some(parsed.format().mode(Mode::E164).to_string())
}

struct Refresh {
    visible_ids: Vec<String>,
    regions: BTreeMap<String, Option<String>>,
}

fn page_sql(limit: i64) -> String {
    format!(
        "SELECT c.book_id,c.id,c.revision,c.body,c.deleted_at FROM contacts c \
           LEFT JOIN contact_resolution_projection p ON p.book_id=c.book_id AND p.contact_id=c.id \
         WHERE c.book_id=? AND c.id>? AND (p.revision IS NULL OR p.revision!=c.revision) \
         ORDER BY c.id LIMIT {limit}"
    )
}

fn prepare_refresh(conn: &mut Connection) -> Result<Refresh, Error> {
    let visible_book_ids = contacts::visible_book_ids(conn)?;
    let book_rows: Vec<(String, String, String, String)> = {
        let mut statement = conn.prepare(
            "SELECT id,owner_device_id,state,body FROM contact_books WHERE forgotten=0 ORDER BY id",
        )?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<_, _>>()?
    };
    let mut books = Vec::new();
    for (id, _owner, _state, body) in book_rows {
        if visible_book_ids.contains(&id) {
            books.push((id, body));
        }
    }
    for (book_id, body) in &books {
        let current = normalization_fingerprint(body);
        let previous: Option<String> = conn
            .query_row(
                "SELECT book_digest FROM contact_resolution_state WHERE book_id=?",
                [book_id],
                |row| row.get(0),
            )
            .optional()?;
        if previous.as_deref() != Some(&current) {
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM contact_address_index WHERE book_id=?",
                [book_id],
            )?;
            tx.execute(
                "DELETE FROM contact_resolution_projection WHERE book_id=?",
                [book_id],
            )?;
            tx.execute("INSERT INTO contact_resolution_state(book_id,book_digest,cursor) VALUES(?,?, '') ON CONFLICT(book_id) DO UPDATE SET book_digest=excluded.book_digest,cursor=''", params![book_id, current])?;
            tx.commit()?;
        }
    }
    let visible_ids = books.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>();
    let placeholders = std::iter::repeat_n("?", visible_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    if visible_ids.is_empty() {
        conn.execute("DELETE FROM contact_address_index", [])?;
        conn.execute("DELETE FROM contact_resolution_projection", [])?;
    } else {
        conn.execute(
            &format!("DELETE FROM contact_address_index WHERE book_id NOT IN ({placeholders})"),
            rusqlite::params_from_iter(&visible_ids),
        )?;
        conn.execute(
            &format!(
                "DELETE FROM contact_resolution_projection WHERE book_id NOT IN ({placeholders})"
            ),
            rusqlite::params_from_iter(&visible_ids),
        )?;
    }
    Ok(Refresh {
        visible_ids: books.iter().map(|(id, _)| id.clone()).collect(),
        regions: books
            .into_iter()
            .map(|(id, body)| (id, region(&body)))
            .collect(),
    })
}

fn refresh_page(
    conn: &mut Connection,
    refresh: &Refresh,
    after: Option<&(String, String)>,
) -> Result<(bool, Option<(String, String)>), Error> {
    if refresh.visible_ids.is_empty() {
        return Ok((false, None));
    }
    let mut rows = Vec::new();
    for book_id in &refresh.visible_ids {
        let cursor = match after {
            Some((after_book, _)) if book_id < after_book => continue,
            Some((after_book, contact_id)) if book_id == after_book => contact_id.as_str(),
            _ => "",
        };
        let remaining = BATCH as usize - rows.len();
        let mut statement = conn.prepare(&page_sql(remaining as i64))?;
        rows.extend(
            statement
                .query_map(params![book_id, cursor], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })?
                .collect::<Result<Vec<(String, String, i64, String, Option<i64>)>, _>>()?,
        );
        if rows.len() == BATCH as usize {
            break;
        }
    }
    let has_more = rows.len() == BATCH as usize;
    let next = rows
        .last()
        .map(|(book_id, contact_id, ..)| (book_id.clone(), contact_id.clone()));
    let tx = conn.transaction()?;
    for (book_id, contact_id, revision, body, deleted_at) in &rows {
        tx.execute(
            "DELETE FROM contact_address_index WHERE book_id=? AND contact_id=?",
            params![book_id, contact_id],
        )?;
        if deleted_at.is_none()
            && let Some(phones) = serde_json::from_str::<Value>(body)
                .ok()
                .and_then(|v| v.get("phones").cloned())
                .and_then(|v| v.as_array().cloned())
        {
            let book_region = refresh.regions.get(book_id).ok_or(Error::Database)?;
            for phone in phones {
                let Some(value) = phone
                    .get("value")
                    .and_then(Value::as_str)
                    .filter(|v| v.len() <= MAX_ADDRESS_BYTES)
                else {
                    continue;
                };
                tx.execute("INSERT OR IGNORE INTO contact_address_index(key,key_kind,book_id,contact_id) VALUES(?,?,?,?)", params![value, ORIGINAL, book_id, contact_id])?;
                if let Some(value) = normalized(value, book_region.as_deref()) {
                    tx.execute("INSERT OR IGNORE INTO contact_address_index(key,key_kind,book_id,contact_id) VALUES(?,?,?,?)", params![value, E164, book_id, contact_id])?;
                }
            }
        }
        tx.execute("INSERT INTO contact_resolution_projection(book_id,contact_id,revision) VALUES(?,?,?) ON CONFLICT(book_id,contact_id) DO UPDATE SET revision=excluded.revision", params![book_id, contact_id, revision])?;
    }
    tx.commit()?;
    Ok((has_more, next))
}

pub(crate) fn refresh(conn: &mut Connection) -> Result<bool, Error> {
    let prepared = prepare_refresh(conn)?;
    refresh_page(conn, &prepared, None).map(|(has_more, _)| has_more)
}

#[derive(Clone)]
struct Candidate {
    contact_id: String,
    book_id: String,
    display_name: String,
    photo_attachment_id: Option<String>,
    owner: String,
}

struct Book {
    owner: String,
    region: Option<String>,
}

fn books(conn: &Connection, visible_book_ids: &BTreeSet<String>) -> Result<Vec<Book>, Error> {
    let mut statement =
        conn.prepare("SELECT id,owner_device_id,state,body FROM contact_books WHERE forgotten=0")?;
    let rows = statement
        .query_map([], |row| {
            let body: String = row.get(3)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                body,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut books = Vec::new();
    for (id, owner, _state, body) in rows {
        if visible_book_ids.contains(&id) {
            books.push(Book {
                owner,
                region: region(&body),
            });
        }
    }
    Ok(books)
}

fn candidates(
    conn: &Connection,
    address: &str,
    source: Option<&str>,
    books: &[Book],
    visible_book_ids: &BTreeSet<String>,
) -> Result<Vec<Candidate>, Error> {
    let mut keys = BTreeSet::from([(ORIGINAL, address.to_owned())]);
    if address.starts_with('+') {
        if let Some(key) = normalized(address, None) {
            keys.insert((E164, key));
        }
    } else if let Some(source) = source {
        // A source phone's address book supplies the only allowed national context.
        let regions = books
            .iter()
            .filter(|book| book.owner == source)
            .filter_map(|book| book.region.clone())
            .collect::<BTreeSet<_>>();
        if regions.len() == 1
            && let Some(key) = normalized(address, regions.iter().next().map(String::as_str))
        {
            keys.insert((E164, key));
        }
    } else {
        // Without source context, national input is safe only when every interpretation
        // comes from one explicit shared region.
        let regions = books
            .iter()
            .filter_map(|book| book.region.clone())
            .collect::<BTreeSet<_>>();
        if regions.len() == 1
            && let Some(key) = normalized(address, regions.iter().next().map(String::as_str))
        {
            keys.insert((E164, key));
        }
    }
    let mut result = Vec::new();
    for (kind, key) in keys {
        let mut statement = conn.prepare(
            "SELECT DISTINCT c.id,c.book_id,c.body,b.owner_device_id FROM contact_address_index i \
             JOIN contacts c ON c.book_id=i.book_id AND c.id=i.contact_id AND c.deleted_at IS NULL \
             JOIN contact_books b ON b.id=c.book_id AND b.forgotten=0 \
             WHERE i.key_kind=? AND i.key=?",
        )?;
        let found = statement
            .query_map(params![kind, key], |row| {
                let body: String = row.get(2)?;
                let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                Ok(Candidate {
                    contact_id: row.get(0)?,
                    book_id: row.get(1)?,
                    display_name: value
                        .get("display_name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    photo_attachment_id: value
                        .pointer("/photo/attachment_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    owner: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for candidate in found {
            if visible_book_ids.contains(&candidate.book_id) {
                result.push(candidate);
            }
        }
    }
    result.sort_by(|a, b| (&a.book_id, &a.contact_id).cmp(&(&b.book_id, &b.contact_id)));
    result.dedup_by(|a, b| a.book_id == b.book_id && a.contact_id == b.contact_id);
    Ok(result)
}

fn choose(candidates: Vec<Candidate>, source: Option<&str>) -> Option<Candidate> {
    let mut books: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    for candidate in candidates {
        books
            .entry(candidate.book_id.clone())
            .or_default()
            .push(candidate);
    }
    // A shared address in one book is always ambiguous, even when another book also has it.
    if books.values().any(|items| items.len() != 1) {
        return None;
    }
    let unique = books
        .into_values()
        .map(|mut items| items.remove(0))
        .collect::<Vec<_>>();
    if let Some(source) = source {
        let source_books = unique
            .iter()
            .filter(|candidate| candidate.owner == source)
            .cloned()
            .collect::<Vec<_>>();
        if source_books.len() == 1 {
            return source_books.into_iter().next();
        }
        if source_books.len() > 1 {
            return None;
        }
    }
    // Contacts have no cross-book typed update timestamp. Generations are independent
    // counters, not recency, so remote contenders are deliberately left ambiguous.
    match unique.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

#[derive(Debug)]
pub(crate) struct AddressResolution {
    pub chosen_display_name: Option<String>,
    pub candidate_display_names: Vec<String>,
}

pub(crate) struct DisplayResolver<'a> {
    conn: &'a Connection,
    books: Vec<Book>,
    visible_book_ids: BTreeSet<String>,
}

impl DisplayResolver<'_> {
    pub(crate) fn resolve(
        &self,
        address: &str,
        source: Option<&str>,
    ) -> Result<AddressResolution, Error> {
        let candidates = candidates(
            self.conn,
            address,
            source,
            &self.books,
            &self.visible_book_ids,
        )?;
        let mut candidate_display_names = candidates
            .iter()
            .map(|candidate| candidate.display_name.clone())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        candidate_display_names.sort();
        candidate_display_names.dedup();
        let chosen_display_name = choose(candidates, source)
            .map(|candidate| candidate.display_name)
            .filter(|name| !name.is_empty());
        Ok(AddressResolution {
            chosen_display_name,
            candidate_display_names,
        })
    }
}

/// Creates a read-only resolver that shares contact-book metadata across an entire search.
pub(crate) fn display_resolver(conn: &Connection) -> Result<DisplayResolver<'_>, Error> {
    let visible_book_ids = contacts::visible_book_ids(conn)?;
    Ok(DisplayResolver {
        conn,
        books: books(conn, &visible_book_ids)?,
        visible_book_ids,
    })
}

/// Returns a resolver only after the visible contact index is current. If the bounded refresh
/// cannot finish, callers must fall back to structured identity rather than expose a stale name.
pub(crate) fn current_display_resolver(
    conn: &mut Connection,
) -> Result<Option<DisplayResolver<'_>>, Error> {
    let prepared = prepare_refresh(conn)?;
    let mut after = None;
    for _ in 0..MAX_DISPLAY_REFRESH_PAGES {
        let (has_more, next) = refresh_page(conn, &prepared, after.as_ref())?;
        if !has_more {
            return display_resolver(conn).map(Some);
        }
        after = next;
    }
    Ok(None)
}

pub(crate) fn resolve(conn: &mut Connection, input: &str) -> Result<String, Error> {
    let input: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    let addresses = input
        .get("addresses")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty() && items.len() <= MAX_ADDRESSES)
        .ok_or_else(invalid)?;
    let source = input
        .get("source_device_id")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty() && v.len() <= MAX_ADDRESS_BYTES);
    let addresses = addresses
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|v| !v.is_empty() && v.len() <= MAX_ADDRESS_BYTES)
                .map(str::to_owned)
                .ok_or_else(invalid)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let more_indexing = refresh(conn)?;
    let visible_book_ids = contacts::visible_book_ids(conn)?;
    let books = books(conn, &visible_book_ids)?;
    let mut matches = Vec::new();
    let mut ambiguous = Vec::new();
    for address in addresses {
        if more_indexing {
            ambiguous.push(json!(address));
            continue;
        }
        if let Some(candidate) = choose(
            candidates(conn, &address, source, &books, &visible_book_ids)?,
            source,
        ) {
            let mut item = json!({"address": address, "contact_id": candidate.contact_id, "book_id": candidate.book_id, "display_name": candidate.display_name});
            if let Some(photo_attachment_id) = candidate.photo_attachment_id {
                item["photo_attachment_id"] = json!(photo_attachment_id);
            }
            matches.push(item);
        } else {
            ambiguous.push(json!(address));
        }
    }
    Ok(
        json!({"matches": matches, "ambiguous": ambiguous, "more_indexing": more_indexing})
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::StatementStatus;

    fn page_steps(
        conn: &Connection,
        books: &[String],
        after: Option<(String, String)>,
    ) -> (i32, Vec<(String, String)>) {
        let mut steps = 0;
        let mut rows = Vec::new();
        for book_id in books {
            let cursor = match &after {
                Some((after_book, _)) if book_id < after_book => continue,
                Some((after_book, contact_id)) if book_id == after_book => contact_id.as_str(),
                _ => "",
            };
            let remaining = BATCH as usize - rows.len();
            let mut statement = conn.prepare(&page_sql(remaining as i64)).unwrap();
            rows.extend(
                statement
                    .query_map(params![book_id, cursor], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .unwrap()
                    .collect::<Result<Vec<(String, String)>, _>>()
                    .unwrap(),
            );
            steps += statement.get_status(StatementStatus::VmStep);
            if rows.len() == BATCH as usize {
                break;
            }
        }
        (steps, rows)
    }

    fn sweep_steps(contact_count: usize, book_count: usize) -> i32 {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE contacts(book_id TEXT NOT NULL,id TEXT NOT NULL,revision INTEGER NOT NULL,body TEXT NOT NULL,deleted_at INTEGER,PRIMARY KEY(book_id,id));
             CREATE TABLE contact_resolution_projection(book_id TEXT NOT NULL,contact_id TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(book_id,contact_id));",
        )
        .unwrap();
        let books = (0..book_count)
            .map(|index| format!("book-{index:02}"))
            .collect::<Vec<_>>();
        for index in 0..contact_count {
            conn.execute(
                "INSERT INTO contacts(book_id,id,revision,body,deleted_at) VALUES(?,?,1,'{}',NULL)",
                params![&books[index % book_count], format!("contact-{index:05}")],
            )
            .unwrap();
        }
        let mut after = None;
        let mut steps = 0;
        loop {
            let (page_steps, rows) = page_steps(&conn, &books, after);
            steps += page_steps;
            let Some((book_id, contact_id)) = rows.last().cloned() else {
                break;
            };
            for (book_id, contact_id) in rows {
                conn.execute(
                    "INSERT INTO contact_resolution_projection(book_id,contact_id,revision) VALUES(?,?,1)",
                    params![book_id, contact_id],
                )
                .unwrap();
            }
            after = Some((book_id, contact_id));
        }
        steps
    }

    #[test]
    fn keyset_page_seeks_and_sweep_work_scales_linearly() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE contacts(book_id TEXT NOT NULL,id TEXT NOT NULL,revision INTEGER NOT NULL,body TEXT NOT NULL,deleted_at INTEGER,PRIMARY KEY(book_id,id));
             CREATE TABLE contact_resolution_projection(book_id TEXT NOT NULL,contact_id TEXT NOT NULL,revision INTEGER NOT NULL,PRIMARY KEY(book_id,contact_id));",
        )
        .unwrap();
        let mut statement = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {}", page_sql(BATCH)))
            .unwrap();
        let plan = statement
            .query_map(params!["book", "contact"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join(" ");
        assert!(
            plan.contains("SEARCH c") && plan.contains("book_id") && plan.contains("id>"),
            "{plan}"
        );

        let single_2000 = sweep_steps(2000, 1);
        let single_4000 = sweep_steps(4000, 1);
        let multi_4000 = sweep_steps(4000, 4);
        assert!(
            single_4000 < single_2000 * 28 / 10,
            "{single_2000} -> {single_4000}"
        );
        assert!(
            multi_4000 < single_2000 * 28 / 10,
            "{single_2000} -> {multi_4000}"
        );
    }
}
