//! SQLCipher-backed notification lifecycle regression coverage.
mod common;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use common::*;
use tempfile::TempDir;

fn capture(key: &str, instance: &str, text: &str) -> NotificationCapture {
    NotificationCapture {
        notification_key: key.into(),
        instance: instance.into(),
        package_name: "com.example.mail".into(),
        app_name: "Mail".into(),
        app_icon_png_base64: None,
        title: "Subject".into(),
        text: text.into(),
        category: None,
        posted_at: 42,
        dismissible: true,
    }
}

#[test]
fn notification_capture_serializes_optional_app_icon() {
    let value = serde_json::to_value(capture("icon", "one", "body")).unwrap();
    assert_eq!(value["appIconPngBase64"], serde_json::Value::Null);
}

#[test]
fn legacy_notification_capture_without_icon_deserializes() {
    let mut value = serde_json::to_value(capture("icon", "one", "body")).unwrap();
    value.as_object_mut().unwrap().remove("appIconPngBase64");
    assert_eq!(
        serde_json::from_value::<NotificationCapture>(value)
            .unwrap()
            .app_icon_png_base64,
        None
    );
}

#[test]
fn notification_icons_require_a_32px_png() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "icon-phone", &vault), &vault);
    let mut valid = capture("icon", "one", "body");
    valid.app_icon_png_base64 = Some("iVBORw0KGgoAAAANSUhEUgAAACAAAAAg".into());
    assert_eq!(
        client.capture_notification(valid).unwrap(),
        NotificationCaptureOutcome::Captured
    );
    let mut invalid = capture("bad-icon", "one", "body");
    invalid.app_icon_png_base64 = Some("not an image".into());
    assert!(matches!(
        client.capture_notification(invalid),
        Err(Error::InvalidRequest(_))
    ));
    let valid_png_32x32_header = [
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 32, 0, 0, 0, 32,
    ];
    let decoded_oversize = [
        valid_png_32x32_header.as_slice(),
        &vec![0; 8_193 - valid_png_32x32_header.len()],
    ]
    .concat();
    let encoded_oversize = STANDARD.encode(decoded_oversize);
    assert_eq!(encoded_oversize.len(), 10_924);
    for (key, icon) in [
        ("bad-padding", "iVBORw0KGgoAAAANSUhEUgAAACAAAAA=".to_owned()),
        ("too-large-encoded", "A".repeat(10_925)),
        ("too-large-decoded", encoded_oversize),
        (
            "wrong-dimensions",
            STANDARD.encode([
                137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 32, 0, 0, 0,
                33,
            ]),
        ),
    ] {
        let mut invalid = capture(key, "one", "body");
        invalid.app_icon_png_base64 = Some(icon);
        assert!(matches!(
            client.capture_notification(invalid),
            Err(Error::InvalidRequest(_))
        ));
    }
    let canonical = STANDARD.encode([valid_png_32x32_header.as_slice(), &[0]].concat());
    assert_eq!(STANDARD.decode(&canonical).unwrap().len(), 25);
    let noncanonical = format!("{}B==", &canonical[..canonical.len() - 3]);
    let mut invalid = capture("noncanonical-trailing-bits", "one", "body");
    invalid.app_icon_png_base64 = Some(noncanonical);
    assert!(matches!(
        client.capture_notification(invalid),
        Err(Error::InvalidRequest(_))
    ));
}

#[test]
fn snapshots_use_phone_time_and_deterministic_identity_ties() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let first_phone_cfg = config(&dir, "ordered-first-phone", &vault);
    let second_phone_cfg = config(&dir, "ordered-second-phone", &vault);
    let desktop = unlocked(&config(&dir, "ordered-desktop", &vault), &vault);
    let first_phone = unlocked(&first_phone_cfg, &vault);
    let second_phone = unlocked(&second_phone_cfg, &vault);
    let mut server = Server::default();
    for (phone, key, posted_at) in [
        (&first_phone, "later", 200),
        (&second_phone, "z-tie", 100),
        (&first_phone, "same-key", 100),
        (&second_phone, "same-key", 100),
    ] {
        let mut item = capture(key, key, key);
        item.posted_at = posted_at;
        phone.capture_notification(item).unwrap();
    }
    server.upload(&first_phone);
    server.upload(&second_phone);
    server.sync(&desktop);
    let identities: Vec<_> = desktop
        .notification_snapshot()
        .unwrap()
        .notifications
        .into_iter()
        .map(|item| (item.target.source_device_id, item.target.notification_key))
        .collect();
    assert_eq!(identities[0].1, "later");
    let mut expected = vec![
        (first_phone_cfg.device_id.to_string(), "same-key".into()),
        (second_phone_cfg.device_id.to_string(), "same-key".into()),
        (second_phone_cfg.device_id.to_string(), "z-tie".into()),
    ];
    expected.sort();
    assert_eq!(
        identities[1..],
        expected,
        "same timestamps remain distinct across phones and order by source then key"
    );
}

