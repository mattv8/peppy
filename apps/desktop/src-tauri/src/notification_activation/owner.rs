//! A single bounded native owner. Callback signals are coalesced per registration;
//! a full wake channel cannot lose a click because every queued command scans signals.
use super::*;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64},
        mpsc::{self, SyncSender},
        Arc,
    },
};
use tokio_util::sync::CancellationToken;

#[cfg(target_os = "macos")]
use super::macos as platform;
#[cfg(target_os = "windows")]
use super::windows as platform;

pub(super) const CAPACITY: usize = 256;
const ACCEPTANCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
#[cfg(any(target_os = "macos", test))]
pub(super) const PLATFORM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const CLICK: u64 = 1;
const RETIRE: u64 = 2;

pub struct Post {
    pub activation: NotificationActivation,
    pub cancel: CancellationToken,
    pub title: String,
    pub body: String,
    pub sound: bool,
}

#[derive(Clone)]
pub struct Signal {
    event: Arc<AtomicU64>,
    sequence: Arc<AtomicU64>,
    wake: SyncSender<Command>,
}

impl Signal {
    pub fn click(&self) {
        self.set(CLICK);
    }
    pub fn retire(&self) {
        self.set(RETIRE);
    }
    #[cfg(any(target_os = "windows", test))]
    pub fn dismissed(&self, timed_out: bool) {
        if !timed_out {
            self.retire();
        }
    }
    fn set(&self, event: u64) {
        // Exactly one terminal event per post; popup timeout is not terminal.
        let sequence = self.sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.event.compare_exchange(
            0,
            (sequence << 2) | event,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let _ = self.wake.try_send(Command::Wake);
    }
}

enum Command {
    Post(Post, SyncSender<Result<(), ()>>),
    Wake,
}

/// Only a bounded background banner drain waits here, never the UI or an OS
/// callback. Dropping/timing out a receipt cancels this post, not its Session.
pub struct Acceptance {
    result: mpsc::Receiver<Result<(), ()>>,
    cancel: CancellationToken,
    wake: SyncSender<Command>,
    stopped: Arc<AtomicBool>,
    accepted: bool,
}

impl Acceptance {
    pub fn wait(self) -> Result<(), ()> {
        self.wait_for(ACCEPTANCE_TIMEOUT)
    }

    fn wait_for(mut self, timeout: std::time::Duration) -> Result<(), ()> {
        let result = await_platform_acceptance(&self.result, timeout);
        if self.cancel.is_cancelled() || self.stopped.load(Ordering::Acquire) {
            return Err(());
        }
        self.accepted = result.is_ok();
        result
    }
}

impl Drop for Acceptance {
    fn drop(&mut self) {
        if !self.accepted {
            self.cancel.cancel();
            let _ = self.wake.try_send(Command::Wake);
        }
    }
}

pub(super) fn await_platform_acceptance(
    result: &mpsc::Receiver<Result<(), ()>>,
    timeout: std::time::Duration,
) -> Result<(), ()> {
    result.recv_timeout(timeout).map_err(|_| ())?
}

pub struct Owner {
    sender: SyncSender<Command>,
    stopped: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

struct Entry<T> {
    activation: NotificationActivation,
    cancel: CancellationToken,
    signal: Signal,
    handle: T,
}

trait Backend {
    type Item;
    fn post(&mut self, id: &str, title: &str, body: &str, sound: bool, signal: Signal)
        -> Result<Self::Item, ()>;
    fn remove(&mut self, item: &Self::Item) -> bool;
}

impl Backend for platform::Backend {
    type Item = platform::Item;
    fn post(
        &mut self,
        id: &str,
        title: &str,
        body: &str,
        sound: bool,
        signal: Signal,
    ) -> Result<Self::Item, ()> {
        self.post(id, title, body, sound, signal)
    }
    fn remove(&mut self, item: &Self::Item) -> bool {
        self.remove(item)
    }
}

struct Registry<B: Backend> {
    backend: B,
    entries: VecDeque<Entry<B::Item>>,
    sequence: Arc<AtomicU64>,
    wake: SyncSender<Command>,
}

impl<B: Backend> Registry<B> {
    fn post(&mut self, post: Post) -> Result<(), ()> {
        if post.cancel.is_cancelled() {
            return Err(());
        }
        if self.entries.len() == CAPACITY {
            if !self.backend.remove(&self.entries.front().unwrap().handle) {
                return Err(());
            }
            self.entries.pop_front();
        }
        let signal = Signal {
            event: Arc::new(AtomicU64::new(0)),
            sequence: self.sequence.clone(),
            wake: self.wake.clone(),
        };
        let handle = self.backend.post(
            &post.activation.request_id,
            &post.title,
            &post.body,
            post.sound,
            signal.clone(),
        )?;
        self.entries.push_back(Entry {
            activation: post.activation,
            cancel: post.cancel,
            signal,
            handle,
        });
        Ok(())
    }

