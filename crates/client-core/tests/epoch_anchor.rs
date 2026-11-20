use peppy_client_core::{Client, ClientConfig, DatabaseKey, Error};
use peppy_crypto::{KeyProfile, create_vault_check_header, derive_root_key};
use peppy_domain::{DeviceId, VaultId};

#[test]
fn active_epoch_confirmation_obeys_anchor_and_requires_unlocked_keys() {
    let dir = tempfile::tempdir().unwrap();
    let vault_id = VaultId::new();
    let client = Client::open(
        ClientConfig {
            database_path: dir.path().join("confirm.db"),
            vault_id,
            device_id: DeviceId::new(),
        },
        DatabaseKey::new(&[21; 32]).unwrap(),
    )
    .unwrap();
    let profile = KeyProfile::new(vault_id.0, 1).unwrap();
    let phrase = "correct horse battery staple";
    let header =
        create_vault_check_header(&derive_root_key(phrase, &profile).unwrap(), profile.clone())
            .unwrap();
    client.unlock(&profile, &header, phrase).unwrap();
    client.confirm_active_epoch(1).unwrap();
    assert!(client.confirm_active_epoch(2).is_err());
    client
        .set_epoch_anchor(1, &profile.fingerprint().unwrap())
        .unwrap();
    client.confirm_active_epoch(1).unwrap();
    client.set_epoch_anchor(2, &"a".repeat(64)).unwrap();
    assert!(client.confirm_active_epoch(1).is_err());
}

#[test]
fn epoch_anchor_survives_reopen_and_rejects_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let vault_id = VaultId::new();
    let config = ClientConfig {
        database_path: dir.path().join("anchor.db"),
        vault_id,
        device_id: DeviceId::new(),
    };
    let profile = KeyProfile::new(vault_id.0, 1).unwrap();
    let root = derive_root_key("correct horse battery staple", &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    let client = Client::open(config.clone(), DatabaseKey::new(&[21; 32]).unwrap()).unwrap();
    client.set_epoch_anchor(2, &"a".repeat(64)).unwrap();
    drop(client);
    let client = Client::open(config, DatabaseKey::new(&[21; 32]).unwrap()).unwrap();
    assert!(matches!(
        client.set_epoch_anchor(1, &"a".repeat(64)),
        Err(Error::StaleKeyEpoch)
    ));
    assert!(matches!(
        client.unlock(&profile, &header, "correct horse battery staple"),
        Err(Error::StaleKeyEpoch)
    ));
}

#[test]
fn anchor_checks_fingerprint_and_activation() {
    let dir = tempfile::tempdir().unwrap();
    let vault_id = VaultId::new();
    let client = Client::open(
        ClientConfig {
            database_path: dir.path().join("activation.db"),
            vault_id,
            device_id: DeviceId::new(),
        },
        DatabaseKey::new(&[21; 32]).unwrap(),
    )
    .unwrap();
    let p1 = KeyProfile::new(vault_id.0, 1).unwrap();
    let p2 = KeyProfile::new(vault_id.0, 2).unwrap();
    let phrase = "correct horse battery staple";
    let header = |profile: &KeyProfile| {
        create_vault_check_header(&derive_root_key(phrase, profile).unwrap(), profile.clone())
            .unwrap()
    };
    client.unlock(&p1, &header(&p1), phrase).unwrap();
    client.unlock(&p2, &header(&p2), phrase).unwrap();
    client.set_epoch_anchor(2, &"a".repeat(64)).unwrap();
    assert!(matches!(
        client.activate_epoch(2),
        Err(Error::StaleKeyEpoch)
    ));
    assert!(matches!(
        client.unlock(&p2, &header(&p2), phrase),
        Err(Error::StaleKeyEpoch)
    ));
    assert!(matches!(
        client.set_epoch_anchor(2, &p2.fingerprint().unwrap()),
        Err(Error::StaleKeyEpoch)
    ));
}

#[test]
fn historical_unlock_remains_available_after_anchor() {
    let dir = tempfile::tempdir().unwrap();
    let vault_id = VaultId::new();
    let client = Client::open(
        ClientConfig {
            database_path: dir.path().join("history.db"),
            vault_id,
            device_id: DeviceId::new(),
        },
        DatabaseKey::new(&[21; 32]).unwrap(),
    )
    .unwrap();
    let phrase = "correct horse battery staple";
    let newer = KeyProfile::new(vault_id.0, 2).unwrap();
    let older = KeyProfile::new(vault_id.0, 1).unwrap();
    let header = |p: &KeyProfile| {
        create_vault_check_header(&derive_root_key(phrase, p).unwrap(), p.clone()).unwrap()
    };
    client
        .set_epoch_anchor(2, &newer.fingerprint().unwrap())
        .unwrap();
    client.unlock(&newer, &header(&newer), phrase).unwrap();
    client.unlock(&older, &header(&older), phrase).unwrap();
    assert_eq!(client.key_status().unwrap().active_epoch, Some(2));
    assert!(client.key_status().unwrap().unlocked_epochs.contains(&1));
}

#[test]
fn setting_anchor_locks_stale_active_and_refuses_cache_restore() {
    let dir = tempfile::tempdir().unwrap();
    let vault_id = VaultId::new();
    let config = ClientConfig {
        database_path: dir.path().join("cache.db"),
        vault_id,
        device_id: DeviceId::new(),
    };
    let client = Client::open(config.clone(), DatabaseKey::new(&[21; 32]).unwrap()).unwrap();
    let old = KeyProfile::new(vault_id.0, 1).unwrap();
    let new = KeyProfile::new(vault_id.0, 2).unwrap();
    let phrase = "correct horse battery staple";
    let header = |p: &KeyProfile| {
        create_vault_check_header(&derive_root_key(phrase, p).unwrap(), p.clone()).unwrap()
    };
    client.unlock(&old, &header(&old), phrase).unwrap();
    let cache = client.export_native_key_cache(1).unwrap();
    client
        .set_epoch_anchor(2, &new.fingerprint().unwrap())
        .unwrap();
    assert!(!client.key_status().unwrap().unlocked_epochs.contains(&1));
    assert!(matches!(
        client.import_native_key_cache(&cache),
        Err(Error::StaleKeyEpoch)
    ));
    assert!(matches!(
        client.unlock(&old, &header(&old), phrase),
        Err(Error::StaleKeyEpoch)
    ));
    drop(client);
    let client = Client::open(config, DatabaseKey::new(&[21; 32]).unwrap()).unwrap();
    assert!(matches!(
        client.import_native_key_cache(&cache),
        Err(Error::StaleKeyEpoch)
    ));
    client.unlock(&new, &header(&new), phrase).unwrap();
    client.activate_epoch(2).unwrap();
    assert_eq!(client.key_status().unwrap().active_epoch, Some(2));
    client.unlock(&old, &header(&old), phrase).unwrap();
    assert!(client.key_status().unwrap().unlocked_epochs.contains(&1));
}
