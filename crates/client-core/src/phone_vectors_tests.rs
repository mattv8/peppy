use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PhoneVector {
    name: String,
    input: String,
    region: Option<String>,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    e164: Option<String>,
    #[serde(deserialize_with = "Deserialize::deserialize")]
    canonical: Option<String>,
    note: Option<String>,
}

#[test]
fn phone_vectors_reject_missing_results_and_unknown_fields() {
    let valid = r#"{"name":"x","input":"x","region":null,"e164":null,"canonical":null}"#;
    assert!(serde_json::from_str::<PhoneVector>(valid).is_ok());
    for json in [
        r#"{"name":"x","input":"x","region":null,"canonical":null}"#,
        r#"{"name":"x","input":"x","region":null,"e164":null}"#,
        r#"{"name":"x","input":"x","region":null,"e164":null,"canonical":null,"typo":null}"#,
    ] {
        assert!(serde_json::from_str::<PhoneVector>(json).is_err(), "{json}");
    }
}

#[test]
fn phone_vectors_match_core() {
    let fixture = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/contracts/fixtures/phone-canonical.json"
    ));
    let vectors: Vec<PhoneVector> = serde_json::from_str(fixture).expect("valid phone fixture");
    assert!(!vectors.is_empty());
    for vector in vectors {
        let region = vector.region.as_deref();
        let normalized = crate::contact_resolution::normalized(&vector.input, region);
        let canonical = crate::conversation_match::canonical_address(&vector.input, region);
        assert_eq!(normalized, vector.e164, "normalized: {}", vector.name);
        assert_eq!(
            Some(canonical),
            vector.canonical,
            "canonical: {}",
            vector.name
        );
        if vector.e164 != vector.canonical {
            assert!(
                vector
                    .note
                    .as_deref()
                    .is_some_and(|note| !note.trim().is_empty()),
                "missing disagreement note: {}",
                vector.name
            );
        }
    }
}
