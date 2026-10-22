//! Exclusive restart admission control for update preparation.
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    operations: usize,
    reserved: bool,
}

#[derive(Clone, Default)]
pub struct SafetyGate {
    state: Arc<Mutex<State>>,
}

impl SafetyGate {
    pub fn enter(&self) -> Result<OperationGuard, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Restart safety state is unavailable.")?;
        if state.reserved {
            return Err("Peppy is preparing an update restart.");
        }
        state.operations += 1;
        Ok(OperationGuard {
            state: self.state.clone(),
        })
    }

    pub fn reserve(&self) -> Result<RestartGuard, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Restart safety state is unavailable.")?;
        if state.reserved || state.operations != 0 {
            return Err("Peppy is busy and cannot safely restart yet.");
        }
        state.reserved = true;
        Ok(RestartGuard {
            state: self.state.clone(),
        })
    }
}

#[must_use = "operation admission must remain held until the mutation finishes"]
pub struct OperationGuard {
    state: Arc<Mutex<State>>,
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.operations = state.operations.saturating_sub(1);
        }
    }
}

#[must_use = "restart reservation must remain held until installation commits or aborts"]
pub struct RestartGuard {
    state: Arc<Mutex<State>>,
}

impl Drop for RestartGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.reserved = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservation_rejects_active_operations_and_new_admission() {
        let gate = SafetyGate::default();
        let operation = gate.enter().unwrap();
        assert!(gate.reserve().is_err());
        drop(operation);

        let restart = gate.reserve().unwrap();
        assert!(gate.enter().is_err());
        drop(restart);
        assert!(gate.enter().is_ok());
    }

    #[test]
    fn dropping_guards_releases_their_admission() {
        let gate = SafetyGate::default();
        drop(gate.enter().unwrap());
        let restart = gate.reserve().unwrap();
        drop(restart);
        assert!(gate.reserve().is_ok());
    }

    #[tokio::test]
    async fn operation_held_across_blocking_work_blocks_restart_until_completion() {
        let gate = SafetyGate::default();
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        let worker_gate = gate.clone();
        let worker = tokio::spawn(async move {
            let _operation = worker_gate.enter().unwrap();
            tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                let _ = release_rx.blocking_recv();
            })
            .await
            .unwrap();
        });

        started_rx.await.unwrap();
        assert!(gate.reserve().is_err());
        release.send(()).unwrap();
        worker.await.unwrap();
        let restart = gate.reserve().unwrap();
        drop(restart);
    }
}
