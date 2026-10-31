//! Durable phone-read permits are issued only for live, locally acquired SMS rows.

use crate::common::*;
use tempfile::TempDir;

fn input(id: &str, thread: &str, date: i64, imported: bool) -> SmsAcquisitionInput {
    SmsAcquisitionInput {
        source: SmsSource {
            source_generation: "generation-1".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: id.into(),
            provider_thread_id: Some(thread.into()),
            broadcast_id: None,
        },
        direction: Direction::Incoming,
        sender_address: Some(ADDRESS.into()),
        recipients: vec![],
        body: format!("body-{id}"),
        imported,
        sent_at_ms: Some(date),
        date_sent_ms: Some(date),
        creator: None,
        source_row_fingerprint: format!("fingerprint-{id}"),
    }
}

fn acquire(
    client: &Client,
    id: &str,
    thread: &str,
    date: i64,
    imported: bool,
) -> (MessageId, ProviderRowRef) {
    let acquisition = client
        .begin_sms_acquisition(input(id, thread, date, imported))
        .unwrap();
    let captured = client
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    (
        captured.message_id,
        ProviderRowRef {
            source_generation: "generation-1".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: id.into(),
            provider_thread_id: thread.into(),
            provider_date_ms: date,
            source_row_fingerprint: format!("fingerprint-{id}"),
        },
    )
}

fn enable(client: &Client) {
    let mut settings = client.gateway_settings().unwrap();
    settings.phone_read_enabled = true;
    client.set_gateway_settings(settings).unwrap();
}

#[test]
fn disabled_reads_do_not_backfill_when_enabled_later() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let (message_id, _) = acquire(&client, "one", "thread", 10, false);

    assert!(client.mark_seen(message_id).unwrap());
    enable(&client);
    assert!(
        client
            .pending_phone_read_effects(100, 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn consume_uses_requested_effects_and_all_unread_rows_without_order_dependence() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&dir, "gateway", &vault);
    let client = unlocked(&gateway_config, &vault);
    enable(&client);
    let (first, first_row) = acquire(&client, "one", "thread", 10, false);
    let (second, second_row) = acquire(&client, "two", "thread", 20, false);
    let (already_read, third_row) = acquire(&client, "three", "thread", 30, false);
    for id in [first, second, already_read] {
        assert!(client.mark_seen(id).unwrap());
    }
    client.complete_phone_read_effects(&[already_read]).unwrap();

    assert_eq!(
        client
            .consume_phone_read_effects(
                &[first, second],
                &[third_row, second_row.clone(), first_row.clone()],
                100,
            )
            .unwrap(),
        2
    );
    assert!(
        client
            .pending_phone_read_effects(100, 100)
            .unwrap()
            .is_empty()
    );
    drop(client);

    let reopened = unlocked(&gateway_config, &vault);
    assert_eq!(
        reopened
            .consume_phone_read_effects(&[first, second], &[first_row, second_row], 100)
            .unwrap(),
        0,
        "terminal identities survive a crash/reopen retry with matching provider rows"
    );
}

#[test]
fn consume_rejects_duplicates_stale_rows_and_unrelated_threads_without_partial_retirement() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    enable(&client);
    let (first, first_row) = acquire(&client, "one", "thread-a", 10, false);
    let (second, second_row) = acquire(&client, "two", "thread-a", 20, false);
    let (_, other_thread) = acquire(&client, "other", "thread-b", 30, false);
    for id in [first, second] {
        assert!(client.mark_seen(id).unwrap());
    }

    assert_eq!(
        client
            .consume_phone_read_effects(&[first, first], std::slice::from_ref(&first_row), 100)
            .unwrap(),
        0
    );
    let mut stale = first_row.clone();
    stale.source_row_fingerprint = "wrong".into();
    assert_eq!(
        client
            .consume_phone_read_effects(&[first, second], &[stale], 100)
            .unwrap(),
        0
    );
    assert_eq!(
        client
            .consume_phone_read_effects(
                &[first, second],
                &[first_row, second_row, other_thread],
                100
            )
            .unwrap(),
        0
    );
    assert_eq!(
        client.pending_phone_read_effects(100, 100).unwrap().len(),
        2
    );
}

