//! Encrypted remote contact-scan requests are durable, capability-gated and fenced to a new scan.
use crate::common::*;
use serde_json::{Value, json};
use tempfile::TempDir;

fn value(result: Result<String, Error>) -> Value {
    serde_json::from_str(&result.unwrap()).unwrap()
}

#[test]
fn fresh_no_change_scan_completes_an_encrypted_peer_request() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner", &vault);
    let owner = unlocked(&owner_config, &vault);
    let requester = unlocked(&config(&dir, "requester", &vault), &vault);
    let book = json!({"id":"book","owner_device_id":owner_config.device_id.to_string(),"generation":"1","state":"active","capabilities":{"read":true,"write":true},"policy":{"remote_edits":"auto"}});
    owner
        .capture_contact_book(&json!({"schema_version":1,"book":book,"contacts":[]}).to_string())
        .unwrap();
    value(owner.set_contact_scan_request_capability(
        &json!({"book_id":"book","enabled":true}).to_string(),
    ));
    let mut server = Server::default();
    server.upload(&owner);
    assert_eq!(server.sync(&requester).quarantined, 0);

    let request = value(requester.request_contact_sync(&json!({"book_id":"book"}).to_string()));
    assert_eq!(
        value(requester.contact_sync_request_status(&json!({"book_id":"book"}).to_string()))["state"],
        "queued"
    );
    server.upload(&requester);
    assert_eq!(
        value(requester.contact_sync_request_status(&json!({"book_id":"book"}).to_string()))["state"],
        "waiting"
    );
    assert_eq!(server.sync(&owner).quarantined, 0);
    assert_eq!(
        value(owner.pending_contact_sync_scans(&json!({"book_id":"book"}).to_string()))["requests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    owner.begin_contact_scan(&json!({"schema_version":1,"scan_id":"fresh","book_id":"book","generation":"1","access":"full","authoritative":true}).to_string()).unwrap();
    let finished = value(owner.finish_contact_scan(
        &json!({"schema_version":1,"scan_id":"fresh","complete":true}).to_string(),
    ));
    assert_eq!(finished["status"], "finished");
    server.upload(&owner);
    assert_eq!(server.sync(&requester).quarantined, 0);
    let status =
        value(requester.contact_sync_request_status(&json!({"book_id":"book"}).to_string()));
    assert_eq!(status["requestId"], request["requestId"]);
    assert_eq!(status["state"], "completed");
}

#[test]
fn scan_request_capability_is_required() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner", &vault);
    let owner = unlocked(&owner_config, &vault);
    let peer = unlocked(&config(&dir, "peer", &vault), &vault);
    owner.capture_contact_book(&json!({"schema_version":1,"book":{"id":"book","owner_device_id":owner_config.device_id.to_string(),"generation":"1","state":"active","capabilities":{"read":true},"policy":{"remote_edits":"auto"}},"contacts":[]}).to_string()).unwrap();
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&peer);
    assert!(
        peer.request_contact_sync(&json!({"book_id":"book"}).to_string())
            .is_err()
    );
}

