//! Pure, sanitized contact projections and edit translation shared by native and browser hosts.
//! Host adapters own core calls, avatar decoding, staging files and effects.
use crate::{ContactError, ContactResult};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use peppy_client_core::AttachmentId;
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, HashSet},
    str::FromStr,
};

pub const PAGE_LIMIT: u32 = 200;
pub const MAX_SEARCH_RESULTS: u64 = 20;
const MAX_ID: usize = 1024;
pub const MAX_TEXT: usize = 1024;
const MAX_NOTES_BYTES: usize = 8192;
const MAX_VALUES: usize = 20;
const MAX_PATCHES: usize = 100;
pub const MAX_PHOTO_SOURCE_BYTES: usize = 8 * 1024 * 1024;
const ADDRESS_KEYS: [(&str, &str); 11] = [
    ("value", "formatted"),
    ("street", "street"),
    ("po_box", "poBox"),
    ("neighborhood", "neighborhood"),
    ("sub_locality", "subLocality"),
    ("city", "city"),
    ("sub_administrative_area", "subAdministrativeArea"),
    ("state", "state"),
    ("postal_code", "postalCode"),
    ("country", "country"),
    ("iso_country_code", "isoCountryCode"),
];

fn invalid(message: &'static str) -> ContactError {
    ContactError::ui("invalid-contact-edit", message)
}

pub fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_ID
}

pub fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Reads an optional UI text value: absent and `null` are empty; anything else must be a string
/// within `max` (characters, or bytes for notes).
fn ui_text(value: Option<&Value>, max: usize, bytes: bool) -> ContactResult<String> {
    match value {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(text)) => {
            let size = if bytes {
                text.len()
            } else {
                text.chars().count()
            };
            if size <= max {
                Ok(text.clone())
            } else {
                Err(invalid("A contact value is too long."))
            }
        }
        Some(_) => Err(invalid("A contact value is invalid.")),
    }
}

pub fn required_id(input: &Value, key: &str) -> ContactResult<String> {
    str_field(input, key)
        .filter(|id| valid_id(id))
        .map(str::to_owned)
        .ok_or_else(|| invalid("A required contact identifier is missing."))
}

/// Core revisions are canonical non-negative decimal strings.
pub fn required_revision(input: &Value) -> ContactResult<String> {
    let revision = required_id(input, "baseRevision")?;
    let canonical = revision.bytes().all(|b| b.is_ascii_digit())
        && (revision == "0" || !revision.starts_with('0'))
        && revision.len() <= 18;
    canonical
        .then_some(revision)
        .ok_or_else(|| invalid("The contact revision is invalid."))
}

// ---------------------------------------------------------------------------------------------
// Books
// ---------------------------------------------------------------------------------------------

/// What this device may ask of a book. The owner still enforces policy and capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub can_write: bool,
    pub supports_notes: bool,
    pub supports_photo: bool,
    pub supports_birthday: bool,
    /// Any device holding a retained tombstone may ask the owner to recreate it.
    pub can_restore: bool,
}

pub fn capabilities(book: &Value, own_device: &str) -> Capabilities {
    let caps = book.get("capabilities");
    let flag = |key: &str| caps.and_then(|c| c.get(key)).and_then(Value::as_bool);
    let owned = str_field(book, "owner_device_id") == Some(own_device);
    let live = matches!(str_field(book, "state"), Some("active" | "limited"));
    let remote_edits = book.pointer("/policy/remote_edits").and_then(Value::as_str);
    // A book whose every account is read-only cannot take edits even if `write` is set.
    let accounts = book.get("accounts").and_then(Value::as_array);
    let all_read_only = accounts.is_some_and(|accounts| {
        !accounts.is_empty()
            && accounts
                .iter()
                .all(|a| a.get("writable").and_then(Value::as_bool) == Some(false))
    });
    let can_write = !owned
        && live
        && flag("write") == Some(true)
        && remote_edits != Some("off")
        && !all_read_only;
    Capabilities {
        can_write,
        // iOS advertises `notes:false` (entitlement); books without the flag accept notes.
        supports_notes: can_write && flag("notes") != Some(false),
        supports_photo: can_write && flag("photo") == Some(true),
        supports_birthday: can_write && flag("birthday") != Some(false),
        can_restore: can_write,
    }
}

pub fn book_state(book: &Value) -> &'static str {
    match str_field(book, "state") {
        Some("active") => "active",
        Some("limited") => "limited",
        Some("retired") => "retired",
        _ => "unavailable",
    }
}

