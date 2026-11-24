//! Cross-device proofs that app mutes and dismissals replicate within a vault.
use crate::common::*;
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
        mark_read_supported: false,
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

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// A fresh capture (eligible for a banner) whose app offers one safe mark-as-read action.
fn live(key: &str, instance: &str, text: &str, mark_read_supported: bool) -> NotificationCapture {
    NotificationCapture {
        posted_at: now(),
        mark_read_supported,
        ..capture(key, instance, text)
    }
}

fn set_mark_read(phone: &Client, enabled: bool) {
    phone
        .set_gateway_settings(GatewaySettings {
            mirroring_enabled: true,
            notification_mark_read_enabled: enabled,
            ..GatewaySettings::default()
        })
        .unwrap();
}

fn notification_banner(client: &Client) -> BannerCandidate {
    client
        .pending_banner_candidates(10)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.kind == "notification")
        .expect("a notification banner")
}

fn seen(client: &Client, key: &str) -> bool {
    client
        .notification_snapshot()
        .unwrap()
        .notifications
        .into_iter()
        .find(|n| n.target.notification_key == key)
        .unwrap()
        .seen
}

struct MarkReadVault {
    _dir: TempDir,
    phone: Client,
    desktop: Client,
    server: Server,
}

/// A phone with the setting on has posted one supported notification the desktop has seen.
fn mark_read_vault(name: &str) -> MarkReadVault {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, &format!("{name}-phone"), &vault), &vault);
    let desktop = unlocked(&config(&dir, &format!("{name}-desktop"), &vault), &vault);
    let mut server = Server::default();
    set_mark_read(&phone, true);
    phone
        .capture_notification(live("k1", "i1", "first", true))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    MarkReadVault {
        _dir: dir,
        phone,
        desktop,
        server,
    }
}

fn request_from(client: &Client) -> bool {
    let banner = notification_banner(client);
    client
        .request_notification_mark_read(
            banner.notification_target.unwrap(),
            banner.content_sequence.unwrap(),
        )
        .unwrap()
}

#[test]
fn capability_requires_phone_setting() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "capability-phone", &vault), &vault);
    let desktop = unlocked(&config(&dir, "capability-desktop", &vault), &vault);
    let mut server = Server::default();
    set_mark_read(&phone, false);
    phone
        .capture_notification(live("k1", "i1", "first", true))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);

    let banner = notification_banner(&desktop);
    assert!(!banner.mark_read_supported);
    assert!(banner.dismissible);
    assert!(banner.content_sequence.is_some());
    assert!(
        !request_from(&desktop),
        "an unsupported notification refuses"
    );
    assert!(desktop.pending_outbox().unwrap().is_empty());
}

#[test]
fn renotify_with_setting_off_and_supported_input_stays_duplicate() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "renotify-off-phone", &vault), &vault);
    set_mark_read(&phone, false);
    let first = live("k1", "i1", "same", true);
    phone.capture_notification(first.clone()).unwrap();
    assert_eq!(
        phone
            .capture_notification(NotificationCapture {
                instance: "i2".into(),
                ..first
            })
            .unwrap(),
        NotificationCaptureOutcome::Duplicate
    );
}

#[test]
fn capability_flip_publishes_update_without_new_banner() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "flip-phone", &vault), &vault);
    let desktop = unlocked(&config(&dir, "flip-desktop", &vault), &vault);
    let mut server = Server::default();
    set_mark_read(&phone, false);
    let first = live("k1", "i1", "first", true);
    phone.capture_notification(first.clone()).unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    let banner = notification_banner(&desktop);
    let before = banner.content_sequence.unwrap();
    desktop.ack_banner_candidates(vec![banner.id]).unwrap();
    desktop
        .mark_notifications_seen(vec![banner.notification_target.unwrap()])
        .unwrap();

    set_mark_read(&phone, true);
    assert_eq!(
        phone.capture_notification(first).unwrap(),
        NotificationCaptureOutcome::Captured,
        "a capability change publishes"
    );
    server.upload(&phone);
    server.sync(&desktop);

    assert!(desktop.pending_banner_candidates(10).unwrap().is_empty());
    assert!(seen(&desktop, "k1"), "a capability update keeps seen");
    let target = desktop.notification_snapshot().unwrap().notifications[0]
        .target
        .clone();
    assert!(
        !desktop
            .request_notification_mark_read(target.clone(), before)
            .unwrap(),
        "the pre-flip version is stale"
    );
}

