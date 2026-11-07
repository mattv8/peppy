//! Bounded ownership for ticket database work.
//!
//! This module deliberately does not decide ticket state.  Callers supply one
//! operation-wide [`OperationDeadline`] (five seconds for socket renewals or
//! sixty seconds for HTTP work), claim the device before their first authority
//! mutation, and use [`ExecutionScope::stage`] before every authority statement.
//! A deadline or cancellation after a statement has been sent is indeterminate:
//! the child does not run another statement or report success, and hard-closes
//! the checked-out SQLx connection before relinquishing its permits.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_util::FutureExt;
use sqlx::{Connection, PgConnection, PgPool, Postgres, pool::PoolConnection};
use tokio::{
    sync::{Mutex, OwnedSemaphorePermit, Semaphore, oneshot, watch},
    task::JoinHandle,
};
use uuid::Uuid;

const GLOBAL_WORK_LIMIT: usize = 64;
const DEVICE_WORK_LIMIT: usize = 1;
const HARD_CLOSE_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TenantDevice {
    pub(crate) vault_id: Uuid,
    pub(crate) device_id: Uuid,
}

/// A single caller-provided monotonic deadline. It is never refreshed by a stage.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OperationDeadline(Instant);

impl OperationDeadline {
    pub(crate) fn after(budget: Duration) -> Self {
        Self(Instant::now() + budget)
    }