pub fn book_view(
    book: &Value,
    own_device: &str,
    device_names: &HashMap<String, String>,
    pending: &HashMap<String, usize>,
) -> Option<Value> {
    let id = str_field(book, "id").filter(|id| valid_id(id))?;
    let caps = capabilities(book, own_device);
    let device_name = str_field(book, "owner_device_id")
        .and_then(|owner| device_names.get(owner))
        .map_or("Phone", String::as_str);
    let mut view = json!({
        "id": id,
        "deviceName": device_name,
        "state": book_state(book),
        "capabilities": {
            "canWrite": caps.can_write,
            "canDelete": caps.can_write,
            "canRestore": caps.can_restore,
            "supportsNotes": caps.supports_notes,
            "supportsPhoto": caps.supports_photo,
            "supportsBirthday": caps.supports_birthday,
        },
        "contactCount": book.get("contact_count").and_then(Value::as_u64).unwrap_or(0),
        "pendingEditCount": pending.get(id).copied().unwrap_or(0),
    });
    // Synced books carry account names but not which is the owner's default; a single
    // account is unambiguous.
    if let Some([account]) = book
        .get("accounts")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        && let Some(name) = str_field(account, "name").filter(|n| !n.is_empty())
    {
        view["defaultAccountLabel"] = json!(name.chars().take(MAX_TEXT).collect::<String>());
    }
    Some(view)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Phones,
    Emails,
    Addresses,
}

impl List {
    fn from_field(field: &str) -> Option<Self> {
        match field {
            "phones" => Some(Self::Phones),
            "emails" => Some(Self::Emails),
            "addresses" => Some(Self::Addresses),
            _ => None,
        }
    }
    fn core_key(self) -> &'static str {
        match self {
            Self::Phones => "phones",
            Self::Emails => "emails",
            Self::Addresses => "addresses",
        }
    }
    /// View key of the single value of a phone or email.
    fn value_key(self) -> &'static str {
        match self {
            Self::Phones => "number",
            Self::Emails => "address",
            Self::Addresses => "",
        }
    }
    fn known_core_key(self, key: &str) -> bool {
        matches!(key, "id" | "label" | "read_only" | "writable")
            || match self {
                Self::Phones | Self::Emails => key == "value",
                Self::Addresses => ADDRESS_KEYS.iter().any(|(core, _)| *core == key),
            }
    }
}

/// One core list item as a view. Items carrying keys this view cannot show are read-only, because
/// a replacement built from the view would drop those keys on the owner's device.
pub fn item_view(list: List, item: &Value) -> Option<Value> {
    let obj = item.as_object()?;
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| valid_id(id))?;
    let read_only = obj.get("read_only").and_then(Value::as_bool) == Some(true)
        || obj.get("writable").and_then(Value::as_bool) == Some(false)
        || obj.keys().any(|key| !list.known_core_key(key));
    let label = obj.get("label").and_then(Value::as_str).unwrap_or("");
    let mut out = Map::new();
    out.insert("id".into(), json!(id));
    // `label` is the wire value and must round-trip unchanged; `displayLabel` is for people.
    out.insert("label".into(), json!(label));
    out.insert("displayLabel".into(), json!(display_label(label)));
    match list {
        List::Phones | List::Emails => {
            out.insert(
                list.value_key().into(),
                json!(obj.get("value").and_then(Value::as_str).unwrap_or("")),
            );
        }
        List::Addresses => {
            for (core, view) in ADDRESS_KEYS {
                if let Some(text) = obj.get(core).and_then(Value::as_str) {
                    out.insert(view.into(), json!(text));
                }
            }
        }
    }
    out.insert("readOnly".into(), json!(read_only));
    Some(Value::Object(out))
}

/// Human label for a wire label. Apple's built-in labels arrive raw as `_$!<Mobile>!$_`.
pub fn display_label(label: &str) -> String {
    label
        .strip_prefix("_$!<")
        .and_then(|rest| rest.strip_suffix(">!$_"))
        .map_or_else(|| label.to_owned(), str::to_lowercase)
}

/// Core birthday `{year?, month, day}` as a view value (numbers only).
pub fn birthday_view(value: Option<&Value>) -> Option<Value> {
    let value = value?.as_object()?;
    let part = |key: &str| value.get(key).and_then(Value::as_i64);
    let (month, day) = (part("month")?, part("day")?);
    let mut out = json!({"month": month, "day": day});
    if let Some(year) = part("year") {
        out["year"] = json!(year);
    }
    Some(out)
}

pub fn list_view(contact: &Value, list: List) -> Value {
    Value::Array(
        contact
            .get(list.core_key())
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item_view(list, item))
            .collect(),
    )
}

pub fn copy_text(out: &mut Map<String, Value>, view_key: &str, value: Option<&Value>) {
    if let Some(text) = value.and_then(Value::as_str) {
        out.insert(view_key.into(), json!(text));
    }
}

/// Core view photo `{attachment_id, available}`; the ID is resolved natively and never returned.
pub fn photo_reference(contact: &Value) -> Option<(AttachmentId, bool)> {
    let photo = contact.get("photo")?;
    let id = AttachmentId::from_str(str_field(photo, "attachment_id")?).ok()?;
    Some((
        id,
        photo.get("available").and_then(Value::as_bool) == Some(true),
    ))
}