#[test]
fn bulk_dismiss_excludes_muted_non_dismissible_and_pending_rows() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone_cfg = config(&dir, "bulk-phone", &vault);
    let desktop = unlocked(&config(&dir, "bulk-desktop", &vault), &vault);
    let phone = unlocked(&phone_cfg, &vault);
    let mut server = Server::default();
    for key in ["eligible", "fixed", "muted"] {
        let mut item = capture(key, key, key);
        if key == "fixed" {
            item.dismissible = false;
        }
        if key == "muted" {
            item.package_name = "com.example.muted".into();
        }
        phone.capture_notification(item).unwrap();
    }
    server.upload(&phone);
    server.sync(&desktop);
    desktop
        .set_app_muted(
            &phone_cfg.device_id.to_string(),
            "com.example.muted",
            "Mail",
            true,
        )
        .unwrap();
    desktop.dismiss_all_notifications().unwrap();
    assert!(
        desktop
            .notification_snapshot()
            .unwrap()
            .notifications
            .iter()
            .any(|item| item.target.notification_key == "fixed" && !item.dismissal_pending)
    );
    assert!(
        desktop
            .notification_snapshot()
            .unwrap()
            .notifications
            .iter()
            .any(|item| item.target.notification_key == "eligible" && item.dismissal_pending)
    );
    desktop
        .set_app_muted(
            &phone_cfg.device_id.to_string(),
            "com.example.muted",
            "Mail",
            false,
        )
        .unwrap();
    assert!(
        desktop
            .notification_snapshot()
            .unwrap()
            .notifications
            .iter()
            .any(|item| item.target.notification_key == "muted" && !item.dismissal_pending)
    );
}

#[test]
fn bulk_dismiss_skips_pending_and_caps_the_newest_hundred() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "bulk-cap", &vault);
    let client = unlocked(&cfg, &vault);
    for index in 0..102 {
        let mut item = capture(&format!("key-{index:03}"), "instance", "body");
        item.posted_at = index;
        client.capture_notification(item).unwrap();
    }
    let oldest = client
        .notification_snapshot()
        .unwrap()
        .notifications
        .pop()
        .unwrap();
    client.dismiss_notification(oldest.target).unwrap();
    client.dismiss_all_notifications().unwrap();
    let snapshot = client.notification_snapshot().unwrap();
    let pending: std::collections::BTreeSet<_> = snapshot
        .notifications
        .iter()
        .filter(|item| item.dismissal_pending)
        .map(|item| item.target.notification_key.as_str())
        .collect();
    let expected: std::collections::BTreeSet<_> = (0..=101)
        .filter(|index| *index != 1)
        .map(|index| format!("key-{index:03}"))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        pending,
        expected.iter().map(String::as_str).collect(),
        "the previous pending oldest row remains, then bulk selects keys 002 through 101"
    );
    assert!(
        snapshot
            .notifications
            .iter()
            .any(|item| { item.target.notification_key == "key-000" && item.dismissal_pending })
    );
    assert!(
        snapshot
            .notifications
            .iter()
            .any(|item| { item.target.notification_key == "key-001" && !item.dismissal_pending })
    );
    drop(client);
    assert!(matches!(
        open(&cfg).dismiss_all_notifications(),
        Err(Error::KeysUnavailable)
    ));
}

