//! Provider-ID reuse only creates a second immutable message with strong source evidence.

use crate::common::*;
use std::fs;
use tempfile::TempDir;

fn input(transaction_id: Option<&str>) -> MmsAcquisitionInput {
    MmsAcquisitionInput {
        source: MmsSource {
            source_generation: "phone-install".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: "42".into(),
            provider_thread_id: Some("7".into()),
        },
        direction: Direction::Incoming,
        sender_address: Some(ADDRESS.into()),
        recipients: vec!["+15555550102".into(), "+15555550101".into()],
        subject: Some("mutable subject".into()),
        body: "message body".into(),
        imported: false,
        observed_at_ms: 1,
        transaction_id: transaction_id.map(str::to_owned),
    }
}

#[test]
fn strong_provider_id_reuse_preserves_the_old_message_and_imports_once_per_instance() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);

    let first = client
        .begin_mms_acquisition(input(Some("first-pdu")))
        .unwrap();
    let first_message = client
        .complete_mms_acquisition(&first.acquisition_id)
        .unwrap();
    let mut reused = input(Some("second-pdu"));
    reused.body = "new immutable source instance".into();
    let second = client.begin_mms_acquisition(reused).unwrap();
    assert_ne!(second.acquisition_id, first.acquisition_id);
    let second_message = client
        .complete_mms_acquisition(&second.acquisition_id)
        .unwrap();
    assert_ne!(second_message.message_id, first_message.message_id);
    assert_eq!(client.messages(first.conversation_id).unwrap().len(), 2);

    assert_eq!(
        client
            .begin_mms_acquisition(input(Some("second-pdu")))
            .unwrap()
            .acquisition_id,
        second.acquisition_id
    );
}

#[test]
fn mutable_only_reuse_is_typed_quarantine_and_does_not_create_actionable_work() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let first = client.begin_mms_acquisition(input(None)).unwrap();
    client
        .complete_mms_acquisition(&first.acquisition_id)
        .unwrap();

    let mut ambiguous = input(Some("new-pdu"));
    ambiguous.observed_at_ms = 99;
    ambiguous.subject = Some("edited subject".into());
    ambiguous.recipients.reverse();
    let outcome = client.begin_mms_acquisition_outcome(ambiguous).unwrap();
    assert!(format!("{outcome:?}").starts_with("Quarantined"));
    assert_eq!(client.mms_acquisitions(10).unwrap().len(), 0);
    assert_eq!(
        client
            .mms_backfill_status("phone-install", "sim-1")
            .unwrap()
            .pending_count,
        0
    );
}

#[test]
fn pending_provider_identity_accepts_late_transaction_enrichment() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let initial = client.begin_mms_acquisition(input(None)).unwrap();

    let outcome = client
        .begin_mms_acquisition_outcome(input(Some("late-pdu")))
        .unwrap();
    let MmsBeginOutcome::Acquired(enriched) = outcome else {
        panic!("pending identity enrichment must remain actionable");
    };

    assert_eq!(enriched.acquisition_id, initial.acquisition_id);
    assert_eq!(enriched.input.transaction_id.as_deref(), Some("late-pdu"));
}

#[test]
fn pending_provider_identity_keeps_distinct_non_null_transactions_separate() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway", &vault), &vault);
    let first = client
        .begin_mms_acquisition(input(Some("first-pdu")))
        .unwrap();

    let outcome = client
        .begin_mms_acquisition_outcome(input(Some("second-pdu")))
        .unwrap();
    let MmsBeginOutcome::Acquired(second) = outcome else {
        panic!("distinct strong provider identity must remain actionable");
    };

    assert_ne!(second.acquisition_id, first.acquisition_id);
}

#[test]
fn strong_reuse_retires_stale_pending_instance_and_exposes_quarantine_count() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "gateway-stale", &vault);
    let client = unlocked(&cfg, &vault);
    client
        .plan_mms_backfill_scans("phone-install", "sim-1", "42")
        .unwrap();
    let stale = client
        .begin_mms_acquisition(input(Some("first-pdu")))
        .unwrap();
    let media_path = dir.path().join("stale.png");
    fs::write(&media_path, b"stale media").unwrap();
    let media = client
        .prepare_attachment(&media_path, "image/png", "stale.png")
        .unwrap()
        .attachment_id;
    client
        .set_mms_acquisition_part(&stale.acquisition_id, "stale-part", media)
        .unwrap();

    let MmsBeginOutcome::Acquired(current) = client
        .begin_mms_acquisition_outcome(input(Some("second-pdu")))
        .unwrap()
    else {
        panic!("strong new instance must remain actionable");
    };

    assert_ne!(current.acquisition_id, stale.acquisition_id);
    assert_eq!(client.mms_pending_media_bytes().unwrap(), 0);
    assert_eq!(client.attachment_info(media), Err(Error::NotFound));
    let visible = client.mms_acquisitions(10).unwrap();
    assert!(visible.iter().any(|row| {
        row.acquisition_id == stale.acquisition_id
            && row.state == MmsAcquisitionState::Unavailable
            && row.reason.as_deref() == Some("provider identity superseded")
    }));
    let status = client
        .mms_backfill_status("phone-install", "sim-1")
        .unwrap();
    assert_eq!(status.pending_count, 1);
    assert_eq!(status.quarantined_count, 1);
    drop(client);
    assert_eq!(
        unlocked(&cfg, &vault)
            .mms_backfill_status("phone-install", "sim-1")
            .unwrap()
            .quarantined_count,
        1
    );
}

