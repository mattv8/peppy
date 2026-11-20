use crate::common::*;
use serde_json::{Value, json};
use tempfile::TempDir;

const BOOK: &str = "book";

fn contact(id: &str, given: Option<&str>, family: Option<&str>, display_name: &str) -> Value {
    let mut name = serde_json::Map::new();
    if let Some(given) = given {
        name.insert("given".into(), json!(given));
    }
    if let Some(family) = family {
        name.insert("family".into(), json!(family));
    }
    json!({
        "id": id,
        "book_id": BOOK,
        "display_name": display_name,
        "name": name,
        "phones": [],
        "emails": [],
        "addresses": []
    })
}

struct Fixture {
    _directory: TempDir,
    client: Client,
}

fn client_with(contacts: Vec<Value>) -> Fixture {
    let directory = TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "owner", &vault);
    let client = unlocked(&config, &vault);
    client
        .capture_contact_book(
            &json!({
                "schema_version": 1,
                "book": {
                    "id": BOOK,
                    "owner_device_id": config.device_id.to_string(),
                    "generation": "1"
                },
                "contacts": contacts
            })
            .to_string(),
        )
        .unwrap();
    Fixture {
        _directory: directory,
        client,
    }
}

fn ids(client: &Client, input: Value) -> Vec<String> {
    let output: Value =
        serde_json::from_str(&client.contact_book_view(&input.to_string()).unwrap()).unwrap();
    output["contacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|contact| contact["id"].as_str().unwrap().to_owned())
        .collect()
}

fn view(sort: Value) -> Value {
    json!({"schema_version": 1, "book_id": BOOK, "sort": sort, "limit": 200})
}

#[test]
fn explicit_sort_orders_both_name_keys_and_directions() {
    let client = client_with(vec![
        contact("3", Some("Zoe"), Some("Smith"), "Zoe Smith"),
        contact("1", Some("Bob"), Some("Adams"), "Bob Adams"),
        contact("2", Some("Alice"), Some("Adams"), "Alice Adams"),
    ]);

    assert_eq!(
        ids(
            &client.client,
            view(json!({"key": "family", "direction": "asc"}))
        ),
        ["2", "1", "3"]
    );
    assert_eq!(
        ids(
            &client.client,
            view(json!({"key": "family", "direction": "desc"}))
        ),
        ["3", "1", "2"]
    );
    assert_eq!(
        ids(
            &client.client,
            view(json!({"key": "given", "direction": "asc"}))
        ),
        ["2", "1", "3"]
    );
    assert_eq!(
        ids(
            &client.client,
            view(json!({"key": "given", "direction": "desc"}))
        ),
        ["3", "1", "2"]
    );
}

#[test]
fn explicit_sort_normalizes_falls_back_and_keeps_unnamed_last() {
    let client = client_with(vec![
        contact("e", None, None, "  "),
        contact("d", None, None, " Dora "),
        contact("c", Some(" Cato "), Some(" "), "Cato Display"),
        contact("b", Some("Bea"), Some("Brown"), "Bea Brown"),
        contact("a", Some(" Abe "), None, "Abe Display"),
    ]);

    assert_eq!(
        ids(
            &client.client,
            view(json!({"key": "family", "direction": "asc"}))
        ),
        ["a", "b", "c", "d", "e"]
    );
    assert_eq!(
        ids(
            &client.client,
            view(json!({"key": "family", "direction": "desc"}))
        ),
        ["d", "c", "b", "a", "e"]
    );
}

#[test]
fn explicit_sort_filters_before_paging_and_keeps_ties_in_id_order() {
    let client = client_with(vec![
        contact("z", Some("Mia"), Some("Match"), "Match Z"),
        contact("a", Some("Mia"), Some("Match"), "Match A"),
        contact("c", Some("Ada"), Some("Match"), "Match C"),
        contact("b", Some("Bea"), Some("Other"), "Other B"),
    ]);
    let sort = json!({"key": "given", "direction": "asc"});

    assert_eq!(
        ids(
            &client.client,
            json!({"book_id": BOOK, "search": "match", "sort": sort, "offset": 0, "limit": 2})
        ),
        ["c", "a"]
    );
    assert_eq!(
        ids(
            &client.client,
            json!({"book_id": BOOK, "search": "match", "sort": {"key": "given", "direction": "asc"}, "offset": 2, "limit": 2})
        ),
        ["z"]
    );
}

#[test]
fn omitted_sort_preserves_legacy_id_order_and_malformed_sort_is_rejected() {
    let client = client_with(vec![
        contact("a", Some("Zoe"), Some("Zulu"), "Zoe Zulu"),
        contact("z", Some("Ada"), Some("Alpha"), "Ada Alpha"),
    ]);

    assert_eq!(
        ids(&client.client, json!({"book_id": BOOK, "limit": 200})),
        ["a", "z"]
    );
    for sort in [
        Value::Null,
        json!({}),
        json!({"key": "middle", "direction": "asc"}),
        json!({"key": "given", "direction": "up"}),
        json!({"key": "given", "direction": "asc", "extra": true}),
    ] {
        assert!(
            client
                .client
                .contact_book_view(&view(sort).to_string())
                .is_err()
        );
    }
}

#[test]
fn cursor_pages_match_offset_order() {
    let fixture = client_with(vec![
        contact("1", Some("émile"), Some("Ångström"), "émile Ångström"),
        contact("2", Some("Ada"), None, "Ada"),
        contact("3", None, None, ""),
        contact("4", Some("ada"), None, "Ada"),
        contact("5", None, Some("Smith"), "Smith"),
    ]);
    for key in ["given", "family"] {
        for direction in ["asc", "desc"] {
            let sort = json!({"key": key, "direction": direction});
            let expected = ids(&fixture.client, view(sort.clone()));
            let mut actual = Vec::new();
            let mut after = None;
            loop {
                let mut input = json!({"book_id": BOOK, "sort": sort, "limit": 2});
                if let Some(cursor) = after {
                    input["after"] = cursor;
                }
                let output: Value = serde_json::from_str(
                    &fixture
                        .client
                        .contact_book_view(&input.to_string())
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(output["total"], 5);
                actual.extend(
                    output["contacts"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|row| row["id"].as_str().unwrap().to_owned()),
                );
                if output["next_cursor"].is_null() {
                    break;
                }
                after = Some(output["next_cursor"].clone());
            }
            assert_eq!(actual, expected);
        }
    }
}

#[test]
fn cursor_validation_total_and_through() {
    let fixture = client_with(vec![
        contact("a", Some("Ada"), None, "Ada"),
        contact("b", Some("Bea"), None, "Bea"),
        contact("c", Some("Cato"), None, "Cato"),
        contact("d", Some("Dora"), None, "Dora"),
    ]);
    let call = |input: Value| -> Value {
        serde_json::from_str(
            &fixture
                .client
                .contact_book_view(&input.to_string())
                .unwrap(),
        )
        .unwrap()
    };
    let first = call(json!({"book_id": BOOK, "limit": 2, "through": "d"}));
    assert_eq!(first["total"], 4);
    assert_eq!(first["contacts"].as_array().unwrap().len(), 4);
    assert_eq!(first["through_found"], true);
    assert!(first["next_cursor"].is_null());
    let first = call(json!({"book_id": BOOK, "limit": 2, "through": "missing"}));
    assert_eq!(first["contacts"].as_array().unwrap().len(), 2);
    assert_eq!(first["through_found"], false);
    let cursor = first["next_cursor"].clone();
    let short = call(json!({"book_id": BOOK, "limit": 2, "through": "a"}));
    assert_eq!(short["contacts"].as_array().unwrap().len(), 2);
    assert_eq!(short["through_found"], true);
    let through_after = call(json!({"book_id": BOOK, "limit": 1, "after": cursor, "through": "d"}));
    assert_eq!(through_after["contacts"].as_array().unwrap().len(), 2);
    assert_eq!(through_after["through_found"], true);
    let last = call(json!({"book_id": BOOK, "limit": 2, "after": cursor}));
    assert!(last["next_cursor"].is_null());
    let remaining: Vec<&str> = last["contacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(remaining, ["c", "d"]);
    let searched = call(json!({"book_id": BOOK, "search": "ada"}));
    assert_eq!(searched["total"], 1);
    for input in [
        json!({"book_id": BOOK, "after": "not-a-cursor"}),
        json!({"book_id": BOOK, "after": cursor, "offset": 0}),
        json!({"book_id": BOOK, "after": cursor, "sort": {"key": "given", "direction": "asc"}}),
    ] {
        assert!(
            fixture
                .client
                .contact_book_view(&input.to_string())
                .is_err()
        );
    }
    assert_eq!(
        fixture.client.contact_book_view(
            &json!({"book_id": BOOK, "after": cursor, "sort": {"key": "given", "direction": "asc"}}).to_string()
        ).unwrap_err(),
        peppy_client_core::Error::InvalidRequest("contact cursor sort mismatch")
    );
}

#[test]
fn through_beyond_cap_returns_only_first_page() {
    let contacts = (0..2001)
        .map(|i| contact(&format!("{i:04}"), None, None, ""))
        .collect();
    let fixture = client_with(contacts);
    let output: Value = serde_json::from_str(
        &fixture
            .client
            .contact_book_view(&json!({"book_id": BOOK, "limit": 2, "through": "2000"}).to_string())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(output["total"], 2001);
    assert_eq!(output["contacts"].as_array().unwrap().len(), 2);
    assert_eq!(output["through_found"], false);
}
