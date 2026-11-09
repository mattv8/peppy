use crate::{
    cleared_draft_after_accepted_send, draft_event_target_labels_after_save,
    draft_view_with_conflict_attachments, load_state_for_window, resolve_draft_conflict_view,
    save_draft_for_window,
    secure_store::MemoryStore,
    send_draft_for_window, session_for_update_flush, session_is_current,
    tests::{fixture_at, gateway, input, open, routed, Fixture, PHRASE},
    upload_public_copy, AppState, BridgeError, PreparedPublicCopy,
};
use axum::{
    extract::{Path as AxumPath, State as AxumState},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use peppy_client_core::{DraftPushOutcome, IncomingSms};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::time::{sleep, Duration};

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
        attachments: moved.attachments.clone(),
        gateway_id: moved.gateway_id.clone(),
        sim_id: moved.sim_id.clone(),
        revision: moved.revision.clone(),
        sync: None,
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

fn synced_pending_draft(
    session: &crate::session::Session,
) -> (String, crate::DraftView, crate::DraftView) {
    session.client.set_draft_sync_supported(true).unwrap();
    let (target, accepted) = saved_draft_for_target(session, "accepted");
    let push = session.client.pending_draft_pushes().unwrap().remove(0);
    session
        .client
        .record_draft_push(
            push.draft_id,
            push.revision_id,
            DraftPushOutcome::Accepted {
                branch_id: uuid::Uuid::new_v4(),
                expires_at_ms: 4_102_444_800_000,
                conflict: false,
            },
        )
        .unwrap();
    let pending = session
        .save_draft(&input(
            &accepted.id,
            &target,
            "pending",
            &["+15555550100"],
            &accepted.revision,
        ))
        .unwrap();
    assert_eq!(session.client.pending_draft_pushes().unwrap().len(), 1);
    (target, accepted, pending)
}

fn known_gateway(session: &crate::session::Session) -> String {
    let gateway_id = uuid::Uuid::new_v4().to_string();
    session.set_status(|status| {
        status.gateways = vec![gateway(&gateway_id, true, true)];
        status.gateways_known = true;
    });
    gateway_id
}

fn pending_send_input(
    target: &str,
    draft: &crate::DraftView,
    gateway_id: &str,
) -> crate::DraftInput {
    routed(
        input(&draft.id, target, "", &[], &draft.revision),
        gateway_id,
        "sim-1",
    )
}

fn put_response(conflict: bool, duplicate: bool, revision_id: &str) -> String {
    json!({
        "branch_id": uuid::Uuid::new_v4(),
        "revision_id": revision_id,
        "expires_at": "2099-12-01T00:00:00Z",
        "conflict": conflict,
        "duplicate": duplicate,
    })
    .to_string()
}

#[tokio::test]
async fn pending_synced_draft_send_pushes_then_remotely_accepts_once() {
    let (origin, seen) = crate::contacts::tests::mock_server(|method, path, body| {
        if method == "PUT" && path.starts_with("/v1/drafts/") {
            return (
                200,
                put_response(false, false, path.rsplit('/').next().unwrap()),
            );
        }
        if method == "POST" && path.ends_with("/send") {
            let command_id = serde_json::from_str::<serde_json::Value>(body).unwrap()["command_id"]
                .as_str()
                .unwrap()
                .to_owned();
            return (
                200,
                json!({ "command_id": command_id, "duplicate": false }).to_string(),
            );
        }
        (404, "{}".into())
    })
    .await;
    let fixture = fixture_at(&origin);
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let gateway_id = known_gateway(&session);
    let (target, _, pending) = synced_pending_draft(&session);

    let sent = send_draft_for_window(
        &state(&fixture),
        &label(&target),
        session,
        pending_send_input(&target, &pending, &gateway_id),
    )
    .await
    .unwrap();

    assert!(sent.accepted);
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.iter()
            .filter(|request| request.method == "PUT")
            .count(),
        1
    );
    assert_eq!(
        seen.iter()
            .filter(|request| request.path.ends_with("/send"))
            .count(),
        1
    );
}

