use crate::common::*;
use tempfile::TempDir;

fn mark_snapshot(client: &Client) -> Result<u64, Error> {
    client
        .unread_message_batches()?
        .iter()
        .try_fold(0, |total, batch| Ok(total + client.mark_read_batch(batch)?))
}

#[test]
fn through_missing_message_is_empty() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "reader", &vault), &vault);
    assert!(
        client
            .unread_message_batches_through(MessageId::new())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn through_marks_target_and_earlier_unread_only_in_its_conversation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "reader", &vault), &vault);
    let first = client.capture_incoming(incoming("first", "first")).unwrap();
    let target = client
        .capture_incoming(incoming("target", "target"))
        .unwrap();
    let later = client.capture_incoming(incoming("later", "later")).unwrap();
    let other = client
        .capture_incoming(IncomingSms {
            sender_address: "+15555550101".into(),
            ..incoming("other", "other")
        })
        .unwrap();
    let batches = client
        .unread_message_batches_through(target.message_id)
        .unwrap();
    let ids: Vec<_> = batches.iter().flatten().copied().collect();
    assert_eq!(ids, vec![first.message_id, target.message_id]);
    for batch in batches {
        client.mark_read_batch(&batch).unwrap();
    }
    assert_eq!(client.unread_count(later.conversation_id).unwrap(), 1);
    assert_eq!(client.unread_count(other.conversation_id).unwrap(), 1);
}

#[test]
fn through_splits_batches_at_read_state_batch_size() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "reader", &vault), &vault);
    let mut target = None;
    for index in 0..=READ_STATE_BATCH_SIZE {
        target = Some(
            client
                .capture_incoming(incoming("message", &format!("message-{index}")))
                .unwrap()
                .message_id,
        );
    }
    let batches = client
        .unread_message_batches_through(target.unwrap())
        .unwrap();
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].len(), READ_STATE_BATCH_SIZE);
    assert_eq!(batches[1].len(), 1);
}

#[test]
fn snapshot_is_frozen_before_later_arrivals_and_batches_are_idempotent() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "reader", &vault), &vault);
    let first = client.capture_incoming(incoming("first", "first")).unwrap();
    let second = client
        .capture_incoming(IncomingSms {
            sender_address: "+15555550101".into(),
            ..incoming("second", "second")
        })
        .unwrap();
    let batches = client.unread_message_batches().unwrap();
    let later = client.capture_incoming(incoming("later", "later")).unwrap();
    let changed = batches
        .iter()
        .map(|batch| client.mark_read_batch(batch).unwrap())
        .sum::<u64>();

    assert_eq!(changed, 2);
    assert_eq!(client.unread_count(first.conversation_id).unwrap(), 1);
    assert_eq!(client.unread_count(second.conversation_id).unwrap(), 0);
    assert_eq!(client.unread_count(later.conversation_id).unwrap(), 1);
    assert_eq!(mark_snapshot(&client).unwrap(), 1);
    assert_eq!(mark_snapshot(&client).unwrap(), 0);
}

#[test]
fn batch_ignores_duplicates_already_read_and_absent_ids() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "reader", &vault), &vault);
    let message = client.capture_incoming(incoming("first", "first")).unwrap();
    let outbox_before = client.pending_outbox().unwrap().len();

    assert_eq!(client.mark_read_batch(&[]).unwrap(), 0);
    assert_eq!(
        client
            .mark_read_batch(&[message.message_id, message.message_id, MessageId::new()])
            .unwrap(),
        1
    );
    assert_eq!(client.mark_read_batch(&[message.message_id]).unwrap(), 0);
    assert_eq!(client.pending_outbox().unwrap().len(), outbox_before + 1);
}

#[test]
fn batch_rejects_oversize_input_before_mutation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "reader", &vault), &vault);
    let message = client.capture_incoming(incoming("first", "first")).unwrap();
    let batch = vec![message.message_id; READ_STATE_BATCH_SIZE + 1];

    assert_eq!(
        client.mark_read_batch(&batch),
        Err(Error::InvalidRequest(
            "read-state batch exceeds the maximum size"
        ))
    );
    assert_eq!(client.unread_count(message.conversation_id).unwrap(), 1);
}

#[test]
fn leaves_imported_and_outgoing_messages_seen() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let reader_config = config(&dir, "reader", &vault);
    let gateway_config = config(&dir, "gateway", &vault);
    let client = unlocked(&reader_config, &vault);
    let unread = client
        .capture_incoming(incoming("unread", "unread"))
        .unwrap();
    let imported = client
        .capture_incoming(IncomingSms {
            imported: true,
            ..incoming("history", "history")
        })
        .unwrap();
    let outgoing_conversation = ConversationId::new();
    client
        .queue_send(
            sms(outgoing_conversation, "outgoing"),
            route(&gateway_config),
        )
        .unwrap();

    assert_eq!(mark_snapshot(&client).unwrap(), 1);
    assert!(
        client
            .messages(imported.conversation_id)
            .unwrap()
            .into_iter()
            .find(|message| message.payload.record.message_id == imported.message_id)
            .unwrap()
            .seen
    );
    assert!(only_message(&client, outgoing_conversation).seen);
    assert_eq!(client.unread_count(unread.conversation_id).unwrap(), 0);
}

#[test]
fn queues_read_states_that_other_devices_apply_out_of_order() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "gateway", &vault), &vault);
    let reader = unlocked(&config(&dir, "reader", &vault), &vault);
    let peer = unlocked(&config(&dir, "peer", &vault), &vault);
    let message = gateway
        .capture_incoming(incoming("hello", "hello"))
        .unwrap();
    let mut server = Server::default();

    server.upload(&gateway);
    server.sync(&reader);
    assert_eq!(mark_snapshot(&reader).unwrap(), 1);
    server.upload(&reader);
    server.sync(&peer);

    assert!(only_message(&peer, message.conversation_id).seen);
    assert_eq!(peer.unread_count(message.conversation_id).unwrap(), 0);
}

#[test]
fn defers_read_state_sealing_until_keys_are_unlocked() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = open(&config(&dir, "locked-reader", &vault));
    client.capture_incoming(incoming("first", "first")).unwrap();
    client
        .capture_incoming(incoming("second", "second"))
        .unwrap();

    assert_eq!(mark_snapshot(&client).unwrap(), 2);
    assert!(client.pending_outbox().unwrap().is_empty());
    client
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    assert_eq!(client.pending_outbox().unwrap().len(), 4);
}

#[test]
fn rejected_later_batch_preserves_earlier_commit_and_explicit_retry_converges() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "reader", &vault), &vault);
    for index in 0..=READ_STATE_BATCH_SIZE {
        client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: "+15555550100".into(),
                body: "message".into(),
                provider_message_id: Some(format!("message-{index}")),
                imported: false,
            })
            .unwrap();
    }
    let batches = client.unread_message_batches().unwrap();
    assert_eq!(batches.len(), 2);
    assert_eq!(
        client.mark_read_batch(&batches[0]).unwrap(),
        READ_STATE_BATCH_SIZE as u64
    );
    assert_eq!(
        client.mark_read_batch(&vec![batches[1][0]; READ_STATE_BATCH_SIZE + 1]),
        Err(Error::InvalidRequest(
            "read-state batch exceeds the maximum size"
        ))
    );

    assert_eq!(mark_snapshot(&client).unwrap(), 1);
    assert_eq!(mark_snapshot(&client).unwrap(), 0);
}
