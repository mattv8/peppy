mod common;

use common::*;
use serde_json::json;

fn request(query: &str) -> LocalSearchRequest {
    LocalSearchRequest {
        query: query.into(),
        kind: Some(SearchKind::Message),
        cursor: None,
        limit: Some(6),
    }
}

#[test]
fn actual_messages_are_hydrated_from_canonical_rows() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "search", &vault), &vault);
    let captured = client
        .capture_incoming(incoming("needle body", "search-message"))
        .unwrap();

    let page = client.search_local(request("needle")).unwrap();
    assert_eq!(
        page.items,
        vec![LocalSearchHit::Message {
            conversation_id: captured.conversation_id.to_string(),
            message_id: captured.message_id.to_string(),
            title: "Message".into(),
            snippet: "needle body".into(),
            timestamp: page
                .items
                .first()
                .and_then(|item| match item {
                    LocalSearchHit::Message { timestamp, .. } => Some(timestamp.clone()),
                    _ => None,
                })
                .unwrap(),
        }]
    );
}

#[test]
fn category_cursor_pages_without_duplicates_and_rejects_query_mismatch() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "cursor", &vault), &vault);
    for number in 0..3 {
        client
            .capture_incoming(incoming(
                &format!("needle {number}"),
                &format!("cursor-{number}"),
            ))
            .unwrap();
    }
    let mut first = request("needle");
    first.limit = Some(1);
    let first = client.search_local(first).unwrap();
    let cursor = first.next_cursor.clone().unwrap();
    let mut second = request("needle");
    second.limit = Some(2);
    second.cursor = Some(cursor);
    let second = client.search_local(second).unwrap();
    assert_eq!(first.items.len() + second.items.len(), 3);
    let mut mismatched = request("other");
    mismatched.cursor = first.next_cursor;
    assert!(matches!(
        client.search_local(mismatched),
        Err(Error::InvalidRequest("search cursor"))
    ));
}

#[test]
fn ambiguous_duplicate_contacts_leave_conversation_title_as_address() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "contacts", &vault);
    let client = unlocked(&config, &vault);
    let contacts = |book_id: &str| {
        json!([{
            "id": format!("{book_id}-alice"), "book_id": book_id, "revision": "1", "display_name": "Alice",
            "name": {"given": "Alice", "family": "Example"},
            "phones": [{"id": "phone", "value": ADDRESS}],
            "emails": [{"id": "email", "value": "alice@example.test"}], "addresses": [],
            "provenance": {"source_id": format!("source-{book_id}"), "read_only": false}
        }])
    };
    for book_id in ["personal", "work"] {
        client.capture_contact_book(&json!({
            "schema_version": 1,
            "book": {"id": book_id, "owner_device_id": config.device_id.to_string(), "generation": "1", "state": "active", "region": "US"},
            "contacts": contacts(book_id)
        }).to_string()).unwrap();
    }
    // Populate the existing durable address projection before matching conversations.
    client
        .resolve_contact_addresses_json(&json!({"addresses": [ADDRESS]}).to_string())
        .unwrap();
    let captured = client
        .capture_incoming(incoming("group message", "alice-group"))
        .unwrap();

    let page = client
        .search_local(LocalSearchRequest {
            query: "Alice".into(),
            kind: Some(SearchKind::Conversation),
            cursor: None,
            limit: Some(6),
        })
        .unwrap();
    assert_eq!(
        page.items,
        vec![LocalSearchHit::Conversation {
            conversation_id: captured.conversation_id.to_string(),
            title: ADDRESS.into(),
            subtitle: String::new(),
        }]
    );

    let contacts = client
        .search_local(LocalSearchRequest {
            query: "Alice".into(),
            kind: Some(SearchKind::Contact),
            cursor: None,
            limit: Some(6),
        })
        .unwrap();
    assert_eq!(contacts.items.len(), 2);
    assert!(contacts.items.iter().all(
        |hit| matches!(hit, LocalSearchHit::Contact { subtitle, .. } if subtitle.ends_with(ADDRESS))
    ));
}

