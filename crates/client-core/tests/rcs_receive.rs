#[path = "common/mod.rs"]
mod common;

use common::*;

fn client() -> (tempfile::TempDir, Client, ConversationId) {
    let directory = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-receive", &vault);
    (directory, open(&config), ConversationId::new())
}

#[test]
fn link_ownership_is_generation_fenced_and_identities_are_replayed() {
    let (_directory, client, conversation_id) = client();
    let ownership = client
        .begin_rcs_link_ownership(conversation_id, "opaque-page-token", 41)
        .unwrap();
    assert_eq!(ownership.owner, RcsReceiveOwner::Link);
    assert_eq!(ownership.ownership_generation, 1);
    assert!(matches!(
        client.record_rcs_link_identity(
            conversation_id,
            ownership.ownership_generation,
            "google-message",
            "google-conversation",
            Some("opaque-page-token"),
        ),
        Ok(RcsIdentityOutcome::Recorded)
    ));
    assert!(matches!(
        client.record_rcs_link_identity(
            conversation_id,
            ownership.ownership_generation,
            "google-message",
            "google-conversation",
            None,
        ),
        Ok(RcsIdentityOutcome::Duplicate)
    ));
    assert!(matches!(
        client.record_rcs_link_identity(
            conversation_id,
            ownership.ownership_generation + 1,
            "stale-message",
            "google-conversation",
            None,
        ),
        Ok(RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale))
    ));
}

#[test]
fn unlink_freezes_link_and_marks_ambiguous_detectors_for_alert() {
    let (_directory, client, conversation_id) = client();
    let link = client
        .begin_rcs_link_ownership(conversation_id, "opaque-page-token", 10)
        .unwrap();
    client
        .record_rcs_provider_detector(RcsProviderDetector {
            conversation_id,
            provider_row_id: 10,
            provider_thread_id: 2,
            provider_type: 132,
            observed_at_ms: 100,
            observed_generation: link.ownership_generation,
        })
        .unwrap();
    client
        .record_rcs_provider_detector(RcsProviderDetector {
            conversation_id,
            provider_row_id: 11,
            provider_thread_id: 2,
            provider_type: 132,
            observed_at_ms: 101,
            observed_generation: link.ownership_generation,
        })
        .unwrap();

    let cutover = client.begin_rcs_unlink_cutover(conversation_id).unwrap();
    assert_eq!(cutover.owner, RcsReceiveOwner::Provider);
    assert_eq!(cutover.freeze_detector_floor, 11);
    assert!(cutover.ambiguous_detector_alert);
    assert!(matches!(
        client.record_rcs_link_identity(
            conversation_id,
            link.ownership_generation,
            "late-message",
            "google-conversation",
            None,
        ),
        Ok(RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale))
    ));
}