#[test]
fn icon_refresh_keeps_post_time_seen_state_and_single_banner() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "refresh-phone", &vault), &vault);
    let desktop = unlocked(&config(&dir, "refresh-desktop", &vault), &vault);
    let mut server = Server::default();
    let mut first = capture("refresh", "one", "body");
    first.posted_at = 77;
    phone.capture_notification(first).unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    assert_eq!(desktop.pending_banner_candidates(10).unwrap().len(), 1);
    let original = desktop
        .notification_snapshot()
        .unwrap()
        .notifications
        .remove(0);
    desktop
        .mark_notifications_seen(vec![original.target.clone()])
        .unwrap();
    let mut refresh = capture("refresh", "two", "body");
    refresh.posted_at = 77;
    refresh.app_icon_png_base64 = Some("iVBORw0KGgoAAAANSUhEUgAAACAAAAAg".into());
    phone.capture_notification(refresh).unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    let updated = desktop
        .notification_snapshot()
        .unwrap()
        .notifications
        .remove(0);
    assert_eq!(updated.target.lifetime, original.target.lifetime);
    assert_eq!(updated.posted_at, 77);
    assert!(updated.seen);
    assert!(updated.app_icon_png_base64.is_some());
    assert!(desktop.pending_banner_candidates(10).unwrap().is_empty());
}

#[test]
fn unseen_icon_refresh_does_not_create_another_banner() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "unseen-refresh-phone", &vault), &vault);
    let desktop = unlocked(&config(&dir, "unseen-refresh-desktop", &vault), &vault);
    let mut server = Server::default();
    let mut first = capture("refresh", "one", "body");
    first.posted_at = 77;
    phone.capture_notification(first).unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    let first_banner = desktop.pending_banner_candidates(10).unwrap().remove(0);
    desktop
        .ack_banner_candidates(vec![first_banner.id])
        .unwrap();

    let mut refresh = capture("refresh", "two", "body");
    refresh.posted_at = 77;
    refresh.app_icon_png_base64 = Some("iVBORw0KGgoAAAANSUhEUgAAACAAAAAg".into());
    phone.capture_notification(refresh).unwrap();
    server.upload(&phone);
    server.sync(&desktop);

    let updated = desktop
        .notification_snapshot()
        .unwrap()
        .notifications
        .remove(0);
    assert!(!updated.seen);
    assert_eq!(updated.posted_at, 77);
    assert!(desktop.pending_banner_candidates(10).unwrap().is_empty());
}

#[test]
fn icon_changes_capture_updates_but_identical_icons_duplicate() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "icon-duplicate", &vault), &vault);
    let icon = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAg";
    let mut first = capture("icon", "one", "body");
    first.posted_at = 99;
    first.app_icon_png_base64 = Some(icon.into());
    assert_eq!(
        client.capture_notification(first.clone()).unwrap(),
        NotificationCaptureOutcome::Captured
    );
    let original = client
        .notification_snapshot()
        .unwrap()
        .notifications
        .remove(0);
    assert_eq!(
        client.capture_notification(first).unwrap(),
        NotificationCaptureOutcome::Duplicate
    );
    let mut changed = capture("icon", "two", "body");
    changed.posted_at = 99;
    changed.app_icon_png_base64 = Some(format!("{icon}AA=="));
    assert_eq!(
        client.capture_notification(changed).unwrap(),
        NotificationCaptureOutcome::Captured
    );
    let updated = client
        .notification_snapshot()
        .unwrap()
        .notifications
        .remove(0);
    assert_eq!(updated.target.lifetime, original.target.lifetime);
    assert_eq!(updated.posted_at, 99);
    assert_eq!(updated.target.notification_key, "icon");
    assert_eq!(updated.app_icon_png_base64, Some(format!("{icon}AA==")));
}