#[tokio::test]
async fn conflicting_inline_push_requires_review_without_remote_send() {
    for duplicate in [false, true] {
        let (origin, seen) = crate::contacts::tests::mock_server(move |method, path, _| {
            if method == "PUT" && path.starts_with("/v1/drafts/") {
                return (
                    200,
                    put_response(true, duplicate, path.rsplit('/').next().unwrap()),
                );
            }
            (404, "{}".into())
        })
        .await;
        let fixture = fixture_at(&origin);
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        unlock(&session, &fixture);
        let gateway_id = known_gateway(&session);
        let (target, _, pending) = synced_pending_draft(&session);

        let error = send_draft_for_window(
            &state(&fixture),
            &label(&target),
            session,
            pending_send_input(&target, &pending, &gateway_id),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, "draft-review-required");
        assert_eq!(
            seen.lock()
                .unwrap()
                .iter()
                .filter(|request| request.path.ends_with("/send"))
                .count(),
            0
        );
    }
}

#[tokio::test]
async fn offline_inline_push_returns_sync_offline_without_remote_send() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let gateway_id = known_gateway(&session);
    let (target, _, pending) = synced_pending_draft(&session);

    let error = send_draft_for_window(
        &state(&fixture),
        &label(&target),
        session,
        pending_send_input(&target, &pending, &gateway_id),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "draft-sync-offline");
}

#[tokio::test]
async fn terminal_inline_push_returns_draft_sent_without_remote_send() {
    let (origin, seen) = crate::contacts::tests::mock_server(|method, path, _| {
        if method == "PUT" && path.starts_with("/v1/drafts/") {
            return (410, json!({ "code": "draft_sent" }).to_string());
        }
        (404, "{}".into())
    })
    .await;
    let fixture = fixture_at(&origin);
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let gateway_id = known_gateway(&session);
    let (target, _, pending) = synced_pending_draft(&session);

    let error = send_draft_for_window(
        &state(&fixture),
        &label(&target),
        session,
        pending_send_input(&target, &pending, &gateway_id),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, "draft-sent");
    assert_eq!(
        seen.lock()
            .unwrap()
            .iter()
            .filter(|request| request.path.ends_with("/send"))
            .count(),
        0
    );
}

#[tokio::test]
async fn held_draft_push_lock_times_out_then_allows_a_later_send() {
    let (origin, seen) = crate::contacts::tests::mock_server(|method, path, body| {
        if method == "PUT" && path.starts_with("/v1/drafts/") {
            return (
                200,
                put_response(false, false, path.rsplit('/').next().unwrap()),
            );
        }
        if method == "POST" && path.ends_with("/send") {
            let command_id = serde_json::from_str::<serde_json::Value>(body).unwrap()["command_id"]
                .as_str()
                .unwrap()
                .to_owned();
            return (
                200,
                json!({ "command_id": command_id, "duplicate": false }).to_string(),
            );
        }
        (404, "{}".into())
    })
    .await;
    let fixture = fixture_at(&origin);
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let gateway_id = known_gateway(&session);
    let (target, _, pending) = synced_pending_draft(&session);
    let lock = session.draft_push_lock.lock().await;

    let error = send_draft_for_window(
        &state(&fixture),
        &label(&target),
        session.clone(),
        pending_send_input(&target, &pending, &gateway_id),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "draft-syncing");
    assert_eq!(
        error.message,
        "This draft could not be synced. Try sending again."
    );
    drop(lock);

    let mut current = pending_send_input(&target, &pending, &gateway_id);
    current.expected_revision = session
        .client
        .compose_draft(pending.id.parse().unwrap())
        .unwrap()
        .unwrap()
        .revision
        .to_string();

    let sent = send_draft_for_window(&state(&fixture), &label(&target), session, current)
        .await
        .unwrap();
    assert!(sent.accepted);
    assert_eq!(
        seen.lock()
            .unwrap()
            .iter()
            .filter(|request| request.path.ends_with("/send"))
            .count(),
        1
    );
}