#[test]
fn structured_search_finds_matches_after_six_hundred_source_rows() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "late-structured-match", &vault);
    let client = unlocked(&config, &vault);
    let contacts = (0..600)
        .map(|number| {
            json!({
                "id": format!("contact-{number:03}"), "book_id": "large", "revision": "1",
                "display_name": if number == 599 { "Late Needle" } else { "Ordinary Person" },
                "phones": [{"id": "phone", "value": format!("+1555000{number:04}")}],
                "emails": [], "addresses": [],
                "provenance": {"source_id": format!("source-{number:03}"), "read_only": false}
            })
        })
        .collect::<Vec<_>>();
    client.capture_contact_book(&json!({
        "schema_version": 1,
        "book": {"id": "large", "owner_device_id": config.device_id.to_string(), "generation": "1", "state": "active", "region": "US", "display_name": "Large book"},
        "contacts": contacts
    }).to_string()).unwrap();

    let page = client
        .search_local(LocalSearchRequest {
            query: "Late Needle".into(),
            kind: Some(SearchKind::Contact),
            cursor: None,
            limit: Some(1),
        })
        .unwrap();
    assert!(
        matches!(page.items.as_slice(), [LocalSearchHit::Contact { contact_id, .. }] if contact_id == "contact-599")
    );
    assert!(!page.has_more.contact);
}

#[test]
fn message_cursor_pages_twenty_mixed_length_hits_exactly_once() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "cursor-precision", &vault), &vault);
    for number in 0..20 {
        client
            .capture_incoming(incoming(
                &format!("needle {}", "padding ".repeat(number)),
                &format!("precision-{number}"),
            ))
            .unwrap();
    }
    let mut cursor = None;
    let mut ids = std::collections::BTreeSet::new();
    loop {
        let page = client
            .search_local(LocalSearchRequest {
                query: "needle".into(),
                kind: Some(SearchKind::Message),
                cursor,
                limit: Some(1),
            })
            .unwrap();
        for item in page.items {
            let LocalSearchHit::Message { message_id, .. } = item else {
                panic!("message hit expected")
            };
            assert!(
                ids.insert(message_id),
                "duplicate message across cursor pages"
            );
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids.len(), 20);
}

#[test]
fn mixed_text_does_not_match_phone_digits_but_phone_format_does() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "numeric-query", &vault);
    let client = unlocked(&config, &vault);
    client.capture_contact_book(&json!({
        "schema_version": 1,
        "book": {"id": "personal", "owner_device_id": config.device_id.to_string(), "generation": "1", "state": "active", "region": "US", "display_name": "Personal"},
        "contacts": [{"id":"bob","book_id":"personal","revision":"1","display_name":"Bob","phones":[{"id":"p","value":"(555) 0002"}],"emails":[],"addresses":[],"provenance":{"source_id":"s","read_only":false}}]
    }).to_string()).unwrap();
    let search = |query: &str| {
        client
            .search_local(LocalSearchRequest {
                query: query.into(),
                kind: Some(SearchKind::Contact),
                cursor: None,
                limit: Some(6),
            })
            .unwrap()
    };
    assert!(search("Alice2").items.is_empty());
    assert_eq!(search("(555) 0002").items.len(), 1);
}

#[test]
fn computed_display_name_fallbacks_retain_primary_subtitle() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "contact-labels", &vault);
    let client = unlocked(&config, &vault);
    client.capture_contact_book(&json!({
        "schema_version": 1,
        "book": {"id": "opaque-id", "owner_device_id": config.device_id.to_string(), "generation": "1", "state": "active", "region": "US", "display_name": "Coworkers"},
        "contacts": [{"id":"org","book_id":"opaque-id","revision":"1","display_name":"","organization":"Acme Labs","phones":[],"emails":[],"addresses":[],"provenance":{"source_id":"s","read_only":false}}]
    }).to_string()).unwrap();
    let page = client
        .search_local(LocalSearchRequest {
            query: "Acme".into(),
            kind: Some(SearchKind::Contact),
            cursor: None,
            limit: Some(6),
        })
        .unwrap();
    assert_eq!(
        page.items,
        vec![LocalSearchHit::Contact {
            book_id: "opaque-id".into(),
            contact_id: "org".into(),
            title: "Acme Labs".into(),
            subtitle: "Coworkers".into(),
        }]
    );
}

