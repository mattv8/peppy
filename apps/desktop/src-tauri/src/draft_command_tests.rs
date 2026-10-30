use crate::{
    cleared_draft_after_accepted_send, draft_event_target_labels_after_save, load_state_for_window,
    save_draft_for_window,
    secure_store::MemoryStore,
    send_draft_for_window, session_for_update_flush, session_is_current,
    tests::{fixture_at, gateway, input, open, routed, Fixture, PHRASE},
    AppState, BridgeError,
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
async fn update_flush_rejects_a_cancelled_retained_session() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    let state = state(&fixture);
    *state.session.lock().await = Some(session.clone());

    let active = session_for_update_flush(&state).await.unwrap();
    assert!(Arc::ptr_eq(&active, &session));
    assert!(session_is_current(&state, &session).await);

    session.cancel.cancel();

    assert!(!session_is_current(&state, &session).await);
    let error = match session_for_update_flush(&state).await {
        Ok(_) => panic!("a cancelled retained session must not accept an update flush"),
        Err(error) => error,
    };
    assert_eq!(error.code, BridgeError::no_session().code);
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
async fn blank_recovery_create_works_without_old_history_and_never_overwrites_an_occupied_draft() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);

    let created = session
        .save_draft(&input("", "", "recovered", &["+15555550100"], "0"))
        .unwrap();
    assert_eq!(created.text, "recovered");
    assert!(!created.conversation_id.is_empty());

    let occupied = session
        .save_draft(&input("", "", "must not overwrite", &["+15555550100"], "0"))
        .unwrap_err();
    assert_eq!(occupied.code, "draft-moved");
    let preserved = session
        .client
        .compose_draft(created.id.parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(preserved.text, "recovered");
}

#[tokio::test]
async fn composer_blank_recovery_rebinds_after_a_deleted_draft_without_history() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let old = session.client.recipientless_compose_draft().unwrap();
    let old_conversation = old.conversation_id.to_string();
    session
        .client
        .discard_compose_draft(old.draft_id, old.revision)
        .unwrap();
    let state = state(&fixture);
    let window = label(&old_conversation);

    let recovered = save_draft_for_window(
        &state,
        &window,
        session.clone(),
        input("", "", "recovered", &["+15555550100"], "0"),
    )
    .await
    .unwrap();
    assert_ne!(recovered.conversation_id, old_conversation);
    assert_eq!(recovered.text, "recovered");
    assert_eq!(recovered.recipient_ids, ["+15555550100"]);

    let saved = save_draft_for_window(
        &state,
        &window,
        session.clone(),
        input(
            &recovered.id,
            &recovered.conversation_id,
            "saved again",
            &["+15555550100"],
            &recovered.revision,
        ),
    )
    .await
    .unwrap();
    assert_eq!(saved.text, "saved again");
    let (snapshot, _) = load_state_for_window(
        &state,
        &window,
        session,
        Some(recovered.conversation_id.clone()),
        None,
        None,
        Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        snapshot.active_conversation_id.as_deref(),
        Some(recovered.conversation_id.as_str())
    );
}

#[tokio::test]
async fn composer_blank_recovery_rejects_an_occupied_bound_conversation() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let draft = session.client.recipientless_compose_draft().unwrap();
    let conversation = draft.conversation_id.to_string();
    let window = label(&conversation);
    let state = state(&fixture);
    session
        .save_draft(&input(
            &draft.draft_id.to_string(),
            &conversation,
            "keep this",
            &[],
            &draft.revision.to_string(),
        ))
        .unwrap();

    let error = save_draft_for_window(
        &state,
        &window,
        session.clone(),
        input("", "", "must not overwrite", &["+15555550100"], "0"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "window-context");
    let preserved = session
        .client
        .compose_draft(draft.draft_id)
        .unwrap()
        .unwrap();
    assert_eq!(preserved.text, "keep this");
}

#[tokio::test]
async fn composer_blank_recovery_cannot_target_another_conversation() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let source = session.client.recipientless_compose_draft().unwrap();
    let other = incoming(&session, "+15555550100", "history");

    let error = save_draft_for_window(
        &state(&fixture),
        &label(&source.conversation_id.to_string()),
        session,
        input("", &other, "forged", &["+15555550100"], "0"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "window-context");
}