    fn reconcile(&mut self) -> Vec<(u64, NotificationActivation, CancellationToken)> {
        let mut clicks = Vec::new();
        let mut index = 0;
        while index < self.entries.len() {
            let entry = &self.entries[index];
            let event = entry.signal.event.load(Ordering::Acquire);
            if event != 0 || entry.cancel.is_cancelled() {
                if event & 3 == CLICK && !entry.cancel.is_cancelled() {
                    clicks.push((event >> 2, entry.activation.clone(), entry.cancel.clone()));
                    // Cleanup failure must not suppress a genuine click or replay
                    // it on the next wake. Keep its registration until removal succeeds.
                    entry.signal.event.store(event | RETIRE, Ordering::Release);
                }
                if !self.backend.remove(&entry.handle) {
                    index += 1;
                    continue;
                }
                self.entries.remove(index);
            } else {
                index += 1;
            }
        }
        clicks.sort_by_key(|(sequence, _, _)| *sequence);
        clicks
    }
}

impl<B: Backend> Drop for Registry<B> {
    fn drop(&mut self) {
        // Drop callback tokens while the backend's thread apartment is alive.
        while let Some(entry) = self.entries.pop_front() {
            self.backend.remove(&entry.handle);
        }
    }
}

fn serve<B: Backend>(
    mut registry: Registry<B>,
    receiver: mpsc::Receiver<Command>,
    stop: Arc<AtomicBool>,
    mut activate: impl FnMut(u64, NotificationActivation, CancellationToken),
) {
    while let Ok(command) = receiver.recv() {
        if stop.load(Ordering::Acquire) {
            break;
        }
        for (sequence, activation, cancel) in registry.reconcile() {
            activate(sequence, activation, cancel);
        }
        if let Command::Post(post, reply) = command {
            let result = registry.post(post);
            let _ = reply.try_send(if stop.load(Ordering::Acquire) {
                Err(())
            } else {
                result
            });
        }
    }
}

impl Owner {
    pub fn start(app: tauri::AppHandle) -> Option<Self> {
        let setup = platform::prepare()?;
        let (sender, receiver) = mpsc::sync_channel(32);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let wake = sender.clone();
        let thread = std::thread::Builder::new()
            .name("notification-owner".into())
            .spawn(move || {
                let Some(backend) = platform::Backend::new(&app, setup) else {
                    return;
                };
                let registry = Registry {
                    backend,
                    entries: VecDeque::new(),
                    sequence: Arc::new(AtomicU64::new(0)),
                    wake,
                };
                serve(registry, receiver, stop, |sequence, activation, cancel| {
                    super::activate(&app, activation, &cancel, sequence)
                });
            })
            .ok()?;
        Some(Self {
            sender,
            stopped,
            thread,
        })
    }

