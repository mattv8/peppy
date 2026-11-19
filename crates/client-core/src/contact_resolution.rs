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
const MAX_NAME_PINS: i64 = 5000;
const ORIGINAL: i64 = 0;
const E164: i64 = 1;

pub(crate) fn initialize(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(MIGRATION)?;
    Ok(())
}

const MIGRATION: &str = "
CREATE TABLE IF NOT EXISTS contact_resolution_state(book_id TEXT PRIMARY KEY, book_digest TEXT NOT NULL, cursor TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS contact_name_pins(key TEXT PRIMARY KEY, book_id TEXT NOT NULL, contact_id TEXT NOT NULL);
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

fn lookup_keys(address: &str, source: Option<&str>, books: &[Book]) -> BTreeSet<(i64, String)> {
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
    keys
}

fn candidates(
    conn: &Connection,
    address: &str,
    source: Option<&str>,
    books: &[Book],
    visible_book_ids: &BTreeSet<String>,
) -> Result<Vec<Candidate>, Error> {
    let mut result = Vec::new();
    for (kind, key) in lookup_keys(address, source, books) {
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

fn valid_pin<'a>(
    conn: &Connection,
    keys: &BTreeSet<(i64, String)>,
    candidates: &'a [Candidate],
) -> Result<Option<&'a Candidate>, Error> {
    for (_, key) in keys.iter().rev() {
        let pin: Option<(String, String)> = conn
            .query_row(
                "SELECT book_id,contact_id FROM contact_name_pins WHERE key=?",
                [key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((book, contact)) = pin
            && let Ok(index) = candidates.binary_search_by(|candidate| {
                (&candidate.book_id, &candidate.contact_id).cmp(&(&book, &contact))
            })
        {
            return Ok(Some(&candidates[index]));
        }
    }
    Ok(None)
}

type NameFallback<'a> = (&'a str, &'static str, Option<&'a str>);

fn name_fallback<'a>(
    conn: &Connection,
    address: &str,
    source: Option<&str>,
    books: &[Book],
    candidates: &'a [Candidate],
) -> Result<Option<NameFallback<'a>>, Error> {
    if let Some(pin) = valid_pin(conn, &lookup_keys(address, source, books), candidates)? {
        return Ok((!pin.display_name.is_empty()).then_some((
            &pin.display_name,
            "pinned",
            pin.photo_attachment_id.as_deref(),
        )));
    }
    let names = candidates
        .iter()
        .map(|c| c.display_name.as_str())
        .filter(|name| !name.is_empty())
        .collect::<BTreeSet<_>>();
    if names.len() != 1 {
        return Ok(None);
    }
    let photo = candidates
        .first()
        .and_then(|c| c.photo_attachment_id.as_deref());
    let photo = photo.filter(|photo| {
        candidates
            .iter()
            .all(|c| c.photo_attachment_id.as_deref() == Some(*photo))
    });
    Ok(Some((
        names.into_iter().next().unwrap(),
        "consensus",
        photo,
    )))
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
        let chosen_display_name = if let Some(candidate) = choose(candidates.clone(), source) {
            (!candidate.display_name.is_empty()).then_some(candidate.display_name)
        } else {
            name_fallback(self.conn, address, source, &self.books, &candidates)?
                .map(|(name, _, _)| name.to_owned())
        };
        Ok(AddressResolution {
            chosen_display_name,
            candidate_display_names,
        })
    }
}