#[tokio::test]
async fn released_draft_push_lock_before_timeout_allows_send() {
    let (origin, seen) = crate::contacts::tests::mock_server(|method, path, body| {
        if method == "PUT" && path.starts_with("/v1/drafts/") {
            return (
                200,
                put_response(false, false, path.rsplit('/').next().unwrap()),
            );
        }
        if method == "POST" && path.ends_with("/send") {
            let command_id = serde_json::from_str::<serde_json::Value>(body).unwrap()["command_id"]
                .as_str()
                .unwrap()
                .to_owned();
            return (
                200,
                json!({ "command_id": command_id, "duplicate": false }).to_string(),
            );
        }
        (404, "{}".into())
    })
    .await;
    let fixture = fixture_at(&origin);
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let gateway_id = known_gateway(&session);
    let (target, _, pending) = synced_pending_draft(&session);
    let lock = session.draft_push_lock.lock().await;
    let send_state = state(&fixture);
    let send_label = label(&target);
    let send_session = session.clone();
    let input = pending_send_input(&target, &pending, &gateway_id);
    let send = tokio::spawn(async move {
        send_draft_for_window(&send_state, &send_label, send_session, input).await
    });
    sleep(Duration::from_millis(50)).await;
    let puts = || {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|request| request.method == "PUT")
            .count()
    };
    assert_eq!(puts(), 0, "the inline push must wait for the held lock");
    assert!(!send.is_finished());
    drop(lock);

    let sent = send.await.unwrap().unwrap();
    assert!(sent.accepted);
    assert_eq!(puts(), 1);
}

#[test]
fn resolved_draft_view_includes_its_attachment_dtos() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let notifications = Arc::new(AtomicUsize::new(0));
    let session =
        crate::session::open_session(fixture.dir.path(), &fixture.store, &fixture.binding, &[], {
            let notifications = notifications.clone();
            Arc::new(move || {
                notifications.fetch_add(1, Ordering::Relaxed);
            })
        })
        .unwrap();
    unlock(&session, &fixture);
    let source = fixture.dir.path().join("conflict-image.png");
    std::fs::write(&source, crate::media::tests::sample_png(2, 2)).unwrap();
    let attachment = session.prepare_attachment(&source).unwrap();
    let draft = session.client.recipientless_compose_draft().unwrap();
    let mut draft_input = input(
        &draft.draft_id.to_string(),
        &draft.conversation_id.to_string(),
        "image",
        &[],
        &draft.revision.to_string(),
    );
    draft_input.attachment_ids = vec![attachment.id.clone()];
    session.save_draft(&draft_input).unwrap();

    let resolved = resolve_draft_conflict_view(&session, draft.draft_id, None).unwrap();

    assert_eq!(resolved.attachment_ids, vec![attachment.id.clone()]);
    assert_eq!(resolved.attachments.unwrap()[0].id, attachment.id);
    assert_eq!(notifications.load(Ordering::Relaxed), 1);
}

#[test]
fn resolved_draft_view_unions_local_and_conflict_attachment_dtos() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = open(&fixture, &fixture.binding, &[]);
    unlock(&session, &fixture);
    let local_path = fixture.dir.path().join("local-image.png");
    let conflict_path = fixture.dir.path().join("conflict-image.png");
    std::fs::write(&local_path, crate::media::tests::sample_png(2, 2)).unwrap();
    std::fs::write(&conflict_path, crate::media::tests::sample_png(3, 3)).unwrap();
    let local = session.prepare_attachment(&local_path).unwrap();
    let conflict = session.prepare_attachment(&conflict_path).unwrap();
    let draft = session.client.recipientless_compose_draft().unwrap();
    let mut draft_input = input(
        &draft.draft_id.to_string(),
        &draft.conversation_id.to_string(),
        "image",
        &[],
        &draft.revision.to_string(),
    );
    draft_input.attachment_ids = vec![local.id.clone()];
    session.save_draft(&draft_input).unwrap();
    let current = session
        .client
        .compose_draft(draft.draft_id)
        .unwrap()
        .unwrap();

    let view = draft_view_with_conflict_attachments(
        &session,
        &current,
        vec![conflict.id.parse().unwrap()],
    )
    .unwrap();

    let attachments = view.attachments.unwrap();
    assert_eq!(attachments.len(), 2);
    assert!(attachments
        .iter()
        .any(|attachment| attachment.id == local.id));
    assert!(attachments
        .iter()
        .any(|attachment| attachment.id == conflict.id));
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
async fn unnegotiated_attachment_send_is_denied_while_text_send_is_accepted() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    unlock(&session, &fixture);
    let gateway_id = uuid::Uuid::new_v4().to_string();
    session.set_status(|status| {
        status.gateways = vec![gateway(&gateway_id, true, true)];
        status.gateways_known = true;
    });
    let (text_conversation, text_draft) = saved_draft_for_target(&session, "text");
    let state = state(&fixture);

    let sent = send_draft_for_window(
        &state,
        &label(&text_conversation),
        session.clone(),
        routed(
            input(
                &text_draft.id,
                &text_conversation,
                "",
                &[],
                &text_draft.revision,
            ),
            &gateway_id,
            "sim-1",
        ),
    )
    .await
    .unwrap();
    assert!(sent.accepted);

    let source = fixture.dir.path().join("unnegotiated-attachment.png");
    std::fs::write(&source, crate::media::tests::sample_png(8, 8)).unwrap();
    let attachment = session.prepare_attachment(&source).unwrap();
    let attachment_conversation = incoming(&session, "+15555550101", "history");
    let composer = session.client.recipientless_compose_draft().unwrap();
    let mut attachment_input = input(
        &composer.draft_id.to_string(),
        &composer.conversation_id.to_string(),
        "photo",
        &["+15555550101"],
        &composer.revision.to_string(),
    );
    attachment_input.attachment_ids = vec![attachment.id];
    let attachment_draft = session.save_draft(&attachment_input).unwrap();
    assert_eq!(attachment_draft.conversation_id, attachment_conversation);

    let error = send_draft_for_window(
        &state,
        &label(&attachment_conversation),
        session,
        routed(
            input(
                &attachment_draft.id,
                &attachment_conversation,
                "",
                &[],
                &attachment_draft.revision,
            ),
            &gateway_id,
            "sim-1",
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "attachment-retention-negotiating");
}

