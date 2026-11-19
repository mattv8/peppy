//! Address resolution uses real captured contact projections, never native IDs.

use crate::common::*;
use peppy_crypto::{KeyPurpose, derive_purpose_key, derive_root_key, encrypt};
use serde_json::{Value, json};
use tempfile::TempDir;

const SUCCESSION_HINT: &[u8] = b"reinstall-stable-android-id";

fn capture(
    client: &Client,
    owner: &ClientConfig,
    book_id: &str,
    region: &str,
    generation: &str,
    contacts: Value,
) {
    capture_state(
        client, owner, book_id, region, generation, "active", contacts,
    );
}

fn capture_state(
    client: &Client,
    owner: &ClientConfig,
    book_id: &str,
    region: &str,
    generation: &str,
    state: &str,
    contacts: Value,
) {
    client.capture_contact_book(&json!({
        "schema_version": 1,
        "book": {"id": book_id, "owner_device_id": owner.device_id.to_string(), "generation": generation, "state": state, "region": region},
        "contacts": contacts,
    }).to_string()).unwrap();
}

fn contact(book_id: &str, id: &str, name: &str, phone: &str) -> Value {
    json!({"id":id,"book_id":book_id,"display_name":name,"name":{},"phones":[{"id":"p1","value":phone}],"emails":[],"addresses":[]})
}

fn resolve(client: &Client, addresses: &[&str]) -> Value {
    let mut result = resolve_names(client, addresses);
    result.as_object_mut().unwrap().remove("name_matches");
    result.as_object_mut().unwrap().remove("name_conflicts");
    result
}

fn resolve_names(client: &Client, addresses: &[&str]) -> Value {
    serde_json::from_str(
        &client
            .resolve_contact_addresses_json(&json!({"addresses": addresses}).to_string())
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn duplicate_names_provide_consensus_without_identity() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "owner", &vault);
    let client = unlocked(&cfg, &vault);
    capture(
        &client,
        &cfg,
        "book",
        "US",
        "1",
        json!([
            contact("book", "one", "Alex", "+12025550100"),
            contact("book", "two", "Alex", "+12025550100")
        ]),
    );
    let result = resolve_names(&client, &["+12025550100"]);
    assert_eq!(result["matches"], json!([]));
    assert_eq!(result["ambiguous"], json!(["+12025550100"]));
    assert_eq!(
        result["name_matches"],
        json!([
            {"address":"+12025550100","display_name":"Alex","basis":"consensus"}
        ])
    );
}

#[test]
fn different_duplicate_names_report_conflict_without_identity() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "owner", &vault);
    let client = unlocked(&cfg, &vault);
    capture(
        &client,
        &cfg,
        "book",
        "US",
        "1",
        json!([
            contact("book", "one", "Alex", "+12025550100"),
            contact("book", "two", "alex", "+12025550100")
        ]),
    );
    let result = resolve_names(&client, &["+12025550100"]);
    assert_eq!(result["matches"], json!([]));
    assert_eq!(result["ambiguous"], json!(["+12025550100"]));
    assert_eq!(result["name_conflicts"], json!(["+12025550100"]));
}

#[test]
fn empty_duplicate_name_does_not_block_consensus() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "owner", &vault);
    let client = unlocked(&cfg, &vault);
    capture(
        &client,
        &cfg,
        "book",
        "US",
        "1",
        json!([
            contact("book", "one", "", "+12025550100"),
            contact("book", "two", "Alex", "+12025550100")
        ]),
    );
    let result = resolve_names(&client, &["+12025550100"]);
    assert_eq!(result["matches"], json!([]));
    assert_eq!(
        result["name_matches"],
        json!([
            {"address":"+12025550100","display_name":"Alex","basis":"consensus"}
        ])
    );
}