    pub fn submit(&self, mut post: Post) -> Result<Acceptance, ()> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(());
        }
        post.cancel = post.cancel.child_token();
        let (reply, result) = mpsc::sync_channel(1);
        let acceptance = Acceptance {
            result,
            cancel: post.cancel.clone(),
            wake: self.sender.clone(),
            stopped: self.stopped.clone(),
            accepted: false,
        };
        self.sender
            .try_send(Command::Post(post, reply))
            .map_err(|_| ())?;
        Ok(acceptance)
    }
    pub fn wake(&self) {
        let _ = self.sender.try_send(Command::Wake);
    }
    pub fn stop(self) {
        self.stopped.store(true, Ordering::Release);
        self.wake();
        let _ = self.thread.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    type BackendAcceptanceGate = (SyncSender<()>, mpsc::Receiver<Result<(), ()>>);

    #[derive(Default)]
    struct TestBackend {
        callbacks: HashMap<String, Signal>,
        sounds: Vec<bool>,
        log: Arc<Mutex<Vec<String>>>,
        fail_post: bool,
        fail_remove: bool,
        reject_after: Option<usize>,
        acceptance: Option<BackendAcceptanceGate>,
        retired: Option<SyncSender<()>>,
    }
    struct TestItem {
        id: String,
        log: Arc<Mutex<Vec<String>>>,
    }
    impl Drop for TestItem {
        fn drop(&mut self) {
            self.log.lock().unwrap().push(format!("drop:{}", self.id));
        }
    }
    impl Backend for TestBackend {
        type Item = TestItem;
        fn post(&mut self, id: &str, _: &str, _: &str, sound: bool, signal: Signal) -> Result<TestItem, ()> {
            self.sounds.push(sound);
            if self.fail_post || self.reject_after == Some(self.callbacks.len()) {
                signal.retire();
                return Err(());
            }
            if let Some((started, result)) = self.acceptance.take() {
                started.send(()).unwrap();
                await_platform_acceptance(&result, PLATFORM_TIMEOUT)?;
            }
            self.callbacks.insert(id.into(), signal);
            Ok(TestItem {
                id: id.into(),
                log: self.log.clone(),
            })
        }
        fn remove(&mut self, item: &TestItem) -> bool {
            if self.fail_remove {
                return false;
            }
            self.log.lock().unwrap().push(format!("remove:{}", item.id));
            self.callbacks.remove(&item.id);
            if let Some(retired) = &self.retired {
                let _ = retired.try_send(());
            }
            true
        }
    }
    fn registry() -> Registry<TestBackend> {
        let (wake, _) = mpsc::sync_channel(1);
        Registry {
            backend: TestBackend::default(),
            entries: VecDeque::new(),
            sequence: Arc::new(AtomicU64::new(0)),
            wake,
        }
    }
    fn post(id: usize) -> Post {
        Post {
            activation: NotificationActivation {
                request_id: id.to_string(),
                session_generation: "instance".into(),
                destination: Destination::Main,
            },
            cancel: CancellationToken::new(),
            title: "not retained".into(),
            body: "not retained".into(),
            sound: false,
        }
    }

    #[test]
    fn notification_sound_choice_reaches_the_backend_for_each_post() {
        let mut registry = registry();
        registry.post(post(0)).unwrap();
        let mut audible = post(1);
        audible.sound = true;
        registry.post(audible).unwrap();
        assert_eq!(registry.backend.sounds, [false, true]);
    }

    fn test_owner(backend: TestBackend, prefill: usize) -> Owner {
        let (sender, receiver) = mpsc::sync_channel(32);
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let mut registry = Registry {
            backend,
            entries: VecDeque::new(),
            sequence: Arc::new(AtomicU64::new(0)),
            wake: sender.clone(),
        };
        // Populate before enabling a failing-cap cleanup seam.
        let fail_remove = registry.backend.fail_remove;
        registry.backend.fail_remove = false;
        for id in 0..prefill {
            registry.post(post(id)).unwrap();
        }
        registry.backend.fail_remove = fail_remove;
        let thread = std::thread::spawn(move || serve(registry, receiver, stop, |_, _, _| {}));
        Owner {
            sender,
            stopped,
            thread,
        }
    }

    fn durable_candidates(
        count: usize,
    ) -> (
        crate::tests::Fixture,
        Session,
        crate::notifications::NotificationSettings,
    ) {
        use peppy_client_core::{Client, ClientConfig, Cursor, DatabaseKey, DeviceId, IncomingSms};
        let fixture = crate::tests::fixture_at("http://127.0.0.1:9");
        let session = crate::tests::open(&fixture, &fixture.binding, &[]);
        session
            .client
            .unlock(&fixture.profile, &fixture.header, crate::tests::PHRASE)
            .unwrap();
        session
            .client
            .set_server_compaction_state(true, true)
            .unwrap();
        let phone = Client::open(
            ClientConfig {
                database_path: fixture.dir.path().join("phone.db"),
                vault_id: fixture.binding.vault_id.parse().unwrap(),
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[7; 32]).unwrap(),
        )
        .unwrap();
        phone
            .unlock(&fixture.profile, &fixture.header, crate::tests::PHRASE)
            .unwrap();
        phone.set_server_compaction_state(true, true).unwrap();
        for id in 0..count {
            phone
                .capture_incoming(IncomingSms {
                    conversation_id: None,
                    sender_address: "+15555550100".into(),
                    body: "message".into(),
                    provider_message_id: Some(format!("sms-{id}")),
                    imported: false,
                })
                .unwrap();
        }
        for (index, envelope) in phone.pending_outbox().unwrap().iter().enumerate() {
            session
                .client
                .ingest(envelope, Cursor(index as u64 + 1))
                .unwrap();
        }
        session.client.apply_pending(100).unwrap();
        assert_eq!(
            session.client.pending_banner_candidates(100).unwrap().len(),
            count
        );
        let settings = crate::notifications::NotificationSettings::load(fixture.dir.path());
        (fixture, session, settings)
    }

    fn drain_to_owner(
        session: &Session,
        settings: &crate::notifications::NotificationSettings,
        owner: &Owner,
    ) -> crate::error::BridgeResult<bool> {
        crate::notifications::drain_with_delivery(
            session,
            settings,
            || false,
            |_, _| false,
            |destination, title, body| {
                owner
                    .submit(Post {
                        activation: NotificationActivation {
                            request_id: uuid::Uuid::new_v4().to_string(),
                            session_generation: session.generation.clone(),
                            destination,
                        },
                        cancel: session.cancel.clone(),
                        sound: settings.sound_enabled(),
                        title,
                        body,
                    })
                    .and_then(Acceptance::wait)
            },
        )
    }

    #[test]
    fn notification_drain_backend_rejection_preserves_durable_candidate_and_error() {
        let (_fixture, session, settings) = durable_candidates(1);
        let owner = test_owner(
            TestBackend {
                fail_post: true,
                ..Default::default()
            },
            0,
        );
        let result = drain_to_owner(&session, &settings, &owner);
        owner.stop();
        assert_eq!(result.unwrap_err().code, "notification-post");
        assert_eq!(
            session.client.pending_banner_candidates(100).unwrap().len(),
            1
        );
        assert_eq!(
            session.status.lock().unwrap().work_error,
            Some("notification-post")
        );
    }

    #[test]
    fn notification_drain_failed_cap_cleanup_preserves_durable_candidate_and_error() {
        let (_fixture, session, settings) = durable_candidates(1);
        let owner = test_owner(
            TestBackend {
                fail_remove: true,
                ..Default::default()
            },
            CAPACITY,
        );
        let result = drain_to_owner(&session, &settings, &owner);
        owner.stop();
        assert_eq!(result.unwrap_err().code, "notification-post");
        assert_eq!(
            session.client.pending_banner_candidates(100).unwrap().len(),
            1
        );
        assert_eq!(
            session.status.lock().unwrap().work_error,
            Some("notification-post")
        );
    }

    #[test]
    fn notification_drain_failed_summary_preserves_every_aggregated_candidate() {
        let (_fixture, session, settings) = durable_candidates(4);
        let owner = test_owner(
            TestBackend {
                fail_post: true,
                ..Default::default()
            },
            0,
        );
        let result = drain_to_owner(&session, &settings, &owner);
        owner.stop();
        assert_eq!(result.unwrap_err().code, "notification-post");
        assert_eq!(
            session.client.pending_banner_candidates(100).unwrap().len(),
            4
        );
        assert_eq!(
            session.status.lock().unwrap().work_error,
            Some("notification-post")
        );
    }

    #[test]
    fn notification_drain_preserves_prior_success_ack_and_successful_retry_clears_error() {
        let (_fixture, session, settings) = durable_candidates(2);
        let original = session.client.pending_banner_candidates(100).unwrap();
        let owner = test_owner(
            TestBackend {
                reject_after: Some(1),
                ..Default::default()
            },
            0,
        );
        let result = drain_to_owner(&session, &settings, &owner);
        owner.stop();
        assert_eq!(result.unwrap_err().code, "notification-post");
        let remaining = session.client.pending_banner_candidates(100).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, original[1].id);
        assert_eq!(
            session.status.lock().unwrap().work_error,
            Some("notification-post")
        );
        let owner = test_owner(TestBackend::default(), 0);
        let result = drain_to_owner(&session, &settings, &owner);
        owner.stop();
        assert!(!result.unwrap());
        assert!(session
            .client
            .pending_banner_candidates(100)
            .unwrap()
            .is_empty());
        assert_eq!(session.status.lock().unwrap().work_error, None);
    }