#[test]
fn contact_photo_set_payload_requires_negotiation_but_keep_remove_and_absent_do_not() {
    let update = |photo| {
        json!({
            "targetBookId": "book",
            "kind": "update",
            "contactId": "contact",
            "baseRevision": "1",
            "patches": [],
            "photo": photo,
        })
    };

    assert!(crate::contact_photo_requires_negotiation(&update(json!({
        "kind": "set",
        "croppedDataUrl": format!("data:image/png;base64,{}", STANDARD.encode(crate::media::tests::sample_png(8, 8)))
    })))
    .unwrap());
    assert!(!crate::contact_photo_requires_negotiation(&update(json!({"kind": "keep"}))).unwrap());
    assert!(
        !crate::contact_photo_requires_negotiation(&update(json!({"kind": "remove"}))).unwrap()
    );
    assert!(!crate::contact_photo_requires_negotiation(&json!({
        "targetBookId": "book",
        "kind": "update",
        "contactId": "contact",
        "baseRevision": "1",
        "patches": []
    }))
    .unwrap());
}

#[tokio::test]
async fn public_copy_storage_policy_removal_tombstones_the_local_attachment_not_the_remote_id() {
    async fn removed(
        AxumPath(remote): AxumPath<String>,
        AxumState(requested_remote): AxumState<Arc<std::sync::Mutex<Option<String>>>>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        *requested_remote.lock().unwrap() = Some(remote);
        (
            StatusCode::GONE,
            Json(json!({"code": "attachment_removed_by_storage_policy"})),
        )
    }

    let requested_remote = Arc::new(std::sync::Mutex::new(None));
    let router = Router::new()
        .route("/v1/attachments/{remote}/public-copies", post(removed))
        .with_state(requested_remote.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let fixture = fixture_at(&origin);
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    let source = fixture.dir.path().join("local-only.png");
    std::fs::write(&source, crate::media::tests::sample_png(8, 8)).unwrap();
    let local = session.prepare_attachment(&source).unwrap();
    let local_id = local.id.parse().unwrap();
    let remote_id = uuid::Uuid::new_v4().to_string();
    assert_ne!(local.id, remote_id);

    let error = match upload_public_copy(
        &session,
        local_id,
        &remote_id,
        PreparedPublicCopy {
            bytes: vec![1],
            name: "copy.png".into(),
            width: 1,
            height: 1,
        },
    )
    .await
    {
        Ok(_) => panic!("a typed storage-policy removal must refuse public publication"),
        Err(error) => error,
    };

    assert_eq!(error.code, "attachment-removed");
    assert_eq!(
        requested_remote.lock().unwrap().as_deref(),
        Some(remote_id.as_str())
    );
    let state = session.client.attachment_retention_state(local_id).unwrap();
    assert_eq!(
        state.remote,
        peppy_client_core::RemoteAttachmentState::RemovedByStoragePolicy
    );
    assert!(state.local_available);
    server.abort();
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
