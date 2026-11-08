use base64::Engine as _;
use peppy_domain::{CommandId, DeviceId, EnvelopeId, SourceSequence, VaultId};
use peppy_protocol::{
    CompactionMetadata, CompactionReference, Envelope, EnvelopeError, EnvelopePurpose,
    GatewayRoute, PairingQrRecord,
};
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

fn envelope() -> Envelope {
    Envelope {
        protocol_version: 1,
        envelope_id: EnvelopeId::new(),
        command_id: Some(CommandId::new()),
        vault_id: VaultId::new(),
        producer_device_id: DeviceId::new(),
        producer_sequence: SourceSequence(u64::MAX),
        key_epoch: 1,
        crypto_suite: 1,
        profile_fingerprint: "a".repeat(64),
        purpose: EnvelopePurpose::Command,
        route: Some(GatewayRoute::Carrier {
            gateway_device_id: DeviceId::new(),
            subscription_id: "sim:42:generation:7".into(),
        }),
        compaction: None,
        origin_tag: None,
        ciphertext: vec![1, 2, 3],
    }
}

fn legacy_carrier_envelope() -> Envelope {
    Envelope {
        protocol_version: 1,
        envelope_id: "00000000-0000-0000-0000-000000000001".parse().unwrap(),
        command_id: Some("00000000-0000-0000-0000-000000000002".parse().unwrap()),
        vault_id: "00000000-0000-0000-0000-000000000003".parse().unwrap(),
        producer_device_id: "00000000-0000-0000-0000-000000000004".parse().unwrap(),
        producer_sequence: SourceSequence(9),
        key_epoch: 2,
        crypto_suite: 3,
        profile_fingerprint: "a".repeat(64),
        purpose: EnvelopePurpose::Command,
        route: Some(GatewayRoute::Carrier {
            gateway_device_id: "00000000-0000-0000-0000-000000000005".parse().unwrap(),
            subscription_id: "sim:fixture".into(),
        }),
        compaction: None,
        origin_tag: None,
        ciphertext: vec![1, 2, 3],
    }
}

fn rcs_link_envelope() -> Envelope {
    let mut envelope = legacy_carrier_envelope();
    envelope.route = Some(GatewayRoute::RcsLink {
        gateway_device_id: "00000000-0000-0000-0000-000000000005".parse().unwrap(),
    });
    envelope
}

#[test]
fn carrier_route_json_is_byte_stable_fixture() {
    let envelope = legacy_carrier_envelope();
    assert_eq!(
        serde_json::to_string(&envelope.route).unwrap(),
        r#"{"gateway_device_id":"00000000-0000-0000-0000-000000000005","subscription_id":"sim:fixture"}"#
    );
}

#[test]
fn carrier_route_aad_is_byte_stable_fixture() {
    let envelope = legacy_carrier_envelope();
    assert_eq!(
        envelope.aad_bytes().unwrap(),
        base64::engine::general_purpose::STANDARD
            .decode("cGVwcHktZW52ZWxvcGUtYWFkLXYxAAABAAAAAAAAAAAAAAAAAAAAAQAAAAAAAAAAAAAAAAAAAAMAAAAAAAAAAAAAAAAAAAAEAAAAAAAAAAkAAAACAANhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhAQEAAAAAAAAAAAAAAAAAAAACAQAAAAAAAAAAAAAAAAAAAAUAC3NpbTpmaXh0dXJl")
            .unwrap(),
    );
    assert_eq!(
        envelope.wire_digest().unwrap(),
        [
            0x12, 0x86, 0x46, 0xe3, 0xb4, 0x68, 0xbd, 0x6b, 0x0a, 0x32, 0x2c, 0x28, 0x72, 0x96,
            0xfa, 0xe9, 0x60, 0x35, 0x30, 0x59, 0x52, 0x59, 0xc4, 0x07, 0x3f, 0xc0, 0x83, 0xfa,
            0x10, 0xa6, 0x76, 0x5d,
        ]
    );
}

#[derive(serde::Deserialize)]
// Mirrors the pre-R1 `GatewayRoute` shape at 3b61c27 exactly.
struct LegacyGatewayRoute {
    gateway_device_id: DeviceId,
    subscription_id: String,
}