/// Creates a read-only resolver that shares contact-book metadata across an entire search.
pub(crate) fn display_resolver(conn: &Connection) -> Result<DisplayResolver<'_>, Error> {
    let visible_book_ids = contacts::lookup_book_ids(conn)?;
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
    let visible_book_ids = contacts::lookup_book_ids(conn)?;
    let books = books(conn, &visible_book_ids)?;
    let mut matches = Vec::new();
    let mut ambiguous = Vec::new();
    let mut name_matches = Vec::new();
    let mut name_conflicts = Vec::new();
    for address in addresses {
        if more_indexing {
            ambiguous.push(json!(address));
            continue;
        }
        let candidates = candidates(conn, &address, source, &books, &visible_book_ids)?;
        if let Some(candidate) = choose(candidates.clone(), source) {
            let mut item = json!({"address": address, "contact_id": candidate.contact_id, "book_id": candidate.book_id, "display_name": candidate.display_name});
            if let Some(photo_attachment_id) = candidate.photo_attachment_id {
                item["photo_attachment_id"] = json!(photo_attachment_id);
            }
            matches.push(item);
        } else {
            if let Some((name, basis, photo)) =
                name_fallback(conn, &address, source, &books, &candidates)?
            {
                let mut item = json!({"address": address, "display_name": name, "basis": basis});
                if let Some(photo) = photo {
                    item["photo_attachment_id"] = json!(photo);
                }
                name_matches.push(item);
            } else if valid_pin(conn, &lookup_keys(&address, source, &books), &candidates)?
                .is_none()
                && candidates
                    .iter()
                    .map(|c| c.display_name.as_str())
                    .filter(|name| !name.is_empty())
                    .collect::<BTreeSet<_>>()
                    .len()
                    >= 2
            {
                name_conflicts.push(json!(address));
            }
            ambiguous.push(json!(address));
        }
    }
    Ok(
        json!({"matches": matches, "ambiguous": ambiguous, "more_indexing": more_indexing, "name_matches": name_matches, "name_conflicts": name_conflicts})
            .to_string(),
    )
}

fn required<'a>(input: &'a Value, key: &str) -> Result<&'a str, Error> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= MAX_ADDRESS_BYTES)
        .ok_or_else(invalid)
}

fn source(input: &Value) -> Result<Option<&str>, Error> {
    match input.get("source_device_id") {
        None | Some(Value::Null) => Ok(None),
        Some(_) => required(input, "source_device_id").map(Some),
    }
}

pub(crate) fn name_candidates(conn: &mut Connection, input: &str) -> Result<String, Error> {
    let input: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    let address = required(&input, "address")?;
    let source = source(&input)?;
    let more_indexing = refresh(conn)?;
    let ids = contacts::lookup_book_ids(conn)?;
    let books = books(conn, &ids)?;
    let candidates = if more_indexing {
        Vec::new()
    } else {
        candidates(conn, address, source, &books, &ids)?
    };
    let pin = if choose(candidates.clone(), source).is_none() && !more_indexing {
        valid_pin(conn, &lookup_keys(address, source, &books), &candidates)?.map(
            |candidate| json!({"book_id": candidate.book_id, "contact_id": candidate.contact_id}),
        )
    } else {
        None
    };
    let items = candidates.iter().take(32).map(|candidate| json!({
        "book_id":candidate.book_id, "contact_id":candidate.contact_id, "display_name":candidate.display_name
    })).collect::<Vec<_>>();
    Ok(json!({"candidates":items, "pinned":pin, "more_indexing":more_indexing}).to_string())
}

pub(crate) fn set_pin(conn: &mut Connection, input: &str) -> Result<String, Error> {
    let input: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    let address = required(&input, "address")?;
    let source = source(&input)?;
    let book = required(&input, "book_id")?;
    let contact = required(&input, "contact_id")?;
    if refresh(conn)? {
        return Err(Error::NotFound);
    }
    let ids = contacts::lookup_book_ids(conn)?;
    let books = books(conn, &ids)?;
    let candidates = candidates(conn, address, source, &books, &ids)?;
    if candidates
        .binary_search_by(|candidate| {
            (candidate.book_id.as_str(), candidate.contact_id.as_str()).cmp(&(book, contact))
        })
        .is_err()
    {
        return Err(Error::NotFound);
    }
    let keys = lookup_keys(address, source, &books);
    let key = &keys.iter().next_back().ok_or_else(invalid)?.1;
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM contact_name_pins WHERE key=?)",
        [key],
        |row| row.get(0),
    )?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM contact_name_pins", [], |row| {
        row.get(0)
    })?;
    if !exists && count >= MAX_NAME_PINS {
        return Err(invalid());
    }
    conn.execute("INSERT INTO contact_name_pins(key,book_id,contact_id) VALUES(?,?,?) ON CONFLICT(key) DO UPDATE SET book_id=excluded.book_id,contact_id=excluded.contact_id", params![key, book, contact])?;
    Ok(json!({"status":"pinned"}).to_string())
}

