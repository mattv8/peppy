use crate::{Client, Error, Message, Store, message_from_row, message_row};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use std::fmt::Write;

pub const DEFAULT_SEARCH_LIMIT: usize = 6;
pub const MAX_SEARCH_LIMIT: usize = 50;
pub const MAX_SEARCH_QUERY_CHARS: usize = 128;
pub const HISTORY_WINDOW_LIMIT: usize = 50;
pub const HISTORY_REFRESH_LIMIT: usize = 150;
const INDEX_BATCH_LIMIT: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKind {
    All,
    Conversation,
    Message,
    Contact,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalSearchRequest {
    pub query: String,
    pub kind: Option<SearchKind>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalSearchHit {
    Conversation {
        conversation_id: String,
        title: String,
        subtitle: String,
    },
    Message {
        conversation_id: String,
        message_id: String,
        title: String,
        snippet: String,
        timestamp: String,
    },
    Contact {
        book_id: String,
        contact_id: String,
        title: String,
        subtitle: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchIndexState {
    Ready,
    Indexing,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHasMore {
    pub conversation: bool,
    pub message: bool,
    pub contact: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalSearchPage {
    pub query: String,
    pub items: Vec<LocalSearchHit>,
    pub next_cursor: Option<String>,
    pub index_state: SearchIndexState,
    pub has_more: SearchHasMore,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchIndexProgress {
    pub index_state: SearchIndexState,
    pub processed: usize,
    pub remaining: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageWindowRequest {
    Around {
        conversation_id: String,
        message_id: String,
    },
    Older {
        conversation_id: String,
        message_id: String,
    },
    Newer {
        conversation_id: String,
        message_id: String,
    },
    Refresh {
        conversation_id: String,
        message_ids: Vec<String>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageWindow {
    pub conversation_id: String,
    pub messages: Vec<Message>,
    pub has_older: bool,
    pub has_newer: bool,
}

impl Client {
    pub fn search_local(&self, request: LocalSearchRequest) -> Result<LocalSearchPage, Error> {
        let store = self.lock()?;
        search(&store, request)
    }
    /// The only standalone indexing operation; it indexes at most 100 rows.
    pub fn advance_search_index(
        &self,
        budget_rows: Option<usize>,
    ) -> Result<SearchIndexProgress, Error> {
        let budget = budget_rows.unwrap_or(INDEX_BATCH_LIMIT);
        if !(1..=INDEX_BATCH_LIMIT).contains(&budget) {
            return Err(Error::InvalidRequest("search index batch size"));
        }
        let mut store = self.lock()?;
        if store.keys.is_empty() {
            return Err(Error::KeysUnavailable);
        }
        let tx = store.conn.transaction()?;
        let highwater = metadata_i64(&tx, "search_backfill_highwater")?;
        let watermark = metadata_i64(&tx, "search_backfill_watermark")?;
        let rows = tx
            .prepare("SELECT local_order,id,conversation_id,payload,sort_at_ms FROM messages WHERE local_order>? AND local_order<=? ORDER BY local_order LIMIT ?")?
            .query_map(params![watermark, highwater, budget as i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, i64>(4)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut inserted = 0_i64;
        for (order, id, conversation_id, payload, timestamp) in &rows {
            let already_indexed: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM local_search_fts WHERE rowid=?)",
                [order],
                |row| row.get(0),
            )?;
            if already_indexed {
                continue;
            }
            // Legacy rows can contain malformed canonical JSON. Preserve the row while
            // indexing it as empty so one bad payload cannot block the backfill watermark.
            let payload = serde_json::from_str(payload).unwrap_or(Value::Null);
            tx.execute("INSERT INTO local_search_fts(rowid,kind,target_id,conversation_id,title,body,timestamp) VALUES(?,?,?,?,?,?,?)", params![order, "message", id, conversation_id, payload.get("subject").and_then(Value::as_str).unwrap_or(""), payload.get("body").and_then(Value::as_str).unwrap_or(""), timestamp])?;
            inserted += 1;
        }
        let next_watermark = rows.last().map(|row| row.0).unwrap_or(highwater);
        if next_watermark != watermark {
            set_metadata_i64(&tx, "search_backfill_watermark", next_watermark)?;
        }
        let remaining: i64 = tx.query_row(
            "SELECT COUNT(*) FROM messages WHERE local_order>? AND local_order<=?",
            params![next_watermark, highwater],
            |row| row.get(0),
        )?;
        if metadata_i64(&tx, "search_remaining")? != remaining {
            set_metadata_i64(&tx, "search_remaining", remaining)?;
        }
        if inserted > 0 {
            tx.execute(
                "UPDATE metadata SET v=CAST(v AS INTEGER)+1 WHERE k='search_generation'",
                [],
            )?;
        }
        tx.commit()?;
        Ok(SearchIndexProgress {
            index_state: if remaining == 0 {
                SearchIndexState::Ready
            } else {
                SearchIndexState::Indexing
            },
            processed: rows.len(),
            remaining: usize::try_from(remaining).map_err(|_| Error::Database)?,
        })
    }
    pub fn load_message_window(
        &self,
        request: MessageWindowRequest,
    ) -> Result<MessageWindow, Error> {
        let store = self.lock()?;
        message_window(&store, request)
    }
}

fn search(store: &Store, request: LocalSearchRequest) -> Result<LocalSearchPage, Error> {
    if store.keys.is_empty() {
        return Err(Error::KeysUnavailable);
    }
    let query = request.query.trim();
    let limit = request.limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
    if query.chars().count() > MAX_SEARCH_QUERY_CHARS || !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
        return Err(Error::InvalidRequest("search request"));
    }
    let kind = request.kind.unwrap_or(SearchKind::All);
    let generation = search_generation(&store.conn)?;
    let cursor = match request.cursor.as_deref() {
        Some(cursor) => Some(
            decode_cursor(cursor)
                .filter(|value| {
                    value.query == query && value.kind == kind && value.generation == generation
                })
                .ok_or(Error::InvalidRequest("search cursor"))?,
        ),
        None => None,
    };
    if cursor.is_some() && kind == SearchKind::All {
        return Err(Error::InvalidRequest("search cursor"));
    }
    let (index_state, _) = index_progress(&store.conn)?;
    if query.is_empty() {
        return Ok(LocalSearchPage {
            query: String::new(),
            items: vec![],
            next_cursor: None,
            index_state,
            has_more: SearchHasMore {
                conversation: false,
                message: false,
                contact: false,
            },
        });
    }
    let mut items = Vec::new();
    let mut next_key = None;
    let mut has_more = SearchHasMore {
        conversation: false,
        message: false,
        contact: false,
    };
    if matches!(kind, SearchKind::All | SearchKind::Message) {
        let hits = message_hits(store, query, limit + 1, cursor.as_ref())?;
        has_more.message = hits.len() > limit;
        for (hit, key) in hits.into_iter().take(limit) {
            next_key = Some(key);
            items.push(hit);
        }
    }
    if matches!(kind, SearchKind::All | SearchKind::Conversation) {
        let hits = conversation_hits(store, query, limit + 1, cursor.as_ref())?;
        has_more.conversation = hits.len() > limit;
        for (hit, key) in hits.into_iter().take(limit) {
            next_key = Some(key);
            items.push(hit);
        }
    }
    if matches!(kind, SearchKind::All | SearchKind::Contact) {
        let hits = contact_hits(store, query, limit + 1, cursor.as_ref())?;
        has_more.contact = hits.len() > limit;
        for (hit, key) in hits.into_iter().take(limit) {
            next_key = Some(key);
            items.push(hit);
        }
    }
    let next_cursor = if kind == SearchKind::All || !has_more_for(kind, &has_more) {
        None
    } else {
        next_key.map(|key| encode_cursor(query, kind, generation, &key))
    };
    Ok(LocalSearchPage {
        query: query.to_owned(),
        items,
        next_cursor,
        index_state,
        has_more,
    })
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchCursor {
    query: String,
    kind: SearchKind,
    generation: i64,
    key: String,
}

fn kind_name(kind: SearchKind) -> &'static str {
    match kind {
        SearchKind::All => "all",
        SearchKind::Conversation => "conversation",
        SearchKind::Message => "message",
        SearchKind::Contact => "contact",
    }
}
fn parse_kind(value: &str) -> Option<SearchKind> {
    match value {
        "all" => Some(SearchKind::All),
        "conversation" => Some(SearchKind::Conversation),
        "message" => Some(SearchKind::Message),
        "contact" => Some(SearchKind::Contact),
        _ => None,
    }
}
fn hex(value: &str) -> String {
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value.bytes() {
        write!(&mut output, "{byte:02x}").expect("string write");
    }
    output
}
fn unhex(value: &str) -> Option<String> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).ok())
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}
fn encode_cursor(query: &str, kind: SearchKind, generation: i64, key: &str) -> String {
    format!(
        "{}:{}:{}:{}",
        hex(query),
        kind_name(kind),
        generation,
        hex(key)
    )
}
fn decode_cursor(cursor: &str) -> Option<SearchCursor> {
    let mut fields = cursor.split(':');
    let query = unhex(fields.next()?)?;
    let kind = parse_kind(fields.next()?)?;
    let generation = fields.next()?.parse().ok()?;
    let key = unhex(fields.next()?)?;
    if fields.next().is_some() {
        return None;
    }
    Some(SearchCursor {
        query,
        kind,
        generation,
        key,
    })
}
fn has_more_for(kind: SearchKind, more: &SearchHasMore) -> bool {
    match kind {
        SearchKind::Conversation => more.conversation,
        SearchKind::Message => more.message,
        SearchKind::Contact => more.contact,
        SearchKind::All => false,
    }
}
fn metadata_i64(conn: &rusqlite::Connection, key: &str) -> Result<i64, Error> {
    let value: String = conn
        .query_row("SELECT v FROM metadata WHERE k=?", [key], |row| row.get(0))
        .optional()?
        .ok_or(Error::Database)?;
    value.parse().map_err(|_| Error::Database)
}

fn set_metadata_i64(conn: &rusqlite::Connection, key: &str, value: i64) -> Result<(), Error> {
    if conn.execute(
        "UPDATE metadata SET v=? WHERE k=?",
        params![value.to_string(), key],
    )? != 1
    {
        return Err(Error::Database);
    }
    Ok(())
}

pub(crate) fn search_generation(conn: &rusqlite::Connection) -> Result<i64, Error> {
    metadata_i64(conn, "search_generation")
}

fn message_hits(
    store: &Store,
    query: &str,
    limit: usize,
    cursor: Option<&SearchCursor>,
) -> Result<Vec<(LocalSearchHit, String)>, Error> {
    let literal = query
        .split_whitespace()
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ");
    if literal.is_empty() {
        return Ok(vec![]);
    }
    let (score, timestamp, local_order, id) = cursor
        .map(|value| parse_message_cursor(&value.key))
        .transpose()?
        .unwrap_or((f64::NEG_INFINITY, i64::MAX, i64::MAX, String::new()));
    store.conn.prepare("WITH scores AS (SELECT rowid,bm25(local_search_fts) AS score FROM local_search_fts WHERE local_search_fts MATCH ?1 LIMIT -1), page AS MATERIALIZED (SELECT m.local_order,m.id,m.conversation_id,m.sort_at_ms,s.score FROM scores s JOIN messages m INDEXED BY messages_search_metadata ON m.local_order=s.rowid WHERE (s.score>?2 OR (s.score=?2 AND (m.sort_at_ms<?3 OR (m.sort_at_ms=?3 AND (m.local_order<?4 OR (m.local_order=?4 AND m.id>?5)))))) ORDER BY s.score,m.sort_at_ms DESC,m.local_order DESC,m.id LIMIT ?6) SELECT page.id,page.conversation_id,COALESCE(json_extract(m.payload,'$.subject'),''),COALESCE(json_extract(m.payload,'$.body'),''),page.sort_at_ms,page.local_order,page.score FROM page JOIN messages m ON m.local_order=page.local_order ORDER BY page.score,page.sort_at_ms DESC,page.local_order DESC,page.id")?
        .query_map(params![literal, score, timestamp, local_order, id, limit as i64], |row| {
            let message_id: String = row.get(0)?;
            let timestamp: i64 = row.get(4)?;
            let local_order: i64 = row.get(5)?;
            let score: f64 = row.get(6)?;
            let title: String = row.get(2)?;
            Ok((LocalSearchHit::Message { message_id: message_id.clone(), conversation_id: row.get(1)?, title: if title.is_empty() { "Message".into() } else { title }, snippet: row.get::<_, String>(3)?.chars().take(160).collect(), timestamp: timestamp.to_string() }, format!("{:016x}|{timestamp}|{local_order}|{message_id}", score.to_bits())))
        })?
        .collect::<Result<Vec<_>, _>>().map_err(Into::into)
}
fn parse_message_cursor(value: &str) -> Result<(f64, i64, i64, String), Error> {
    let mut fields = value.splitn(4, '|');
    let score = fields
        .next()
        .and_then(|value| u64::from_str_radix(value, 16).ok())
        .map(f64::from_bits);
    let timestamp: Option<i64> = fields.next().and_then(|value| value.parse().ok());
    let local_order: Option<i64> = fields.next().and_then(|value| value.parse().ok());
    let id = fields
        .next()
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    match (score, timestamp, local_order, id) {
        (Some(score), Some(timestamp), Some(local_order), Some(id)) if score.is_finite() => {
            Ok((score, timestamp, local_order, id))
        }
        _ => Err(Error::InvalidRequest("search cursor")),
    }
}
fn phone_query_digits(query: &str) -> Option<String> {
    let digits = query
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>();
    (digits.len() >= 2
        && query.chars().all(|character| {
            character.is_ascii_digit() || matches!(character, '+' | ' ' | '(' | ')' | '.' | '-')
        }))
    .then_some(digits)
}

fn text_rank(text: &str, query: &str) -> Option<usize> {
    let text = text.to_lowercase();
    let query = query.to_lowercase();
    if text.starts_with(&query) {
        Some(0)
    } else if text.contains(&query) {
        Some(1)
    } else {
        None
    }
}

fn conversation_hits(
    store: &Store,
    query: &str,
    limit: usize,
    cursor: Option<&SearchCursor>,
) -> Result<Vec<(LocalSearchHit, String)>, Error> {
    let phone_digits = phone_query_digits(query);
    let mut statement = store.conn.prepare(
        "SELECT a.conversation_id,a.address,COALESCE((SELECT MAX(m.sort_at_ms) FROM messages m WHERE m.conversation_id=a.conversation_id),0) FROM addresses a",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let resolver = crate::contact_resolution::display_resolver(&store.conn)?;
    let mut matches = std::collections::BTreeMap::<String, (usize, i64, String, String)>::new();
    for (conversation_id, address, latest) in rows {
        let resolution = resolver.resolve(&address, None)?;
        let candidate_rank = resolution
            .candidate_display_names
            .iter()
            .filter_map(|name| text_rank(name, query))
            .min();
        let address_rank = text_rank(&address, query).or_else(|| {
            phone_digits.as_ref().and_then(|digits| {
                address
                    .chars()
                    .filter(char::is_ascii_digit)
                    .collect::<String>()
                    .contains(digits)
                    .then_some(2)
            })
        });
        let Some(rank) = candidate_rank.into_iter().chain(address_rank).min() else {
            continue;
        };
        let title = resolution
            .chosen_display_name
            .unwrap_or_else(|| address.clone());
        let subtitle = if title == address {
            String::new()
        } else {
            address
        };
        let candidate = (rank, latest, title, subtitle);
        matches
            .entry(conversation_id)
            .and_modify(|current| {
                if (candidate.0, std::cmp::Reverse(candidate.1), &candidate.2)
                    < (current.0, std::cmp::Reverse(current.1), &current.2)
                {
                    *current = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    let mut matches = matches.into_iter().collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        (left.1.0, std::cmp::Reverse(left.1.1), &left.0).cmp(&(
            right.1.0,
            std::cmp::Reverse(right.1.1),
            &right.0,
        ))
    });
    let after = cursor.map(|cursor| cursor.key.as_str());
    let mut seen_after = after.is_none();
    let mut output = Vec::new();
    for (conversation_id, (_, _, title, subtitle)) in matches {
        if !seen_after {
            seen_after = after == Some(conversation_id.as_str());
            continue;
        }
        let key = conversation_id.clone();
        output.push((
            LocalSearchHit::Conversation {
                conversation_id,
                title,
                subtitle,
            },
            key,
        ));
        if output.len() == limit {
            break;
        }
    }
    Ok(output)
}

fn display_name(contact: &Value) -> Option<String> {
    let scalar = |key| {
        contact
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    scalar("display_name")
        .map(str::to_owned)
        .or_else(|| {
            let name = contact.get("name")?;
            let parts = ["given", "family"]
                .into_iter()
                .filter_map(|key| name.get(key).and_then(Value::as_str))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join(" "))
        })
        .or_else(|| scalar("nickname").map(str::to_owned))
        .or_else(|| scalar("organization").map(str::to_owned))
        .or_else(|| first_contact_value(contact, "phones"))
        .or_else(|| first_contact_value(contact, "emails"))
        .or_else(|| Some("Contact".to_owned()))
}

fn first_contact_value(contact: &Value, field: &str) -> Option<String> {
    contact
        .get(field)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("value").and_then(Value::as_str).map(str::trim))
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

struct ContactMatch {
    rank: usize,
    field: Option<&'static str>,
    snippet: String,
}

fn contact_match(contact: &Value, query: &str) -> Option<ContactMatch> {
    let computed_display_name = display_name(contact);
    let name_match = computed_display_name
        .as_deref()
        .into_iter()
        .chain(
            contact
                .get("name")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|name| name.values())
                .filter_map(Value::as_str),
        )
        .filter_map(|value| text_rank(value, query))
        .min();
    let phone_match = phone_query_digits(query).and_then(|digits| {
        contact
            .get("phones")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("value").and_then(Value::as_str))
            .any(|value| {
                value
                    .chars()
                    .filter(char::is_ascii_digit)
                    .collect::<String>()
                    .contains(&digits)
            })
            .then_some(2)
    });
    let primary_match = name_match.into_iter().chain(phone_match).min();
    let metadata_match = contact_metadata(contact)
        .into_iter()
        .filter_map(|(field, value)| text_rank(&value, query).map(|rank| (rank + 10, field, value)))
        .min_by_key(|(rank, _, _)| *rank)
        .map(|(rank, field, value)| ContactMatch {
            rank,
            field: Some(field),
            snippet: contextual_snippet(&value, query),
        });
    primary_match
        .map(|rank| ContactMatch {
            rank,
            field: None,
            snippet: String::new(),
        })
        .into_iter()
        .chain(metadata_match)
        .min_by_key(|matched| matched.rank)
}

fn contact_metadata(contact: &Value) -> Vec<(&'static str, String)> {
    let mut fields = [
        ("Nickname", "nickname"),
        ("Organization", "organization"),
        ("Title", "title"),
        ("Notes", "notes"),
    ]
    .into_iter()
    .filter_map(|(field, key)| {
        contact
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| (field, value.to_owned()))
    })
    .collect::<Vec<_>>();
    for (field, key) in [("Phone", "phones"), ("Email", "emails")] {
        fields.extend(
            contact
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|item| item.get("value").and_then(Value::as_str))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| (field, value.to_owned())),
        );
    }
    fields.extend(
        contact
            .get("addresses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|address| {
                [
                    "value",
                    "street",
                    "po_box",
                    "neighborhood",
                    "sub_locality",
                    "city",
                    "sub_administrative_area",
                    "state",
                    "postal_code",
                    "country",
                    "iso_country_code",
                ]
                .into_iter()
                .filter_map(move |key| address.get(key).and_then(Value::as_str))
            })
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| ("Address", value.to_owned())),
    );
    if let Some(birthday) = birthday_values(contact) {
        fields.extend(birthday.into_iter().map(|value| ("Birthday", value)));
    }
    fields
}

fn birthday_values(contact: &Value) -> Option<Vec<String>> {
    let birthday = contact.get("birthday")?.as_object()?;
    let month = birthday.get("month")?.as_u64()?;
    let day = birthday.get("day")?.as_u64()?;
    let month_day = format!("{month:02}-{day:02}");
    let values = birthday
        .get("year")
        .and_then(Value::as_u64)
        .map(|year| vec![format!("{year:04}-{month_day}"), month_day.clone()])
        .unwrap_or_else(|| vec![month_day]);
    Some(values)
}

fn contextual_snippet(value: &str, query: &str) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower_value = value.to_lowercase();
    let mut source_indices = Vec::new();
    for (source_index, character) in value.chars().enumerate() {
        source_indices.extend(std::iter::repeat_n(
            source_index,
            character.to_lowercase().count(),
        ));
    }
    let lower_query = query.to_lowercase();
    let match_start = lower_value
        .find(&lower_query)
        .and_then(|index| {
            lower_value
                .char_indices()
                .zip(source_indices)
                .find(|((offset, _), _)| *offset == index)
                .map(|(_, source_index)| source_index)
        })
        .unwrap_or(0);
    let characters = value.chars().collect::<Vec<_>>();
    let start = match_start.saturating_sub(20);
    let end = (start + 60).min(characters.len());
    let mut snippet = characters[start..end].iter().collect::<String>();
    if start > 0 {
        snippet.insert(0, '…');
    }
    if end < characters.len() {
        snippet.push('…');
    }
    snippet
}

fn contact_hits(
    store: &Store,
    query: &str,
    limit: usize,
    cursor: Option<&SearchCursor>,
) -> Result<Vec<(LocalSearchHit, String)>, Error> {
    let mut statement = store.conn.prepare(
        "SELECT c.book_id,c.id,c.body,b.body FROM contacts c JOIN contact_books b ON b.id=c.book_id \
         WHERE b.forgotten=0 AND c.deleted_at IS NULL ORDER BY c.book_id,c.id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut matches = Vec::new();
    for (book_id, contact_id, contact_body, book_body) in rows {
        let Ok(contact) = serde_json::from_str::<Value>(&contact_body) else {
            continue;
        };
        let Some(matched) = contact_match(&contact, query) else {
            continue;
        };
        let book = serde_json::from_str::<Value>(&book_body).unwrap_or(Value::Null);
        let book_label = book
            .get("display_name")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&book_id);
        let phone = contact
            .get("phones")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|phone| phone.get("value").and_then(Value::as_str))
            .find(|value| !value.trim().is_empty());
        let primary_subtitle = phone.map_or_else(
            || book_label.to_owned(),
            |phone| format!("{book_label} · {phone}"),
        );
        let subtitle = matched.field.map_or(primary_subtitle, |field| {
            format!("{book_label} · {field} · {}", matched.snippet)
        });
        let key = format!("{book_id}\0{contact_id}");
        matches.push((
            matched.rank,
            key,
            LocalSearchHit::Contact {
                book_id,
                contact_id,
                title: display_name(&contact).unwrap_or_default(),
                subtitle,
            },
        ));
    }
    matches.sort_by(|left, right| (left.0, &left.1).cmp(&(right.0, &right.1)));
    let after = cursor.map(|cursor| cursor.key.as_str());
    let mut seen_after = after.is_none();
    let mut output = Vec::new();
    for (_, key, hit) in matches {
        if !seen_after {
            seen_after = after == Some(key.as_str());
            continue;
        }
        output.push((hit, key));
        if output.len() == limit {
            break;
        }
    }
    Ok(output)
}