#[tokio::test]
async fn composer_blank_recovery_rejects_a_bound_history_conversation_without_a_draft() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let bound = incoming(&session, "+15555550100", "bound history");
    let other = incoming(&session, "+15555550101", "other history");

    let error = save_draft_for_window(
        &state(&fixture),
        &label(&bound),
        session.clone(),
        input("", "", "forged", &["+15555550101"], "0"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "window-context");
    assert!(session.client.compose_drafts().unwrap().is_empty());
    assert_eq!(
        session
            .client
            .latest_message(bound.parse().unwrap())
            .unwrap()
            .unwrap()
            .payload
            .body,
        "bound history"
    );
    assert_eq!(
        session
            .client
            .latest_message(other.parse().unwrap())
            .unwrap()
            .unwrap()
            .payload
            .body,
        "other history"
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

#[tokio::test]
async fn normal_autosave_targets_main_and_matching_composers_without_broad_notify() {
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
    let conversation = draft.conversation_id.to_string();
    let state = state(&fixture);

    let saved = save_draft_for_window(
        &state,
        crate::tray::MAIN,
        session.clone(),
        input(
            &draft.draft_id.to_string(),
            &conversation,
            "autosaved",
            &[],
            &draft.revision.to_string(),
        ),
    )
    .await;

    let targets = draft_event_target_labels_after_save(
        &state,
        &session,
        &saved,
        false,
        true,
        [
            crate::tray::MAIN,
            &label(&conversation),
            &label(&uuid::Uuid::new_v4().to_string()),
        ],
    );
    assert_eq!(
        targets,
        vec![crate::tray::MAIN.to_owned(), label(&conversation)]
    );
    assert_eq!(notifications.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn draft_event_targets_composer_after_move_and_not_unrelated_composer() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let target = incoming(&session, "+15555550100", "history");
    let source = session.client.recipientless_compose_draft().unwrap();
    let source_id = source.conversation_id.to_string();
    let window = label(&source_id);
    let state = state(&fixture);

    let saved = save_draft_for_window(
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
    .await;

    let moved = saved.as_ref().unwrap();
    assert_eq!(moved.conversation_id, target);
    let destination = label(&target);
    let targets = draft_event_target_labels_after_save(
        &state,
        &session,
        &saved,
        false,
        true,
        [
            crate::tray::MAIN,
            window.as_str(),
            destination.as_str(),
            &label(&uuid::Uuid::new_v4().to_string()),
        ],
    );
    assert_eq!(
        targets,
        vec![crate::tray::MAIN.to_owned(), window.clone(), destination]
    );

    let original = crate::DraftView {
        id: moved.id.clone(),
        conversation_id: source_id,
        text: moved.text.clone(),
        recipient_ids: moved.recipient_ids.clone(),
        attachment_ids: moved.attachment_ids.clone(),
        gateway_id: moved.gateway_id.clone(),
        sim_id: moved.sim_id.clone(),
        revision: moved.revision.clone(),
    };
    assert_eq!(
        draft_event_target_labels_after_save(
            &state,
            &session,
            &Ok(original),
            false,
            true,
            [crate::tray::MAIN, window.as_str()],
        ),
        vec![crate::tray::MAIN]
    );
}

#[tokio::test]
async fn saved_draft_event_gate_blocks_failures_locked_and_replaced_sessions() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let draft = session.client.recipientless_compose_draft().unwrap();
    let state = state(&fixture);
    let saved = save_draft_for_window(
        &state,
        crate::tray::MAIN,
        session.clone(),
        input(
            &draft.draft_id.to_string(),
            &draft.conversation_id.to_string(),
            "saved",
            &[],
            &draft.revision.to_string(),
        ),
    )
    .await;
    assert_eq!(
        draft_event_target_labels_after_save(
            &state,
            &session,
            &saved,
            false,
            true,
            [crate::tray::MAIN],
        ),
        vec![crate::tray::MAIN]
    );
    for (locked, current) in [(true, true), (false, false)] {
        assert!(draft_event_target_labels_after_save(
            &state,
            &session,
            &saved,
            locked,
            current,
            [
                crate::tray::MAIN,
                label(&draft.conversation_id.to_string()).as_str(),
            ],
        )
        .is_empty());
    }
    let failed = save_draft_for_window(
        &state,
        crate::tray::MAIN,
        session.clone(),
        input(
            &draft.draft_id.to_string(),
            &draft.conversation_id.to_string(),
            "stale",
            &[],
            "999",
        ),
    )
    .await;

    assert!(failed.is_err());
    assert!(draft_event_target_labels_after_save(
        &state,
        &session,
        &failed,
        false,
        true,
        [
            crate::tray::MAIN,
            label(&draft.conversation_id.to_string()).as_str(),
        ],
    )
    .is_empty());
}

/// Saves a recipient-bearing draft that moves into an existing conversation.
fn saved_draft_for_target(
    session: &crate::session::Session,
    text: &str,
) -> (String, crate::DraftView) {
    let target = incoming(session, "+15555550100", "history");
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

#[tokio::test]
async fn accepted_send_loads_the_retained_cleared_draft_for_matching_composers_only() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let gateway_id = uuid::Uuid::new_v4().to_string();
    session.set_status(|status| {
        status.gateways = vec![gateway(&gateway_id, true, true)];
        status.gateways_known = true;
    });
    let (target, saved) = saved_draft_for_target(&session, "hello");
    let state = state(&fixture);
    let window = label(&target);

    let sent = send_draft_for_window(
        &state,
        &window,
        session.clone(),
        routed(
            input(&saved.id, &target, "", &[], &saved.revision),
            &gateway_id,
            "sim-1",
        ),
    )
    .await;
    let accepted = sent.as_ref().is_ok_and(|sent| sent.accepted);
    assert!(accepted);
    let sent_revision = sent.as_ref().unwrap().revision.clone();

    let cleared = cleared_draft_after_accepted_send(&session, &saved.id, accepted).unwrap();
    assert_eq!(cleared.id, saved.id);
    assert_eq!(cleared.conversation_id, target);
    assert_eq!(cleared.text, "");
    assert!(cleared.attachment_ids.is_empty());
    assert_eq!(cleared.recipient_ids, vec!["+15555550100".to_string()]);
    assert_eq!(cleared.gateway_id.as_deref(), Some(gateway_id.as_str()));
    assert_eq!(cleared.sim_id.as_deref(), Some("sim-1"));
    assert_eq!(Some(cleared.revision.clone()), sent_revision);

    let unrelated = label(&uuid::Uuid::new_v4().to_string());
    let locked = cleared_draft_after_accepted_send(&session, &saved.id, accepted).unwrap();
    assert!(draft_event_target_labels_after_save(
        &state,
        &session,
        &Ok(locked),
        true,
        true,
        [crate::tray::MAIN, window.as_str()],
    )
    .is_empty());
    assert_eq!(
        draft_event_target_labels_after_save(
            &state,
            &session,
            &Ok(cleared),
            false,
            true,
            [crate::tray::MAIN, window.as_str(), unrelated.as_str()],
        ),
        vec![crate::tray::MAIN.to_owned(), window]
    );
}

#[tokio::test]
async fn rejected_send_retains_the_draft_and_loads_no_cleared_broadcast() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let gateway_id = uuid::Uuid::new_v4().to_string();
    let (target, saved) = saved_draft_for_target(&session, "hello");
    let queued_before = session.client.pending_outbox_batch(10).unwrap().len();
    let state = state(&fixture);

    // Gateway capabilities are unknown, so the send is refused before anything is queued.
    let sent = send_draft_for_window(
        &state,
        &label(&target),
        session.clone(),
        routed(
            input(&saved.id, &target, "", &[], &saved.revision),
            &gateway_id,
            "sim-1",
        ),
    )
    .await;
    assert_eq!(sent.as_ref().unwrap_err().code, "gateways-unknown");
    let accepted = sent.as_ref().is_ok_and(|sent| sent.accepted);
    assert!(cleared_draft_after_accepted_send(&session, &saved.id, accepted).is_none());
    let draft = session
        .client
        .compose_draft(saved.id.parse().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(draft.text, "hello");
    assert_eq!(
        session.client.pending_outbox_batch(10).unwrap().len(),
        queued_before
    );
}

#[tokio::test]
async fn accepted_send_without_a_readable_cleared_draft_broadcasts_nothing() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);

    assert!(cleared_draft_after_accepted_send(&session, "not-a-draft-id", true).is_none());
    assert!(
        cleared_draft_after_accepted_send(&session, &uuid::Uuid::new_v4().to_string(), true)
            .is_none()
    );
}

#[tokio::test]
async fn poisoned_composer_scope_limits_draft_event_to_main() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let draft = session.client.recipientless_compose_draft().unwrap();
    let state = state(&fixture);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _scope = state.composer_scopes.lock().unwrap();
        panic!("poison composer scope");
    }));

    let saved = session
        .save_draft(&input(
            &draft.draft_id.to_string(),
            &draft.conversation_id.to_string(),
            "autosaved",
            &[],
            &draft.revision.to_string(),
        ))
        .unwrap();
    assert_eq!(
        draft_event_target_labels_after_save(
            &state,
            &session,
            &Ok(saved),
            false,
            true,
            [
                crate::tray::MAIN,
                label(&draft.conversation_id.to_string()).as_str(),
            ],
        ),
        vec![crate::tray::MAIN]
    );
}