#[test]
fn carrier_route_cross_version_round_trip() {
    let legacy_json = r#"{"gateway_device_id":"00000000-0000-0000-0000-000000000005","subscription_id":"sim:fixture"}"#;
    let route = serde_json::from_str::<GatewayRoute>(legacy_json).unwrap();
    assert_eq!(serde_json::to_string(&route).unwrap(), legacy_json);

    let legacy =
        serde_json::from_str::<LegacyGatewayRoute>(&serde_json::to_string(&route).unwrap())
            .unwrap();
    assert_eq!(
        legacy.gateway_device_id.to_string(),
        "00000000-0000-0000-0000-000000000005"
    );
    assert_eq!(legacy.subscription_id, "sim:fixture");
}

#[test]
fn rcs_link_route_requires_known_kind() {
    let gateway_device_id = "00000000-0000-0000-0000-000000000005";
    let route = GatewayRoute::RcsLink {
        gateway_device_id: gateway_device_id.parse().unwrap(),
    };
    assert_eq!(
        serde_json::to_string(&route).unwrap(),
        format!(r#"{{"kind":"rcs_link","gateway_device_id":"{gateway_device_id}"}}"#),
    );
    let unknown_kind = format!(r#"{{"kind":"unknown","gateway_device_id":"{gateway_device_id}"}}"#);
    assert!(serde_json::from_str::<GatewayRoute>(&unknown_kind).is_err());
    let missing_kind = format!(r#"{{"gateway_device_id":"{gateway_device_id}"}}"#);
    assert!(serde_json::from_str::<GatewayRoute>(&missing_kind).is_err());
}

#[test]
fn legacy_parser_rejects_rcs_link_route() {
    let gateway_device_id = "00000000-0000-0000-0000-000000000005";
    let route = format!(r#"{{"kind":"rcs_link","gateway_device_id":"{gateway_device_id}"}}"#);
    assert!(
        serde_json::from_str::<LegacyGatewayRoute>(&route)
            .err()
            .unwrap()
            .to_string()
            .contains("subscription_id")
    );
    let extra_subscription = format!(
        r#"{{"kind":"rcs_link","gateway_device_id":"{gateway_device_id}","subscription_id":"sim:fixture"}}"#
    );
    assert!(serde_json::from_str::<GatewayRoute>(&extra_subscription).is_err());
}

#[test]
fn rcs_link_route_aad_uses_a_distinct_route_presence_marker() {
    let carrier = legacy_carrier_envelope();
    let rcs_link = rcs_link_envelope();
    assert_eq!(rcs_link.validate(), Ok(()));
    assert_eq!(
        rcs_link.aad_bytes().unwrap(),
        base64::engine::general_purpose::STANDARD
            .decode("cGVwcHktZW52ZWxvcGUtYWFkLXYxAAABAAAAAAAAAAAAAAAAAAAAAQAAAAAAAAAAAAAAAAAAAAMAAAAAAAAAAAAAAAAAAAAEAAAAAAAAAAkAAAACAANhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhAQEAAAAAAAAAAAAAAAAAAAACAgAAAAAAAAAAAAAAAAAAAAU=")
            .unwrap(),
    );
    assert_eq!(
        rcs_link.wire_digest().unwrap(),
        [
            0x6f, 0x4e, 0x80, 0x9e, 0x11, 0x57, 0x08, 0xb5, 0x3e, 0x35, 0x12, 0x01, 0x2b, 0xf4,
            0x88, 0x0b, 0x60, 0x06, 0x4e, 0x29, 0x3d, 0x95, 0x38, 0xcd, 0xd0, 0x3b, 0x1d, 0x88,
            0xdb, 0x1b, 0x62, 0xfd,
        ]
    );
    assert_eq!(carrier.aad_bytes().unwrap()[168], 1);
    assert_eq!(rcs_link.aad_bytes().unwrap()[168], 2);
    assert_ne!(rcs_link.aad_bytes().unwrap(), carrier.aad_bytes().unwrap());
}

#[test]
fn generated_gateway_route_schema_matches_exactly_one_variant() {
    let contracts = peppy_protocol::generate_contracts();
    let openapi: serde_json::Value = serde_json::from_str(&contracts.openapi).unwrap();
    let branches = openapi
        .pointer("/components/schemas/GatewayRoute/oneOf")
        .and_then(serde_json::Value::as_array)
        .unwrap();

    for route in [
        GatewayRoute::Carrier {
            gateway_device_id: "00000000-0000-0000-0000-000000000005".parse().unwrap(),
            subscription_id: "sim:fixture".into(),
        },
        GatewayRoute::RcsLink {
            gateway_device_id: "00000000-0000-0000-0000-000000000005".parse().unwrap(),
        },
    ] {
        let value = serde_json::to_value(route).unwrap();
        let matching_branches = branches
            .iter()
            .filter(|branch| schema_branch_matches(branch, &value))
            .count();
        assert_eq!(matching_branches, 1, "{value}");
    }
}

fn schema_branch_matches(schema: &serde_json::Value, value: &serde_json::Value) -> bool {
    let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    let required = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str);
    if required.clone().any(|name| !object.contains_key(name)) {
        return false;
    }
    if schema.get("additionalProperties") == Some(&serde_json::Value::Bool(false))
        && object.keys().any(|name| !properties.contains_key(name))
    {
        return false;
    }
    properties.iter().all(|(name, property)| {
        property
            .get("const")
            .is_none_or(|expected| object.get(name) == Some(expected))
    })
}

#[test]
fn origin_tag_round_trips_is_omitted_and_does_not_change_digest() {
    let mut event = envelope();
    event.purpose = EnvelopePurpose::Event;
    event.command_id = None;
    event.route = None;
    let digest = event.wire_digest().unwrap();
    assert!(
        serde_json::to_value(&event)
            .unwrap()
            .get("origin_tag")
            .is_none()
    );
    event.origin_tag = Some([7; 32]);
    assert_eq!(event.wire_digest().unwrap(), digest);
    let encoded = serde_json::to_string(&event).unwrap();
    assert_eq!(
        serde_json::from_str::<Envelope>(&encoded)
            .unwrap()
            .origin_tag,
        Some([7; 32])
    );
}

#[test]
fn origin_tag_rejects_commands_and_wrong_lengths() {
    let mut command = envelope();
    command.origin_tag = Some([7; 32]);
    assert_eq!(command.validate(), Err(EnvelopeError::UnexpectedOriginTag));

    let mut value = serde_json::to_value(envelope()).unwrap();
    value["origin_tag"] =
        serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode([7; 31]));
    assert!(serde_json::from_value::<Envelope>(value).is_err());
}

#[test]
fn compaction_is_outside_legacy_aad_and_digest_but_validated() {
    let mut event = envelope();
    event.purpose = EnvelopePurpose::Event;
    event.command_id = None;
    event.route = None;
    let digest = event.wire_digest().unwrap();
    event.compaction = Some(CompactionMetadata {
        key: vec![7; 32],
        terminal: true,
        supersedes: vec![CompactionReference {
            producer_device_id: DeviceId::new(),
            producer_sequence: SourceSequence(1),
        }],
        checkpoint: false,
    });
    assert_eq!(event.wire_digest().unwrap(), digest);
    let legacy = serde_json::to_value(event.compaction.as_ref().unwrap()).unwrap();
    assert!(
        legacy.get("checkpoint").is_none(),
        "false marker is omitted"
    );
    event.compaction.as_mut().unwrap().key.clear();
    assert_eq!(event.validate(), Err(EnvelopeError::InvalidCompaction));
}

#[test]
fn compaction_rejects_own_current_or_future_references() {
    let mut event = envelope();
    event.purpose = EnvelopePurpose::Event;
    event.command_id = None;
    event.route = None;
    let own = event.producer_device_id;
    let at = |sequence: u64| CompactionReference {
        producer_device_id: own,
        producer_sequence: SourceSequence(sequence),
    };
    for (sequence, ok) in [
        (event.producer_sequence.0 - 1, true),
        (event.producer_sequence.0, false),
    ] {
        event.compaction = Some(CompactionMetadata {
            key: vec![7; 32],
            terminal: false,
            supersedes: vec![at(sequence)],
            checkpoint: true,
        });
        assert_eq!(event.validate().is_ok(), ok, "{sequence}");
    }
    // Another producer's sequence numbers are independent.
    event.compaction = Some(CompactionMetadata {
        key: vec![7; 32],
        terminal: false,
        checkpoint: false,
        supersedes: vec![CompactionReference {
            producer_device_id: DeviceId::new(),
            producer_sequence: SourceSequence(u64::MAX),
        }],
    });
    assert!(event.validate().is_ok());
}

#[test]
fn compaction_rejects_duplicate_references_but_accepts_distinct_references() {
    let mut event = envelope();
    event.purpose = EnvelopePurpose::Event;
    event.command_id = None;
    event.route = None;
    let reference = CompactionReference {
        producer_device_id: DeviceId::new(),
        producer_sequence: SourceSequence(1),
    };
    event.compaction = Some(CompactionMetadata {
        key: vec![7; 32],
        terminal: false,
        supersedes: vec![reference.clone(), reference.clone()],
        checkpoint: false,
    });
    assert_eq!(event.validate(), Err(EnvelopeError::InvalidCompaction));

    event.compaction.as_mut().unwrap().supersedes = vec![
        reference.clone(),
        CompactionReference {
            producer_device_id: reference.producer_device_id,
            producer_sequence: SourceSequence(2),
        },
    ];
    assert!(event.validate().is_ok());
}

#[test]
fn json_keeps_64_bit_sequence_as_a_string() {
    let encoded = serde_json::to_string(&envelope()).unwrap();
    assert!(encoded.contains("\"producer_sequence\":\"18446744073709551615\""));
    assert_eq!(
        serde_json::from_str::<Envelope>(&encoded)
            .unwrap()
            .producer_sequence,
        SourceSequence(u64::MAX)
    );
}

#[test]
fn malformed_or_oversize_envelopes_are_rejected() {
    let mut malformed = envelope();
    malformed.command_id = None;
    assert_eq!(malformed.validate(), Err(EnvelopeError::CommandIdRequired));
    let mut oversize = envelope();
    oversize.ciphertext = vec![0; peppy_protocol::MAX_CIPHERTEXT_BYTES + 1];
    assert_eq!(oversize.validate(), Err(EnvelopeError::CiphertextTooLarge));
}

#[test]
fn compaction_references_are_bounded_while_deserializing() {
    let mut event = envelope();
    event.purpose = EnvelopePurpose::Event;
    event.command_id = None;
    event.route = None;
    event.compaction = Some(CompactionMetadata {
        key: vec![0; peppy_protocol::COMPACTION_KEY_BYTES],
        terminal: false,
        supersedes: (1..=peppy_protocol::MAX_COMPACTION_SUPERSEDES)
            .map(|sequence| CompactionReference {
                producer_device_id: DeviceId::new(),
                producer_sequence: SourceSequence(sequence as u64),
            })
            .collect(),
        checkpoint: false,
    });
    let mut encoded = serde_json::to_value(&event).unwrap();
    assert_eq!(
        serde_json::from_value::<Envelope>(encoded.clone()).unwrap(),
        event
    );
    encoded["compaction"]["supersedes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "producer_device_id": DeviceId::new(),
            "producer_sequence": "129"
        }));
    assert!(
        serde_json::from_value::<Envelope>(encoded)
            .unwrap_err()
            .to_string()
            .contains("too many compaction references")
    );
}