    #[test]
    fn notification_drain_waits_for_async_backend_acceptance_not_enqueue() {
        for rejected in [false, true] {
            let (_fixture, session, settings) = durable_candidates(1);
            let session = Arc::new(session);
            let drain_session = session.clone();
            let (started, observing) = mpsc::sync_channel(1);
            let (complete, result) = mpsc::sync_channel(1);
            let owner = test_owner(
                TestBackend {
                    acceptance: Some((started, result)),
                    ..Default::default()
                },
                0,
            );
            let drain = std::thread::spawn(move || {
                let result = drain_to_owner(&drain_session, &settings, &owner);
                owner.stop();
                result
            });
            observing.recv_timeout(PLATFORM_TIMEOUT).unwrap();
            assert_eq!(
                session.client.pending_banner_candidates(100).unwrap().len(),
                1
            );
            assert_eq!(session.status.lock().unwrap().work_error, None);
            complete
                .send(if rejected { Err(()) } else { Ok(()) })
                .unwrap();
            let result = drain.join().unwrap();
            if rejected {
                assert_eq!(result.unwrap_err().code, "notification-post");
                assert_eq!(
                    session.client.pending_banner_candidates(100).unwrap().len(),
                    1
                );
                assert_eq!(
                    session.status.lock().unwrap().work_error,
                    Some("notification-post")
                );
            } else {
                assert!(!result.unwrap());
                assert!(session
                    .client
                    .pending_banner_candidates(100)
                    .unwrap()
                    .is_empty());
            }
        }
    }

