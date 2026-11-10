use super::group_fixture::{
    GROUP_GOOGLE_ID, GROUP_MEMBERS, GROUP_SELF, group_fixture, group_member_digest,
    group_observation, group_snapshot,
};
use super::*;

const NOW: i64 = 1_000;

#[test]
fn group_preparation_carries_the_stored_member_digest_and_the_group_facts_shape() {
    let fixture = group_fixture(&GROUP_MEMBERS);
    let digest = group_member_digest(&GROUP_MEMBERS);
    assert!(fixture.advance_route(&group_observation(&GROUP_MEMBERS, NOW), NOW));
    assert_eq!(
        fixture
            .ingest(&group_snapshot(GROUP_GOOGLE_ID, &GROUP_MEMBERS, NOW), NOW)
            .linked,
        1
    );
    assert_eq!(
        fixture.verify(&group_observation(&GROUP_MEMBERS, NOW), NOW),
        NativeRcsGroupVerification::Proven
    );
    assert_eq!(fixture.proved_revision(), Some(1));

    let preparation = fixture.prepare(NOW);
    assert_eq!(
        preparation.expected_member_digest().as_deref(),
        Some(digest.as_str())
    );
    let facts: Value = serde_json::from_str(&preparation.facts_json().unwrap()).unwrap();
    // The facts request keeps the three 1:1 keys; for a group its recipient is the member digest.
    assert_eq!(facts["request"].as_object().unwrap().len(), 3);
    assert_eq!(facts["request"]["canonical_recipient"], json!(digest));
    let stored = &facts["stored_mapping"];
    assert_eq!(stored["class"], "Group");
    assert_eq!(stored["mapping_revision"], 1);
    assert_eq!(stored["verified_at_ms"], NOW);
    assert_eq!(stored["proof"]["canonical_participant"], json!(digest));
    assert!(!stored["proof"]["digest"].is_null());
    assert_eq!(stored["proof"]["own_identity"], json!(GROUP_SELF));
}

/// The production order for a group-only link through the exported functions: the route step
/// writes the context, the proof follows, and the advisory digest is not a freshness gate. A
/// healthy idle group still sends after its proof is older than the freshness window.
#[test]
fn group_only_link_sends_and_an_idle_group_re_proves_through_the_bindings() {
    let fixture = group_fixture(&GROUP_MEMBERS);
    let digest = group_member_digest(&GROUP_MEMBERS);
    let conversation = fixture.conversation_id.to_string();
    assert_eq!(
        gateway_proved_group_member_digest(fixture.gateway.clone(), conversation.clone()).unwrap(),
        None,
        "no group is Live before its snapshot proves it"
    );
    assert!(fixture.advance_route(&group_observation(&GROUP_MEMBERS, NOW), NOW));
    assert_eq!(
        fixture
            .ingest(&group_snapshot(GROUP_GOOGLE_ID, &GROUP_MEMBERS, NOW), NOW)
            .linked,
        1
    );
    assert_eq!(
        gateway_proved_group_member_digest(fixture.gateway.clone(), conversation.clone()).unwrap(),
        Some(digest.clone())
    );
    assert_eq!(
        fixture.verify(&group_observation(&GROUP_MEMBERS, NOW), NOW),
        NativeRcsGroupVerification::Proven
    );
    // Prepare reads the command without consuming it; only begin spends the one-use command.
    assert_eq!(
        fixture.prepare(NOW).expected_member_digest().as_deref(),
        Some(digest.as_str())
    );

    // Idle for 30,001 ms with no index event: the stored proof is stale, the digest still reads.
    let later = NOW + 30_001;
    assert_eq!(
        gateway_proved_group_member_digest(fixture.gateway.clone(), conversation).unwrap(),
        Some(digest.clone())
    );
    assert!(
        gateway_prepare_native_rcs_send(fixture.gateway.clone(), fixture.command_id.clone(), later)
            .is_err(),
        "a stale proof cannot prepare on its own"
    );
    assert!(fixture.advance_route(&group_observation(&GROUP_MEMBERS, later), later));
    assert_eq!(
        fixture.verify(&group_observation(&GROUP_MEMBERS, later), later),
        NativeRcsGroupVerification::Proven
    );
    let idle = fixture.prepare(later);
    assert_eq!(
        idle.expected_member_digest().as_deref(),
        Some(digest.as_str())
    );
    assert!(
        idle.begin(
            serde_json::to_string(&group_observation(&GROUP_MEMBERS, later)).unwrap(),
            later
        )
        .is_ok()
    );
}

#[test]
fn group_begin_on_a_changed_roster_is_group_members_changed_and_pauses_the_group() {
    let fixture = group_fixture(&GROUP_MEMBERS);
    assert!(fixture.advance_route(&group_observation(&GROUP_MEMBERS, NOW), NOW));
    fixture.ingest(&group_snapshot(GROUP_GOOGLE_ID, &GROUP_MEMBERS, NOW), NOW);
    assert_eq!(
        fixture.verify(&group_observation(&GROUP_MEMBERS, NOW), NOW),
        NativeRcsGroupVerification::Proven
    );
    let preparation = fixture.prepare(NOW);

    // A third visible member joins between prepare and begin.
    let changed = group_observation(&[GROUP_MEMBERS[0], GROUP_MEMBERS[1], "+12025550104"], NOW);
    assert!(matches!(
        preparation.begin(serde_json::to_string(&changed).unwrap(), NOW),
        Err(MobileBindingsError::GroupMembersChanged)
    ));
    assert_eq!(fixture.proved_revision(), None, "the group is paused");
}