#[test]
fn contact_metadata_searches_user_fields_without_labels_or_internal_ids() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "contact-metadata", &vault);
    let client = unlocked(&config, &vault);
    client.capture_contact_book(&json!({
        "schema_version": 1,
        "book": {"id": "opaque-book", "owner_device_id": config.device_id.to_string(), "generation": "1", "state": "active", "region": "US", "display_name": "People"},
        "contacts": [{
            "id": "opaque-contact", "book_id": "opaque-book", "revision": "1", "display_name": "Ada",
            "name": {"given": "Ada", "family": "Lovelace"}, "nickname": "Enchantress", "organization": "Analytical Engines", "title": "Programmer", "notes": "Unicode context:\n café 東京 needle followed by enough text to trim safely.",
            "phones": [{"id": "private-phone", "label": "Mobile", "value": "+1 (555) 010-2222"}],
            "emails": [{"id": "private-email", "label": "Work", "value": "ada@analysis.test"}],
            "addresses": [{"id": "private-address", "label": "Home", "value": "12 St James Square", "city": "London", "postal_code": "SW1Y 4JH", "country": "United Kingdom"}],
            "birthday": {"year": 1815, "month": 12, "day": 10},
            "provenance": {"source_id": "private-source", "read_only": false}
        }]
    }).to_string()).unwrap();
    let search = |query: &str| {
        client
            .search_local(LocalSearchRequest {
                query: query.into(),
                kind: Some(SearchKind::Contact),
                cursor: None,
                limit: Some(6),
            })
            .unwrap()
    };

    for (query, field) in [
        ("Enchantress", "Nickname"),
        ("Engines", "Organization"),
        ("Programmer", "Title"),
        ("café", "Notes"),
        ("analysis.test", "Email"),
        ("James", "Address"),
        ("London", "Address"),
        ("1815-12-10", "Birthday"),
        ("12-10", "Birthday"),
    ] {
        assert!(
            matches!(search(query).items.as_slice(), [LocalSearchHit::Contact { subtitle, .. }] if subtitle.contains(&format!(" · {field} · "))),
            "{query}"
        );
    }
    let notes = search("café");
    assert!(
        matches!(notes.items.as_slice(), [LocalSearchHit::Contact { subtitle, .. }] if !subtitle.contains('\n') && subtitle.contains("café 東京 needle") && subtitle.rsplit(" · ").next().is_some_and(|snippet| snippet.chars().count() <= 61))
    );
    assert_eq!(search("(555) 010-2222").items.len(), 1);
    for query in ["Mobile", "Work", "Home", "opaque-contact", "private-source"] {
        assert!(search(query).items.is_empty(), "{query}");
    }
}