/// Whitelisted contact view. `avatar` turns an available local photo into a data URL.
pub fn contact_view(
    contact: &Value,
    book_id: &str,
    avatar: &mut dyn FnMut(AttachmentId) -> Option<String>,
) -> Option<Value> {
    let id = str_field(contact, "id").filter(|id| valid_id(id))?;
    let mut out = Map::new();
    out.insert("id".into(), json!(id));
    out.insert("bookId".into(), json!(book_id));
    copy_text(&mut out, "revision", contact.get("revision"));
    out.insert(
        "displayName".into(),
        json!(
            str_field(contact, "display_name")
                .filter(|name| !name.is_empty())
                .unwrap_or("Contact")
        ),
    );
    let name = contact.get("name");
    copy_text(&mut out, "givenName", name.and_then(|n| n.get("given")));
    copy_text(&mut out, "familyName", name.and_then(|n| n.get("family")));
    for key in ["nickname", "organization", "title", "notes"] {
        copy_text(&mut out, key, contact.get(key));
    }
    if let Some(birthday) = birthday_view(contact.get("birthday")) {
        out.insert("birthday".into(), birthday);
    }
    out.insert("phones".into(), list_view(contact, List::Phones));
    out.insert("emails".into(), list_view(contact, List::Emails));
    out.insert("addresses".into(), list_view(contact, List::Addresses));
    if let Some((photo, available)) = photo_reference(contact) {
        match available.then(|| avatar(photo)).flatten() {
            Some(url) => {
                out.insert("photoDataUrl".into(), json!(url));
            }
            None if !available => {
                out.insert("photoPending".into(), json!(true));
            }
            None => {}
        }
    }
    Some(Value::Object(out))
}

/// `YYYY-MM-DD` (UTC) for a Unix timestamp (Howard Hinnant's civil-from-days).
pub fn utc_date(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn edit_state(status: &str, expires_at: i64, now: i64) -> Option<&'static str> {
    Some(match status {
        "requested" | "approved" | "awaiting_approval" if expires_at <= now => "expired",
        "requested" | "approved" | "applying" => "pending",
        "awaiting_approval" => "awaiting-approval",
        "outcome_unknown" => "outcome-unknown",
        "applied" => "applied",
        "conflict" => "conflict",
        "rejected" => "rejected",
        "expired" => "expired",
        "failed" => "failed",
        _ => return None,
    })
}

pub fn safe_reason(result: &Value) -> Option<&str> {
    str_field(result, "reason").filter(|reason| {
        !reason.is_empty()
            && reason.len() <= 64
            && reason.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    })
}

pub fn edit_summary(request: &Value) -> String {
    let mut fields: Vec<&str> = Vec::new();
    for path in request
        .get("field_paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let root = path.split(['.', '[']).next().unwrap_or(path);
        let label = match root {
            "name" | "display_name" => "name",
            "phones" => "phone numbers",
            "emails" => "email addresses",
            "addresses" => "addresses",
            "notes" => "notes",
            "photo" => "photo",
            "birthday" => "birthday",
            "nickname" => "nickname",
            "organization" | "title" => "work details",
            _ => continue,
        };
        if !fields.contains(&label) {
            fields.push(label);
        }
    }
    match str_field(request, "kind") {
        Some("create") => "New contact".to_owned(),
        Some("delete") => "Delete contact".to_owned(),
        _ if fields.is_empty() => "Contact change".to_owned(),
        _ => {
            let joined = fields.join(", ");
            let mut chars = joined.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        }
    }
}

/// The edit to badge on each contact: the newest unresolved request, or a recent failed one
/// (conflict, rejected, failed, expired) until a day after its expiry. Applied edits clear.
pub fn contact_badges(edits: &[Value], now: i64) -> HashMap<String, Value> {
    let mut badges = HashMap::new();
    for edit in edits {
        let (Some(contact), Some(state)) = (str_field(edit, "contactId"), str_field(edit, "state"))
        else {
            continue;
        };
        if badges.contains_key(contact) {
            continue;
        }
        let unresolved = matches!(state, "pending" | "awaiting-approval" | "outcome-unknown");
        let recent = edit
            .get("expiresAt")
            .and_then(Value::as_i64)
            .is_some_and(|expires| now < expires.saturating_add(24 * 60 * 60));
        if state == "applied" {
            badges.insert(contact.to_owned(), Value::Null);
        } else if unresolved || recent {
            badges.insert(contact.to_owned(), edit.clone());
        }
    }
    badges.retain(|_, edit| !edit.is_null());
    badges
}

pub fn apply_badge(view: &mut Value, edit: &Value) {
    view["pendingEditId"] = edit["requestId"].clone();
    view["pendingEditState"] = edit["state"].clone();
    view["pendingEditSummary"] = edit["summary"].clone();
}

pub fn pending_counts(edits: &[Value]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for edit in edits {
        if matches!(
            str_field(edit, "state"),
            Some("pending" | "awaiting-approval" | "outcome-unknown")
        ) && let Some(book) = str_field(edit, "bookId")
        {
            *counts.entry(book.to_owned()).or_default() += 1;
        }
    }
    counts
}

