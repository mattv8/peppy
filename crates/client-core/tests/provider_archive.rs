//! The gateway mirrors its messaging app's archive into folder events, reverting only its own.
use crate::common::*;
use peppy_client_core::ConversationFolder;
use tempfile::TempDir;

pub(crate) fn provider_sms(
    client: &Client,
    id: &str,
    sender: &str,
    thread: &str,
) -> ConversationId {
    let acquisition = client
        .begin_sms_acquisition(SmsAcquisitionInput {
            source: SmsSource {
                source_generation: "install-1".into(),
                subscription_id: "sim-1".into(),
                provider_message_id: id.into(),
                provider_thread_id: Some(thread.into()),
                broadcast_id: None,
            },
            direction: Direction::Incoming,
            sender_address: Some(sender.into()),
            recipients: vec![],
            body: format!("body {id}"),
            imported: false,
            sent_at_ms: Some(1_000),
            date_sent_ms: Some(1_000),
            creator: None,
            source_row_fingerprint: id.into(),
        })
        .unwrap();
    client
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap()
        .conversation_id
}

pub(crate) fn folder_of(client: &Client, conversation: ConversationId) -> ConversationFolder {
    client
        .list_conversations()
        .unwrap()
        .into_iter()
        .find(|row| row.conversation_id == conversation)
        .unwrap()
        .folder
}

fn archived(threads: &[&str]) -> Vec<String> {
    threads.iter().map(|thread| (*thread).to_owned()).collect()
}

#[test]
fn archive_edges_move_the_conversation_and_replicate() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "archive-gateway", &vault), &vault);
    let desktop = unlocked(&config(&dir, "archive-desktop", &vault), &vault);
    let conversation = provider_sms(&gateway, "a-1", ADDRESS, "t1");

    assert_eq!(
        gateway
            .reconcile_provider_archive(archived(&["t1"]))
            .unwrap(),
        1
    );
    assert_eq!(
        folder_of(&gateway, conversation),
        ConversationFolder::Archived
    );
    let mut server = Server::default();
    server.upload(&gateway);
    server.sync(&desktop);
    assert_eq!(
        folder_of(&desktop, conversation),
        ConversationFolder::Archived
    );

    assert_eq!(
        gateway.reconcile_provider_archive(archived(&[])).unwrap(),
        1
    );
    assert_eq!(folder_of(&gateway, conversation), ConversationFolder::Inbox);
}

#[test]
fn unchanged_archive_set_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "archive-noop", &vault), &vault);
    provider_sms(&gateway, "n-1", ADDRESS, "t1");
    gateway
        .reconcile_provider_archive(archived(&["t1", "unknown"]))
        .unwrap();
    let outbox = gateway.pending_outbox().unwrap().len();
    assert_eq!(
        gateway
            .reconcile_provider_archive(archived(&["unknown", "t1"]))
            .unwrap(),
        0
    );
    assert_eq!(gateway.pending_outbox().unwrap().len(), outbox);
}

#[test]
fn move_to_inbox_persists_until_the_next_archive_edge() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "archive-user-inbox", &vault), &vault);
    let conversation = provider_sms(&gateway, "u-1", ADDRESS, "t1");
    gateway
        .reconcile_provider_archive(archived(&["t1"]))
        .unwrap();
    gateway
        .set_conversation_folder(conversation, ConversationFolder::Inbox)
        .unwrap();

    assert_eq!(
        gateway
            .reconcile_provider_archive(archived(&["t1"]))
            .unwrap(),
        0
    );
    assert_eq!(folder_of(&gateway, conversation), ConversationFolder::Inbox);
    assert_eq!(
        gateway.reconcile_provider_archive(archived(&[])).unwrap(),
        0
    );
    assert_eq!(
        gateway
            .reconcile_provider_archive(archived(&["t1"]))
            .unwrap(),
        1
    );
    assert_eq!(
        folder_of(&gateway, conversation),
        ConversationFolder::Archived
    );
}

#[test]
fn unarchive_never_reverts_a_user_archive() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "archive-user-archive", &vault), &vault);
    let conversation = provider_sms(&gateway, "ua-1", ADDRESS, "t1");
    gateway
        .set_conversation_folder(conversation, ConversationFolder::Archived)
        .unwrap();
    assert_eq!(
        gateway
            .reconcile_provider_archive(archived(&["t1"]))
            .unwrap(),
        0
    );
    assert_eq!(
        gateway.reconcile_provider_archive(archived(&[])).unwrap(),
        0
    );
    assert_eq!(
        folder_of(&gateway, conversation),
        ConversationFolder::Archived
    );
}

#[test]
fn archive_edge_never_overrides_spam() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "archive-spam", &vault), &vault);
    let conversation = provider_sms(&gateway, "s-1", ADDRESS, "t1");
    gateway
        .set_conversation_folder(conversation, ConversationFolder::Spam)
        .unwrap();
    assert_eq!(
        gateway
            .reconcile_provider_archive(archived(&["t1"]))
            .unwrap(),
        0
    );
    assert_eq!(folder_of(&gateway, conversation), ConversationFolder::Spam);
}

