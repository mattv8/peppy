//! Public API regressions for draft movement, deletion, and phone identity.
mod common;

use common::*;
use std::{fs, sync::{Arc, Barrier}, thread};
use tempfile::TempDir;

const PEER: &str = "+14155552671";
const OTHER_PEER: &str = "+14155552672";

fn update(text: &str, recipients: &[&str], attachment_ids: Vec<AttachmentId>) -> ComposeDraftUpdate {
    ComposeDraftUpdate {
        text: text.into(),
        recipients: recipients.iter().map(|recipient| (*recipient).into()).collect(),
        attachment_ids,
        route: None,
    }
}

fn attachment(client: &Client, dir: &TempDir, name: &str) -> AttachmentId {
    let path = dir.path().join(name);
    fs::write(&path, b"draft attachment").unwrap();
    client
        .prepare_attachment(&path, "image/png", name)
        .unwrap()
        .attachment_id
}

fn incoming_group(id: &str, thread: Option<&str>, sender: &str, recipients: &[&str]) -> MmsAcquisitionInput {
    MmsAcquisitionInput {
        source: MmsSource {
            source_generation: "telephony-v1".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: id.into(),
            provider_thread_id: thread.map(str::to_owned),
        },
        direction: Direction::Incoming,
        sender_address: Some(sender.into()),
        recipients: recipients.iter().map(|recipient| (*recipient).into()).collect(),
        subject: None,
        body: "group message".into(),
        imported: false,
        observed_at_ms: 1,
        transaction_id: None,
    }
}

#[test]
fn discard_refuses_recipient_or_history_drafts_without_creating_outbox_work() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);

    let source = client.recipientless_compose_draft().unwrap();
    let recipient_draft = client
        .save_compose_draft(source.draft_id, source.revision, update("unsent", &[PEER], vec![]))
        .unwrap();
    let outbox_before = client.pending_outbox().unwrap();
    assert_eq!(
        client.discard_compose_draft(recipient_draft.draft_id, recipient_draft.revision),
        Err(Error::DraftNotDiscardable)
    );
    assert_eq!(client.pending_outbox().unwrap(), outbox_before);
    assert_eq!(
        client.compose_draft(recipient_draft.draft_id).unwrap().unwrap(),
        recipient_draft
    );

    let history = client.capture_incoming(incoming("history", "history-1")).unwrap();
    let reply = client.create_compose_draft(Some(history.conversation_id)).unwrap();
    assert_eq!(
        client.discard_compose_draft(reply.draft_id, reply.revision),
        Err(Error::DraftNotDiscardable)
    );
    assert_eq!(client.pending_outbox().unwrap().len(), outbox_before.len() + 1);
    assert!(client.compose_draft(reply.draft_id).unwrap().is_some());
}

#[test]
fn discard_is_cas_idempotent_and_keeps_attachment_shared_by_another_draft() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let media = attachment(&client, &dir, "shared.png");

    let first = client.recipientless_compose_draft().unwrap();
    let first = client
        .save_compose_draft(first.draft_id, first.revision, update("", &[], vec![media]))
        .unwrap();
    let second = client.create_compose_draft(None).unwrap();
    let second = client
        .save_compose_draft(second.draft_id, second.revision, update("", &[], vec![media]))
        .unwrap();

    assert!(matches!(
        client.discard_compose_draft(first.draft_id, first.revision + 1),
        Err(Error::StaleDraft { current_revision }) if current_revision == first.revision
    ));
    client
        .discard_compose_draft(first.draft_id, first.revision)
        .unwrap();
    client
        .discard_compose_draft(first.draft_id, first.revision)
        .unwrap();
    assert!(client.compose_draft(first.draft_id).unwrap().is_none());
    assert!(client.attachment_info(media).is_ok(), "shared media survives deletion");
    assert_eq!(
        client.compose_draft(second.draft_id).unwrap().unwrap().attachment_ids,
        vec![media]
    );
}