#[test]
fn capture_update_remove_repost_and_duplicate_are_durable() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone_cfg = config(&dir, "phone", &vault);
    let desktop_cfg = config(&dir, "desktop", &vault);
    let phone = unlocked(&phone_cfg, &vault);
    let desktop = unlocked(&desktop_cfg, &vault);
    let mut server = Server::default();
    assert_eq!(
        phone
            .capture_notification(capture("n", "one", "first"))
            .unwrap(),
        NotificationCaptureOutcome::Captured
    );
    assert_eq!(
        phone
            .capture_notification(capture("n", "one", "first"))
            .unwrap(),
        NotificationCaptureOutcome::Duplicate
    );
    assert_eq!(
        phone
            .capture_notification(capture("n", "one", "updated"))
            .unwrap(),
        NotificationCaptureOutcome::Captured
    );
    server.upload(&phone);
    server.sync(&desktop);
    let mut snapshot = desktop.notification_snapshot().unwrap();
    let first = snapshot.notifications.remove(0);
    assert_eq!(first.text, "updated");
    phone.remove_notification("n", "one").unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    assert!(
        desktop
            .notification_snapshot()
            .unwrap()
            .notifications
            .is_empty()
    );
    assert_eq!(
        phone
            .capture_notification(capture("n", "two", "repost"))
            .unwrap(),
        NotificationCaptureOutcome::Captured
    );
    server.upload(&phone);
    server.sync(&desktop);
    let mut snapshot = desktop.notification_snapshot().unwrap();
    let repost = snapshot.notifications.remove(0);
    assert_ne!(first.target.lifetime, repost.target.lifetime);
    assert_eq!(repost.text, "repost");
}

#[test]
fn filters_purge_only_notification_banners_and_locked_capture_has_no_queue() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let cfg = config(&dir, "phone", &vault);
    let phone = unlocked(&cfg, &vault);
    phone
        .capture_incoming(incoming("carrier", "sms-1"))
        .unwrap();
    phone
        .capture_notification(capture("n", "one", "body"))
        .unwrap();
    // Phone-local captures never generate a desktop banner candidate.
    assert!(phone.pending_banner_candidates(10).unwrap().is_empty());
    phone
        .set_app_muted(&cfg.device_id.to_string(), "com.example.mail", "Mail", true)
        .unwrap();
    let candidates = phone.pending_banner_candidates(10).unwrap();
    assert!(candidates.is_empty());
    let locked_cfg = config(&dir, "locked", &vault);
    let locked = open(&locked_cfg);
    assert_eq!(
        locked
            .capture_notification(capture("locked", "one", "never persisted"))
            .unwrap(),
        NotificationCaptureOutcome::DroppedLocked
    );
    assert!(locked.pending_outbox().unwrap().is_empty());
    assert!(
        locked
            .notification_snapshot()
            .unwrap()
            .notifications
            .is_empty()
    );
}

#[test]
fn snapshot_notification_history_has_no_banner_or_phone_effect() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone_cfg = config(&dir, "phone", &vault);
    let restore_cfg = config(&dir, "restore", &vault);
    let phone = unlocked(&phone_cfg, &vault);
    let restore = unlocked(&restore_cfg, &vault);
    let mut server = Server::default();
    phone
        .capture_notification(capture("n", "one", "history"))
        .unwrap();
    server.upload(&phone);
    let progress = restore
        .begin_snapshot(Cursor(1), 1, SnapshotPurpose::Resync)
        .unwrap();
    restore
        .append_snapshot_page(
            progress.generation,
            &[SnapshotRecord {
                cursor: Cursor(1),
                envelope: server.log[0].clone(),
            }],
        )
        .unwrap();
    restore.finish_snapshot(progress.generation).unwrap();
    restore.apply_pending(10).unwrap();
    assert_eq!(
        restore.notification_snapshot().unwrap().notifications.len(),
        1
    );
    assert!(restore.pending_banner_candidates(10).unwrap().is_empty());
    assert!(
        restore
            .pending_notification_dismissals(10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn concurrent_filters_converge_and_observed_clock_advances() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone_cfg = config(&dir, "phone-filter", &vault);
    let desktop_cfg = config(&dir, "desktop-filter", &vault);
    let phone = unlocked(&phone_cfg, &vault);
    let desktop = unlocked(&desktop_cfg, &vault);
    let source = phone_cfg.device_id.to_string();

    phone
        .set_app_muted(&source, "com.example.mail", "Mail", true)
        .unwrap();
    desktop
        .set_app_muted(&source, "com.example.mail", "Mail", false)
        .unwrap();

    let mut server = Server::default();
    server.upload(&phone);
    server.upload(&desktop);
    server.sync(&phone);
    server.sync(&desktop);
    let phone_filter = phone.notification_snapshot().unwrap().app_filters.remove(0);
    let desktop_filter = desktop
        .notification_snapshot()
        .unwrap()
        .app_filters
        .remove(0);
    assert_eq!(phone_filter, desktop_filter);

    // A change made after observing the other writer must dominate both revision-1 writes.
    phone
        .set_app_muted(
            &source,
            "com.example.mail",
            "Mail renamed after sync",
            !phone_filter.muted,
        )
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    assert_eq!(
        phone.notification_snapshot().unwrap().app_filters,
        desktop.notification_snapshot().unwrap().app_filters
    );
}

#[test]
fn muting_purges_only_unacknowledged_notification_post_envelopes() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone_cfg = config(&dir, "phone-purge", &vault);
    let desktop_cfg = config(&dir, "desktop-purge", &vault);
    let phone = unlocked(&phone_cfg, &vault);
    let desktop = unlocked(&desktop_cfg, &vault);

    phone
        .capture_incoming(incoming("carrier", "sms-purge"))
        .unwrap();
    phone
        .capture_notification(capture("purged", "one", "secret"))
        .unwrap();
    phone
        .set_app_muted(
            &phone_cfg.device_id.to_string(),
            "com.example.mail",
            "Mail",
            true,
        )
        .unwrap();

    // The sealed notification-post is gone, while the SMS and filter event remain uploadable.
    assert_eq!(phone.pending_outbox().unwrap().len(), 2);
    let mut server = Server::default();
    server.upload(&phone);
    server.sync(&desktop);
    assert!(
        desktop
            .notification_snapshot()
            .unwrap()
            .notifications
            .is_empty()
    );
    assert_eq!(desktop.list_conversations().unwrap().len(), 1);
}