#[test]
fn consume_rejects_empty_rows_expired_effects_and_disabled_sms_sync() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    enable(&client);
    let (message_id, row) = acquire(&client, "one", "thread", 10, false);
    assert!(client.mark_seen(message_id).unwrap());

    assert_eq!(
        client
            .consume_phone_read_effects(&[message_id], &[], 100)
            .unwrap(),
        0
    );
    assert_eq!(
        client.pending_phone_read_effects(100, 100).unwrap().len(),
        1
    );

    let mut settings = client.gateway_settings().unwrap();
    settings.sms_sync_enabled = false;
    client.set_gateway_settings(settings.clone()).unwrap();
    assert_eq!(
        client
            .consume_phone_read_effects(&[message_id], std::slice::from_ref(&row), 100)
            .unwrap(),
        0
    );

    settings.sms_sync_enabled = true;
    client.set_gateway_settings(settings).unwrap();
    assert_eq!(
        client
            .consume_phone_read_effects(&[message_id], &[row], i64::MAX)
            .unwrap(),
        0
    );
    assert!(
        client
            .pending_phone_read_effects(i64::MAX, 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn provider_rows_seen_requires_nonempty_matching_rows() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let (first_id, first) = acquire(&client, "one", "thread", 10, false);
    let (second_id, second) = acquire(&client, "two", "thread", 20, false);
    assert!(client.mark_seen(first_id).unwrap());
    assert!(client.mark_seen(second_id).unwrap());

    assert!(!client.sms_provider_rows_seen(&[]).unwrap());
    assert!(
        client
            .sms_provider_rows_seen(&[first.clone(), second])
            .unwrap()
    );
    let mut mismatch = first;
    mismatch.source_row_fingerprint = "wrong".into();
    assert!(!client.sms_provider_rows_seen(&[mismatch]).unwrap());
}

#[test]
fn consume_rejects_each_stale_provider_provenance_field() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    enable(&client);
    let (message_id, row) = acquire(&client, "one", "thread", 10, false);
    assert!(client.mark_seen(message_id).unwrap());

    for (field, stale) in [
        (
            "source generation",
            ProviderRowRef {
                source_generation: "generation-2".into(),
                ..row.clone()
            },
        ),
        (
            "subscription",
            ProviderRowRef {
                subscription_id: "sim-2".into(),
                ..row.clone()
            },
        ),
        (
            "fingerprint",
            ProviderRowRef {
                source_row_fingerprint: "fingerprint-other".into(),
                ..row.clone()
            },
        ),
        (
            "date",
            ProviderRowRef {
                provider_date_ms: 11,
                ..row.clone()
            },
        ),
    ] {
        assert_eq!(
            client
                .consume_phone_read_effects(&[message_id], &[stale], 100)
                .unwrap(),
            0,
            "{field} mismatch must not authorize the effect"
        );
        assert_eq!(
            client.pending_phone_read_effects(100, 100).unwrap().len(),
            1
        );
    }
}

#[test]
fn disable_expiry_and_snapshot_retire_pending_effects() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    enable(&client);
    let (first, _) = acquire(&client, "one", "thread", 10, false);
    assert!(client.mark_seen(first).unwrap());
    assert_eq!(
        client.pending_phone_read_effects(100, 100).unwrap().len(),
        1
    );

    let mut settings = client.gateway_settings().unwrap();
    settings.phone_read_enabled = false;
    client.set_gateway_settings(settings).unwrap();
    enable(&client);
    assert!(
        client
            .pending_phone_read_effects(100, 100)
            .unwrap()
            .is_empty()
    );

    let (second, _) = acquire(&client, "two", "thread", 20, false);
    assert!(client.mark_seen(second).unwrap());
    assert!(
        client
            .pending_phone_read_effects(i64::MAX, 100)
            .unwrap()
            .is_empty()
    );

    let (third, _) = acquire(&client, "three", "thread", 30, false);
    assert!(client.mark_seen(third).unwrap());
    client
        .begin_snapshot(Cursor(0), 0, SnapshotPurpose::Resync)
        .unwrap();
    assert!(
        client
            .pending_phone_read_effects(100, 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn local_batch_and_live_remote_reads_enqueue_but_imported_and_outgoing_rows_do_not() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&dir, "gateway", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&config(&dir, "desktop", &vault), &vault);
    enable(&gateway);
    let (local_one, _) = acquire(&gateway, "local-one", "thread", 10, false);
    let (local_two, _) = acquire(&gateway, "local-two", "thread", 20, false);
    assert_eq!(gateway.mark_read_batch(&[local_one, local_two]).unwrap(), 2);
    assert_eq!(
        gateway.pending_phone_read_effects(100, 100).unwrap().len(),
        2
    );

    let (imported, _) = acquire(&gateway, "imported", "thread", 30, true);
    assert!(
        !gateway.mark_seen(imported).unwrap(),
        "historical imports are already seen"
    );
    assert_eq!(
        gateway.pending_phone_read_effects(100, 100).unwrap().len(),
        2
    );

    let mut outgoing = input("outgoing", "thread", 40, false);
    outgoing.direction = Direction::Outgoing;
    outgoing.sender_address = None;
    outgoing.recipients = vec![ADDRESS.into()];
    let outgoing = gateway.begin_sms_acquisition(outgoing).unwrap();
    gateway
        .complete_sms_acquisition(&outgoing.acquisition_id)
        .unwrap();
    assert_eq!(
        gateway.pending_phone_read_effects(100, 100).unwrap().len(),
        2
    );

    let (remote, _) = acquire(&gateway, "remote", "remote-thread", 50, false);
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    assert!(desktop.mark_seen(remote).unwrap());
    server.upload(&desktop);
    server.sync(&gateway);
    assert!(
        gateway
            .pending_phone_read_effects(100, 100)
            .unwrap()
            .iter()
            .any(|effect| effect.message_id == remote)
    );
}

