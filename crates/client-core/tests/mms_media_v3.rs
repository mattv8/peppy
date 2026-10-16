//! v3 MMS media lifecycle regressions: thumbnails are private derivatives, never visible parts.
mod common;

use common::*;
use std::{collections::HashMap, fs, path::Path};
use tempfile::TempDir;

fn input(id: &str) -> MmsAcquisitionInput {
    MmsAcquisitionInput {
        source: MmsSource {
            source_generation: "thumbnail-regression".into(),
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

fn prepare(client: &Client, dir: &TempDir, name: &str, bytes: &[u8]) -> AttachmentId {
    let path = dir.path().join(name);
    fs::write(&path, bytes).unwrap();
    client
        .prepare_attachment(&path, "image/png", name)
        .unwrap()
        .attachment_id
}

#[derive(Default)]
struct Objects(HashMap<String, Vec<u8>>);

impl Objects {
    fn upload_all(&mut self, client: &Client) {
        for object in client.pending_uploads().unwrap() {
            let remote = uuid::Uuid::new_v4().to_string();
            self.0.insert(
                remote.clone(),
                fs::read(client.native_cipher_file(object.attachment_id).unwrap()).unwrap(),
            );
            client
                .mark_attachment_uploaded(object.attachment_id, &remote)
                .unwrap();
        }
    }

    fn install(&self, client: &Client, object: &CipherObject, path: &Path) {
        fs::write(path, &self.0[object.remote_object_id.as_ref().unwrap()]).unwrap();
        client
            .install_downloaded_attachment(object.attachment_id, path)
            .unwrap();
    }
}

#[test]
fn thumbnail_link_is_retained_idempotent_and_gates_sealing_across_reopen() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "gateway", &vault);
    let client = unlocked(&cfg, &vault);
    let acquisition = client
        .begin_mms_acquisition(input("thumbnail-gate"))
        .unwrap();
    let original = prepare(&client, &dir, "original.png", b"original image");
    let thumbnail = prepare(&client, &dir, "thumbnail.png", b"thumbnail image");
    client
        .set_mms_acquisition_part(&acquisition.acquisition_id, "original", original)
        .unwrap();
    client
        .set_mms_acquisition_part_metadata(
            &acquisition.acquisition_id,
            "original",
            Some(480),
            Some(320),
            None,
            Some(thumbnail),
        )
        .unwrap();
    let pending_bytes = client.mms_pending_media_bytes().unwrap();
    assert!(!client.discard_unreferenced_attachment(thumbnail).unwrap());
    client
        .set_mms_acquisition_part_metadata(
            &acquisition.acquisition_id,
            "original",
            Some(480),
            Some(320),
            None,
            Some(thumbnail),
        )
        .unwrap();
    assert_eq!(client.mms_pending_media_bytes().unwrap(), pending_bytes);

    client
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    let original_object = client
        .pending_uploads()
        .unwrap()
        .into_iter()
        .find(|object| object.attachment_id == original)
        .unwrap();
    client
        .mark_attachment_uploaded(original, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    assert!(client.pending_outbox().unwrap().is_empty());
    drop(original_object);
    drop(client);

    let reopened = unlocked(&cfg, &vault);
    let part = reopened
        .mms_acquisition_parts(&acquisition.acquisition_id)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(part.thumbnail_attachment_id, Some(thumbnail));
    assert_eq!((part.width, part.height), (Some(480), Some(320)));
    reopened
        .mark_attachment_uploaded(thumbnail, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    assert_eq!(reopened.pending_outbox().unwrap().len(), 1);
}

#[test]
fn v3_thumbnail_is_eager_but_visible_originals_remain_demanded_through_snapshot() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let source = unlocked(&config(&dir, "source", &vault), &vault);
    let receiver_cfg = config(&dir, "receiver", &vault);
    let receiver = unlocked(&receiver_cfg, &vault);
    let mut server = Server::default();
    let mut objects = Objects::default();

    let acquisition = source.begin_mms_acquisition(input("receive-v3")).unwrap();
    let first = prepare(&source, &dir, "first.png", b"first visible image");
    let second = prepare(&source, &dir, "second.png", b"second visible image");
    let thumbnail = prepare(&source, &dir, "first-thumb.png", b"first thumbnail");
    source
        .set_mms_acquisition_part(&acquisition.acquisition_id, "first", first)
        .unwrap();
    source
        .set_mms_acquisition_part(&acquisition.acquisition_id, "second", second)
        .unwrap();
    source
        .set_mms_acquisition_part_metadata(
            &acquisition.acquisition_id,
            "first",
            Some(480),
            Some(320),
            None,
            Some(thumbnail),
        )
        .unwrap();
    let captured = source
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    objects.upload_all(&source);
    server.upload(&source);
    server.sync(&receiver);

    let downloads = receiver.pending_downloads().unwrap();
    assert_eq!(downloads.len(), 1, "only the thumbnail is eager for v3 MMS");
    assert_eq!(downloads[0].attachment_id, thumbnail);
    let download = dir.path().join("thumbnail.cipher");
    objects.install(&receiver, &downloads[0], &download);
    let page = receiver
        .media_page(captured.conversation_id, None, 10)
        .unwrap();
    assert_eq!(
        page.items.len(),
        2,
        "thumbnails are not visible attachments"
    );
    let first_item = page
        .items
        .iter()
        .find(|item| item.attachment.attachment_id == first)
        .unwrap();
    assert_eq!(
        first_item
            .media_meta
            .as_ref()
            .and_then(|metadata| metadata.thumbnail.as_ref())
            .map(|info| info.attachment_id),
        Some(thumbnail)
    );
    assert!(receiver.pending_downloads().unwrap().is_empty());

    receiver.request_attachment_download(first).unwrap();
    let demanded = receiver.pending_downloads().unwrap();
    assert_eq!(demanded.len(), 1);
    assert_eq!(demanded[0].attachment_id, first);
    objects.install(&receiver, &demanded[0], &download);
    assert_eq!(
        receiver.attachment_info(first).unwrap().state,
        AttachmentState::Available
    );

    let snapshot_cfg = config(&dir, "snapshot", &vault);
    let snapshot_client = unlocked(&snapshot_cfg, &vault);
    let records: Vec<_> = server
        .log
        .iter()
        .enumerate()
        .map(|(index, envelope)| SnapshotRecord {
            cursor: Cursor(index as u64 + 1),
            envelope: envelope.clone(),
        })
        .collect();
    let session = snapshot_client
        .begin_snapshot(
            Cursor(records.len() as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
        )
        .unwrap();
    snapshot_client
        .append_snapshot_page(session.generation, &records)
        .unwrap();
    snapshot_client.finish_snapshot(session.generation).unwrap();
    snapshot_client.apply_pending(100).unwrap();
    let snapshot_page = snapshot_client
        .media_page(captured.conversation_id, None, 10)
        .unwrap();
    assert_eq!(snapshot_page.items.len(), 2);
    assert_eq!(
        snapshot_page
            .items
            .iter()
            .find(|item| item.attachment.attachment_id == first)
            .unwrap()
            .media_meta
            .as_ref()
            .and_then(|metadata| metadata.thumbnail.as_ref())
            .map(|info| info.attachment_id),
        Some(thumbnail)
    );
}

#[test]
fn evicting_a_demanded_original_returns_it_to_pending_without_evicting_uploads() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let source = unlocked(&config(&dir, "source", &vault), &vault);
    let receiver = unlocked(&config(&dir, "receiver", &vault), &vault);
    let mut server = Server::default();
    let mut objects = Objects::default();

    let acquisition = source.begin_mms_acquisition(input("evict-v3")).unwrap();
    let original = prepare(&source, &dir, "original.png", b"original image");
    let thumbnail = prepare(&source, &dir, "thumbnail.png", b"thumbnail image");
    source
        .set_mms_acquisition_part(&acquisition.acquisition_id, "original", original)
        .unwrap();
    source
        .set_mms_acquisition_part_metadata(
            &acquisition.acquisition_id,
            "original",
            None,
            None,
            None,
            Some(thumbnail),
        )
        .unwrap();
    source
        .complete_mms_acquisition(&acquisition.acquisition_id)
        .unwrap();
    assert!(!source.evict_attachment_ciphertext(original).unwrap());
    objects.upload_all(&source);
    server.upload(&source);
    server.sync(&receiver);

    let download = dir.path().join("original.cipher");
    let thumbnail_download = receiver.pending_downloads().unwrap().pop().unwrap();
    objects.install(&receiver, &thumbnail_download, &download);
    receiver.request_attachment_download(original).unwrap();
    let original_download = receiver.pending_downloads().unwrap().pop().unwrap();
    objects.install(&receiver, &original_download, &download);
    assert!(receiver.evict_attachment_ciphertext(original).unwrap());
    assert_eq!(
        receiver.attachment_info(original).unwrap().state,
        AttachmentState::PendingDownload
    );
    assert!(receiver.pending_downloads().unwrap().is_empty());
    assert!(
        !receiver
            .local_attachment_ids(None, 1_000)
            .unwrap()
            .contains(&original)
    );
}