#[test]
fn name_and_phone_matches_rank_above_metadata_and_keep_phone_subtitle() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "contact-ranking", &vault);
    let client = unlocked(&config, &vault);
    client.capture_contact_book(&json!({
        "schema_version": 1,
        "book": {"id": "people", "owner_device_id": config.device_id.to_string(), "generation": "1", "state": "active", "region": "US", "display_name": "People"},
        "contacts": [
            {"id":"name","book_id":"people","revision":"1","display_name":"Needle Name","phones":[{"id":"p1","value":"+15550001"}],"emails":[],"addresses":[],"provenance":{"source_id":"s1","read_only":false}},
            {"id":"metadata","book_id":"people","revision":"1","display_name":"Other","notes":"Needle metadata","phones":[],"emails":[],"addresses":[],"provenance":{"source_id":"s2","read_only":false}},
            {"id":"phone","book_id":"people","revision":"1","display_name":"Phone","phones":[{"id":"p2","value":"+1 555 0002"}],"emails":[],"addresses":[],"provenance":{"source_id":"s3","read_only":false}}
        ]
    }).to_string()).unwrap();
    let names = client
        .search_local(LocalSearchRequest {
            query: "Needle".into(),
            kind: Some(SearchKind::Contact),
            cursor: None,
            limit: Some(6),
        })
        .unwrap();
    assert!(
        matches!(names.items.as_slice(), [LocalSearchHit::Contact { contact_id, subtitle, .. }, LocalSearchHit::Contact { contact_id: metadata_id, .. }] if contact_id == "name" && metadata_id == "metadata" && subtitle == "People · +15550001")
    );
    let phones = client
        .search_local(LocalSearchRequest {
            query: "(555) 0002".into(),
            kind: Some(SearchKind::Contact),
            cursor: None,
            limit: Some(6),
        })
        .unwrap();
    assert!(
        matches!(phones.items.as_slice(), [LocalSearchHit::Contact { contact_id, subtitle, .. }] if contact_id == "phone" && subtitle == "People · +1 555 0002")
    );
}

#[test]
fn metadata_snippets_map_expanding_unicode_matches_to_source_characters() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "unicode-snippet", &vault);
    let client = unlocked(&config, &vault);
    let notes = format!("{} needle after", "İ".repeat(100));
    client
        .capture_contact_book(
            &json!({
                "schema_version": 1,
                "book": {"id": "people", "owner_device_id": config.device_id.to_string(), "generation": "1", "state": "active", "region": "US"},
                "contacts": [{"id":"unicode","book_id":"people","revision":"1","display_name":"Unicode","notes":notes,"phones":[],"emails":[],"addresses":[],"provenance":{"source_id":"s","read_only":false}}]
            })
            .to_string(),
        )
        .unwrap();
    let page = client
        .search_local(LocalSearchRequest {
            query: "needle".into(),
            kind: Some(SearchKind::Contact),
            cursor: None,
            limit: Some(6),
        })
        .unwrap();
    assert!(
        matches!(page.items.as_slice(), [LocalSearchHit::Contact { subtitle, .. }] if subtitle.contains("Notes") && subtitle.contains("needle") && subtitle.rsplit(" · ").next().is_some_and(|snippet| snippet.chars().count() <= 61))
    );
}

#[test]
fn metadata_snippets_use_whole_string_lowercase_for_terminal_sigma() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "sigma-snippet", &vault);
    let client = unlocked(&config, &vault);
    let notes = format!("{} ΟΣ", "x".repeat(100));
    client
        .capture_contact_book(
            &json!({
                "schema_version": 1,
                "book": {"id": "people", "owner_device_id": config.device_id.to_string(), "generation": "1", "state": "active", "region": "US"},
                "contacts": [{"id":"sigma","book_id":"people","revision":"1","display_name":"Sigma","notes":notes,"phones":[],"emails":[],"addresses":[],"provenance":{"source_id":"s","read_only":false}}]
            })
            .to_string(),
        )
        .unwrap();
    let page = client
        .search_local(LocalSearchRequest {
            query: "ος".into(),
            kind: Some(SearchKind::Contact),
            cursor: None,
            limit: Some(6),
        })
        .unwrap();
    assert!(
        matches!(page.items.as_slice(), [LocalSearchHit::Contact { subtitle, .. }] if subtitle.contains("Notes") && subtitle.contains("ΟΣ"))
    );
}

fn hydrated_message_id(message: &Message) -> String {
    message.payload.record.message_id.to_string()
}

