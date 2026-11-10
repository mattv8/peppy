use super::group_fixture::{GROUP_MEMBERS, group_fixture, group_observation, group_snapshot};
use super::*;

/// The conversation id the Go group export uses (`exportGroupReceiveRecords`).
const GO_GROUP_GOOGLE_ID: &str = "synthetic-group";
const NOW: i64 = 1_000;

/// Consumes the Go-produced group receive records (`PEPPY_RCS_GO_GROUP_RECEIVE_RECORDS_PATH`):
/// a visible sender is attributed, an unknown sender is stored unattributed and pauses the group,
/// and a self-sent record is stored once.
#[test]
fn go_exported_group_receive_records_ingest_into_a_live_group() {
    let Ok(path) = std::env::var("PEPPY_RCS_GO_GROUP_RECEIVE_RECORDS_PATH") else {
        return;
    };
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read Go group receive records {path:?}: {error}"));
    let projections: Vec<Value> = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("decode Go group receive records: {error}"));
    // One record per sender shape: visible sender, unknown sender, and self-sent.
    assert_eq!(projections.len(), 3);
    for projection in &projections {
        assert_eq!(projection["code"], "ok");
    }

    let fixture = group_fixture(&GROUP_MEMBERS);
    assert!(fixture.advance_route(&group_observation(&GROUP_MEMBERS, NOW), NOW));
    let report = fixture.ingest(
        &group_snapshot(GO_GROUP_GOOGLE_ID, &GROUP_MEMBERS, NOW),
        NOW,
    );
    assert_eq!(report.linked, 1);

    let gateway = &fixture.gateway;
    let context = gateway
        .gateway_rcs_receive_context(GO_GROUP_GOOGLE_ID.into())
        .unwrap()
        .expect("the live group has a receive lane");
    assert_eq!(context.conversation_id, fixture.conversation_id.to_string());
    let ingest = |projection: &Value| {
        gateway
            .gateway_ingest_go_rcs_record_json(
                context.conversation_id.clone(),
                context.ownership_generation,
                projection["record"].to_string(),
            )
            .unwrap()
    };

    // A visible member is attributed to their canonical address.
    assert_eq!(ingest(&projections[0]).record_outcome, "inserted");
    // An unknown sender is stored unattributed; the group pauses for re-proof, and receive goes on.
    assert_eq!(ingest(&projections[1]).record_outcome, "inserted");
    assert_eq!(
        fixture.proved_revision(),
        None,
        "an unknown sender pauses the group"
    );
    // A self-sent record is stored once: a second delivery of the same message is a duplicate.
    assert_eq!(ingest(&projections[2]).record_outcome, "unmatched_outgoing");
    assert_eq!(ingest(&projections[2]).record_outcome, "duplicate");

    let messages = gateway.messages(context.conversation_id.clone()).unwrap();
    let incoming: Vec<_> = messages.iter().filter(|m| m.incoming).collect();
    assert_eq!(incoming.len(), 2);
    assert!(
        incoming
            .iter()
            .any(|m| m.sender_address.as_deref() == Some("+12025550102"))
    );
    assert!(incoming.iter().any(|m| m.sender_address.is_none()));
    // The fixture's own draft is also outgoing; only the Go self-sent record counts here.
    let self_sent = messages
        .iter()
        .filter(|m| !m.incoming && m.body == "synthetic group")
        .count();
    assert_eq!(self_sent, 1, "the self-sent record is stored once");
}