#[test]
fn pin_and_book_exclusion_change_names_without_inventing_identity() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "owner", &vault);
    let client = unlocked(&cfg, &vault);
    capture(
        &client,
        &cfg,
        "book",
        "US",
        "1",
        json!([
            contact("book", "one", "Alex", "+12025550100"),
            contact("book", "two", "Blake", "+12025550100")
        ]),
    );
    let request = json!({"address":"2025550100","book_id":"book","contact_id":"one"}).to_string();
    assert!(matches!(
        client.set_contact_name_pin_json(
            &json!({"address":"2025550100","book_id":"book","contact_id":"missing"}).to_string()
        ),
        Err(peppy_client_core::Error::NotFound)
    ));
    assert_eq!(
        client.set_contact_name_pin_json(&request).unwrap(),
        r#"{"status":"pinned"}"#
    );
    let result = resolve_names(&client, &["+12025550100"]);
    assert_eq!(result["matches"], json!([]));
    assert_eq!(
        result["name_matches"],
        json!([{"address":"+12025550100","display_name":"Alex","basis":"pinned"}])
    );
    let input = json!({"address":"+12025550100"}).to_string();
    let candidates: Value =
        serde_json::from_str(&client.contact_name_candidates_json(&input).unwrap()).unwrap();
    assert_eq!(
        candidates["pinned"],
        json!({"book_id":"book","contact_id":"one"})
    );
    client
        .set_contact_book_lookup_json(&json!({"book_id":"book","enabled":false}).to_string())
        .unwrap();
    let hidden = resolve_names(&client, &["+12025550100"]);
    assert_eq!(hidden["matches"], json!([]));
    assert_eq!(hidden["name_matches"], json!([]));
    let books: Value = serde_json::from_str(&client.list_contact_books_json().unwrap()).unwrap();
    assert_eq!(books["books"][0]["lookup_excluded"], true);
    client
        .set_contact_book_lookup_json(&json!({"book_id":"book","enabled":true}).to_string())
        .unwrap();
    assert_eq!(
        resolve_names(&client, &["+12025550100"])["name_matches"],
        result["name_matches"]
    );
    client.clear_contact_name_pin_json(&input).unwrap();
    assert_eq!(
        resolve_names(&client, &["+12025550100"])["name_conflicts"],
        json!(["+12025550100"])
    );
}

#[test]
fn excluding_one_book_resolves_identity_and_restoring_returns_ambiguity() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "owner", &vault);
    let client = unlocked(&cfg, &vault);
    for book in ["one", "two"] {
        capture(
            &client,
            &cfg,
            book,
            "US",
            "1",
            json!([contact(book, "alex", "Alex", "+12025550100")]),
        );
    }
    let before = resolve_names(&client, &["+12025550100"]);
    assert_eq!(before["matches"], json!([]));
    assert_eq!(before["name_matches"][0]["basis"], "consensus");
    let search = || {
        serde_json::from_str::<Value>(
            &client
                .search_contact_recipients_json(
                    &json!({"schema_version":1,"query":"Alex"}).to_string(),
                )
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(search()["results"].as_array().unwrap().len(), 2);
    client
        .set_contact_book_lookup_json(&json!({"book_id":"two","enabled":false}).to_string())
        .unwrap();
    let hidden = resolve_names(&client, &["+12025550100"]);
    assert_eq!(hidden["matches"][0]["book_id"], "one");
    assert_eq!(hidden["ambiguous"], json!([]));
    assert_eq!(search()["results"].as_array().unwrap().len(), 1);
    client
        .set_contact_book_lookup_json(&json!({"book_id":"two","enabled":true}).to_string())
        .unwrap();
    assert_eq!(
        resolve_names(&client, &["+12025550100"])["matches"],
        json!([])
    );
    assert_eq!(search()["results"].as_array().unwrap().len(), 2);
}

fn succession_tag(vault: &Vault) -> String {
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Compaction).unwrap();
    peppy_crypto::compaction_hmac(&key, b"device-hint-v1", &[b"android", SUCCESSION_HINT])
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn succession_envelope(
    vault: &Vault,
    predecessor: DeviceId,
    successor: DeviceId,
    sequence: u64,
) -> Envelope {
    let root = derive_root_key(PASSPHRASE, &vault.profile).unwrap();
    let key = derive_purpose_key(&root, &vault.profile, KeyPurpose::Event).unwrap();
    let mut envelope = Envelope {
        protocol_version: 1,
        envelope_id: EnvelopeId::new(),
        command_id: None,
        vault_id: vault.id,
        producer_device_id: successor,
        producer_sequence: SourceSequence(sequence),
        key_epoch: 1,
        crypto_suite: vault.profile.crypto_suite,
        profile_fingerprint: vault.profile.fingerprint().unwrap(),
        purpose: EnvelopePurpose::Event,
        route: None,
        compaction: None,
        origin_tag: None,
        ciphertext: Vec::new(),
    };
    let payload = PrivatePayload::DeviceSucceeded {
        predecessor,
        successor,
        platform: "android".into(),
        hint_tag: succession_tag(vault),
        key_epoch: 1,
    };
    let encrypted = encrypt(
        &key,
        &envelope.aad_bytes().unwrap(),
        &serde_json::to_vec(&payload).unwrap(),
    )
    .unwrap();
    envelope.ciphertext = [encrypted.nonce.as_slice(), &encrypted.ciphertext].concat();
    envelope
}

#[test]
fn resolves_international_and_national_per_book_region_without_shortcuts() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "owner", &vault);
    let client = unlocked(&cfg, &vault);
    capture(
        &client,
        &cfg,
        "us",
        "US",
        "1",
        json!([contact("us", "ada", "Ada", "(202) 555-0100")]),
    );
    capture(
        &client,
        &cfg,
        "gb",
        "GB",
        "2",
        json!([contact("gb", "bea", "Bea", "020 7946 0018")]),
    );
    assert_eq!(
        resolve(
            &client,
            &[
                "+12025550100",
                "020 7946 0018",
                "2025550100",
                "911",
                "not a phone"
            ]
        ),
        json!({
            "matches":[
                {"address":"+12025550100","contact_id":"ada","book_id":"us","display_name":"Ada"},
                {"address":"020 7946 0018","contact_id":"bea","book_id":"gb","display_name":"Bea"}
            ],
            "ambiguous":["2025550100","911","not a phone"], "more_indexing":false
        })
    );
}