#[test]
fn history_windows_are_bounded_chronological_scoped_and_report_edges() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "history-windows", &vault), &vault);
    let messages = (0..55)
        .map(|number| {
            client
                .capture_incoming(incoming(
                    &format!("body {number}"),
                    &format!("history-{number}"),
                ))
                .unwrap()
        })
        .collect::<Vec<_>>();
    let conversation_id = messages[0].conversation_id.to_string();
    let ids = messages
        .iter()
        .map(|message| message.message_id.to_string())
        .collect::<Vec<_>>();

    let around = client
        .load_message_window(MessageWindowRequest::Around {
            conversation_id: conversation_id.clone(),
            message_id: ids[30].clone(),
        })
        .unwrap();
    assert_eq!(around.messages.len(), 50);
    assert!(around.has_older && !around.has_newer);
    assert_eq!(hydrated_message_id(&around.messages[25]), ids[30]);

    let older = client
        .load_message_window(MessageWindowRequest::Older {
            conversation_id: conversation_id.clone(),
            message_id: ids[30].clone(),
        })
        .unwrap();
    assert_eq!(older.messages.len(), 30);
    assert!(!older.has_older && older.has_newer);

    let newer = client
        .load_message_window(MessageWindowRequest::Newer {
            conversation_id: conversation_id.clone(),
            message_id: ids[30].clone(),
        })
        .unwrap();
    assert_eq!(newer.messages.len(), 24);
    assert!(newer.has_older && !newer.has_newer);

    let foreign = client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "+15555550999".into(),
            body: "foreign".into(),
            provider_message_id: Some("foreign-history".into()),
            imported: false,
        })
        .unwrap();
    let refresh = client
        .load_message_window(MessageWindowRequest::Refresh {
            conversation_id: conversation_id.clone(),
            message_ids: vec![
                ids[54].clone(),
                ids[1].clone(),
                ids[30].clone(),
                ids[1].clone(),
                foreign.message_id.to_string(),
                "missing".into(),
            ],
        })
        .unwrap();
    assert_eq!(
        refresh
            .messages
            .iter()
            .map(hydrated_message_id)
            .collect::<Vec<_>>(),
        vec![ids[1].clone(), ids[30].clone(), ids[54].clone()]
    );
    assert!(refresh.has_older);
    assert!(!refresh.has_newer);

    assert!(matches!(
        client.load_message_window(MessageWindowRequest::Refresh {
            conversation_id,
            message_ids: (0..151).map(|number| format!("id-{number}")).collect(),
        }),
        Err(Error::InvalidRequest("history refresh size"))
    ));
}

#[test]
fn conversation_search_finds_only_match_after_six_hundred_addresses() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "late-conversation-match", &vault);
    let client = unlocked(&config, &vault);
    let late_address = "+15550000599";
    let mut late_conversation = None;
    for number in 0..600 {
        let address = format!("+1555000{number:04}");
        let captured = client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: address,
                body: "ordinary".into(),
                provider_message_id: Some(format!("late-conversation-{number}")),
                imported: false,
            })
            .unwrap();
        if number == 599 {
            late_conversation = Some(captured.conversation_id.to_string());
        }
    }
    client.capture_contact_book(&json!({
        "schema_version": 1,
        "book": {"id": "personal", "owner_device_id": config.device_id.to_string(), "generation": "1", "state": "active", "region": "US", "display_name": "Personal"},
        "contacts": [{"id":"late","book_id":"personal","revision":"1","display_name":"Late Conversation Needle","phones":[{"id":"p","value":late_address}],"emails":[],"addresses":[],"provenance":{"source_id":"late-source","read_only":false}}]
    }).to_string()).unwrap();
    client
        .resolve_contact_addresses_json(&json!({"addresses":[late_address]}).to_string())
        .unwrap();
    let page = client
        .search_local(LocalSearchRequest {
            query: "Late Conversation Needle".into(),
            kind: Some(SearchKind::Conversation),
            cursor: None,
            limit: Some(1),
        })
        .unwrap();
    assert!(
        matches!(page.items.as_slice(), [LocalSearchHit::Conversation { conversation_id, .. }] if Some(conversation_id) == late_conversation.as_ref())
    );
    assert!(!page.has_more.conversation);
}

