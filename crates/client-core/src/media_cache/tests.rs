use crate::{
    AttachmentId, AttachmentState, Client, ClientConfig, CommandId, DatabaseKey, DeviceId, Error,
    KeyProfile, VaultId, attachment_row, media,
};
use peppy_crypto::{create_vault_check_header, derive_root_key};
use rusqlite::params;
use std::fs;
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple";

struct TestVault {
    id: VaultId,
    profile: KeyProfile,
    header: peppy_crypto::VaultCheckHeader,
}

fn vault() -> TestVault {
    let id = VaultId::new();
    let profile = KeyProfile::new(id.0, 1).unwrap();
    let root = derive_root_key(PASSPHRASE, &profile).unwrap();
    TestVault {
        id,
        header: create_vault_check_header(&root, profile.clone()).unwrap(),
        profile,
    }
}

fn client(dir: &TempDir, name: &str, vault: &TestVault) -> (ClientConfig, Client) {
    let config = ClientConfig {
        database_path: dir.path().join(format!("{name}.db")),
        vault_id: vault.id,
        device_id: DeviceId::new(),
    };
    let client = Client::open(config.clone(), DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
    client
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    (config, client)
}

fn prepare(client: &Client, dir: &TempDir, name: &str, bytes: &[u8]) -> AttachmentId {
    let path = dir.path().join(name);
    fs::write(&path, bytes).unwrap();
    client
        .prepare_attachment(&path, "image/png", name)
        .unwrap()
        .attachment_id
}

/// Installs an actual secretstream ciphertext produced by `prepare_attachment` as a remote v3
/// download. The test copies the descriptor exactly, then exercises the production verifier.
fn stage_download(
    source: &Client,
    receiver: &Client,
    id: AttachmentId,
    dir: &TempDir,
) -> std::path::PathBuf {
    let (media_type, display_name, plaintext_bytes, ciphertext_bytes, sha256, file_key, cipher) = {
        let source_store = source.lock().unwrap();
        let row = attachment_row(&source_store.conn, id).unwrap().unwrap();
        (
            row.media_type,
            row.display_name,
            row.plaintext_bytes,
            row.ciphertext_bytes,
            row.ciphertext_sha256,
            row.file_key.to_vec(),
            fs::read(media::cipher_path(&source_store.media_dir, id)).unwrap(),
        )
    };
    {
        let receiver_store = receiver.lock().unwrap();
        receiver_store.conn.execute(
            "INSERT INTO attachments(attachment_id,state,media_type,display_name,plaintext_bytes,ciphertext_bytes,ciphertext_sha256,stream_version,file_key,remote_object_id) VALUES(?,?,?,?,?,?,?,?,?,?)",
            params![
                id.to_string(),
                AttachmentState::PendingDownload.code(),
                media_type,
                display_name,
                plaintext_bytes as i64,
                ciphertext_bytes as i64,
                sha256,
                media::STREAM_VERSION,
                file_key,
                uuid::Uuid::new_v4().to_string(),
            ],
        ).unwrap();
    }
    let path = dir.path().join(format!("{id}.download"));
    fs::write(&path, cipher).unwrap();
    path
}

fn download(source: &Client, receiver: &Client, id: AttachmentId, dir: &TempDir) {
    let path = stage_download(source, receiver, id, dir);
    receiver.install_downloaded_attachment(id, &path).unwrap();
}

fn mark_v3_original(
    client: &Client,
    attachment_id: AttachmentId,
    thumbnail_attachment_id: Option<AttachmentId>,
) {
    client
        .lock()
        .unwrap()
        .conn
        .execute(
            "INSERT INTO message_media_meta(message_id,attachment_id,width,height,duration_ms,thumbnail_attachment_id) VALUES(?,?,?,?,?,?)",
            params![
                uuid::Uuid::new_v4().to_string(),
                attachment_id.to_string(),
                1_i64,
                1_i64,
                None::<i64>,
                thumbnail_attachment_id.map(|id| id.to_string()),
            ],
        )
        .unwrap();
}

#[test]
fn verified_download_lru_leases_protection_and_reopen_are_core_owned() {
    let dir = TempDir::new().unwrap();
    let vault = vault();
    let (_, source) = client(&dir, "source", &vault);
    let (receiver_config, receiver) = client(&dir, "receiver", &vault);
    let oldest = prepare(&source, &dir, "oldest.png", b"oldest encrypted image");
    let leased = prepare(&source, &dir, "leased.png", b"leased encrypted image");
    let thumbnail = prepare(&source, &dir, "thumbnail.png", b"thumbnail encrypted image");

    download(&source, &receiver, oldest, &dir);
    download(&source, &receiver, leased, &dir);
    download(&source, &receiver, thumbnail, &dir);
    mark_v3_original(&receiver, oldest, Some(thumbnail));
    mark_v3_original(&receiver, leased, None);
    let second_owner = receiver.clone();
    second_owner.retain_attachment_ciphertext(leased).unwrap();

    crate::media_cache::trim_to_budget_for_test(&receiver, 0).unwrap();
    assert_eq!(
        receiver.attachment_info(oldest).unwrap().state,
        AttachmentState::PendingDownload
    );
    assert_eq!(
        receiver.attachment_info(leased).unwrap().state,
        AttachmentState::Available
    );
    assert_eq!(
        receiver.attachment_info(thumbnail).unwrap().state,
        AttachmentState::Available
    );

    assert!(second_owner.release_attachment_ciphertext(leased).unwrap());
    drop(second_owner);
    drop(receiver);
    let reopened = Client::open(receiver_config, DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
    crate::media_cache::trim_to_budget_for_test(&reopened, 0).unwrap();
    assert_eq!(
        reopened.attachment_info(leased).unwrap().state,
        AttachmentState::PendingDownload
    );
    assert_eq!(
        reopened.attachment_info(thumbnail).unwrap().state,
        AttachmentState::Available
    );
}

fn command_dependency(
    client: &Client,
    attachment_id: AttachmentId,
    attempt_state: &str,
) -> CommandId {
    let command_id = CommandId::new();
    let message_id = uuid::Uuid::new_v4().to_string();
    let store = client.lock().unwrap();
    store
        .conn
        .execute(
            "INSERT INTO commands(command_id,message_id,gateway_device_id) VALUES(?,?,?)",
            params![
                command_id.to_string(),
                message_id.clone(),
                store.config.device_id.to_string(),
            ],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO message_attachments(message_id,position,attachment_id) VALUES(?,?,?)",
            params![message_id, 0_i64, attachment_id.to_string()],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO command_ledger(command_id,key_epoch,subscription_id,payload,retired,historical) VALUES(?,?,?,?,0,0)",
            params![command_id.to_string(), 1_i64, "sim-1", "{}"],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO attempts(command_id,state) VALUES(?,?)",
            params![command_id.to_string(), attempt_state],
        )
        .unwrap();
    command_id
}

#[test]
fn terminal_command_media_is_evictable_but_unfinished_command_media_is_protected() {
    let dir = TempDir::new().unwrap();
    let vault = vault();
    let (_, source) = client(&dir, "command-source", &vault);
    let (_, receiver) = client(&dir, "command-receiver", &vault);
    let terminal = prepare(&source, &dir, "terminal.png", b"terminal command image");
    let unfinished = prepare(&source, &dir, "unfinished.png", b"unfinished command image");

    download(&source, &receiver, terminal, &dir);
    download(&source, &receiver, unfinished, &dir);
    mark_v3_original(&receiver, terminal, None);
    mark_v3_original(&receiver, unfinished, None);
    command_dependency(&receiver, terminal, "sent");
    command_dependency(&receiver, unfinished, "outcome_unknown");

    assert!(receiver.evict_attachment_ciphertext(terminal).unwrap());
    assert!(!receiver.evict_attachment_ciphertext(unfinished).unwrap());
}

#[test]
fn fulfilled_demand_is_ready_then_evicts_under_later_lru_pressure() {
    let dir = TempDir::new().unwrap();
    let vault = vault();
    let (_, source) = client(&dir, "cache-source", &vault);
    let (_, receiver) = client(&dir, "cache-receiver", &vault);
    let legacy = prepare(&source, &dir, "legacy.png", b"legacy eager image");
    let contact = prepare(&source, &dir, "contact.png", b"contact photo image");
    let thumbnail = prepare(&source, &dir, "thumbnail.png", b"thumbnail image");
    let demanded = prepare(&source, &dir, "original.png", b"demanded v3 original");
    let newer = prepare(&source, &dir, "newer.png", b"newer v3 original");

    download(&source, &receiver, legacy, &dir);
    download(&source, &receiver, contact, &dir);
    download(&source, &receiver, thumbnail, &dir);
    mark_v3_original(&receiver, contact, None);
    let demanded_path = stage_download(&source, &receiver, demanded, &dir);
    receiver.request_attachment_download(demanded).unwrap();
    receiver
        .install_downloaded_attachment(demanded, &demanded_path)
        .unwrap();
    mark_v3_original(&receiver, demanded, Some(thumbnail));
    receiver
        .lock()
        .unwrap()
        .conn
        .execute(
            "INSERT INTO contact_photo_received(attachment_id,rejected) VALUES(?,0)",
            [contact.to_string()],
        )
        .unwrap();

    assert_eq!(
        crate::media_cache::RETAINED_DOWNLOAD_CIPHERTEXT_BUDGET_BYTES,
        128 * 1024 * 1024
    );
    for protected in [legacy, contact, thumbnail] {
        assert_eq!(
            receiver.attachment_info(protected).unwrap().state,
            AttachmentState::Available
        );
    }
    assert_eq!(
        receiver.attachment_info(demanded).unwrap().state,
        AttachmentState::Available
    );
    assert!(!receiver.attachment_download_requested(demanded).unwrap());

    download(&source, &receiver, newer, &dir);
    mark_v3_original(&receiver, newer, None);
    let budget = receiver.attachment_info(newer).unwrap().ciphertext_bytes;
    crate::media_cache::trim_to_budget_for_test(&receiver, budget).unwrap();
    assert_eq!(
        receiver.attachment_info(demanded).unwrap().state,
        AttachmentState::PendingDownload
    );
    assert_eq!(
        receiver.attachment_info(newer).unwrap().state,
        AttachmentState::Available
    );
}

#[test]
fn locally_prepared_ciphertext_can_be_leased_while_pending_remote_is_rejected() {
    let dir = TempDir::new().unwrap();
    let vault = vault();
    let (_, client) = client(&dir, "local-source", &vault);
    let local_source = prepare(&client, &dir, "local-source.png", b"locally prepared image");

    client.retain_attachment_ciphertext(local_source).unwrap();
    client.touch_attachment_ciphertext(local_source).unwrap();
    assert!(client.release_attachment_ciphertext(local_source).unwrap());
    let cached: bool = client
        .lock()
        .unwrap()
        .conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM media_cache_entries WHERE attachment_id=?)",
            [local_source.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !cached,
        "local sources are leased but not retained-download LRU entries"
    );

    let pending_remote = prepare(&client, &dir, "pending-remote.png", b"not downloaded here");
    client
        .lock()
        .unwrap()
        .conn
        .execute(
            "UPDATE attachments SET state='pending_download',remote_object_id=? WHERE attachment_id=?",
            params![uuid::Uuid::new_v4().to_string(), pending_remote.to_string()],
        )
        .unwrap();
    assert!(matches!(
        client.retain_attachment_ciphertext(pending_remote),
        Err(Error::InvalidMedia)
    ));
    assert!(matches!(
        client.touch_attachment_ciphertext(pending_remote),
        Err(Error::InvalidMedia)
    ));
}

#[test]
fn reopen_clears_only_fulfilled_legacy_demand_and_keeps_pending_request() {
    let dir = TempDir::new().unwrap();
    let vault = vault();
    let (_, source) = client(&dir, "reopen-source", &vault);
    let (receiver_config, receiver) = client(&dir, "reopen-receiver", &vault);
    let legacy = prepare(
        &source,
        &dir,
        "legacy-fulfilled.png",
        b"legacy fulfilled original",
    );
    let pending = prepare(
        &source,
        &dir,
        "genuine-pending.png",
        b"genuine pending original",
    );

    download(&source, &receiver, legacy, &dir);
    mark_v3_original(&receiver, legacy, None);
    stage_download(&source, &receiver, pending, &dir);
    receiver.request_attachment_download(pending).unwrap();
    receiver
        .lock()
        .unwrap()
        .conn
        .execute(
            "INSERT INTO attachment_demand(attachment_id,requested_at) VALUES(?,0)",
            [legacy.to_string()],
        )
        .unwrap();
    assert!(receiver.attachment_download_requested(legacy).unwrap());
    assert!(receiver.attachment_download_requested(pending).unwrap());

    drop(receiver);
    let reopened = Client::open(receiver_config, DatabaseKey::new(&[7; 32]).unwrap()).unwrap();
    assert!(!reopened.attachment_download_requested(legacy).unwrap());
    assert_eq!(
        reopened.attachment_info(legacy).unwrap().state,
        AttachmentState::Available
    );
    assert!(reopened.attachment_download_requested(pending).unwrap());
    assert_eq!(
        reopened.attachment_info(pending).unwrap().state,
        AttachmentState::PendingDownload
    );

    crate::media_cache::trim_to_budget_for_test(&reopened, 0).unwrap();
    assert_eq!(
        reopened.attachment_info(legacy).unwrap().state,
        AttachmentState::PendingDownload
    );
    assert_eq!(
        reopened.attachment_info(pending).unwrap().state,
        AttachmentState::PendingDownload
    );
    assert!(reopened.attachment_download_requested(pending).unwrap());
}
