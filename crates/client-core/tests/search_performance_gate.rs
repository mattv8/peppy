//! Explicit native performance evidence for the SQLCipher-backed local search index.
//! Run with `cargo test --release -p peppy-client-core --test integration search_performance_gate:: -- --ignored --nocapture`.

use crate::common::*;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tempfile::TempDir;

const SQLCIPHER_KEY: &str =
    "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";";
const WARM_ITERATIONS: usize = 20;
const CORE_WARM_BUDGET: Duration = Duration::from_millis(150);
const INDEX_GROWTH_BUDGET_PERCENT: u64 = 35;

struct Fixture {
    directory: TempDir,
    config: ClientConfig,
    vault: Vault,
    indexed_path: PathBuf,
    baseline_path: PathBuf,
}

fn search(query: &str, kind: SearchKind) -> LocalSearchRequest {
    LocalSearchRequest {
        query: query.into(),
        kind: Some(kind),
        cursor: None,
        limit: Some(6),
    }
}

fn sqlcipher(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(SQLCIPHER_KEY).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = OFF;")
        .unwrap();
    connection
}

fn database_bytes(path: &Path) -> u64 {
    fs::metadata(path).unwrap().len()
}

fn p95(samples: &mut [Duration]) -> Duration {
    samples.sort_unstable();
    samples[(samples.len() * 95).div_ceil(100) - 1]
}

fn measure(client: &Client, query: &str, kind: SearchKind) -> Duration {
    let request = search(query, kind);
    let page = client.search_local(request.clone()).unwrap();
    assert_eq!(page.index_state, SearchIndexState::Ready);
    assert!(
        !page.items.is_empty(),
        "benchmark query {query:?} unexpectedly returned no results"
    );
    let mut samples = Vec::with_capacity(WARM_ITERATIONS);
    for _ in 0..WARM_ITERATIONS {
        let started = Instant::now();
        client.search_local(request.clone()).unwrap();
        samples.push(started.elapsed());
    }
    p95(&mut samples)
}

fn measure_empty(client: &Client, query: &str, kind: SearchKind) -> Duration {
    let request = search(query, kind);
    let page = client.search_local(request.clone()).unwrap();
    assert_eq!(page.index_state, SearchIndexState::Ready);
    assert!(page.items.is_empty());
    let mut samples = Vec::with_capacity(WARM_ITERATIONS);
    for _ in 0..WARM_ITERATIONS {
        let started = Instant::now();
        client.search_local(request.clone()).unwrap();
        samples.push(started.elapsed());
    }
    p95(&mut samples)
}

fn body_from_reference(reference: &str, number: usize, body: String) -> String {
    let mut payload: MessagePayload = serde_json::from_str(reference).unwrap();
    payload.body = body;
    payload.record.message_id = format!("00000000-0000-4000-8000-{number:012}")
        .parse()
        .unwrap();
    payload.record.source_sequence = SourceSequence(number as u64 + 10);
    payload.provider_message_id = Some(format!("perf-message-{number:06}"));
    payload.sent_at_ms = Some(number as i64 + 10);
    payload.ordered_at_ms = Some(number as i64 + 10);
    serde_json::to_string(&payload).unwrap()
}

fn assert_search_hit_window_integrity(client: &Client, hit: &LocalSearchHit) {
    let LocalSearchHit::Message {
        conversation_id,
        message_id,
        ..
    } = hit
    else {
        panic!("expected a message search hit");
    };
    let window = client
        .load_message_window(MessageWindowRequest::Around {
            conversation_id: conversation_id.clone(),
            message_id: message_id.clone(),
        })
        .unwrap();
    let payload_message_ids = window
        .messages
        .iter()
        .map(|message| message.payload.record.message_id.to_string())
        .collect::<HashSet<_>>();
    assert!(payload_message_ids.len() <= 50);
    assert_eq!(payload_message_ids.len(), window.messages.len());
    assert!(payload_message_ids.contains(message_id));
}

fn contact_from_reference(reference: &str, number: usize, contact_count: usize) -> String {
    let mut contact: Value = serde_json::from_str(reference).unwrap();
    let late = number + 1 == contact_count;
    contact["id"] = Value::String(format!("perf-contact-{number:05}"));
    contact["revision"] = Value::String("1".into());
    contact["display_name"] = Value::String(if late {
        "Late Contact Needle".into()
    } else {
        format!("Common Contact {number:05}")
    });
    contact["name"]["given"] = contact["display_name"].clone();
    contact["emails"][0]["value"] = Value::String(format!("person{number:05}@example.test"));
    contact["phones"][0]["value"] = Value::String(format!("+1555{number:07}"));
    contact["provenance"]["source_id"] = Value::String(format!("perf-source-{number:05}"));
    serde_json::to_string(&contact).unwrap()
}