#[test]
fn refresh_normalizes_each_contact_in_a_book_with_its_cached_region() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "owner", &vault);
    let client = unlocked(&cfg, &vault);
    capture(
        &client,
        &cfg,
        "us",
        "US",
        "1",
        json!([
            contact("us", "ada", "Ada", "(202) 555-0100"),
            contact("us", "bea", "Bea", "(303) 555-0100")
        ]),
    );
    assert_eq!(
        resolve(&client, &["+12025550100", "+13035550100"]),
        json!({
            "matches":[
                {"address":"+12025550100","contact_id":"ada","book_id":"us","display_name":"Ada"},
                {"address":"+13035550100","contact_id":"bea","book_id":"us","display_name":"Bea"}
            ],
            "ambiguous":[], "more_indexing":false
        })
    );
}

#[test]
fn refreshes_late_and_earlier_revisions_without_stale_address_matches() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "owner", &vault);
    let client = unlocked(&cfg, &vault);
    let contacts = (0..600)
        .map(|number| {
            contact(
                "book",
                &format!("c{number:03}"),
                &format!("Contact {number}"),
                &format!("+1202555{number:04}"),
            )
        })
        .collect::<Vec<_>>();
    capture(&client, &cfg, "book", "US", "1", Value::Array(contacts));

    for _ in 0..4 {
        resolve(&client, &["+12025550000"]);
    }

    capture(
        &client,
        &cfg,
        "book",
        "US",
        "2",
        json!([contact("book", "c599", "Updated", "+12025559999")]),
    );
    assert_eq!(
        resolve(&client, &["+12025550599", "+12025559999"]),
        json!({
            "matches":[{"address":"+12025559999","contact_id":"c599","book_id":"book","display_name":"Updated"}],
            "ambiguous":["+12025550599"], "more_indexing":false
        })
    );

    capture(
        &client,
        &cfg,
        "book",
        "US",
        "3",
        json!([contact("book", "c000", "Earliest", "+12025558888")]),
    );
    assert_eq!(
        resolve(&client, &["+12025550000", "+12025558888"]),
        json!({
            "matches":[{"address":"+12025558888","contact_id":"c000","book_id":"book","display_name":"Earliest"}],
            "ambiguous":["+12025550000"], "more_indexing":false
        })
    );
}

