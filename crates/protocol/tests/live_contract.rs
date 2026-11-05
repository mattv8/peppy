use peppy_protocol::{EphemeralSignalV1, LiveFrameV1, LiveHelloV1};
use serde_json::{Value, json};

fn hello() -> Value {
    json!({"type":"hello","protocol_version":1,"max_ttl_ms":15000,"max_ciphertext_bytes":5700})
}

#[test]
fn current_hello_serializes_to_the_exact_contract() {
    let expected =
        r#"{"type":"hello","protocol_version":1,"max_ttl_ms":15000,"max_ciphertext_bytes":5700}"#;
    assert_eq!(
        serde_json::to_string(&LiveHelloV1::new()).unwrap(),
        expected
    );
    assert_eq!(
        serde_json::from_str::<LiveHelloV1>(expected).unwrap(),
        LiveHelloV1::new()
    );
}

#[test]
fn hello_requires_all_fields_and_rejects_extras() {
    for field in [
        "type",
        "protocol_version",
        "max_ttl_ms",
        "max_ciphertext_bytes",
    ] {
        let mut value = hello();
        value.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<LiveHelloV1>(value).is_err(),
            "{field}"
        );
    }
    for field in ["unknown", "vault_id", "sender_device_id", "token"] {
        let mut value = hello();
        value[field] = json!("synthetic");
        assert!(serde_json::from_value::<LiveHelloV1>(value).is_err());
    }
}

#[test]
fn hello_rejects_unknown_literals_and_invalid_limits() {
    for (field, invalid) in [
        ("type", json!("unknown")),
        ("protocol_version", json!(0)),
        ("protocol_version", json!(2)),
        ("protocol_version", json!("1")),
        ("max_ttl_ms", json!(15001)),
        ("max_ciphertext_bytes", json!(5701)),
    ] {
        let mut value = hello();
        value[field] = invalid;
        assert!(
            serde_json::from_value::<LiveHelloV1>(value).is_err(),
            "{field}"
        );
    }
    for field in ["max_ttl_ms", "max_ciphertext_bytes"] {
        for invalid in [
            json!(0),
            json!(-1),
            json!(1.5),
            json!("1"),
            Value::Null,
            json!(u64::MAX),
        ] {
            let mut value = hello();
            value[field] = invalid;
            assert!(
                serde_json::from_value::<LiveHelloV1>(value).is_err(),
                "{field}"
            );
        }
        for valid in [1, 2] {
            let mut value = hello();
            value[field] = json!(valid);
            assert!(serde_json::from_value::<LiveHelloV1>(value).is_ok());
        }
    }
}

#[test]
fn live_frame_is_the_existing_signal_without_a_wrapper_or_new_aad() {
    let value = json!({"protocol_version":1,"kind":"typing",
        "sender_device_id":"00000000-0000-0000-0000-000000000001",
        "key_epoch":1,"sent_at_ms":"0","expires_at_ms":"1","sequence":"9","ciphertext_b64":"AQID"});
    let signal: EphemeralSignalV1 = serde_json::from_value(value.clone()).unwrap();
    let frame: LiveFrameV1 = signal.clone();
    assert_eq!(serde_json::to_value(&frame).unwrap(), value);
    let vault = "00000000-0000-0000-0000-000000000002".parse().unwrap();
    assert_eq!(
        frame.aad_bytes(vault).unwrap(),
        signal.aad_bytes(vault).unwrap()
    );
    assert!(serde_json::from_value::<LiveFrameV1>(hello()).is_err());
    assert!(serde_json::from_value::<LiveHelloV1>(value).is_err());
}

#[test]
fn generated_live_contracts_use_literals_and_a_signal_reference() {
    let contracts = peppy_protocol::generate_contracts();
    let schema: Value = serde_json::from_str(&contracts.openapi).unwrap();
    let hello = &schema["components"]["schemas"]["LiveHelloV1"];
    assert_eq!(hello["additionalProperties"], false);
    assert_eq!(hello["properties"]["type"]["const"], "hello");
    assert_eq!(hello["properties"]["protocol_version"]["const"], 1);
    assert_eq!(hello["properties"]["max_ttl_ms"]["minimum"], 1);
    assert_eq!(hello["properties"]["max_ttl_ms"]["maximum"], 15000);
    assert_eq!(hello["properties"]["max_ciphertext_bytes"]["maximum"], 5700);
    assert_eq!(hello["required"].as_array().unwrap().len(), 4);
    assert_eq!(
        schema["components"]["schemas"]["LiveFrameV1"]["$ref"],
        "#/components/schemas/EphemeralSignalV1"
    );
    assert!(
        contracts
            .typescript
            .contains("export type LiveFrameV1 = EphemeralSignalV1;")
    );
    assert!(contracts.typescript.contains("type: 'hello';"));
}
