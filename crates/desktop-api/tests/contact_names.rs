use peppy_desktop_api::contacts::{
    MAX_TEXT, book_view, name_candidates_view, name_conflicts_view, name_lookup_input,
    resolved_addresses_view,
};
use serde_json::json;
use std::collections::HashMap;

#[test]
fn name_projections_are_sanitized_and_identity_wins() {
    let raw = json!({
        "matches": [{"address":"1","book_id":"b","contact_id":"c","display_name":"Identity"}],
        "name_matches": [
            {"address":"1","display_name":"Other","basis":"consensus"},
            {"address":"2","display_name":"Name","basis":"pinned","photo_attachment_id":"invalid"},
            {"address":"3","display_name":" ","basis":"consensus"},
            {"address":"4","display_name":"Name","basis":"unknown"}
        ],
        "name_conflicts":["2", "x".repeat(257), 5]
    });
    let resolved = resolved_addresses_view(&raw);
    assert_eq!(resolved["1"].display_name, "Identity");
    assert_eq!(resolved["1"].basis, None);
    assert_eq!(resolved["2"].basis, Some("pinned"));
    assert_eq!(resolved["2"].contact_id, None);
    assert_eq!(resolved["2"].photo, None);
    assert_eq!(resolved.len(), 2);
    assert_eq!(name_conflicts_view(&raw), vec!["2"]);
}

#[test]
fn book_and_candidates_expose_only_bounded_public_fields() {
    let book = book_view(
        &json!({"id":"b","lookup_excluded":true}),
        "",
        &HashMap::new(),
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(book["lookupExcluded"], true);
    let result = name_candidates_view(&json!({
        "candidates":[{"book_id":"b","contact_id":"c","display_name":"Ada","secret":"x"},{"book_id":"","contact_id":"c","display_name":"bad"}],
        "pinned":{"book_id":"b","contact_id":"c","secret":"x"},
        "more_indexing":true
    }));
    assert_eq!(
        result,
        json!({"candidates":[{"bookId":"b","contactId":"c","displayName":"Ada"}],"pinned":{"bookId":"b","contactId":"c"},"moreIndexing":true})
    );
}

#[test]
fn name_inputs_and_projection_limits_are_enforced() {
    assert!(name_lookup_input("", None).is_err());
    assert!(name_lookup_input(&"x".repeat(257), None).is_err());
    assert!(name_lookup_input("1", Some(&"x".repeat(257))).is_err());
    assert_eq!(
        name_lookup_input("1", Some("phone")).unwrap(),
        json!({"address":"1","source_device_id":"phone"})
    );
    let rows = (0..40)
        .map(|n| json!({"book_id":"b","contact_id":n.to_string(),"display_name":"x".repeat(MAX_TEXT + 1)}))
        .collect::<Vec<_>>();
    let result = name_candidates_view(
        &json!({"candidates": rows, "pinned":{"book_id":"","contact_id":"c"}}),
    );
    assert_eq!(result["candidates"].as_array().unwrap().len(), 32);
    assert_eq!(
        result["candidates"][0]["displayName"]
            .as_str()
            .unwrap()
            .len(),
        MAX_TEXT
    );
    assert!(result["pinned"].is_null());
    assert_eq!(result["moreIndexing"], false);
}
