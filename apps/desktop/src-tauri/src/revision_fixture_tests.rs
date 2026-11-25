use crate::{gate_draft_send, tests};

#[test]
fn send_gate_revision_strings_match_fixture() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../packages/contracts/fixtures/draft-cas.json"
    ))
    .unwrap();
    let session_fixture = tests::fixture_at("http://127.0.0.1:9");
    let session = tests::open(&session_fixture, &session_fixture.binding, &[]);
    tests::unlock(&session, &session_fixture);
    let id = peppy_client_core::DraftId::new().to_string();
    for case in fixture["revisionStrings"]["cases"].as_array().unwrap() {
        let revision = case["input"].as_str().unwrap();
        let input = tests::input(&id, "", "", &[], revision);
        let error = gate_draft_send(&session, &input).err();
        let parse_error = error
            .as_ref()
            .filter(|error| error.message == "The draft revision is invalid.");
        if let Some(expected) = case["error"].as_str() {
            assert_eq!(
                parse_error.map(|error| error.code),
                Some(expected),
                "{revision:?}"
            );
        } else {
            assert!(parse_error.is_none(), "{revision:?}: {error:?}");
            assert_ne!(
                error.as_ref().map(|error| error.code),
                Some("invalid-draft"),
                "{revision:?}: {error:?}"
            );
        }
    }
}