#[test]
fn dismissal_effect_is_lifetime_bound_gated_and_completion_is_not_removal() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone_cfg = config(&dir, "phone-dismiss", &vault);
    let desktop_cfg = config(&dir, "desktop-dismiss", &vault);
    let phone = unlocked(&phone_cfg, &vault);
    let desktop = unlocked(&desktop_cfg, &vault);
    let mut server = Server::default();

    phone
        .capture_notification(capture("dismiss", "one", "first"))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    let first = desktop
        .notification_snapshot()
        .unwrap()
        .notifications
        .remove(0);
    desktop.dismiss_notification(first.target.clone()).unwrap();
    assert!(desktop.notification_snapshot().unwrap().notifications[0].dismissal_pending);
    server.upload(&desktop);
    server.sync(&phone);

    let effects = phone.pending_notification_dismissals(10).unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].target, first.target);
    assert_eq!(effects[0].instance, "one");
    phone
        .complete_notification_dismissal(&effects[0].id)
        .unwrap();
    assert!(
        phone
            .pending_notification_dismissals(10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        phone.notification_snapshot().unwrap().notifications.len(),
        1
    );

    phone.remove_notification("dismiss", "one").unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    assert!(
        desktop
            .notification_snapshot()
            .unwrap()
            .notifications
            .is_empty()
    );

    phone
        .capture_notification(capture("dismiss", "two", "repost"))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    let repost = desktop
        .notification_snapshot()
        .unwrap()
        .notifications
        .remove(0);
    assert_ne!(first.target.lifetime, repost.target.lifetime);
    assert!(
        phone
            .pending_notification_dismissals(10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn non_dismissible_notification_refuses_dismissal() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone_cfg = config(&dir, "phone-fixed", &vault);
    let desktop_cfg = config(&dir, "desktop-fixed", &vault);
    let phone = unlocked(&phone_cfg, &vault);
    let desktop = unlocked(&desktop_cfg, &vault);
    let mut item = capture("fixed", "one", "cannot clear");
    item.dismissible = false;
    phone.capture_notification(item).unwrap();
    let mut server = Server::default();
    server.upload(&phone);
    server.sync(&desktop);
    let target = desktop
        .notification_snapshot()
        .unwrap()
        .notifications
        .remove(0)
        .target;
    assert!(matches!(
        desktop.dismiss_notification(target),
        Err(Error::InvalidRequest(_))
    ));
    assert!(desktop.pending_outbox().unwrap().is_empty());
}

#[test]
fn remote_banners_are_first_lifetime_only_with_full_content_and_sms_read_gating() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone_cfg = config(&dir, "phone-banner", &vault);
    let desktop_cfg = config(&dir, "desktop-banner", &vault);
    let phone = unlocked(&phone_cfg, &vault);
    let desktop = unlocked(&desktop_cfg, &vault);
    let mut server = Server::default();

    phone
        .capture_notification(capture("banner", "one", "full body"))
        .unwrap();
    assert!(phone.pending_banner_candidates(10).unwrap().is_empty());
    server.upload(&phone);
    server.sync(&phone); // own echo remains non-presentational
    server.sync(&desktop);
    let first = desktop.pending_banner_candidates(10).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].title, "Subject");
    assert_eq!(first[0].body, "full body");
    let stable_id = first[0].id.clone();

    phone
        .capture_notification(capture("banner", "one", "updated body"))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    let after_update = desktop.pending_banner_candidates(10).unwrap();
    assert_eq!(after_update.len(), 1);
    assert_eq!(after_update[0].id, stable_id);
    assert_eq!(after_update[0].body, "full body");

    phone
        .capture_incoming(incoming("sms body", "sms-banner"))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    let conversation = desktop
        .list_conversations()
        .unwrap()
        .remove(0)
        .conversation_id;
    let message = desktop.messages(conversation).unwrap().remove(0);
    desktop
        .mark_seen(message.payload.record.message_id)
        .unwrap();
    assert!(
        desktop
            .pending_banner_candidates(10)
            .unwrap()
            .iter()
            .all(|candidate| candidate.kind != "message")
    );
}

