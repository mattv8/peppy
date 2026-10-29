//! Send-path recents tests over real client-core databases. They drive `send_draft_for_window`
//! directly so the recorded conversation comes from the same code the desktop send uses.
use crate::{
    credentials::Binding,
    secure_store::MemoryStore,
    send_draft_for_window,
    session::Session,
    tests::{fixture_at, gateway, input, open, routed, Fixture, PHRASE},
    tray, AppState, DraftView,
};
use peppy_client_core::IncomingSms;
use std::{fs, sync::Arc};

fn unlock(session: &Session, fixture: &Fixture) {
    session
        .client
        .unlock(&fixture.profile, &fixture.header, PHRASE)
        .unwrap();
}

fn state(fixture: &Fixture) -> AppState {
    AppState::new(
        fixture.dir.path().to_path_buf(),
        Arc::new(MemoryStore::default()),
        Arc::new(|| {}),
    )
}

fn other_binding() -> Binding {
    Binding {
        origin: "https://other.test".into(),
        vault_id: "other-vault".into(),
        device_id: "other-device".into(),
    }
}

fn incoming_conversation(session: &Session, sender: &str) -> String {
    session
        .client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: sender.into(),
            body: "history".into(),
            provider_message_id: Some(uuid::Uuid::new_v4().to_string()),
            imported: false,
        })
        .unwrap()
        .conversation_id
        .to_string()
}

/// Saves a recipient-bearing draft that moves into an existing conversation, as the composer does.
fn saved_draft_for_target(session: &Session, text: &str) -> (String, DraftView) {
    let target = incoming_conversation(session, "+15555550100");
    let source = session.client.recipientless_compose_draft().unwrap();
    let saved = session
        .save_draft(&input(
            &source.draft_id.to_string(),
            &source.conversation_id.to_string(),
            text,
            &["+15555550100"],
            "0",
        ))
        .unwrap();
    assert_eq!(saved.conversation_id, target);
    (target, saved)
}

/// Makes the session's gateway capabilities known so a routed send can be queued.
fn known_gateway(session: &Session) -> String {
    let gateway_id = uuid::Uuid::new_v4().to_string();
    session.set_status(|status| {
        status.gateways = vec![gateway(&gateway_id, true, true)];
        status.gateways_known = true;
    });
    gateway_id
}

fn recents(state: &AppState, binding: &Binding) -> Vec<String> {
    state.recents.list(binding).unwrap()
}

#[tokio::test]
async fn accepted_send_records_its_conversation_and_leaves_other_bindings_alone() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let gateway_id = known_gateway(&session);
    let (target, saved) = saved_draft_for_target(&session, "hello");
    let state = state(&fixture);
    let other = other_binding();
    let other_id = uuid::Uuid::new_v4().to_string();
    state.recents.touch(&other, &other_id, 1).unwrap();

    let sent = send_draft_for_window(
        &state,
        tray::MAIN,
        session.clone(),
        routed(
            input(&saved.id, &target, "", &[], &saved.revision),
            &gateway_id,
            "sim-1",
        ),
    )
    .await
    .unwrap();

    assert!(sent.accepted);
    assert_eq!(recents(&state, &fixture.binding), vec![target]);
    assert_eq!(recents(&state, &other), vec![other_id]);
}

#[tokio::test]
async fn accepted_send_with_empty_conversation_records_the_draft_conversation() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let gateway_id = known_gateway(&session);
    let (target, saved) = saved_draft_for_target(&session, "hello");
    let state = state(&fixture);

    let sent = send_draft_for_window(
        &state,
        tray::MAIN,
        session.clone(),
        routed(
            input(&saved.id, "", "", &[], &saved.revision),
            &gateway_id,
            "sim-1",
        ),
    )
    .await
    .unwrap();

    assert!(sent.accepted);
    assert_eq!(recents(&state, &fixture.binding), vec![target]);
}

#[tokio::test]
async fn rejected_send_leaves_recent_history_unchanged() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    // No gateway status is known, so the send is refused before anything is queued.
    let gateway_id = uuid::Uuid::new_v4().to_string();
    let (target, saved) = saved_draft_for_target(&session, "hello");
    let state = state(&fixture);
    let prior = uuid::Uuid::new_v4().to_string();
    state.recents.touch(&fixture.binding, &prior, 1).unwrap();
    let before = recents(&state, &fixture.binding);
    let queued_before = session.client.pending_outbox_batch(10).unwrap().len();

    let error = send_draft_for_window(
        &state,
        tray::MAIN,
        session.clone(),
        routed(
            input(&saved.id, &target, "", &[], &saved.revision),
            &gateway_id,
            "sim-1",
        ),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "gateways-unknown");
    assert_eq!(recents(&state, &fixture.binding), before);
    assert_eq!(recents(&state, &fixture.binding), vec![prior]);
    assert_eq!(
        session.client.pending_outbox_batch(10).unwrap().len(),
        queued_before
    );
}

#[tokio::test]
async fn accepted_send_stays_accepted_when_recent_persistence_fails() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let gateway_id = known_gateway(&session);
    let (target, saved) = saved_draft_for_target(&session, "hello");
    let state = state(&fixture);
    // A directory at the store path makes every recents read and write fail.
    fs::create_dir(fixture.dir.path().join("recent-conversations.json")).unwrap();
    let queued_before = session.client.pending_outbox_batch(10).unwrap().len();

    let sent = send_draft_for_window(
        &state,
        tray::MAIN,
        session.clone(),
        routed(
            input(&saved.id, &target, "", &[], &saved.revision),
            &gateway_id,
            "sim-1",
        ),
    )
    .await
    .unwrap();

    assert!(sent.accepted);
    assert!(state.recents.list(&fixture.binding).is_err());
    assert_eq!(
        session.client.pending_outbox_batch(10).unwrap().len(),
        queued_before + 1
    );
}
