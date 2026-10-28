mod common;
use common::*;
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
