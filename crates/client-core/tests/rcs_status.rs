#[path = "common/mod.rs"]
mod common;

use common::*;

fn record(body: &str) -> RcsProjectedRecord {
    RcsProjectedRecord {
        google_message_id: "provider-message".into(),
        google_conversation_id: "provider-conversation".into(),
        direction: Direction::Incoming,
        body: body.into(),
        sent_at_ms: Some(1),
        tmp_id: None,
        rcs_reported: true,
        e2ee_reported: Some(true),
        history_token: None,
    }
}

#[test]
fn deterministic_rcs_message_ids_converge_without_exposing_provider_identity() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_config = config(&directory, "first", &vault);
    let second_config = config(&directory, "second", &vault);
    let first = unlocked(&first_config, &vault);
    let second = unlocked(&second_config, &vault);
    let conversation = ConversationId::new();
    let first_owner = first
        .begin_rcs_link_ownership(conversation, "first-token", 0)
        .unwrap();
    let second_owner = second
        .begin_rcs_link_ownership(conversation, "second-token", 0)
        .unwrap();

    assert_eq!(
        first
            .ingest_rcs_projected_record(
                conversation,
                first_owner.ownership_generation,
                record("one")
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    assert_eq!(
        second
            .ingest_rcs_projected_record(
                conversation,
                second_owner.ownership_generation,
                record("one")
            )
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    let first_id = only_message(&first, conversation).payload.record.message_id;
    let second_id = only_message(&second, conversation)
        .payload
        .record
        .message_id;
    assert_eq!(first_id, second_id);
    assert_ne!(first_id.to_string(), "provider-message");
    assert_ne!(first_id.to_string(), "provider-conversation");

    let other_vault = Vault::new();
    let other_config = config(&directory, "other", &other_vault);
    let other = unlocked(&other_config, &other_vault);
    let other_owner = other
        .begin_rcs_link_ownership(conversation, "other-token", 0)
        .unwrap();
    other
        .ingest_rcs_projected_record(
            conversation,
            other_owner.ownership_generation,
            record("one"),
        )
        .unwrap();
    assert_ne!(
        first_id,
        only_message(&other, conversation).payload.record.message_id
    );
}

#[test]
fn replicated_rcs_messages_are_idempotent() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let first_config = config(&directory, "first", &vault);
    let second_config = config(&directory, "second", &vault);
    let first = unlocked(&first_config, &vault);
    let second = unlocked(&second_config, &vault);
    let conversation = ConversationId::new();
    let first_owner = first
        .begin_rcs_link_ownership(conversation, "first-token", 0)
        .unwrap();
    let second_owner = second
        .begin_rcs_link_ownership(conversation, "second-token", 0)
        .unwrap();
    first
        .ingest_rcs_projected_record(
            conversation,
            first_owner.ownership_generation,
            record("one"),
        )
        .unwrap();
    second
        .ingest_rcs_projected_record(
            conversation,
            second_owner.ownership_generation,
            record("one"),
        )
        .unwrap();

    let mut server = Server::default();
    server.upload(&first);
    server.upload(&second);
    server.sync(&first);
    server.sync(&second);
    assert_eq!(first.messages(conversation).unwrap().len(), 1);
    assert_eq!(second.messages(conversation).unwrap().len(), 1);
}

#[test]
fn incoming_rcs_replay_stale_writer_and_page_floor_are_rejected_without_rows() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client
        .begin_rcs_link_ownership(conversation, "first-token", 10)
        .unwrap();

    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("one"))
            .unwrap(),
        RcsReceiveOutcome::Inserted
    );
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("one"))
            .unwrap(),
        RcsReceiveOutcome::Duplicate
    );
    assert_eq!(client.messages(conversation).unwrap().len(), 1);
    assert_eq!(
        client
            .ingest_rcs_projected_record(
                conversation,
                owner.ownership_generation + 1,
                record("two")
            )
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
    let mut page = record("page");
    page.google_message_id = "page-message".into();
    page.history_token = Some("page-token".into());
    page.sent_at_ms = Some(10);
    assert_eq!(
        client
            .ingest_rcs_projected_record(conversation, owner.ownership_generation, page)
            .unwrap(),
        RcsReceiveOutcome::Rejected(RcsReceiveCode::ProviderFloor)
    );
    assert_eq!(client.messages(conversation).unwrap().len(), 1);
}

#[test]
fn page_records_are_imported_while_live_records_remain_unseen() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client
        .begin_rcs_link_ownership(conversation, "token", 0)
        .unwrap();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("live"))
        .unwrap();
    let mut page = record("page");
    page.google_message_id = "page-message".into();
    page.history_token = Some("page-token".into());
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, page)
        .unwrap();

    let messages = client.messages(conversation).unwrap();
    assert_eq!(messages.len(), 2);
    assert!(
        messages
            .iter()
            .any(|message| message.payload.body == "live" && !message.seen)
    );
    assert!(
        messages
            .iter()
            .any(|message| message.payload.body == "page" && message.seen)
    );
    assert_eq!(
        client
            .rcs_receive_state(conversation)
            .unwrap()
            .unwrap()
            .history_token
            .as_deref(),
        Some("page-token")
    );
}

#[test]
fn rcs_status_lattice_is_monotonic() {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "client", &vault);
    let client = unlocked(&config, &vault);
    let conversation = ConversationId::new();
    let owner = client
        .begin_rcs_link_ownership(conversation, "token", 0)
        .unwrap();
    client
        .ingest_rcs_projected_record(conversation, owner.ownership_generation, record("live"))
        .unwrap();
    for status in [
        RcsProjectedStatus::Sending,
        RcsProjectedStatus::Sent,
        RcsProjectedStatus::Failed,
        RcsProjectedStatus::Delivered,
        RcsProjectedStatus::Read,
    ] {
        assert_eq!(
            client
                .apply_rcs_projected_status(
                    conversation,
                    owner.ownership_generation,
                    RcsStatusApplication {
                        google_message_id: "provider-message".into(),
                        google_conversation_id: "provider-conversation".into(),
                        tmp_id: None,
                        status,
                    },
                )
                .unwrap(),
            RcsReceiveOutcome::StatusApplied
        );
    }
    assert_eq!(
        client
            .apply_rcs_projected_status(
                conversation,
                owner.ownership_generation,
                RcsStatusApplication {
                    google_message_id: "provider-message".into(),
                    google_conversation_id: "provider-conversation".into(),
                    tmp_id: None,
                    status: RcsProjectedStatus::Failed,
                },
            )
            .unwrap(),
        RcsReceiveOutcome::StatusUnchanged
    );
}