#[test]
fn exact_shared_forgotten_and_changed_numbers_do_not_leak_stale_matches() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "owner", &vault);
    let client = unlocked(&cfg, &vault);
    capture(
        &client,
        &cfg,
        "book",
        "US",
        "1",
        json!([
            contact("book", "one", "One", "+12025550100"),
            contact("book", "two", "Two", "+12025550100"),
            contact("book", "short", "Short", "*123")
        ]),
    );
    assert_eq!(
        resolve(&client, &["+12025550100", "*123"]),
        json!({
            "matches":[{"address":"*123","contact_id":"short","book_id":"book","display_name":"Short"}],
            "ambiguous":["+12025550100"], "more_indexing":false
        })
    );
    capture(
        &client,
        &cfg,
        "book",
        "US",
        "2",
        json!([contact("book", "one", "One", "+12025550101")]),
    );
    assert_eq!(
        resolve(&client, &["+12025550100", "+12025550101"]),
        json!({
            "matches":[
                {"address":"+12025550100","contact_id":"two","book_id":"book","display_name":"Two"},
                {"address":"+12025550101","contact_id":"one","book_id":"book","display_name":"One"}
            ],
            "ambiguous":[], "more_indexing":false
        })
    );
    client
        .forget_contact_book(&json!({"book_id":"book"}).to_string())
        .unwrap();
    assert_eq!(
        resolve(&client, &["+12025550101"]),
        json!({"matches":[],"ambiguous":["+12025550101"],"more_indexing":false})
    );
}

#[test]
fn updated_and_deleted_contacts_stop_resolving_before_index_cleanup() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "owner", &vault);
    let client = unlocked(&cfg, &vault);
    let contacts = (0..40)
        .map(|number| {
            contact(
                "book",
                &format!("c{number:02}"),
                &format!("Contact {number}"),
                &format!("+1202555{number:04}"),
            )
        })
        .collect::<Vec<_>>();
    capture(
        &client,
        &cfg,
        "book",
        "US",
        "1",
        Value::Array(contacts.clone()),
    );
    assert_eq!(
        resolve(&client, &["+12025550000"]),
        json!({"matches":[{"address":"+12025550000","contact_id":"c00","book_id":"book","display_name":"Contact 0"}],"ambiguous":[],"more_indexing":false})
    );

    capture(
        &client,
        &cfg,
        "book",
        "US",
        "2",
        json!([contact("book", "c00", "Contact 0", "+12025559999")]),
    );
    assert_eq!(
        resolve(&client, &["+12025550000", "+12025559999"]),
        json!({"matches":[{"address":"+12025559999","contact_id":"c00","book_id":"book","display_name":"Contact 0"}],"ambiguous":["+12025550000"],"more_indexing":false})
    );

    client
        .begin_contact_scan(
            &json!({"schema_version":1,"scan_id":"scan","book_id":"book","generation":"2","access":"full","authoritative":true}).to_string(),
        )
        .unwrap();
    for contact in contacts.iter().skip(1) {
        client
            .observe_contact_scan(
                &json!({"schema_version":1,"scan_id":"scan","contact":contact}).to_string(),
            )
            .unwrap();
    }
    client
        .finish_contact_scan(
            &json!({"schema_version":1,"scan_id":"scan","complete":true}).to_string(),
        )
        .unwrap();
    assert_eq!(
        resolve(&client, &["+12025559999"]),
        json!({"matches":[],"ambiguous":["+12025559999"],"more_indexing":false})
    );
}

#[test]
fn incomplete_refresh_withholds_a_tail_match_until_the_index_is_current() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "owner", &vault);
    let client = unlocked(&cfg, &vault);
    let contacts = (0..201)
        .map(|number| {
            contact(
                "book",
                &format!("c{number:03}"),
                &format!("Contact {number}"),
                &format!("+1202555{number:04}"),
            )
        })
        .collect::<Vec<_>>();
    capture(&client, &cfg, "book", "US", "1", Value::Array(contacts));

    let first = resolve(&client, &["+12025550200"]);
    assert_eq!(first["matches"], json!([]));
    assert_eq!(first["ambiguous"], json!(["+12025550200"]));
    assert_eq!(first["more_indexing"], true);

    let settled = resolve(&client, &["+12025550200"]);
    assert_eq!(settled["matches"][0]["contact_id"], "c200");
    assert_eq!(settled["more_indexing"], false);
}