    pub(crate) fn remaining(self) -> Option<Duration> {
        self.0.checked_duration_since(Instant::now())
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TicketDbWorkError {
    #[error("ticket database operation deadline elapsed")]
    Deadline,
    #[error("ticket database operation cancelled")]
    Cancelled,
    #[error("ticket database work capacity exhausted")]
    Capacity,
    #[error("ticket database workflow ended without known settlement")]
    Unsettled,
    #[error("ticket database error")]
    Database(#[from] sqlx::Error),
    #[error("ticket database child stopped")]
    ChildStopped,
    #[error("ticket database workflow panicked")]
    Panicked,
}

#[cfg(test)]
#[derive(Default)]
struct CancellationProbe {
    cancelled: std::sync::atomic::AtomicUsize,
    changed: tokio::sync::Notify,
}

#[cfg(test)]
impl CancellationProbe {
    fn record(self: &Arc<Self>, mut receiver: watch::Receiver<bool>) {
        let probe = self.clone();
        tokio::spawn(async move {
            while !*receiver.borrow_and_update() {
                if receiver.changed().await.is_err() {
                    return;
                }
            }
            probe
                .cancelled
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            probe.changed.notify_waiters();
        });
    }

    async fn wait_for(&self, expected: usize) {
        loop {
            let changed = self.changed.notified();
            if self.cancelled.load(std::sync::atomic::Ordering::SeqCst) >= expected {
                return;
            }
            changed.await;
        }
    }
}

#[cfg(test)]
#[derive(Default)]
struct CheckoutWaitProbe {
    entered: std::sync::atomic::AtomicUsize,
    changed: tokio::sync::Notify,
}

#[cfg(test)]
impl CheckoutWaitProbe {
    fn record_pending(&self) {
        self.entered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    async fn wait_for(&self, expected: usize) {
        loop {
            let changed = self.changed.notified();
            if self.entered.load(std::sync::atomic::Ordering::SeqCst) >= expected {
                return;
            }
            changed.await;
        }
    }
}

/// A registry whose permits cover both active and indeterminate work.
#[derive(Clone)]
pub(crate) struct TicketDbWorkRegistry {
    global: Arc<Semaphore>,
    devices: Arc<Mutex<HashMap<TenantDevice, std::sync::Weak<Semaphore>>>>,
    #[cfg(test)]
    checkout_waits: Arc<CheckoutWaitProbe>,
    #[cfg(test)]
    cancellations: Arc<CancellationProbe>,
}

impl Default for TicketDbWorkRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TicketDbWorkRegistry {
    pub(crate) fn new() -> Self {
        Self {
            global: Arc::new(Semaphore::new(GLOBAL_WORK_LIMIT)),
            devices: Arc::new(Mutex::new(HashMap::new())),
            #[cfg(test)]
            checkout_waits: Arc::new(CheckoutWaitProbe::default()),
            #[cfg(test)]
            cancellations: Arc::new(CancellationProbe::default()),
        }
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_checkout_waiters(&self, expected: usize) {
        self.checkout_waits.wait_for(expected).await;
    }

    #[cfg(test)]
    pub(crate) fn checkout_waiter_count(&self) -> usize {
        self.checkout_waits
            .entered
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_cancellations(&self, expected: usize) {
        self.cancellations.wait_for(expected).await;
    }

    #[cfg(test)]
    pub(crate) async fn wait_until_idle(&self) {
        let _all_slots = self
            .global
            .acquire_many(GLOBAL_WORK_LIMIT as u32)
            .await
            .expect("test work registry remains open");
    }

    /// Starts a tracked child. Dropping the returned handle only abandons the
    /// caller's response; it cannot release the child's permits or connection.
    pub(crate) fn spawn<T: Send + 'static>(
        &self,
        pool: PgPool,
        deadline: OperationDeadline,
        cancellation: watch::Receiver<bool>,
        workflow: TicketWorkflow<T>,
    ) -> Result<TicketDbWork<T>, TicketDbWorkError> {
        let global = self
            .global
            .clone()
            .try_acquire_owned()
            .map_err(|_| TicketDbWorkError::Capacity)?;
        #[cfg(test)]
        self.cancellations.record(cancellation.clone());
        let registry = self.clone();
        let outcome_cancellation = cancellation.clone();
        let (result, receiver) = oneshot::channel();
        #[cfg(test)]
        let (queued_result, queued_receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            let outcome = run_work(registry, global, pool, deadline, cancellation, workflow).await;
            #[cfg(test)]
            let succeeded = outcome.is_ok();
            let _ = result.send(outcome);
            #[cfg(test)]
            let _ = queued_result.send(succeeded);
        });
        Ok(TicketDbWork {
            receiver,
            task,
            deadline,
            cancellation: Some(outcome_cancellation),
            #[cfg(test)]
            queued_result: Some(queued_receiver),
        })
    }
}

pub(crate) type TicketWorkflow<T> = Box<
    dyn for<'scope> FnOnce(
            &'scope mut ExecutionScope,
        ) -> Pin<
            Box<dyn Future<Output = Result<T, TicketDbWorkError>> + Send + 'scope>,
        > + Send,
>;

/// The caller's handle to a still-owned child.
pub(crate) struct TicketDbWork<T> {
    receiver: oneshot::Receiver<Result<T, TicketDbWorkError>>,
    task: JoinHandle<()>,
    deadline: OperationDeadline,
    cancellation: Option<watch::Receiver<bool>>,
    #[cfg(test)]
    queued_result: Option<oneshot::Receiver<bool>>,
}

impl<T> TicketDbWork<T> {
    pub(crate) async fn outcome(self) -> Result<T, TicketDbWorkError> {
        let result = self
            .receiver
            .await
            .map_err(|_| TicketDbWorkError::ChildStopped)?;
        self.task
            .await
            .map_err(|_| TicketDbWorkError::ChildStopped)?;
        if result.is_ok()
            && (self.deadline.remaining().is_none()
                || self
                    .cancellation
                    .as_ref()
                    .is_some_and(|value| *value.borrow()))
        {
            return Err(if self.deadline.remaining().is_none() {
                TicketDbWorkError::Deadline
            } else {
                TicketDbWorkError::Cancelled
            });
        }
        result
    }

    #[cfg(test)]
    async fn wait_until_result_queued(&mut self) -> bool {
        self.queued_result
            .take()
            .expect("queued result may be observed once")
            .await
            .expect("child must report whether its queued result succeeded")
    }
}

/// Child-owned connection, operation deadline, and permits.
///
/// Do not create `sqlx::Transaction` from the returned connection: dropping a
/// transaction is not a rollback proof and prevents this scope from owning the
/// connection needed for quarantine. Use explicit `BEGIN`/`COMMIT` statements
/// through `stage`, and call `settled` only after a known response.
pub(crate) struct ExecutionScope {
    deadline: OperationDeadline,
    cancellation: watch::Receiver<bool>,
    connection: Option<PoolConnection<Postgres>>,
    _global: OwnedSemaphorePermit,
    _device: Option<OwnedSemaphorePermit>,
    devices: Arc<Mutex<HashMap<TenantDevice, std::sync::Weak<Semaphore>>>>,
    settled: bool,
}

impl Drop for ExecutionScope {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        if let Some(connection) = self.connection.take() {
            // A task abort or panic can bypass async quarantine. Detach first so
            // the pool cannot reuse an uncertain manual transaction, then drop
            // the actual socket synchronously before device/global permits drop.
            drop(connection.detach());
        }
    }
}

impl ExecutionScope {
    /// Claims the exact tenant/device before any authority mutation. Discovery
    /// may run with the global permit alone before this call.
    pub(crate) async fn claim_device(
        &mut self,
        device: TenantDevice,
    ) -> Result<(), TicketDbWorkError> {
        if self._device.is_some() {
            return Err(TicketDbWorkError::Capacity);
        }
        self.check()?;
        let semaphore = {
            let mut devices = self.devices.lock().await;
            devices.retain(|_, semaphore| semaphore.strong_count() != 0);
            if let Some(semaphore) = devices.get(&device).and_then(std::sync::Weak::upgrade) {
                semaphore
            } else {
                let semaphore = Arc::new(Semaphore::new(DEVICE_WORK_LIMIT));
                devices.insert(device, Arc::downgrade(&semaphore));
                semaphore
            }
        };
        self._device = Some(
            semaphore
                .try_acquire_owned()
                .map_err(|_| TicketDbWorkError::Capacity)?,
        );
        Ok(())
    }

    /// Checks cancellation and the original deadline immediately before one
    /// authority statement. A caller must not retain this reference across work.
    pub(crate) async fn stage(&mut self) -> Result<&mut PgConnection, TicketDbWorkError> {
        self.check()?;
        Ok(self
            .connection
            .as_deref_mut()
            .expect("child owns its connection"))
    }

    /// Marks a workflow as known-settled only after its explicit COMMIT or
    /// ROLLBACK statement returned successfully.
    pub(crate) fn settled(&mut self) {
        self.settled = true;
    }

    fn check(&self) -> Result<(), TicketDbWorkError> {
        if *self.cancellation.borrow() {
            return Err(TicketDbWorkError::Cancelled);
        }
        if self.deadline.remaining().is_none() {
            return Err(TicketDbWorkError::Deadline);
        }
        Ok(())
    }

    async fn quarantine(mut self) {
        if let Some(connection) = self.connection.take() {
            // `detach` retains ownership of the actual socket; do not return an
            // uncertain transaction to the pool. `close_hard` sends no graceful
            // SQL shutdown and is bounded only for local responsiveness.
            let connection = connection.detach();
            let _ = tokio::time::timeout(HARD_CLOSE_WAIT, connection.close_hard()).await;
        }
    }
}

async fn run_work<T: Send + 'static>(
    registry: TicketDbWorkRegistry,
    global: OwnedSemaphorePermit,
    pool: PgPool,
    deadline: OperationDeadline,
    mut cancellation: watch::Receiver<bool>,
    workflow: TicketWorkflow<T>,
) -> Result<T, TicketDbWorkError> {
    let connection = acquire_connection(
        &mut cancellation,
        deadline,
        &pool,
        #[cfg(test)]
        registry.checkout_waits.clone(),
    )
    .await?;
    let mut scope = ExecutionScope {
        deadline,
        cancellation,
        connection: Some(connection),
        _global: global,
        _device: None,
        devices: registry.devices,
        settled: false,
    };
    let result = execute_workflow(&mut scope, deadline, workflow).await;
    let final_check = scope.check();
    if result.is_ok() && scope.settled && final_check.is_ok() {
        return result;
    }
    scope.quarantine().await;
    match result {
        Err(error) => Err(error),
        Ok(_) => match final_check {
            Err(error) => Err(error),
            Ok(()) => Err(TicketDbWorkError::Unsettled),
        },
    }
}

async fn execute_workflow<T>(
    scope: &mut ExecutionScope,
    deadline: OperationDeadline,
    workflow: TicketWorkflow<T>,
) -> Result<T, TicketDbWorkError> {
    let mut cancellation = scope.cancellation.clone();
    let constructed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| workflow(scope)));
    let Ok(workflow) = constructed else {
        return Err(TicketDbWorkError::Panicked);
    };
    let remaining = deadline.remaining().ok_or(TicketDbWorkError::Deadline)?;
    let mut workflow = workflow;
    let result = std::panic::AssertUnwindSafe(async {
        tokio::select! {
            _ = tokio::time::sleep(remaining) => Err(TicketDbWorkError::Deadline),
            changed = cancellation.changed() => {
                let _ = changed;
                Err(TicketDbWorkError::Cancelled)
            }
            result = &mut workflow => result,
        }
    })
    .catch_unwind()
    .await
    .unwrap_or(Err(TicketDbWorkError::Panicked));
    let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(workflow)));
    if dropped.is_err() {
        Err(TicketDbWorkError::Panicked)
    } else {
        result
    }
}

