//! Activation, unlock, and session-lifetime regressions for the native owner-claim path.
use crate::{
    AppState,
    contacts::tests::mock_server,
    credentials::{parse_credential, tests::TOKEN, tests::credential_json},
    secure_store::{BundledStore, MemoryStore},
    session::VaultSummary,
    tests::{PHRASE, noop},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use peppy_client_core::KeyProfile;
use peppy_crypto::{create_vault_check_header, derive_root_key};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use std::time::Duration;
use zeroize::Zeroizing;

fn snapshot(session: &crate::session::Session, origin: &str) -> crate::dto::Snapshot {
    session
        .snapshot(
            None,
            None,
            crate::head(),
            Some(origin.to_owned()),
            crate::notifications::NotificationPreferences::default(),
        )
        .unwrap()
        .0
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claim_unlock_survives_stale_session_race_reopen_and_live_rounds() {
    let vault_id = uuid::Uuid::new_v4();
    let device_id = uuid::Uuid::new_v4();
    let profile = KeyProfile::new(vault_id, 1).unwrap();
    let root_key = derive_root_key(PHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root_key, profile.clone()).unwrap();
    let fingerprint = profile.fingerprint().unwrap();
    let profile_json = serde_json::to_value(&profile).unwrap();
    let header_base64 = STANDARD.encode(serde_json::to_vec(&header).unwrap());
    let server_epoch = Arc::new(AtomicU32::new(1));
    let route_epoch = server_epoch.clone();
    let route_profile = profile_json.clone();
    let route_fingerprint = fingerprint.clone();
    let route_header = header_base64.clone();
    let (origin, _seen) = mock_server(move |method, path, _| match (method, path) {
        ("GET", "/v1/vault") => {
            let mut public_key_profile = route_profile.clone();
            public_key_profile["key_epoch"] = json!(route_epoch.load(Ordering::SeqCst));
            (
                200,
                json!({
                    "vault_id": vault_id,
                    "device_id": device_id,
                    "public_key_profile": public_key_profile,
                    "encrypted_vault_check_header": route_header,
                    "profile_fingerprint": route_fingerprint,
                })
                .to_string(),
            )
        }
        ("GET", "/v1/devices") => (200, json!({"devices": []}).to_string()),
        ("GET", "/v1/capabilities") => (200, json!({"devices": []}).to_string()),
        ("GET", "/v1/snapshot") => (
            200,
            json!({
                "high_water_cursor": "0",
                "record_count": "0",
                "compaction_supported": false,
            })
            .to_string(),
        ),
        ("GET", path) if path.starts_with("/v1/events") => (
            200,
            json!({
                "high_water_cursor": "0",
                "replay_floor_cursor": "0",
                "next_after": null,
                "events": [],
            })
            .to_string(),
        ),
        _ => (404, json!({"code": "not_found"}).to_string()),
    })
    .await;

    let root = tempfile::tempdir().unwrap();
    let state = Arc::new(AppState::new(
        root.path().to_path_buf(),
        Arc::new(BundledStore::new(
            MemoryStore::default(),
            root.path().join("secrets.lock"),
        )),
        noop(),
    ));
    state
        .update_config(|config| config.select_origin(&origin))
        .unwrap();
    let credential = Arc::new(
        parse_credential(&credential_json(
            &origin,
            &vault_id.to_string(),
            &device_id.to_string(),
            TOKEN,
        ))
        .unwrap(),
    );

    // Force a session() caller to read the pre-activation config and wait on the session mutex.
    let guard = state.session.lock().await;
    let stale_state = state.clone();
    let stale_session = tokio::spawn(async move { stale_state.session().await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let activation_state = state.clone();
    let activation_credential = credential.clone();
    let activation = tokio::spawn(async move {
        crate::verify_and_activate_test_credential(&activation_state, activation_credential).await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    drop(guard);
    assert!(stale_session.await.unwrap().unwrap().is_none());
    activation.await.unwrap().unwrap();

    let session = state.session().await.unwrap().unwrap();
    session.cancel.cancel();
    let recovered = state.session().await.unwrap().unwrap();
    assert!(!Arc::ptr_eq(&session, &recovered));
    assert!(!recovered.cancel.is_cancelled());
    let session = recovered;
    crate::unlock_with(
        &state,
        &session,
        profile,
        header,
        Zeroizing::new(PHRASE.to_owned()),
        fingerprint.clone(),
    )
    .await
    .unwrap();

    for _ in 0..30 {
        let keys = session.client.key_status().unwrap();
        assert_eq!(keys.active_epoch, Some(1));
        assert!(keys.unlocked_epochs.contains(&1));
        assert_eq!(snapshot(&session, &origin).encryption.state, "unlocked");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let config = state.config().unwrap();
    assert_eq!(
        config.known(&session.binding).unwrap().cached_epochs,
        vec![1]
    );
    assert!(
        state
            .store
            .get(&session.binding.key_cache_account(1))
            .unwrap()
            .is_some()
    );

    // A normal close/reopen must recover the valid native cache.
    state.close_session().await;
    drop(session);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let reopened = state.session().await.unwrap().unwrap();
    let reopened_keys = reopened.client.key_status().unwrap();
    assert_eq!(reopened_keys.active_epoch, Some(1));
    assert!(reopened_keys.unlocked_epochs.contains(&1));
    assert_eq!(snapshot(&reopened, &origin).encryption.state, "unlocked");

    // Exercise refresh_vault through the real live loop. A newer server epoch does not remove
    // keys, but snapshot deliberately reports locked because local active_epoch is behind.
    server_epoch.store(2, Ordering::SeqCst);
    reopened.reconnect_wake.notify_one();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if reopened
                .status
                .lock()
                .unwrap()
                .vault
                .as_ref()
                .is_some_and(|vault| vault.epoch == 2)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("live refresh did not observe epoch 2");
    let after_refresh = reopened.client.key_status().unwrap();
    assert_eq!(after_refresh.active_epoch, Some(1));
    assert!(after_refresh.unlocked_epochs.contains(&1));
    reopened.set_status(|status| status.live = true);
    let behind = snapshot(&reopened, &origin);
    assert_eq!(behind.connection.state, "connected");
    assert_eq!(behind.encryption.state, "locked");

    reopened.set_status(|status| {
        status.vault = Some(VaultSummary {
            epoch: 1,
            fingerprint,
        });
    });
    assert_eq!(snapshot(&reopened, &origin).encryption.state, "unlocked");
    state.close_session().await;
}

/// The renderer only refreshes its snapshot on a state event. A claim notifies before the
/// unlock, so the unlock itself must notify or the UI keeps showing "not unlocked".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unlock_with_emits_a_state_event_after_the_keys_are_installed() {
    let vault_id = uuid::Uuid::new_v4();
    let device_id = uuid::Uuid::new_v4();
    let profile = KeyProfile::new(vault_id, 1).unwrap();
    let root_key = derive_root_key(PHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root_key, profile.clone()).unwrap();
    let fingerprint = profile.fingerprint().unwrap();
    let profile_json = serde_json::to_value(&profile).unwrap();
    let header_base64 = STANDARD.encode(serde_json::to_vec(&header).unwrap());
    let route_fingerprint = fingerprint.clone();
    let (origin, _seen) = mock_server(move |method, path, _| match (method, path) {
        ("GET", "/v1/vault") => (
            200,
            json!({
                "vault_id": vault_id,
                "device_id": device_id,
                "public_key_profile": profile_json,
                "encrypted_vault_check_header": header_base64,
                "profile_fingerprint": route_fingerprint,
            })
            .to_string(),
        ),
        _ => (404, json!({"code": "not_found"}).to_string()),
    })
    .await;

    let notifications = Arc::new(AtomicU32::new(0));
    let counter = notifications.clone();
    let root = tempfile::tempdir().unwrap();
    let state = Arc::new(AppState::new(
        root.path().to_path_buf(),
        Arc::new(BundledStore::new(
            MemoryStore::default(),
            root.path().join("secrets.lock"),
        )),
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    ));
    state
        .update_config(|config| config.select_origin(&origin))
        .unwrap();
    let credential = Arc::new(
        parse_credential(&credential_json(
            &origin,
            &vault_id.to_string(),
            &device_id.to_string(),
            TOKEN,
        ))
        .unwrap(),
    );
    crate::verify_and_activate_test_credential(&state, credential)
        .await
        .unwrap();
    let session = state.session().await.unwrap().unwrap();
    // Stop the background sync tasks so only the unlock itself can notify.
    session.cancel.cancel();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = notifications.load(Ordering::SeqCst);

    crate::unlock_with(
        &state,
        &session,
        profile,
        header,
        Zeroizing::new(PHRASE.to_owned()),
        fingerprint,
    )
    .await
    .unwrap();

    assert_eq!(snapshot(&session, &origin).encryption.state, "unlocked");
    assert_eq!(
        notifications.load(Ordering::SeqCst),
        before + 1,
        "unlocking must publish exactly one state event"
    );
    state.close_session().await;
}

#[test]
fn reopened_session_only_stays_locked_when_cached_material_is_missing_or_invalid() {
    use crate::secure_store::SecretStore as _;
    use crate::tests::{fixture_at, open};

    let fixture = fixture_at("http://127.0.0.1:9");
    let cache = {
        let first = open(&fixture, &fixture.binding, &[]);
        first
            .client
            .unlock(&fixture.profile, &fixture.header, PHRASE)
            .unwrap();
        first.client.export_native_key_cache(1).unwrap()
    };
    fixture
        .store
        .set(
            &fixture.binding.key_cache_account(1),
            cache.native_storage_bytes(),
        )
        .unwrap();
    let valid = open(&fixture, &fixture.binding, &[1]);
    assert_eq!(valid.client.key_status().unwrap().unlocked_epochs, vec![1]);
    drop(valid);

    fixture
        .store
        .set(&fixture.binding.key_cache_account(1), b"corrupt-cache")
        .unwrap();
    let invalid = open(&fixture, &fixture.binding, &[1]);
    let keys = invalid.client.key_status().unwrap();
    assert_eq!(keys.active_epoch, Some(1));
    assert!(keys.unlocked_epochs.is_empty());
    assert_eq!(
        snapshot(&invalid, "http://127.0.0.1:9").encryption.state,
        "locked"
    );
}