#[test]
fn conversation_results_rank_matching_recent_threads_first() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "conversation-ranking", &vault);
    let client = unlocked(&config, &vault);
    client.capture_contact_book(&json!({
        "schema_version": 1,
        "book": {"id": "personal", "owner_device_id": config.device_id.to_string(), "generation": "1", "state": "active", "region": "US", "display_name": "Personal"},
        "contacts": [
            {"id":"old","book_id":"personal","revision":"1","display_name":"Needle Old","phones":[{"id":"p1","value":"+15555550111"}],"emails":[],"addresses":[],"provenance":{"source_id":"old","read_only":false}},
            {"id":"new","book_id":"personal","revision":"1","display_name":"Needle New","phones":[{"id":"p2","value":"+15555550222"}],"emails":[],"addresses":[],"provenance":{"source_id":"new","read_only":false}}
        ]
    }).to_string()).unwrap();
    client
        .resolve_contact_addresses_json(
            &json!({"addresses":["+15555550111","+15555550222"]}).to_string(),
        )
        .unwrap();
    let old = client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "+15555550111".into(),
            body: "old".into(),
            provider_message_id: Some("rank-old".into()),
            imported: false,
        })
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let new = client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "+15555550222".into(),
            body: "new".into(),
            provider_message_id: Some("rank-new".into()),
            imported: false,
        })
        .unwrap();
    let page = client
        .search_local(LocalSearchRequest {
            query: "Needle".into(),
            kind: Some(SearchKind::Conversation),
            cursor: None,
            limit: Some(2),
        })
        .unwrap();
    let ids = page
        .items
        .into_iter()
        .map(|hit| match hit {
            LocalSearchHit::Conversation {
                conversation_id, ..
            } => conversation_id,
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            new.conversation_id.to_string(),
            old.conversation_id.to_string()
        ]
    );
}

#[test]
fn search_index_driver_rejects_out_of_range_budgets() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "index-budget", &vault), &vault);
    assert!(matches!(
        client.advance_search_index(Some(0)),
        Err(Error::InvalidRequest("search index batch size"))
    ));
    assert!(matches!(
        client.advance_search_index(Some(101)),
        Err(Error::InvalidRequest("search index batch size"))
    ));
}

fn collect_category_ids(
    client: &Client,
    query: &str,
    kind: SearchKind,
) -> std::collections::BTreeSet<String> {
    let mut cursor = None;
    let mut ids = std::collections::BTreeSet::new();
    loop {
        let page = client
            .search_local(LocalSearchRequest {
                query: query.into(),
                kind: Some(kind),
                cursor,
                limit: Some(1),
            })
            .unwrap();
        for item in page.items {
            let id = match item {
                LocalSearchHit::Conversation {
                    conversation_id, ..
                } => conversation_id,
                LocalSearchHit::Contact {
                    book_id,
                    contact_id,
                    ..
                } => format!("{book_id}/{contact_id}"),
                LocalSearchHit::Message { .. } => panic!("structured hit expected"),
            };
            assert!(ids.insert(id), "duplicate result across cursor pages");
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    ids
}

#[test]
fn contact_and_conversation_cursors_page_ten_results_exactly_once() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "structured-cursor-completeness", &vault);
    let client = unlocked(&config, &vault);
    let contacts = (0..10)
        .map(|number| {
            json!({
                "id": format!("person-{number:02}"), "book_id": "people", "revision": "1",
                "display_name": format!("Needle Person {number:02}"),
                "phones": [{"id": "phone", "value": format!("+15555551{number:02}")}],
                "emails": [], "addresses": [],
                "provenance": {"source_id": format!("source-{number:02}"), "read_only": false}
            })
        })
        .collect::<Vec<_>>();
    client
        .capture_contact_book(
            &json!({
                "schema_version": 1,
                "book": {"id": "people", "owner_device_id": config.device_id.to_string(), "generation": "1", "state": "active", "region": "US", "display_name": "People"},
                "contacts": contacts
            })
            .to_string(),
        )
        .unwrap();
    let addresses = (0..10)
        .map(|number| format!("+15555551{number:02}"))
        .collect::<Vec<_>>();
    client
        .resolve_contact_addresses_json(&json!({"addresses": addresses}).to_string())
        .unwrap();
    for number in 0..10 {
        client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: format!("+15555551{number:02}"),
                body: "cursor fixture".into(),
                provider_message_id: Some(format!("structured-cursor-{number:02}")),
                imported: false,
            })
            .unwrap();
    }

    assert_eq!(
        collect_category_ids(&client, "Needle", SearchKind::Contact).len(),
        10
    );
    assert_eq!(
        collect_category_ids(&client, "Needle", SearchKind::Conversation).len(),
        10
    );
}

