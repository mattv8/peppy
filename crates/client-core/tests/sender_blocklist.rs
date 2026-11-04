//! Phone blocklists classify blocked conversations as spam, and a confirmed unblock is a
//! one-use, phone-executed request.
use crate::common::*;
use crate::provider_archive::{folder_of, provider_sms};
use peppy_client_core::{
    Conversation, ConversationFolder, ConversationFolderReason, SenderUnblockOutcome,
    SenderUnblockStatus,
};
use tempfile::TempDir;

const OTHER: &str = "+15555550199";
/// A valid number, so national formatting canonicalizes to the same E.164 form.
const PEER: &str = "+14155552671";

fn row(client: &Client, conversation: ConversationId) -> Conversation {
    client
        .list_conversations()
        .unwrap()
        .into_iter()
        .find(|row| row.conversation_id == conversation)
        .unwrap()
}

fn numbers(values: &[&str]) -> Option<Vec<String>> {
    Some(values.iter().map(|value| (*value).to_owned()).collect())
}

#[test]
fn blocking_a_sender_moves_its_conversation_to_spam_with_a_blocked_reason() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "block-gateway", &vault), &vault);
    let blocked = provider_sms(&gateway, "b-1", PEER, "t1");
    let other = provider_sms(&gateway, "b-2", OTHER, "t2");

    assert!(
        gateway
            .publish_sender_blocklist(numbers(&["(415) 555-2671"]), true)
            .unwrap()
    );
    let state = row(&gateway, blocked);
    assert_eq!(state.folder, ConversationFolder::Spam);
    assert_eq!(state.folder_reason, Some(ConversationFolderReason::Blocked));
    assert!(state.can_unblock);
    assert_eq!(folder_of(&gateway, other), ConversationFolder::Inbox);

    let outbox = gateway.pending_outbox().unwrap().len();
    assert!(
        !gateway
            .publish_sender_blocklist(numbers(&[PEER]), true)
            .unwrap()
    );
    assert_eq!(gateway.pending_outbox().unwrap().len(), outbox);
}

#[test]
fn unblocking_on_the_phone_reverts_only_gateway_written_spam() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "unblock-revert", &vault), &vault);
    let blocked = provider_sms(&gateway, "r-1", ADDRESS, "t1");
    let marked = provider_sms(&gateway, "r-2", OTHER, "t2");
    gateway
        .set_conversation_folder(marked, ConversationFolder::Spam)
        .unwrap();

    gateway
        .publish_sender_blocklist(numbers(&[ADDRESS, OTHER]), true)
        .unwrap();
    assert_eq!(
        row(&gateway, marked).folder_reason,
        Some(ConversationFolderReason::Blocked)
    );
    gateway
        .publish_sender_blocklist(numbers(&[]), true)
        .unwrap();
    assert_eq!(folder_of(&gateway, blocked), ConversationFolder::Inbox);
    let marked_row = row(&gateway, marked);
    assert_eq!(marked_row.folder, ConversationFolder::Spam);
    assert_eq!(
        marked_row.folder_reason,
        Some(ConversationFolderReason::Marked)
    );
}

#[test]
fn blocklist_spam_wins_over_a_provider_archive() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "block-archive", &vault), &vault);
    let conversation = provider_sms(&gateway, "ba-1", ADDRESS, "t1");
    gateway
        .reconcile_provider_archive(vec!["t1".into()])
        .unwrap();
    gateway
        .publish_sender_blocklist(numbers(&[ADDRESS]), true)
        .unwrap();
    assert_eq!(folder_of(&gateway, conversation), ConversationFolder::Spam);
    gateway.reconcile_provider_archive(vec![]).unwrap();
    assert_eq!(folder_of(&gateway, conversation), ConversationFolder::Spam);
}

