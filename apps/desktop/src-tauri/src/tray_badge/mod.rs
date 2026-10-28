//! Native tray unread badge state. Rendering stays platform-specific; this
//! module owns the authoritative state query and its lifecycle fences.
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Listener, Manager};

use crate::{
    error::{core_error, BridgeError, BridgeResult},
    session::{encryption_state, EncryptionState, Session},
    AppState, STATE_EVENT,
};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "windows", test))]
mod windows;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayBadge {
    Hidden,
    Unread(u64),
    Locked,
}

impl TrayBadge {
    pub fn label(&self) -> Option<String> {
        match self {
            Self::Hidden => None,
            Self::Unread(total @ 1..=99) => Some(total.to_string()),
            Self::Unread(_) => Some("99+".into()),
            Self::Locked => Some("lock".into()),
        }
    }

    pub fn tier(&self) -> u8 {
        match self {
            Self::Hidden => 0,
            Self::Unread(1..=9) => 1,
            Self::Unread(10..=99) => 2,
            Self::Unread(_) => 3,
            Self::Locked => 4,
        }
    }

    pub fn tooltip(&self) -> String {
        match self {
            Self::Hidden => "Peppy".into(),
            Self::Unread(1) => "Peppy – 1 unread message".into(),
            Self::Unread(total) => format!("Peppy – {total} unread messages"),
            Self::Locked => "Peppy – Locked".into(),
        }
    }
}

#[derive(Default)]
struct RefreshState {
    refreshing: bool,
    queued: bool,
    dirty: bool,
    applied: Option<TrayBadge>,
}

impl RefreshState {
    fn request(&mut self) -> bool {
        if self.refreshing {
            self.queued = true;
            false
        } else {
            self.refreshing = true;
            true
        }
    }

    fn finish(&mut self) -> bool {
        if self.queued {
            self.queued = false;
            true
        } else {
            self.refreshing = false;
            false
        }
    }

    fn needs_apply(&self, badge: &TrayBadge) -> bool {
        self.dirty || self.applied.as_ref() != Some(badge)
    }
}

struct Runtime {
    refresh: Mutex<RefreshState>,
}

#[derive(Clone)]
struct Candidate {
    badge: TrayBadge,
    session: Option<Arc<Session>>,
    revoked: bool,
    cancelled: bool,
    mismatch: bool,
    vault_epoch: Option<u32>,
}

#[derive(Clone, Copy)]
struct ApplyFence {
    app_locked: bool,
    session_current: bool,
    cancelled: bool,
    revoked_matches: bool,
    mismatch_matches: bool,
    vault_epoch_matches: bool,
}

enum RefreshOutcome {
    Applied,
    Stale,
    Busy,
    Failed,
}

enum ReadOutcome {
    Candidate(Candidate),
    Busy,
}

const BUSY_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
const MAX_BUSY_RETRIES: u8 = 3;

fn runtime(app: &AppHandle) -> tauri::State<'_, Runtime> {
    app.state::<Runtime>()
}

/// Installs the state-event listener and performs an initial refresh after the tray exists.
pub fn install(app: &AppHandle) {
    if !cfg!(any(target_os = "macos", target_os = "windows"))
        || app.try_state::<Runtime>().is_some()
    {
        return;
    }
    app.manage(Runtime {
        refresh: Mutex::new(RefreshState::default()),
    });
    let handle = app.clone();
    app.listen(STATE_EVENT, move |_| schedule_refresh(&handle));
    schedule_refresh(app);
}

fn schedule_refresh(app: &AppHandle) {
    let start = runtime(app)
        .refresh
        .lock()
        .map(|mut refresh| refresh.request())
        .unwrap_or(false);
    if !start {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            refresh_with_busy_retries(|| refresh(&app)).await;
            let again = runtime(&app)
                .refresh
                .lock()
                .map(|mut refresh| refresh.finish())
                .unwrap_or(false);
            if !again {
                break;
            }
        }
    });
}

fn should_retry_busy(retries: u8) -> bool {
    retries < MAX_BUSY_RETRIES
}

