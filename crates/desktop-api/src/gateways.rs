//! Sanitized gateway capability projections shared by native and browser hosts.
use crate::GatewayView;
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
fn sims(value: &serde_json::Value) -> Option<Vec<SimReport>> {
    let list = value.get("sims")?.as_array()?;
    if list.is_empty() || list.len() > MAX_SIMS {
        return None;
    }
    let sims: Vec<SimReport> = list
        .iter()
        .map(|item| serde_json::from_value::<SimReport>(item.clone()).ok())
        .collect::<Option<_>>()?;
    sims.iter()
        .all(|sim| valid_subscription(&sim.subscription_id))
        .then_some(sims)
}

/// Returns a role only for this exact active roster device.
pub fn extract_device_role(devices: &DevicesResponse, device_id: &str) -> Option<String> {
    devices
        .devices
        .iter()
        .find(|device| device.device_id == device_id && !device.revoked)
        .map(|device| device.role.clone())
}

/// Projects valid server capability reports. Server presence is unknown, so `online` is always false.
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
                    || device.role == "owner" && capable.contains(device.device_id.as_str()))
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
        if let Some(sims) = report.and_then(|row| sims(&row.capabilities)) {
            for sim in sims {
                let limits = mms_limits(&sim);
                let supports_sms = sim.sms == Status::Available;
                let supports_mms = supports_sms
                    && sim.mms == Status::Available
                    && valid_optional_mms_fields(&sim)
                    && limits.0.is_some_and(|version| version >= 2);
                let name = clean_label(sim.label.as_deref().unwrap_or(""))
                    .unwrap_or_else(|| format!("Gateway {short}"));
                let mut note = format!(
                    "SMS {}; MMS {}. Presence is not reported by the server; commands wait durably until the gateway syncs.",
                    sim.sms.describe(),
                    sim.mms.describe()
                );
                if simulated {
                    note = format!("SIMULATED gateway: carrier delivery is simulated. {note}");
                }
                views.push(GatewayView {
                    id: id.to_string(),
                    name: if simulated {
                        format!("{name} (simulated)")
                    } else {
                        name
                    },
                    sim_id: sim.subscription_id,
                    online: false,
                    simulated,
                    supports_sms,
                    supports_mms,
                    capability_note: Some(note),
                    mms_content_version: limits.0,
                    mms_max_bytes: limits.1,
                    mms_limit_source: limits.2,
                    mms_max_recipients: limits.3,
                });
            }
        } else {
            views.push(GatewayView { id: id.to_string(), name: format!("Gateway {short}"), sim_id: String::new(), online: false, simulated, supports_sms: false, supports_mms: false, capability_note: Some("This gateway has not reported valid SIM capabilities; sending through it is disabled.".into()), mms_content_version: None, mms_max_bytes: None, mms_limit_source: None, mms_max_recipients: None });
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
}