#[test]
fn lost_access_keeps_the_list_but_withdraws_unblocking() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "block-lost", &vault), &vault);
    let conversation = provider_sms(&gateway, "l-1", ADDRESS, "t1");
    gateway
        .publish_sender_blocklist(numbers(&[ADDRESS]), true)
        .unwrap();
    assert!(gateway.publish_sender_blocklist(None, false).unwrap());
    let state = row(&gateway, conversation);
    assert_eq!(state.folder_reason, Some(ConversationFolderReason::Blocked));
    assert!(!state.can_unblock);
    assert_eq!(
        gateway.request_sender_unblock(conversation),
        Err(Error::InvalidRequest("sender unblock"))
    );
}

#[test]
fn marked_spam_cannot_request_an_unblock() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "unblock-marked", &vault), &vault);
    let conversation = provider_sms(&client, "m-1", ADDRESS, "t1");
    client
        .set_conversation_folder(conversation, ConversationFolder::Spam)
        .unwrap();
    let outbox = client.pending_outbox().unwrap().len();
    assert_eq!(
        client.request_sender_unblock(conversation),
        Err(Error::InvalidRequest("sender unblock"))
    );
    assert_eq!(client.pending_outbox().unwrap().len(), outbox);
}

#[test]
fn desktop_unblock_runs_once_on_the_listing_phone_and_reports_back() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "unblock-gateway", &vault), &vault);
    let desktop = unlocked(&config(&dir, "unblock-desktop", &vault), &vault);
    let conversation = provider_sms(&gateway, "d-1", ADDRESS, "t1");
    gateway
        .publish_sender_blocklist(numbers(&[ADDRESS]), true)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    let state = row(&desktop, conversation);
    assert_eq!(state.folder_reason, Some(ConversationFolderReason::Blocked));
    assert!(state.can_unblock);

    desktop.request_sender_unblock(conversation).unwrap();
    let state = row(&desktop, conversation);
    assert_eq!(state.folder, ConversationFolder::Inbox);
    assert_eq!(state.unblock, Some(SenderUnblockStatus::Pending));
    assert!(
        desktop.pending_sender_unblocks().unwrap().is_empty(),
        "only the target phone acts"
    );

    server.upload(&desktop);
    server.sync(&gateway);
    let work = gateway.pending_sender_unblocks().unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].numbers, vec![ADDRESS.to_owned()]);
    assert!(gateway.begin_sender_unblock(&work[0].request_id).unwrap());
    assert!(
        !gateway.begin_sender_unblock(&work[0].request_id).unwrap(),
        "the permit is one-use"
    );
    assert!(gateway.pending_sender_unblocks().unwrap().is_empty());
    assert_eq!(gateway.interrupted_sender_unblocks().unwrap(), work);
    gateway
        .complete_sender_unblock(&work[0].request_id, SenderUnblockOutcome::Unblocked)
        .unwrap();
    assert!(gateway.interrupted_sender_unblocks().unwrap().is_empty());
    assert!(
        gateway
            .complete_sender_unblock(&work[0].request_id, SenderUnblockOutcome::Failed)
            .is_err()
    );
    gateway
        .publish_sender_blocklist(numbers(&[]), true)
        .unwrap();

    server.upload(&gateway);
    server.sync(&desktop);
    let state = row(&desktop, conversation);
    assert_eq!(state.folder, ConversationFolder::Inbox);
    assert_eq!(state.unblock, None);
}

#[test]
fn a_failed_unblock_is_reported_to_every_device() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "unblock-fail-gateway", &vault), &vault);
    let desktop = unlocked(&config(&dir, "unblock-fail-desktop", &vault), &vault);
    let conversation = provider_sms(&gateway, "f-1", ADDRESS, "t1");
    gateway
        .publish_sender_blocklist(numbers(&[ADDRESS]), true)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    desktop.request_sender_unblock(conversation).unwrap();
    server.upload(&desktop);
    server.sync(&gateway);
    let work = gateway.pending_sender_unblocks().unwrap();
    gateway.begin_sender_unblock(&work[0].request_id).unwrap();
    gateway
        .complete_sender_unblock(&work[0].request_id, SenderUnblockOutcome::NotAllowed)
        .unwrap();
    server.upload(&gateway);
    server.sync(&desktop);
    assert_eq!(
        row(&desktop, conversation).unblock,
        Some(SenderUnblockStatus::Failed)
    );
}

