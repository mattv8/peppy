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

fn challenge() -> serde_json::Value {
    json!({"type":"membership_challenge","version":1,"connection_id":CONNECTION_ID,"challenge_id":CHALLENGE_ID})
}

fn hint() -> serde_json::Value {
    json!({"type":"membership_hint","version":1,"membership_version":"1"})
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
    assert_eq!(
        MembershipControlV1::parse(&raw),
        Err(peppy_protocol::MembershipControlError::TooLarge)
    );

    let valid = serde_json::to_vec(&bootstrap()).unwrap();
    let exact = [
        valid.as_slice(),
        &vec![b' '; MAX_MEMBERSHIP_CONTROL_BYTES - valid.len()],
    ]
    .concat();
    assert_eq!(exact.len(), MAX_MEMBERSHIP_CONTROL_BYTES);
    assert!(MembershipControlV1::parse(&exact).is_ok());
    let oversized = [exact.as_slice(), b" "].concat();
    assert_eq!(oversized.len(), MAX_MEMBERSHIP_CONTROL_BYTES + 1);
    assert_eq!(
        MembershipControlV1::parse(&oversized),
        Err(peppy_protocol::MembershipControlError::TooLarge)
    );
}

#[test]
fn emitted_membership_schemas_match_runtime_version_and_id_boundaries() {
    let schema: serde_json::Value =
        serde_json::from_str(&peppy_protocol::generate_contracts().openapi).unwrap();
    for control in [bootstrap(), challenge(), response(), hint()] {
        assert_schema_and_union_accept(&schema, &control);
    }
    for version in ["1", "9223372036854775807"] {
        for mut control in [bootstrap(), response(), hint()] {
            control["membership_version"] = json!(version);
            assert_schema_and_union_accept(&schema, &control);
        }
    }
    for version in ["0", "01", "+1", "-1", "9223372036854775808"] {
        for mut control in [bootstrap(), response(), hint()] {
            control["membership_version"] = json!(version);
            assert_schema_and_union_reject(&schema, &control, version);
        }
    }
    for (id, expected_length) in [
        ("AAAAAAAAAAAAAAAAAAAAAB", 22),
        ("AAAAAAAAAAAAAAAAAAAAA", 21),
        ("AAAAAAAAAAAAAAAAAAAAAA=", 23),
    ] {
        assert_eq!(id.len(), expected_length);
        for mut control in [bootstrap(), challenge(), response()] {
            control["connection_id"] = json!(id);
            assert_schema_and_union_reject(&schema, &control, id);
        }
        for mut control in [challenge(), response()] {
            control["challenge_id"] = json!(id);
            assert_schema_and_union_reject(&schema, &control, id);
        }
    }
}

#[test]
fn membership_key_epoch_schema_matches_u32_runtime_bounds() {
    let schema: serde_json::Value =
        serde_json::from_str(&peppy_protocol::generate_contracts().openapi).unwrap();
    for value in [json!(1), json!(u32::MAX)] {
        let mut control = bootstrap();
        control["roster"][0]["key_epoch"] = value;
        assert_schema_and_union_accept(&schema, &control);
    }
    for value in [
        json!(0),
        json!(u64::from(u32::MAX) + 1),
        json!(-1),
        json!(1.5),
        json!("1"),
        json!(true),
        serde_json::Value::Null,
    ] {
        let mut control = bootstrap();
        control["roster"][0]["key_epoch"] = value;
        assert_schema_and_union_reject(&schema, &control, "key_epoch");
    }
}

fn assert_schema_and_union_accept(schema: &serde_json::Value, control: &serde_json::Value) {
    assert!(schema_validates(
        schema_for_control(schema, control),
        control
    ));
    assert!(schema_validates(
        schema_for_control(schema, &json!({})),
        control
    ));
    assert!(MembershipControlV1::parse(&serde_json::to_vec(control).unwrap()).is_ok());
}

fn assert_schema_and_union_reject(
    schema: &serde_json::Value,
    control: &serde_json::Value,
    case: &str,
) {
    assert!(
        !schema_validates(schema_for_control(schema, control), control),
        "standalone: {case}"
    );
    assert!(
        !schema_validates(schema_for_control(schema, &json!({})), control),
        "union: {case}"
    );
    assert!(
        MembershipControlV1::parse(&serde_json::to_vec(control).unwrap()).is_err(),
        "runtime: {case}"
    );
}

fn schema_for_control<'a>(
    schema: &'a serde_json::Value,
    control: &serde_json::Value,
) -> &'a serde_json::Value {
    let name = match control["type"].as_str() {
        Some("membership_bootstrap") => "MembershipBootstrapV1",
        Some("membership_challenge") => "MembershipChallengeV1",
        Some("membership_response") => "MembershipResponseV1",
        Some("membership_hint") => "MembershipHintV1",
        _ => "MembershipControlV1",
    };
    &schema["components"]["schemas"][name]
}

fn schema_validates(schema: &serde_json::Value, instance: &serde_json::Value) -> bool {
    jsonschema::validator_for(schema)
        .unwrap()
        .is_valid(instance)
}

#[test]
fn emitted_membership_schema_test_detects_a_weakened_pattern() {
    let mut schema: serde_json::Value =
        serde_json::from_str(&peppy_protocol::generate_contracts().openapi).unwrap();
    let invalid = json!({"type":"membership_bootstrap","version":1,"connection_id":"AAAAAAAAAAAAAAAAAAAAAB","membership_version":"1","roster":roster()});
    assert!(!schema_validates(
        schema_for_control(&schema, &invalid),
        &invalid
    ));
    schema["components"]["schemas"]["MembershipBootstrapV1"]["properties"]["connection_id"]["pattern"] =
        json!(".*");
    assert!(schema_validates(
        schema_for_control(&schema, &invalid),
        &invalid
    ));
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