async fn refresh_with_busy_retries<F, Fut>(mut attempt: F) -> RefreshOutcome
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = RefreshOutcome>,
{
    let mut retries = 0;
    loop {
        let outcome = attempt().await;
        if !matches!(outcome, RefreshOutcome::Busy) || !should_retry_busy(retries) {
            return outcome;
        }
        retries += 1;
        tokio::time::sleep(BUSY_RETRY_DELAY).await;
    }
}

async fn refresh(app: &AppHandle) -> RefreshOutcome {
    let state = app.state::<AppState>();
    let candidate = match badge_for_state(&state).await {
        ReadOutcome::Candidate(candidate) => candidate,
        ReadOutcome::Busy => {
            mark_dirty(app);
            return RefreshOutcome::Busy;
        }
    };
    let needs_apply = runtime(app)
        .refresh
        .lock()
        .map(|refresh| refresh.needs_apply(&candidate.badge))
        .unwrap_or(false);
    if !needs_apply {
        return RefreshOutcome::Applied;
    }
    match apply_on_main(app, candidate.clone()).await {
        RefreshOutcome::Applied => {
            if let Ok(mut refresh) = runtime(app).refresh.lock() {
                refresh.applied = Some(candidate.badge);
                refresh.dirty = false;
            }
            RefreshOutcome::Applied
        }
        outcome => {
            mark_dirty(app);
            outcome
        }
    }
}

fn mark_dirty(app: &AppHandle) {
    if let Ok(mut refresh) = runtime(app).refresh.lock() {
        refresh.dirty = true;
    }
}

async fn badge_for_state(state: &AppState) -> ReadOutcome {
    if state.app_locked.load(std::sync::atomic::Ordering::Acquire) {
        return ReadOutcome::Candidate(Candidate {
            badge: TrayBadge::Locked,
            session: None,
            revoked: false,
            cancelled: false,
            mismatch: false,
            vault_epoch: None,
        });
    }
    let session = match observe_slot(&state.session, |session| session.clone()) {
        Ok(session) => session,
        Err(()) => return ReadOutcome::Busy,
    };
    let Some(session) = session else {
        return ReadOutcome::Candidate(Candidate {
            badge: TrayBadge::Hidden,
            session: None,
            revoked: false,
            cancelled: false,
            mismatch: false,
            vault_epoch: None,
        });
    };
    match crate::sync::blocking({
        let session = session.clone();
        move || candidate_for_session(session)
    })
    .await
    {
        Ok(candidate) => ReadOutcome::Candidate(candidate),
        Err(_) => ReadOutcome::Busy,
    }
}

fn candidate_for_session(session: Arc<Session>) -> BridgeResult<Candidate> {
    let keys = session.client.key_status().map_err(core_error)?;
    let status = session
        .status
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    let revoked = status.revoked;
    let cancelled = session.cancel.is_cancelled();
    let mismatch = session.mismatch.load(std::sync::atomic::Ordering::Acquire);
    let vault = status.vault.clone();
    let vault_epoch = vault.as_ref().map(|vault| vault.epoch);
    drop(status);
    let (encryption, _) = encryption_state(mismatch, vault.as_ref(), &keys);
    let badge = match classify_session_security(keys.active_epoch, revoked, cancelled, &encryption)
    {
        TrayBadge::Unread(_) => {
            let total = session
                .client
                .list_conversations()
                .map_err(core_error)
                .map(|conversations| {
                    unread_total(conversations.into_iter().map(|item| item.unread_count))
                })?;
            if total == 0 {
                TrayBadge::Hidden
            } else {
                TrayBadge::Unread(total)
            }
        }
        badge => badge,
    };
    Ok(Candidate {
        badge,
        session: Some(session),
        revoked,
        cancelled,
        mismatch,
        vault_epoch,
    })
}

fn unread_total(counts: impl IntoIterator<Item = u64>) -> u64 {
    counts
        .into_iter()
        .fold(0_u64, |total, count| total.saturating_add(count))
}

fn classify_session_security(
    active_epoch: Option<u32>,
    revoked: bool,
    cancelled: bool,
    encryption: &EncryptionState,
) -> TrayBadge {
    if revoked || cancelled || active_epoch.is_none() {
        TrayBadge::Hidden
    } else if matches!(encryption, EncryptionState::Unlocked { .. }) {
        TrayBadge::Unread(1)
    } else {
        TrayBadge::Locked
    }
}

