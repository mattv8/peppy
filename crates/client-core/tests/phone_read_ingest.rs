//! Phone-provider reads only accept exact, live per-message acquisition evidence.

use crate::common::*;
use std::fs;
use tempfile::TempDir;

fn sms_input(id: &str, date: i64) -> SmsAcquisitionInput {
    SmsAcquisitionInput {
        source: SmsSource {
            source_generation: "generation-1".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: id.into(),
            provider_thread_id: Some("thread-1".into()),
            broadcast_id: None,
        },
        direction: Direction::Incoming,
        sender_address: Some(ADDRESS.into()),
        recipients: vec![],
        body: format!("body-{id}"),
        imported: false,
        sent_at_ms: Some(date),
        date_sent_ms: Some(date),
        creator: None,
        source_row_fingerprint: format!("fingerprint-{id}"),
    }
}

fn enable(client: &Client) {
    let mut settings = client.gateway_settings().unwrap();
    settings.phone_read_enabled = true;
    client.set_gateway_settings(settings).unwrap();
}

#[test]
fn exact_sms_provider_read_enqueues_one_encrypted_event_without_phone_echo() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "gateway", &vault), &vault);
    enable(&gateway);
    let acquisition = gateway.begin_sms_acquisition(sms_input("one", 10)).unwrap();
    let captured = gateway
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();

    let candidates = gateway
        .phone_read_candidates(PhoneReadTransport::Sms, "generation-1", 0, 101)
        .unwrap();
    assert_eq!(candidates.len(), 1, "the core clamps provider pages to 100");
    assert_eq!(candidates[0].message_id, captured.message_id);
    let outbox_before = gateway.pending_outbox().unwrap().len();
    assert_eq!(gateway.apply_phone_reads(&candidates).unwrap(), 1);
    assert_eq!(
        gateway.apply_phone_reads(&candidates).unwrap(),
        0,
        "retries are idempotent"
    );
    assert!(gateway.unread_message_batches().unwrap().is_empty());
    assert!(
        gateway
            .pending_phone_read_effects(100, 100)
            .unwrap()
            .is_empty()
    );
    assert_eq!(gateway.pending_outbox().unwrap().len(), outbox_before + 1);
}

#[test]
fn apply_rejects_a_batch_larger_than_the_provider_page() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    enable(&client);
    let acquisition = client.begin_sms_acquisition(sms_input("one", 10)).unwrap();
    client
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    let candidate = client
        .phone_read_candidates(PhoneReadTransport::Sms, "generation-1", 0, 1)
        .unwrap()
        .remove(0);

    let oversized = vec![candidate; 101];
    assert!(matches!(
        client.apply_phone_reads(&oversized),
        Err(Error::InvalidRequest(_))
    ));
}

#[test]
fn provider_read_rejects_wrong_source_or_identity_without_collateral_read() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    enable(&client);
    let first = client.begin_sms_acquisition(sms_input("one", 10)).unwrap();
    let first = client
        .complete_sms_acquisition(&first.acquisition_id)
        .unwrap();
    let second = client.begin_sms_acquisition(sms_input("two", 20)).unwrap();
    let second = client
        .complete_sms_acquisition(&second.acquisition_id)
        .unwrap();
    let candidate = client
        .phone_read_candidates(PhoneReadTransport::Sms, "generation-1", 0, 100)
        .unwrap()
        .remove(0);

    for (field, invalid) in [
        (
            "fingerprint",
            PhoneReadCandidate {
                row: ProviderRowRef {
                    source_row_fingerprint: "reused-row".into(),
                    ..candidate.row.clone()
                },
                ..candidate.clone()
            },
        ),
        (
            "source generation",
            PhoneReadCandidate {
                row: ProviderRowRef {
                    source_generation: "wrong-generation".into(),
                    ..candidate.row.clone()
                },
                ..candidate.clone()
            },
        ),
        (
            "subscription",
            PhoneReadCandidate {
                row: ProviderRowRef {
                    subscription_id: "other-sim".into(),
                    ..candidate.row.clone()
                },
                ..candidate.clone()
            },
        ),
        (
            "provider id",
            PhoneReadCandidate {
                row: ProviderRowRef {
                    provider_message_id: "other-provider".into(),
                    ..candidate.row.clone()
                },
                ..candidate.clone()
            },
        ),
        (
            "thread",
            PhoneReadCandidate {
                row: ProviderRowRef {
                    provider_thread_id: "other-thread".into(),
                    ..candidate.row.clone()
                },
                ..candidate.clone()
            },
        ),
        (
            "date",
            PhoneReadCandidate {
                row: ProviderRowRef {
                    provider_date_ms: 11,
                    ..candidate.row.clone()
                },
                ..candidate.clone()
            },
        ),
        (
            "local order",
            PhoneReadCandidate {
                local_order: candidate.local_order + 1,
                ..candidate.clone()
            },
        ),
        (
            "message id",
            PhoneReadCandidate {
                message_id: second.message_id,
                ..candidate.clone()
            },
        ),
        (
            "transport",
            PhoneReadCandidate {
                transport: PhoneReadTransport::Mms,
                ..candidate.clone()
            },
        ),
    ] {
        assert_eq!(client.apply_phone_reads(&[invalid]).unwrap(), 0, "{field}");
    }
    assert_eq!(
        client.unread_message_batches().unwrap(),
        vec![vec![first.message_id, second.message_id]]
    );
    assert!(
        client
            .phone_read_candidates(PhoneReadTransport::Sms, "wrong-generation", 0, 100)
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        client.phone_read_candidates(PhoneReadTransport::Sms, "generation-1", -1, 1),
        Err(Error::InvalidRequest(_))
    ));
}