#[test]
fn request_marks_seen_and_begin_consumes_once() {
    let MarkReadVault {
        _dir: dir,
        phone,
        desktop,
        mut server,
    } = mark_read_vault("request");
    let banner = notification_banner(&desktop);
    assert!(banner.mark_read_supported);
    assert!(request_from(&desktop));
    assert!(seen(&desktop, "k1"));
    assert!(desktop.pending_banner_candidates(10).unwrap().is_empty());
    server.upload(&desktop);
    server.sync(&phone);

    assert!(seen(&phone, "k1"));
    let pending = phone.pending_notification_mark_reads(now(), 10).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].instance, "i1");
    assert_eq!(Some(pending[0].target.clone()), banner.notification_target);
    assert!(
        phone
            .begin_notification_mark_read(&pending[0].id, now())
            .unwrap()
    );
    assert!(
        !phone
            .begin_notification_mark_read(&pending[0].id, now())
            .unwrap()
    );
    assert!(
        phone
            .pending_notification_mark_reads(now(), 10)
            .unwrap()
            .is_empty()
    );
    drop(dir);
}

#[test]
fn third_device_shows_the_requested_notification_read() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "third-phone", &vault), &vault);
    let desktop = unlocked(&config(&dir, "third-desktop", &vault), &vault);
    let browser = unlocked(&config(&dir, "third-browser", &vault), &vault);
    let mut server = Server::default();
    set_mark_read(&phone, true);
    phone
        .capture_notification(live("k1", "i1", "first", true))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    server.sync(&browser);
    assert!(request_from(&desktop));
    server.upload(&desktop);
    server.sync(&browser);
    assert!(seen(&browser, "k1"));
    assert!(browser.pending_banner_candidates(10).unwrap().is_empty());
}

#[test]
fn identical_renotify_keeps_request_executable_with_new_instance() {
    let MarkReadVault {
        _dir,
        phone,
        desktop,
        mut server,
    } = mark_read_vault("renotify");
    assert!(request_from(&desktop));
    server.upload(&desktop);
    server.sync(&phone);
    assert_eq!(
        phone
            .capture_notification(live("k1", "i2", "first", true))
            .unwrap(),
        NotificationCaptureOutcome::Duplicate
    );
    let pending = phone.pending_notification_mark_reads(now(), 10).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].instance, "i2");
}

#[test]
fn content_change_makes_request_stale() {
    let MarkReadVault {
        _dir,
        phone,
        desktop,
        mut server,
    } = mark_read_vault("stale");
    let banner = notification_banner(&desktop);
    assert!(request_from(&desktop));
    server.upload(&desktop);
    server.sync(&phone);
    phone
        .capture_notification(live("k1", "i2", "a newer message", true))
        .unwrap();
    assert!(
        phone
            .pending_notification_mark_reads(now(), 10)
            .unwrap()
            .is_empty()
    );

    server.upload(&phone);
    server.sync(&desktop);
    assert!(
        !desktop
            .request_notification_mark_read(
                banner.notification_target.unwrap(),
                banner.content_sequence.unwrap()
            )
            .unwrap(),
        "a request for the old version is refused"
    );
}