fn index_progress(conn: &rusqlite::Connection) -> Result<(SearchIndexState, usize), Error> {
    let remaining =
        usize::try_from(metadata_i64(conn, "search_remaining")?).map_err(|_| Error::Database)?;
    Ok((
        if remaining == 0 {
            SearchIndexState::Ready
        } else {
            SearchIndexState::Indexing
        },
        remaining,
    ))
}

fn message_window(store: &Store, request: MessageWindowRequest) -> Result<MessageWindow, Error> {
    if store.keys.is_empty() {
        return Err(Error::KeysUnavailable);
    }
    if let MessageWindowRequest::Refresh {
        conversation_id,
        mut message_ids,
    } = request
    {
        if message_ids.len() > HISTORY_REFRESH_LIMIT {
            return Err(Error::InvalidRequest("history refresh size"));
        }
        message_ids.sort();
        message_ids.dedup();
        let ids = if message_ids.is_empty() {
            Vec::new()
        } else {
            let placeholders = std::iter::repeat_n("?", message_ids.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT id FROM messages WHERE conversation_id=? AND id IN ({placeholders}) ORDER BY sort_at_ms,local_order,id"
            );
            let values = std::iter::once(conversation_id.as_str())
                .chain(message_ids.iter().map(String::as_str));
            store
                .conn
                .prepare(&sql)?
                .query_map(rusqlite::params_from_iter(values), |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let messages = ids
            .iter()
            .map(|id| {
                load_message(store, &conversation_id, id)
                    .and_then(|message| message.ok_or(Error::Database))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (has_older, has_newer) = message_edge_flags(store, &conversation_id, &ids)?;
        return Ok(MessageWindow {
            conversation_id,
            messages,
            has_older,
            has_newer,
        });
    }
    let (conversation_id, message_id, direction) = match request {
        MessageWindowRequest::Around {
            conversation_id,
            message_id,
        } => (conversation_id, message_id, "around"),
        MessageWindowRequest::Older {
            conversation_id,
            message_id,
        } => (conversation_id, message_id, "older"),
        MessageWindowRequest::Newer {
            conversation_id,
            message_id,
        } => (conversation_id, message_id, "newer"),
        MessageWindowRequest::Refresh { .. } => unreachable!(),
    };
    let (sort_at_ms, local_order): (i64, i64) = store
        .conn
        .query_row(
            "SELECT sort_at_ms,local_order FROM messages WHERE id=? AND conversation_id=?",
            params![message_id, conversation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    let (where_clause, order, take) = match direction {
        "older" => (
            "(sort_at_ms<?2 OR (sort_at_ms=?2 AND local_order<?3))",
            "sort_at_ms DESC,local_order DESC",
            HISTORY_WINDOW_LIMIT,
        ),
        "newer" => (
            "(sort_at_ms>?2 OR (sort_at_ms=?2 AND local_order>?3))",
            "sort_at_ms ASC,local_order ASC",
            HISTORY_WINDOW_LIMIT,
        ),
        _ => (
            "(sort_at_ms<?2 OR (sort_at_ms=?2 AND local_order<=?3))",
            "sort_at_ms DESC,local_order DESC",
            HISTORY_WINDOW_LIMIT / 2 + 1,
        ),
    };
    let sql = format!(
        "SELECT id FROM messages WHERE conversation_id=? AND {where_clause} ORDER BY {order} LIMIT {take}"
    );
    let mut ids = store
        .conn
        .prepare(&sql)?
        .query_map(params![conversation_id, sort_at_ms, local_order], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if direction == "older" || direction == "around" {
        ids.reverse();
    }
    if direction == "around" {
        let newer = store.conn.prepare("SELECT id FROM messages WHERE conversation_id=?1 AND (sort_at_ms>?2 OR (sort_at_ms=?2 AND local_order>?3)) ORDER BY sort_at_ms,local_order LIMIT ?4")?.query_map(params![conversation_id, sort_at_ms, local_order, (HISTORY_WINDOW_LIMIT - ids.len()) as i64], |row| row.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
        ids.extend(newer);
    }
    let messages = ids
        .iter()
        .map(|id| {
            load_message(store, &conversation_id, id)
                .and_then(|message| message.ok_or(Error::Database))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (has_older, has_newer) = message_edge_flags(store, &conversation_id, &ids)?;
    Ok(MessageWindow {
        conversation_id,
        messages,
        has_older,
        has_newer,
    })
}
fn message_edge_flags(
    store: &Store,
    conversation_id: &str,
    ordered_ids: &[String],
) -> Result<(bool, bool), Error> {
    let edge_exists = |id: &str, newer: bool| -> Result<bool, Error> {
        let comparator = if newer { ">" } else { "<" };
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM messages candidate JOIN messages anchor ON anchor.id=? AND anchor.conversation_id=? WHERE candidate.conversation_id=? AND (candidate.sort_at_ms {comparator} anchor.sort_at_ms OR (candidate.sort_at_ms=anchor.sort_at_ms AND candidate.local_order {comparator} anchor.local_order)))"
        );
        Ok(store
            .conn
            .query_row(&sql, params![id, conversation_id, conversation_id], |row| {
                row.get(0)
            })?)
    };
    let has_older = ordered_ids
        .first()
        .map(|id| edge_exists(id, false))
        .transpose()?
        .unwrap_or(false);
    let has_newer = ordered_ids
        .last()
        .map(|id| edge_exists(id, true))
        .transpose()?
        .unwrap_or(false);
    Ok((has_older, has_newer))
}

fn load_message(store: &Store, conversation_id: &str, id: &str) -> Result<Option<Message>, Error> {
    store.conn.query_row("SELECT m.payload,m.seen,selected_status.state,selected_status.status_at_ms,(SELECT o.state FROM commands c JOIN outbox o ON o.command_id=c.command_id WHERE c.message_id=m.id),EXISTS(SELECT 1 FROM commands c WHERE c.message_id=m.id) FROM messages m LEFT JOIN send_status selected_status ON selected_status.rowid=(SELECT st.rowid FROM commands c JOIN send_status st ON st.command_id=c.command_id AND st.producer_device_id=c.gateway_device_id WHERE c.message_id=m.id ORDER BY c.rowid LIMIT 1) WHERE m.id=? AND m.conversation_id=?", params![id, conversation_id], message_row).optional()?.map(|row| message_from_row(&store.conn, row)).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientConfig, DatabaseKey, DeviceId, VaultId};
    use peppy_crypto::{KeyProfile, VaultCheckHeader, create_vault_check_header, derive_root_key};

    fn unlocked_client(
        path: std::path::PathBuf,
        vault_id: VaultId,
        device_id: DeviceId,
        profile: &KeyProfile,
        header: &VaultCheckHeader,
    ) -> Client {
        let client = Client::open(
            ClientConfig {
                database_path: path,
                vault_id,
                device_id,
            },
            DatabaseKey::new(&[91; 32]).unwrap(),
        )
        .unwrap();
        client
            .unlock(profile, header, "search-test-passphrase")
            .unwrap();
        client
    }

    #[test]
    fn upgraded_backfill_advances_source_watermark_and_invalidates_cursors() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("backfill.db");
        let vault_id = VaultId::new();
        let device_id = DeviceId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("search-test-passphrase", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let client = unlocked_client(path.clone(), vault_id, device_id, &profile, &header);
        {
            let store = client.lock().unwrap();
            store.conn.execute_batch(
                "DROP TRIGGER messages_search_insert;
                 DROP TRIGGER messages_search_update;
                 DROP TRIGGER messages_search_delete;
                 DROP TRIGGER contacts_search_insert;
                 DROP TRIGGER contacts_search_update;
                 DROP TRIGGER contacts_search_delete;
                 DROP TRIGGER contact_books_search_insert;
                 DROP TRIGGER contact_books_search_update;
                 DROP TRIGGER contact_books_search_delete;
                 DROP TRIGGER contact_address_search_insert;
                 DROP TRIGGER contact_address_search_update;
                 DROP TRIGGER contact_address_search_delete;
                 DROP TRIGGER addresses_search_insert;
                 DROP TRIGGER addresses_search_update;
                 DROP TRIGGER addresses_search_delete;
                 DROP INDEX messages_search_metadata;
                 DROP TABLE local_search_fts;
                 DELETE FROM metadata WHERE k IN ('search_remaining','search_generation','search_backfill_highwater','search_backfill_watermark');
                 UPDATE schema_meta SET version=34;",
            ).unwrap();
            let mut insert = store.conn.prepare(
                "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen,sort_at_ms) VALUES(?,?,?,?,?,0,?)",
            ).unwrap();
            for number in 0..205_i64 {
                insert
                    .execute(params![
                        format!("message-{number:03}"),
                        "conversation",
                        "source",
                        number,
                        serde_json::json!({"body": format!("needle {number}")}).to_string(),
                        number,
                    ])
                    .unwrap();
            }
        }
        drop(client);

        let client = unlocked_client(path.clone(), vault_id, device_id, &profile, &header);
        {
            let store = client.lock().unwrap();
            store
                .conn
                .execute(
                    "UPDATE messages SET payload=? WHERE id='message-000'",
                    [serde_json::json!({"body":"needle rewritten"}).to_string()],
                )
                .unwrap();
            store
                .conn
                .execute(
                    "UPDATE messages SET payload=? WHERE id='message-001'",
                    [serde_json::json!({"body":"needle second"}).to_string()],
                )
                .unwrap();
        }
        assert!(
            client
                .search_local(LocalSearchRequest {
                    query: "rewritten".into(),
                    kind: Some(SearchKind::Message),
                    cursor: None,
                    limit: Some(6),
                })
                .unwrap()
                .items
                .len()
                == 1
        );
        assert!(
            client
                .search_local(LocalSearchRequest {
                    query: "0".into(),
                    kind: Some(SearchKind::Message),
                    cursor: None,
                    limit: Some(6),
                })
                .unwrap()
                .items
                .is_empty()
        );
        let first_page = client
            .search_local(LocalSearchRequest {
                query: "needle".into(),
                kind: Some(SearchKind::Message),
                cursor: None,
                limit: Some(1),
            })
            .unwrap();
        let old_cursor = first_page.next_cursor.unwrap();
        drop(client);

        let client = unlocked_client(path, vault_id, device_id, &profile, &header);
        let first = client.advance_search_index(Some(100)).unwrap();
        assert_eq!((first.processed, first.remaining), (100, 105));
        assert!(matches!(
            client.search_local(LocalSearchRequest {
                query: "needle".into(),
                kind: Some(SearchKind::Message),
                cursor: Some(old_cursor),
                limit: Some(1),
            }),
            Err(Error::InvalidRequest("search cursor"))
        ));
        let second = client.advance_search_index(Some(100)).unwrap();
        assert_eq!((second.processed, second.remaining), (100, 5));
        let third = client.advance_search_index(Some(100)).unwrap();
        assert_eq!(
            (third.processed, third.remaining, third.index_state),
            (5, 0, SearchIndexState::Ready)
        );
        let ready_again = client.advance_search_index(Some(100)).unwrap();
        assert_eq!((ready_again.processed, ready_again.remaining), (0, 0));
    }

    #[test]
    fn corrupt_search_metadata_fails_closed() {
        let directory = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("search-test-passphrase", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let client = unlocked_client(
            directory.path().join("metadata.db"),
            vault_id,
            DeviceId::new(),
            &profile,
            &header,
        );
        client
            .lock()
            .unwrap()
            .conn
            .execute(
                "UPDATE metadata SET v='not-a-number' WHERE k='search_generation'",
                [],
            )
            .unwrap();
        assert!(matches!(
            client.search_local(LocalSearchRequest {
                query: "needle".into(),
                kind: Some(SearchKind::Message),
                cursor: None,
                limit: Some(1),
            }),
            Err(Error::Database)
        ));
    }

    #[test]
    fn all_core_search_entrypoints_reject_locked_keys() {
        let directory = tempfile::TempDir::new().unwrap();
        let vault_id = VaultId::new();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("search-test-passphrase", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        let client = unlocked_client(
            directory.path().join("locked-search.db"),
            vault_id,
            DeviceId::new(),
            &profile,
            &header,
        );
        client.lock().unwrap().keys.clear();

        assert!(matches!(
            client.search_local(LocalSearchRequest {
                query: "needle".into(),
                kind: Some(SearchKind::Message),
                cursor: None,
                limit: Some(1),
            }),
            Err(Error::KeysUnavailable)
        ));
        assert!(matches!(
            client.advance_search_index(Some(1)),
            Err(Error::KeysUnavailable)
        ));
        assert!(matches!(
            client.load_message_window(MessageWindowRequest::Around {
                conversation_id: "conversation".into(),
                message_id: "message".into(),
            }),
            Err(Error::KeysUnavailable)
        ));
    }
}
