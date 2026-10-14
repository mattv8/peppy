use crate::{
    load_state_for_window, save_draft_for_window,
    secure_store::MemoryStore,
    send_draft_for_window,
    tests::{fixture_at, gateway, input, open, routed, Fixture, PHRASE},
    AppState,
};
use peppy_client_core::IncomingSms;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn unlock(session: &crate::session::Session, fixture: &Fixture) {
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

fn label(conversation: &str) -> String {
    format!("composer-{conversation}")
}

fn incoming(session: &crate::session::Session, sender: &str, body: &str) -> String {
    session
        .client
        .capture_incoming(IncomingSms {
            conversation_id: None,
            sender_address: sender.into(),
            body: body.into(),
            provider_message_id: Some(uuid::Uuid::new_v4().to_string()),
            imported: false,
        })
        .unwrap()
        .conversation_id
        .to_string()
}

#[tokio::test]
async fn main_load_for_window_preserves_requested_second_conversation() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    incoming(&session, "+15555550100", "first");
    let second = incoming(&session, "+15555550101", "second");
    session
        .save_draft(&input("draft-new", &second, "second draft", &[], "0"))
        .unwrap();

    let (snapshot, _) = load_state_for_window(
        &state(&fixture),
        crate::tray::MAIN,
        session,
        Some(second.clone()),
        None,
        None,
        Default::default(),
    )
    .await
    .unwrap();

    assert_eq!(
        snapshot.active_conversation_id.as_deref(),
        Some(second.as_str())
    );
    assert_eq!(snapshot.draft.unwrap().text, "second draft");
    assert_eq!(
        snapshot
            .conversations
            .iter()
            .find(|conversation| conversation.id == second)
            .unwrap()
            .messages[0]
            .body,
        "second"
    );
}

#[tokio::test]
async fn composer_save_move_updates_scope_for_next_load_and_save() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let target = incoming(&session, "+15555550100", "history");
    let source = session.client.recipientless_compose_draft().unwrap();
    let source_id = source.conversation_id.to_string();
    let window = label(&source_id);
    let state = state(&fixture);

    let moved = save_draft_for_window(
        &state,
        &window,
        session.clone(),
        input(
            &source.draft_id.to_string(),
            &source_id,
            "draft",
            &["+15555550100"],
            "0",
        ),
    )
    .await
    .unwrap();
    assert_eq!(moved.conversation_id, target);

    let (snapshot, _) = load_state_for_window(
        &state,
        &window,
        session.clone(),
        Some(target.clone()),
        None,
        None,
        Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        snapshot.active_conversation_id.as_deref(),
        Some(target.as_str())
    );
    let retry = save_draft_for_window(
        &state,
        &window,
        session.clone(),
        input(
            &source.draft_id.to_string(),
            &source_id,
            "old-source retry",
            &["+15555550100"],
            &moved.revision,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(retry.code, "draft-moved");
    assert!(state.composer_scopes.lock().unwrap().contains_key(&window));
    let saved = save_draft_for_window(
        &state,
        &window,
        session,
        input(
            &moved.id,
            &target,
            "updated",
            &["+15555550100"],
            &moved.revision,
        ),
    )
    .await
    .unwrap();
    assert_eq!(saved.conversation_id, target);
}

#[tokio::test]
async fn foreign_old_draft_is_rejected_by_save_and_send_without_scope_change() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let gateway_id = uuid::Uuid::new_v4().to_string();
    session.set_status(|status| {
        status.gateways = vec![gateway(&gateway_id, true, true)];
        status.gateways_known = true;
    });
    let target = incoming(&session, "+15555550100", "history");
    let old = session.client.recipientless_compose_draft().unwrap();
    let moved = session
        .save_draft(&input(
            &old.draft_id.to_string(),
            &old.conversation_id.to_string(),
            "foreign",
            &["+15555550100"],
            "0",
        ))
        .unwrap();
    assert_eq!(moved.conversation_id, target);
    let own = session.client.recipientless_compose_draft().unwrap();
    let own_id = own.conversation_id.to_string();
    let window = label(&own_id);
    let state = state(&fixture);
    assert_eq!(
        save_draft_for_window(
            &state,
            &window,
            session.clone(),
            input(&old.draft_id.to_string(), &own_id, "forged", &[], "0"),
        )
        .await
        .unwrap_err()
        .code,
        "invalid-draft"
    );
    assert_eq!(
        send_draft_for_window(
            &state,
            &window,
            session,
            routed(
                input(&old.draft_id.to_string(), &own_id, "forged", &[], "0"),
                &gateway_id,
                "sim-1",
            ),
        )
        .await
        .unwrap_err()
        .code,
        "invalid-draft"
    );
    assert!(!state.composer_scopes.lock().unwrap().contains_key(&window));
}