pub(crate) fn clear_pin(conn: &Connection, input: &str) -> Result<String, Error> {
    let input: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    let address = required(&input, "address")?;
    let source = source(&input)?;
    let ids = contacts::lookup_book_ids(conn)?;
    let books = books(conn, &ids)?;
    for (_, key) in lookup_keys(address, source, &books) {
        conn.execute("DELETE FROM contact_name_pins WHERE key=?", [key])?;
    }
    Ok(json!({"status":"cleared"}).to_string())
}

pub(crate) fn set_book_lookup(conn: &Connection, input: &str) -> Result<String, Error> {
    let input: Value = serde_json::from_str(input).map_err(|_| invalid())?;
    let book = required(&input, "book_id")?;
    let enabled = input
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(invalid)?;
    if enabled {
        conn.execute(
            "DELETE FROM contact_book_lookup_exclusions WHERE book_id=?",
            [book],
        )?;
    } else {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM contact_books WHERE id=? AND forgotten=0)",
            [book],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(Error::NotFound);
        }
        conn.execute(
            "INSERT OR IGNORE INTO contact_book_lookup_exclusions(book_id) VALUES(?)",
            [book],
        )?;
    }
    Ok(json!({"status":"ok","book_id":book,"enabled":enabled}).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::StatementStatus;

    #[test]
    fn consensus_photo_requires_identical_present_ids() {
        let conn = Connection::open_in_memory().unwrap();
        initialize(&conn).unwrap();
        let candidate = Candidate {
            contact_id: "one".into(),
            book_id: "book".into(),
            display_name: "Alex".into(),
            photo_attachment_id: Some("photo".into()),
            owner: "owner".into(),
        };
        let mut candidates = vec![candidate.clone(), candidate];
        assert_eq!(
            name_fallback(&conn, "123", None, &[], &candidates).unwrap(),
            Some(("Alex", "consensus", Some("photo")))
        );
        candidates[1].photo_attachment_id = Some("other".into());
        assert_eq!(
            name_fallback(&conn, "123", None, &[], &candidates).unwrap(),
            Some(("Alex", "consensus", None))
        );
        candidates[1].photo_attachment_id = None;
        assert_eq!(
            name_fallback(&conn, "123", None, &[], &candidates).unwrap(),
            Some(("Alex", "consensus", None))
        );
    }

    #[test]
    fn pin_cap_allows_replacement_but_not_new_keys() {
        let mut conn = Connection::open_in_memory().unwrap();
        contacts::initialize(&conn).unwrap();
        initialize(&conn).unwrap();
        conn.execute(
            "INSERT INTO contact_books(id,owner_device_id,generation,state,body) VALUES('book','owner',1,'active',?)",
            [r#"{"region":"US"}"#],
        ).unwrap();
        for (id, phone) in [("one", "+12025550100"), ("two", "+12025550101")] {
            let body = json!({"display_name":"Alex","phones":[{"value":phone}]}).to_string();
            conn.execute(
                "INSERT INTO contacts(id,book_id,revision,body) VALUES(?,'book',1,?)",
                params![id, body],
            )
            .unwrap();
        }
        conn.execute_batch(
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<4999)
             INSERT INTO contact_name_pins(key,book_id,contact_id) SELECT 'key-'||x,'book','one' FROM n;
             INSERT INTO contact_name_pins(key,book_id,contact_id) VALUES('+12025550100','book','one');",
        ).unwrap();
        let first =
            json!({"address":"+12025550100","book_id":"book","contact_id":"one"}).to_string();
        let second =
            json!({"address":"+12025550101","book_id":"book","contact_id":"two"}).to_string();
        assert!(set_pin(&mut conn, &first).is_ok());
        assert!(matches!(
            set_pin(&mut conn, &second),
            Err(Error::InvalidRequest(_))
        ));
    }

    #[test]
    fn display_resolver_uses_consensus_without_selecting_identity() {
        let mut conn = Connection::open_in_memory().unwrap();
        contacts::initialize(&conn).unwrap();
        initialize(&conn).unwrap();
        conn.execute(
            "INSERT INTO contact_books(id,owner_device_id,generation,state,body) VALUES('book','owner',1,'active',?)",
            [r#"{"region":"US"}"#],
        ).unwrap();
        for id in ["one", "two"] {
            conn.execute(
                "INSERT INTO contacts(id,book_id,revision,body) VALUES(?,'book',1,?)",
                params![
                    id,
                    r#"{"display_name":"Alex","phones":[{"value":"+12025550100"}]}"#
                ],
            )
            .unwrap();
        }
        assert!(!refresh(&mut conn).unwrap());
        let resolved = display_resolver(&conn)
            .unwrap()
            .resolve("+12025550100", None)
            .unwrap();
        assert_eq!(resolved.chosen_display_name.as_deref(), Some("Alex"));
        assert_eq!(resolved.candidate_display_names, vec!["Alex"]);
    }

    #[test]
    fn incomplete_refresh_withholds_pinned_names() {
        let mut conn = Connection::open_in_memory().unwrap();
        contacts::initialize(&conn).unwrap();
        initialize(&conn).unwrap();
        conn.execute(
            "INSERT INTO contact_books(id,owner_device_id,generation,state,body) VALUES('book','owner',1,'active',?)",
            [r#"{"region":"US"}"#],
        ).unwrap();
        for number in 0..201 {
            conn.execute(
                "INSERT INTO contacts(id,book_id,revision,body) VALUES(?,'book',1,?)",
                params![
                    format!("c{number:03}"),
                    r#"{"display_name":"Alex","phones":[{"value":"+12025550100"}]}"#
                ],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO contact_name_pins(key,book_id,contact_id) VALUES('+12025550100','book','c000')",
            [],
        ).unwrap();
        let result: Value = serde_json::from_str(
            &resolve(
                &mut conn,
                &json!({"addresses":["+12025550100"]}).to_string(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["more_indexing"], true);
        assert_eq!(result["matches"], json!([]));
        assert_eq!(result["name_matches"], json!([]));
        assert_eq!(result["name_conflicts"], json!([]));
        let candidates: Value = serde_json::from_str(
            &name_candidates(&mut conn, &json!({"address":"+12025550100"}).to_string()).unwrap(),
        )
        .unwrap();
        assert_eq!(candidates["more_indexing"], false);
        assert_eq!(candidates["candidates"].as_array().unwrap().len(), 32);
        assert_eq!(candidates["candidates"][0]["contact_id"], "c000");
        assert_eq!(candidates["pinned"]["contact_id"], "c000");
    }

    #[test]
    fn pin_validity_tracks_deleted_and_forgotten_candidates() {
        let mut conn = Connection::open_in_memory().unwrap();
        contacts::initialize(&conn).unwrap();
        initialize(&conn).unwrap();
        for id in ["one", "two"] {
            conn.execute(
                "INSERT INTO contact_books(id,owner_device_id,generation,state,body) VALUES(?,?,1,'active',?)",
                params![id, id, r#"{"region":"US"}"#],
            ).unwrap();
            conn.execute(
                "INSERT INTO contacts(id,book_id,revision,body) VALUES('contact',?,1,?)",
                params![
                    id,
                    r#"{"display_name":"Alex","phones":[{"value":"+12025550100"}]}"#
                ],
            )
            .unwrap();
        }
        assert!(!refresh(&mut conn).unwrap());
        conn.execute(
            "INSERT INTO contact_name_pins(key,book_id,contact_id) VALUES('+12025550100','one','contact')",
            [],
        ).unwrap();
        let pinned = |conn: &Connection| {
            let ids = contacts::lookup_book_ids(conn).unwrap();
            let books = books(conn, &ids).unwrap();
            let candidates = candidates(conn, "+12025550100", None, &books, &ids).unwrap();
            valid_pin(
                conn,
                &lookup_keys("+12025550100", None, &books),
                &candidates,
            )
            .unwrap()
            .is_some()
        };
        assert!(pinned(&conn));
        conn.execute("UPDATE contacts SET deleted_at=1 WHERE book_id='one'", [])
            .unwrap();
        assert!(!pinned(&conn));
        conn.execute(
            "UPDATE contacts SET deleted_at=NULL WHERE book_id='one'",
            [],
        )
        .unwrap();
        conn.execute("UPDATE contact_books SET forgotten=1 WHERE id='one'", [])
            .unwrap();
        assert!(!pinned(&conn));
        conn.execute("UPDATE contact_books SET forgotten=0 WHERE id='one'", [])
            .unwrap();
        assert!(pinned(&conn));
    }

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
