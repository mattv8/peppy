use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_protocol::{
    MAX_MEMBERSHIP_CONTROL_BYTES, MAX_MEMBERSHIP_ROSTER_ENTRIES, MembershipBootstrapV1,
    MembershipChallengeV1, MembershipControlV1, MembershipDeviceV1, MembershipHintV1,
    MembershipResponseV1, MembershipRoleV1, MembershipVersionV1,
};
use serde_json::json;

const CONNECTION_ID: &str = "AQEBAQEBAQEBAQEBAQEBAQ";
const CHALLENGE_ID: &str = "AgICAgICAgICAgICAgICAg";
const DEVICE_ID: &str = "00000000-0000-0000-0000-000000000001";

fn roster() -> serde_json::Value {
    json!([{"device_id":DEVICE_ID,"revoked":false,"key_epoch":1,"role":"device"}])
}

fn bootstrap() -> serde_json::Value {
    json!({"type":"membership_bootstrap","version":1,"connection_id":CONNECTION_ID,"membership_version":"1","roster":roster()})
}

fn response() -> serde_json::Value {
    json!({"type":"membership_response","version":1,"connection_id":CONNECTION_ID,"challenge_id":CHALLENGE_ID,"membership_version":"1","roster":roster()})
}

#[test]
fn membership_controls_round_trip_with_distinct_provisional_bootstrap_and_response() {
    let bootstrap_control: MembershipControlV1 = serde_json::from_value(bootstrap()).unwrap();
    let response_control: MembershipControlV1 = serde_json::from_value(response()).unwrap();
    assert!(matches!(
        bootstrap_control,
        MembershipControlV1::Bootstrap(_)
    ));
    assert!(matches!(response_control, MembershipControlV1::Response(_)));
    assert_ne!(bootstrap_control, response_control);
    assert_eq!(
        serde_json::to_value(&bootstrap_control).unwrap(),
        bootstrap()
    );
    assert_eq!(serde_json::to_value(&response_control).unwrap(), response());
}

#[test]
fn membership_control_parsing_rejects_oversized_raw_input_before_json_decode() {
    let raw = vec![b' '; MAX_MEMBERSHIP_CONTROL_BYTES + 1];
    assert!(MembershipControlV1::parse(&raw).is_err());
}

#[test]
fn membership_controls_reject_unknown_duplicate_and_missing_fields() {
    for mut control in [bootstrap(), response()] {
        control["unexpected"] = json!(true);
        assert!(serde_json::from_value::<MembershipControlV1>(control).is_err());
    }
    assert!(serde_json::from_str::<MembershipControlV1>(
        r#"{"type":"membership_hint","version":1,"membership_version":"1","membership_version":"2"}"#
    )
    .is_err());
    let mut missing = bootstrap();
    missing.as_object_mut().unwrap().remove("roster");
    assert!(serde_json::from_value::<MembershipControlV1>(missing).is_err());
}

#[test]
fn membership_controls_enforce_canonical_ids_versions_and_roster_bounds() {
    for field in ["connection_id", "challenge_id"] {
        let mut control = response();
        control[field] = json!("AAAAAAAAAAAAAAAAAAAAAA==");
        assert!(
            serde_json::from_value::<MembershipControlV1>(control).is_err(),
            "{field}"
        );
    }
    for version in ["0", "01", "9223372036854775808", "-1"] {
        let mut control = bootstrap();
        control["membership_version"] = json!(version);
        assert!(
            serde_json::from_value::<MembershipControlV1>(control).is_err(),
            "{version}"
        );
    }
    let mut duplicate = bootstrap();
    duplicate["roster"] = json!([
        {"device_id":DEVICE_ID,"revoked":false,"key_epoch":1,"role":"device"},
        {"device_id":DEVICE_ID,"revoked":true,"key_epoch":2,"role":"owner"}
    ]);
    assert!(serde_json::from_value::<MembershipControlV1>(duplicate).is_err());

    let mut oversized = Vec::with_capacity(MAX_MEMBERSHIP_ROSTER_ENTRIES + 1);
    for index in 0..=MAX_MEMBERSHIP_ROSTER_ENTRIES {
        oversized.push(json!({"device_id":format!("00000000-0000-0000-0000-{:012x}", index + 1),"revoked":false,"key_epoch":1,"role":"device"}));
    }
    let mut control = bootstrap();
    control["roster"] = json!(oversized);
    assert!(serde_json::from_value::<MembershipControlV1>(control).is_err());
}

#[test]
fn membership_device_keeps_revoked_entries_and_rejects_invalid_values() {
    let device = json!({"device_id":DEVICE_ID,"revoked":true,"key_epoch":1,"role":"gateway"});
    let parsed: MembershipDeviceV1 = serde_json::from_value(device.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), device);
    for (field, value) in [
        ("device_id", json!("00000000-0000-0000-0000-00000000000A")),
        ("revoked", json!("false")),
        ("key_epoch", json!(0)),
        ("role", json!("admin")),
    ] {
        let mut invalid = device.clone();
        invalid[field] = value;
        assert!(
            serde_json::from_value::<MembershipDeviceV1>(invalid).is_err(),
            "{field}"
        );
    }
}

#[test]
fn membership_dto_serialization_is_exact() {
    let device = MembershipDeviceV1 {
        device_id: DEVICE_ID.parse().unwrap(),
        revoked: false,
        key_epoch: 1,
        role: MembershipRoleV1::Device,
    };
    let version = MembershipVersionV1::parse("1").unwrap();
    assert_eq!(
        serde_json::to_string(&MembershipBootstrapV1 {
            r#type: "membership_bootstrap".into(),
            version: 1,
            connection_id: CONNECTION_ID.into(),
            membership_version: version.clone(),
            roster: vec![device.clone()],
        })
        .unwrap(),
        format!(
            r#"{{"type":"membership_bootstrap","version":1,"connection_id":"{CONNECTION_ID}","membership_version":"1","roster":[{{"device_id":"{DEVICE_ID}","revoked":false,"key_epoch":1,"role":"device"}}]}}"#
        )
    );
    assert_eq!(
        serde_json::to_string(&MembershipChallengeV1 {
            r#type: "membership_challenge".into(),
            version: 1,
            connection_id: CONNECTION_ID.into(),
            challenge_id: CHALLENGE_ID.into()
        })
        .unwrap(),
        format!(
            r#"{{"type":"membership_challenge","version":1,"connection_id":"{CONNECTION_ID}","challenge_id":"{CHALLENGE_ID}"}}"#
        )
    );
    assert_eq!(
        serde_json::to_string(&MembershipResponseV1 {
            r#type: "membership_response".into(),
            version: 1,
            connection_id: CONNECTION_ID.into(),
            challenge_id: CHALLENGE_ID.into(),
            membership_version: version.clone(),
            roster: vec![device]
        })
        .unwrap(),
        format!(
            r#"{{"type":"membership_response","version":1,"connection_id":"{CONNECTION_ID}","challenge_id":"{CHALLENGE_ID}","membership_version":"1","roster":[{{"device_id":"{DEVICE_ID}","revoked":false,"key_epoch":1,"role":"device"}}]}}"#
        )
    );
    assert_eq!(
        serde_json::to_string(&MembershipHintV1 {
            r#type: "membership_hint".into(),
            version: 1,
            membership_version: version
        })
        .unwrap(),
        r#"{"type":"membership_hint","version":1,"membership_version":"1"}"#
    );
    assert_eq!(URL_SAFE_NO_PAD.decode(CONNECTION_ID).unwrap().len(), 16);
}