// ---------------------------------------------------------------------------------------------
// Edit requests
// ---------------------------------------------------------------------------------------------

/// Core list item from a view item. `None` when the item carries no value (a blank row).
pub fn wire_item(list: List, item: &Value, id: &str) -> ContactResult<Option<Value>> {
    let obj = item
        .as_object()
        .ok_or_else(|| invalid("A contact list entry is invalid."))?;
    let mut out = Map::new();
    out.insert("id".into(), json!(id));
    let label = ui_text(obj.get("label"), MAX_TEXT, false)?;
    if !label.is_empty() {
        out.insert("label".into(), json!(label));
    }
    let mut has_value = false;
    let mut put = |core: &str, view: &str| -> ContactResult<()> {
        let text = ui_text(obj.get(view), MAX_TEXT, false)?;
        if !text.trim().is_empty() {
            out.insert(core.into(), json!(text));
            has_value = true;
        }
        Ok(())
    };
    match list {
        List::Phones | List::Emails => put("value", list.value_key())?,
        List::Addresses => {
            for (core, view) in ADDRESS_KEYS {
                put(core, view)?;
            }
        }
    }
    Ok(has_value.then_some(Value::Object(out)))
}

/// A field ID from the view; core item paths are `list[<id>]`, so brackets are refused.
pub fn item_id(item: &Value) -> ContactResult<Option<String>> {
    match item.get("id") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(id)) if valid_id(id) && !id.contains(['[', ']']) => Ok(Some(id.clone())),
        Some(_) => Err(invalid("A contact field ID is invalid.")),
    }
}

pub fn ui_items(value: Option<&Value>) -> ContactResult<&[Value]> {
    match value {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(items)) if items.len() <= MAX_VALUES * 2 => Ok(items),
        Some(_) => Err(invalid("Contact list changes are invalid.")),
    }
}

/// Items for a new contact: blank rows are dropped and every item gets a fresh field ID.
pub fn new_items(list: List, value: Option<&Value>) -> ContactResult<Vec<Value>> {
    let mut items = Vec::new();
    for item in ui_items(value)? {
        if let Some(wire) = wire_item(list, item, &uuid::Uuid::new_v4().to_string())? {
            items.push(wire);
        }
    }
    if items.len() > MAX_VALUES {
        return Err(invalid("A contact can have at most 20 entries per list."));
    }
    Ok(items)
}

/// Patches turning `old` (the view's original items) into `next`. Existing items keep their IDs;
/// blank or missing items are removed, new ones are added with fresh IDs. Returns the patches and
/// the original list in core form for `expected_old`.
pub fn list_patches(
    list: List,
    next: Option<&Value>,
    old: Option<&Value>,
) -> ContactResult<(Vec<Value>, Value)> {
    let key = list.core_key();
    let old = match old {
        Some(Value::Array(items)) => items,
        _ => {
            return Err(invalid(
                "Contact list changes require their original values.",
            ));
        }
    };
    let mut old_wire = Vec::with_capacity(old.len());
    let mut read_only = HashSet::new();
    for item in old {
        let id = item_id(item)?.ok_or_else(|| invalid("An original contact field has no ID."))?;
        if item.get("readOnly").and_then(Value::as_bool) == Some(true) {
            read_only.insert(id.clone());
        }
        let wire = wire_item(list, item, &id)?.unwrap_or_else(|| json!({"id": id}));
        old_wire.push((id, wire));
    }
    let mut kept = HashMap::new();
    let mut added = Vec::new();
    for item in ui_items(next)? {
        match item_id(item)? {
            Some(id) => {
                if !old_wire.iter().any(|(old_id, _)| *old_id == id) {
                    return Err(invalid(
                        "A contact field ID does not belong to this contact.",
                    ));
                }
                if kept
                    .insert(id.clone(), wire_item(list, item, &id)?)
                    .is_some()
                {
                    return Err(invalid("A contact field appears twice."));
                }
            }
            None => added.extend(wire_item(list, item, &uuid::Uuid::new_v4().to_string())?),
        }
    }
    let mut patches = Vec::new();
    let mut remaining = 0;
    for (id, previous) in &old_wire {
        let path = format!("{key}[{id}]");
        match kept.remove(id).flatten() {
            Some(current) if current == *previous => remaining += 1,
            current => {
                if read_only.contains(id) {
                    return Err(ContactError::ui(
                        "contact-field-read-only",
                        "A read-only contact field cannot be changed from this computer.",
                    ));
                }
                match current {
                    Some(current) => {
                        remaining += 1;
                        patches.push(json!({"op": "replace", "path": path, "value": current}));
                    }
                    None => patches.push(json!({"op": "remove", "path": path})),
                }
            }
        }
    }
    if remaining + added.len() > MAX_VALUES {
        return Err(invalid("A contact can have at most 20 entries per list."));
    }
    patches.extend(
        added
            .into_iter()
            .map(|item| json!({"op": "add", "path": key, "value": item})),
    );
    let old_core = Value::Array(old_wire.into_iter().map(|(_, wire)| wire).collect());
    Ok((patches, old_core))
}

