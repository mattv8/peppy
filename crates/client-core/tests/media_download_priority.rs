//! Download candidate priority is host-scheduled after core eligibility filtering.

use crate::common::*;
use std::fs;
use tempfile::TempDir;

fn input(id: &str) -> MmsAcquisitionInput {
    MmsAcquisitionInput {
        source: MmsSource {
            source_generation: "download-priority".into(),
            subscription_id: "sim-1".into(),
            provider_message_id: id.into(),
            provider_thread_id: Some("thread-1".into()),
        },
        direction: Direction::Incoming,
        sender_address: Some(ADDRESS.into()),
        recipients: vec![],
        subject: None,
        body: "pictures".into(),
        imported: false,
        observed_at_ms: 1,
        transaction_id: None,
    }
}

fn prepare(client: &Client, dir: &TempDir, name: &str) -> AttachmentId {
    let path = dir.path().join(name);
    fs::write(&path, name.as_bytes()).unwrap();
    client
        .prepare_attachment(&path, "image/png", name)
        .unwrap()
        .attachment_id
}

#[test]
fn message_by_id_returns_existing_message_and_none_for_missing_id() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "message-lookup", &vault), &vault);
    let captured = client
        .capture_incoming(incoming("lookup body", "lookup-id"))
        .unwrap();

    let message = client.message_by_id(captured.message_id).unwrap().unwrap();
    assert_eq!(message.payload.record.message_id, captured.message_id);
    assert_eq!(
        message.payload.record.conversation_id,
        captured.conversation_id
    );
    assert_eq!(message.payload.body, "lookup body");
    assert_eq!(client.message_by_id(MessageId::new()).unwrap(), None);
}

#[test]
fn pending_download_candidates_classify_demand_without_changing_eligibility() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let source = unlocked(&config(&dir, "source", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let acquisition = source.begin_mms_acquisition(input("priority")).unwrap();
    let original = prepare(&source, &dir, "original.png");
    let thumbnail = prepare(&source, &dir, "thumbnail.png");
    source
        .set_mms_acquisition_part(&acquisition.acquisition_id, "original", original)
        .unwrap();
    source
        .set_mms_acquisition_part_metadata(
            &acquisition.acquisition_id,
            "original",
            Some(480),
            Some(320),
            None,
            Some(thumbnail),
        )
        .unwrap();
    source
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();

    let mut server = Server::default();
    for object in source.pending_uploads().unwrap() {
        source
            .mark_attachment_uploaded(object.attachment_id, &uuid::Uuid::new_v4().to_string())
            .unwrap();
    }
    server.upload(&source);
    server.sync(&receiver);

    let eager = receiver.pending_download_candidates().unwrap();
    assert_eq!(eager.len(), 1);
    assert_eq!(eager[0].object.attachment_id, thumbnail);
    assert!(!eager[0].demanded);
    assert_eq!(
        receiver.pending_downloads().unwrap()[0].attachment_id,
        thumbnail,
        "the existing API retains its eligible object list"
    );

    receiver.request_attachment_download(original).unwrap();
    let candidates = receiver.pending_download_candidates().unwrap();
    assert_eq!(
        candidates
            .iter()
            .find(|candidate| candidate.object.attachment_id == original)
            .map(|candidate| candidate.demanded),
        Some(true)
    );
}
