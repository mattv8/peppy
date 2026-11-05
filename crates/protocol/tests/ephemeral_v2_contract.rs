use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use peppy_domain::{DeviceId, VaultId};
use peppy_protocol::{
    ChallengePlaintextV2, Envelope, EphemeralSignalV1, EphemeralSignalV2, EphemeralSignalV2Kind,
    LiveHelloV2, MAX_EPHEMERAL_V2_CONTROL_CIPHERTEXT_BYTES,
    MAX_EPHEMERAL_V2_TYPING_CIPHERTEXT_BYTES, SessionAckPlaintextV2, TypingPlaintextV2,
};

fn signal(kind: EphemeralSignalV2Kind) -> EphemeralSignalV2 {
    EphemeralSignalV2 {
        protocol_version: 2,
        kind,
        sender_device_id: "00000000-0000-0000-0000-000000000001"
            .parse::<DeviceId>()
            .unwrap(),
        key_epoch: 7,
        session_id: URL_SAFE_NO_PAD.encode([3_u8; 16]),
        sent_at_ms: "1000".into(),
        expires_at_ms: "10001".into(),
        sequence: "9".into(),
        ciphertext_b64: STANDARD.encode([1, 2, 3]),
    }
}

#[test]
fn v2_signal_round_trips_and_uses_the_specified_aad_vector() {
    let signal = signal(EphemeralSignalV2Kind::Typing);
    let encoded = serde_json::to_value(&signal).unwrap();
    assert_eq!(
        serde_json::from_value::<EphemeralSignalV2>(encoded).unwrap(),
        signal
    );

    let vault = "00000000-0000-0000-0000-000000000002"
        .parse::<VaultId>()
        .unwrap();
    let mut expected = b"peppy-ephemeral-typing-v2\0".to_vec();
    expected.extend_from_slice(vault.0.as_bytes());
    expected.extend_from_slice(&2_u16.to_be_bytes());
    expected.extend_from_slice(&[6]);
    expected.extend_from_slice(b"typing");
    expected.extend_from_slice(signal.sender_device_id.0.as_bytes());
    expected.extend_from_slice(&7_u32.to_be_bytes());
    expected.extend_from_slice(&[3_u8; 16]);
    expected.extend_from_slice(&1000_u64.to_be_bytes());
    expected.extend_from_slice(&10001_u64.to_be_bytes());
    expected.extend_from_slice(&9_u64.to_be_bytes());
    assert_eq!(signal.aad_bytes(vault).unwrap(), expected);
}