#[test]
fn forgotten_peer_does_not_wedge_a_following_scan_request() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner", &vault);
    let owner = unlocked(&owner_config, &vault);
    let requester = unlocked(&config(&dir, "requester", &vault), &vault);
    let forgotten = unlocked(&config(&dir, "forgotten", &vault), &vault);
    let book = json!({"id":"book","owner_device_id":owner_config.device_id.to_string(),"generation":"1","state":"active","capabilities":{"read":true},"policy":{"remote_edits":"auto"}});
    owner
        .capture_contact_book(&json!({"schema_version":1,"book":book,"contacts":[]}).to_string())
        .unwrap();
    value(owner.set_contact_scan_request_capability(
        &json!({"book_id":"book","enabled":true}).to_string(),
    ));
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&requester);
    server.sync(&forgotten);
    forgotten
        .forget_contact_book(&json!({"book_id":"book"}).to_string())
        .unwrap();

    value(requester.request_contact_sync(&json!({"book_id":"book"}).to_string()));
    server.upload(&requester);
    assert_eq!(server.sync(&forgotten).quarantined, 0);
    assert_eq!(server.sync(&owner).quarantined, 0);
    assert_eq!(
        value(owner.pending_contact_sync_scans(&json!({"book_id":"book"}).to_string()))["requests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

fn enabled_book(owner: &Client, owner_config: &ClientConfig, contacts: Vec<Value>) {
    let book = json!({"id":"book","owner_device_id":owner_config.device_id.to_string(),"generation":"1","state":"active","capabilities":{"read":true,"write":true},"policy":{"remote_edits":"auto"}});
    owner
        .capture_contact_book(
            &json!({"schema_version":1,"book":book,"contacts":contacts}).to_string(),
        )
        .unwrap();
    value(owner.set_contact_scan_request_capability(
        &json!({"book_id":"book","enabled":true}).to_string(),
    ));
}

#[test]
fn scanning_progress_is_visible_before_terminal_completion() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner-progress", &vault);
    let owner = unlocked(&owner_config, &vault);
    let requester = unlocked(&config(&dir, "requester-progress", &vault), &vault);
    enabled_book(&owner, &owner_config, vec![]);
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&requester);
    value(requester.request_contact_sync(r#"{"book_id":"book"}"#));
    server.upload(&requester);
    server.sync(&owner);
    owner.begin_contact_scan(r#"{"schema_version":1,"scan_id":"progress","book_id":"book","generation":"1","access":"full","authoritative":true}"#).unwrap();
    server.upload(&owner);
    server.sync(&requester);
    assert_eq!(
        value(requester.contact_sync_request_status(r#"{"book_id":"book"}"#))["state"],
        "scanning"
    );
}

#[test]
fn request_accepted_after_scan_begin_waits_for_the_next_scan() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner-fence", &vault);
    let owner = unlocked(&owner_config, &vault);
    let first = unlocked(&config(&dir, "first-fence", &vault), &vault);
    let second = unlocked(&config(&dir, "second-fence", &vault), &vault);
    enabled_book(&owner, &owner_config, vec![]);
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&first);
    server.sync(&second);
    value(first.request_contact_sync(r#"{"book_id":"book"}"#));
    server.upload(&first);
    server.sync(&owner);
    owner.begin_contact_scan(r#"{"schema_version":1,"scan_id":"fenced","book_id":"book","generation":"1","access":"full","authoritative":true}"#).unwrap();
    value(second.request_contact_sync(r#"{"book_id":"book"}"#));
    server.upload(&second);
    server.sync(&owner);
    owner
        .finish_contact_scan(r#"{"schema_version":1,"scan_id":"fenced","complete":true}"#)
        .unwrap();
    server.upload(&owner);
    server.sync(&first);
    server.sync(&second);
    assert_eq!(
        value(first.contact_sync_request_status(r#"{"book_id":"book"}"#))["state"],
        "completed"
    );
    assert_eq!(
        value(second.contact_sync_request_status(r#"{"book_id":"book"}"#))["state"],
        "waiting"
    );
}

#[test]
fn orphaned_scanning_work_is_exposed_and_reclaimed_by_a_fresh_scan() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner-restart", &vault);
    let owner = unlocked(&owner_config, &vault);
    let requester = unlocked(&config(&dir, "requester-restart", &vault), &vault);
    enabled_book(&owner, &owner_config, vec![]);
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&requester);
    value(requester.request_contact_sync(r#"{"book_id":"book"}"#));
    server.upload(&requester);
    server.sync(&owner);
    owner.begin_contact_scan(r#"{"schema_version":1,"scan_id":"abandoned","book_id":"book","generation":"1","access":"full","authoritative":true}"#).unwrap();
    let pending = value(owner.pending_contact_sync_scans(r#"{"book_id":"book"}"#));
    assert_eq!(pending["requests"][0]["state"], "scanning");
    owner.begin_contact_scan(r#"{"schema_version":1,"scan_id":"restarted","book_id":"book","generation":"1","access":"full","authoritative":true}"#).unwrap();
    owner
        .finish_contact_scan(r#"{"schema_version":1,"scan_id":"restarted","complete":true}"#)
        .unwrap();
    server.upload(&owner);
    server.sync(&requester);
    assert_eq!(
        value(requester.contact_sync_request_status(r#"{"book_id":"book"}"#))["state"],
        "completed"
    );
}

#[test]
fn disabling_after_acceptance_publishes_sync_disabled() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner-disabled", &vault);
    let owner = unlocked(&owner_config, &vault);
    let requester = unlocked(&config(&dir, "requester-disabled", &vault), &vault);
    enabled_book(&owner, &owner_config, vec![]);
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&requester);
    value(requester.request_contact_sync(r#"{"book_id":"book"}"#));
    server.upload(&requester);
    server.sync(&owner);
    value(owner.set_contact_scan_request_capability(r#"{"book_id":"book","enabled":false}"#));
    server.upload(&owner);
    server.sync(&requester);
    let status = value(requester.contact_sync_request_status(r#"{"book_id":"book"}"#));
    assert_eq!(
        (status["state"].as_str(), status["reason"].as_str()),
        (Some("blocked"), Some("sync_disabled"))
    );
}

#[test]
fn incomplete_scan_publishes_blocked_and_terminal_compaction_pairs_request() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner-incomplete", &vault);
    let owner = unlocked(&owner_config, &vault);
    let requester_config = config(&dir, "requester-incomplete", &vault);
    let requester = unlocked(&requester_config, &vault);
    enabled_book(&owner, &owner_config, vec![]);
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&requester);
    value(requester.request_contact_sync(r#"{"book_id":"book"}"#));
    server.upload(&requester);
    server.sync(&owner);
    owner.begin_contact_scan(r#"{"schema_version":1,"scan_id":"partial","book_id":"book","generation":"1","access":"full","authoritative":true}"#).unwrap();
    owner
        .finish_contact_scan(r#"{"schema_version":1,"scan_id":"partial","complete":false}"#)
        .unwrap();
    server.upload(&owner);
    server.sync(&requester);
    let status = value(requester.contact_sync_request_status(r#"{"book_id":"book"}"#));
    assert_eq!(
        (status["state"].as_str(), status["reason"].as_str()),
        (Some("blocked"), Some("incomplete_scan"))
    );
    let terminal = server
        .log
        .iter()
        .rev()
        .find_map(|envelope| {
            envelope
                .compaction
                .as_ref()
                .filter(|metadata| metadata.terminal)
        })
        .unwrap();
    assert!(
        terminal
            .supersedes
            .iter()
            .any(|reference| { reference.producer_device_id == owner_config.device_id })
    );
    assert!(
        server.log.iter().any(
            |envelope| envelope.compaction.as_ref().is_some_and(|metadata| {
                !metadata.terminal
                    && metadata
                        .supersedes
                        .iter()
                        .any(|reference| reference.producer_device_id == requester_config.device_id)
            })
        )
    );
}

#[test]
fn retired_owner_state_rejects_waiting_work_with_a_terminal_result() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner-retired", &vault);
    let owner = unlocked(&owner_config, &vault);
    let requester = unlocked(&config(&dir, "requester-retired", &vault), &vault);
    enabled_book(&owner, &owner_config, vec![]);
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&requester);
    value(requester.request_contact_sync(r#"{"book_id":"book"}"#));
    server.upload(&requester);
    server.sync(&owner);
    let retired = json!({"id":"book","owner_device_id":owner_config.device_id.to_string(),"generation":"1","state":"retired","capabilities":{"read":true,"write":true,"scan_request":true},"policy":{"remote_edits":"auto"}});
    owner
        .capture_contact_book(&json!({"schema_version":1,"book":retired,"contacts":[]}).to_string())
        .unwrap();
    owner.begin_contact_scan(r#"{"schema_version":1,"scan_id":"retired-race","book_id":"book","generation":"1","access":"full","authoritative":true}"#).unwrap();
    server.upload(&owner);
    server.sync(&requester);
    let status = value(requester.contact_sync_request_status(r#"{"book_id":"book"}"#));
    assert_eq!(
        (status["state"].as_str(), status["reason"].as_str()),
        (Some("blocked"), Some("source_retired"))
    );
}

#[test]
fn photo_held_contact_event_keeps_later_scan_result_out_of_upload() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner-photo", &vault);
    let owner = unlocked(&owner_config, &vault);
    let requester = unlocked(&config(&dir, "requester-photo", &vault), &vault);
    enabled_book(&owner, &owner_config, vec![]);
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&requester);
    value(requester.request_contact_sync(r#"{"book_id":"book"}"#));
    server.upload(&requester);
    server.sync(&owner);
    owner.begin_contact_scan(r#"{"schema_version":1,"scan_id":"photo-scan","book_id":"book","generation":"1","access":"full","authoritative":true}"#).unwrap();
    server.upload(&owner);
    let path = dir.path().join("held.png");
    image::RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3]))
        .save(&path)
        .unwrap();
    let attachment = owner.prepare_contact_photo(&path).unwrap().attachment_id;
    let fields = json!({"display_name":"Photo","name":{"given":"Photo"},"phones":[],"emails":[],"addresses":[],"photo":{"attachment_id":attachment.to_string()}});
    let book = json!({"id":"book","owner_device_id":owner_config.device_id.to_string(),"generation":"1","state":"active","capabilities":{"read":true,"write":true,"scan_request":true},"policy":{"remote_edits":"auto"}});
    owner.capture_platform_contacts_json(&json!({"schema_version":1,"book":book,"contacts":[{"source_key":"photo","fields":fields}]}).to_string()).unwrap();
    owner
        .finish_contact_scan(r#"{"schema_version":1,"scan_id":"photo-scan","complete":true}"#)
        .unwrap();
    server.upload(&owner);
    assert!(!server.log.iter().any(|envelope| {
        envelope.producer_device_id == owner_config.device_id
            && envelope
                .compaction
                .as_ref()
                .is_some_and(|metadata| metadata.terminal)
    }));
    owner
        .mark_attachment_uploaded(attachment, &uuid::Uuid::new_v4().to_string())
        .unwrap();
    server.upload(&owner);
    assert!(server.log.iter().any(|envelope| {
        envelope.producer_device_id == owner_config.device_id
            && envelope
                .compaction
                .as_ref()
                .is_some_and(|metadata| metadata.terminal)
    }));
}

#[test]
fn held_large_deletion_scan_reports_incomplete_instead_of_completion() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner-held", &vault);
    let owner = unlocked(&owner_config, &vault);
    let requester = unlocked(&config(&dir, "requester-held", &vault), &vault);
    let contacts: Vec<Value> = (0..11).map(|index| json!({
        "id":format!("contact-{index}"),"book_id":"book","revision":"1","display_name":format!("Contact {index}"),
        "name":{"given":"Contact"},"phones":[],"emails":[],"addresses":[]
    })).collect();
    enabled_book(&owner, &owner_config, contacts);
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&requester);
    value(requester.request_contact_sync(r#"{"book_id":"book"}"#));
    server.upload(&requester);
    server.sync(&owner);
    owner.begin_contact_scan(r#"{"schema_version":1,"scan_id":"held","book_id":"book","generation":"1","access":"full","authoritative":true}"#).unwrap();
    let finish = value(
        owner.finish_contact_scan(r#"{"schema_version":1,"scan_id":"held","complete":true}"#),
    );
    assert_eq!(finish["held"], 11);
    server.upload(&owner);
    server.sync(&requester);
    let status = value(requester.contact_sync_request_status(r#"{"book_id":"book"}"#));
    assert_eq!(
        (status["state"].as_str(), status["reason"].as_str()),
        (Some("blocked"), Some("incomplete_scan"))
    );
}

#[test]
fn superseded_generation_is_returned_as_terminal_blocked() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner-generation", &vault);
    let owner = unlocked(&owner_config, &vault);
    let requester = unlocked(&config(&dir, "requester-generation", &vault), &vault);
    enabled_book(&owner, &owner_config, vec![]);
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&requester);
    value(requester.request_contact_sync(r#"{"book_id":"book"}"#));
    server.upload(&requester);
    let newer = json!({"id":"book","owner_device_id":owner_config.device_id.to_string(),"generation":"2","state":"active","capabilities":{"read":true,"write":true,"scan_request":true},"policy":{"remote_edits":"auto"}});
    owner
        .capture_contact_book(&json!({"schema_version":1,"book":newer,"contacts":[]}).to_string())
        .unwrap();
    server.sync(&owner);
    assert!(
        value(owner.pending_contact_sync_scans(r#"{"book_id":"book"}"#))["requests"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    server.upload(&owner);
    server.sync(&requester);
    let status = value(requester.contact_sync_request_status(r#"{"book_id":"book"}"#));
    assert_eq!(
        (status["state"].as_str(), status["reason"].as_str()),
        (Some("blocked"), Some("superseded_generation"))
    );
}

#[test]
fn pre_cutover_request_epoch_receives_stale_result_sealed_by_new_epoch() {
    let dir = TempDir::new().unwrap();
    let epoch1 = Vault::new();
    let epoch2 = Vault::epoch(epoch1.id, 2);
    let owner_config = config(&dir, "owner-epoch", &epoch1);
    let owner = unlocked(&owner_config, &epoch1);
    let requester = unlocked(&config(&dir, "requester-epoch", &epoch1), &epoch1);
    enabled_book(&owner, &owner_config, vec![]);
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&requester);
    value(requester.request_contact_sync(r#"{"book_id":"book"}"#));
    server.upload(&requester);
    for client in [&owner, &requester] {
        client
            .unlock(&epoch2.profile, &epoch2.header, PASSPHRASE)
            .unwrap();
        client.activate_epoch(2).unwrap();
    }
    server.sync(&owner);
    assert!(
        value(owner.pending_contact_sync_scans(r#"{"book_id":"book"}"#))["requests"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    server.upload(&owner);
    server.sync(&requester);
    let status = value(requester.contact_sync_request_status(r#"{"book_id":"book"}"#));
    assert_eq!(
        (status["state"].as_str(), status["reason"].as_str()),
        (Some("blocked"), Some("stale_epoch"))
    );
}

#[test]
fn delayed_request_received_after_capability_removal_returns_unsupported() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner-unsupported", &vault);
    let owner = unlocked(&owner_config, &vault);
    let requester = unlocked(&config(&dir, "requester-unsupported", &vault), &vault);
    enabled_book(&owner, &owner_config, vec![]);
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&requester);
    value(requester.request_contact_sync(r#"{"book_id":"book"}"#));
    server.upload(&requester);
    value(owner.set_contact_scan_request_capability(r#"{"book_id":"book","enabled":false}"#));
    server.sync(&owner);
    assert!(
        value(owner.pending_contact_sync_scans(r#"{"book_id":"book"}"#))["requests"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    server.upload(&owner);
    server.sync(&requester);
    let status = value(requester.contact_sync_request_status(r#"{"book_id":"book"}"#));
    assert_eq!(
        (status["state"].as_str(), status["reason"].as_str()),
        (Some("blocked"), Some("unsupported"))
    );
}

#[test]
fn failed_pending_rejection_enqueue_rolls_back_and_retries_terminal_publication() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let owner_config = config(&dir, "owner-atomic", &vault);
    let owner = unlocked(&owner_config, &vault);
    let requester = unlocked(&config(&dir, "requester-atomic", &vault), &vault);
    enabled_book(&owner, &owner_config, vec![]);
    let mut server = Server::default();
    server.upload(&owner);
    server.sync(&requester);

    value(requester.request_contact_sync(r#"{"book_id":"book"}"#));
    server.upload(&requester);
    let newer = json!({"id":"book","owner_device_id":owner_config.device_id.to_string(),"generation":"2","state":"active","capabilities":{"read":true,"write":true,"scan_request":true},"policy":{"remote_edits":"auto"}});
    owner
        .capture_contact_book(&json!({"schema_version":1,"book":newer,"contacts":[]}).to_string())
        .unwrap();
    server.sync(&owner);

    owner.set_server_compaction_state(false, false).unwrap();
    assert!(
        owner
            .pending_contact_sync_scans(r#"{"book_id":"book"}"#)
            .is_err()
    );
    owner.set_server_compaction_state(true, true).unwrap();
    assert!(
        value(owner.pending_contact_sync_scans(r#"{"book_id":"book"}"#))["requests"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    server.upload(&owner);
    server.sync(&requester);
    let status = value(requester.contact_sync_request_status(r#"{"book_id":"book"}"#));
    assert_eq!(
        (status["state"].as_str(), status["reason"].as_str()),
        (Some("blocked"), Some("superseded_generation"))
    );
}
