use super::*;
use peppy_client_core::NotificationTarget;

pub(super) fn handles(command: &str) -> bool {
    matches!(
        command,
        "dismiss_notifications"
            | "set_app_muted"
            | "mark_notifications_seen"
            | "set_notification_preferences"
            | "notification_preferences"
            | "adopt_legacy_notification_preferences"
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
            "dismiss_notifications" => {
                #[derive(Deserialize)]
                struct Input {
                    targets: Vec<NotificationTarget>,
                }
                let input: Input = serde_json::from_value(args).map_err(|_| invalid())?;
                return serde_json::to_value(
                    self.client()?
                        .dismiss_notifications(input.targets)
                        .map_err(core)?,
                )
                .map_err(|_| invalid());
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
                self.client()?
                    .set_notification_preferences(preferences)
                    .map_err(core)?;
            }
            "notification_preferences" => {
                return serde_json::to_value(
                    self.client()?.notification_preferences().map_err(core)?,
                )
                .map_err(|_| invalid());
            }
            "adopt_legacy_notification_preferences" => {
                let preferences: NotificationPreferences =
                    serde_json::from_value(args).map_err(|_| invalid())?;
                let adopted = self
                    .client()?
                    .adopt_legacy_notification_preferences(preferences)
                    .map_err(core)?;
                return Ok(json!({ "adopted": adopted }));
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
                app_icon_png_base64: None,
                mark_read_supported: false,
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
    }

    #[test]
    fn dismiss_notifications_reports_outcomes() {
        let (_directory, mut core) = fixture();
        let first = capture(&core, "first", true);
        let permanent = capture(&core, "permanent", false);

        let results = call(
            &mut core,
            "dismiss_notifications",
            json!({"targets":[first.clone(), permanent.clone(), first.clone()]}),
        );
        assert_eq!(
            results,
            json!([
                {"target": first, "outcome": "recorded"},
                {"target": permanent, "outcome": "notDismissible"},
                {"target": first, "outcome": "alreadyRecorded"},
            ])
        );
        let outbox = core.client().unwrap().pending_outbox().unwrap().len();

        call(
            &mut core,
            "dismiss_notifications",
            json!({"targets":[first]}),
        );
        assert_eq!(
            core.client().unwrap().pending_outbox().unwrap().len(),
            outbox
        );
    }

    #[test]
    fn dismiss_notifications_caps_one_hundred_targets() {
        let (_directory, mut core) = fixture();
        let targets: Vec<NotificationTarget> = (0..101)
            .map(|index| capture(&core, &format!("notice-{index}"), true))
            .collect();

        let rejected = core.dispatch(
            &json!({"command":"dismiss_notifications","args":{"targets":targets}}).to_string(),
        );
        let rejected: Value = serde_json::from_str(&rejected).unwrap();
        assert_eq!(rejected["ok"], false, "{rejected}");
        let pending = core
            .client()
            .unwrap()
            .notification_snapshot()
            .unwrap()
            .notifications
            .into_iter()
            .filter(|notification| notification.dismissal_pending)
            .count();
        assert_eq!(pending, 0);

        let accepted = call(
            &mut core,
            "dismiss_notifications",
            json!({"targets":&targets[..100]}),
        );
        assert_eq!(accepted.as_array().unwrap().len(), 100);
        let pending = core
            .client()
            .unwrap()
            .notification_snapshot()
            .unwrap()
            .notifications
            .into_iter()
            .filter(|notification| notification.dismissal_pending)
            .count();
        assert_eq!(pending, 100);
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
            json!({"messageBanners":false,"mirroredBanners":true,"preview":"hidden","muteWithHeads":true,"showSource":false})
        );
        assert_eq!(
            call(&mut core, "notification_preferences", json!({})),
            snapshot["notificationPreferences"]
        );
        let rejected: Value=serde_json::from_str(&core.dispatch(r#"{"command":"set_notification_preferences","args":{"messageBanners":true,"mirroredBanners":true,"preview":"secret-fixture"}}"#)).unwrap();
        assert_eq!(rejected["ok"], false);
        assert!(!rejected.to_string().contains("secret-fixture"));
        assert_eq!(
            call(&mut core, "snapshot", json!({}))["notificationPreferences"],
            snapshot["notificationPreferences"]
        );
    }

    #[test]
    fn unset_preferences_are_null_and_legacy_adoption_is_one_shot() {
        let (_directory, mut core) = fixture();
        assert_eq!(
            call(&mut core, "notification_preferences", json!({})),
            Value::Null
        );
        let legacy = json!({"messageBanners":true,"mirroredBanners":false,"preview":"full","muteWithHeads":true});
        assert_eq!(
            call(
                &mut core,
                "adopt_legacy_notification_preferences",
                legacy.clone()
            ),
            json!({"adopted":true})
        );
        assert_eq!(
            call(
                &mut core,
                "adopt_legacy_notification_preferences",
                legacy.clone()
            ),
            json!({"adopted":false})
        );
        assert_eq!(
            call(&mut core, "notification_preferences", json!({})),
            json!({"messageBanners":true,"mirroredBanners":false,"preview":"full","muteWithHeads":true,"showSource":false})
        );
    }
}