#[test]
fn archive_requires_unlocked_keys() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = open(&config(&dir, "archive-locked", &vault));
    assert_eq!(
        gateway.reconcile_provider_archive(archived(&["t1"])),
        Err(Error::KeysUnavailable)
    );
}

#[test]
fn a_thread_archived_before_capture_archives_once_it_maps() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "archive-late", &vault), &vault);
    assert_eq!(
        gateway
            .reconcile_provider_archive(archived(&["t1"]))
            .unwrap(),
        0
    );
    let conversation = provider_sms(&gateway, "late-1", ADDRESS, "t1");
    assert_eq!(
        gateway
            .reconcile_provider_archive(archived(&["t1"]))
            .unwrap(),
        1
    );
    assert_eq!(
        folder_of(&gateway, conversation),
        ConversationFolder::Archived
    );
}

#[test]
fn a_reused_thread_id_from_an_old_provider_generation_is_ignored() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "archive-generation", &vault), &vault);
    let old = provider_sms(&gateway, "gen-1", ADDRESS, "t1");
    let acquisition = gateway
        .begin_sms_acquisition(SmsAcquisitionInput {
            source: SmsSource {
                source_generation: "install-2".into(),
                subscription_id: "sim-1".into(),
                provider_message_id: "gen-2".into(),
                provider_thread_id: Some("t9".into()),
                broadcast_id: None,
            },
            direction: Direction::Incoming,
            sender_address: Some("+14155552672".into()),
            recipients: vec![],
            body: "new install".into(),
            imported: false,
            sent_at_ms: Some(2_000),
            date_sent_ms: Some(2_000),
            creator: None,
            source_row_fingerprint: "gen-2".into(),
        })
        .unwrap();
    gateway
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    assert_eq!(
        gateway
            .reconcile_provider_archive(archived(&["t1"]))
            .unwrap(),
        0
    );
    assert_eq!(folder_of(&gateway, old), ConversationFolder::Inbox);
}

#[test]
fn a_provider_reset_forgets_observed_threads() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "archive-reset", &vault), &vault);
    let old = provider_sms(&gateway, "rs-1", ADDRESS, "t1");
    gateway
        .reconcile_provider_archive(archived(&["t1"]))
        .unwrap();
    assert_eq!(folder_of(&gateway, old), ConversationFolder::Archived);
    gateway
        .set_conversation_folder(old, ConversationFolder::Inbox)
        .unwrap();
    let acquisition = gateway
        .begin_sms_acquisition(SmsAcquisitionInput {
            source: SmsSource {
                source_generation: "install-2".into(),
                subscription_id: "sim-1".into(),
                provider_message_id: "rs-2".into(),
                provider_thread_id: Some("t1".into()),
                broadcast_id: None,
            },
            direction: Direction::Incoming,
            sender_address: Some("+14155552672".into()),
            recipients: vec![],
            body: "new install".into(),
            imported: false,
            sent_at_ms: Some(2_000),
            date_sent_ms: Some(2_000),
            creator: None,
            source_row_fingerprint: "rs-2".into(),
        })
        .unwrap();
    let new = gateway
        .complete_sms_acquisition(&acquisition.acquisition_id)
        .unwrap()
        .conversation_id;
    assert_ne!(new, old);
    assert_eq!(
        gateway
            .reconcile_provider_archive(archived(&["t1"]))
            .unwrap(),
        1
    );
    assert_eq!(folder_of(&gateway, new), ConversationFolder::Archived);
    assert_eq!(folder_of(&gateway, old), ConversationFolder::Inbox);
}

#[test]
fn a_first_mms_generation_is_not_a_provider_reset() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let gateway = unlocked(&config(&dir, "archive-first-mms", &vault), &vault);
    let conversation = provider_sms(&gateway, "fm-1", ADDRESS, "t1");
    gateway
        .reconcile_provider_archive(archived(&["t1"]))
        .unwrap();
    gateway
        .set_conversation_folder(conversation, ConversationFolder::Inbox)
        .unwrap();
    gateway
        .begin_mms_acquisition(MmsAcquisitionInput {
            source: MmsSource {
                source_generation: "mms-install-1".into(),
                subscription_id: "sim-1".into(),
                provider_message_id: "fm-mms".into(),
                provider_thread_id: Some("t2".into()),
            },
            direction: Direction::Incoming,
            sender_address: Some("+14155552672".into()),
            recipients: vec![],
            subject: None,
            body: "first mms".into(),
            imported: false,
            observed_at_ms: 3_000,
            transaction_id: None,
        })
        .unwrap();
    assert_eq!(
        gateway
            .reconcile_provider_archive(archived(&["t1"]))
            .unwrap(),
        0
    );
    assert_eq!(folder_of(&gateway, conversation), ConversationFolder::Inbox);
}