#[test]
fn dismissal_effects_stay_blocked_after_reopen_until_unlock_and_after_rotation() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let pc = config(&dir, "effect-phone", &vault);
    let dc = config(&dir, "effect-desktop", &vault);
    let phone = unlocked(&pc, &vault);
    let desktop = unlocked(&dc, &vault);
    let mut server = Server::default();
    phone
        .capture_notification(capture("effect", "1", "body"))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    desktop
        .dismiss_notification(
            desktop.notification_snapshot().unwrap().notifications[0]
                .target
                .clone(),
        )
        .unwrap();
    server.upload(&desktop);
    server.sync(&phone);
    assert_eq!(phone.pending_notification_dismissals(10).unwrap().len(), 1);
    drop(phone);
    let phone = open(&pc);
    assert!(
        phone
            .pending_notification_dismissals(10)
            .unwrap()
            .is_empty()
    );
    phone
        .unlock(&vault.profile, &vault.header, PASSPHRASE)
        .unwrap();
    assert_eq!(phone.pending_notification_dismissals(10).unwrap().len(), 1);
    let next = Vault::epoch(vault.id, 2);
    phone
        .unlock(&next.profile, &next.header, PASSPHRASE)
        .unwrap();
    phone.activate_epoch(2).unwrap();
    assert!(
        phone
            .pending_notification_dismissals(10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn dismissal_imported_in_phone_snapshot_cannot_execute() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let pc = config(&dir, "history-phone", &vault);
    let dc = config(&dir, "history-desktop", &vault);
    let phone = unlocked(&pc, &vault);
    let desktop = unlocked(&dc, &vault);
    let mut server = Server::default();
    phone
        .capture_notification(capture("history", "1", "body"))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    desktop
        .dismiss_notification(
            desktop.notification_snapshot().unwrap().notifications[0]
                .target
                .clone(),
        )
        .unwrap();
    server.upload(&desktop);
    // Restore the same phone identity into another empty database: history is presentation only.
    let mut restore_cfg = pc.clone();
    restore_cfg.database_path = dir.path().join("restored.db");
    let restored = unlocked(&restore_cfg, &vault);
    let progress = restored
        .begin_snapshot(Cursor(2), 2, SnapshotPurpose::Resync)
        .unwrap();
    let records: Vec<_> = server
        .log
        .iter()
        .enumerate()
        .map(|(i, e)| SnapshotRecord {
            cursor: Cursor(i as u64 + 1),
            envelope: e.clone(),
        })
        .collect();
    restored
        .append_snapshot_page(progress.generation, &records)
        .unwrap();
    restored.finish_snapshot(progress.generation).unwrap();
    restored.apply_pending(100).unwrap();
    assert!(restored.notification_snapshot().unwrap().notifications[0].dismissal_pending);
    assert!(
        restored
            .pending_notification_dismissals(100)
            .unwrap()
            .is_empty()
    );
    assert!(restored.pending_banner_candidates(100).unwrap().is_empty());
}

#[test]
fn coalesced_os_update_removal_retires_the_captured_lifetime() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let pc = config(&dir, "coalesced", &vault);
    let phone = unlocked(&pc, &vault);
    phone
        .capture_notification(capture("key", "A", "first"))
        .unwrap();
    let old = phone.notification_snapshot().unwrap().notifications[0]
        .target
        .clone();
    // Host confirmed the OS key is absent; update B was removed before its coalescer flushed.
    phone.remove_notification("key", "B").unwrap();
    assert!(
        phone
            .notification_snapshot()
            .unwrap()
            .notifications
            .is_empty()
    );
    phone
        .capture_notification(capture("key", "C", "repost"))
        .unwrap();
    assert_ne!(
        old.lifetime,
        phone.notification_snapshot().unwrap().notifications[0]
            .target
            .lifetime
    );
}

