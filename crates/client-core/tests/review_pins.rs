use crate::common::*;
use peppy_client_core::{ConversationId, MessageWindowRequest};

#[test]
fn group_name_only_draft_edit_is_not_authored() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let draft = client.recipientless_compose_draft().unwrap();
    let saved = client
        .save_compose_draft(
            draft.draft_id,
            draft.revision,
            ComposeDraftUpdate {
                group_name: Some("Group".into()),
                text: String::new(),
                recipients: vec![],
                attachment_ids: vec![],
                route: None,
            },
        )
        .unwrap();

    // Pins current behaviour of review finding D-06; flip this assertion when the fix is authorized.
    let pushes = client.pending_draft_pushes().unwrap();
    assert_eq!(pushes.len(), 1);
    assert!(!pushes[0].authored);
    assert_eq!(saved.group_name.as_deref(), Some("Group"));
}

#[test]
fn self_target_history_request_stays_queued() {
    use peppy_client_core::{HistorySearchReplayProof, HistorySearchSyncFacts, HistorySearchWork};
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&dir, "history", &vault);
    let client = unlocked(&config, &vault);
    client.set_server_compaction_state(true, true).unwrap();
    for _ in 0..16 {
        let report: serde_json::Value =
            serde_json::from_str(&client.compaction_backfill_step_json(100).unwrap()).unwrap();
        if report["state"] == "ready" {
            break;
        }
    }
    client.publish_message_history_source(r#"{"generation":"1","revision":"1","capabilities":{"sms":true,"mms":false},"permission":true,"policy":true,"exhausted":false}"#).unwrap();
    let attempt = client
        .start_history_search(uuid::Uuid::new_v4())
        .unwrap()
        .attempt_id
        .unwrap();
    let context = client.history_search_sync_context().unwrap();
    client
        .observe_history_search_sync(
            attempt,
            HistorySearchSyncFacts {
                proof: Some(HistorySearchReplayProof {
                    context,
                    high_water_cursor: "0".into(),
                    next_after_present: true,
                    terminal_next_after: true,
                }),
            },
        )
        .unwrap();
    let HistorySearchWork::RequestPhone {
        request_id,
        target_device_id,
    } = client.history_search_work(attempt).unwrap()
    else {
        panic!("expected request")
    };
    assert_eq!(target_device_id, config.device_id.to_string());
    client.pending_message_history_requests("{}").unwrap();
    let status: serde_json::Value = serde_json::from_str(
        &client
            .message_history_request_status(
                &serde_json::json!({"request_id": request_id}).to_string(),
            )
            .unwrap(),
    )
    .unwrap();
    // Pins current behaviour of review finding D-10; flip this assertion when the fix is authorized.
    assert_eq!(status["state"], "queued");
}

