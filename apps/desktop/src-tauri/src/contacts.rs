//! Contact IPC: a whitelist adapter between the webview's camelCase contact DTOs and the
//! client-core contact JSON API.
//!
//! * Views copy only named, displayable fields. Provider identity, photo descriptors, file keys,
//!   attachment IDs and filesystem paths never reach the webview; photos arrive as re-encoded
//!   data URLs.
//! * Edits become core `ContactEditRequest`s addressed to the book's owner device. The desktop
//!   never owns a book, so an accepted edit is only `pending` (with its request ID) until the
//!   owner's result reaches the local ledger, which `list_edits` reads. Nothing here reports an
//!   apply early, and an issued request cannot be withdrawn.
//! * View field IDs are preserved so list patches address existing items, and the original
//!   values (`expectedOld`) become `expected_old` so the owner can merge a stale base safely.
use crate::{
    error::{BridgeError, BridgeResult, core_error},
    fsutil, media,
    session::Session,
};
use peppy_client_core::{AttachmentId, Client, Error as CoreError};
use peppy_desktop_api::contacts as shared;
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

impl From<peppy_desktop_api::ContactError> for BridgeError {
    fn from(error: peppy_desktop_api::ContactError) -> Self {
        match error {
            peppy_desktop_api::ContactError::Ui { code, message } => {
                BridgeError::new(code, message)
            }
            peppy_desktop_api::ContactError::Core(error) => core_error(error),
        }
    }
}

const MAX_AVATAR_BYTES: u64 = 64 * 1024;
const AVATAR_EDGE: u32 = 128;
const AVATAR_MAX_BYTES: usize = 16 * 1024;
const AVATAR_BUDGET: usize = 64;
const CROP_SOURCE_EDGE: u32 = 1024;
const PHOTO_STAGING_DIR: &str = "contact-photo-staging";
const STAGING_MAX_AGE: Duration = Duration::from_secs(10 * 60);
const MAX_PREVIEW_CACHE: usize = 256;

fn invalid(message: &'static str) -> BridgeError {
    BridgeError::new("invalid-contact-edit", message)
}

