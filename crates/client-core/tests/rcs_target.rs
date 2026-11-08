mod common;

use common::*;

#[test]
fn public_target_classification_requires_current_core_route_context() {
    let directory = tempfile::tempdir().unwrap();
    let vault = Vault::new();
    let config = config(&directory, "rcs-target", &vault);
    let client = unlocked(&config, &vault);
    let decision = client
        .classify_and_commit_rcs_target(ConversationId::new(), "+12025550123", "{}", 1_000)
        .unwrap();
    assert!(matches!(decision, RcsTargetDecision::Unknown { .. }));
    assert!(client.carrier_route_context("sim-test").unwrap().is_none());
}
