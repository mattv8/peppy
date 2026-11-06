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

#[test]
fn history_tokens_are_canonical_opaque_values() {
    let (_directory, client, conversation_id) = client();
    assert!(matches!(
        client.begin_rcs_link_ownership(conversation_id, "not canonical", 1),
        Err(Error::InvalidRequest(_))
    ));
    assert!(matches!(
        client.begin_rcs_link_ownership(conversation_id, "", 1),
        Err(Error::InvalidRequest(_))
    ));
    assert!(matches!(
        client.begin_rcs_link_ownership(conversation_id, &"a".repeat(4_097), 1),
        Err(Error::InvalidRequest(_))
    ));
}

#[test]
fn identity_is_scoped_to_the_google_conversation_and_stale_writes_are_refused() {
    let (_directory, client, first) = client();
    let second = ConversationId::new();
    let first_owner = client
        .begin_rcs_link_ownership(first, "token-one", 7)
        .unwrap();
    let second_owner = client
        .begin_rcs_link_ownership(second, "token-two", 9)
        .unwrap();
    assert_eq!(
        client
            .record_rcs_link_identity(
                first,
                first_owner.ownership_generation,
                "message",
                "chat-one",
                None
            )
            .unwrap(),
        RcsIdentityOutcome::Recorded
    );
    assert_eq!(
        client
            .record_rcs_link_identity(
                second,
                second_owner.ownership_generation,
                "message",
                "chat-two",
                None
            )
            .unwrap(),
        RcsIdentityOutcome::Recorded
    );
    assert_eq!(
        client
            .record_rcs_link_identity(
                first,
                first_owner.ownership_generation + 1,
                "late",
                "chat-one",
                None
            )
            .unwrap(),
        RcsIdentityOutcome::Rejected(RcsReceiveCode::OwnershipStale)
    );
}

#[test]
fn proof_handover_cutover_and_relink_preserve_generations() {
    let (_directory, client, conversation_id) = client();
    let link = client
        .begin_rcs_link_ownership(conversation_id, "token-one", 21)
        .unwrap();
    assert_eq!(link.detector_floor, 21);
    let no_alert = client.begin_rcs_unlink_cutover(conversation_id).unwrap();
    assert!(!no_alert.ambiguous_detector_alert);
    assert_eq!(no_alert.freeze_detector_floor, 21);
    let relink = client
        .begin_rcs_link_ownership(conversation_id, "token-two", 30)
        .unwrap();
    assert_eq!(relink.owner, RcsReceiveOwner::Link);
    assert_eq!(relink.ownership_generation, link.ownership_generation + 2);
    client
        .record_rcs_provider_detector(RcsProviderDetector {
            conversation_id,
            provider_row_id: 31,
            provider_thread_id: 4,
            provider_type: 132,
            observed_at_ms: 1,
            observed_generation: relink.ownership_generation.saturating_sub(1),
        })
        .unwrap();
    let alert = client.begin_rcs_unlink_cutover(conversation_id).unwrap();
    assert!(alert.ambiguous_detector_alert);
    assert_eq!(alert.freeze_detector_floor, 31);
}
