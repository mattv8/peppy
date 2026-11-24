//! Durable, host-neutral gateway policy. Platform permission, network, and carrier
//! facts are supplied by native code; this module neither discovers nor persists them.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatewayPlatform {
    Android,
    Ios,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewaySettings {
    pub mirroring_enabled: bool,
    pub mirroring_wifi_only: bool,
    pub skip_silent: bool,
    pub sms_sync_enabled: bool,
    pub mms_sync_enabled: bool,
    pub media_wifi_only: bool,
    pub phone_read_enabled: bool,
    /// Lets the vault's computers ask this phone to press a mirrored app notification's own
    /// semantic mark-as-read action.
    pub notification_mark_read_enabled: bool,
}

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            mirroring_enabled: false,
            mirroring_wifi_only: false,
            skip_silent: true,
            sms_sync_enabled: true,
            mms_sync_enabled: true,
            media_wifi_only: true,
            phone_read_enabled: false,
            notification_mark_read_enabled: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GatewayHostFacts {
    pub wifi_connected: bool,
    pub notification_listener_available: bool,
    pub sms_available: bool,
    pub notification_is_silent: bool,
    pub phone_read_available: bool,
    /// Android 9 or later with notification access, so the host can fire a semantic action.
    pub notification_mark_read_available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayCapabilities {
    pub notification_mirroring_supported: bool,
    pub sms_sync_supported: bool,
    pub mms_sync_supported: bool,
    pub rcs_supported: bool,
    pub phone_read_supported: bool,
    pub notification_mark_read_supported: bool,
}

/// Why this phone cannot currently dismiss mirrored notifications. Stored and sent as snake_case
/// codes; codes this build does not know decode as [`Self::Other`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationDismissalBlockReason {
    MirroringOff,
    NotificationAccessOff,
    WaitingForWifi,
    #[serde(other)]
    Other,
}