/// Scalar field → (core path, limit, limit counts bytes, is notes).
pub fn scalar(field: &str) -> Option<(&'static str, usize, bool)> {
    Some(match field {
        "givenName" => ("name.given", MAX_TEXT, false),
        "familyName" => ("name.family", MAX_TEXT, false),
        "nickname" => ("nickname", MAX_TEXT, false),
        "organization" => ("organization", MAX_TEXT, false),
        "title" => ("title", MAX_TEXT, false),
        "notes" => ("notes", MAX_NOTES_BYTES, true),
        _ => return None,
    })
}

/// A view birthday as core `{year?, month, day}`: absent, `null` or all-blank parts mean no
/// birthday. Parts may be numbers or numeric strings (form inputs); core validates the date.
pub fn ui_birthday(value: Option<&Value>) -> ContactResult<Option<Value>> {
    let bad = || invalid("The birthday is invalid.");
    let parts = match value {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(parts)) => parts,
        Some(_) => return Err(bad()),
    };
    let mut out = Map::new();
    for (key, part) in parts {
        if !matches!(key.as_str(), "year" | "month" | "day") {
            return Err(bad());
        }
        let number = match part {
            Value::Null => continue,
            Value::String(text) if text.trim().is_empty() => continue,
            Value::String(text) => text.trim().parse::<i64>().map_err(|_| bad())?,
            Value::Number(n) => n.as_i64().ok_or_else(bad)?,
            _ => return Err(bad()),
        };
        out.insert(key.clone(), json!(number));
    }
    if out.is_empty() {
        return Ok(None);
    }
    let month = out.get("month").and_then(Value::as_i64).ok_or_else(bad)?;
    let day = out.get("day").and_then(Value::as_i64).ok_or_else(bad)?;
    let year_ok = out
        .get("year")
        .and_then(Value::as_i64)
        .is_none_or(|year| (1..=9999).contains(&year));
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || !year_ok {
        return Err(bad());
    }
    Ok(Some(Value::Object(out)))
}

pub fn require_birthday_support(caps: &Capabilities) -> ContactResult<()> {
    if caps.supports_birthday {
        Ok(())
    } else {
        Err(ContactError::ui(
            "contact-field-unsupported",
            "This phone does not accept contact birthdays.",
        ))
    }
}

pub fn reject_notes(caps: &Capabilities, notes: &str) -> ContactResult<()> {
    if notes.is_empty() || caps.supports_notes {
        Ok(())
    } else {
        Err(ContactError::ui(
            "contact-field-unsupported",
            "This phone does not accept contact notes.",
        ))
    }
}

/// UI patches as (field, entry), refusing duplicates and oversized inputs.
pub fn ui_patches(input: &Value) -> ContactResult<Vec<(&str, &Value)>> {
    let patches = match input.get("patches") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(patches)) if patches.len() <= MAX_PATCHES => patches,
        Some(_) => return Err(invalid("Contact changes are invalid.")),
    };
    let mut seen = HashSet::new();
    patches
        .iter()
        .map(|patch| {
            let field =
                str_field(patch, "field").ok_or_else(|| invalid("A contact field is invalid."))?;
            if !seen.insert(field) {
                return Err(invalid("A contact field was changed twice."));
            }
            Ok((field, patch))
        })
        .collect()
}