#[test]
fn searchable_contact_edit_and_delete_invalidate_structured_cursors() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "structured-cursor-staleness", &vault);
    let client = unlocked(&config, &vault);
    let capture = |generation: &str, names: &[&str]| {
        let contacts = names
            .iter()
            .enumerate()
            .map(|(number, name)| {
                json!({
                    "id": format!("person-{number}"), "book_id": "people", "revision": generation,
                    "display_name": name,
                    "phones": [{"id": "phone", "value": format!("+1555555200{number}")}],
                    "emails": [], "addresses": [],
                    "provenance": {"source_id": format!("source-{number}"), "read_only": false}
                })
            })
            .collect::<Vec<_>>();
        client
            .capture_contact_book(
                &json!({
                    "schema_version": 1,
                    "book": {"id": "people", "owner_device_id": config.device_id.to_string(), "generation": generation, "state": "active", "region": "US", "display_name": "People"},
                    "contacts": contacts
                })
                .to_string(),
            )
            .unwrap();
    };
    capture("1", &["Needle Alpha", "Needle Beta", "Needle Gamma"]);
    let first = client
        .search_local(LocalSearchRequest {
            query: "Needle".into(),
            kind: Some(SearchKind::Contact),
            cursor: None,
            limit: Some(1),
        })
        .unwrap();
    let edited_cursor = first.next_cursor.unwrap();
    capture("2", &["Renamed Alpha", "Needle Beta", "Needle Gamma"]);
    assert!(matches!(
        client.search_local(LocalSearchRequest {
            query: "Needle".into(),
            kind: Some(SearchKind::Contact),
            cursor: Some(edited_cursor),
            limit: Some(1),
        }),
        Err(Error::InvalidRequest("search cursor"))
    ));

    let first = client
        .search_local(LocalSearchRequest {
            query: "Needle".into(),
            kind: Some(SearchKind::Contact),
            cursor: None,
            limit: Some(1),
        })
        .unwrap();
    let deleted_cursor = first.next_cursor.unwrap();
    client
        .begin_contact_scan(
            &json!({"schema_version":1,"scan_id":"delete-anchor","book_id":"people","generation":"2","access":"full","authoritative":true}).to_string(),
        )
        .unwrap();
    for (id, name, number) in [
        ("person-0", "Renamed Alpha", 0),
        ("person-2", "Needle Gamma", 2),
    ] {
        client
            .observe_contact_scan(
                &json!({
                    "schema_version":1,"scan_id":"delete-anchor",
                    "contact":{"id":id,"book_id":"people","revision":"2","display_name":name,
                    "phones":[{"id":"phone","value":format!("+1555555200{number}")}],
                    "emails":[],"addresses":[],"provenance":{"source_id":format!("source-{number}"),"read_only":false}}
                }).to_string(),
            )
            .unwrap();
    }
    client
        .finish_contact_scan(
            &json!({"schema_version":1,"scan_id":"delete-anchor","complete":true}).to_string(),
        )
        .unwrap();
    let current = client
        .search_local(LocalSearchRequest {
            query: "Needle".into(),
            kind: Some(SearchKind::Contact),
            cursor: None,
            limit: Some(50),
        })
        .unwrap();
    assert!(current.items.iter().all(|item| {
        !matches!(item, LocalSearchHit::Contact { contact_id, .. } if contact_id == "person-1")
    }));
    assert!(matches!(
        client.search_local(LocalSearchRequest {
            query: "Needle".into(),
            kind: Some(SearchKind::Contact),
            cursor: Some(deleted_cursor),
            limit: Some(1),
        }),
        Err(Error::InvalidRequest("search cursor"))
    ));
}

