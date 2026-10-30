use crate::{
    composer_scope::MovedScope,
    media_viewer_scope, require_media_viewer_parent, scoped_window_conversation,
    secure_store::MemoryStore,
    session::Session,
    tests::{fixture_at, noop, open, Fixture},
    window_conversation_scope, AppState, MediaViewer,
};
use peppy_client_core::ConversationId;
use std::sync::Arc;

fn state(fixture: &Fixture) -> AppState {
    AppState::new(
        fixture.dir.path().to_path_buf(),
        Arc::new(MemoryStore::default()),
        noop(),
    )
}

fn composer_label(conversation: ConversationId) -> String {
    format!("composer-{conversation}")
}

fn media_label(conversation: ConversationId) -> String {
    format!("media-{conversation}-0")
}

fn register_viewer(
    state: &AppState,
    label: String,
    parent_label: String,
    conversation: ConversationId,
    session: Arc<Session>,
) {
    state.media_viewers.lock().unwrap().insert(
        label,
        MediaViewer {
            window: None,
            parent_label,
            conversation,
            session,
        },
    );
}

#[test]
fn registered_viewer_allows_only_its_conversation_and_session() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    let state = state(&fixture);
    let conversation = ConversationId::new();
    let label = media_label(conversation);

    register_viewer(
        &state,
        label.clone(),
        composer_label(conversation),
        conversation,
        session.clone(),
    );

    assert!(media_viewer_scope(&state, &label, &session, conversation).is_ok());
    assert!(media_viewer_scope(&state, &label, &session, ConversationId::new()).is_err());
}

#[test]
fn missing_or_stale_viewer_registry_entry_is_rejected() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    let state = state(&fixture);
    let conversation = ConversationId::new();
    let label = media_label(conversation);

    assert!(media_viewer_scope(&state, &label, &session, conversation).is_err());

    register_viewer(
        &state,
        label.clone(),
        composer_label(conversation),
        conversation,
        session.clone(),
    );
    let replacement = Arc::new(open(&fixture, &fixture.binding, &[]));

    assert!(media_viewer_scope(&state, &label, &replacement, conversation).is_err());
}

#[test]
fn only_composer_labels_are_valid_media_viewer_parents() {
    let conversation = ConversationId::new();

    assert_eq!(
        require_media_viewer_parent(&composer_label(conversation)).unwrap(),
        conversation
    );
    for label in [
        "main".to_owned(),
        media_label(conversation),
        "random-window".to_owned(),
    ] {
        assert!(require_media_viewer_parent(&label).is_err(), "{label}");
    }
}

#[test]
fn moved_composer_scope_allows_only_its_target_conversation() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    let state = state(&fixture);
    let source = ConversationId::new();
    let target = ConversationId::new();
    let parent = composer_label(source);
    state.composer_scopes.lock().unwrap().insert(
        parent.clone(),
        MovedScope {
            binding: session.binding.clone(),
            conversation: target,
        },
    );

    assert_eq!(
        scoped_window_conversation(&state, &parent, &session).unwrap(),
        target
    );
    assert!(window_conversation_scope(&state, &parent, &session, target).is_ok());
    assert!(window_conversation_scope(&state, &parent, &session, source).is_err());
    assert!(window_conversation_scope(&state, &parent, &session, ConversationId::new()).is_err());
}

#[test]
fn viewer_for_old_conversation_is_rejected_after_parent_moves() {
    let fixture = fixture_at("http://127.0.0.1:9");
    let session = Arc::new(open(&fixture, &fixture.binding, &[]));
    let state = state(&fixture);
    let source = ConversationId::new();
    let target = ConversationId::new();
    let parent = composer_label(source);
    let label = media_label(source);
    register_viewer(
        &state,
        label.clone(),
        parent.clone(),
        source,
        session.clone(),
    );
    state.composer_scopes.lock().unwrap().insert(
        parent,
        MovedScope {
            binding: session.binding.clone(),
            conversation: target,
        },
    );

    assert!(media_viewer_scope(&state, &label, &session, source).is_err());
    assert!(window_conversation_scope(&state, &label, &session, source).is_err());
}
