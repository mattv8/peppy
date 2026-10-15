//! Sanitized gateway capability projections shared by native and browser hosts.
use crate::{GatewayAvailability, GatewayPresence, GatewayPresenceState, GatewayView};
use serde::Deserialize;
use std::collections::HashSet;

const MAX_GATEWAYS: usize = 16;
const MAX_SIMS: usize = 8;
const MAX_SUBSCRIPTION_CHARS: usize = 128;
const MAX_LABEL_CHARS: usize = 48;
const FALLBACK_MMS_MAX_BYTES: u64 = 300 * 1024;
const MAX_MMS_BYTES: u64 = 10 * 1024 * 1024;
const MAX_MMS_RECIPIENTS: usize = 20;

#[derive(Deserialize)]
pub struct DevicesResponse {
    pub devices: Vec<DeviceRow>,
}
#[derive(Deserialize)]
pub struct DeviceRow {
    pub device_id: String,
    pub role: String,
    pub revoked: bool,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    presence: Option<serde_json::Value>,
    #[serde(default)]
    wake_route: bool,
}
#[derive(Deserialize)]
pub struct CapabilitiesResponse {
    pub capabilities: Vec<CapabilityRow>,
}
#[derive(Deserialize)]
pub struct CapabilityRow {
    pub device_id: String,
    pub simulator: bool,
    pub capabilities: serde_json::Value,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Available,
    PermissionRequired,
    ApprovalRequired,
    RegionRestricted,
    Experimental,
    Unsupported,
    #[serde(other)]
    Unknown,
}
impl Status {
    fn describe(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::PermissionRequired => "needs permission on the phone",
            Self::ApprovalRequired => "awaiting approval",
            Self::RegionRestricted => "region restricted",
            Self::Experimental => "disabled",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Deserialize)]
struct SimReport {
    subscription_id: String,
    #[serde(default)]
    label: Option<String>,
    sms: Status,
    mms: Status,
    #[serde(default)]
    mms_content_version: Option<serde_json::Value>,
    #[serde(default)]
    mms_max_bytes: Option<serde_json::Value>,
    #[serde(default)]
    mms_limit_source: Option<serde_json::Value>,
    #[serde(default)]
    mms_max_recipients: Option<serde_json::Value>,
}
fn bounded_u64(value: Option<&serde_json::Value>, min: u64, max: u64) -> Option<u64> {
    value?
        .as_u64()
        .filter(|value| *value >= min && *value <= max)
}
fn mms_limits(sim: &SimReport) -> (Option<u32>, Option<u64>, Option<String>, Option<usize>) {
    let version =
        bounded_u64(sim.mms_content_version.as_ref(), 1, u32::MAX as u64).map(|value| value as u32);
    let max_bytes = bounded_u64(sim.mms_max_bytes.as_ref(), 1, MAX_MMS_BYTES);
    let source = Some(
        match (
            max_bytes,
            sim.mms_limit_source
                .as_ref()
                .and_then(|value| value.as_str()),
        ) {
            (Some(_), Some("carrier")) => "carrier",
            _ => "fallback",
        }
        .into(),
    );
    let recipients = bounded_u64(
        sim.mms_max_recipients.as_ref(),
        1,
        MAX_MMS_RECIPIENTS as u64,
    )
    .map(|value| value as usize)
    .or(Some(MAX_MMS_RECIPIENTS));
    (
        version,
        max_bytes.or(Some(FALLBACK_MMS_MAX_BYTES)),
        source,
        recipients,
    )
}
fn valid_optional_mms_fields(sim: &SimReport) -> bool {
    sim.mms_limit_source
        .as_ref()
        .is_none_or(|value| matches!(value.as_str(), Some("carrier" | "fallback")))
        && sim
            .mms_content_version
            .as_ref()
            .is_none_or(|value| bounded_u64(Some(value), 1, u32::MAX as u64).is_some())
        && sim
            .mms_max_bytes
            .as_ref()
            .is_none_or(|value| bounded_u64(Some(value), 1, MAX_MMS_BYTES).is_some())
        && sim
            .mms_max_recipients
            .as_ref()
            .is_none_or(|value| bounded_u64(Some(value), 1, MAX_MMS_RECIPIENTS as u64).is_some())
}
fn clean_label(value: &str) -> Option<String> {
    let value = value
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL_CHARS)
        .collect::<String>();
    (!value.trim().is_empty()).then(|| value.trim().into())
}
fn valid_subscription(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_SUBSCRIPTION_CHARS
        && value.chars().all(|c| c.is_ascii_graphic())
}
enum SimReports {
    Valid(Vec<SimReport>),
    Empty,
    Invalid,
}