#[tokio::test]
async fn deleted_draft_is_not_found_by_save_and_send_without_scope_change() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let gateway_id = uuid::Uuid::new_v4().to_string();
    session.set_status(|status| {
        status.gateways = vec![gateway(&gateway_id, true, true)];
        status.gateways_known = true;
    });
    let draft = session.client.recipientless_compose_draft().unwrap();
    let draft_id = draft.draft_id.to_string();
    let conversation_id = draft.conversation_id.to_string();
    session
        .client
        .discard_compose_draft(draft.draft_id, draft.revision)
        .unwrap();
    let window = label(&conversation_id);
    let state = state(&fixture);

    assert_eq!(
        save_draft_for_window(
            &state,
            &window,
            session.clone(),
            input(&draft_id, &conversation_id, "retired", &[], "0"),
        )
        .await
        .unwrap_err()
        .code,
        "not-found"
    );
    assert_eq!(
        send_draft_for_window(
            &state,
            &window,
            session.clone(),
            routed(
                input(&draft_id, &conversation_id, "retired", &[], "0"),
                &gateway_id,
                "sim-1",
            ),
        )
        .await
        .unwrap_err()
        .code,
        "not-found"
    );
    assert!(!state.composer_scopes.lock().unwrap().contains_key(&window));
    assert!(session.client.pending_outbox_batch(10).unwrap().is_empty());
}

#[tokio::test]
async fn unknown_composer_load_never_falls_back_to_known_conversation() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let known = incoming(&session, "+15555550100", "known");
    let deleted = uuid::Uuid::new_v4().to_string();

    let result = load_state_for_window(
        &state(&fixture),
        &label(&deleted),
        session,
        Some(deleted),
        None,
        None,
        Default::default(),
    )
    .await;
    let error = match result {
        Ok(_) => panic!("unknown composer load must not select another conversation"),
        Err(error) => error,
    };
    assert_eq!(error.code, "not-found");
    assert_ne!(error.conversation_id.as_deref(), Some(known.as_str()));
}

#[tokio::test]
async fn normal_autosave_for_window_does_not_notify_or_create_or_move() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let notifications = Arc::new(AtomicUsize::new(0));
    let notifier = {
        let notifications = notifications.clone();
        Arc::new(move || {
            notifications.fetch_add(1, Ordering::Relaxed);
        })
    };
    let session = Arc::new(
        crate::session::open_session(
            fixture.dir.path(),
            &fixture.store,
            &fixture.binding,
            &[],
            notifier,
        )
        .unwrap(),
    );
    unlock(&session, &fixture);
    let draft = session.client.recipientless_compose_draft().unwrap();
    let before = session.client.compose_drafts().unwrap().len();

    let saved = save_draft_for_window(
        &state(&fixture),
        crate::tray::MAIN,
        session.clone(),
        input(
            &draft.draft_id.to_string(),
            &draft.conversation_id.to_string(),
            "autosaved",
            &[],
            &draft.revision.to_string(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(notifications.load(Ordering::Relaxed), 0);
    assert_eq!(session.client.compose_drafts().unwrap().len(), before);
    assert_eq!(saved.id, draft.draft_id.to_string());
    assert_eq!(saved.conversation_id, draft.conversation_id.to_string());
}