fn seed_fixture(message_count: usize, contact_count: usize) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "synthetic-indexed", &vault);
    let client = unlocked(&config, &vault);
    let captured = client
        .capture_incoming(incoming("canonical search reference", "perf-canonical"))
        .unwrap();
    client
        .capture_contact_book(
            &json!({
                "schema_version": 1,
                "book": {"id": "perf-book", "owner_device_id": config.device_id.to_string(), "generation": "1", "state": "active", "region": "US", "display_name": "Performance contacts"},
                "contacts": [{
                    "id": "canonical-contact", "book_id": "perf-book", "revision": "1", "display_name": "Canonical Contact",
                    "name": {"given": "Canonical", "family": "Contact"},
                    "phones": [{"id": "phone", "value": "+15555550100"}],
                    "emails": [{"id": "email", "value": "canonical@example.test"}], "addresses": [],
                    "provenance": {"source_id": "canonical-source", "read_only": false}
                }]
            })
            .to_string(),
        )
        .unwrap();
    drop(client);

    let indexed_path = config.database_path.clone();
    let baseline_path = directory.path().join("synthetic-baseline.db");
    fs::copy(&indexed_path, &baseline_path).unwrap();

    let indexed = sqlcipher(&indexed_path);
    let (message_payload, conversation_id, source_device_id): (String, String, String) = indexed
        .query_row(
            "SELECT payload,conversation_id,source_device_id FROM messages WHERE id=?",
            [captured.message_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let contact_payload: String = indexed
        .query_row(
            "SELECT body FROM contacts WHERE id='canonical-contact'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let book_body: String = indexed
        .query_row(
            "SELECT body FROM contact_books WHERE id='perf-book'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&book_body).unwrap()["display_name"],
        "Performance contacts"
    );
    let tx = indexed.unchecked_transaction().unwrap();
    let mut insert_message = tx
        .prepare("INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,provider_message_id,payload,seen,actual_sent_at_ms,sort_at_ms) VALUES(?,?,?,?,?,?,?,?,?)")
        .unwrap();
    for number in 0..message_count {
        let late = number + 1 == message_count;
        let body = if late {
            format!("Common message Late Message Needle {number:06}")
        } else {
            format!("Common message body {number:06}")
        };
        insert_message
            .execute(params![
                format!("00000000-0000-4000-8000-{number:012}"),
                conversation_id,
                source_device_id,
                number as i64 + 10,
                format!("perf-message-{number:06}"),
                body_from_reference(&message_payload, number, body),
                1_i64,
                number as i64 + 10,
                number as i64 + 10,
            ])
            .unwrap();
    }
    drop(insert_message);
    let mut insert_contact = tx
        .prepare("INSERT INTO contacts(id,book_id,revision,body,deleted_at) VALUES(?,?,?,?,NULL)")
        .unwrap();
    for number in 0..contact_count {
        insert_contact
            .execute(params![
                format!("perf-contact-{number:05}"),
                "perf-book",
                1_i64,
                contact_from_reference(&contact_payload, number, contact_count),
            ])
            .unwrap();
    }
    drop(insert_contact);
    tx.commit().unwrap();
    indexed
        .execute_batch("PRAGMA synchronous = FULL; VACUUM;")
        .unwrap();
    drop(indexed);

    let baseline = sqlcipher(&baseline_path);
    baseline
        .execute_batch(
            "DROP TRIGGER messages_search_insert; DROP TRIGGER messages_search_update; DROP TRIGGER messages_search_delete; DROP INDEX messages_search_metadata; DROP TABLE local_search_fts;",
        )
        .unwrap();
    let tx = baseline.unchecked_transaction().unwrap();
    let mut insert_message = tx
        .prepare("INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,provider_message_id,payload,seen,actual_sent_at_ms,sort_at_ms) VALUES(?,?,?,?,?,?,?,?,?)")
        .unwrap();
    for number in 0..message_count {
        let late = number + 1 == message_count;
        let body = if late {
            format!("Common message Late Message Needle {number:06}")
        } else {
            format!("Common message body {number:06}")
        };
        insert_message
            .execute(params![
                format!("00000000-0000-4000-8000-{number:012}"),
                conversation_id,
                source_device_id,
                number as i64 + 10,
                format!("perf-message-{number:06}"),
                body_from_reference(&message_payload, number, body),
                1_i64,
                number as i64 + 10,
                number as i64 + 10,
            ])
            .unwrap();
    }
    drop(insert_message);
    let mut insert_contact = tx
        .prepare("INSERT INTO contacts(id,book_id,revision,body,deleted_at) VALUES(?,?,?,?,NULL)")
        .unwrap();
    for number in 0..contact_count {
        insert_contact
            .execute(params![
                format!("perf-contact-{number:05}"),
                "perf-book",
                1_i64,
                contact_from_reference(&contact_payload, number, contact_count)
            ])
            .unwrap();
    }
    drop(insert_contact);
    tx.commit().unwrap();
    baseline
        .execute_batch("PRAGMA synchronous = FULL; VACUUM;")
        .unwrap();
    drop(baseline);

    Fixture {
        directory,
        config,
        vault,
        indexed_path,
        baseline_path,
    }
}

fn export_fixture(fixture: &Fixture) {
    let Ok(directory) = std::env::var("PEPPY_SEARCH_PERF_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    assert!(
        directory.is_absolute(),
        "PEPPY_SEARCH_PERF_DIR must be absolute"
    );
    fs::create_dir_all(&directory).unwrap();
    fs::copy(
        &fixture.indexed_path,
        directory.join("synthetic-indexed-100k.db"),
    )
    .unwrap();
    fs::copy(
        &fixture.baseline_path,
        directory.join("synthetic-baseline-100k.db"),
    )
    .unwrap();
    fs::write(directory.join("synthetic-100k-metadata.json"), serde_json::to_vec_pretty(&json!({
        "database": "synthetic-indexed-100k.db", "baselineDatabase": "synthetic-baseline-100k.db",
        "vaultId": fixture.vault.id.to_string(), "deviceId": fixture.config.device_id.to_string(),
        "profile": fixture.vault.profile, "header": fixture.vault.header, "databaseKey": DB_KEY,
        "messages": 100_000, "contacts": 10_000
    })).unwrap()).unwrap();
}

#[test]
#[ignore = "explicit SQLCipher synthetic performance gate"]
fn synthetic_search_performance_gate() {
    let mut measurements = Vec::new();
    for (messages, contacts) in [(1_000, 1_000), (10_000, 10_000), (100_000, 10_000)] {
        let fixture = seed_fixture(messages, contacts);
        let indexed_bytes = database_bytes(&fixture.indexed_path);
        let baseline_bytes = database_bytes(&fixture.baseline_path);

        let client = unlocked(&fixture.config, &fixture.vault);
        let late = client
            .search_local(search("Late Message Needle", SearchKind::Message))
            .unwrap();
        let late_hit = late.items.first().unwrap();
        assert!(
            matches!(late_hit, LocalSearchHit::Message { message_id, .. } if message_id.ends_with(&format!("{:012}", messages - 1)))
        );
        assert_search_hit_window_integrity(&client, late_hit);
        let broad_messages = measure(&client, "Common", SearchKind::Message);
        let late_message = measure(&client, "Late Message Needle", SearchKind::Message);
        let broad_contacts = measure(&client, "Common", SearchKind::Contact);
        let late_contact = measure(&client, "Late Contact Needle", SearchKind::Contact);
        let unmatched_contacts = measure_empty(&client, "No Such Contact", SearchKind::Contact);
        let conversations = measure(&client, "+1555", SearchKind::Conversation);
        let all = measure(&client, "Common", SearchKind::All);
        let growth_percent = (indexed_bytes.saturating_sub(baseline_bytes) * 100) / baseline_bytes;
        println!(
            "SEARCH_PERF messages={messages} contacts={contacts} bytes(indexed={indexed_bytes},baseline={baseline_bytes},growth={growth_percent}%) p95_ms(message_broad={},message_late={},contact_broad={},contact_late={},contact_unmatched={},conversation={},all={})",
            broad_messages.as_secs_f64() * 1_000.0,
            late_message.as_secs_f64() * 1_000.0,
            broad_contacts.as_secs_f64() * 1_000.0,
            late_contact.as_secs_f64() * 1_000.0,
            unmatched_contacts.as_secs_f64() * 1_000.0,
            conversations.as_secs_f64() * 1_000.0,
            all.as_secs_f64() * 1_000.0,
        );
        measurements.push((
            messages,
            indexed_bytes,
            baseline_bytes,
            growth_percent,
            broad_messages,
            late_message,
            broad_contacts,
            late_contact,
            unmatched_contacts,
            conversations,
            all,
        ));
        drop(client);
        if messages == 100_000 {
            export_fixture(&fixture);
        }
        drop(fixture.directory);
    }
    let (
        _,
        _,
        _,
        growth,
        broad_messages,
        late_message,
        broad_contacts,
        late_contact,
        unmatched_contacts,
        conversations,
        all,
    ) = measurements.last().unwrap();
    let measurements = [
        broad_messages,
        late_message,
        broad_contacts,
        late_contact,
        unmatched_contacts,
        conversations,
        all,
    ];
    println!(
        "SEARCH_PERF_GATE 100k growth_budget={}%, growth={}%, warm_core_budget_ms={}, p95_max_ms={}, warm_core_budget_met={}",
        INDEX_GROWTH_BUDGET_PERCENT,
        growth,
        CORE_WARM_BUDGET.as_millis(),
        measurements
            .into_iter()
            .map(|duration| duration.as_millis())
            .max()
            .unwrap(),
        measurements
            .into_iter()
            .all(|duration| *duration <= CORE_WARM_BUDGET),
    );
    assert!(
        *growth <= INDEX_GROWTH_BUDGET_PERCENT,
        "FTS index growth {growth}% exceeds {INDEX_GROWTH_BUDGET_PERCENT}% budget"
    );
}
