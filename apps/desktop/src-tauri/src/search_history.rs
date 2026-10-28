//! Desktop host adapter for the core-owned history-search coordinator.
use crate::{
    admit_update_operation,
    error::{core_error, BridgeError, BridgeResult},
    require_main,
    session::Session,
    sync::blocking,
    AppState,
};
use peppy_client_core::HistorySearchWork;
use peppy_desktop_api::{
    HistorySearchAttemptInput, HistorySearchSnapshot, StartHistorySearchInput,
};
use std::sync::{atomic::Ordering, Arc};
use tauri::State;
use tokio::time::{sleep, Duration};

const MAX_CONSECUTIVE_DRIVER_ERRORS: usize = 3;
use tokio_util::sync::CancellationToken;

fn snapshot(status: peppy_client_core::HistorySearchStatus) -> HistorySearchSnapshot {
    HistorySearchSnapshot::from_core(status)
}

fn locked(state: &AppState) -> BridgeResult<()> {
    if state.app_locked.load(Ordering::Acquire) {
        return Err(BridgeError::new("app-locked", "Peppy is locked."));
    }
    Ok(())
}

async fn current(state: &AppState, session: &Arc<Session>) -> bool {
    state
        .session
        .lock()
        .await
        .as_ref()
        .is_some_and(|candidate| Arc::ptr_eq(candidate, session))
}

#[tauri::command]
pub async fn get_history_search(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
) -> BridgeResult<HistorySearchSnapshot> {
    let _operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    locked(&state)?;
    // A status read must not create a session, wake networking, or open the encrypted store.
    // Starting a session is an explicit user action elsewhere; no active session is idle.
    let session = state.session.lock().await.clone();
    let Some(session) = session else {
        return Ok(HistorySearchSnapshot {
            attempt_id: None,
            state: peppy_desktop_api::HistorySearchState::Idle,
            reason: None,
            can_search_further: false,
            progress_version: "0".into(),
            index_version: "0".into(),
        });
    };
    let attempt = session.history_view(window.label());
    let client = session.client.clone();
    let status =
        blocking(move || client.history_search_status(attempt).map_err(core_error)).await?;
    if !current(&state, &session).await || state.app_locked.load(Ordering::Acquire) {
        return Err(BridgeError::new("app-locked", "Peppy is locked."));
    }
    Ok(snapshot(status))
}

#[tauri::command]
pub async fn start_history_search(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    input: StartHistorySearchInput,
) -> BridgeResult<HistorySearchSnapshot> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    locked(&state)?;
    let operation_id = input
        .parse_operation_id()
        .map_err(|_| BridgeError::new("invalid-request", "The operation ID is invalid."))?;
    let session = state.require_session_admitted(&operation).await?;
    let client = session.client.clone();
    let status = blocking(move || {
        client
            .start_history_search(operation_id)
            .map_err(core_error)
    })
    .await?;
    let attempt = status.attempt_id.ok_or_else(BridgeError::host_state)?;
    if let Some(previous) = session.history_view(window.label()) {
        if previous != attempt {
            session.detach_history_interest(previous, window.label());
        }
    }
    let (token, new_driver) = session.attach_history_interest(attempt, window.label());
    if new_driver {
        drive_with_token(session.clone(), attempt, token);
    }
    if !current(&state, &session).await || state.app_locked.load(Ordering::Acquire) {
        return Err(BridgeError::new("app-locked", "Peppy is locked."));
    }
    Ok(snapshot(status))
}

#[tauri::command]
pub async fn cancel_history_search(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    input: HistorySearchAttemptInput,
) -> BridgeResult<HistorySearchSnapshot> {
    let operation = admit_update_operation(&state)?;
    require_main(window.label())?;
    locked(&state)?;
    let attempt = input
        .parse_attempt_id()
        .map_err(|_| BridgeError::new("invalid-request", "The attempt ID is invalid."))?;
    let session = state.require_session_admitted(&operation).await?;
    if session.history_view(window.label()) != Some(attempt) {
        return Err(BridgeError::new(
            "not-found",
            "The history search is not owned by this window.",
        ));
    }
    cancel_owned_attempt(&session, window.label(), attempt)
        .await
        .map(snapshot)
}