#[test]
fn snapshot_imported_unblock_requests_are_history_and_lists_survive() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&dir, "unblock-snap-gateway", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&config(&dir, "unblock-snap-desktop", &vault), &vault);
    let conversation = provider_sms(&gateway, "s-1", ADDRESS, "t1");
    gateway
        .publish_sender_blocklist(numbers(&[ADDRESS]), true)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    desktop.request_sender_unblock(conversation).unwrap();
    server.upload(&desktop);

    // The target phone restored from a snapshot must not act on a request it learned historically.
    let restored_config = ClientConfig {
        database_path: dir.path().join("unblock-snap-restored.db"),
        ..gateway_config.clone()
    };
    let restored = unlocked(&restored_config, &vault);
    let records: Vec<SnapshotRecord> = server
        .log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect();
    let session = restored
        .begin_snapshot_with_compaction(
            Cursor(records.len() as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
            Some(1),
        )
        .unwrap();
    restored
        .append_snapshot_page(session.generation, &records)
        .unwrap();
    restored.finish_snapshot(session.generation).unwrap();
    loop {
        let report = restored.apply_pending(100).unwrap();
        if report.applied + report.drained + report.quarantined + report.superseded == 0
            && report.snapshot_remaining == 0
        {
            break;
        }
    }
    let state = row(&restored, conversation);
    assert_eq!(
        state.unblock, None,
        "a historical request is not shown as in progress"
    );
    assert_eq!(state.folder, ConversationFolder::Inbox);
    assert!(restored.pending_sender_unblocks().unwrap().is_empty());
    assert!(restored.interrupted_sender_unblocks().unwrap().is_empty());
}

#[test]
fn a_conversation_captured_after_blocking_still_moves_to_spam() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "block-late", &vault), &vault);
    gateway
        .publish_sender_blocklist(numbers(&[PEER]), true)
        .unwrap();
    let conversation = provider_sms(&gateway, "late-1", PEER, "t1");
    assert_eq!(folder_of(&gateway, conversation), ConversationFolder::Inbox);
    assert!(
        !gateway
            .publish_sender_blocklist(numbers(&[PEER]), true)
            .unwrap()
    );
    assert_eq!(folder_of(&gateway, conversation), ConversationFolder::Spam);
}

#[test]
fn a_request_cannot_complete_without_its_permit() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "unblock-no-permit-gateway", &vault), &vault);
    let desktop = unlocked(&config(&dir, "unblock-no-permit-desktop", &vault), &vault);
    let conversation = provider_sms(&gateway, "np-1", ADDRESS, "t1");
    gateway
        .publish_sender_blocklist(numbers(&[ADDRESS]), true)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    desktop.request_sender_unblock(conversation).unwrap();
    server.upload(&desktop);
    server.sync(&gateway);
    let work = gateway.pending_sender_unblocks().unwrap();
    assert_eq!(
        gateway.complete_sender_unblock(&work[0].request_id, SenderUnblockOutcome::NotAllowed),
        Err(Error::InvalidRequest("sender unblock"))
    );
}

#[test]
fn a_phone_that_lost_access_answers_not_allowed_instead_of_starting() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "unblock-lost-gateway", &vault), &vault);
    let desktop = unlocked(&config(&dir, "unblock-lost-desktop", &vault), &vault);
    let conversation = provider_sms(&gateway, "lo-1", ADDRESS, "t1");
    gateway
        .publish_sender_blocklist(numbers(&[ADDRESS]), true)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    desktop.request_sender_unblock(conversation).unwrap();
    server.upload(&desktop);
    server.sync(&gateway);
    gateway.publish_sender_blocklist(None, false).unwrap();
    let work = gateway.pending_sender_unblocks().unwrap();
    assert!(!gateway.begin_sender_unblock(&work[0].request_id).unwrap());
    assert!(gateway.pending_sender_unblocks().unwrap().is_empty());
    assert!(gateway.interrupted_sender_unblocks().unwrap().is_empty());
    server.upload(&gateway);
    server.sync(&desktop);
    assert_eq!(
        row(&desktop, conversation).unblock,
        Some(SenderUnblockStatus::Failed)
    );
}