pub fn create_fields(input: &Value, caps: &Capabilities) -> ContactResult<Map<String, Value>> {
    let mut fields = Map::new();
    let mut name = Map::new();
    for (field, patch) in ui_patches(input)? {
        let value = patch.get("value");
        if let Some(list) = List::from_field(field) {
            let items = new_items(list, value)?;
            if !items.is_empty() {
                fields.insert(list.core_key().into(), Value::Array(items));
            }
        } else if let Some((path, max, bytes)) = scalar(field) {
            let text = ui_text(value, max, bytes)?;
            if path == "notes" {
                reject_notes(caps, &text)?;
            }
            if text.trim().is_empty() {
                continue;
            }
            match path.strip_prefix("name.") {
                Some(part) => name.insert(part.into(), json!(text)),
                None => fields.insert(path.into(), json!(text)),
            };
        } else if field == "birthday" {
            if let Some(birthday) = ui_birthday(value)? {
                require_birthday_support(caps)?;
                fields.insert("birthday".into(), birthday);
            }
        } else {
            return Err(invalid(
                "This contact field cannot be created from this computer.",
            ));
        }
    }
    let first_value = |list: &str| {
        fields
            .get(list)
            .and_then(|items| items.get(0))
            .and_then(|item| str_field(item, "value"))
            .map(str::to_owned)
    };
    let person = ["given", "family"]
        .iter()
        .filter_map(|part| name.get(*part).and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let display = [Some(person)]
        .into_iter()
        .chain(
            ["nickname", "organization"]
                .map(|key| fields.get(key).and_then(Value::as_str).map(str::to_owned)),
        )
        .chain([first_value("phones"), first_value("emails")])
        .flatten()
        .find(|candidate| !candidate.trim().is_empty())
        .ok_or_else(|| invalid("A new contact needs a name, phone number or email address."))?;
    fields.insert(
        "display_name".into(),
        json!(display.chars().take(MAX_TEXT).collect::<String>()),
    );
    if !name.is_empty() {
        fields.insert("name".into(), Value::Object(name));
    }
    Ok(fields)
}

/// Update patches and the matching `expected_old` subset of the original contact.
pub fn update_patches(
    input: &Value,
    caps: &Capabilities,
) -> ContactResult<(Vec<Value>, Map<String, Value>)> {
    let mut patches = Vec::new();
    let mut expected = Map::new();
    for (field, patch) in ui_patches(input)? {
        let value = patch.get("value");
        if let Some(list) = List::from_field(field) {
            let (changes, previous) = list_patches(list, value, patch.get("expectedOld"))?;
            if !changes.is_empty() {
                patches.extend(changes);
                expected.insert(list.core_key().into(), previous);
            }
        } else if let Some((path, max, bytes)) = scalar(field) {
            let text = ui_text(value, max, bytes)?;
            if path == "notes" {
                reject_notes(caps, &text)?;
            }
            let old = ui_text(patch.get("expectedOld"), max, bytes)?;
            if text == old {
                continue;
            }
            patches.push(if text.is_empty() {
                json!({"op": "remove", "path": path})
            } else {
                json!({"op": "replace", "path": path, "value": text})
            });
            let old = if old.is_empty() {
                Value::Null
            } else {
                json!(old)
            };
            match path.split_once('.') {
                Some((root, part)) => {
                    let parent = expected.entry(root).or_insert_with(|| json!({}));
                    parent[part] = old;
                }
                None => {
                    expected.insert(path.into(), old);
                }
            }
        } else if field == "birthday" {
            let next = ui_birthday(value)?;
            let old = ui_birthday(patch.get("expectedOld"))?;
            if next == old {
                continue;
            }
            require_birthday_support(caps)?;
            patches.push(match &next {
                Some(birthday) => json!({"op": "replace", "path": "birthday", "value": birthday}),
                None => json!({"op": "remove", "path": "birthday"}),
            });
            expected.insert("birthday".into(), old.unwrap_or(Value::Null));
        } else {
            return Err(invalid(
                "This contact field cannot be changed from this computer.",
            ));
        }
    }
    if patches.len() > MAX_PATCHES {
        return Err(invalid("Too many contact changes."));
    }
    Ok((patches, expected))
}

/// Builds a core `ContactEditRequest` (without `photo_op`) for `book`. `photo_change` says whether
/// the caller will add a photo operation, which alone makes an update non-empty.
pub fn build_request(
    input: &Value,
    book: &Value,
    own_device: &str,
    photo_change: bool,
) -> ContactResult<Value> {
    let kind = str_field(input, "kind").unwrap_or("");
    if !matches!(kind, "create" | "update" | "delete") {
        return Err(invalid("The contact operation is invalid."));
    }
    let book_id = required_id(input, "targetBookId")?;
    if str_field(book, "id") != Some(book_id.as_str()) {
        return Err(invalid("The contact book is invalid."));
    }
    let owner = str_field(book, "owner_device_id")
        .filter(|owner| valid_id(owner))
        .ok_or_else(|| ContactError::ui("host-state", "Native state is unavailable."))?;
    let caps = capabilities(book, own_device);
    if !caps.can_write {
        return Err(ContactError::ui(
            "contact-book-read-only",
            "This contact book does not accept changes from this computer.",
        ));
    }
    if photo_change && (kind == "delete" || !caps.supports_photo) {
        return Err(ContactError::ui(
            "contact-field-unsupported",
            "This phone does not accept contact photo changes.",
        ));
    }
    let mut request = json!({
        "schema_version": 1,
        "request_id": uuid::Uuid::new_v4().to_string(),
        "target_owner": owner,
        "book_id": book_id,
        "kind": kind,
    });
    match kind {
        "create" => {
            for (key, value) in create_fields(input, &caps)? {
                request[key] = value;
            }
        }
        "update" => {
            request["contact_id"] = json!(required_id(input, "contactId")?);
            request["base_revision"] = json!(required_revision(input)?);
            let (patches, expected) = update_patches(input, &caps)?;
            if patches.is_empty() && !photo_change {
                return Err(invalid("No contact changes were made."));
            }
            request["patches"] = Value::Array(patches);
            request["expected_old"] = Value::Object(expected);
        }
        _ => {
            request["contact_id"] = json!(required_id(input, "contactId")?);
            request["base_revision"] = json!(required_revision(input)?);
        }
    }
    Ok(request)
}

// ---------------------------------------------------------------------------------------------
// Photos
// ---------------------------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub enum PhotoInput {
    Keep,
    Remove,
    Set(Vec<u8>),
}

