use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryState {
    Ready,
    Claimed,
    Uncertain,
}

#[derive(Clone)]
pub(crate) struct RecoveryItem {
    pub id: String,
    pub session_generation: String,
    pub conversation_id: String,
    pub body: String,
    pub reason: String,
    pub state: RecoveryState,
}

pub(crate) struct RecoveryReservation {
    pool: Arc<AtomicUsize>,
}

impl Drop for RecoveryReservation {
    fn drop(&mut self) {
        self.pool.fetch_sub(1, Ordering::AcqRel);
    }
}

struct Entry {
    item: RecoveryItem,
    restore_id: Option<String>,
    _reservation: Arc<RecoveryReservation>,
}

struct CompletedAck {
    session_generation: String,
    id: String,
    restore_id: String,
}

struct StoreState {
    pool: Arc<AtomicUsize>,
    entries: Vec<Entry>,
    completed_acks: Vec<CompletedAck>,
}

/// Reservations outlive popup handles, so notification churn cannot evict an authored reply.
pub(crate) struct RecoveryStore {
    capacity: usize,
    state: Mutex<StoreState>,
}

impl Default for RecoveryStore {
    fn default() -> Self {
        Self::new(256)
    }
}

impl RecoveryStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(StoreState {
                pool: Arc::new(AtomicUsize::new(0)),
                entries: Vec::new(),
                completed_acks: Vec::new(),
            }),
        }
    }

    pub fn reserve(&self) -> Option<Arc<RecoveryReservation>> {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state
            .pool
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < self.capacity).then_some(count + 1)
            })
            .ok()?;
        Some(Arc::new(RecoveryReservation {
            pool: state.pool.clone(),
        }))
    }

    pub fn record(&self, reservation: Arc<RecoveryReservation>, item: RecoveryItem) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if !Arc::ptr_eq(&state.pool, &reservation.pool)
            || item.body.len() > 64 * 1024
            || state.entries.len() >= self.capacity
            || state.entries.iter().any(|entry| entry.item.id == item.id)
        {
            return false;
        }
        state.entries.push(Entry {
            item,
            restore_id: None,
            _reservation: reservation,
        });
        true
    }

    pub fn list(&self, session: &str, conversation: &str) -> Vec<RecoveryItem> {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entries
            .iter()
            .filter(|entry| {
                entry.item.session_generation == session
                    && entry.item.conversation_id == conversation
            })
            .map(|entry| entry.item.clone())
            .collect()
    }

    pub fn claim(&self, session: &str, id: &str, restore_id: &str) -> bool {
        if restore_id.is_empty() || restore_id.len() > 128 {
            return false;
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = state
            .entries
            .iter_mut()
            .find(|entry| entry.item.id == id && entry.item.session_generation == session)
        else {
            return false;
        };
        if entry.item.state != RecoveryState::Ready {
            return false;
        }
        entry.item.state = RecoveryState::Claimed;
        entry.restore_id = Some(restore_id.to_owned());
        true
    }

    /// Only a definite refusal before any draft edit may release this exact claim.
    pub fn release(&self, session: &str, id: &str, restore_id: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = state.entries.iter_mut().find(|entry| {
            entry.item.id == id
                && entry.item.session_generation == session
                && entry.restore_id.as_deref() == Some(restore_id)
        }) else {
            return false;
        };
        if entry.item.state == RecoveryState::Uncertain {
            return false;
        }
        entry.item.state = RecoveryState::Ready;
        // Retain the exact ID for a lost-response retry; a newer claim replaces it.
        true
    }

    pub fn ack(&self, session: &str, id: &str, restore_id: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(index) = state.entries.iter().position(|entry| entry.item.id == id) else {
            return state.completed_acks.iter().any(|ack| {
                ack.session_generation == session && ack.id == id && ack.restore_id == restore_id
            });
        };
        let entry = &state.entries[index];
        if entry.item.session_generation != session
            || entry.restore_id.as_deref() != Some(restore_id)
            || entry.item.state != RecoveryState::Claimed
        {
            return false;
        }
        state.entries.remove(index);
        state.completed_acks.push(CompletedAck {
            session_generation: session.to_owned(),
            id: id.to_owned(),
            restore_id: restore_id.to_owned(),
        });
        // Ack retries are best-effort. Keep only one owner's worth of exact tombstones,
        // so a long-running session cannot retain unbounded restore identifiers.
        let excess = state.completed_acks.len().saturating_sub(self.capacity);
        if excess > 0 {
            state.completed_acks.drain(..excess);
        }
        true
    }

    pub fn discard(&self, session: &str, id: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(index) = state
            .entries
            .iter()
            .position(|entry| entry.item.id == id && entry.item.session_generation == session)
        else {
            return false;
        };
        state.entries.remove(index);
        true
    }

    pub fn clear(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.entries.clear();
        state.completed_acks.clear();
        // Late work holding an old reservation can no longer populate this store.
        state.pool = Arc::new(AtomicUsize::new(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(store: &RecoveryStore, lease: Arc<RecoveryReservation>, id: &str) {
        assert!(store.record(
            lease,
            RecoveryItem {
                id: id.into(),
                session_generation: "session".into(),
                conversation_id: "conversation".into(),
                body: "reply".into(),
                reason: "needs-route-choice".into(),
                state: RecoveryState::Ready,
            }
        ));
    }

    #[test]
    fn reservation_transfers_to_recovery_without_releasing_capacity() {
        let store = RecoveryStore::new(1);
        let lease = store.reserve().unwrap();
        assert!(store.reserve().is_none());
        record(&store, lease.clone(), "reply");
        drop(lease);
        assert!(store.reserve().is_none());
        assert!(store.discard("session", "reply"));
        assert!(store.reserve().is_some());
    }

    #[test]
    fn claim_is_one_use_and_ack_retry_never_authorizes_another_edit() {
        let store = RecoveryStore::new(1);
        record(&store, store.reserve().unwrap(), "reply");
        assert!(store.claim("session", "reply", "restore"));
        assert!(!store.claim("session", "reply", "restore"));
        assert!(!store.claim("session", "reply", "different"));
        assert_eq!(
            store.list("session", "conversation")[0].state,
            RecoveryState::Claimed
        );
        assert!(!store.ack("session", "reply", "different"));
        assert!(store.ack("session", "reply", "restore"));
        assert!(store.ack("session", "reply", "restore"));
        assert!(store.list("session", "conversation").is_empty());
    }

    #[test]
    fn pre_edit_release_is_exact_and_cannot_release_a_newer_claim() {
        let store = RecoveryStore::new(1);
        record(&store, store.reserve().unwrap(), "reply");
        assert!(store.claim("session", "reply", "first"));
        assert!(!store.release("other", "reply", "first"));
        assert!(!store.release("session", "reply", "wrong"));
        assert!(store.release("session", "reply", "first"));
        assert!(store.release("session", "reply", "first"));
        assert!(!store.ack("session", "reply", "first"));
        assert_eq!(
            store.list("session", "conversation")[0].state,
            RecoveryState::Ready
        );
        assert!(store.claim("session", "reply", "second"));
        assert!(!store.release("session", "reply", "first"));
        assert_eq!(
            store.list("session", "conversation")[0].state,
            RecoveryState::Claimed
        );
        assert!(store.ack("session", "reply", "second"));
        assert!(!store.release("session", "reply", "second"));
    }

    #[test]
    fn retirement_rejects_old_reservations_and_releases_new_capacity() {
        let store = RecoveryStore::new(1);
        let stale = store.reserve().unwrap();
        store.clear();
        assert!(store.reserve().is_some());
        assert!(!store.record(
            stale,
            RecoveryItem {
                id: "late".into(),
                session_generation: "session".into(),
                conversation_id: "conversation".into(),
                body: "private".into(),
                reason: "late".into(),
                state: RecoveryState::Ready,
            }
        ));
        assert!(store.list("session", "conversation").is_empty());
    }

    #[test]
    fn wrong_session_and_uncertain_replies_cannot_restore() {
        let store = RecoveryStore::new(2);
        record(&store, store.reserve().unwrap(), "ready");
        assert!(store.list("other", "conversation").is_empty());
        assert!(!store.claim("other", "ready", "restore"));
        assert!(!store.discard("other", "ready"));
        assert!(store.record(
            store.reserve().unwrap(),
            RecoveryItem {
                id: "uncertain".into(),
                session_generation: "session".into(),
                conversation_id: "conversation".into(),
                body: "reply".into(),
                reason: "uncertain".into(),
                state: RecoveryState::Uncertain,
            }
        ));
        assert!(!store.claim("session", "uncertain", "restore"));
        assert_eq!(store.list("session", "conversation").len(), 2);
    }

    #[test]
    fn acknowledgement_only_retries_the_exact_completed_restore() {
        let store = RecoveryStore::new(1);
        record(&store, store.reserve().unwrap(), "reply");
        assert!(store.claim("session", "reply", "restore"));
        assert!(store.ack("session", "reply", "restore"));
        assert!(store.ack("session", "reply", "restore"));
        assert!(!store.ack("session", "reply", "different"));
        assert!(!store.ack("other", "reply", "restore"));
        assert!(!store.ack("session", "unknown", "restore"));
    }

    #[test]
    fn completed_ack_tombstones_are_bounded_by_recovery_capacity() {
        let store = RecoveryStore::new(2);
        for id in ["first", "second", "third"] {
            record(&store, store.reserve().unwrap(), id);
            assert!(store.claim("session", id, "restore"));
            assert!(store.ack("session", id, "restore"));
        }
        assert_eq!(store.state.lock().unwrap().completed_acks.len(), 2);
        assert!(!store.ack("session", "first", "restore"));
        assert!(store.ack("session", "third", "restore"));
    }
}
