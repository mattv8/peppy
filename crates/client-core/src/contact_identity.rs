//! Content identity of a contact, independent of the ids a device minted for it.
//!
//! Two enrollments of one phone mint different contact and list-item ids for the same OS
//! contact. A successor recognises its predecessor's contacts by what they contain:
//! - the exact key equals when the whole content equals;
//! - the identity key equals when name, phone numbers and emails equal, which tolerates a label
//!   or a note that changed between the two captures;
//! - `covers` tells whether one contact holds everything another holds, so preferring it over
//!   the other loses nothing. It compares text verbatim: only the formatting of a phone number,
//!   the case of an email address and item labels are allowed to differ.
use crate::contact_resolution;
use crate::contacts;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use unicode_normalization::UnicodeNormalization;

const LISTS: [&str; 3] = ["phones", "emails", "addresses"];
/// Keys that identify, version or decorate a stored contact. The photo is excluded because each
/// enrollment uploads its own copy of the same image.
const NON_CONTENT_KEYS: [&str; 8] = [
    "id",
    "book_id",
    "revision",
    "digest",
    "generation",
    "deleted_at",
    "restored_to",
    "photo",
];
/// Keys of a list item that do not describe what the item holds.
const ITEM_ATTRIBUTE_KEYS: [&str; 3] = ["id", "label", "writable"];

/// A contact reduced to its content, with the keys used to match it.
pub(crate) struct Content {
    view: Value,
    pub exact: String,
    pub identity: Option<String>,
    phones: BTreeSet<String>,
    emails: BTreeSet<String>,
    addresses: BTreeSet<String>,
}

impl Content {
    /// `body` is a stored contact body or a platform entry's fields; `region` is the book's.
    pub(crate) fn of(body: &Value, region: Option<&str>) -> Self {
        let view = content_view(body);
        let phones = values(&view, "phones", |value| phone_key(value, region));
        let emails = values(&view, "emails", |value| value.trim().to_lowercase());
        let addresses = view
            .get("addresses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|item| canonical_text(&without(item, &ITEM_ATTRIBUTE_KEYS)))
            .collect();
        let name = text_key(view.get("display_name"));
        let identity = (!name.is_empty() && !(phones.is_empty() && emails.is_empty()))
            .then(|| contacts::digest(&json!({"name": name, "phones": phones, "emails": emails})));
        Self {
            exact: contacts::digest(&view),
            identity,
            phones,
            emails,
            addresses,
            view,
        }
    }

    /// Whether this contact holds everything `other` holds: every text field of `other`
    /// verbatim, and every phone number, email and address. Labels are not compared.
    pub(crate) fn covers(&self, other: &Content) -> bool {
        let Some(fields) = other.view.as_object() else {
            return false;
        };
        fields.iter().all(|(key, value)| match key.as_str() {
            "phones" => other.phones.is_subset(&self.phones),
            "emails" => other.emails.is_subset(&self.emails),
            "addresses" => other.addresses.is_subset(&self.addresses),
            "name" => value.as_object().is_some_and(|parts| {
                parts
                    .iter()
                    .all(|(part, text)| self.view.pointer(&format!("/name/{part}")) == Some(text))
            }),
            key => self.view.get(key) == Some(value),
        })
    }
}

/// What a list item holds, without the id a device minted for it.
pub(crate) fn item_key(item: &Value) -> String {
    let mut item = without(item, &["id"]);
    prune_empty(&mut item);
    canonical_text(&item)
}

/// Pairs each absorbed contact with one keeper that holds the same contact, one to one.
/// Exactly equal contents pair first, in id order. A remaining contact pairs through the
/// identity key only when that key is unique on both sides and the keeper covers it; anything
/// ambiguous stays unpaired. Returns `(absorbed id, keeper id)`.
pub(crate) fn pair(
    absorbed: &BTreeMap<String, Content>,
    keepers: &BTreeMap<String, Content>,
) -> Vec<(String, String)> {
    let mut free_keepers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (id, content) in keepers.iter().rev() {
        free_keepers.entry(&content.exact).or_default().push(id);
    }
    let mut pairs = Vec::new();
    let mut unpaired = Vec::new();
    for (id, content) in absorbed {
        match free_keepers
            .get_mut(content.exact.as_str())
            .and_then(Vec::pop)
        {
            Some(keeper) => pairs.push((id.clone(), keeper.to_owned())),
            None => unpaired.push(id.as_str()),
        }
    }
    let free_keepers: Vec<&str> = free_keepers.into_values().flatten().collect();
    let by_identity = |ids: &[&str], side: &BTreeMap<String, Content>| {
        let mut index: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for id in ids {
            if let Some(identity) = &side[*id].identity {
                index
                    .entry(identity.clone())
                    .or_default()
                    .push((*id).to_owned());
            }
        }
        index
    };
    let keeper_index = by_identity(&free_keepers, keepers);
    for (identity, ids) in by_identity(&unpaired, absorbed) {
        if let ([absorbed_id], Some([keeper_id])) = (
            ids.as_slice(),
            keeper_index.get(&identity).map(Vec::as_slice),
        ) && keepers[keeper_id].covers(&absorbed[absorbed_id])
        {
            pairs.push((absorbed_id.clone(), keeper_id.clone()));
        }
    }
    pairs
}

fn content_view(body: &Value) -> Value {
    let mut view = without(body, &NON_CONTENT_KEYS);
    contacts::strip_sources(&mut view);
    if let Some(fields) = view.as_object_mut() {
        for list in LISTS {
            if let Some(Value::Array(items)) = fields.get_mut(list) {
                for item in items.iter_mut() {
                    *item = without(item, &["id"]);
                }
            }
        }
    }
    prune_empty(&mut view);
    if let Some(fields) = view.as_object_mut() {
        for list in LISTS {
            if let Some(Value::Array(items)) = fields.get_mut(list) {
                items.sort_by_cached_key(canonical_text);
            }
        }
    }
    view
}

fn without(value: &Value, keys: &[&str]) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !keys.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Map<_, _>>(),
        ),
        other => other.clone(),
    }
}