    #[test]
    fn notification_drain_timeout_preserves_candidate_and_retires_late_acceptance() {
        let (_fixture, session, settings) = durable_candidates(1);
        let (started, observing) = mpsc::sync_channel(1);
        let (complete, result) = mpsc::sync_channel(1);
        let (retired, retirement) = mpsc::sync_channel(1);
        let owner = test_owner(
            TestBackend {
                acceptance: Some((started, result)),
                retired: Some(retired),
                ..Default::default()
            },
            0,
        );
        let result = crate::notifications::drain_with_delivery(
            &session,
            &settings,
            || false,
            |_, _| false,
            |destination, title, body| {
                let receipt = owner.submit(Post {
                    activation: NotificationActivation {
                        request_id: "late-post".into(),
                        session_generation: session.generation.clone(),
                        destination,
                    },
                    cancel: session.cancel.clone(),
                    sound: settings.sound_enabled(),
                    title,
                    body,
                })?;
                observing.recv_timeout(PLATFORM_TIMEOUT).unwrap();
                receipt.wait_for(std::time::Duration::ZERO)
            },
        );
        assert_eq!(result.unwrap_err().code, "notification-post");
        assert_eq!(
            session.client.pending_banner_candidates(100).unwrap().len(),
            1
        );
        assert!(!session.cancel.is_cancelled());
        complete.send(Ok(())).unwrap();
        retirement.recv_timeout(PLATFORM_TIMEOUT).unwrap();
        owner.stop();
        assert_eq!(
            session.client.pending_banner_candidates(100).unwrap().len(),
            1
        );
        assert_eq!(
            session.status.lock().unwrap().work_error,
            Some("notification-post")
        );
    }

    #[test]
    fn notification_drain_shutdown_during_acceptance_preserves_candidate() {
        let (_fixture, session, settings) = durable_candidates(1);
        let session = Arc::new(session);
        let drain_session = session.clone();
        let (started, observing) = mpsc::sync_channel(1);
        let (complete, result) = mpsc::sync_channel(1);
        let owner = test_owner(
            TestBackend {
                acceptance: Some((started, result)),
                ..Default::default()
            },
            0,
        );
        let stopped = owner.stopped.clone();
        let drain = std::thread::spawn(move || {
            let result = drain_to_owner(&drain_session, &settings, &owner);
            owner.stop();
            result
        });
        observing.recv_timeout(PLATFORM_TIMEOUT).unwrap();
        stopped.store(true, Ordering::Release);
        complete.send(Ok(())).unwrap();
        assert_eq!(drain.join().unwrap().unwrap_err().code, "notification-post");
        assert_eq!(
            session.client.pending_banner_candidates(100).unwrap().len(),
            1
        );
        assert_eq!(
            session.status.lock().unwrap().work_error,
            Some("notification-post")
        );
    }