#[test]
fn only_source_phone_records_request() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "source-phone", &vault), &vault);
    let other = unlocked(&config(&dir, "source-other", &vault), &vault);
    let desktop = unlocked(&config(&dir, "source-desktop", &vault), &vault);
    let mut server = Server::default();
    set_mark_read(&phone, true);
    set_mark_read(&other, true);
    phone
        .capture_notification(live("k1", "i1", "first", true))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    server.sync(&other);
    assert!(request_from(&desktop));
    server.upload(&desktop);
    server.sync(&other);
    server.sync(&phone);
    assert!(
        other
            .pending_notification_mark_reads(now(), 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        phone
            .pending_notification_mark_reads(now(), 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn request_expires_after_ttl_and_future_dated_is_refused() {
    let MarkReadVault {
        _dir,
        phone,
        desktop,
        mut server,
    } = mark_read_vault("expiry");
    assert!(request_from(&desktop));
    server.upload(&desktop);
    server.sync(&phone);
    let later = now() + NOTIFICATION_MARK_READ_TTL_MS + 10 * 60 * 1000;
    assert!(
        phone
            .pending_notification_mark_reads(later, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        phone
            .pending_notification_mark_reads(now(), 10)
            .unwrap()
            .is_empty(),
        "an expired request is retired, not revived"
    );

    let MarkReadVault {
        _dir: _future_dir,
        phone,
        desktop,
        mut server,
    } = mark_read_vault("future");
    assert!(request_from(&desktop));
    server.upload(&desktop);
    server.sync(&phone);
    let earlier = now() - 10 * 60 * 1000;
    let id = phone.pending_notification_mark_reads(now(), 10).unwrap()[0]
        .id
        .clone();
    assert!(
        !phone.begin_notification_mark_read(&id, earlier).unwrap(),
        "a request dated after the phone's clock plus skew never runs"
    );
}

#[test]
fn requests_from_two_computers_execute_once() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "twice-phone", &vault), &vault);
    let desktop = unlocked(&config(&dir, "twice-desktop", &vault), &vault);
    let browser = unlocked(&config(&dir, "twice-browser", &vault), &vault);
    let mut server = Server::default();
    set_mark_read(&phone, true);
    phone
        .capture_notification(live("k1", "i1", "first", true))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    server.sync(&browser);
    assert!(request_from(&desktop));
    assert!(request_from(&browser));
    server.upload(&desktop);
    server.upload(&browser);
    server.sync(&phone);
    assert_eq!(
        phone
            .pending_notification_mark_reads(now(), 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn setting_off_retires_pending_requests() {
    let MarkReadVault {
        _dir,
        phone,
        desktop,
        mut server,
    } = mark_read_vault("setting-off");
    assert!(request_from(&desktop));
    server.upload(&desktop);
    server.sync(&phone);
    set_mark_read(&phone, false);
    assert!(
        phone
            .pending_notification_mark_reads(now(), 10)
            .unwrap()
            .is_empty()
    );
    set_mark_read(&phone, true);
    assert!(
        phone
            .pending_notification_mark_reads(now(), 10)
            .unwrap()
            .is_empty(),
        "turning the setting back on does not revive a request"
    );
}

#[test]
fn dismiss_if_unchanged_refuses_changed_or_non_dismissible() {
    let MarkReadVault {
        _dir,
        phone,
        desktop,
        mut server,
    } = mark_read_vault("dismiss");
    let banner = notification_banner(&desktop);
    let target = banner.notification_target.clone().unwrap();
    let version = banner.content_sequence.unwrap();

    phone
        .capture_notification(live("k1", "i2", "changed", true))
        .unwrap();
    phone
        .capture_notification(NotificationCapture {
            dismissible: false,
            ..live("k2", "j1", "pinned", true)
        })
        .unwrap();
    phone
        .capture_notification(live("k3", "m1", "current", true))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    assert!(
        !desktop
            .dismiss_notification_if_unchanged(target.clone(), version)
            .unwrap(),
        "a changed notification is not dismissed for an older banner"
    );
    let banners = desktop.pending_banner_candidates(10).unwrap();
    let by_key = |key: &str| {
        banners
            .iter()
            .find(|c| {
                c.notification_target
                    .as_ref()
                    .is_some_and(|t| t.notification_key == key)
            })
            .unwrap()
            .clone()
    };
    let changed = by_key("k1");
    assert_eq!(
        changed.content_sequence, None,
        "a stale banner offers no phone effect"
    );
    assert!(!changed.dismissible && !changed.mark_read_supported);
    let pinned = by_key("k2");
    assert!(!pinned.dismissible);
    assert!(
        !desktop
            .dismiss_notification_if_unchanged(
                pinned.notification_target.unwrap(),
                pinned.content_sequence.unwrap()
            )
            .unwrap()
    );
    let current = by_key("k3");
    assert!(
        desktop
            .dismiss_notification_if_unchanged(
                current.notification_target.clone().unwrap(),
                current.content_sequence.unwrap()
            )
            .unwrap()
    );
    server.upload(&desktop);
    server.sync(&phone);
    let dismissals = phone.pending_notification_dismissals(10).unwrap();
    assert_eq!(dismissals.len(), 1);
    assert_eq!(
        Some(dismissals[0].target.clone()),
        current.notification_target
    );
    assert_eq!(dismissals[0].instance, "m1");
}

#[test]
fn content_change_before_first_drain_leaves_the_banner_without_phone_effects() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "early-change-phone", &vault), &vault);
    let desktop = unlocked(&config(&dir, "early-change-desktop", &vault), &vault);
    let mut server = Server::default();
    set_mark_read(&phone, true);
    phone
        .capture_notification(live("k1", "i1", "first", true))
        .unwrap();
    phone
        .capture_notification(live("k1", "i2", "second", true))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    let banner = notification_banner(&desktop);
    assert_eq!(banner.body, "first", "the banner shows the first version");
    assert_eq!(banner.content_sequence, None);
    assert!(!banner.dismissible && !banner.mark_read_supported);
}

#[test]
fn request_received_while_disabled_never_runs_after_reenable() {
    for disable_mirroring in [false, true] {
        let MarkReadVault {
            _dir,
            phone,
            desktop,
            mut server,
        } = mark_read_vault(if disable_mirroring {
            "off-mirroring"
        } else {
            "off-setting"
        });
        phone
            .set_gateway_settings(GatewaySettings {
                mirroring_enabled: !disable_mirroring,
                notification_mark_read_enabled: disable_mirroring,
                ..GatewaySettings::default()
            })
            .unwrap();
        assert!(request_from(&desktop));
        server.upload(&desktop);
        server.sync(&phone);
        set_mark_read(&phone, true);
        assert!(
            phone
                .pending_notification_mark_reads(now(), 10)
                .unwrap()
                .is_empty(),
            "disable_mirroring={disable_mirroring}"
        );
    }
}

#[test]
fn consumed_version_stays_consumed_after_maintenance() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone = unlocked(&config(&dir, "tombstone-phone", &vault), &vault);
    let desktop = unlocked(&config(&dir, "tombstone-desktop", &vault), &vault);
    let browser = unlocked(&config(&dir, "tombstone-browser", &vault), &vault);
    let mut server = Server::default();
    set_mark_read(&phone, true);
    phone
        .capture_notification(live("k1", "i1", "first", true))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    server.sync(&browser);
    let browser_banner = notification_banner(&browser);
    assert!(request_from(&desktop));
    server.upload(&desktop);
    server.sync(&phone);
    let id = phone.pending_notification_mark_reads(now(), 10).unwrap()[0]
        .id
        .clone();
    assert!(phone.begin_notification_mark_read(&id, now()).unwrap());

    let two_days = 2 * 86_400_000;
    assert!(
        phone
            .pending_notification_mark_reads(now() + two_days, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        browser
            .request_notification_mark_read(
                browser_banner.notification_target.unwrap(),
                browser_banner.content_sequence.unwrap()
            )
            .unwrap()
    );
    server.upload(&browser);
    server.sync(&phone);
    assert!(
        phone
            .pending_notification_mark_reads(now(), 10)
            .unwrap()
            .is_empty(),
        "a second request for a consumed, still-live version never runs"
    );
}

#[test]
fn begin_refuses_when_eligibility_changes_after_pending() {
    let MarkReadVault {
        _dir,
        phone,
        desktop,
        mut server,
    } = mark_read_vault("begin-race");
    assert!(request_from(&desktop));
    server.upload(&desktop);
    server.sync(&phone);
    let id = phone.pending_notification_mark_reads(now(), 10).unwrap()[0]
        .id
        .clone();
    phone
        .capture_notification(live("k1", "i2", "newer content", true))
        .unwrap();
    assert!(!phone.begin_notification_mark_read(&id, now()).unwrap());
}

#[test]
fn locked_phone_lists_and_begins_nothing() {
    let dir = TempDir::new().unwrap();
    let vault = Vault::new();
    let phone_config = config(&dir, "locked-phone", &vault);
    let phone = unlocked(&phone_config, &vault);
    let desktop = unlocked(&config(&dir, "locked-desktop", &vault), &vault);
    let mut server = Server::default();
    set_mark_read(&phone, true);
    phone
        .capture_notification(live("k1", "i1", "first", true))
        .unwrap();
    server.upload(&phone);
    server.sync(&desktop);
    assert!(request_from(&desktop));
    server.upload(&desktop);
    server.sync(&phone);
    let id = phone.pending_notification_mark_reads(now(), 10).unwrap()[0]
        .id
        .clone();
    drop(phone);
    let reopened = open(&phone_config);
    assert!(
        reopened
            .pending_notification_mark_reads(now(), 10)
            .unwrap()
            .is_empty()
    );
    assert!(!reopened.begin_notification_mark_read(&id, now()).unwrap());
}