async fn apply_on_main(app: &AppHandle, candidate: Candidate) -> RefreshOutcome {
    let (send, receive) = tokio::sync::oneshot::channel();
    let main_app = app.clone();
    let dispatched = app.run_on_main_thread(move || {
        let result = match current_fence(&main_app, &candidate) {
            Ok(fence) if apply_fence_accepts(&candidate, fence) => {
                if crate::tray::icon(&main_app)
                    .is_some_and(|tray| apply(&tray, &candidate.badge).is_ok())
                {
                    RefreshOutcome::Applied
                } else {
                    RefreshOutcome::Failed
                }
            }
            Ok(_) => RefreshOutcome::Stale,
            Err(()) => RefreshOutcome::Busy,
        };
        let _ = send.send(result);
    });
    if dispatched.is_err() {
        return RefreshOutcome::Failed;
    }
    receive.await.unwrap_or(RefreshOutcome::Failed)
}

fn current_fence(app: &AppHandle, candidate: &Candidate) -> Result<ApplyFence, ()> {
    let state = app.state::<AppState>();
    let app_locked = state.app_locked.load(std::sync::atomic::Ordering::Acquire);
    if app_locked && candidate.badge == TrayBadge::Locked {
        return Ok(ApplyFence {
            app_locked,
            session_current: false,
            cancelled: false,
            revoked_matches: true,
            mismatch_matches: true,
            vault_epoch_matches: true,
        });
    }
    let current = observe_slot(&state.session, |session| session.clone())?;
    let Some(expected) = candidate.session.as_ref() else {
        return Ok(ApplyFence {
            app_locked,
            session_current: current.is_none(),
            cancelled: false,
            revoked_matches: true,
            mismatch_matches: true,
            vault_epoch_matches: true,
        });
    };
    if !same_session(Some(expected), current.as_ref()) {
        return Ok(ApplyFence {
            app_locked,
            session_current: false,
            cancelled: true,
            revoked_matches: false,
            mismatch_matches: false,
            vault_epoch_matches: false,
        });
    }
    let session = current.as_ref().expect("matched session is present");
    let (revoked, vault_epoch) = observe_lock(&session.status, |status| {
        (
            status.revoked,
            status.vault.as_ref().map(|vault| vault.epoch),
        )
    })?;
    Ok(ApplyFence {
        app_locked,
        session_current: true,
        cancelled: session.cancel.is_cancelled(),
        revoked_matches: revoked == candidate.revoked,
        mismatch_matches: session.mismatch.load(std::sync::atomic::Ordering::Acquire)
            == candidate.mismatch,
        vault_epoch_matches: vault_epoch == candidate.vault_epoch,
    })
}

fn observe_slot<T, R>(slot: &tokio::sync::Mutex<T>, read: impl FnOnce(&T) -> R) -> Result<R, ()> {
    slot.try_lock().map(|value| read(&value)).map_err(|_| ())
}

fn observe_lock<T, R>(lock: &Mutex<T>, read: impl FnOnce(&T) -> R) -> Result<R, ()> {
    lock.try_lock().map(|value| read(&value)).map_err(|_| ())
}

fn same_session<T>(expected: Option<&Arc<T>>, current: Option<&Arc<T>>) -> bool {
    matches!((expected, current), (Some(expected), Some(current)) if Arc::ptr_eq(expected, current))
}

fn apply_fence_accepts(candidate: &Candidate, fence: ApplyFence) -> bool {
    apply_fence_accepts_parts(
        &candidate.badge,
        candidate.session.is_some(),
        candidate.cancelled,
        fence,
    )
}

fn apply_fence_accepts_parts(
    badge: &TrayBadge,
    session_expected: bool,
    cancelled: bool,
    fence: ApplyFence,
) -> bool {
    if fence.app_locked {
        return *badge == TrayBadge::Locked;
    }
    if !session_expected {
        return fence.session_current && *badge == TrayBadge::Hidden;
    }
    fence.session_current
        && fence.cancelled == cancelled
        && fence.revoked_matches
        && fence.mismatch_matches
        && fence.vault_epoch_matches
}