#[test]
fn a_number_already_unblocked_in_the_messaging_app_answers_not_blocked() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "unblock-already-gateway", &vault), &vault);
    let desktop = unlocked(&config(&dir, "unblock-already-desktop", &vault), &vault);
    let conversation = provider_sms(&gateway, "al-1", ADDRESS, "t1");
    gateway
        .publish_sender_blocklist(numbers(&[ADDRESS]), true)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    desktop.request_sender_unblock(conversation).unwrap();
    server.upload(&desktop);
    server.sync(&gateway);
    gateway
        .publish_sender_blocklist(numbers(&[]), true)
        .unwrap();
    let work = gateway.pending_sender_unblocks().unwrap();
    assert!(!gateway.begin_sender_unblock(&work[0].request_id).unwrap());
    server.upload(&gateway);
    server.sync(&desktop);
    assert_eq!(row(&desktop, conversation).unblock, None);
}

#[test]
fn unblock_delivered_after_epoch_cutover_is_not_queued() {
    let dir = TempDir::new().unwrap();
    let epoch1 = Vault::new();
    let epoch2 = Vault::epoch(epoch1.id, 2);
    let gateway_config = config(&dir, "unblock-epoch-gateway", &epoch1);
    let gateway = unlocked(&gateway_config, &epoch1);
    let desktop = unlocked(&config(&dir, "unblock-epoch-desktop", &epoch1), &epoch1);
    let conversation = provider_sms(&gateway, "epoch-1", ADDRESS, "t1");
    gateway
        .publish_sender_blocklist(numbers(&[ADDRESS]), true)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    desktop.request_sender_unblock(conversation).unwrap();
    server.upload(&desktop);

    gateway
        .unlock(&epoch2.profile, &epoch2.header, PASSPHRASE)
        .unwrap();
    gateway.activate_epoch(2).unwrap();
    server.sync(&gateway);

    assert!(gateway.pending_sender_unblocks().unwrap().is_empty());
}

#[test]
fn queued_unblock_is_fenced_again_after_epoch_cutover() {
    let dir = TempDir::new().unwrap();
    let epoch1 = Vault::new();
    let epoch2 = Vault::epoch(epoch1.id, 2);
    let gateway = unlocked(&config(&dir, "queued-epoch-gateway", &epoch1), &epoch1);
    let desktop = unlocked(&config(&dir, "queued-epoch-desktop", &epoch1), &epoch1);
    let conversation = provider_sms(&gateway, "queued-epoch", ADDRESS, "t1");
    gateway
        .publish_sender_blocklist(numbers(&[ADDRESS]), true)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    desktop.request_sender_unblock(conversation).unwrap();
    server.upload(&desktop);
    server.sync(&gateway);
    let request_id = gateway.pending_sender_unblocks().unwrap()[0]
        .request_id
        .clone();

    gateway
        .unlock(&epoch2.profile, &epoch2.header, PASSPHRASE)
        .unwrap();
    gateway.activate_epoch(2).unwrap();

    assert!(gateway.pending_sender_unblocks().unwrap().is_empty());
    assert!(!gateway.begin_sender_unblock(&request_id).unwrap());
}

#[test]
fn locked_core_cannot_acquire_an_unblock_permit() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway_config = config(&dir, "locked-unblock-gateway", &vault);
    let gateway = unlocked(&gateway_config, &vault);
    let desktop = unlocked(&config(&dir, "locked-unblock-desktop", &vault), &vault);
    let conversation = provider_sms(&gateway, "locked-unblock", ADDRESS, "t1");
    gateway
        .publish_sender_blocklist(numbers(&[ADDRESS]), true)
        .unwrap();
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    desktop.request_sender_unblock(conversation).unwrap();
    server.upload(&desktop);
    server.sync(&gateway);
    let request_id = gateway.pending_sender_unblocks().unwrap()[0]
        .request_id
        .clone();
    drop(gateway);

    let locked = open(&gateway_config);
    assert_eq!(
        locked.begin_sender_unblock(&request_id),
        Err(Error::KeysUnavailable)
    );
}