#[test]
fn incomplete_refresh_withholds_stale_tail_numbers_and_head_tail_duplicates() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "owner", &vault);
    let client = unlocked(&cfg, &vault);
    let contacts = (0..201)
        .map(|number| {
            contact(
                "book",
                &format!("c{number:03}"),
                &format!("Contact {number}"),
                &format!("+1202555{number:04}"),
            )
        })
        .collect::<Vec<_>>();
    capture(&client, &cfg, "book", "US", "1", Value::Array(contacts));
    resolve(&client, &["+12025550200"]);
    resolve(&client, &["+12025550200"]);

    let changed_contacts = (0..201)
        .map(|number| {
            let phone = if number == 200 {
                "+12025559999".to_owned()
            } else {
                format!("+1202555{number:04}")
            };
            contact(
                "book",
                &format!("c{number:03}"),
                &format!("Changed {number}"),
                &phone,
            )
        })
        .collect::<Vec<_>>();
    capture(
        &client,
        &cfg,
        "book",
        "US",
        "2",
        Value::Array(changed_contacts),
    );
    let incomplete = resolve(&client, &["+12025550200", "+12025559999"]);
    assert_eq!(incomplete["matches"], json!([]));
    assert_eq!(
        incomplete["ambiguous"],
        json!(["+12025550200", "+12025559999"])
    );
    assert_eq!(incomplete["more_indexing"], true);
    let settled = resolve(&client, &["+12025550200", "+12025559999"]);
    assert_eq!(settled["matches"][0]["contact_id"], "c200");
    assert_eq!(settled["ambiguous"], json!(["+12025550200"]));

    let duplicate_contacts = (0..201)
        .map(|number| {
            let phone = if number == 200 {
                "+12025550000".to_owned()
            } else {
                format!("+1202555{number:04}")
            };
            contact(
                "book",
                &format!("c{number:03}"),
                &format!("Duplicate {number}"),
                &phone,
            )
        })
        .collect::<Vec<_>>();
    capture(
        &client,
        &cfg,
        "book",
        "US",
        "3",
        Value::Array(duplicate_contacts),
    );
    let incomplete = resolve(&client, &["+12025550000"]);
    assert_eq!(incomplete["matches"], json!([]));
    assert_eq!(incomplete["ambiguous"], json!(["+12025550000"]));
    assert_eq!(incomplete["more_indexing"], true);
    let settled = resolve(&client, &["+12025550000"]);
    assert_eq!(settled["matches"], json!([]));
    assert_eq!(settled["ambiguous"], json!(["+12025550000"]));
    assert_eq!(settled["more_indexing"], false);
}

#[test]
fn superseded_retired_contacts_do_not_starve_visible_successor_indexing() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let predecessor_config = config(&dir, "retired-predecessor", &vault);
    let successor_config = config(&dir, "visible-successor", &vault);
    let predecessor = unlocked(&predecessor_config, &vault);
    let successor = unlocked(&successor_config, &vault);
    let observer = unlocked(&config(&dir, "retired-observer", &vault), &vault);
    let mut server = Server::default();
    let active_number = "+12025550199";
    let retired_contacts = (0..200)
        .map(|index| {
            contact(
                "retired-book",
                &format!("retired-{index:03}"),
                &format!("Retired {index}"),
                &format!("+1202555{index:04}"),
            )
        })
        .collect::<Vec<_>>();

    capture(
        &predecessor,
        &predecessor_config,
        "retired-book",
        "US",
        "1",
        Value::Array(retired_contacts.clone()),
    );
    capture_state(
        &predecessor,
        &predecessor_config,
        "retired-book",
        "US",
        "2",
        "retired",
        Value::Array(retired_contacts),
    );
    predecessor
        .register_device_hint("android", SUCCESSION_HINT)
        .unwrap();
    capture(
        &successor,
        &successor_config,
        "successor-book",
        "US",
        "1",
        json!([contact(
            "successor-book",
            "visible",
            "Visible Successor",
            active_number
        )]),
    );
    successor
        .register_device_hint("android", SUCCESSION_HINT)
        .unwrap();
    server.upload(&predecessor);
    server.upload(&successor);
    server.sync(&observer);
    server.sync(&observer);
    server.sync(&observer);

    server.log.push(succession_envelope(
        &vault,
        predecessor_config.device_id,
        successor_config.device_id,
        10_000,
    ));
    server.sync(&observer);
    server.sync(&observer);

    assert_eq!(
        resolve(&observer, &[active_number]),
        json!({
            "matches": [{
                "address": active_number,
                "contact_id": "visible",
                "book_id": "successor-book",
                "display_name": "Visible Successor"
            }],
            "ambiguous": [],
            "more_indexing": false
        })
    );
}
