use peppy_browser_bindings::BrowserCore;
use serde_json::{Value, json};

fn response(core: &mut BrowserCore, command: &str, args: Value) -> Value {
    serde_json::from_str(&core.dispatch(&json!({"command": command, "args": args}).to_string()))
        .unwrap()
}

#[test]
fn production_library_refuses_legacy_raw_key_commands() {
    let root = tempfile::tempdir().unwrap();
    let mut core = BrowserCore::new(root.path().to_owned());
    let raw_key = vec![7_u8; 32];

    for (command, args) in [
        (
            "open",
            json!({
                "vaultId": "11111111-1111-1111-1111-111111111111",
                "deviceId": "22222222-2222-2222-2222-222222222222",
                "databaseKey": raw_key,
                "origin": "https://peppy.test",
                "deviceRole": "device"
            }),
        ),
        (
            "unlock",
            json!({"databaseKey": raw_key, "passphrase": "not accepted"}),
        ),
    ] {
        let result = response(&mut core, command, args);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["error"]["code"], "unknown-command", "{result}");
    }
    assert!(!root.path().join("client.db").exists());
}