#[test]
fn restore_guard_retires_existing_effects_and_blocks_future_enqueue() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    enable(&client);
    let (first, _) = acquire(&client, "one", "thread", 10, false);
    assert!(client.mark_seen(first).unwrap());
    client
        .begin_snapshot(Cursor(0), 0, SnapshotPurpose::Restore)
        .unwrap();
    assert!(
        client
            .pending_phone_read_effects(100, 100)
            .unwrap()
            .is_empty()
    );

    let (later, _) = acquire(&client, "later", "thread", 20, false);
    assert!(client.mark_seen(later).unwrap());
    assert!(
        client
            .pending_phone_read_effects(100, 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn direct_restore_retires_effects_before_reopen() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&dir, "gateway", &vault);
    let client = unlocked(&gateway_config, &vault);
    enable(&client);
    let (message_id, row) = acquire(&client, "one", "thread", 10, false);
    assert!(client.mark_seen(message_id).unwrap());
    client.mark_restored_gateway().unwrap();
    drop(client);

    let reopened = unlocked(&gateway_config, &vault);
    assert!(reopened.restore_guarded().unwrap());
    assert_eq!(
        reopened
            .consume_phone_read_effects(&[message_id], &[row], 100)
            .unwrap(),
        0
    );
    drop(reopened);

    let conn = rusqlite::Connection::open(&gateway_config.database_path).unwrap();
    conn.execute_batch(
        "PRAGMA key = \"x'0707070707070707070707070707070707070707070707070707070707070707'\";",
    )
    .unwrap();
    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM phone_read_effects", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(remaining, 0);
}

#[test]
fn epoch_activation_retires_pending_effects() {
    let dir = TempDir::new().unwrap();
    let epoch1 = Vault::new();
    let epoch2 = Vault::epoch(epoch1.id, 2);
    let client = unlocked(&config(&dir, "gateway", &epoch1), &epoch1);
    enable(&client);
    let (message_id, _) = acquire(&client, "one", "thread", 10, false);
    assert!(client.mark_seen(message_id).unwrap());
    assert_eq!(
        client.pending_phone_read_effects(100, 100).unwrap().len(),
        1
    );

    client
        .unlock(&epoch2.profile, &epoch2.header, PASSPHRASE)
        .unwrap();
    client.activate_epoch(2).unwrap();
    assert!(
        client
            .pending_phone_read_effects(100, 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn late_old_epoch_read_state_marks_seen_without_enqueueing_an_effect() {
    let dir = TempDir::new().unwrap();
    let epoch1 = Vault::new();
    let epoch2 = Vault::epoch(epoch1.id, 2);
    let gateway = unlocked(&config(&dir, "gateway", &epoch1), &epoch1);
    let desktop = unlocked(&config(&dir, "desktop", &epoch1), &epoch1);
    enable(&gateway);
    let (message_id, _) = acquire(&gateway, "one", "thread", 10, false);
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    assert!(desktop.mark_seen(message_id).unwrap());
    server.upload(&desktop);

    gateway
        .unlock(&epoch2.profile, &epoch2.header, PASSPHRASE)
        .unwrap();
    gateway.activate_epoch(2).unwrap();
    server.sync(&gateway);

    assert!(gateway.unread_message_batches().unwrap().is_empty());
    assert!(
        gateway
            .pending_phone_read_effects(100, 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn historical_snapshot_read_state_does_not_enqueue_an_effect() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "gateway", &vault), &vault);
    let desktop = unlocked(&config(&dir, "desktop", &vault), &vault);
    enable(&gateway);
    let (message_id, _) = acquire(&gateway, "one", "thread", 10, false);
    assert_eq!(
        gateway.unread_message_batches().unwrap().len(),
        1,
        "precondition: the gateway row is unread before the snapshot"
    );
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    assert!(desktop.mark_seen(message_id).unwrap());
    server.upload(&desktop);

    let records: Vec<_> = server
        .log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect();
    let snapshot = gateway
        .begin_snapshot(
            Cursor(records.len() as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
        )
        .unwrap();
    gateway
        .append_snapshot_page(snapshot.generation, &records)
        .unwrap();
    gateway.finish_snapshot(snapshot.generation).unwrap();
    gateway.apply_pending(100).unwrap();

    assert!(
        gateway.unread_message_batches().unwrap().is_empty(),
        "historical snapshot must apply the desktop read state before the effect check"
    );
    assert!(
        gateway
            .pending_phone_read_effects(100, 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn read_before_acquisition_completion_does_not_enqueue_after_completion() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&dir, "gateway", &vault);
    let client = unlocked(&gateway_config, &vault);
    enable(&client);
    let acquisition = client
        .begin_sms_acquisition(input("one", "thread", 10, false))
        .unwrap();
    let mut peer_config = config(&dir, "peer", &vault);
    peer_config.device_id = gateway_config.device_id;
    let peer = unlocked(&peer_config, &vault);
    let (message_id, _) = acquire(&peer, "one", "thread", 10, false);
    let mut server = Server::default();
    server.upload(&peer);
    server.sync(&client);
    assert!(peer.mark_seen(message_id).unwrap());
    server.upload(&peer);
    server.sync(&client);
    client
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    assert!(
        client
            .pending_phone_read_effects(100, 100)
            .unwrap()
            .is_empty()
    );
}