#[test]
fn command_payload_conflicts_are_exact_wire_conflicts() {
    let first = envelope();
    let mut second = first.clone();
    second.ciphertext.push(4);
    assert_ne!(first.wire_digest().unwrap(), second.wire_digest().unwrap());
    assert!(first.conflicts_with(&second).unwrap());
}

#[test]
fn same_command_id_in_another_vault_is_not_a_conflict() {
    let first = envelope();
    let mut other_vault = first.clone();
    other_vault.vault_id = VaultId::new();
    assert!(!first.conflicts_with(&other_vault).unwrap());
}

#[test]
fn aad_binds_profile_and_is_available_before_ciphertext_exists() {
    let mut header = envelope();
    header.ciphertext.clear();
    let original = header.aad_bytes().unwrap();
    header.profile_fingerprint.replace_range(..1, "b");
    assert_ne!(original, header.aad_bytes().unwrap());
    assert_eq!(
        header.validate_ciphertext(),
        Err(EnvelopeError::CiphertextTooLarge)
    );
}

#[test]
fn invalid_header_never_produces_a_digest_or_conflict_result() {
    let mut invalid = envelope();
    invalid.key_epoch = 0;
    assert_eq!(invalid.wire_digest(), Err(EnvelopeError::InvalidKeyEpoch));
    assert_eq!(
        invalid.conflicts_with(&envelope()),
        Err(EnvelopeError::InvalidKeyEpoch)
    );
}

