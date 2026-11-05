use base64::Engine as _;
use peppy_domain::{DeviceId, VaultId};
use peppy_protocol::{
    Envelope, EphemeralSignalError, EphemeralSignalV1, MAX_EPHEMERAL_CIPHERTEXT_BYTES,
    POSTGRES_NOTIFY_PAYLOAD_BYTES,
};

fn signal() -> EphemeralSignalV1 {
    EphemeralSignalV1 {
        protocol_version: 1,
        kind: "typing".into(),
        sender_device_id: "00000000-0000-0000-0000-000000000001"
            .parse::<DeviceId>()
            .unwrap(),
        key_epoch: 7,
        sent_at_ms: "1000".into(),
        expires_at_ms: "10000".into(),
        sequence: "9".into(),
        ciphertext_b64: base64::engine::general_purpose::STANDARD.encode([1, 2, 3]),
    }
}

#[test]
fn signal_round_trips_with_nonzero_epoch_and_canonical_aad() {
    let signal = signal();
    let json = serde_json::to_string(&signal).unwrap();
    assert_eq!(
        serde_json::from_str::<EphemeralSignalV1>(&json).unwrap(),
        signal
    );
    assert_eq!(
        signal
            .aad_bytes("00000000-0000-0000-0000-000000000002".parse::<VaultId>().unwrap())
            .unwrap(),
        base64::engine::general_purpose::STANDARD
            .decode("cGVwcHktZXBoZW1lcmFsLXR5cGluZy12MQAAAAAAAAAAAAAAAAAAAAACAAEGdHlwaW5nAAAAAAAAAAAAAAAAAAAAAQAAAAcAAAAAAAAD6AAAAAAAACcQAAAAAAAAAAk=")
            .unwrap(),
    );
}

#[test]
fn signal_rejects_invalid_headers_and_ciphertext() {
    let signal = signal();
    for (field, raw_value) in [
        ("protocol_version", "2"),
        ("kind", "\"unknown\""),
        ("key_epoch", "0"),
        ("expires_at_ms", "16001"),
        ("expires_at_ms", "1000"),
        ("ciphertext_b64", "\"%%%\""),
    ] {
        let mut value = serde_json::to_value(&signal).unwrap();
        value[field] = serde_json::from_str(raw_value).unwrap();
        assert!(
            serde_json::from_value::<EphemeralSignalV1>(value).is_err(),
            "{field}"
        );
    }
}

#[test]
fn signal_rejects_unknown_fields_and_is_not_an_envelope() {
    let mut value = serde_json::to_value(signal()).unwrap();
    value["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<EphemeralSignalV1>(value.clone()).is_err());
    assert!(serde_json::from_value::<Envelope>(value).is_err());
}

#[test]
fn signal_rejects_ciphertext_larger_than_notification_payload_limit() {
    let mut signal = signal();
    signal.ciphertext_b64 =
        base64::engine::general_purpose::STANDARD
            .encode(vec![0_u8; MAX_EPHEMERAL_CIPHERTEXT_BYTES + 1]);
    assert_eq!(
        signal.validate(),
        Err(EphemeralSignalError::CiphertextTooLarge)
    );
}

#[test]
fn largest_signal_fits_the_postgres_notification_payload_limit() {
    let mut signal = signal();
    signal.key_epoch = u32::MAX;
    signal.sent_at_ms = "18446744073709536615".into();
    signal.expires_at_ms = "18446744073709551615".into();
    signal.sequence = u64::MAX.to_string();
    signal.ciphertext_b64 = base64::engine::general_purpose::STANDARD
        .encode(vec![0_u8; MAX_EPHEMERAL_CIPHERTEXT_BYTES]);
    assert!(signal.validate().is_ok());
    let relay_payload = serde_json::json!({
        "vault_id": "00000000-0000-0000-0000-000000000002",
        "frame": signal,
    });
    let payload_len = serde_json::to_vec(&relay_payload).unwrap().len();
    assert_eq!(payload_len, 7908);
    assert!(payload_len < POSTGRES_NOTIFY_PAYLOAD_BYTES);
}

#[test]
fn signal_rejects_noncanonical_and_overflowing_decimals() {
    for field in ["sent_at_ms", "expires_at_ms", "sequence"] {
        for invalid in [
            "".into(),
            "+9".into(),
            "-9".into(),
            "09".into(),
            " 9".into(),
            "9 ".into(),
            "９".into(),
            "٩".into(),
            "18446744073709551616".into(),
            "9".repeat(9000),
            format!("+{}9", "0".repeat(9000)),
        ] {
            let mut value = serde_json::to_value(signal()).unwrap();
            value[field] = serde_json::Value::String(invalid);
            assert!(
                serde_json::from_value::<EphemeralSignalV1>(value).is_err(),
                "{field}"
            );
        }
    }
}

#[test]
fn accepted_decimals_round_trip_canonically() {
    for sequence in [0, 1, 9, u64::MAX] {
        let mut frame = signal();
        frame.sequence = sequence.to_string();
        frame.sent_at_ms = "0".into();
        frame.expires_at_ms = "1".into();
        let parsed: EphemeralSignalV1 =
            serde_json::from_value(serde_json::to_value(&frame).unwrap()).unwrap();
        for value in [&parsed.sequence, &parsed.sent_at_ms, &parsed.expires_at_ms] {
            assert_eq!(*value, value.parse::<u64>().unwrap().to_string());
        }
    }
}