impl NotificationDismissalBlockReason {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::MirroringOff => "mirroring_off",
            Self::NotificationAccessOff => "notification_access_off",
            Self::WaitingForWifi => "waiting_for_wifi",
            Self::Other => "other",
        }
    }

    pub(crate) fn from_code(code: &str) -> Self {
        match code {
            "mirroring_off" => Self::MirroringOff,
            "notification_access_off" => Self::NotificationAccessOff,
            "waiting_for_wifi" => Self::WaitingForWifi,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayPolicyDecision {
    pub capture_notification: bool,
    pub capture_sms: bool,
    pub capture_mms: bool,
    pub transfer_media: bool,
    pub rcs_supported: bool,
    pub apply_phone_read: bool,
    pub ingest_phone_read: bool,
    pub apply_notification_mark_read: bool,
}

impl Client {
    pub fn gateway_settings(&self) -> Result<GatewaySettings, Error> {
        let store = self.lock()?;
        load(&store.conn)
    }

    pub fn set_gateway_settings(&self, settings: GatewaySettings) -> Result<(), Error> {
        let mut store = self.lock()?;
        let tx = store
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        save(&tx, &settings)?;
        if !settings.phone_read_enabled {
            super::phone_read::retire_all_phone_read_effects(&tx)?;
        }
        if !settings.notification_mark_read_enabled || !settings.mirroring_enabled {
            super::notifications::retire_pending_mark_reads(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn gateway_capabilities(
        platform: GatewayPlatform,
        facts: GatewayHostFacts,
    ) -> GatewayCapabilities {
        let carrier_supported = platform == GatewayPlatform::Android && facts.sms_available;
        GatewayCapabilities {
            notification_mirroring_supported: platform == GatewayPlatform::Android
                && facts.notification_listener_available,
            sms_sync_supported: carrier_supported,
            mms_sync_supported: carrier_supported,
            // Neither host has a legitimate general RCS integration.
            rcs_supported: false,
            phone_read_supported: carrier_supported && facts.phone_read_available,
            notification_mark_read_supported: platform == GatewayPlatform::Android
                && facts.notification_mark_read_available,
        }
    }

    /// RCS is not a platform-wide claim: it is available only while this gateway's current
    /// content-free link capability record remains fresh. It authorizes no transport effect.
    pub fn gateway_capabilities_with_rcs_link(
        &self,
        platform: GatewayPlatform,
        facts: GatewayHostFacts,
        now_ms: i64,
    ) -> Result<GatewayCapabilities, Error> {
        let mut capabilities = Self::gateway_capabilities(platform, facts);
        let store = self.lock()?;
        capabilities.rcs_supported = platform == GatewayPlatform::Android
            && super::rcs_capability::current_gateway(&store.conn, store.config.device_id, now_ms)?;
        Ok(capabilities)
    }

    pub fn gateway_policy_decision(
        &self,
        platform: GatewayPlatform,
        facts: GatewayHostFacts,
    ) -> Result<GatewayPolicyDecision, Error> {
        let settings = self.gateway_settings()?;
        let capabilities = Self::gateway_capabilities(platform, facts);
        let block = notification_policy_block(&settings, &capabilities, facts);
        Ok(GatewayPolicyDecision {
            capture_notification: block.is_none()
                && (!settings.skip_silent || !facts.notification_is_silent),
            capture_sms: settings.sms_sync_enabled && capabilities.sms_sync_supported,
            capture_mms: settings.sms_sync_enabled
                && settings.mms_sync_enabled
                && capabilities.mms_sync_supported,
            // Media transfer is separately gated; acquisition and SMS text capture are not.
            transfer_media: !settings.media_wifi_only || facts.wifi_connected,
            rcs_supported: capabilities.rcs_supported,
            apply_phone_read: settings.phone_read_enabled
                && capabilities.phone_read_supported
                && settings.sms_sync_enabled,
            ingest_phone_read: settings.phone_read_enabled
                && settings.sms_sync_enabled
                && platform == GatewayPlatform::Android
                && capabilities.sms_sync_supported,
            apply_notification_mark_read: settings.notification_mark_read_enabled
                && block.is_none()
                && capabilities.notification_mark_read_supported,
        })
    }
}

/// Shared mirroring gate: `None` means this phone can dismiss and capture mirrored notifications.
pub(crate) fn notification_policy_block(
    settings: &GatewaySettings,
    caps: &GatewayCapabilities,
    facts: GatewayHostFacts,
) -> Option<NotificationDismissalBlockReason> {
    if !settings.mirroring_enabled {
        Some(NotificationDismissalBlockReason::MirroringOff)
    } else if !caps.notification_mirroring_supported {
        Some(NotificationDismissalBlockReason::NotificationAccessOff)
    } else if settings.mirroring_wifi_only && !facts.wifi_connected {
        Some(NotificationDismissalBlockReason::WaitingForWifi)
    } else {
        None
    }
}

pub(super) fn seed(conn: &Connection, legacy_enabled_installation: bool) -> Result<(), Error> {
    let mut defaults = GatewaySettings::default();
    if legacy_enabled_installation {
        // Native hosts import their own legacy master preference exactly once. Core has no safe
        // cross-platform knowledge of that preference, so it must retain the fresh default here.
        defaults.skip_silent = false;
        defaults.media_wifi_only = false;
    }
    conn.execute(
        "INSERT OR IGNORE INTO gateway_settings(id,mirroring_enabled,mirroring_wifi_only,skip_silent,sms_sync_enabled,mms_sync_enabled,media_wifi_only) VALUES(1,?,?,?,?,?,?)",
        params![
            defaults.mirroring_enabled,
            defaults.mirroring_wifi_only,
            defaults.skip_silent,
            defaults.sms_sync_enabled,
            defaults.mms_sync_enabled,
            defaults.media_wifi_only,
        ],
    )?;
    Ok(())
}

pub(crate) fn load(conn: &Connection) -> Result<GatewaySettings, Error> {
    conn.query_row(
        "SELECT mirroring_enabled,mirroring_wifi_only,skip_silent,sms_sync_enabled,mms_sync_enabled,media_wifi_only,phone_read_enabled,notification_mark_read_enabled FROM gateway_settings WHERE id=1",
        [],
        |r| Ok(GatewaySettings {
            mirroring_enabled: r.get(0)?, mirroring_wifi_only: r.get(1)?, skip_silent: r.get(2)?,
            sms_sync_enabled: r.get(3)?, mms_sync_enabled: r.get(4)?, media_wifi_only: r.get(5)?,
            phone_read_enabled: r.get(6)?, notification_mark_read_enabled: r.get(7)?,
        }),
    ).map_err(Into::into)
}

fn save(conn: &Connection, settings: &GatewaySettings) -> Result<(), Error> {
    conn.execute(
        "UPDATE gateway_settings SET mirroring_enabled=?,mirroring_wifi_only=?,skip_silent=?,sms_sync_enabled=?,mms_sync_enabled=?,media_wifi_only=?,phone_read_enabled=?,notification_mark_read_enabled=? WHERE id=1",
        params![settings.mirroring_enabled, settings.mirroring_wifi_only, settings.skip_silent, settings.sms_sync_enabled, settings.mms_sync_enabled, settings.media_wifi_only, settings.phone_read_enabled, settings.notification_mark_read_enabled],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_REASONS: [NotificationDismissalBlockReason; 4] = [
        NotificationDismissalBlockReason::MirroringOff,
        NotificationDismissalBlockReason::NotificationAccessOff,
        NotificationDismissalBlockReason::WaitingForWifi,
        NotificationDismissalBlockReason::Other,
    ];

    fn caps(supported: bool) -> GatewayCapabilities {
        GatewayCapabilities {
            notification_mirroring_supported: supported,
            sms_sync_supported: false,
            mms_sync_supported: false,
            rcs_supported: false,
            phone_read_supported: false,
            notification_mark_read_supported: false,
        }
    }

    fn settings(mirroring_enabled: bool, wifi_only: bool, skip_silent: bool) -> GatewaySettings {
        GatewaySettings {
            mirroring_enabled,
            mirroring_wifi_only: wifi_only,
            skip_silent,
            ..GatewaySettings::default()
        }
    }

    fn facts(wifi_connected: bool, notification_is_silent: bool) -> GatewayHostFacts {
        GatewayHostFacts {
            wifi_connected,
            notification_is_silent,
            ..GatewayHostFacts::default()
        }
    }

    /// The pre-refactor formulas, kept verbatim so the refactor is checked against them.
    fn legacy_capture(s: &GatewaySettings, c: &GatewayCapabilities, f: GatewayHostFacts) -> bool {
        let wifi_permits = !s.mirroring_wifi_only || f.wifi_connected;
        s.mirroring_enabled
            && c.notification_mirroring_supported
            && wifi_permits
            && (!s.skip_silent || !f.notification_is_silent)
    }

    fn legacy_mark_read(s: &GatewaySettings, c: &GatewayCapabilities, f: GatewayHostFacts) -> bool {
        let wifi_permits = !s.mirroring_wifi_only || f.wifi_connected;
        s.notification_mark_read_enabled
            && s.mirroring_enabled
            && c.notification_mirroring_supported
            && c.notification_mark_read_supported
            && wifi_permits
    }

    #[test]
    fn dismissal_block_reason_precedence() {
        let f = facts(false, false);
        assert_eq!(
            notification_policy_block(&settings(false, true, true), &caps(false), f),
            Some(NotificationDismissalBlockReason::MirroringOff)
        );
        assert_eq!(
            notification_policy_block(&settings(true, true, true), &caps(false), f),
            Some(NotificationDismissalBlockReason::NotificationAccessOff)
        );
        assert_eq!(
            notification_policy_block(&settings(true, true, true), &caps(true), f),
            Some(NotificationDismissalBlockReason::WaitingForWifi)
        );
        assert_eq!(
            notification_policy_block(&settings(true, false, true), &caps(true), f),
            None
        );
        assert_eq!(
            notification_policy_block(&settings(true, true, true), &caps(true), facts(true, false)),
            None
        );
    }

    #[test]
    fn capture_notification_matches_dismissal_availability_for_every_fact_combination() {
        let mut combinations = 0;
        for mirroring_enabled in [false, true] {
            for mirroring_wifi_only in [false, true] {
                for skip_silent in [false, true] {
                    for listener_available in [false, true] {
                        for wifi_connected in [false, true] {
                            for notification_is_silent in [false, true] {
                                let mut s =
                                    settings(mirroring_enabled, mirroring_wifi_only, skip_silent);
                                s.notification_mark_read_enabled = true;
                                let c = GatewayCapabilities {
                                    notification_mark_read_supported: true,
                                    ..caps(listener_available)
                                };
                                let f = GatewayHostFacts {
                                    wifi_connected,
                                    notification_is_silent,
                                    notification_listener_available: listener_available,
                                    ..GatewayHostFacts::default()
                                };
                                let block = notification_policy_block(&s, &c, f);
                                let silence_allows = !skip_silent || !notification_is_silent;
                                let capture = block.is_none() && silence_allows;
                                assert_eq!(capture, legacy_capture(&s, &c, f));
                                let mark_read = s.notification_mark_read_enabled
                                    && block.is_none()
                                    && c.notification_mark_read_supported;
                                assert_eq!(mark_read, legacy_mark_read(&s, &c, f));
                                combinations += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(combinations, 64);
    }

    #[test]
    fn dismissal_block_reason_codes_round_trip_and_unknown_is_other() {
        for reason in ALL_REASONS {
            assert_eq!(
                NotificationDismissalBlockReason::from_code(reason.code()),
                reason
            );
            let json = serde_json::to_string(&reason).unwrap();
            assert_eq!(json, format!("\"{}\"", reason.code()));
            assert_eq!(
                serde_json::from_str::<NotificationDismissalBlockReason>(&json).unwrap(),
                reason
            );
        }
        assert_eq!(
            NotificationDismissalBlockReason::from_code("from_the_future"),
            NotificationDismissalBlockReason::Other
        );
        assert_eq!(
            serde_json::from_str::<NotificationDismissalBlockReason>("\"from_the_future\"")
                .unwrap(),
            NotificationDismissalBlockReason::Other
        );
    }
}