fn sims(value: &serde_json::Value) -> SimReports {
    let Some(list) = value.get("sims").and_then(serde_json::Value::as_array) else {
        return SimReports::Invalid;
    };
    if list.is_empty() {
        return SimReports::Empty;
    }
    if list.len() > MAX_SIMS {
        return SimReports::Invalid;
    }
    let sims: Option<Vec<SimReport>> = list
        .iter()
        .map(|item| serde_json::from_value::<SimReport>(item.clone()).ok())
        .collect::<Option<Vec<SimReport>>>()
        .filter(|sims| {
            sims.iter()
                .all(|sim| valid_subscription(&sim.subscription_id))
        });
    sims.map_or(SimReports::Invalid, SimReports::Valid)
}

fn presence(value: Option<&serde_json::Value>) -> Option<GatewayPresence> {
    let value = value?;
    let state = match value.get("state")?.as_str()? {
        "connected" => GatewayPresenceState::Connected,
        "seen" => GatewayPresenceState::Seen,
        "never" => GatewayPresenceState::Never,
        _ => return None,
    };
    let age = match value.get("last_seen_age_seconds")? {
        serde_json::Value::Null => None,
        value => Some(value.as_u64()?),
    };
    Some(GatewayPresence {
        state,
        last_seen_age_seconds: age,
    })
}

fn availability(
    kind: Option<&str>,
    report_exists: bool,
    reports: &SimReports,
    presence: Option<&GatewayPresence>,
    wake_route: bool,
    sms: Option<Status>,
) -> GatewayAvailability {
    if kind == Some("ios") {
        return GatewayAvailability::UnsupportedPlatform;
    }
    if matches!(
        presence.map(|value| value.state),
        Some(GatewayPresenceState::Never)
    ) && !report_exists
    {
        return GatewayAvailability::NeverConnected;
    }
    match reports {
        SimReports::Empty => return GatewayAvailability::NoSmsSim,
        SimReports::Valid(_) => match sms {
            Some(Status::PermissionRequired) => {
                return GatewayAvailability::SmsPermissionNeeded;
            }
            Some(Status::Available) => {}
            Some(Status::Unknown) => {}
            Some(_) => return GatewayAvailability::SmsUnavailable,
            None => return GatewayAvailability::CapabilitiesUnknown,
        },
        SimReports::Invalid => {}
    }
    let presence_is_offline = match presence {
        Some(GatewayPresence {
            state: GatewayPresenceState::Never,
            ..
        }) => true,
        Some(GatewayPresence {
            state: GatewayPresenceState::Seen,
            last_seen_age_seconds: Some(age),
        }) => *age > 2_700 && !(wake_route && *age <= 86_400),
        Some(GatewayPresence {
            state: GatewayPresenceState::Seen,
            last_seen_age_seconds: None,
        }) => true,
        _ => false,
    };
    if presence_is_offline {
        return GatewayAvailability::Offline;
    }
    if matches!(reports, SimReports::Invalid) || sms == Some(Status::Unknown) {
        return GatewayAvailability::CapabilitiesUnknown;
    }
    match presence {
        Some(GatewayPresence {
            state: GatewayPresenceState::Connected,
            ..
        })
        | Some(GatewayPresence {
            state: GatewayPresenceState::Seen,
            last_seen_age_seconds: Some(..=90),
        }) => GatewayAvailability::Connected,
        Some(GatewayPresence {
            state: GatewayPresenceState::Seen,
            last_seen_age_seconds: Some(..=2_700),
        }) => GatewayAvailability::Delayed,
        Some(GatewayPresence {
            state: GatewayPresenceState::Seen,
            last_seen_age_seconds: Some(..=86_400),
        }) if wake_route => GatewayAvailability::Delayed,
        Some(_) => GatewayAvailability::Offline,
        None => GatewayAvailability::Unknown,
    }
}

/// Returns a role only for this exact active roster device.
pub fn extract_device_role(devices: &DevicesResponse, device_id: &str) -> Option<String> {
    devices
        .devices
        .iter()
        .find(|device| device.device_id == device_id && !device.revoked)
        .map(|device| device.role.clone())
}