#[test]
fn candidates_exclude_nonlive_acquisitions_and_policy_changes_reject_saved_evidence() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    enable(&client);
    let live = client.begin_sms_acquisition(sms_input("live", 10)).unwrap();
    client
        .complete_sms_acquisition(&live.acquisition_id)
        .unwrap();

    let mut outgoing = sms_input("outgoing", 20);
    outgoing.direction = Direction::Outgoing;
    outgoing.sender_address = None;
    outgoing.recipients = vec![ADDRESS.into()];
    let outgoing = client.begin_sms_acquisition(outgoing).unwrap();
    client
        .complete_sms_acquisition(&outgoing.acquisition_id)
        .unwrap();
    client
        .begin_sms_acquisition(sms_input("pending", 30))
        .unwrap();
    let mut unavailable = sms_input("unavailable", 40);
    unavailable.body = "x".repeat(64 * 1024 + 1);
    assert_eq!(
        client.begin_sms_acquisition(unavailable).unwrap().state,
        SmsAcquisitionState::Unavailable
    );
    let imported = client
        .begin_sms_acquisition(SmsAcquisitionInput {
            imported: true,
            ..sms_input("imported", 50)
        })
        .unwrap();
    client
        .complete_sms_acquisition(&imported.acquisition_id)
        .unwrap();

    let candidate = client
        .phone_read_candidates(PhoneReadTransport::Sms, "generation-1", 0, 100)
        .unwrap();
    assert_eq!(candidate.len(), 1);
    let candidate = candidate[0].clone();
    let mut settings = client.gateway_settings().unwrap();
    settings.phone_read_enabled = false;
    client.set_gateway_settings(settings.clone()).unwrap();
    assert!(
        client
            .phone_read_candidates(PhoneReadTransport::Sms, "generation-1", 0, 100)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        client
            .apply_phone_reads(std::slice::from_ref(&candidate))
            .unwrap(),
        0
    );
    settings.phone_read_enabled = true;
    settings.sms_sync_enabled = false;
    client.set_gateway_settings(settings).unwrap();
    assert_eq!(client.apply_phone_reads(&[candidate]).unwrap(), 0);
}

#[test]
fn candidate_pages_are_clamped_and_keyset_progress_reaches_later_unread_rows() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    enable(&client);
    for number in 0..101 {
        let acquisition = client
            .begin_sms_acquisition(sms_input(&format!("page-{number}"), number + 1))
            .unwrap();
        client
            .complete_sms_acquisition(&acquisition.acquisition_id)
            .unwrap();
    }

    let first_page = client
        .phone_read_candidates(PhoneReadTransport::Sms, "generation-1", 0, u32::MAX)
        .unwrap();
    assert_eq!(first_page.len(), 100);
    let first_orders: Vec<_> = first_page
        .iter()
        .map(|candidate| candidate.local_order)
        .collect();
    assert!(first_orders.windows(2).all(|orders| orders[0] < orders[1]));
    let later_page = client
        .phone_read_candidates(
            PhoneReadTransport::Sms,
            "generation-1",
            first_page.last().unwrap().local_order,
            100,
        )
        .unwrap();
    assert_eq!(later_page.len(), 1);
    assert!(later_page[0].local_order > first_page.last().unwrap().local_order);
}