#[test]
fn missing_transaction_ambiguity_is_typed_and_retires_pending_rows() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "gateway-ambiguous", &vault), &vault);
    client
        .plan_mms_backfill_scans("phone-install", "sim-1", "42")
        .unwrap();
    client
        .begin_mms_acquisition(input(Some("first-pdu")))
        .unwrap();
    client
        .begin_mms_acquisition(input(Some("second-pdu")))
        .unwrap();

    let outcome = client.begin_mms_acquisition_outcome(input(None)).unwrap();

    assert!(matches!(outcome, MmsBeginOutcome::Quarantined(_)));
    let status = client
        .mms_backfill_status("phone-install", "sim-1")
        .unwrap();
    assert_eq!(status.pending_count, 0);
    assert_eq!(status.quarantined_count, 1);
    let terminal = client.mms_acquisitions(10).unwrap();
    assert!(
        terminal
            .iter()
            .all(|row| row.state == MmsAcquisitionState::Unavailable)
    );
    assert!(
        terminal
            .iter()
            .any(|row| { row.reason.as_deref() == Some("provider identity superseded") })
    );
    assert!(
        terminal
            .iter()
            .any(|row| { row.reason.as_deref() == Some("provider identity is ambiguous") })
    );
    assert!(
        client
            .finish_mms_backfill_phase(
                "phone-install",
                "sim-1",
                MmsBackfillPhase::History,
                "42",
                true,
            )
            .unwrap()
    );
}

#[test]
fn unavailable_strong_reuse_does_not_add_superseded_quarantine() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "terminal-strong", &vault), &vault);
    client
        .plan_mms_backfill_scans("phone-install", "sim-1", "42")
        .unwrap();
    let unavailable = client
        .begin_mms_acquisition(input(Some("first-pdu")))
        .unwrap();
    client
        .set_mms_acquisition_state(
            &unavailable.acquisition_id,
            MmsAcquisitionState::Unavailable,
            Some("provider missing"),
        )
        .unwrap();

    let MmsBeginOutcome::Acquired(current) = client
        .begin_mms_acquisition_outcome(input(Some("second-pdu")))
        .unwrap()
    else {
        panic!("strong reuse after a terminal row must remain actionable");
    };

    assert_ne!(current.acquisition_id, unavailable.acquisition_id);
    assert_eq!(
        client
            .mms_backfill_status("phone-install", "sim-1")
            .unwrap()
            .quarantined_count,
        0
    );
}

#[test]
fn changed_transactionless_terminal_input_is_quarantined_without_provenance_loss() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "terminal-ambiguous", &vault), &vault);
    client
        .plan_mms_backfill_scans("phone-install", "sim-1", "42")
        .unwrap();
    let original_input = input(None);
    let terminal = client.begin_mms_acquisition(original_input).unwrap();
    let original_input = terminal.input.clone();
    client
        .set_mms_acquisition_state(
            &terminal.acquisition_id,
            MmsAcquisitionState::Unavailable,
            Some("provider missing"),
        )
        .unwrap();

    let mut unchanged_scan = original_input.clone();
    unchanged_scan.imported = true;
    let MmsBeginOutcome::Acquired(unchanged) = client
        .begin_mms_acquisition_outcome(unchanged_scan)
        .unwrap()
    else {
        panic!("scan provenance alone must remain idempotent");
    };
    assert_eq!(unchanged.acquisition_id, terminal.acquisition_id);
    assert_eq!(unchanged.input, original_input);

    let mut changed = original_input.clone();
    changed.observed_at_ms = 99;
    changed.subject = Some("changed caption".into());
    changed.body = "changed provider body".into();
    let outcome = client.begin_mms_acquisition_outcome(changed).unwrap();

    assert!(matches!(outcome, MmsBeginOutcome::Quarantined(_)));
    let stored = client
        .mms_acquisitions(10)
        .unwrap()
        .into_iter()
        .find(|row| row.acquisition_id == terminal.acquisition_id)
        .unwrap();
    assert_eq!(stored.input, original_input);
    assert_eq!(stored.state, MmsAcquisitionState::Unavailable);
    let status = client
        .mms_backfill_status("phone-install", "sim-1")
        .unwrap();
    assert_eq!(status.pending_count, 0);
    assert_eq!(status.quarantined_count, 1);
}