/// Projects server capability and presence reports without treating legacy fields as liveness proof.
pub fn gateway_views(
    devices: &DevicesResponse,
    capabilities: &CapabilitiesResponse,
) -> Vec<GatewayView> {
    let capable: HashSet<&str> = capabilities
        .capabilities
        .iter()
        .map(|row| row.device_id.as_str())
        .collect();
    let mut views = Vec::new();
    for device in devices
        .devices
        .iter()
        .filter(|device| {
            !device.revoked
                && (device.role == "gateway"
                    || device.role == "owner"
                        && (capable.contains(device.device_id.as_str())
                            || matches!(device.kind.as_deref(), Some("android" | "ios"))))
        })
        .take(MAX_GATEWAYS)
    {
        let Ok(id) = uuid::Uuid::parse_str(&device.device_id) else {
            continue;
        };
        let short = id.simple().to_string()[..8].to_owned();
        let report = capabilities
            .capabilities
            .iter()
            .find(|row| uuid::Uuid::parse_str(&row.device_id).is_ok_and(|other| other == id));
        let simulated = report.is_some_and(|row| row.simulator);
        let reports = report.map_or(SimReports::Invalid, |row| sims(&row.capabilities));
        let presence = presence(device.presence.as_ref());
        let wake_route = device.wake_route;
        match &reports {
            SimReports::Valid(sims) => {
                for sim in sims {
                    let limits = mms_limits(&sim);
                    let supports_sms = sim.sms == Status::Available;
                    let supports_mms = supports_sms
                        && sim.mms == Status::Available
                        && valid_optional_mms_fields(&sim)
                        && limits.0.is_some_and(|version| version >= 2);
                    let name = clean_label(sim.label.as_deref().unwrap_or(""))
                        .unwrap_or_else(|| format!("Phone {short}"));
                    let mut note = format!(
                        "SMS {}; MMS {}. Presence is not reported by the server; commands wait durably until the gateway syncs.",
                        sim.sms.describe(),
                        sim.mms.describe()
                    );
                    if simulated {
                        note = format!("SIMULATED gateway: carrier delivery is simulated. {note}");
                    }
                    let availability = availability(
                        device.kind.as_deref(),
                        true,
                        &reports,
                        presence.as_ref(),
                        wake_route,
                        Some(sim.sms),
                    );
                    views.push(GatewayView {
                        id: id.to_string(),
                        name: if simulated {
                            format!("{name} (simulated)")
                        } else {
                            name
                        },
                        sim_id: sim.subscription_id.clone(),
                        online: availability == GatewayAvailability::Connected,
                        simulated,
                        supports_sms,
                        supports_mms,
                        availability,
                        last_seen_age_seconds: presence
                            .as_ref()
                            .and_then(|value| value.last_seen_age_seconds),
                        presence: presence.clone(),
                        wake_route,
                        capability_note: Some(note),
                        mms_content_version: limits.0,
                        mms_max_bytes: limits.1,
                        mms_limit_source: limits.2,
                        mms_max_recipients: limits.3,
                    });
                }
            }
            SimReports::Empty | SimReports::Invalid => {
                let availability = availability(
                    device.kind.as_deref(),
                    report.is_some(),
                    &reports,
                    presence.as_ref(),
                    wake_route,
                    None,
                );
                views.push(GatewayView {
                    id: id.to_string(),
                    name: format!("Phone {short}"),
                    sim_id: String::new(),
                    online: availability == GatewayAvailability::Connected,
                    simulated,
                    supports_sms: false,
                    supports_mms: false,
                    availability,
                    last_seen_age_seconds: presence
                        .as_ref()
                        .and_then(|value| value.last_seen_age_seconds),
                    presence,
                    wake_route,
                    capability_note: Some(
                        "This phone has not reported a sendable SIM route; the draft was kept."
                            .into(),
                    ),
                    mms_content_version: None,
                    mms_max_bytes: None,
                    mms_limit_source: None,
                    mms_max_recipients: None,
                });
            }
        }
    }
    views
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::find_route;
    #[test]
    fn valid_v2_capability_enables_a_route_but_unknown_presence_is_offline() {
        let id = uuid::Uuid::new_v4().to_string();
        let devices: DevicesResponse = serde_json::from_value(
            serde_json::json!({"devices":[{"device_id":id,"role":"gateway","revoked":false}]}),
        )
        .unwrap();
        let caps: CapabilitiesResponse = serde_json::from_value(serde_json::json!({"capabilities":[{"device_id":id,"simulator":false,"capabilities":{"sims":[{"subscription_id":"sim","sms":"available","mms":"available","mms_content_version":2}]}}]})).unwrap();
        let view = gateway_views(&devices, &caps).pop().unwrap();
        assert!(view.supports_sms && view.supports_mms && !view.online);
        assert!(find_route(&[view], &id, "sim").is_some());
    }
    #[test]
    fn malformed_mms_never_enables_mms_and_revoked_self_has_no_role() {
        let id = uuid::Uuid::new_v4().to_string();
        let devices: DevicesResponse = serde_json::from_value(
            serde_json::json!({"devices":[{"device_id":id,"role":"gateway","revoked":true}]}),
        )
        .unwrap();
        let caps: CapabilitiesResponse = serde_json::from_value(serde_json::json!({"capabilities":[{"device_id":id,"simulator":false,"capabilities":{"sims":[{"subscription_id":"sim","sms":"available","mms":"available","mms_content_version":"2"}]}}]})).unwrap();
        assert!(gateway_views(&devices, &caps).is_empty());
        assert_eq!(extract_device_role(&devices, &id), None);
    }

    #[test]
    fn mobile_owners_without_capabilities_are_unavailable_gateways() {
        let android = uuid::Uuid::new_v4().to_string();
        let ios = uuid::Uuid::new_v4().to_string();
        let enabled = uuid::Uuid::new_v4().to_string();
        let devices: DevicesResponse = serde_json::from_value(serde_json::json!({"devices":[
            {"device_id":android,"role":"owner","kind":"android","revoked":false},
            {"device_id":ios,"role":"owner","kind":"ios","revoked":false},
            {"device_id":enabled,"role":"owner","kind":"android","revoked":false}
        ]}))
        .unwrap();
        let caps: CapabilitiesResponse = serde_json::from_value(serde_json::json!({"capabilities":[{
            "device_id":enabled,"simulator":false,"capabilities":{"sims":[{"subscription_id":"sim","sms":"available","mms":"available","mms_content_version":2}]}
        }]}))
        .unwrap();

        let views = gateway_views(&devices, &caps);

        assert_eq!(views.len(), 3);
        let enabled_views = views
            .iter()
            .filter(|view| view.id == enabled)
            .collect::<Vec<_>>();
        assert_eq!(enabled_views.len(), 1);
        assert!(enabled_views[0].supports_sms && enabled_views[0].supports_mms);
        for id in [&android, &ios] {
            let view = views.iter().find(|view| view.id == *id).unwrap();
            assert!(!view.supports_sms && !view.supports_mms);
            assert_eq!(view.sim_id, "");
        }
    }

    #[test]
    fn non_mobile_or_revoked_owners_without_capabilities_are_excluded() {
        let devices: DevicesResponse = serde_json::from_value(serde_json::json!({"devices":[
            {"device_id":uuid::Uuid::new_v4(),"role":"owner","revoked":false},
            {"device_id":uuid::Uuid::new_v4(),"role":"owner","kind":null,"revoked":false},
            {"device_id":uuid::Uuid::new_v4(),"role":"owner","kind":"desktop","revoked":false},
            {"device_id":uuid::Uuid::new_v4(),"role":"owner","kind":"browser","revoked":false},
            {"device_id":uuid::Uuid::new_v4(),"role":"owner","kind":"future-phone","revoked":false},
            {"device_id":uuid::Uuid::new_v4(),"role":"owner","kind":"android","revoked":true}
        ]}))
        .unwrap();
        let caps: CapabilitiesResponse =
            serde_json::from_value(serde_json::json!({"capabilities":[]})).unwrap();

        assert!(gateway_views(&devices, &caps).is_empty());
    }

    #[test]
    fn availability_precedence_and_presence_boundaries_are_truthful() {
        let valid = SimReports::Valid(Vec::new());
        let invalid = SimReports::Invalid;
        let seen = |age| GatewayPresence {
            state: GatewayPresenceState::Seen,
            last_seen_age_seconds: Some(age),
        };

        assert_eq!(
            availability(Some("ios"), false, &invalid, None, false, None),
            GatewayAvailability::UnsupportedPlatform
        );
        assert_eq!(
            availability(
                None,
                false,
                &invalid,
                Some(&GatewayPresence {
                    state: GatewayPresenceState::Never,
                    last_seen_age_seconds: None
                }),
                false,
                None
            ),
            GatewayAvailability::NeverConnected
        );
        assert_eq!(
            availability(
                None,
                true,
                &valid,
                Some(&seen(1)),
                false,
                Some(Status::PermissionRequired)
            ),
            GatewayAvailability::SmsPermissionNeeded
        );
        assert_eq!(
            availability(
                None,
                true,
                &valid,
                Some(&seen(1)),
                false,
                Some(Status::Unsupported)
            ),
            GatewayAvailability::SmsUnavailable
        );
        assert_eq!(
            availability(None, true, &SimReports::Empty, Some(&seen(1)), false, None),
            GatewayAvailability::NoSmsSim
        );
        assert_eq!(
            availability(
                None,
                true,
                &valid,
                Some(&seen(90)),
                false,
                Some(Status::Available)
            ),
            GatewayAvailability::Connected
        );
        assert_eq!(
            availability(
                None,
                true,
                &valid,
                Some(&seen(91)),
                false,
                Some(Status::Available)
            ),
            GatewayAvailability::Delayed
        );
        assert_eq!(
            availability(
                None,
                true,
                &valid,
                Some(&seen(2_700)),
                false,
                Some(Status::Available)
            ),
            GatewayAvailability::Delayed
        );
        assert_eq!(
            availability(
                None,
                true,
                &valid,
                Some(&seen(2_701)),
                false,
                Some(Status::Available)
            ),
            GatewayAvailability::Offline
        );
        assert_eq!(
            availability(
                None,
                true,
                &valid,
                Some(&seen(86_400)),
                true,
                Some(Status::Available)
            ),
            GatewayAvailability::Delayed
        );
        assert_eq!(
            availability(
                None,
                true,
                &valid,
                Some(&seen(86_401)),
                true,
                Some(Status::Available)
            ),
            GatewayAvailability::Offline
        );
        assert_eq!(
            availability(None, true, &invalid, Some(&seen(2_701)), false, None),
            GatewayAvailability::Offline
        );
        assert_eq!(
            availability(None, true, &invalid, Some(&seen(1)), false, None),
            GatewayAvailability::CapabilitiesUnknown
        );
        assert_eq!(
            availability(
                None,
                true,
                &valid,
                Some(&seen(1)),
                false,
                Some(Status::Unknown)
            ),
            GatewayAvailability::CapabilitiesUnknown
        );
        assert_eq!(
            availability(
                None,
                true,
                &valid,
                Some(&seen(2_701)),
                false,
                Some(Status::Unknown)
            ),
            GatewayAvailability::Offline
        );
        assert_eq!(
            availability(None, true, &valid, None, false, Some(Status::Available)),
            GatewayAvailability::Unknown
        );
    }

    #[test]
    fn empty_and_malformed_reports_keep_distinct_placeholder_states() {
        let id = uuid::Uuid::new_v4().to_string();
        let devices: DevicesResponse = serde_json::from_value(serde_json::json!({"devices":[
            {"device_id":id,"role":"gateway","revoked":false,"presence":{"state":"never","last_seen_age_seconds":null}}
        ]})).unwrap();
        let empty: CapabilitiesResponse = serde_json::from_value(serde_json::json!({"capabilities":[{"device_id":id,"simulator":false,"capabilities":{"sims":[]}}]})).unwrap();
        let empty_view = gateway_views(&devices, &empty).pop().unwrap();
        assert_eq!(empty_view.availability, GatewayAvailability::NoSmsSim);
        assert_eq!(empty_view.sim_id, "");

        let malformed: CapabilitiesResponse = serde_json::from_value(serde_json::json!({"capabilities":[{"device_id":id,"simulator":false,"capabilities":{"sims":[{"subscription_id":"sim","sms":"available"}]}}]})).unwrap();
        let malformed_view = gateway_views(&devices, &malformed).pop().unwrap();
        assert_eq!(malformed_view.availability, GatewayAvailability::Offline);
        assert_eq!(
            malformed_view.presence.unwrap().state,
            GatewayPresenceState::Never
        );
    }

    #[test]
    fn unknown_future_mms_status_does_not_disable_valid_sms() {
        let id = uuid::Uuid::new_v4().to_string();
        let devices: DevicesResponse = serde_json::from_value(serde_json::json!({"devices":[
            {"device_id":id,"role":"gateway","revoked":false}
        ]}))
        .unwrap();
        let caps: CapabilitiesResponse =
            serde_json::from_value(serde_json::json!({"capabilities":[{
                "device_id":id,"simulator":false,"capabilities":{"sims":[{
                    "subscription_id":"sim","sms":"available","mms":"future_status"
                }]}
            }]}))
            .unwrap();

        let view = gateway_views(&devices, &caps).pop().unwrap();

        assert!(view.supports_sms);
        assert!(!view.supports_mms);
        assert_eq!(view.availability, GatewayAvailability::Unknown);
    }
}