#[test]
fn restore_and_locked_clients_fail_closed_for_phone_read_ingestion() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&dir, "gateway", &vault);
    let client = unlocked(&gateway_config, &vault);
    enable(&client);
    let acquisition = client.begin_sms_acquisition(sms_input("one", 10)).unwrap();
    client
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    let candidate = client
        .phone_read_candidates(PhoneReadTransport::Sms, "generation-1", 0, 1)
        .unwrap();
    client.mark_restored_gateway().unwrap();
    assert!(
        client
            .phone_read_candidates(PhoneReadTransport::Sms, "generation-1", 0, 1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(client.apply_phone_reads(&candidate).unwrap(), 0);
    drop(client);

    let locked_config = crate::common::config(&dir, "locked-gateway", &vault);
    let unlocked_client = unlocked(&locked_config, &vault);
    enable(&unlocked_client);
    let acquisition = unlocked_client
        .begin_sms_acquisition(sms_input("locked", 20))
        .unwrap();
    unlocked_client
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    let locked_candidate = unlocked_client
        .phone_read_candidates(PhoneReadTransport::Sms, "generation-1", 0, 1)
        .unwrap();
    drop(unlocked_client);
    let locked = open(&locked_config);
    assert!(
        locked
            .phone_read_candidates(PhoneReadTransport::Sms, "generation-1", 0, 1)
            .unwrap()
            .is_empty()
    );
    assert_eq!(locked.apply_phone_reads(&locked_candidate).unwrap(), 0);
}

#[test]
fn mms_read_requires_mms_policy_and_uses_transaction_identity_without_fingerprint() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    enable(&client);
    let input = MmsAcquisitionInput {
        source: MmsSource {
            source_generation: "generation-1".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: "mms-one".into(),
            provider_thread_id: Some("thread-1".into()),
        },
        direction: Direction::Incoming,
        sender_address: Some(ADDRESS.into()),
        recipients: vec![],
        subject: None,
        body: "mms".into(),
        imported: false,
        observed_at_ms: 10,
        transaction_id: Some("pdu-one".into()),
    };
    let acquisition = client.begin_mms_acquisition(input).unwrap();
    let attachment_path = dir.path().join("mms.png");
    fs::write(&attachment_path, b"mms").unwrap();
    let attachment = client
        .prepare_attachment(&attachment_path, "image/png", "mms.png")
        .unwrap();
    client
        .set_mms_acquisition_part(
            &acquisition.acquisition_id,
            "part",
            attachment.attachment_id,
        )
        .unwrap();
    client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    let candidate = client
        .phone_read_candidates(PhoneReadTransport::Mms, "generation-1", 0, 100)
        .unwrap()
        .remove(0);
    assert!(candidate.row.source_row_fingerprint.is_empty());

    let mut settings = client.gateway_settings().unwrap();
    settings.mms_sync_enabled = false;
    client.set_gateway_settings(settings.clone()).unwrap();
    assert_eq!(
        client
            .apply_phone_reads(std::slice::from_ref(&candidate))
            .unwrap(),
        0
    );
    settings.mms_sync_enabled = true;
    client.set_gateway_settings(settings).unwrap();
    let mut wrong_transaction = candidate.clone();
    wrong_transaction.transaction_id = Some("other-pdu".into());
    assert_eq!(client.apply_phone_reads(&[wrong_transaction]).unwrap(), 0);
    assert_eq!(client.apply_phone_reads(&[candidate]).unwrap(), 1);
}

#[test]
fn encrypted_phone_read_has_no_gateway_echo_or_remote_collateral_effects() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "gateway", &vault), &vault);
    let desktop = unlocked(&config(&dir, "desktop", &vault), &vault);
    let other_gateway = unlocked(&config(&dir, "other-gateway", &vault), &vault);
    enable(&gateway);
    enable(&other_gateway);
    let first = gateway.begin_sms_acquisition(sms_input("one", 10)).unwrap();
    let first = gateway
        .complete_sms_acquisition(&first.acquisition_id)
        .unwrap();
    let second = gateway.begin_sms_acquisition(sms_input("two", 20)).unwrap();
    let second = gateway
        .complete_sms_acquisition(&second.acquisition_id)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    server.sync(&other_gateway);

    let candidate = gateway
        .phone_read_candidates(PhoneReadTransport::Sms, "generation-1", 0, 1)
        .unwrap();
    assert_eq!(gateway.apply_phone_reads(&candidate).unwrap(), 1);
    server.upload(&gateway);
    server.sync(&gateway);
    server.sync(&desktop);
    server.sync(&other_gateway);

    let unread = desktop.unread_message_batches().unwrap();
    assert_eq!(unread, vec![vec![second.message_id]]);
    assert_eq!(
        other_gateway.unread_message_batches().unwrap(),
        vec![vec![second.message_id]]
    );
    assert_ne!(first.message_id, second.message_id);
    assert!(
        gateway
            .pending_phone_read_effects(100, 100)
            .unwrap()
            .is_empty(),
        "the source gateway does not echo its own phone-originated read"
    );
    assert!(
        other_gateway
            .pending_phone_read_effects(100, 100)
            .unwrap()
            .is_empty(),
        "a second gateway has no local provider evidence to act on"
    );

    let read_event = server.log.last().unwrap().clone();
    assert!(matches!(
        other_gateway.ingest(&read_event, Cursor(server.log.len() as u64)),
        Ok(IngestResult::Duplicate)
    ));
    assert_eq!(
        other_gateway.unread_message_batches().unwrap(),
        vec![vec![second.message_id]],
        "a duplicate remote read leaves unrelated messages unread"
    );
}