#[cfg(not(test))]
async fn acquire_connection(
    cancellation: &mut watch::Receiver<bool>,
    deadline: OperationDeadline,
    pool: &PgPool,
) -> Result<PoolConnection<Postgres>, TicketDbWorkError> {
    Ok(wait_for(cancellation, deadline, pool.acquire()).await??)
}

#[cfg(test)]
async fn acquire_connection(
    cancellation: &mut watch::Receiver<bool>,
    deadline: OperationDeadline,
    pool: &PgPool,
    checkout_waits: Arc<CheckoutWaitProbe>,
) -> Result<PoolConnection<Postgres>, TicketDbWorkError> {
    let acquire = pool.acquire();
    tokio::pin!(acquire);
    let mut pending_recorded = false;
    let observed_acquire = std::future::poll_fn(move |context| {
        let result = acquire.as_mut().poll(context);
        if result.is_pending() && !pending_recorded {
            pending_recorded = true;
            checkout_waits.record_pending();
        }
        result
    });
    Ok(wait_for(cancellation, deadline, observed_acquire).await??)
}

async fn wait_for<F, T>(
    cancellation: &mut watch::Receiver<bool>,
    deadline: OperationDeadline,
    future: F,
) -> Result<T, TicketDbWorkError>
where
    F: Future<Output = T>,
{
    if *cancellation.borrow() {
        return Err(TicketDbWorkError::Cancelled);
    }
    let remaining = deadline.remaining().ok_or(TicketDbWorkError::Deadline)?;
    tokio::pin!(future);
    tokio::select! {
        _ = tokio::time::sleep(remaining) => Err(TicketDbWorkError::Deadline),
        changed = cancellation.changed() => {
            let _ = changed;
            Err(TicketDbWorkError::Cancelled)
        }
        value = &mut future => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::{Executor, postgres::PgPoolOptions};
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn pool(connections: u32) -> PgPool {
        PgPoolOptions::new()
            .max_connections(connections)
            .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required"))
            .await
            .unwrap()
    }

    fn workflow<T: Send + 'static>(
        f: impl for<'a> FnOnce(
            &'a mut ExecutionScope,
        ) -> Pin<
            Box<dyn Future<Output = Result<T, TicketDbWorkError>> + Send + 'a>,
        > + Send
        + 'static,
    ) -> TicketWorkflow<T> {
        Box::new(f)
    }

    #[tokio::test]
    async fn cancellation_after_a_database_stage_runs_no_next_authority_stage() {
        let pool = pool(2).await;
        let registry = TicketDbWorkRegistry::new();
        let (cancel, receiver) = watch::channel(false);
        let entered = Arc::new(tokio::sync::Barrier::new(2));
        let release = Arc::new(tokio::sync::Barrier::new(2));
        let next_sql = Arc::new(AtomicUsize::new(0));
        let entered_child = entered.clone();
        let release_child = release.clone();
        let next_sql_child = next_sql.clone();
        let child = registry
            .spawn(
                pool.clone(),
                OperationDeadline::after(Duration::from_secs(5)),
                receiver,
                workflow(move |scope| {
                    Box::pin(async move {
                        scope.stage().await?.execute("SELECT 1").await?;
                        entered_child.wait().await;
                        release_child.wait().await;
                        let connection = scope.stage().await?;
                        next_sql_child.fetch_add(1, Ordering::SeqCst);
                        connection.execute("SELECT pg_sleep(10)").await?;
                        scope.settled();
                        Ok(())
                    })
                }),
            )
            .unwrap();
        entered.wait().await;
        cancel.send(true).unwrap();
        release.wait().await;
        assert!(matches!(
            child.outcome().await,
            Err(TicketDbWorkError::Cancelled)
        ));
        assert_eq!(next_sql.load(Ordering::SeqCst), 0);
        pool.close().await;
    }

    #[tokio::test]
    async fn occupied_device_rejects_successor_until_hard_close() {
        let pool = pool(2).await;
        let registry = TicketDbWorkRegistry::new();
        let device = TenantDevice {
            vault_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
        };
        let (first_cancel, receiver) = watch::channel(false);
        let entered = Arc::new(tokio::sync::Barrier::new(2));
        let release = Arc::new(tokio::sync::Barrier::new(2));
        let entered_child = entered.clone();
        let release_child = release.clone();
        let first: TicketDbWork<()> = registry
            .spawn(
                pool.clone(),
                OperationDeadline::after(Duration::from_secs(5)),
                receiver,
                workflow(move |scope| {
                    Box::pin(async move {
                        scope.claim_device(device).await?;
                        scope.stage().await?.execute("SELECT 1").await?;
                        entered_child.wait().await;
                        release_child.wait().await;
                        Err(TicketDbWorkError::Cancelled)
                    })
                }),
            )
            .unwrap();
        entered.wait().await;
        drop(first);
        let (_second_cancel, receiver) = watch::channel(false);
        let second: TicketDbWork<()> = registry
            .spawn(
                pool.clone(),
                OperationDeadline::after(Duration::from_secs(5)),
                receiver,
                workflow(move |scope| {
                    Box::pin(async move {
                        scope.claim_device(device).await?;
                        Err(TicketDbWorkError::Cancelled)
                    })
                }),
            )
            .unwrap();
        assert!(matches!(
            second.outcome().await,
            Err(TicketDbWorkError::Capacity)
        ));
        assert!(!*first_cancel.borrow());
        release.wait().await;
        pool.close().await;
    }

    struct PanicOnDrop;

    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("workflow future drop panic");
        }
    }

    #[tokio::test]
    async fn cancelled_future_drop_panic_disposes_connection_before_device_release() {
        let work_pool = pool(1).await;
        let observer = pool(1).await;
        let registry = TicketDbWorkRegistry::new();
        let device = TenantDevice {
            vault_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
        };
        let (cancel, receiver) = watch::channel(false);
        let (pid_sent, pid_received) = oneshot::channel();
        let child: TicketDbWork<()> = registry
            .spawn(
                work_pool.clone(),
                OperationDeadline::after(Duration::from_secs(5)),
                receiver,
                workflow(move |scope| {
                    Box::pin(async move {
                        scope.claim_device(device).await?;
                        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                            .fetch_one(scope.stage().await?)
                            .await?;
                        scope.stage().await?.execute("BEGIN").await?;
                        scope
                            .stage()
                            .await?
                            .execute("CREATE TEMP TABLE drop_panic_write(value integer)")
                            .await?;
                        scope
                            .stage()
                            .await?
                            .execute("INSERT INTO drop_panic_write VALUES (1)")
                            .await?;
                        pid_sent.send(pid).unwrap();
                        let _panic_on_drop = PanicOnDrop;
                        std::future::pending::<()>().await;
                        unreachable!()
                    })
                }),
            )
            .unwrap();
        let pid = pid_received.await.unwrap();
        cancel.send(true).unwrap();
        assert!(matches!(
            child.outcome().await,
            Err(TicketDbWorkError::Cancelled | TicketDbWorkError::Panicked)
                | Err(TicketDbWorkError::ChildStopped)
        ));

        let (_successor_cancel, successor_receiver) = watch::channel(false);
        let observer_for_successor = observer.clone();
        let successor = registry
            .spawn(
                work_pool.clone(),
                OperationDeadline::after(Duration::from_secs(5)),
                successor_receiver,
                workflow(move |scope| {
                    Box::pin(async move {
                        scope.claim_device(device).await?;
                        let still_connected: bool = sqlx::query_scalar(
                            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1)",
                        )
                        .bind(pid)
                        .fetch_one(&observer_for_successor)
                        .await?;
                        assert!(
                            !still_connected,
                            "device permit released before real connection disposal"
                        );
                        scope.stage().await?.execute("BEGIN").await?;
                        scope.stage().await?.execute("ROLLBACK").await?;
                        scope.settled();
                        Ok(())
                    })
                }),
            )
            .unwrap();
        successor.outcome().await.unwrap();
        work_pool.close().await;
        observer.close().await;
    }

    #[tokio::test]
    async fn global_cap_is_enforced_before_pool_starvation() {
        let pool = pool(1).await;
        let held_connection = pool.acquire().await.unwrap();
        let registry = TicketDbWorkRegistry::new();
        let constructions = Arc::new(AtomicUsize::new(0));
        let (cancel, receiver) = watch::channel(false);
        let mut children = Vec::new();
        for _ in 0..GLOBAL_WORK_LIMIT {
            let constructions = constructions.clone();
            children.push(
                registry
                    .spawn(
                        pool.clone(),
                        OperationDeadline::after(Duration::from_secs(5)),
                        receiver.clone(),
                        workflow(move |_| {
                            constructions.fetch_add(1, Ordering::SeqCst);
                            Box::pin(async { Ok(()) })
                        }),
                    )
                    .unwrap(),
            );
        }
        registry.wait_for_checkout_waiters(GLOBAL_WORK_LIMIT).await;
        assert_eq!(constructions.load(Ordering::SeqCst), 0);

        let constructions_65 = constructions.clone();
        let rejected = registry.spawn(
            pool.clone(),
            OperationDeadline::after(Duration::from_secs(5)),
            receiver,
            workflow(move |_| {
                constructions_65.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }),
        );
        assert!(matches!(rejected, Err(TicketDbWorkError::Capacity)));
        assert_eq!(constructions.load(Ordering::SeqCst), 0);

        cancel.send(true).unwrap();
        for child in children {
            assert!(matches!(
                child.outcome().await,
                Err(TicketDbWorkError::Cancelled)
            ));
        }
        drop(held_connection);

        let (_cancel, receiver) = watch::channel(false);
        let admitted = registry
            .spawn(
                pool.clone(),
                OperationDeadline::after(Duration::from_secs(5)),
                receiver,
                workflow(|scope| {
                    Box::pin(async move {
                        scope.stage().await?.execute("BEGIN").await?;
                        scope.stage().await?.execute("ROLLBACK").await?;
                        scope.settled();
                        Ok(())
                    })
                }),
            )
            .unwrap();
        admitted.outcome().await.unwrap();
        pool.close().await;
    }

    async fn backend_exists(observer: &PgPool, pid: i32) -> bool {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1)")
            .bind(pid)
            .fetch_one(observer)
            .await
            .unwrap()
    }

    async fn wait_for_backend_disposal(observer: &PgPool, pid: i32) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while backend_exists(observer, pid).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("database backend must observe the disposed socket");
    }

    #[tokio::test]
    async fn construction_panic_disposes_checked_out_connection() {
        let pool = pool(1).await;
        let registry = TicketDbWorkRegistry::new();
        let (_cancel, receiver) = watch::channel(false);
        let child = registry
            .spawn::<()>(
                pool.clone(),
                OperationDeadline::after(Duration::from_secs(5)),
                receiver,
                workflow(|_| panic!("construction panic")),
            )
            .unwrap();
        assert!(matches!(
            child.outcome().await,
            Err(TicketDbWorkError::Panicked)
        ));
        assert_eq!(pool.size(), 0);
        pool.close().await;
    }

    #[tokio::test]
    async fn poll_panic_after_begin_and_write_disposes_connection() {
        let work_pool = pool(1).await;
        let observer = pool(1).await;
        let registry = TicketDbWorkRegistry::new();
        let (_cancel, receiver) = watch::channel(false);
        let (pid_sent, pid_received) = oneshot::channel();
        let child = registry
            .spawn::<()>(
                work_pool.clone(),
                OperationDeadline::after(Duration::from_secs(5)),
                receiver,
                workflow(move |scope| {
                    Box::pin(async move {
                        let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
                            .fetch_one(scope.stage().await?)
                            .await?;
                        scope.stage().await?.execute("BEGIN").await?;
                        scope
                            .stage()
                            .await?
                            .execute("CREATE TEMP TABLE poll_panic_write(value integer)")
                            .await?;
                        scope
                            .stage()
                            .await?
                            .execute("INSERT INTO poll_panic_write VALUES (1)")
                            .await?;
                        pid_sent.send(pid).unwrap();
                        panic!("poll panic");
                    })
                }),
            )
            .unwrap();
        let pid = pid_received.await.unwrap();
        assert!(matches!(
            child.outcome().await,
            Err(TicketDbWorkError::Panicked)
        ));
        assert!(!backend_exists(&observer, pid).await);
        assert_eq!(work_pool.size(), 0);
        work_pool.close().await;
        observer.close().await;
    }

    #[tokio::test]
    async fn task_abort_disposes_connection_before_device_release() {
        let work_pool = pool(1).await;
        let observer = pool(1).await;
        let registry = TicketDbWorkRegistry::new();
        let device = TenantDevice {
            vault_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
        };
        let (_cancel, receiver) = watch::channel(false);
        let (pid_sent, pid_received) = oneshot::channel();
        let child = registry
            .spawn::<()>(
                work_pool.clone(),
                OperationDeadline::after(Duration::from_secs(5)),
                receiver,
                workflow(move |scope| {
                    Box::pin(async move {
                        scope.claim_device(device).await?;
                        let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
                            .fetch_one(scope.stage().await?)
                            .await?;
                        scope.stage().await?.execute("BEGIN").await?;
                        pid_sent.send(pid).unwrap();
                        std::future::pending().await
                    })
                }),
            )
            .unwrap();
        let pid = pid_received.await.unwrap();
        child.task.abort();
        assert!(matches!(
            child.outcome().await,
            Err(TicketDbWorkError::ChildStopped)
        ));

        let (_cancel, receiver) = watch::channel(false);
        let observer_for_successor = observer.clone();
        let successor = registry
            .spawn(
                work_pool.clone(),
                OperationDeadline::after(Duration::from_secs(5)),
                receiver,
                workflow(move |scope| {
                    Box::pin(async move {
                        scope.claim_device(device).await?;
                        assert!(!backend_exists(&observer_for_successor, pid).await);
                        scope.stage().await?.execute("BEGIN").await?;
                        scope.stage().await?.execute("ROLLBACK").await?;
                        scope.settled();
                        Ok(())
                    })
                }),
            )
            .unwrap();
        successor.outcome().await.unwrap();
        work_pool.close().await;
        observer.close().await;
    }

    #[tokio::test]
    async fn cancellation_interrupts_real_database_lock_wait_and_disposes_connection() {
        let work_pool = pool(1).await;
        let observer = pool(2).await;
        let mut lock_owner = observer.acquire().await.unwrap();
        let key = 7_000_000_i64 + i64::from(Uuid::new_v4().as_bytes()[0]);
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(key)
            .execute(&mut *lock_owner)
            .await
            .unwrap();
        let registry = TicketDbWorkRegistry::new();
        let (cancel, receiver) = watch::channel(false);
        let (pid_sent, pid_received) = oneshot::channel();
        let child = registry
            .spawn::<()>(
                work_pool.clone(),
                OperationDeadline::after(Duration::from_secs(5)),
                receiver,
                workflow(move |scope| {
                    Box::pin(async move {
                        let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
                            .fetch_one(scope.stage().await?)
                            .await?;
                        pid_sent.send(pid).unwrap();
                        sqlx::query("SELECT pg_advisory_lock($1)")
                            .bind(key)
                            .execute(scope.stage().await?)
                            .await?;
                        Ok(())
                    })
                }),
            )
            .unwrap();
        let pid = pid_received.await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let waiting: bool = sqlx::query_scalar(
                    "SELECT wait_event_type = 'Lock' FROM pg_stat_activity WHERE pid = $1",
                )
                .bind(pid)
                .fetch_optional(&observer)
                .await
                .unwrap()
                .unwrap_or(false);
                if waiting {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        cancel.send(true).unwrap();
        assert!(matches!(
            child.outcome().await,
            Err(TicketDbWorkError::Cancelled)
        ));
        assert_eq!(
            work_pool.size(),
            0,
            "blocked connection returned to the pool"
        );
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(key)
            .execute(&mut *lock_owner)
            .await
            .unwrap();
        drop(lock_owner);
        wait_for_backend_disposal(&observer, pid).await;
        work_pool.close().await;
        observer.close().await;
    }

    #[tokio::test]
    async fn completed_result_queued_past_deadline_is_not_delivered_as_success() {
        let pool = pool(1).await;
        let registry = TicketDbWorkRegistry::new();
        let (_cancel, receiver) = watch::channel(false);
        let mut child = registry
            .spawn(
                pool.clone(),
                OperationDeadline::after(Duration::from_secs(2)),
                receiver,
                workflow(|scope| {
                    Box::pin(async move {
                        scope.stage().await?.execute("BEGIN").await?;
                        scope.stage().await?.execute("ROLLBACK").await?;
                        scope.settled();
                        Ok(7_u8)
                    })
                }),
            )
            .unwrap();
        assert!(child.wait_until_result_queued().await);
        while child.deadline.remaining().is_some() {
            tokio::time::sleep_until(child.deadline.0.into()).await;
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            child.outcome().await,
            Err(TicketDbWorkError::Deadline)
        ));
        pool.close().await;
    }

    #[tokio::test]
    async fn cancellation_wins_at_completion_barrier() {
        let pool = pool(1).await;
        let registry = TicketDbWorkRegistry::new();
        let (cancel, receiver) = watch::channel(false);
        let entered = Arc::new(tokio::sync::Barrier::new(2));
        let release = Arc::new(tokio::sync::Barrier::new(2));
        let entered_child = entered.clone();
        let release_child = release.clone();
        let child = registry
            .spawn(
                pool.clone(),
                OperationDeadline::after(Duration::from_secs(5)),
                receiver,
                workflow(move |scope| {
                    Box::pin(async move {
                        scope.stage().await?.execute("BEGIN").await?;
                        scope.stage().await?.execute("ROLLBACK").await?;
                        scope.settled();
                        entered_child.wait().await;
                        release_child.wait().await;
                        Ok(())
                    })
                }),
            )
            .unwrap();
        entered.wait().await;
        cancel.send(true).unwrap();
        release.wait().await;
        assert!(matches!(
            child.outcome().await,
            Err(TicketDbWorkError::Cancelled)
        ));
        pool.close().await;
    }

    #[tokio::test]
    async fn settled_success_reuses_connection_and_weak_device_map_stays_bounded() {
        let pool = pool(1).await;
        let registry = TicketDbWorkRegistry::new();
        let mut previous_pid: Option<i32> = None;
        for _ in 0..96 {
            let device = TenantDevice {
                vault_id: Uuid::new_v4(),
                device_id: Uuid::new_v4(),
            };
            let (_cancel, receiver) = watch::channel(false);
            let child = registry
                .spawn(
                    pool.clone(),
                    OperationDeadline::after(Duration::from_secs(5)),
                    receiver,
                    workflow(move |scope| {
                        Box::pin(async move {
                            scope.claim_device(device).await?;
                            let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                                .fetch_one(scope.stage().await?)
                                .await?;
                            scope.stage().await?.execute("BEGIN").await?;
                            scope.stage().await?.execute("ROLLBACK").await?;
                            scope.settled();
                            Ok(pid)
                        })
                    }),
                )
                .unwrap();
            let pid = child.outcome().await.unwrap();
            if let Some(previous_pid) = previous_pid {
                assert_eq!(pid, previous_pid);
            }
            previous_pid = Some(pid);
        }
        assert!(registry.devices.lock().await.len() <= 1);
        assert_eq!(pool.size(), 1);
        pool.close().await;
    }

    #[tokio::test]
    async fn abandoned_handle_remains_owned_until_original_deadline() {
        let pool = pool(1).await;
        let registry = TicketDbWorkRegistry::new();
        let device = TenantDevice {
            vault_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
        };
        let (_cancel, receiver) = watch::channel(false);
        let (entered, entered_receiver) = oneshot::channel();
        let child: TicketDbWork<()> = registry
            .spawn(
                pool.clone(),
                OperationDeadline::after(Duration::from_millis(80)),
                receiver,
                workflow(move |scope| {
                    Box::pin(async move {
                        scope.claim_device(device).await?;
                        scope.stage().await?.execute("BEGIN").await?;
                        entered.send(()).unwrap();
                        std::future::pending().await
                    })
                }),
            )
            .unwrap();
        entered_receiver.await.unwrap();
        drop(child);

        let (_cancel, receiver) = watch::channel(false);
        let (successor_entered, mut successor_receiver) = oneshot::channel();
        let successor = registry
            .spawn(
                pool.clone(),
                OperationDeadline::after(Duration::from_secs(2)),
                receiver,
                workflow(move |scope| {
                    Box::pin(async move {
                        scope.claim_device(device).await?;
                        successor_entered.send(()).unwrap();
                        scope.stage().await?.execute("BEGIN").await?;
                        scope.stage().await?.execute("ROLLBACK").await?;
                        scope.settled();
                        Ok(())
                    })
                }),
            )
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut successor_receiver)
                .await
                .is_err()
        );
        tokio::time::timeout(Duration::from_secs(1), &mut successor_receiver)
            .await
            .unwrap()
            .unwrap();
        successor.outcome().await.unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn workflow_finishing_after_deadline_cannot_return_success() {
        let pool = pool(1).await;
        let registry = TicketDbWorkRegistry::new();
        let (_cancel, receiver) = watch::channel(false);
        let child = registry
            .spawn(
                pool.clone(),
                OperationDeadline::after(Duration::from_millis(20)),
                receiver,
                workflow(|scope| {
                    Box::pin(async move {
                        scope.stage().await?.execute("BEGIN").await?;
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        scope.stage().await?.execute("ROLLBACK").await?;
                        scope.settled();
                        Ok(())
                    })
                }),
            )
            .unwrap();
        assert!(matches!(
            child.outcome().await,
            Err(TicketDbWorkError::Deadline)
        ));
        pool.close().await;
    }

    struct CancelWhenWorkflowCompletes(Option<watch::Sender<bool>>);

    impl Drop for CancelWhenWorkflowCompletes {
        fn drop(&mut self) {
            self.0.take().unwrap().send(true).unwrap();
        }
    }

    #[tokio::test]
    async fn completed_workflow_reports_final_cancellation_not_unsettled() {
        let pool = pool(1).await;
        let registry = TicketDbWorkRegistry::new();
        let (cancel, receiver) = watch::channel(false);
        let child = registry
            .spawn(
                pool.clone(),
                OperationDeadline::after(Duration::from_secs(5)),
                receiver,
                workflow(move |scope| {
                    Box::pin(async move {
                        let _cancel_on_completion = CancelWhenWorkflowCompletes(Some(cancel));
                        scope.stage().await?.execute("BEGIN").await?;
                        scope.stage().await?.execute("ROLLBACK").await?;
                        scope.settled();
                        Ok(())
                    })
                }),
            )
            .unwrap();
        assert!(matches!(
            child.outcome().await,
            Err(TicketDbWorkError::Cancelled)
        ));
        pool.close().await;
    }

    #[tokio::test]
    async fn completed_workflow_reports_final_deadline_not_unsettled() {
        let pool = pool(1).await;
        let registry = TicketDbWorkRegistry::new();
        let (_cancel, receiver) = watch::channel(false);
        let child = registry
            .spawn(
                pool.clone(),
                OperationDeadline::after(Duration::from_secs(5)),
                receiver,
                workflow(|scope| {
                    Box::pin(async move {
                        scope.stage().await?.execute("BEGIN").await?;
                        scope.stage().await?.execute("ROLLBACK").await?;
                        scope.settled();
                        scope.deadline =
                            OperationDeadline(Instant::now() - Duration::from_millis(1));
                        Ok(())
                    })
                }),
            )
            .unwrap();
        assert!(matches!(
            child.outcome().await,
            Err(TicketDbWorkError::Deadline)
        ));
        pool.close().await;
    }
}
