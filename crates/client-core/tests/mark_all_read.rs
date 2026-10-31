use crate::common::*;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const BATCH_CRITICAL_SECTION_BUDGET: Duration = Duration::from_secs(1);
const SCALE_CONVERSATION_COUNT: usize = 4;
const SCALE_IMPORTED_COUNT: usize = 8;

fn mark_snapshot(client: &Client) -> Result<u64, Error> {
    client
        .unread_message_batches()?
        .iter()
        .try_fold(0, |total, batch| Ok(total + client.mark_read_batch(batch)?))
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

#[test]
#[ignore = "explicit scale evidence: cargo test --release -p peppy-client-core --test integration mark_all_read:: -- --ignored --nocapture"]
fn batches_a_thousand_unread_messages_with_one_read_state_each() {
    measure_bulk_read(1_000);
}

#[test]
#[ignore = "explicit scale evidence: cargo test --release -p peppy-client-core --test integration mark_all_read:: -- --ignored --nocapture"]
fn batches_fifty_thousand_unread_messages_with_bounded_critical_sections() {
    measure_bulk_read(50_000);
}

fn measure_bulk_read(message_count: usize) {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "scale", &vault), &vault);
    let mut server = Server::default();
    for index in 0..message_count {
        client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: format!("+1555555{:05}", index % SCALE_CONVERSATION_COUNT),
                body: "scale".into(),
                provider_message_id: Some(format!("scale-{index}")),
                imported: false,
            })
            .unwrap();
    }
    for index in 0..SCALE_IMPORTED_COUNT {
        client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: format!("+1555555{:05}", index % SCALE_CONVERSATION_COUNT),
                body: "imported scale history".into(),
                provider_message_id: Some(format!("scale-imported-{index}")),
                imported: true,
            })
            .unwrap();
    }
    assert_eq!(server.upload(&client), message_count + SCALE_IMPORTED_COUNT);

    let snapshot_started = Instant::now();
    let batches = client.unread_message_batches().unwrap();
    let snapshot = snapshot_started.elapsed();
    let operation_started = Instant::now();
    let mut changed = 0;
    let mut batch_latencies = Vec::with_capacity(batches.len());
    for batch in &batches {
        let batch_started = Instant::now();
        changed += client.mark_read_batch(batch).unwrap();
        batch_latencies.push(batch_started.elapsed());
    }
    let total = operation_started.elapsed();
    while client.seal_pending_batch(MAX_SEAL_BATCH).unwrap() > 0 {}
    let read_state_events = server.upload(&client);

    assert_eq!(changed, message_count as u64);
    assert_eq!(read_state_events, message_count);
    let first_batch = batch_latencies[0];
    let last_batch = *batch_latencies.last().unwrap();
    let mut sorted_batch_latencies = batch_latencies.clone();
    sorted_batch_latencies.sort_unstable();
    let median_batch = sorted_batch_latencies[sorted_batch_latencies.len() / 2];
    let p95_batch = sorted_batch_latencies[(sorted_batch_latencies.len() * 95).div_ceil(100) - 1];
    let max_batch = *sorted_batch_latencies.last().unwrap();
    println!(
        "mark_all_read unread={message_count} imported={SCALE_IMPORTED_COUNT} conversations={SCALE_CONVERSATION_COUNT} snapshot={snapshot:?} first_batch={first_batch:?} median_batch={median_batch:?} p95_batch={p95_batch:?} last_batch={last_batch:?} max_batch={max_batch:?} total={total:?} read_state_events={read_state_events}"
    );
    assert!(
        snapshot <= BATCH_CRITICAL_SECTION_BUDGET,
        "snapshot took {snapshot:?}"
    );
    assert!(
        batch_latencies
            .iter()
            .all(|latency| *latency <= BATCH_CRITICAL_SECTION_BUDGET),
        "a batch exceeded the critical-section budget: {batch_latencies:?}"
    );
}