fn parse_core(result: Result<String, CoreError>) -> BridgeResult<Value> {
    serde_json::from_str(&result.map_err(core_error)?).map_err(|_| BridgeError::host_state())
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn valid_id(value: &str) -> bool {
    shared::valid_id(value)
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    shared::str_field(value, key)
}

fn required_id(input: &Value, key: &str) -> BridgeResult<String> {
    shared::required_id(input, key).map_err(BridgeError::from)
}

fn core_books(client: &Client) -> BridgeResult<Vec<Value>> {
    let raw = parse_core(client.list_contact_books_json())?;
    match raw.get("books") {
        Some(Value::Array(books)) => Ok(books.clone()),
        _ => Err(BridgeError::host_state()),
    }
}

fn find_book(client: &Client, book_id: &str) -> BridgeResult<Value> {
    core_books(client)?
        .into_iter()
        .find(|book| str_field(book, "id") == Some(book_id))
        .ok_or_else(|| BridgeError::new("not-found", "The contact book was not found."))
}

pub fn list_books(session: &Session) -> BridgeResult<Vec<Value>> {
    let own = session.binding.device_id.as_str();
    let pending = shared::pending_counts(&list_edits_for(&session.client, own, None)?);
    let device_names = session
        .gateways()
        .0
        .into_iter()
        .map(|gateway| (gateway.id, gateway.name))
        .collect::<HashMap<_, _>>();
    Ok(core_books(&session.client)?
        .iter()
        .filter_map(|book| shared::book_view(book, own, &device_names, &pending))
        .collect())
}

pub fn forget_book(session: &Session, book_id: &str) -> BridgeResult<()> {
    if !valid_id(book_id) {
        return Err(invalid("The contact book is invalid."));
    }
    parse_core(
        session
            .client
            .forget_contact_book(&json!({"book_id": book_id}).to_string()),
    )?;
    session.notify();
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Contact views
// ---------------------------------------------------------------------------------------------

/// Decrypts a verified local contact photo into a small re-encoded JPEG data URL (at most
/// `AVATAR_MAX_BYTES`), cached per attachment in the session preview cache, which the media
/// worker invalidates after downloads. Used for contact lists and display-only name resolution.
pub(crate) fn session_avatar(
    session: &Session,
    budget: &mut usize,
    id: AttachmentId,
) -> Option<String> {
    let cached = session
        .previews
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&id)
        .cloned();
    if let Some(value) = cached {
        return value;
    }
    if *budget == 0 {
        return None;
    }
    *budget -= 1;
    let value = session
        .client
        .open_native_plaintext(id)
        .ok()
        .and_then(|file| {
            let mut bytes = Vec::new();
            fs::File::open(file.path())
                .ok()?
                .take(MAX_AVATAR_BYTES + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            (bytes.len() as u64 <= MAX_AVATAR_BYTES)
                .then(|| media::avatar_data_url(&bytes, AVATAR_EDGE, AVATAR_MAX_BYTES))
                .flatten()
        });
    let mut cache = session
        .previews
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if cache.len() >= MAX_PREVIEW_CACHE {
        cache.clear();
    }
    cache.insert(id, value.clone());
    value
}

/// One page (at most 200, core's cap) of a book's live contacts, starting at `offset`.
pub fn list_contacts(
    session: &Session,
    book_id: &str,
    query: Option<&str>,
    offset: Option<u32>,
) -> BridgeResult<Vec<Value>> {
    if !valid_id(book_id) {
        return Err(invalid("The contact book is invalid."));
    }
    let query = query.unwrap_or("");
    if query.chars().count() > shared::MAX_TEXT {
        return Err(invalid("The contact search is too long."));
    }
    let raw = parse_core(
        session.client.contact_book_view(
            &json!({
                "schema_version": 1,
                "book_id": book_id,
                "search": query,
                "limit": shared::PAGE_LIMIT,
                "offset": offset.unwrap_or(0),
            })
            .to_string(),
        ),
    )?;
    let contacts = raw
        .get("contacts")
        .and_then(Value::as_array)
        .ok_or_else(BridgeError::host_state)?;
    let badges = shared::contact_badges(
        &list_edits_for(&session.client, &session.binding.device_id, Some(book_id))?,
        now_seconds(),
    );
    let mut budget = AVATAR_BUDGET;
    let mut avatar = |id: AttachmentId| session_avatar(session, &mut budget, id);
    Ok(contacts
        .iter()
        .filter_map(|contact| {
            let mut view = shared::contact_view(contact, book_id, &mut avatar)?;
            if let Some(edit) = str_field(&view, "id").and_then(|id| badges.get(id)) {
                shared::apply_badge(&mut view, edit);
            }
            Some(view)
        })
        .collect())
}

pub fn restorable(session: &Session, book_id: &str) -> BridgeResult<Vec<Value>> {
    if !valid_id(book_id) {
        return Err(invalid("The contact book is invalid."));
    }
    let raw = parse_core(
        session
            .client
            .list_restorable_contacts_json(&json!({"book_id": book_id}).to_string()),
    )?;
    let contacts = raw
        .get("contacts")
        .and_then(Value::as_array)
        .ok_or_else(BridgeError::host_state)?;
    let mut budget = AVATAR_BUDGET;
    Ok(contacts
        .iter()
        .filter_map(|contact| {
            shared::restorable_view(contact, book_id, &mut |id| {
                session_avatar(session, &mut budget, id)
            })
        })
        .collect())
}

/// Asks the book owner to recreate a retained tombstone (core `restore_contact_json`). The result
/// is a pending request like any other edit; only the owner's result makes it applied.
pub fn restore(session: &Session, book_id: &str, contact_id: &str) -> BridgeResult<Value> {
    if !valid_id(book_id) || !valid_id(contact_id) {
        return Err(invalid("The contact is invalid."));
    }
    let book = find_book(&session.client, book_id)?;
    if !shared::capabilities(&book, &session.binding.device_id).can_restore {
        return Err(BridgeError::new(
            "contact-book-read-only",
            "This contact book does not accept changes from this computer.",
        ));
    }
    let result =
        parse_core(session.client.restore_contact_json(
            &json!({"book_id": book_id, "contact_id": contact_id}).to_string(),
        ))?;
    let request_id = str_field(&result, "request_id").ok_or_else(BridgeError::host_state)?;
    session.request_work();
    session.notify();
    Ok(json!({"state": "pending", "requestId": request_id}))
}

// ---------------------------------------------------------------------------------------------
// Edit status (requester ledger)
// ---------------------------------------------------------------------------------------------

fn list_edits_for(
    client: &Client,
    own_device: &str,
    book_id: Option<&str>,
) -> BridgeResult<Vec<Value>> {
    let raw = parse_core(client.list_contact_requests_json(
        &json!({"schema_version": 1, "requester": own_device}).to_string(),
    ))?;
    shared::requester_ledger_view(&raw, book_id, now_seconds()).map_err(BridgeError::from)
}

pub fn list_edits(session: &Session, book_id: Option<&str>) -> BridgeResult<Vec<Value>> {
    list_edits_for(&session.client, &session.binding.device_id, book_id)
}

fn photo_error() -> BridgeError {
    BridgeError::from(shared::photo_error())
}

fn is_photo_format(bytes: &[u8]) -> bool {
    shared::is_photo_format(bytes)
}

/// Removes the staged plaintext when dropped, including on early returns and panics.
struct Staged(PathBuf);
impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Private staging directory inside the binding's data directory. Leftovers from a crash are
/// purged once they are older than a few minutes.
fn staging_dir(data_dir: &Path) -> BridgeResult<PathBuf> {
    let dir = data_dir.join(PHOTO_STAGING_DIR);
    fs::create_dir_all(&dir).map_err(|_| {
        BridgeError::new(
            "attachment-local",
            "Local attachment storage is unavailable.",
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let stale = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > STAGING_MAX_AGE);
            if stale {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Ok(dir)
}

/// Normalizes the user's cropped image through core (`prepare_contact_photo`: re-encoded 256×256
/// JPEG, metadata stripped, encrypted into the media store) and returns the local attachment ID.
/// The plaintext exists only as a 0600 file in the private staging directory for the call.
pub fn prepare_photo(client: &Client, data_dir: &Path, bytes: &[u8]) -> BridgeResult<AttachmentId> {
    let staged =
        Staged(staging_dir(data_dir)?.join(format!("{}.img", uuid::Uuid::new_v4().simple())));
    fsutil::write_private_atomic(&staged.0, bytes).map_err(|_| {
        BridgeError::new(
            "attachment-local",
            "Local attachment storage is unavailable.",
        )
    })?;
    let info = client
        .prepare_contact_photo(&staged.0)
        .map_err(|error| match error {
            CoreError::InvalidRequest(_) => photo_error(),
            other => core_error(other),
        })?;
    Ok(info.attachment_id)
}

/// Natively picked image re-encoded for the webview cropper (bounded, metadata-free PNG).
pub fn photo_source(path: &Path) -> BridgeResult<Value> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| {
            file.take(shared::MAX_PHOTO_SOURCE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|_| photo_error())?;
    if bytes.len() > shared::MAX_PHOTO_SOURCE_BYTES || !is_photo_format(&bytes) {
        return Err(photo_error());
    }
    let (data_url, width, height) =
        media::thumbnail_data_url(&bytes, CROP_SOURCE_EDGE).ok_or_else(photo_error)?;
    Ok(json!({"dataUrl": data_url, "naturalWidth": width, "naturalHeight": height}))
}

// ---------------------------------------------------------------------------------------------
// Submit
// ---------------------------------------------------------------------------------------------

/// Validates and sends one edit request. Returns `{state:"pending", requestId}`; the owner's
/// decision arrives later through the ledger (`list_edits`).
pub fn submit_with(
    client: &Client,
    data_dir: &Path,
    own_device: &str,
    input: &Value,
) -> BridgeResult<Value> {
    let book_id = required_id(input, "targetBookId")?;
    let book = find_book(client, &book_id)?;
    shared::submit_with(
        input,
        &book,
        own_device,
        |bytes| prepare_photo(client, data_dir, bytes),
        |request| parse_core(client.request_contact_edit(&request.to_string())),
        |id| {
            let _ = client.discard_unreferenced_attachment(id);
        },
    )
}

pub fn submit(session: &Session, input: &Value) -> BridgeResult<Value> {
    let outcome = submit_with(
        &session.client,
        &session.data_dir,
        &session.binding.device_id,
        input,
    )?;
    session.request_work();
    session.notify();
    Ok(outcome)
}

// ---------------------------------------------------------------------------------------------
// Display-only resolution, recipient discovery and repair
// ---------------------------------------------------------------------------------------------

/// Core's per-call address cap for `resolve_contact_addresses_json`.
const RESOLVE_BATCH: usize = 100;
/// Distinct addresses resolved per snapshot; the rest show their raw number.
const MAX_RESOLVED_ADDRESSES: usize = 1000;
/// Avatars attached to one snapshot (decrypts are cached, so this bounds snapshot size).
const SNAPSHOT_AVATARS: usize = 64;
pub use shared::Resolved;

fn looks_like_phone(address: &str) -> bool {
    let digits = address.bytes().filter(u8::is_ascii_digit).count();
    (3..=15).contains(&digits)
        && address
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b' ' | b'(' | b')' | b'.' | b'-'))
}

/// Resolves phone-number addresses through core's bounded projection, preferring the given
/// source device's book. Ambiguous or unknown numbers are simply absent (shown raw). Purely
/// display data: failures (for example a locked vault) resolve nothing.
pub fn resolve_addresses(
    client: &Client,
    addresses: &[(String, Option<String>)],
) -> HashMap<String, Resolved> {
    let mut by_source: HashMap<Option<&str>, Vec<&str>> = HashMap::new();
    let mut seen = HashSet::new();
    for (address, source) in addresses {
        if address.len() <= 256 && looks_like_phone(address) && seen.insert(address.as_str()) {
            by_source
                .entry(source.as_deref().filter(|s| valid_id(s)))
                .or_default()
                .push(address);
        }
        if seen.len() >= MAX_RESOLVED_ADDRESSES {
            break;
        }
    }
    let mut out = HashMap::new();
    for (source, list) in by_source {
        for batch in list.chunks(RESOLVE_BATCH) {
            let mut input = json!({"addresses": batch});
            if let Some(source) = source {
                input["source_device_id"] = json!(source);
            }
            let Ok(raw) = parse_core(client.resolve_contact_addresses_json(&input.to_string()))
            else {
                return out;
            };
            for (address, contact) in shared::resolved_addresses_view(&raw) {
                out.entry(address).or_insert(contact);
            }
        }
    }
    out
}

/// Contact data carried in each UI snapshot. Every part is optional display data.
#[derive(Default)]
pub struct SnapshotContacts {
    pub resolution: Option<Map<String, Value>>,
    pub books: Option<Vec<Value>>,
    pub pending_count: Option<u64>,
    pub sync: Option<Value>,
}

pub fn snapshot_contacts(
    session: &Session,
    addresses: &[(String, Option<String>)],
) -> SnapshotContacts {
    let resolved = resolve_addresses(&session.client, addresses);
    let mut budget = SNAPSHOT_AVATARS;
    let resolution = resolved
        .into_iter()
        .map(|(address, contact)| {
            let mut view = json!({
                "contactId": contact.contact_id,
                "bookId": contact.book_id,
                "displayName": contact.display_name,
            });
            if let Some(url) = contact
                .photo
                .filter(|id| {
                    session
                        .client
                        .attachment_info(*id)
                        .is_ok_and(|info| info.state.is_local())
                })
                .and_then(|id| session_avatar(session, &mut budget, id))
            {
                view["photoDataUrl"] = json!(url);
            }
            (address, view)
        })
        .collect::<Map<_, _>>();
    let books = list_books(session).ok();
    let pending_count = books.as_ref().map(|books| {
        books
            .iter()
            .filter_map(|book| book.get("pendingEditCount").and_then(Value::as_u64))
            .sum()
    });
    SnapshotContacts {
        resolution: (!resolution.is_empty()).then_some(resolution),
        books,
        pending_count,
        sync: Some(sync_status(&session.client)),
    }
}

/// Repair latch and newest authoritative projection, so the UI shows failure or staleness
/// instead of treating contacts as current.
pub fn sync_status(client: &Client) -> Value {
    use peppy_client_core::SnapshotProjectionState as State;
    let readiness = client
        .contact_sync_readiness_json()
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok());
    let projection = client
        .snapshot_projection_status()
        .ok()
        .flatten()
        .map(|status| {
            let state = match status.state {
                State::Draining | State::Staging => "rebuilding",
                State::Promoted => "current",
                State::Failed => "failed",
            };
            (state, status.reason)
        });
    shared::sync_status(
        client.contact_repair_required().unwrap_or(false),
        readiness,
        projection
            .as_ref()
            .map(|(state, reason)| (*state, reason.as_deref())),
    )
}

/// Manual repair: latches core's projection repair and wakes the live loop, which runs one
/// fenced compaction snapshot. Owned books are never republished wholesale.
pub fn request_repair(session: &Session) -> BridgeResult<Value> {
    session
        .client
        .request_contact_repair()
        .map_err(core_error)?;
    session
        .repair_attempted
        .store(false, std::sync::atomic::Ordering::SeqCst);
    session.repair_wake.notify_one();
    session.request_work();
    session.notify();
    Ok(sync_status(&session.client))
}

/// Phone numbers of contacts matching `query` (names or digits) as recipient candidates. The
/// recipient is the phone address (core-normalized E.164 when the number is valid for the
/// book's region; otherwise the raw digits, e.g. a short code), never a contact ID.
pub fn search_recipients(
    session: &Session,
    query: &str,
    source_device_id: Option<&str>,
) -> BridgeResult<Vec<Value>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    if query.chars().count() > 128 {
        return Err(invalid("The contact search is too long."));
    }
    let mut input =
        json!({"schema_version": 1, "query": query, "limit": shared::MAX_SEARCH_RESULTS});
    if let Some(source) = source_device_id.filter(|s| valid_id(s)) {
        input["source_device_id"] = json!(source);
    }
    let raw = parse_core(
        session
            .client
            .search_contact_recipients_json(&input.to_string()),
    )?;
    let mut budget = shared::MAX_SEARCH_RESULTS as usize;
    Ok(raw
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            shared::recipient_view(row, &mut |id| {
                session
                    .client
                    .attachment_info(id)
                    .is_ok_and(|info| info.state.is_local())
                    .then(|| session_avatar(session, &mut budget, id))
                    .flatten()
            })
        })
        .collect())
}

/// Native banner title for a phone-number title (full previews only): the contact name when
/// the number resolves unambiguously, else the original title. Never used for hidden previews.
pub fn banner_title(client: &Client, title: &str, source_device_id: Option<&str>) -> String {
    resolve_addresses(
        client,
        &[(title.to_owned(), source_device_id.map(str::to_owned))],
    )
    .remove(title)
    .map_or_else(|| title.to_owned(), |contact| contact.display_name)
}

#[cfg(test)]
pub(crate) mod tests;