async fn cancel_owned_attempt(
    session: &Arc<Session>,
    window: &str,
    attempt: uuid::Uuid,
) -> BridgeResult<peppy_client_core::HistorySearchStatus> {
    let client = session.client.clone();
    let status =
        blocking(move || client.cancel_history_search(attempt).map_err(core_error)).await?;
    session.detach_history_interest(attempt, window);
    session.finish_history_attempt(attempt);
    session.notify();
    Ok(status)
}

fn drive_with_token(session: Arc<Session>, attempt: uuid::Uuid, token: CancellationToken) {
    let driver_session = session.clone();
    session.track_worker(tokio::spawn(async move {
        drive_loop(driver_session, attempt, token).await;
    }));
}

/// Called by normal replay/apply after a real observation; it never opens another HTTP loop.
async fn drive_loop(session: Arc<Session>, attempt: uuid::Uuid, token: CancellationToken) {
    let mut consecutive_errors = 0usize;
    loop {
        if token.is_cancelled() || session.cancel.is_cancelled() {
            return;
        }
        let client = session.client.clone();
        let work = blocking(move || client.history_search_work(attempt).map_err(core_error)).await;
        let work = match work {
            Ok(work) => {
                consecutive_errors = 0;
                work
            }
            Err(error) => {
                if handle_driver_error(&session, attempt, "work", &error, &mut consecutive_errors) {
                    return;
                }
                sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let result = drive_step(&session, attempt, work).await;
        if let Err(error) = result {
            if handle_driver_error(&session, attempt, "step", &error, &mut consecutive_errors) {
                return;
            }
        } else {
            consecutive_errors = 0;
        }
        sleep(Duration::from_secs(1)).await;
    }
}

async fn drive_step(
    session: &Arc<Session>,
    attempt: uuid::Uuid,
    work: HistorySearchWork,
) -> BridgeResult<()> {
    match work {
        HistorySearchWork::WakeSync => {
            let client = session.client.clone();
            let context =
                blocking(move || client.history_search_sync_context().map_err(core_error)).await?;
            if session.claim_history_replay_wake(attempt, context.snapshot_generation) {
                session.request_replay();
            }
        }
        HistorySearchWork::AdvanceIndex { limit } => {
            let client = session.client.clone();
            blocking(move || client.advance_search_index(Some(limit)).map_err(core_error)).await?;
            let client = session.client.clone();
            blocking(move || {
                client
                    .observe_history_search_sync(
                        attempt,
                        peppy_client_core::HistorySearchSyncFacts::default(),
                    )
                    .map_err(core_error)
            })
            .await?;
            session.notify();
        }
        HistorySearchWork::RequestPhone { .. } => session.request_work(),
        HistorySearchWork::Wait => {
            let client = session.client.clone();
            let status = blocking(move || {
                client
                    .history_search_status(Some(attempt))
                    .map_err(core_error)
            })
            .await?;
            if is_terminal(&status) || status.attempt_id.is_none() {
                session.finish_history_attempt(attempt);
            }
        }
    }
    Ok(())
}

fn is_terminal(status: &peppy_client_core::HistorySearchStatus) -> bool {
    matches!(
        status.state,
        peppy_client_core::HistorySearchState::Partial
            | peppy_client_core::HistorySearchState::Completed
            | peppy_client_core::HistorySearchState::Blocked
            | peppy_client_core::HistorySearchState::Failed
            | peppy_client_core::HistorySearchState::Cancelled
    )
}

fn handle_driver_error(
    session: &Session,
    attempt: uuid::Uuid,
    phase: &str,
    error: &BridgeError,
    consecutive_errors: &mut usize,
) -> bool {
    eprintln!("peppy: history-search {phase} failed: {}", error.code);
    if error.code == "not-found" {
        session.finish_history_attempt(attempt);
        return true;
    }
    *consecutive_errors += 1;
    if *consecutive_errors >= MAX_CONSECUTIVE_DRIVER_ERRORS {
        session.finish_history_attempt(attempt);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{fixture_at, open};

    fn session() -> (crate::tests::Fixture, Arc<Session>) {
        let fixture = fixture_at("http://127.0.0.1:9");
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        (fixture, session)
    }

    #[tokio::test]
    async fn failed_cancel_keeps_owner_and_driver_for_retry() {
        let (_fixture, session) = session();
        let missing = uuid::Uuid::new_v4();
        let (_, new_driver) = session.attach_history_interest(missing, "main");
        assert!(new_driver);

        let error = cancel_owned_attempt(&session, "main", missing)
            .await
            .unwrap_err();

        assert_eq!(error.code, "not-found");
        assert_eq!(session.history_view("main"), Some(missing));
        assert!(session.history_attempt_token(missing).is_some());
    }

    #[tokio::test]
    async fn successful_cancel_cleans_driver_but_retains_terminal_core_status() {
        let (_fixture, session) = session();
        let started = session
            .client
            .start_history_search(uuid::Uuid::new_v4())
            .unwrap();
        let attempt = started.attempt_id.unwrap();
        session.attach_history_interest(attempt, "main");

        let cancelled = cancel_owned_attempt(&session, "main", attempt)
            .await
            .unwrap();

        assert_eq!(
            cancelled.state,
            peppy_client_core::HistorySearchState::Cancelled
        );
        assert_eq!(session.history_view("main"), None);
        assert!(session.history_attempt_token(attempt).is_none());
        assert_eq!(
            session
                .client
                .history_search_status(Some(attempt))
                .unwrap()
                .state,
            peppy_client_core::HistorySearchState::Cancelled
        );
    }

    #[test]
    fn finished_driver_retains_window_view_and_get_is_read_only() {
        let (_fixture, session) = session();
        let started = session
            .client
            .start_history_search(uuid::Uuid::new_v4())
            .unwrap();
        let attempt = started.attempt_id.unwrap();
        let (_, first_driver) = session.attach_history_interest(attempt, "main");
        let (_, duplicate_driver) = session.attach_history_interest(attempt, "main");
        assert!(first_driver);
        assert!(!duplicate_driver);

        let active = session
            .client
            .history_search_status(session.history_view("main"))
            .unwrap();
        assert_eq!(active, started);
        let terminal = session.client.cancel_history_search(attempt).unwrap();
        session.finish_history_attempt(attempt);
        let first_read = session
            .client
            .history_search_status(session.history_view("main"))
            .unwrap();
        let second_read = session
            .client
            .history_search_status(session.history_view("main"))
            .unwrap();

        assert_eq!(first_read, terminal);
        assert_eq!(second_read, terminal);
        assert_eq!(session.history_view("main"), Some(attempt));
        assert!(session.history_attempt_token(attempt).is_none());
    }

    #[tokio::test]
    async fn cancel_terminal_race_returns_core_terminal_status_and_cleans_owner() {
        let (_fixture, session) = session();
        let started = session
            .client
            .start_history_search(uuid::Uuid::new_v4())
            .unwrap();
        let attempt = started.attempt_id.unwrap();
        session.attach_history_interest(attempt, "main");
        let terminal = session.client.cancel_history_search(attempt).unwrap();
        session.finish_history_attempt(attempt);
        assert_eq!(session.history_view("main"), Some(attempt));

        let observed = cancel_owned_attempt(&session, "main", attempt)
            .await
            .unwrap();

        assert_eq!(observed, terminal);
        assert_eq!(session.history_view("main"), None);
        assert!(session.history_attempt_token(attempt).is_none());
    }

    #[test]
    fn detaching_one_of_multiple_windows_reports_that_window_removal() {
        let (_fixture, session) = session();
        let attempt = uuid::Uuid::new_v4();
        session.attach_history_interest(attempt, "main");
        session.attach_history_interest(attempt, "secondary");

        assert!(session.detach_history_interest(attempt, "main"));
        assert_eq!(session.history_view("main"), None);
        assert_eq!(session.history_view("secondary"), Some(attempt));
        assert!(session.history_attempt_token(attempt).is_some());
    }

    #[tokio::test]
    async fn driver_untracks_a_core_not_found_attempt() {
        let (_fixture, session) = session();
        let missing = uuid::Uuid::new_v4();
        let (token, _) = session.attach_history_interest(missing, "main");

        tokio::time::timeout(
            Duration::from_secs(1),
            drive_loop(session.clone(), missing, token),
        )
        .await
        .unwrap();

        assert!(session.history_attempt_token(missing).is_none());
        assert_eq!(session.history_view("main"), Some(missing));
    }

    #[test]
    fn replay_wake_is_once_per_attempt_and_snapshot_generation() {
        let (_fixture, session) = session();
        let attempt = uuid::Uuid::new_v4();

        assert!(session.claim_history_replay_wake(attempt, 7));
        assert!(!session.claim_history_replay_wake(attempt, 7));
        assert!(session.claim_history_replay_wake(attempt, 8));
        assert!(!session.claim_history_replay_wake(attempt, 8));
    }
}
