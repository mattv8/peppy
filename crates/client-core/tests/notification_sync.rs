//! Cross-device proofs that app mutes and dismissals replicate within a vault.
mod common;
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

fn muted(client: &Client, source_device_id: &str, package: &str) -> Option<bool> {
    client
        .notification_snapshot()
        .unwrap()
        .app_filters
        .into_iter()
        .find(|f| f.source_device_id == source_device_id && f.package_name == package)
        .map(|f| f.muted)
}

/// Calls `apply_pending` like native hosts do until no step reports progress.
fn settle(client: &Client) {
    loop {
        let step = client.apply_pending(1000).unwrap();
        if step.applied + step.drained + step.quarantined + step.superseded == 0
            && step.snapshot_remaining == 0
        {
            return;
        }
    }
}

/// Imports a compaction snapshot the way a host resync does.
fn import(client: &Client, records: &[SnapshotRecord], high_water: usize, generation: u64) {
    let session = client
        .begin_snapshot_with_compaction(
            Cursor(high_water as u64),
            records.len() as u64,
            SnapshotPurpose::Resync,
            Some(generation),
        )
        .unwrap();
    for page in records.chunks(2) {
        client
            .append_snapshot_page(session.generation, page)
            .unwrap();
    }
    client.finish_snapshot(session.generation).unwrap();
}

/// Server compaction model: a record named in any retained record's `supersedes` is dropped,
/// and terminal tombstones are dropped past the purge horizon. Cursors are preserved.
fn compact(log: &[Envelope], purge_terminal: bool) -> Vec<SnapshotRecord> {
    let superseded: std::collections::HashSet<(DeviceId, u64)> = log
        .iter()
        .filter_map(|e| e.compaction.as_ref())
        .flat_map(|c| c.supersedes.iter())
        .map(|r| (r.producer_device_id, r.producer_sequence.0))
        .collect();
    log.iter()
        .enumerate()
        .filter(|(_, e)| !superseded.contains(&(e.producer_device_id, e.producer_sequence.0)))
        .filter(|(_, e)| !(purge_terminal && e.compaction.as_ref().is_some_and(|c| c.terminal)))
        .map(|(i, e)| SnapshotRecord {
            cursor: Cursor(i as u64 + 1),
            envelope: e.clone(),
        })
        .collect()
}

#[test]
fn desktop_mute_stops_capture_on_the_source_phone() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone_cfg = config(&dir, "sync-phone", &vault);
    let phone = unlocked(&phone_cfg, &vault);
    let desktop = unlocked(&config(&dir, "sync-desktop", &vault), &vault);
    let mut server = Server::default();
    let phone_id = phone_cfg.device_id.to_string();

    desktop
        .set_app_muted(&phone_id, "com.example.mail", "Mail", true)
        .unwrap();
    server.upload(&desktop);
    server.sync(&phone);

    assert_eq!(
        phone
            .capture_notification(capture("k1", "i1", "hello"))
            .unwrap(),
        NotificationCaptureOutcome::FilteredOut
    );
    assert!(
        phone
            .notification_snapshot()
            .unwrap()
            .notifications
            .is_empty()
    );
}

#[test]
fn mute_on_one_phone_does_not_mute_the_same_app_on_another_phone() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone_a_cfg = config(&dir, "sync-phone-a", &vault);
    let phone_a = unlocked(&phone_a_cfg, &vault);
    let phone_b = unlocked(&config(&dir, "sync-phone-b", &vault), &vault);
    let desktop = unlocked(&config(&dir, "sync-desktop-b", &vault), &vault);
    let mut server = Server::default();
    let phone_a_id = phone_a_cfg.device_id.to_string();

    desktop
        .set_app_muted(&phone_a_id, "com.example.mail", "Mail", true)
        .unwrap();
    server.upload(&desktop);
    server.sync(&phone_a);
    server.sync(&phone_b);

    assert_eq!(
        phone_b
            .capture_notification(capture("k2", "i2", "still banners"))
            .unwrap(),
        NotificationCaptureOutcome::Captured
    );
    assert_eq!(
        phone_a
            .capture_notification(capture("k3", "i3", "muted"))
            .unwrap(),
        NotificationCaptureOutcome::FilteredOut
    );
}

#[test]
fn browser_dismissal_hides_on_desktop_and_queues_os_effect_on_phone() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone_cfg = config(&dir, "dismiss-phone", &vault);
    let phone = unlocked(&phone_cfg, &vault);
    let desktop = unlocked(&config(&dir, "dismiss-desktop", &vault), &vault);
    let browser = unlocked(&config(&dir, "dismiss-browser", &vault), &vault);
    let mut server = Server::default();

    phone
        .capture_notification(capture("k1", "i1", "first"))
        .unwrap();
    server.upload(&phone);
    server.sync(&browser);
    server.sync(&desktop);
    let item = browser
        .notification_snapshot()
        .unwrap()
        .notifications
        .remove(0);
    browser.dismiss_notification(item.target.clone()).unwrap();
    server.upload(&browser);
    server.sync(&desktop);
    server.sync(&phone);

    let shown = desktop.notification_snapshot().unwrap().notifications;
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].target, item.target);
    assert!(
        shown[0].dismissal_pending,
        "desktop shows the dismissal as pending"
    );

    let effects = phone.pending_notification_dismissals(10).unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].target, item.target);
    assert_eq!(effects[0].instance, "i1");
}

#[test]
fn device_enrolled_after_compaction_learns_existing_mutes() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone_cfg = config(&dir, "compact-phone", &vault);
    let phone = unlocked(&phone_cfg, &vault);
    let desktop = unlocked(&config(&dir, "compact-desktop", &vault), &vault);
    let mut server = Server::default();
    let phone_id = phone_cfg.device_id.to_string();

    desktop
        .set_app_muted(&phone_id, "com.example.mail", "Mail", true)
        .unwrap();
    server.upload(&desktop);
    server.sync(&phone);
    assert_eq!(muted(&phone, &phone_id, "com.example.mail"), Some(true));

    let fresh = unlocked(&config(&dir, "compact-fresh", &vault), &vault);
    fresh.set_server_compaction_supported(true).unwrap();
    let high_water = server.log.len();
    let retained = compact(&server.log, true);
    import(&fresh, &retained, high_water, 11);
    settle(&fresh);

    assert_eq!(
        fresh.snapshot_projection_status().unwrap().map(|s| s.state),
        Some(SnapshotProjectionState::Promoted)
    );
    assert_eq!(muted(&fresh, &phone_id, "com.example.mail"), Some(true));
}