pub fn photo_error() -> ContactError {
    ContactError::ui(
        "contact-photo-invalid",
        "The photo could not be used. Choose a JPEG, PNG or WebP image up to 8 MB.",
    )
}

/// The cropper's `data:image/{png,jpeg,webp};base64,…` output, size-bounded and sniffed.
pub fn decode_photo_data_url(url: &str) -> ContactResult<Vec<u8>> {
    let (header, data) = url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(','))
        .ok_or_else(photo_error)?;
    if !matches!(
        header,
        "image/png;base64" | "image/jpeg;base64" | "image/webp;base64"
    ) || data.len() > MAX_PHOTO_SOURCE_BYTES / 3 * 4 + 4
    {
        return Err(photo_error());
    }
    let bytes = STANDARD.decode(data).map_err(|_| photo_error())?;
    if bytes.len() > MAX_PHOTO_SOURCE_BYTES || !is_photo_format(&bytes) {
        return Err(photo_error());
    }
    Ok(bytes)
}

pub fn photo_input(input: &Value) -> ContactResult<PhotoInput> {
    let Some(photo) = input.get("photo").filter(|photo| !photo.is_null()) else {
        return Ok(PhotoInput::Keep);
    };
    match str_field(photo, "kind") {
        Some("keep") => Ok(PhotoInput::Keep),
        Some("remove") => Ok(PhotoInput::Remove),
        Some("set") => str_field(photo, "croppedDataUrl")
            .ok_or_else(photo_error)
            .and_then(decode_photo_data_url)
            .map(PhotoInput::Set),
        _ => Err(invalid("The contact photo change is invalid.")),
    }
}

/// Sanitizes a raw requester ledger response. The caller supplies its own requester ID to core
/// and filters by book here; book IDs are never used as requester identities.
pub fn requester_ledger_view(
    raw: &Value,
    book_id: Option<&str>,
    now: i64,
) -> ContactResult<Vec<Value>> {
    Ok(raw.get("requests").and_then(Value::as_array).ok_or_else(|| ContactError::ui("host-state", "Native state is unavailable."))?
        .iter().filter(|request| book_id.is_none_or(|book| str_field(request, "book_id") == Some(book)))
        .filter_map(|request| {
            let expires_at = request.get("expires_at").and_then(Value::as_i64)?;
            let state = edit_state(str_field(request, "state")?, expires_at, now)?;
            let mut out = json!({"requestId": str_field(request, "request_id")?, "bookId": str_field(request, "book_id")?, "state": state, "expiresAt": expires_at, "summary": edit_summary(request)});
            if let Some(reason) = request.get("observed").and_then(safe_reason) { out["reason"] = json!(reason); }
            if let Some(kind) = str_field(request, "kind").filter(|k| matches!(*k, "create" | "update" | "delete")) { out["kind"] = json!(kind); }
            if let Some(contact) = str_field(request, "contact_id").filter(|id| valid_id(id)) { out["contactId"] = json!(contact); }
            if let Some(name) = str_field(request, "display_name").filter(|n| !n.is_empty()) { out["displayName"] = json!(name.chars().take(MAX_TEXT).collect::<String>()); }
            Some(out)
        }).collect())
}

/// Sanitized retained-tombstone projection. A host may inject only an already verified avatar URL.
pub fn restorable_view(
    contact: &Value,
    book_id: &str,
    avatar: &mut dyn FnMut(AttachmentId) -> Option<String>,
) -> Option<Value> {
    let id = str_field(contact, "id").filter(|id| valid_id(id))?;
    let mut out = json!({"id": id, "bookId": book_id, "displayName": str_field(contact, "display_name").filter(|n| !n.is_empty()).unwrap_or("Contact"), "deletedAt": utc_date(contact.get("deleted_at").and_then(Value::as_i64)?)});
    if let Some((photo, true)) = photo_reference(contact)
        && let Some(url) = avatar(photo)
    {
        out["photoDataUrl"] = json!(url);
    }
    Some(out)
}

/// Sanitized recipient row. Raw provider/photo descriptors never cross this boundary.
pub fn recipient_view(
    row: &Value,
    avatar: &mut dyn FnMut(AttachmentId) -> Option<String>,
) -> Option<Value> {
    let address = str_field(row, "address")?;
    let mut out = json!({"address": address, "displayName": str_field(row, "display_name").filter(|n| !n.is_empty()).unwrap_or(address), "number": str_field(row, "value").unwrap_or(address), "normalized": row.get("normalized").and_then(Value::as_bool) == Some(true), "contactId": str_field(row, "contact_id")?, "phoneId": str_field(row, "phone_id")?});
    if let Some(label) = str_field(row, "label") {
        out["label"] = json!(display_label(label));
    }
    if let Some(url) = str_field(row, "photo_attachment_id")
        .and_then(|id| AttachmentId::from_str(id).ok())
        .and_then(avatar)
    {
        out["avatarUrl"] = json!(url);
    }
    Some(out)
}