#[test]
fn os_updates_keep_pending_dismissal_executable_without_duplicate_history() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let pc = config(&dir, "updated-phone", &vault);
    let dc = config(&dir, "updated-desktop", &vault);
    let phone = unlocked(&pc, &vault);
    let desktop = unlocked(&dc, &vault);
    let mut server = Server::default();
    phone
        .capture_notification(capture("key", "A", "first"))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    desktop
        .dismiss_notification(
            desktop.notification_snapshot().unwrap().notifications[0]
                .target
                .clone(),
        )
        .unwrap();
    server.upload(&desktop);
    server.sync(&phone);
    assert_eq!(
        phone
            .capture_notification(capture("key", "B", "first"))
            .unwrap(),
        NotificationCaptureOutcome::Duplicate
    );
    assert!(phone.pending_outbox().unwrap().is_empty());
    assert_eq!(
        phone.pending_notification_dismissals(10).unwrap()[0].instance,
        "B"
    );
    phone
        .capture_notification(capture("key", "C", "new text"))
        .unwrap();
    assert_eq!(
        phone.pending_notification_dismissals(10).unwrap()[0].instance,
        "C"
    );
}

#[test]
fn mute_racing_upload_ack_is_harmless_and_unmute_can_recapture_purged_content() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let pc = config(&dir, "race", &vault);
    let phone = unlocked(&pc, &vault);
    phone
        .capture_notification(capture("key", "A", "first"))
        .unwrap();
    let in_flight = phone.pending_outbox().unwrap()[0].envelope_id;
    phone
        .set_app_muted(&pc.device_id.to_string(), "com.example.mail", "Mail", true)
        .unwrap();
    phone.ack_outbox(in_flight).unwrap();
    phone
        .set_app_muted(&pc.device_id.to_string(), "com.example.mail", "Mail", false)
        .unwrap();
    assert_eq!(
        phone
            .capture_notification(capture("key", "A", "first"))
            .unwrap(),
        NotificationCaptureOutcome::Captured
    );
}

#[test]
fn remove_then_immediate_repost_does_not_rebind_old_pending_dismissal() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let pc = config(&dir, "repost-pending", &vault);
    let phone = unlocked(&pc, &vault);
    phone
        .capture_notification(capture("key", "A", "first"))
        .unwrap();
    let target = phone.notification_snapshot().unwrap().notifications[0]
        .target
        .clone();
    phone.dismiss_notification(target.clone()).unwrap();
    assert_eq!(phone.pending_notification_dismissals(10).unwrap().len(), 1);
    phone.remove_notification("key", "A").unwrap();
    phone
        .capture_notification(capture("key", "B", "repost"))
        .unwrap();
    assert_ne!(
        target.lifetime,
        phone.notification_snapshot().unwrap().notifications[0]
            .target
            .lifetime
    );
    assert!(
        phone
            .pending_notification_dismissals(10)
            .unwrap()
            .is_empty()
    );
}