#[test]
fn clearing_recipients_when_a_recipientless_draft_exists_is_atomic() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let existing = client.recipientless_compose_draft().unwrap();
    let source = client.create_compose_draft(None).unwrap();
    let committed = client
        .save_compose_draft(source.draft_id, source.revision, update("keep me", &[PEER], vec![]))
        .unwrap();

    assert_eq!(
        client.save_compose_draft(
            committed.draft_id,
            committed.revision,
            update("should not replace", &[], vec![]),
        ),
        Err(Error::RecipientlessDraftExists)
    );
    assert_eq!(
        client.compose_draft(committed.draft_id).unwrap().unwrap(),
        committed
    );
    assert_eq!(
        client.compose_draft(existing.draft_id).unwrap().unwrap().recipients,
        Vec::<String>::new()
    );
}

#[test]
fn concurrent_recipientless_get_or_create_returns_one_draft() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = Arc::new(open(&config(&dir, "client", &vault)));
    let barrier = Arc::new(Barrier::new(8));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let client = Arc::clone(&client);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                client.recipientless_compose_draft().unwrap().draft_id
            })
        })
        .collect();
    let ids: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();

    assert!(ids.iter().all(|id| *id == ids[0]));
    assert_eq!(client.compose_drafts().unwrap().len(), 1);
}

#[test]
fn movement_merges_destination_content_and_rejects_both_overflow_limits_atomically() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let destination_attachment = attachment(&client, &dir, "destination.png");
    let source_attachment = attachment(&client, &dir, "source.png");

    let destination = client.recipientless_compose_draft().unwrap();
    let destination = client
        .save_compose_draft(
            destination.draft_id,
            destination.revision,
            update("destination", &[PEER], vec![destination_attachment]),
        )
        .unwrap();
    let source = client.recipientless_compose_draft().unwrap();
    let merged = client
        .save_compose_draft(
            source.draft_id,
            source.revision,
            update("source", &[PEER], vec![source_attachment]),
        )
        .unwrap();
    assert_eq!(merged.draft_id, destination.draft_id);
    assert_eq!(merged.text, "destination\n\nsource");
    assert_eq!(merged.attachment_ids, vec![destination_attachment, source_attachment]);
    assert_eq!(merged.revision, destination.revision + 1);

    let full = client.recipientless_compose_draft().unwrap();
    let full = client
        .save_compose_draft(
            full.draft_id,
            full.revision,
            update(&"x".repeat(65_536), &[OTHER_PEER], vec![]),
        )
        .unwrap();
    let text_source = client.recipientless_compose_draft().unwrap();
    assert_eq!(
        client.save_compose_draft(
            text_source.draft_id,
            text_source.revision,
            update("y", &[OTHER_PEER], vec![]),
        ),
        Err(Error::DraftMergeOverflow)
    );
    assert_eq!(client.compose_draft(full.draft_id).unwrap().unwrap(), full);
    assert_eq!(client.compose_draft(text_source.draft_id).unwrap().unwrap(), text_source);

    let attachments: Vec<_> = (0..11)
        .map(|index| attachment(&client, &dir, &format!("overflow-{index}.png")))
        .collect();
    let ten = client.recipientless_compose_draft().unwrap();
    let ten = client
        .save_compose_draft(
            ten.draft_id,
            ten.revision,
            update("ten", &["+14155552673"], attachments[..10].to_vec()),
        )
        .unwrap();
    let one = client.recipientless_compose_draft().unwrap();
    assert_eq!(
        client.save_compose_draft(
            one.draft_id,
            one.revision,
            update("one", &["+14155552673"], vec![attachments[10]]),
        ),
        Err(Error::DraftMergeOverflow)
    );
    assert_eq!(client.compose_draft(ten.draft_id).unwrap().unwrap(), ten);
    assert_eq!(client.compose_draft(one.draft_id).unwrap().unwrap(), one);
}