#[test]
fn confirmed_cancelled_message_appears_in_history_window() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let sender_config = config(&dir, "sender", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let sender = unlocked(&sender_config, &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let conversation = ConversationId::new();
    let queued = sender
        .queue_send(sms(conversation, "cancelled"), route(&gateway_config))
        .unwrap();
    let mut server = Server::default();
    server.upload(&sender);
    server.sync(&gateway);
    gateway.publish_send_cancellation_capability(true).unwrap();
    server.upload(&gateway);
    server.sync(&sender);
    sender.request_send_cancellation(queued.message_id).unwrap();
    server.upload(&sender);
    server.sync(&gateway);
    assert!(gateway.process_pending_send_cancellations().unwrap());
    server.upload(&gateway);
    server.sync(&sender);

    let window = sender
        .load_message_window(MessageWindowRequest::Refresh {
            conversation_id: conversation.to_string(),
            message_ids: vec![queued.message_id.to_string()],
        })
        .unwrap();
    // Pins current behaviour of review finding D-09; flip this assertion when the fix is authorized.
    assert_eq!(window.messages.len(), 1);
    assert_eq!(
        window.messages[0].payload.record.message_id,
        queued.message_id
    );
}

#[test]
fn second_attachment_requirement_remains_unbound() {
    use peppy_client_core::AttachmentUseKind;
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "sender", &vault);
    let gateway = config(&dir, "gateway", &vault);
    let client = unlocked(&cfg, &vault);
    let mut ids = Vec::new();
    for name in ["first", "second"] {
        let path = dir.path().join(name);
        std::fs::write(&path, name).unwrap();
        ids.push(
            client
                .prepare_attachment(&path, "text/plain", name)
                .unwrap()
                .attachment_id,
        );
    }
    for id in &ids {
        client
            .begin_attachment_retention_use(*id, AttachmentUseKind::PendingSend)
            .unwrap();
    }
    for _ in 0..2 {
        client
            .queue_mms(
                OutgoingMms {
                    conversation_id: ConversationId::new(),
                    recipients: vec![ADDRESS.into()],
                    body: "two attachments".into(),
                    attachment_ids: ids.clone(),
                    subject: None,
                },
                route(&gateway),
            )
            .unwrap();
    }
    for id in &ids {
        client
            .begin_attachment_retention_use(*id, AttachmentUseKind::PendingSend)
            .unwrap();
    }
    let conn = rusqlite::Connection::open(&cfg.database_path).unwrap();
    conn.execute_batch("PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\"; PRAGMA cipher_compatibility = 4;").unwrap();
    let bound = |id: &AttachmentId| -> i64 {
        conn.query_row("SELECT COUNT(*) FROM attachment_retention_requirements r JOIN attachment_retention_effects e ON e.envelope_id=r.envelope_id AND e.attachment_id=r.attachment_id AND e.kind=r.kind WHERE r.attachment_id=? AND r.kind='pending_send'", [id.to_string()], |row| row.get(0)).unwrap()
    };
    // Pins current behaviour of review finding D-11; flip this assertion when the fix is authorized.
    assert!(bound(&ids[0]) > 0);
    assert_eq!(bound(&ids[0]), 2);
    assert_eq!(bound(&ids[1]), 1);
}

#[test]
fn future_schema_open_creates_table_before_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "future", &vault);
    drop(unlocked(&cfg, &vault));
    let open_database = || {
        let conn = rusqlite::Connection::open(&cfg.database_path).unwrap();
        conn.execute_batch("PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\"; PRAGMA cipher_compatibility = 4;").unwrap();
        conn
    };
    let conn = open_database();
    conn.execute_batch(
        "UPDATE schema_meta SET version=999999; DROP TABLE message_history_orphans;",
    )
    .unwrap();
    let table_exists = |conn: &rusqlite::Connection| -> bool {
        conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='message_history_orphans')", [], |row| row.get(0)).unwrap()
    };
    assert!(!table_exists(&conn));
    drop(conn);
    // Pins current behaviour of review finding D-13; flip this assertion when the fix is authorized.
    assert!(matches!(
        Client::open(cfg.clone(), DatabaseKey::new(&DB_KEY).unwrap()),
        Err(peppy_client_core::Error::UnsupportedSchema)
    ));
    assert!(table_exists(&open_database()));
}

#[test]
fn quoted_contact_name_is_missed_by_search() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "contacts", &vault);
    let client = unlocked(&cfg, &vault);
    client.capture_contact_book(&serde_json::json!({
        "schema_version": 1,
        "book": {"id": "book", "owner_device_id": cfg.device_id.to_string(), "generation": "1", "state": "active", "region": "US"},
        "contacts": [{"id":"one","book_id":"book","display_name":"Alice \"Ace\" Smith","name":{},"phones":[{"id":"p1","value":"+12025550100"}],"emails":[],"addresses":[]}]
    }).to_string()).unwrap();
    let search = |query| -> serde_json::Value {
        serde_json::from_str(
            &client
                .search_contact_recipients_json(
                    &serde_json::json!({"schema_version":1,"query":query}).to_string(),
                )
                .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(search("Alice")["results"].as_array().unwrap().len(), 1);
    // Pins current behaviour of review finding D-41; flip this assertion when the fix is authorized.
    assert_eq!(search("\"Ace\"")["results"], serde_json::json!([]));
}