    #[test]
    fn notification_cap_removes_os_item_before_releasing_callback() {
        let mut owner = registry();
        for id in 0..=CAPACITY {
            owner.post(post(id)).unwrap();
        }
        assert_eq!(owner.entries.len(), CAPACITY);
        assert_eq!(owner.backend.callbacks.len(), CAPACITY);
        assert_eq!(*owner.backend.log.lock().unwrap(), ["remove:0", "drop:0"]);
        assert!(!owner.backend.callbacks.contains_key("0"));
    }

    #[test]
    fn notification_failed_os_retirement_keeps_callback_and_refuses_overflow() {
        let mut owner = registry();
        for id in 0..CAPACITY {
            owner.post(post(id)).unwrap();
        }
        owner.backend.fail_remove = true;
        assert!(owner.post(post(CAPACITY)).is_err());
        assert_eq!(owner.entries.len(), CAPACITY);
        assert!(owner.backend.callbacks.contains_key("0"));
    }

    #[test]
    fn notification_timeout_retains_history_click_but_dismiss_and_failure_do_not_activate() {
        let mut owner = registry();
        for id in 0..3 {
            owner.post(post(id)).unwrap();
        }
        let history = owner.backend.callbacks["0"].clone();
        history.dismissed(true);
        owner.backend.callbacks["1"].dismissed(false);
        owner.backend.callbacks["2"].retire();
        assert!(owner.reconcile().is_empty());
        assert_eq!(owner.entries.len(), 1);
        history.click();
        let clicks = owner.reconcile();
        assert_eq!(clicks.len(), 1);
        assert_eq!(clicks[0].1.request_id, "0");
        history.click();
        assert!(owner.reconcile().is_empty());
        assert!(owner.entries.is_empty());
    }

    #[test]
    fn notification_cancelled_session_retires_and_late_callback_cannot_activate() {
        let mut owner = registry();
        let first = post(0);
        let cancel = first.cancel.clone();
        owner.post(first).unwrap();
        let delayed = owner.backend.callbacks["0"].clone();
        cancel.cancel();
        delayed.click();
        assert!(owner.reconcile().is_empty());
        assert!(owner.entries.is_empty());
        assert!(owner.backend.callbacks.is_empty());
        delayed.click();
        assert!(owner.reconcile().is_empty());
    }

    #[test]
    fn notification_post_failure_does_not_retain_registration() {
        let mut owner = registry();
        owner.backend.fail_post = true;
        assert!(owner.post(post(0)).is_err());
        assert!(owner.entries.is_empty());
        assert!(owner.reconcile().is_empty());
    }

    #[test]
    fn notification_click_is_delivered_once_even_if_os_cleanup_temporarily_fails() {
        let mut owner = registry();
        owner.post(post(0)).unwrap();
        owner.backend.fail_remove = true;
        owner.backend.callbacks["0"].click();
        assert_eq!(owner.reconcile().len(), 1);
        assert_eq!(owner.entries.len(), 1);
        assert!(owner.reconcile().is_empty());
        owner.backend.fail_remove = false;
        assert!(owner.reconcile().is_empty());
        assert!(owner.entries.is_empty());
    }

    #[test]
    fn notification_shutdown_retires_os_items_before_backend_and_callback_drop() {
        let mut owner = registry();
        owner.post(post(0)).unwrap();
        let log = owner.backend.log.clone();
        drop(owner);
        assert_eq!(*log.lock().unwrap(), ["remove:0", "drop:0"]);
    }

    #[test]
    fn notification_click_order_is_not_post_order_even_when_wake_queue_is_full() {
        let mut owner = registry();
        let (wake, _receiver) = mpsc::sync_channel(1);
        owner.wake = wake;
        owner.wake.try_send(Command::Wake).unwrap();
        owner.post(post(0)).unwrap();
        owner.post(post(1)).unwrap();
        owner.backend.callbacks["1"].click();
        owner.backend.callbacks["0"].click();
        let clicks = owner.reconcile();
        assert_eq!(
            clicks
                .iter()
                .map(|(_, value, _)| value.request_id.as_str())
                .collect::<Vec<_>>(),
            ["1", "0"]
        );
        assert!(clicks[0].0 < clicks[1].0);
    }
}