#[test]
fn v2_rejects_invalid_headers_unknown_fields_and_v1_shapes() {
    let valid = signal(EphemeralSignalV2Kind::Typing);
    for (field, invalid) in [
        ("protocol_version", serde_json::json!(1)),
        ("key_epoch", serde_json::json!(0)),
        ("session_id", serde_json::json!("AAAAAAAAAAAAAAAAAAAAAA==")),
        ("sequence", serde_json::json!("0")),
        ("sent_at_ms", serde_json::json!("01")),
        ("expires_at_ms", serde_json::json!("1000")),
        ("expires_at_ms", serde_json::json!("16001")),
        ("ciphertext_b64", serde_json::json!("%%%")),
    ] {
        let mut value = serde_json::to_value(&valid).unwrap();
        value[field] = invalid;
        assert!(
            serde_json::from_value::<EphemeralSignalV2>(value).is_err(),
            "{field}"
        );
    }
    let mut extra = serde_json::to_value(&valid).unwrap();
    extra["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<EphemeralSignalV2>(extra.clone()).is_err());
    assert!(serde_json::from_value::<EphemeralSignalV1>(extra.clone()).is_err());
    assert!(serde_json::from_value::<Envelope>(extra).is_err());
}

#[test]
fn v2_per_kind_ciphertext_caps_and_aad_bind_every_header_field() {
    let typing = signal(EphemeralSignalV2Kind::Typing);
    let control = signal(EphemeralSignalV2Kind::Challenge);
    for (frame, cap) in [
        (typing.clone(), MAX_EPHEMERAL_V2_TYPING_CIPHERTEXT_BYTES),
        (control, MAX_EPHEMERAL_V2_CONTROL_CIPHERTEXT_BYTES),
    ] {
        let mut oversized = frame;
        oversized.ciphertext_b64 = STANDARD.encode(vec![0_u8; cap + 1]);
        assert!(oversized.validate().is_err());
    }
    let vault = "00000000-0000-0000-0000-000000000002".parse().unwrap();
    let original = typing.aad_bytes(vault).unwrap();
    for field in [
        "kind",
        "sender_device_id",
        "key_epoch",
        "session_id",
        "sent_at_ms",
        "expires_at_ms",
        "sequence",
    ] {
        let mut changed = serde_json::to_value(&typing).unwrap();
        changed[field] = match field {
            "kind" => serde_json::json!("challenge"),
            "sender_device_id" => serde_json::json!("00000000-0000-0000-0000-000000000003"),
            "key_epoch" => serde_json::json!(8),
            "session_id" => serde_json::json!(URL_SAFE_NO_PAD.encode([4_u8; 16])),
            "sent_at_ms" => serde_json::json!("1001"),
            "expires_at_ms" => serde_json::json!("10002"),
            "sequence" => serde_json::json!("10"),
            _ => unreachable!(),
        };
        let changed = serde_json::from_value::<EphemeralSignalV2>(changed).unwrap();
        assert_ne!(original, changed.aad_bytes(vault).unwrap(), "{field}");
    }
    assert_ne!(
        original,
        typing
            .aad_bytes("00000000-0000-0000-0000-000000000004".parse().unwrap())
            .unwrap()
    );
}

#[test]
fn v2_hello_has_fixed_literals_and_strict_limits() {
    assert_eq!(
        serde_json::to_string(&LiveHelloV2::new()).unwrap(),
        r#"{"type":"hello","protocol_version":2,"max_ttl_ms":15000,"max_ciphertext_bytes":5400}"#
    );
    let mut invalid = serde_json::to_value(LiveHelloV2::new()).unwrap();
    invalid["max_ciphertext_bytes"] = serde_json::json!(5399);
    assert!(serde_json::from_value::<LiveHelloV2>(invalid).is_err());
}

#[test]
fn v2_plaintexts_reject_wrong_gateway_nonce_and_unknown_fields() {
    let session = URL_SAFE_NO_PAD.encode([7_u8; 16]);
    let nonce = STANDARD.encode([8_u8; 16]);
    let challenge = serde_json::json!({
        "version": 2, "challenger_device_id":"00000000-0000-0000-0000-000000000001",
        "challenger_session_id":session, "responder_device_id":"00000000-0000-0000-0000-000000000002",
        "nonce_b64":nonce,
    });
    assert!(serde_json::from_value::<ChallengePlaintextV2>(challenge.clone()).is_ok());
    for (field, value) in [
        ("nonce_b64", serde_json::json!("AAAAAAAAAAAAAAAAAAAAAA")),
        ("extra", serde_json::json!(true)),
    ] {
        let mut invalid = challenge.clone();
        invalid[field] = value;
        assert!(serde_json::from_value::<ChallengePlaintextV2>(invalid).is_err());
    }
}

#[test]
fn v2_plaintext_gateway_condition_and_control_lengths_are_exact() {
    let session = URL_SAFE_NO_PAD.encode([7_u8; 16]);
    let nonce = STANDARD.encode([8_u8; 16]);
    let typing = serde_json::json!({
        "version":2,"origin":"vault_device","conversation_id":"00000000-0000-0000-0000-000000000003",
        "state":"started","gateway_device_id":"00000000-0000-0000-0000-000000000004"
    });
    assert!(serde_json::from_value::<TypingPlaintextV2>(typing.clone()).is_ok());
    let mut missing_gateway = typing;
    missing_gateway
        .as_object_mut()
        .unwrap()
        .remove("gateway_device_id");
    assert!(serde_json::from_value::<TypingPlaintextV2>(missing_gateway).is_err());

    let challenge = serde_json::json!({
        "version":2,"challenger_device_id":"00000000-0000-0000-0000-000000000001",
        "challenger_session_id":session,"responder_device_id":"00000000-0000-0000-0000-000000000002","nonce_b64":nonce,
    });
    let ack = serde_json::json!({
        "version":2,"responder_device_id":"00000000-0000-0000-0000-000000000001","responder_session_id":session,
        "challenger_device_id":"00000000-0000-0000-0000-000000000002","challenger_session_id":session,
        "echoed_nonce_b64":nonce,"sequence_floor":"18446744073709551615",
    });
    assert_eq!(
        serde_json::to_vec(&serde_json::from_value::<ChallengePlaintextV2>(challenge).unwrap())
            .unwrap()
            .len(),
        224
    );
    assert_eq!(
        serde_json::to_vec(&serde_json::from_value::<SessionAckPlaintextV2>(ack).unwrap())
            .unwrap()
            .len(),
        319
    );
    assert_eq!(224 + 24 + 16, 264);
    assert_eq!(319 + 24 + 16, 359);
}

#[test]
fn v2_maximum_notify_payloads_use_the_real_relay_wrapper() {
    for (kind, expected) in [
        (EphemeralSignalV2Kind::Typing, 7_599),
        (EphemeralSignalV2Kind::Challenge, 1_086),
        (EphemeralSignalV2Kind::SessionAck, 1_088),
    ] {
        let mut frame = signal(kind);
        frame.key_epoch = u32::MAX;
        frame.sent_at_ms = "18446744073709536615".into();
        frame.expires_at_ms = "18446744073709551615".into();
        frame.sequence = u64::MAX.to_string();
        frame.ciphertext_b64 = STANDARD.encode(vec![
            0_u8;
            match kind {
                EphemeralSignalV2Kind::Typing => MAX_EPHEMERAL_V2_TYPING_CIPHERTEXT_BYTES,
                EphemeralSignalV2Kind::Challenge | EphemeralSignalV2Kind::SessionAck =>
                    MAX_EPHEMERAL_V2_CONTROL_CIPHERTEXT_BYTES,
            }
        ]);
        let bytes = serde_json::to_vec(&serde_json::json!({
            "instance_id":"00000000-0000-0000-0000-000000000001",
            "vault_id":"00000000-0000-0000-0000-000000000002", "frame": frame,
        }))
        .unwrap()
        .len();
        assert_eq!(bytes, expected, "{kind:?}");
    }
}