/// Pure magic-byte validation: declared data URLs must contain a PNG, JPEG or WebP.
pub fn is_photo_format(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(&[0xff, 0xd8, 0xff])
        || bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_never_leaks_raw_photo_or_provider_descriptors() {
        let raw = json!({
            "id": "c1", "display_name": "Ada", "photo": {"attachment_id": "0190a5b2-7c1f-7e4a-9b1e-123456789abc", "available": false},
            "phones": [{"id": "p1", "value": "+12025550100", "provider_id": "secret", "file_key": "secret"}],
            "provider_id": "secret", "path": "/private/contact"
        });
        let view = contact_view(&raw, "book", &mut |_| None).unwrap();
        let text = view.to_string();
        assert!(view["phones"][0]["readOnly"].as_bool().unwrap());
        for forbidden in [
            "attachment_id",
            "0190a5b2",
            "provider_id",
            "file_key",
            "/private",
        ] {
            assert!(!text.contains(forbidden), "leaked {forbidden}: {text}");
        }
    }

    #[test]
    fn update_keeps_field_ids_and_expected_old() {
        let book = json!({"id":"book", "owner_device_id":"owner", "state":"active", "capabilities":{"write":true}, "policy":{"remote_edits":"auto"}});
        let input = json!({
            "targetBookId":"book", "kind":"update", "contactId":"c1", "baseRevision":"1",
            "patches":[{"field":"phones", "expectedOld":[{"id":"p1","label":"mobile","number":"1","readOnly":false}], "value":[{"id":"p1","label":"mobile","number":"2","readOnly":false}]}]
        });
        let request = build_request(&input, &book, "requester", false).unwrap();
        assert_eq!(
            request["patches"],
            json!([{"op":"replace","path":"phones[p1]","value":{"id":"p1","label":"mobile","value":"2"}}])
        );
        assert_eq!(
            request["expected_old"]["phones"],
            json!([{"id":"p1","label":"mobile","value":"1"}])
        );
    }
}

/// Shared edit submission. Hosts provide the narrowly scoped photo preparation, request delivery,
/// and best-effort orphan discard effects; all edit shape and pending-state semantics stay here.
pub fn submit_with<E, Prepare, Request, Discard>(
    input: &Value,
    book: &Value,
    own_device: &str,
    mut prepare_photo: Prepare,
    mut request_edit: Request,
    mut discard: Discard,
) -> Result<Value, E>
where
    E: From<ContactError>,
    Prepare: FnMut(&[u8]) -> Result<AttachmentId, E>,
    Request: FnMut(&Value) -> Result<Value, E>,
    Discard: FnMut(AttachmentId),
{
    let photo = photo_input(input).map_err(E::from)?;
    let mut request = build_request(input, book, own_device, !matches!(photo, PhotoInput::Keep))
        .map_err(E::from)?;
    let prepared = match photo {
        PhotoInput::Keep => None,
        PhotoInput::Remove => {
            request["photo_op"] = json!({"op": "remove"});
            None
        }
        PhotoInput::Set(bytes) => {
            let id = prepare_photo(&bytes)?;
            request["photo_op"] = json!({"op": "set", "attachment_id": id.to_string()});
            Some(id)
        }
    };
    let result = match request_edit(&request) {
        Ok(result) => result,
        Err(error) => {
            if let Some(id) = prepared {
                discard(id);
            }
            return Err(error);
        }
    };
    let request_id = str_field(&result, "request_id").ok_or_else(|| {
        E::from(ContactError::ui(
            "host-state",
            "Native state is unavailable.",
        ))
    })?;
    Ok(json!({"state": "pending", "requestId": request_id}))
}

/// Sanitizes core's resolve-addresses response. It deliberately accepts only display fields.
pub fn resolved_addresses_view(raw: &Value) -> HashMap<String, Resolved> {
    raw.get("matches")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let (address, contact_id, book_id, display_name) = (
                str_field(item, "address")?,
                str_field(item, "contact_id")?,
                str_field(item, "book_id")?,
                str_field(item, "display_name").filter(|name| !name.trim().is_empty())?,
            );
            Some((
                address.to_owned(),
                Resolved {
                    contact_id: contact_id.to_owned(),
                    book_id: book_id.to_owned(),
                    display_name: display_name.chars().take(MAX_TEXT).collect(),
                    photo: str_field(item, "photo_attachment_id")
                        .and_then(|id| AttachmentId::from_str(id).ok()),
                },
            ))
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub contact_id: String,
    pub book_id: String,
    pub display_name: String,
    pub photo: Option<AttachmentId>,
}

pub fn sync_status(
    repair_required: bool,
    readiness: Option<Value>,
    projection: Option<(&str, Option<&str>)>,
) -> Value {
    let mut out = json!({"repairRequired": repair_required});
    if let Some(readiness) = readiness {
        out["readiness"] = readiness;
    }
    if let Some((state, reason)) = projection {
        out["projection"] = json!({"state": state});
        if let Some(reason) = reason.filter(|reason| {
            reason.len() <= 64 && reason.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        }) {
            out["projection"]["reason"] = json!(reason);
        }
    }
    out
}
