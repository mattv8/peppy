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
fn eviction_treats_a_missing_ciphertext_file_as_done_but_keeps_other_io_errors() {
    let dir = TempDir::new().unwrap();
    let vault = vault();
    let (_, source) = client(&dir, "eviction-source", &vault);
    let (_, receiver) = client(&dir, "eviction-receiver", &vault);
    let missing = prepare(&source, &dir, "missing.png", b"missing ciphertext image");
    let blocked = prepare(&source, &dir, "blocked.png", b"blocked ciphertext image");
    for id in [missing, blocked] {
        download(&source, &receiver, id, &dir);
        mark_v3_original(&receiver, id, None);
        command_dependency(&receiver, id, "sent");
    }
    let media_dir = receiver.lock().unwrap().media_dir.clone();
    fs::remove_file(media::cipher_path(&media_dir, missing)).unwrap();
    // A directory at the ciphertext path makes removal fail with a non-NotFound error.
    let blocked_path = media::cipher_path(&media_dir, blocked);
    fs::remove_file(&blocked_path).unwrap();
    fs::create_dir(&blocked_path).unwrap();

    assert!(receiver.evict_attachment_ciphertext(missing).unwrap());
    assert!(matches!(
        receiver.evict_attachment_ciphertext(blocked),
        Err(Error::Storage)
    ));
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
fn multi_eviction_walks_lru_past_leased_and_protected_rows() {
    let dir = TempDir::new().unwrap();
    let vault = vault();
    let (_, source) = client(&dir, "multi-source", &vault);
    let (_, receiver) = client(&dir, "multi-receiver", &vault);
    let leased = prepare(&source, &dir, "leased.png", b"leased lru image");
    let protected = prepare(&source, &dir, "protected.png", b"protected lru image");
    let missing = prepare(&source, &dir, "missing.png", b"missing lru image");
    let evicted = prepare(&source, &dir, "evicted.png", b"evicted lru image");
    let newest = prepare(&source, &dir, "newest.png", b"newest lru image");

    // Leasing bumps `leased` to the next access value, so it stays the oldest candidate; the
    // trim must skip it rather than stop at it.
    download(&source, &receiver, leased, &dir);
    receiver.retain_attachment_ciphertext(leased).unwrap();
    for id in [protected, missing, evicted, newest] {
        download(&source, &receiver, id, &dir);
    }
    for id in [leased, protected, missing, evicted, newest] {
        mark_v3_original(&receiver, id, None);
    }
    receiver
        .lock()
        .unwrap()
        .conn
        .execute(
            "INSERT INTO contact_photo_received(attachment_id,rejected) VALUES(?,0)",
            [protected.to_string()],
        )
        .unwrap();
    // A ciphertext file that is already gone must count as evicted, not stop the walk.
    let media_dir = receiver.lock().unwrap().media_dir.clone();
    fs::remove_file(media::cipher_path(&media_dir, missing)).unwrap();

    let budget = receiver.attachment_info(newest).unwrap().ciphertext_bytes;
    crate::media_cache::trim_to_budget_for_test(&receiver, budget).unwrap();

    assert_eq!(
        receiver.attachment_info(missing).unwrap().state,
        AttachmentState::PendingDownload
    );
    assert_eq!(
        receiver.attachment_info(evicted).unwrap().state,
        AttachmentState::PendingDownload
    );
    for kept in [leased, protected, newest] {
        assert_eq!(
            receiver.attachment_info(kept).unwrap().state,
            AttachmentState::Available
        );
    }
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

#[test]
fn gateway_cleanup_bootstrap_advances_a_fixed_attachment_rowid_window() {
    let dir = TempDir::new().unwrap();
    let vault = vault();
    let (_, client) = client(&dir, "cleanup-bootstrap", &vault);
    for number in 0..(super::MAX_GATEWAY_MMS_CLEANUP_BATCH + 1) {
        let attachment = prepare(
            &client,
            &dir,
            &format!("legacy-{number}.png"),
            b"legacy ciphertext",
        );
        client
            .mark_attachment_uploaded(attachment, &uuid::Uuid::new_v4().to_string())
            .unwrap();
    }

    assert_eq!(
        client.cleanup_uploaded_gateway_mms_ciphertext(1).unwrap(),
        super::GatewayMmsCleanup::default()
    );
    let (first_cursor, first_finished): (i64, bool) = client
        .lock()
        .unwrap()
        .conn
        .query_row(
            "SELECT attachment_rowid,finished FROM gateway_mms_cleanup_bootstrap WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(first_cursor, super::MAX_GATEWAY_MMS_CLEANUP_BATCH as i64);
    assert!(!first_finished);

    client.cleanup_uploaded_gateway_mms_ciphertext(1).unwrap();
    client.cleanup_uploaded_gateway_mms_ciphertext(1).unwrap();
    let (finished_cursor, finished): (i64, bool) = client
        .lock()
        .unwrap()
        .conn
        .query_row(
            "SELECT attachment_rowid,finished FROM gateway_mms_cleanup_bootstrap WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(finished);
    assert_eq!(
        finished_cursor,
        (super::MAX_GATEWAY_MMS_CLEANUP_BATCH + 1) as i64
    );

    client.cleanup_uploaded_gateway_mms_ciphertext(1).unwrap();
    let cursor_after_empty_tail: i64 = client
        .lock()
        .unwrap()
        .conn
        .query_row(
            "SELECT attachment_rowid FROM gateway_mms_cleanup_bootstrap WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cursor_after_empty_tail, finished_cursor);
}

#[test]
fn fulfilled_gateway_mms_marker_allows_normal_lru_thumbnail_eviction() {
    let dir = TempDir::new().unwrap();
    let vault = vault();
    let (_, source) = client(&dir, "marker-source", &vault);
    let (_, receiver) = client(&dir, "marker-receiver", &vault);
    let original = prepare(&source, &dir, "marker-original.png", b"original");
    let thumbnail = prepare(&source, &dir, "marker-thumbnail.png", b"thumbnail");
    download(&source, &receiver, original, &dir);
    download(&source, &receiver, thumbnail, &dir);
    mark_v3_original(&receiver, original, Some(thumbnail));
    receiver
        .lock()
        .unwrap()
        .conn
        .execute(
            "INSERT INTO gateway_mms_media_evictions(attachment_id,marked_at,unlinked_at) VALUES(?,?,?)",
            params![thumbnail.to_string(), 1_i64, 1_i64],
        )
        .unwrap();

    super::trim_to_budget_for_test(&receiver, 0).unwrap();
    assert_eq!(
        receiver.attachment_info(thumbnail).unwrap().state,
        AttachmentState::PendingDownload,
        "the fulfilled marker makes the rehydrated preview normal LRU cache content"
    );
}

#[test]
fn carrier_transaction_rehydrates_an_evicted_uploaded_copy() {
    let dir = TempDir::new().unwrap();
    let vault = vault();
    let (_, client) = client(&dir, "carrier-rehydrate", &vault);
    let attachment = prepare(&client, &dir, "carrier.png", b"carrier source");
    client
        .mark_attachment_uploaded(attachment, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    let cipher = client.native_cipher_file(attachment).unwrap();
    client
        .lock()
        .unwrap()
        .conn
        .execute(
            "INSERT INTO gateway_mms_media_evictions(attachment_id,marked_at,unlinked_at) VALUES(?,?,?)",
            params![attachment.to_string(), 1_i64, 1_i64],
        )
        .unwrap();
    fs::remove_file(cipher).unwrap();

    let mut store = client.lock().unwrap();
    let tx = store
        .conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    assert!(super::request_gateway_mms_rehydration_in(&tx, attachment).unwrap());
    tx.commit().unwrap();
    drop(store);

    assert_eq!(
        client.attachment_info(attachment).unwrap().state,
        AttachmentState::PendingDownload
    );
    assert!(client.attachment_download_requested(attachment).unwrap());
}
