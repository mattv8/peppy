//! Exclusive restart admission control for update preparation.
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

// A restart waits long enough for an in-progress snapshot or command to finish, but remains
// bounded so callers can report the existing retryable busy state instead of waiting forever.
const RESERVATION_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Default)]
struct State {
    operations: usize,
    reserved: bool,
}

#[derive(Clone, Default)]
pub struct SafetyGate {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    drained: tokio::sync::Notify,
}

impl SafetyGate {
    pub fn enter(&self) -> Result<OperationGuard, &'static str> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| "Restart safety state is unavailable.")?;
        if state.reserved {
            return Err("Peppy is preparing an update restart.");
        }
        state.operations += 1;
        Ok(OperationGuard {
            inner: self.inner.clone(),
        })
    }

    pub async fn reserve_async(&self) -> Result<RestartGuard, &'static str> {
        self.reserve_with_timeout(RESERVATION_TIMEOUT).await
    }

    pub fn reserve_now(&self) -> Result<RestartGuard, &'static str> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| "Restart safety state is unavailable.")?;
        if state.reserved || state.operations != 0 {
            return Err("Peppy is busy and cannot safely restart yet.");
        }
        state.reserved = true;
        Ok(RestartGuard {
            inner: self.inner.clone(),
        })
    }

    async fn reserve_with_timeout(&self, timeout: Duration) -> Result<RestartGuard, &'static str> {
        let restart = self.reserve_admission()?;
        match tokio::time::timeout(timeout, self.wait_for_operations()).await {
            Ok(Ok(())) => Ok(restart),
            Ok(Err(error)) => {
                drop(restart);
                Err(error)
            }
            Err(_) => {
                drop(restart);
                Err("Peppy is busy and cannot safely restart yet.")
            }
        }
    }

    fn reserve_admission(&self) -> Result<RestartGuard, &'static str> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| "Restart safety state is unavailable.")?;
        if state.reserved {
            return Err("Peppy is busy and cannot safely restart yet.");
        }
        state.reserved = true;
        Ok(RestartGuard {
            inner: self.inner.clone(),
        })
    }

    async fn wait_for_operations(&self) -> Result<(), &'static str> {
        loop {
            let notified = self.inner.drained.notified();
            let operations = self
                .inner
                .state
                .lock()
                .map(|state| state.operations)
                .map_err(|_| "Restart safety state is unavailable.")?;
            if operations == 0 {
                return Ok(());
            }
            notified.await;
        }
    }
}

#[must_use = "operation admission must remain held until the mutation finishes"]
pub struct OperationGuard {
    inner: Arc<Inner>,
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.operations = state.operations.saturating_sub(1);
            if state.operations == 0 {
                self.inner.drained.notify_waiters();
            }
        }
    }
}

#[must_use = "restart reservation must remain held until installation commits or aborts"]
pub struct RestartGuard {
    inner: Arc<Inner>,
}

impl Drop for RestartGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.reserved = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reservation_waits_for_an_in_flight_operation_then_succeeds() {
        let gate = SafetyGate::default();
        let operation = gate.enter().unwrap();
        let reservation_gate = gate.clone();
        let reservation = tokio::spawn(async move { reservation_gate.reserve_async().await });

        tokio::task::yield_now().await;
        assert!(gate.enter().is_err());
        drop(operation);

        let restart = reservation.await.unwrap().unwrap();
        drop(restart);
        assert!(gate.enter().is_ok());
    }

    #[tokio::test]
    async fn timeout_releases_reservation_for_new_operations() {
        let gate = SafetyGate::default();
        let operation = gate.enter().unwrap();

        assert!(gate
            .reserve_with_timeout(std::time::Duration::ZERO)
            .await
            .is_err());
        assert!(gate.enter().is_ok());
        drop(operation);
    }

    #[test]
    fn immediate_reservation_does_not_wait_for_active_operations() {
        let gate = SafetyGate::default();
        let operation = gate.enter().unwrap();

        assert!(gate.reserve_now().is_err());
        assert!(gate.enter().is_ok());
        drop(operation);
    }

    #[test]
    fn immediate_reservation_keeps_admission_closed_until_dropped() {
        let gate = SafetyGate::default();
        let restart = gate.reserve_now().unwrap();

        assert!(gate.enter().is_err());
        drop(restart);
        assert!(gate.enter().is_ok());
    }

    #[tokio::test]
    async fn reservation_closes_admission_to_a_stream_of_short_operations() {
        let gate = SafetyGate::default();
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let worker_gate = gate.clone();
        let worker = tokio::spawn(async move {
            let mut started = Some(started);
            while let Ok(operation) = worker_gate.enter() {
                if let Some(started) = started.take() {
                    started.send(()).unwrap();
                }
                tokio::task::yield_now().await;
                drop(operation);
            }
        });

        started_rx.await.unwrap();
        let restart = gate.reserve_async().await.unwrap();
        worker.await.unwrap();
        drop(restart);
    }

    #[tokio::test]
    async fn cancelling_a_waiting_reservation_reopens_admission() {
        let gate = SafetyGate::default();
        let operation = gate.enter().unwrap();
        let reservation_gate = gate.clone();
        let reservation = tokio::spawn(async move { reservation_gate.reserve_async().await });

        tokio::task::yield_now().await;
        reservation.abort();
        let _ = reservation.await;
        assert!(gate.enter().is_ok());
        drop(operation);
    }

    #[tokio::test]
    async fn cross_thread_operation_drop_wakes_reservation() {
        let gate = SafetyGate::default();
        let operation = gate.enter().unwrap();
        let reservation_gate = gate.clone();
        let reservation = tokio::spawn(async move { reservation_gate.reserve_async().await });

        std::thread::spawn(move || drop(operation)).join().unwrap();
        drop(reservation.await.unwrap().unwrap());
    }
}