fn apply(tray: &tauri::tray::TrayIcon<tauri::Wry>, badge: &TrayBadge) -> tauri::Result<()> {
    #[cfg(target_os = "macos")]
    macos::apply(tray, badge)?;
    #[cfg(target_os = "windows")]
    windows::apply(tray, badge)?;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let _ = (tray, badge);
    tray.set_tooltip(Some(badge.tooltip()))
}

#[cfg(test)]
mod tests {
    use super::{
        apply_fence_accepts_parts, classify_session_security, observe_lock, observe_slot,
        same_session, should_retry_busy, unread_total, ApplyFence, RefreshState, TrayBadge,
        MAX_BUSY_RETRIES,
    };
    use crate::session::{encryption_state, EncryptionState, VaultSummary};
    use peppy_client_core::KeyStatus;
    use std::sync::Arc;
    use std::sync::Mutex;

    #[test]
    fn badge_boundaries_and_tooltips_are_total() {
        assert_eq!(TrayBadge::Hidden.label(), None);
        assert_eq!(TrayBadge::Hidden.tier(), 0);
        assert_eq!(unread_total([u64::MAX, 1]), u64::MAX);
        for (total, label, tier) in [
            (1, Some("1"), 1),
            (9, Some("9"), 1),
            (10, Some("10"), 2),
            (99, Some("99"), 2),
            (100, Some("99+"), 3),
            (u64::MAX, Some("99+"), 3),
        ] {
            let badge = TrayBadge::Unread(total);
            assert_eq!(badge.label().as_deref(), label);
            assert_eq!(badge.tier(), tier);
            let expected_tooltip = if total == 1 {
                "Peppy – 1 unread message".into()
            } else {
                format!("Peppy – {total} unread messages")
            };
            assert_eq!(badge.tooltip(), expected_tooltip);
        }
        assert_eq!(TrayBadge::Hidden.tooltip(), "Peppy");
        assert_eq!(TrayBadge::Locked.tooltip(), "Peppy – Locked");
    }

    #[test]
    fn application_fence_rejects_a_stale_session_or_lock_transition() {
        let queried_session = Arc::new(());
        let replacement_session = Arc::new(());
        assert!(same_session(Some(&queried_session), Some(&queried_session)));
        assert!(!same_session(
            Some(&queried_session),
            Some(&replacement_session)
        ));
        let current = ApplyFence {
            app_locked: false,
            session_current: true,
            cancelled: false,
            revoked_matches: true,
            mismatch_matches: true,
            vault_epoch_matches: true,
        };
        assert!(apply_fence_accepts_parts(
            &TrayBadge::Unread(4),
            true,
            false,
            current,
        ));
        assert!(!apply_fence_accepts_parts(
            &TrayBadge::Unread(4),
            true,
            false,
            ApplyFence {
                session_current: false,
                ..current
            },
        ));
        assert!(!apply_fence_accepts_parts(
            &TrayBadge::Unread(4),
            true,
            false,
            ApplyFence {
                app_locked: true,
                ..current
            },
        ));
        assert!(apply_fence_accepts_parts(
            &TrayBadge::Locked,
            true,
            false,
            ApplyFence {
                app_locked: true,
                ..current
            },
        ));
        assert!(apply_fence_accepts_parts(
            &TrayBadge::Hidden,
            false,
            false,
            ApplyFence {
                session_current: true,
                ..current
            },
        ));
        assert!(!apply_fence_accepts_parts(
            &TrayBadge::Hidden,
            false,
            false,
            ApplyFence {
                session_current: false,
                ..current
            },
        ));
        assert!(!apply_fence_accepts_parts(
            &TrayBadge::Hidden,
            false,
            false,
            ApplyFence {
                session_current: false,
                cancelled: true,
                revoked_matches: false,
                mismatch_matches: false,
                vault_epoch_matches: false,
                ..current
            },
        ));
        assert!(apply_fence_accepts_parts(
            &TrayBadge::Locked,
            false,
            false,
            ApplyFence {
                app_locked: true,
                session_current: false,
                cancelled: true,
                revoked_matches: false,
                mismatch_matches: false,
                vault_epoch_matches: false,
            },
        ));
    }

