use super::*;
use peppy_client_core::NotificationTarget;

pub(super) fn handles(command: &str) -> bool {
    matches!(
        command,
        "dismiss_notification"
            | "dismiss_all_notifications"
            | "set_app_muted"
            | "mark_notifications_seen"
            | "set_notification_preferences"
            | "pending_banner_candidates"
            | "ack_banner_candidates"
    )
}

impl BrowserCore {
    pub(super) fn notifications_command(
        &mut self,
        command: &str,
        args: Value,
    ) -> Result<Value, Failure> {
        match command {
            "dismiss_notification" => {
                #[derive(Deserialize)]
                struct Input {
                    target: NotificationTarget,
                }
                let input: Input = serde_json::from_value(args).map_err(|_| invalid())?;
                self.client()?
                    .dismiss_notification(input.target)
                    .map_err(core)?;
            }
            "dismiss_all_notifications" => {
                let client = self.client()?;
                for notification in client.notification_snapshot().map_err(core)?.notifications {
                    if notification.dismissible && !notification.dismissal_pending {
                        client
                            .dismiss_notification(notification.target)
                            .map_err(core)?;
                    }
                }
            }
            "set_app_muted" => {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct Input {
                    source_device_id: String,
                    package_name: String,
                    app_name: String,
                    muted: bool,
                }
                let input: Input = serde_json::from_value(args).map_err(|_| invalid())?;
                self.client()?
                    .set_app_muted(
                        &input.source_device_id,
                        &input.package_name,
                        &input.app_name,
                        input.muted,
                    )
                    .map_err(core)?;
            }
            "mark_notifications_seen" => {
                #[derive(Deserialize)]
                struct Input {
                    targets: Vec<NotificationTarget>,
                }
                let input: Input = serde_json::from_value(args).map_err(|_| invalid())?;
                self.client()?
                    .mark_notifications_seen(input.targets)
                    .map_err(core)?;
            }
            "set_notification_preferences" => {
                let preferences: NotificationPreferences =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                self.client()?;
                // The host checkpoints these display flags; notification content stays in SQLCipher.
                self.preferences = preferences;
            }
            "pending_banner_candidates" => {
                #[derive(Deserialize)]
                struct Input {
                    limit: usize,
                }
                let input: Input = serde_json::from_value(args).map_err(|_| invalid())?;
                if input.limit == 0 || input.limit > 100 {
                    return Err(invalid());
                }
                return serde_json::to_value(
                    self.client()?
                        .pending_banner_candidates(input.limit)
                        .map_err(core)?,
                )
                .map_err(|_| invalid());
            }
            "ack_banner_candidates" => {
                #[derive(Deserialize)]
                struct Input {
                    ids: Vec<String>,
                }
                let input: Input = serde_json::from_value(args).map_err(|_| invalid())?;
                if input.ids.len() > 100 {
                    return Err(invalid());
                }
                self.client()?
                    .ack_banner_candidates(input.ids)
                    .map_err(core)?;
            }
            _ => return Err(unknown_command()),
        }
        Ok(json!({}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_client_core::NotificationCapture;
    use peppy_crypto::{create_vault_check_header, derive_root_key};

    fn call(core: &mut BrowserCore, command: &str, args: Value) -> Value {
        let reply: Value = serde_json::from_str(
            &core.dispatch(&json!({"command":command,"args":args}).to_string()),
        )
        .unwrap();
        assert_eq!(reply["ok"], true, "{reply}");
        reply["value"].clone()
    }

    fn fixture() -> (tempfile::TempDir, BrowserCore) {
        let directory = tempfile::tempdir().unwrap();
        let vault = VaultId::new();
        let mut core = BrowserCore::new(directory.path().to_owned());
        call(
            &mut core,
            "open",
            json!({"vaultId":vault,"deviceId":DeviceId::new(),"databaseKey":vec![1;32],"origin":"https://peppy.test","deviceRole":"device"}),
        );
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("browser fixture", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        call(
            &mut core,
            "unlock",
            json!({"profile":profile,"header":header,"passphrase":"browser fixture"}),
        );
        (directory, core)
    }

    fn capture(core: &BrowserCore, name: &str, dismissible: bool) -> NotificationTarget {
        core.client()
            .unwrap()
            .capture_notification(NotificationCapture {
                notification_key: name.into(),
                instance: format!("instance-{name}"),
                package_name: "dev.fixture".into(),
                app_name: "Fixture".into(),
                title: "Private title".into(),
                text: "Private text".into(),
                category: None,
                posted_at: 100,
                dismissible,
            })
            .unwrap();
        core.client()
            .unwrap()
            .notification_snapshot()
            .unwrap()
            .notifications
            .into_iter()
            .find(|entry| entry.target.notification_key == name)
            .unwrap()
            .target
    }

    #[test]
    fn notification_actions_use_core_lifetime_and_dismissibility() {
        let (_directory, mut core) = fixture();
        let first = capture(&core, "first", true);
        let permanent = capture(&core, "permanent", false);
        call(
            &mut core,
            "mark_notifications_seen",
            json!({"targets":[first.clone()]}),
        );
        let snapshot = core.client().unwrap().notification_snapshot().unwrap();
        assert!(
            snapshot
                .notifications
                .iter()
                .find(|entry| entry.target == first)
                .unwrap()
                .seen
        );
        call(
            &mut core,
            "dismiss_notification",
            json!({"target":first.clone()}),
        );
        let outbox = core.client().unwrap().pending_outbox().unwrap().len();
        call(&mut core, "dismiss_notification", json!({"target":first}));
        assert_eq!(
            core.client().unwrap().pending_outbox().unwrap().len(),
            outbox
        );
        call(&mut core, "dismiss_all_notifications", json!({}));
        assert!(
            core.client()
                .unwrap()
                .notification_snapshot()
                .unwrap()
                .notifications
                .iter()
                .any(|entry| entry.target == permanent && !entry.dismissal_pending)
        );
    }

    #[test]
    fn preferences_and_mute_are_reflected_without_changing_identity() {
        let (_directory, mut core) = fixture();
        let target = capture(&core, "notice", true);
        call(
            &mut core,
            "set_app_muted",
            json!({"sourceDeviceId":target.source_device_id,"packageName":"dev.fixture","appName":"Fixture","muted":true}),
        );
        assert!(
            core.client()
                .unwrap()
                .notification_snapshot()
                .unwrap()
                .app_filters
                .iter()
                .any(|filter| filter.muted)
        );
        call(
            &mut core,
            "set_notification_preferences",
            json!({"messageBanners":false,"mirroredBanners":true,"preview":"hidden"}),
        );
        let snapshot = call(&mut core, "snapshot", json!({}));
        assert_eq!(
            snapshot["notificationPreferences"],
            json!({"messageBanners":false,"mirroredBanners":true,"preview":"hidden"})
        );
        let rejected: Value=serde_json::from_str(&core.dispatch(r#"{"command":"set_notification_preferences","args":{"messageBanners":true,"mirroredBanners":true,"preview":"secret-fixture"}}"#)).unwrap();
        assert_eq!(rejected["ok"], false);
        assert!(!rejected.to_string().contains("secret-fixture"));
        assert_eq!(
            call(&mut core, "snapshot", json!({}))["notificationPreferences"],
            snapshot["notificationPreferences"]
        );
    }
}