#[test]
fn moved_draft_refuses_late_save_send_and_discard_without_an_extra_command() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let history = client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: PEER.into(),
            body: "existing history".into(),
            provider_message_id: Some("moved-draft-history".into()),
            imported: false,
        })
        .unwrap();
    let source = client.recipientless_compose_draft().unwrap();
    let moved = client
        .save_compose_draft(source.draft_id, source.revision, update("moved", &[PEER], vec![]))
        .unwrap();
    assert_ne!(moved.draft_id, source.draft_id);
    assert_eq!(moved.conversation_id, history.conversation_id);
    assert!(client.compose_draft_redirect(source.draft_id).unwrap().is_some());
    let outbox_before = client.pending_outbox().unwrap();

    for result in [
        client.save_compose_draft(source.draft_id, source.revision, update("late", &[PEER], vec![])).map(|_| ()),
        client.resolve_compose_draft_for_send(source.draft_id, source.revision).map(|_| ()),
        client.send_compose_draft(source.draft_id, source.revision).map(|_| ()),
        client.discard_compose_draft(source.draft_id, source.revision),
    ] {
        assert!(matches!(
            result,
            Err(Error::DraftMoved { conversation_id, .. }) if conversation_id == moved.conversation_id
        ));
    }
    assert_eq!(client.pending_outbox().unwrap(), outbox_before);
}

#[test]
fn two_clients_deliver_a_command_and_canonicalize_a_formatted_sms_reply_to_its_conversation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let desktop_config = config(&dir, "desktop", &vault);
    let phone_config = config(&dir, "phone", &vault);
    let desktop = unlocked(&desktop_config, &vault);
    let phone = unlocked(&phone_config, &vault);
    let mut server = Server::default();

    let incoming = phone
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: PEER.into(),
            body: "first".into(),
            provider_message_id: Some("first".into()),
            imported: false,
        })
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    let reply = desktop.create_compose_draft(Some(incoming.conversation_id)).unwrap();
    let reply = desktop
        .save_compose_draft(
            reply.draft_id,
            reply.revision,
            ComposeDraftUpdate {
                text: "reply".into(),
                recipients: vec!["(415) 555-2671".into()],
                route: Some(route(&phone_config)),
                attachment_ids: vec![],
            },
        )
        .unwrap();
    let queued = desktop.send_compose_draft(reply.draft_id, reply.revision).unwrap();
    server.upload(&desktop);
    server.sync(&phone);
    assert_eq!(phone.pending_commands().unwrap()[0].command_id, queued.command_id);

    let formatted_reply = phone
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: "415-555-2671".into(),
            body: "second".into(),
            provider_message_id: Some("second".into()),
            imported: false,
        })
        .unwrap();
    assert_eq!(formatted_reply.conversation_id, incoming.conversation_id);
}

#[test]
fn mms_reuses_only_exact_known_groups_while_provider_threads_remain_authoritative() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let known = unlocked(&config(&dir, "known", &vault), &vault);
    known.set_mms_own_address("sim-1", ADDRESS).unwrap();

    let first = known
        .begin_mms_acquisition(incoming_group(
            "exact-first",
            None,
            PEER,
            &[ADDRESS, OTHER_PEER],
        ))
        .unwrap();
    known.complete_mms_acquisition(&first.acquisition_id).unwrap();
    let exact = known
        .begin_mms_acquisition(incoming_group(
            "exact-second",
            None,
            "(415) 555-2671",
            &[OTHER_PEER, "555-555-0100"],
        ))
        .unwrap();
    assert_eq!(exact.conversation_id, first.conversation_id);

    let mapped = known
        .begin_mms_acquisition(incoming_group(
            "thread-first",
            Some("provider-thread"),
            PEER,
            &[ADDRESS, OTHER_PEER],
        ))
        .unwrap();
    let provider_followup = known
        .begin_mms_acquisition(incoming_group(
            "thread-second",
            Some("provider-thread"),
            "+14155552673",
            &[ADDRESS],
        ))
        .unwrap();
    assert_eq!(provider_followup.conversation_id, mapped.conversation_id);

    let unknown = unlocked(&config(&dir, "unknown", &vault), &vault);
    let unknown_first = unknown
        .begin_mms_acquisition(incoming_group(
            "unknown-first",
            None,
            PEER,
            &[ADDRESS, OTHER_PEER],
        ))
        .unwrap();
    unknown
        .complete_mms_acquisition(&unknown_first.acquisition_id)
        .unwrap();
    let unknown_second = unknown
        .begin_mms_acquisition(incoming_group(
            "unknown-second",
            None,
            PEER,
            &[ADDRESS, OTHER_PEER],
        ))
        .unwrap();
    assert_ne!(unknown_second.conversation_id, unknown_first.conversation_id);
}
