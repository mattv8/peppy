mod common;

use common::*;
use tempfile::TempDir;

#[test]
fn revoked_producer_retires_only_unattempted_commands_once() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let revoked_cfg = config(&dir, "revoked", &vault);
    let other_cfg = config(&dir, "other", &vault);
    let gateway_cfg = config(&dir, "gateway", &vault);
    let revoked = unlocked(&revoked_cfg, &vault);
    let other = unlocked(&other_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    gateway
        .record_rcs_link_lifecycle(RcsCapabilityState::Unlinked, false, false, false, 1, 1)
        .unwrap();
    let conversation = ConversationId::new();
    let retired = revoked
        .queue_send(sms(conversation, "retired"), route(&gateway_cfg))
        .unwrap();
    let attempted = revoked
        .queue_send(sms(conversation, "attempted"), route(&gateway_cfg))
        .unwrap();
    let retained = other
        .queue_send(sms(conversation, "retained"), route(&gateway_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&revoked);
    server.upload(&other);
    server.sync(&gateway);
    assert!(matches!(
        gateway.begin_send_attempt(attempted.command_id).unwrap(),
        PermitDecision::Permit(_)
    ));
    let before = gateway.pending_outbox().unwrap().len();

    assert_eq!(
        gateway
            .retire_unattempted_commands_from_producers(&[revoked_cfg.device_id])
            .unwrap(),
        1
    );
    assert_eq!(
        gateway
            .retire_unattempted_commands_from_producers(&[revoked_cfg.device_id])
            .unwrap(),
        0
    );
    assert_eq!(gateway.pending_outbox().unwrap().len(), before + 1);
    let pending = gateway.pending_commands().unwrap();
    assert!(pending.iter().any(|c| c.command_id == retained.command_id));
    assert!(!pending.iter().any(|c| c.command_id == retired.command_id));
    assert_eq!(
        gateway
            .begin_send_attempt_at(retired.command_id, 1)
            .unwrap(),
        PermitDecision::Blocked(PermitBlock::NotReceived)
    );
    assert_eq!(
        gateway
            .begin_automatic_send_attempt(retired.command_id, None, 1)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::NotReceived)
    );
    let db = rusqlite::Connection::open(&gateway_cfg.database_path).unwrap();
    db.execute_batch(&format!("PRAGMA key = \"x'{}'\";", "07".repeat(32)))
        .unwrap();
    let attempts: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM attempts WHERE command_id=?",
            [retired.command_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(attempts, 0);
    drop(db);
    assert!(matches!(
        gateway.prepare_native_rcs_send(retired.command_id, 1),
        Err(Error::NotFound)
    ));
    server.upload(&gateway);
    server.sync(&revoked);
    assert_eq!(
        revoked
            .message_by_id(retired.message_id)
            .unwrap()
            .unwrap()
            .send_state,
        Some(SendState::FailedBeforeSubmission)
    );
    let db = rusqlite::Connection::open(&gateway_cfg.database_path).unwrap();
    db.execute_batch(&format!("PRAGMA key = \"x'{}'\";", "07".repeat(32)))
        .unwrap();
    db.execute(
        "UPDATE command_ledger SET retired=1 WHERE command_id=?",
        [attempted.command_id.to_string()],
    )
    .unwrap();
    assert!(matches!(
        gateway.begin_send_attempt(attempted.command_id).unwrap(),
        PermitDecision::AlreadyAttempted(_)
    ));
    assert!(matches!(
        gateway
            .begin_automatic_send_attempt(attempted.command_id, None, 1)
            .unwrap(),
        AutomaticPermitDecision::AlreadyAttempted(_)
    ));
}

#[test]
fn local_producer_is_never_retired() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_cfg = config(&dir, "gateway", &vault);
    let producer = unlocked(&config(&dir, "producer", &vault), &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let queued = producer
        .queue_send(sms(ConversationId::new(), "local"), route(&gateway_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&producer);
    server.sync(&gateway);
    let db = rusqlite::Connection::open(&gateway_cfg.database_path).unwrap();
    db.execute_batch(&format!("PRAGMA key = \"x'{}'\";", "07".repeat(32)))
        .unwrap();
    db.execute(
        "UPDATE messages SET source_device_id=? WHERE id=?",
        rusqlite::params![
            gateway_cfg.device_id.to_string(),
            queued.message_id.to_string()
        ],
    )
    .unwrap();
    drop(db);
    assert_eq!(
        gateway
            .retire_unattempted_commands_from_producers(&[gateway_cfg.device_id])
            .unwrap(),
        0
    );
    assert!(
        gateway
            .pending_commands()
            .unwrap()
            .iter()
            .any(|c| c.command_id == queued.command_id)
    );
}

#[test]
fn retired_command_cannot_get_an_automatic_permit() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let producer_cfg = config(&dir, "producer-auto", &vault);
    let gateway_cfg = config(&dir, "gateway-auto", &vault);
    let producer = unlocked(&producer_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    gateway
        .record_rcs_link_lifecycle(RcsCapabilityState::Unlinked, false, false, false, 1, 1)
        .unwrap();
    let queued = producer
        .queue_send(sms(ConversationId::new(), "automatic"), route(&gateway_cfg))
        .unwrap();
    let mut server = Server::default();
    server.upload(&producer);
    server.sync(&gateway);
    assert_eq!(
        gateway
            .retire_unattempted_commands_from_producers(&[producer_cfg.device_id])
            .unwrap(),
        1
    );
    assert_eq!(
        gateway
            .begin_automatic_send_attempt(queued.command_id, None, 1)
            .unwrap(),
        AutomaticPermitDecision::Blocked(PermitBlock::NotReceived)
    );
}

#[test]
fn revoked_media_slots_preserve_permit_evidence() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let producer_cfg = config(&dir, "producer", &vault);
    let gateway_cfg = config(&dir, "gateway", &vault);
    let producer = unlocked(&producer_cfg, &vault);
    let gateway = unlocked(&gateway_cfg, &vault);
    let mut ids = Vec::new();
    for i in 0..3 {
        let path = dir.path().join(format!("attachment-{i}"));
        std::fs::write(&path, b"media").unwrap();
        let attachment = producer
            .prepare_attachment(&path, "image/jpeg", "photo.jpg")
            .unwrap();
        producer
            .mark_attachment_uploaded(attachment.attachment_id, &uuid::Uuid::new_v4().to_string())
            .unwrap();
        ids.push(attachment.attachment_id);
    }
    let queued = producer
        .queue_mms(
            OutgoingMms {
                conversation_id: ConversationId::new(),
                recipients: vec![ADDRESS.into()],
                body: "media".into(),
                attachment_ids: ids.clone(),
                subject: None,
            },
            route(&gateway_cfg),
        )
        .unwrap();
    let mut server = Server::default();
    server.upload(&producer);
    server.sync(&gateway);
    let db = rusqlite::Connection::open(&gateway_cfg.database_path).unwrap();
    db.execute_batch(&format!("PRAGMA key = \"x'{}'\";", "07".repeat(32)))
        .unwrap();
    for (index, state) in ["pending", "permit_issued", "accepted"].iter().enumerate() {
        db.execute("INSERT INTO rcs_media_send_ledger(command_id,attachment_index,attachment_id,state) VALUES(?,?,?,?)",
            rusqlite::params![queued.command_id.to_string(), index as i64, ids[index].to_string(), state]).unwrap();
    }
    assert_eq!(
        gateway
            .retire_unattempted_commands_from_producers(&[producer_cfg.device_id])
            .unwrap(),
        1
    );
    let mut query = db
        .prepare(
            "SELECT state FROM rcs_media_send_ledger WHERE command_id=? ORDER BY attachment_index",
        )
        .unwrap();
    let states: Vec<String> = query
        .query_map([queued.command_id.to_string()], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(states, ["refused", "outcome_unknown", "accepted"]);
    let statuses: Vec<(i64, String, Option<String>)> = db.prepare("SELECT attachment_index,state,reason FROM rcs_media_send_status WHERE message_id=? ORDER BY attachment_index").unwrap()
        .query_map([queued.message_id.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect();
    assert_eq!(
        statuses,
        [
            (0, "not_sent".into(), Some("failed".into())),
            (1, "outcome_unknown".into(), None)
        ]
    );
    assert_eq!(
        gateway
            .retire_unattempted_commands_from_producers(&[producer_cfg.device_id])
            .unwrap(),
        0
    );
    assert!(matches!(
        gateway.prepare_native_rcs_media_send(queued.command_id, 0, 1),
        Err(Error::NotFound)
    ));
    assert!(
        !gateway
            .pending_commands()
            .unwrap()
            .iter()
            .any(|c| c.command_id == queued.command_id)
    );
}