#[test]
fn structured_name_fallback_prefers_full_given_family() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "structured-name-fallback", &vault);
    let client = unlocked(&config, &vault);
    client
        .capture_contact_book(
            &json!({
                "schema_version": 1,
                "book": {"id": "people", "owner_device_id": config.device_id.to_string(), "generation": "1", "state": "active", "region": "US", "display_name": "People"},
                "contacts": [{
                    "id": "alice", "book_id": "people", "revision": "1", "display_name": "",
                    "name": {"given": "Alice", "family": "Example"}, "nickname": "Al", "organization": "Acme",
                    "phones": [], "emails": [], "addresses": [],
                    "provenance": {"source_id": "alice-source", "read_only": false}
                }]
            })
            .to_string(),
        )
        .unwrap();
    let page = client
        .search_local(LocalSearchRequest {
            query: "Alice".into(),
            kind: Some(SearchKind::Contact),
            cursor: None,
            limit: Some(1),
        })
        .unwrap();
    assert!(matches!(
        page.items.as_slice(),
        [LocalSearchHit::Contact { title, .. }] if title == "Alice Example"
    ));
}

#[test]
fn contact_edit_invalidates_conversation_cursor() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "conversation-cursor-staleness", &vault);
    let client = unlocked(&config, &vault);
    let capture = |generation: &str, first_name: &str| {
        client
            .capture_contact_book(
                &json!({
                    "schema_version": 1,
                    "book": {"id": "people", "owner_device_id": config.device_id.to_string(), "generation": generation, "state": "active", "region": "US", "display_name": "People"},
                    "contacts": [
                        {"id":"first","book_id":"people","revision":generation,"display_name":first_name,"phones":[{"id":"p1","value":"+15555553001"}],"emails":[],"addresses":[],"provenance":{"source_id":"first","read_only":false}},
                        {"id":"second","book_id":"people","revision":generation,"display_name":"Needle Second","phones":[{"id":"p2","value":"+15555553002"}],"emails":[],"addresses":[],"provenance":{"source_id":"second","read_only":false}}
                    ]
                })
                .to_string(),
            )
            .unwrap();
    };
    capture("1", "Needle First");
    client
        .resolve_contact_addresses_json(
            &json!({"addresses":["+15555553001", "+15555553002"]}).to_string(),
        )
        .unwrap();
    for (number, address) in ["+15555553001", "+15555553002"].into_iter().enumerate() {
        client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: address.into(),
                body: "conversation cursor".into(),
                provider_message_id: Some(format!("conversation-stale-{number}")),
                imported: false,
            })
            .unwrap();
    }
    let first = client
        .search_local(LocalSearchRequest {
            query: "Needle".into(),
            kind: Some(SearchKind::Conversation),
            cursor: None,
            limit: Some(1),
        })
        .unwrap();
    let cursor = first.next_cursor.unwrap();

    capture("2", "Renamed First");
    assert!(matches!(
        client.search_local(LocalSearchRequest {
            query: "Needle".into(),
            kind: Some(SearchKind::Conversation),
            cursor: Some(cursor),
            limit: Some(1),
        }),
        Err(Error::InvalidRequest("search cursor"))
    ));
}

#[test]
fn message_query_treats_fts_syntax_literally_and_does_not_search_json_keys() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&directory, "literal-search", &vault), &vault);
    client
        .capture_incoming(incoming(
            "NEAR( quoted star OR body colon ordinary",
            "literal-search-message",
        ))
        .unwrap();
    for query in ["NEAR(", "quoted\"", "star*", "OR", "body:"] {
        client.search_local(request(query)).unwrap();
    }
    assert!(
        client
            .search_local(request("provider_message_id"))
            .unwrap()
            .items
            .is_empty()
    );
}