    #[test]
    fn security_classification_hides_unavailable_sessions_and_locks_key_problems() {
        let unlocked = EncryptionState::Unlocked {
            profile_fingerprint: None,
        };
        assert_eq!(
            classify_session_security(None, false, false, &unlocked),
            TrayBadge::Hidden
        );
        assert_eq!(
            classify_session_security(Some(4), true, false, &unlocked),
            TrayBadge::Hidden
        );
        assert_eq!(
            classify_session_security(Some(4), false, false, &EncryptionState::Locked),
            TrayBadge::Locked
        );
        assert_eq!(
            classify_session_security(Some(4), false, false, &EncryptionState::Mismatch),
            TrayBadge::Locked
        );
        let keys = KeyStatus {
            active_epoch: Some(4),
            unlocked_epochs: vec![4],
        };
        let (server_ahead, _) = encryption_state(
            false,
            Some(&VaultSummary {
                epoch: 5,
                fingerprint: "next".into(),
            }),
            &keys,
        );
        assert_eq!(
            classify_session_security(keys.active_epoch, false, false, &server_ahead),
            TrayBadge::Locked
        );
    }

    #[test]
    fn refresh_requests_coalesce_and_failed_apply_waits_for_next_event() {
        let mut refresh = RefreshState::default();
        assert!(refresh.request());
        assert!(!refresh.request());
        assert!(refresh.finish());
        assert!(!refresh.finish());
        assert!(!refresh.refreshing);

        refresh.dirty = true;
        refresh.applied = Some(TrayBadge::Unread(4));
        assert!(refresh.needs_apply(&TrayBadge::Unread(4)));
        assert!(refresh.request());
        refresh.dirty = false;
        assert!(!refresh.needs_apply(&TrayBadge::Unread(4)));
    }

    #[test]
    fn contention_observation_retries_then_stops_without_an_event() {
        let slot = tokio::sync::Mutex::new(Some(Arc::new(())));
        let slot_guard = slot.try_lock().unwrap();
        assert!(observe_slot(&slot, |value| value.clone()).is_err());
        drop(slot_guard);
        assert!(observe_slot(&slot, |value| value.clone())
            .unwrap()
            .is_some());

        let lock = Mutex::new(());
        let lock_guard = lock.lock().unwrap();
        assert!(observe_lock(&lock, |_| ()).is_err());
        drop(lock_guard);
        assert!(observe_lock(&lock, |_| ()).is_ok());

        assert!(should_retry_busy(0));
        assert!(should_retry_busy(MAX_BUSY_RETRIES - 1));
        assert!(!should_retry_busy(MAX_BUSY_RETRIES));
    }

    #[tokio::test]
    async fn busy_refresh_recovers_without_another_state_event() {
        let slot = tokio::sync::Mutex::new(None::<Arc<()>>);
        let mut held = Some(slot.try_lock().unwrap());
        let mut attempts = 0;
        let result = super::refresh_with_busy_retries(|| {
            attempts += 1;
            let observed = observe_slot(&slot, |session| session.is_none());
            drop(held.take());
            std::future::ready(match observed {
                Ok(true) => super::RefreshOutcome::Applied,
                Ok(false) => super::RefreshOutcome::Stale,
                Err(()) => super::RefreshOutcome::Busy,
            })
        })
        .await;
        assert!(matches!(result, super::RefreshOutcome::Applied));
        assert_eq!(attempts, 2);
    }

    #[tokio::test]
    async fn persistent_busy_is_bounded_and_native_failure_is_not_retried() {
        let slot = tokio::sync::Mutex::new(None::<Arc<()>>);
        let _held = slot.try_lock().unwrap();
        let mut attempts = 0;
        let result = super::refresh_with_busy_retries(|| {
            attempts += 1;
            assert!(observe_slot(&slot, |session| session.clone()).is_err());
            std::future::ready(super::RefreshOutcome::Busy)
        })
        .await;
        assert!(matches!(result, super::RefreshOutcome::Busy));
        assert_eq!(attempts, usize::from(MAX_BUSY_RETRIES) + 1);

        let mut failed_attempts = 0;
        super::refresh_with_busy_retries(|| {
            failed_attempts += 1;
            std::future::ready(super::RefreshOutcome::Failed)
        })
        .await;
        assert_eq!(failed_attempts, 1);
    }
}