#[test]
fn malformed_base64_is_rejected_before_large_decode() {
    let mut value = serde_json::to_value(envelope()).unwrap();
    value["ciphertext"] =
        serde_json::Value::String("A".repeat(peppy_protocol::MAX_BASE64_CIPHERTEXT_CHARS + 1));
    assert!(serde_json::from_value::<Envelope>(value).is_err());
}

#[test]
fn pairing_qr_requires_valid_origin_expiry_and_device_binding() {
    let qr = PairingQrRecord {
        https_origin: "https://example.test:8443".into(),
        challenge_token: "A".repeat(43),
        expires_at_unix_seconds: 101,
        protocol_version: 1,
        vault_id: VaultId::new(),
        key_epoch: 1,
        profile_fingerprint: "a".repeat(64),
        device_binding: DeviceId::new(),
    };
    assert!(qr.validate_at(100).is_ok());
    assert_eq!(
        qr.validate_at(101),
        Err(EnvelopeError::ExpiredPairingChallenge)
    );
}

#[test]
fn generator_is_idempotent_and_check_detects_drift() {
    let root = std::env::temp_dir().join(format!(
        "peppy-contract-test-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    assert!(!peppy_protocol::write_contracts(&root, true).unwrap());
    assert!(!peppy_protocol::write_contracts(&root, false).unwrap());
    assert!(peppy_protocol::write_contracts(&root, true).unwrap());
    fs::write(root.join("packages/generated/src/protocol.ts"), "drift").unwrap();
    assert!(!peppy_protocol::write_contracts(&root, true).unwrap());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn generated_schemas_have_resolvable_references_and_derived_ts() {
    let contracts = peppy_protocol::generate_contracts();
    let openapi: serde_json::Value = serde_json::from_str(&contracts.openapi).unwrap();
    assert!(peppy_protocol::all_schema_references_resolve(&openapi));
    assert!(contracts.typescript.contains("crypto_suite"));
    assert!(contracts.typescript.contains("profile_fingerprint"));
    assert!(contracts.typescript.contains("command_id?: string | null;"));
    assert!(contracts.typescript.contains("kind: 'rcs_link';"));
}

fn assert_local_refs_resolve(root: &serde_json::Value, node: &serde_json::Value) {
    match node {
        serde_json::Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(serde_json::Value::as_str) {
                let pointer = reference.strip_prefix('#').expect("local schema reference");
                assert!(
                    root.pointer(pointer).is_some(),
                    "unresolved reference {reference}"
                );
            }
            for child in object.values() {
                assert_local_refs_resolve(root, child);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                assert_local_refs_resolve(root, child);
            }
        }
        _ => {}
    }
}

#[test]
fn generated_json_schema_references_resolve() {
    let contracts = peppy_protocol::generate_contracts();
    for text in [&contracts.openapi, &contracts.event_schema] {
        let root: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_local_refs_resolve(&root, &root);
    }
}

#[test]
fn durable_event_schema_remains_envelope_rooted() {
    let contracts = peppy_protocol::generate_contracts();
    let schema: serde_json::Value = serde_json::from_str(&contracts.event_schema).unwrap();
    assert_eq!(schema["title"], "Envelope");
    assert!(schema.get("oneOf").is_none());
    assert!(schema["properties"].get("envelope_id").is_some());
    assert!(schema["properties"].get("kind").is_none());
}

#[test]
fn event_envelope_conflict_is_scoped_to_vault_producer_and_envelope_id() {
    let mut first = envelope();
    first.purpose = EnvelopePurpose::Event;
    first.command_id = None;
    first.route = None;
    let mut changed = first.clone();
    changed.ciphertext.push(9);
    assert!(first.event_conflicts_with(&changed).unwrap());
    changed.envelope_id = EnvelopeId::new();
    assert!(!first.event_conflicts_with(&changed).unwrap());
}

#[test]
fn pairing_origin_and_token_reject_lookalikes() {
    let mut qr = PairingQrRecord {
        https_origin: "https://[::1]:443".into(),
        challenge_token: "A".repeat(43),
        expires_at_unix_seconds: 101,
        protocol_version: 1,
        vault_id: VaultId::new(),
        key_epoch: 1,
        profile_fingerprint: "a".repeat(64),
        device_binding: DeviceId::new(),
    };
    assert!(qr.validate_at(100).is_ok());
    for origin in [
        "https://[::1]evil",
        "https://[abc]",
        "https://user@example.test",
        "https://example.test/path",
        "https://example.test?x=1",
    ] {
        qr.https_origin = origin.into();
        assert_eq!(
            qr.validate_at(100),
            Err(EnvelopeError::InvalidPairingOrigin)
        );
    }
    qr.https_origin = "https://example.test".into();
    qr.challenge_token = "A".repeat(42);
    assert_eq!(qr.validate_at(100), Err(EnvelopeError::InvalidPairingToken));
}