/// Removes values that carry no content, so an absent key equals an empty one.
fn prune_empty(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.values_mut().for_each(prune_empty);
            map.retain(|_, value| !is_empty(value));
        }
        Value::Array(items) => {
            items.iter_mut().for_each(prune_empty);
            items.retain(|value| !is_empty(value));
        }
        _ => {}
    }
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn canonical_text(value: &Value) -> String {
    let mut text = String::new();
    contacts::canonical(value, &mut text);
    text
}

fn values(view: &Value, list: &str, key: impl Fn(&str) -> String) -> BTreeSet<String> {
    view.get(list)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("value").and_then(Value::as_str))
        .map(key)
        .filter(|key| !key.is_empty())
        .collect()
}

/// E.164 when the number parses in the book's region. Otherwise the dial string without its
/// formatting characters only: `*`, `#` and pause or wait characters change what is dialled.
fn phone_key(value: &str, region: Option<&str>) -> String {
    contact_resolution::normalized(value, region).unwrap_or_else(|| {
        value
            .chars()
            .filter(|c| !c.is_whitespace() && !matches!(c, '(' | ')' | '.' | '-'))
            .collect()
    })
}

/// A name as the identity key compares it: without regard to case, spacing or Unicode
/// composition. `covers` still requires the exact text.
fn text_key(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .nfkc()
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored() -> Value {
        json!({
            "id": "contact-1", "book_id": "book", "revision": "4", "digest": "abc",
            "display_name": "Ada Lovelace", "name": {"given": "Ada", "family": "Lovelace"},
            "phones": [
                {"id": "f1", "label": "mobile", "value": "+1 (801) 657-9284", "writable": true},
                {"id": "f2", "label": "work", "value": "+12025550100", "writable": true}
            ],
            "emails": [{"id": "f3", "label": "home", "value": "Ada@Example.test"}],
            "addresses": [],
            "photo": {"attachment_id": "photo-a"}
        })
    }

    fn content(body: &Value) -> Content {
        Content::of(body, Some("US"))
    }

    fn side(contacts: &[(&str, &Value)]) -> BTreeMap<String, Content> {
        contacts
            .iter()
            .map(|(id, body)| ((*id).to_owned(), content(body)))
            .collect()
    }

    #[test]
    fn exact_key_ignores_minted_ids_photo_empty_values_and_list_order() {
        let mut recaptured = stored();
        recaptured["id"] = json!("contact-2");
        recaptured["revision"] = json!("1");
        recaptured["digest"] = json!("def");
        recaptured["photo"] = json!({"attachment_id": "photo-b"});
        recaptured["phones"] = json!([
            {"id": "x2", "label": "work", "value": "+12025550100", "writable": true},
            {"id": "x1", "label": "mobile", "value": "+1 (801) 657-9284", "writable": true}
        ]);
        recaptured["emails"][0]["id"] = json!("x3");
        recaptured.as_object_mut().unwrap().remove("addresses");
        recaptured["nickname"] = json!("");
        recaptured["provenance"] = json!({"raw_ids": ["7"]});

        assert_eq!(content(&stored()).exact, content(&recaptured).exact);
    }

    #[test]
    fn exact_key_changes_with_any_content() {
        for (pointer, value) in [
            ("/display_name", json!("Ada King")),
            ("/phones/0/label", json!("home")),
            ("/phones/0/value", json!("+18016579285")),
            ("/notes", json!("Analyst")),
            ("/birthday", json!({"month": 12, "day": 10})),
        ] {
            let mut changed = stored();
            match changed.pointer_mut(pointer) {
                Some(slot) => *slot = value,
                None => changed[&pointer[1..]] = value,
            }
            assert_ne!(
                content(&stored()).exact,
                content(&changed).exact,
                "{pointer}"
            );
        }
    }

    #[test]
    fn identity_key_tolerates_formatting_labels_and_other_fields() {
        let mut recaptured = stored();
        recaptured["display_name"] = json!("  ada   LOVELACE ");
        recaptured["phones"] = json!([
            {"id": "x1", "label": "home", "value": "202-555-0100"},
            {"id": "x2", "label": "other", "value": "(801) 657-9284"}
        ]);
        recaptured["emails"][0]["value"] = json!("ada@example.test");
        recaptured["notes"] = json!("Analyst");

        let (original, recaptured) = (content(&stored()), content(&recaptured));
        assert_ne!(original.exact, recaptured.exact);
        assert_eq!(original.identity, recaptured.identity);
        assert!(original.identity.is_some());
    }

    #[test]
    fn identity_key_needs_a_name_and_an_address() {
        let nameless =
            json!({"display_name": " ", "phones": [{"id": "p", "value": "+12025550100"}]});
        let unreachable = json!({"display_name": "Ada", "phones": [], "emails": []});
        assert!(content(&nameless).identity.is_none());
        assert!(content(&unreachable).identity.is_none());
    }

    #[test]
    fn covers_requires_every_field_of_the_other_contact() {
        let base = content(&stored());
        let mut richer = stored();
        richer["notes"] = json!("Analyst");
        richer["phones"][0]["label"] = json!("home");
        richer["addresses"] = json!([{"id": "a1", "label": "home", "city": "London"}]);
        let richer = content(&richer);

        assert!(
            richer.covers(&base),
            "a contact that only gained content covers the old one"
        );
        assert!(
            !base.covers(&richer),
            "the old contact lacks the note and the address"
        );

        let mut fewer_phones = stored();
        fewer_phones["phones"].as_array_mut().unwrap().pop();
        assert!(!content(&fewer_phones).covers(&base));
        let mut renamed = stored();
        renamed["name"]["family"] = json!("King");
        assert!(!content(&renamed).covers(&base));
        let mut birthday = stored();
        birthday["birthday"] = json!({"month": 12, "day": 10});
        assert!(content(&birthday).covers(&base));
        assert!(!base.covers(&content(&birthday)));
    }

    #[test]
    fn covers_compares_text_verbatim() {
        let with_notes = |notes: &str| {
            let mut contact = stored();
            contact["notes"] = json!(notes);
            content(&contact)
        };
        let original = with_notes("Password: AbC\nDoor 4");
        assert!(with_notes("Password: AbC\nDoor 4").covers(&original));
        assert!(!with_notes("Password: abc\nDoor 4").covers(&original));
        assert!(!with_notes("Password: AbC Door 4").covers(&original));
        let mut recased = stored();
        recased["name"]["family"] = json!("LOVELACE");
        assert!(!content(&recased).covers(&content(&stored())));
    }

    #[test]
    fn dial_strings_that_dial_differently_are_different_phones() {
        let with_phone = |value: &str| {
            let mut contact = stored();
            contact["phones"] = json!([{"id": "p", "label": "mobile", "value": value}]);
            content(&contact)
        };
        let service_code = with_phone("*123#");
        for other in ["123", "#123", "123,4", "123;4"] {
            let other = with_phone(other);
            assert_ne!(service_code.identity, other.identity);
            assert!(!other.covers(&service_code));
            assert!(!service_code.covers(&other));
        }
        // Only formatting differs: the same dial string.
        assert_eq!(service_code.identity, with_phone(" *123# ").identity);
        assert_eq!(with_phone("123,4").identity, with_phone("1-2 3,4").identity);
    }

    #[test]
    fn pair_matches_equal_contents_one_to_one() {
        let ada = stored();
        let absorbed = side(&[("old-1", &ada), ("old-2", &ada), ("old-3", &ada)]);
        let keepers = side(&[("new-1", &ada), ("new-2", &ada)]);

        assert_eq!(
            pair(&absorbed, &keepers),
            vec![
                ("old-1".to_owned(), "new-1".to_owned()),
                ("old-2".to_owned(), "new-2".to_owned())
            ]
        );
    }

    #[test]
    fn pair_uses_identity_only_when_unique_and_covered() {
        let old = stored();
        let mut gained = stored();
        gained["notes"] = json!("Analyst");
        assert_eq!(
            pair(&side(&[("old", &old)]), &side(&[("new", &gained)])),
            vec![("old".to_owned(), "new".to_owned())]
        );
        // The keeper lacks the old contact's note: pairing would lose it.
        assert!(pair(&side(&[("old", &gained)]), &side(&[("new", &old)])).is_empty());
        // Two keepers share the identity: which one the old contact became is unknown.
        let mut other = stored();
        other["notes"] = json!("Countess");
        assert!(
            pair(
                &side(&[("old", &old)]),
                &side(&[("new-1", &gained), ("new-2", &other)])
            )
            .is_empty()
        );
    }

    #[test]
    fn item_key_ignores_only_the_minted_id() {
        let item =
            json!({"id": "f1", "label": "mobile", "value": "+12025550100", "writable": true});
        let same =
            json!({"id": "x9", "label": "mobile", "value": "+12025550100", "writable": true});
        let relabelled =
            json!({"id": "f1", "label": "work", "value": "+12025550100", "writable": true});
        assert_eq!(item_key(&item), item_key(&same));
        assert_ne!(item_key(&item), item_key(&relabelled));
    }
}
