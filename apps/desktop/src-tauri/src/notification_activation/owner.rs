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
// Terminal events occupy the low bits; the sequence sits above them. RETIRE may be OR-ed
// onto CLICK or COPY after delivery so cleanup retries never redeliver the same event.
const CLICK: u64 = 1;
const RETIRE: u64 = 2;
const COPY: u64 = 4;
const REVEAL: u64 = 8;
const REPLY: u64 = 16;
const EVENT_MASK: u64 = 0b1_1111;
const SEQUENCE_SHIFT: u32 = 5;

pub struct Post {
    pub activation: Option<NotificationActivation>,
    pub cancel: CancellationToken,
    pub title: String,
    pub body: String,
    pub sound: bool,
    pub icon: Option<crate::notification_icon::Icon>,
    /// Copy target for the popup's Copy action. The OS carries only the post id.
    pub copy: Option<OneTimeCodeTarget>,
    /// Optional native-only action capability. The callback owns any host reservation.
    pub popup_actions: Option<PopupActions>,
}

/// Identifies one presentation of a logical notification request. A replacement
/// must present a fresh generation; callbacks for an earlier generation fail
/// closed even when the OS reuses the request tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PopupVersion {
    pub request_id: String,
    pub presentation_generation: u64,
}

#[derive(Clone)]
pub struct Signal {
    event: Arc<AtomicU64>,
    popup_action: Arc<Mutex<Option<PopupAction>>>,
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
    pub fn copy(&self) {
        self.set(COPY);
    }
    pub fn reveal(&self) {
        self.set_popup(REVEAL, PopupAction::Reveal);
    }
    pub fn reply(&self, body: String) {
        if body.len() > MAX_REPLY_BYTES || body.trim().is_empty() {
            return;
        }
        self.set_popup(REPLY, PopupAction::Reply(body));
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
            (sequence << SEQUENCE_SHIFT) | event,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let _ = self.wake.try_send(Command::Wake);
    }
    fn set_popup(&self, event: u64, action: PopupAction) {
        let mut popup_action = self.popup_action.lock().unwrap();
        let sequence = self.sequence.fetch_add(1, Ordering::AcqRel) + 1;
        if self
            .event
            .compare_exchange(
                0,
                (sequence << SEQUENCE_SHIFT) | event,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            *popup_action = Some(action);
            let _ = self.wake.try_send(Command::Wake);
        }
    }
}

enum Command {
    Post(Box<Post>, PopupVersion, SyncSender<Result<(), PostError>>),
    Replace(
        PopupVersion,
        Box<Post>,
        PopupVersion,
        SyncSender<Result<(), PostError>>,
    ),
    Wake,
}

/// Only a bounded background banner drain waits here, never the UI or an OS
/// callback. Dropping/timing out a receipt cancels this post, not its Session.
pub struct Acceptance {
    result: mpsc::Receiver<Result<(), PostError>>,
    cancel: CancellationToken,
    wake: SyncSender<Command>,
    stopped: Arc<AtomicBool>,
    accepted: bool,
    version: PopupVersion,
}

impl Acceptance {
    pub(crate) fn version(&self) -> PopupVersion {
        self.version.clone()
    }

    /// Lets callers use the same acceptance chain for an initial post and a
    /// versioned replacement after the latter has been admitted synchronously.
    pub(crate) fn and_then<T>(
        self,
        wait: impl FnOnce(Self) -> Result<T, PostError>,
    ) -> Result<T, PostError> {
        wait(self)
    }

    pub fn wait(self) -> Result<(), PostError> {
        self.wait_for(ACCEPTANCE_TIMEOUT)
    }

    fn wait_for(mut self, timeout: std::time::Duration) -> Result<(), PostError> {
        let result =
            await_platform_acceptance(&self.result, timeout).map_err(|_| PostError::Failed)?;
        if self.cancel.is_cancelled() || self.stopped.load(Ordering::Acquire) {
            return Err(PostError::Failed);
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

pub(super) fn await_platform_acceptance<E>(
    result: &mpsc::Receiver<Result<(), E>>,
    timeout: std::time::Duration,
) -> Result<Result<(), E>, ()> {
    result.recv_timeout(timeout).map_err(|_| ())
}

pub struct Owner {
    sender: SyncSender<Command>,
    stopped: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
    generations: AtomicU64,
}

struct Entry<T> {
    version: PopupVersion,
    revealing: bool,
    activation: Option<NotificationActivation>,
    cancel: CancellationToken,
    signal: Signal,
    copy: Option<OneTimeCodeTarget>,
    popup_actions: Option<PopupActions>,
    handle: T,
}

type DeliveredPopupAction = (Arc<dyn Fn(PopupAction) + Send + Sync>, PopupAction);
type DeliveredNotification = (
    u64,
    NotificationActivation,
    CancellationToken,
    Option<OneTimeCodeTarget>,
    Option<DeliveredPopupAction>,
);

trait Backend {
    type Item;
    fn ensure_authorized(&mut self) -> Result<(), PostError>;
    fn ensure_replacement_authorized(&mut self) -> Result<(), PostError> {
        self.ensure_authorized()
    }
    #[allow(clippy::too_many_arguments)]
    fn post(
        &mut self,
        id: &str,
        presentation_generation: u64,
        title: &str,
        body: &str,
        sound: bool,
        icon: Option<&crate::notification_icon::Icon>,
        actions: ActionAvailability,
        signal: Signal,
    ) -> Result<Self::Item, PostError>;
    fn remove(&mut self, item: &Self::Item) -> bool;
    /// Drops callbacks for an item superseded by a freshly posted replacement.
    /// Windows must not remove history for this path because its tag is shared.
    fn retire_replaced(&mut self, item: &Self::Item) -> bool {
        self.remove(item)
    }
}

impl Backend for platform::Backend {
    type Item = platform::Item;
    #[allow(clippy::too_many_arguments)]
    fn post(
        &mut self,
        id: &str,
        presentation_generation: u64,
        title: &str,
        body: &str,
        sound: bool,
        icon: Option<&crate::notification_icon::Icon>,
        actions: ActionAvailability,
        signal: Signal,
    ) -> Result<Self::Item, PostError> {
        platform::Backend::post(
            self,
            id,
            presentation_generation,
            title,
            body,
            sound,
            icon,
            actions,
            signal,
        )
    }
    fn ensure_authorized(&mut self) -> Result<(), PostError> {
        platform::Backend::ensure_authorized(self)
    }
    fn ensure_replacement_authorized(&mut self) -> Result<(), PostError> {
        // A replacement follows an authorized post. Skip the blocking settings
        // read on the owner thread; revocation still fails the native add.
        #[cfg(target_os = "macos")]
        {
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.ensure_authorized()
        }
    }
    fn remove(&mut self, item: &Self::Item) -> bool {
        self.remove(item)
    }
    fn retire_replaced(&mut self, item: &Self::Item) -> bool {
        platform::Backend::retire_replaced(self, item)
    }
}

struct Registry<B: Backend> {
    backend: B,
    entries: VecDeque<Entry<B::Item>>,
    sequence: Arc<AtomicU64>,
    wake: SyncSender<Command>,
}

impl<B: Backend> Registry<B> {
    #[cfg(test)]
    fn post(&mut self, post: Post) -> Result<(), PostError> {
        let request_id = post.activation.as_ref().map_or_else(
            || uuid::Uuid::new_v4().simple().to_string(),
            |activation| activation.request_id.clone(),
        );
        self.post_versioned(
            post,
            PopupVersion {
                request_id,
                presentation_generation: 1,
            },
        )
    }

    fn post_versioned(&mut self, post: Post, version: PopupVersion) -> Result<(), PostError> {
        self.post_versioned_with_authorization(post, version, false)
    }

    fn post_versioned_with_authorization(
        &mut self,
        post: Post,
        version: PopupVersion,
        replacement: bool,
    ) -> Result<(), PostError> {
        if post.cancel.is_cancelled() {
            return Err(PostError::Failed);
        }
        if replacement {
            self.backend.ensure_replacement_authorized()?;
        } else {
            self.backend.ensure_authorized()?;
        }
        if self.entries.len() == CAPACITY {
            if !self.backend.remove(&self.entries.front().unwrap().handle) {
                return Err(PostError::Failed);
            }
            self.entries.pop_front();
        }
        let signal = Signal {
            event: Arc::new(AtomicU64::new(0)),
            popup_action: Arc::new(Mutex::new(None)),
            sequence: self.sequence.clone(),
            wake: self.wake.clone(),
        };
        let handle = self.backend.post(
            &version.request_id,
            version.presentation_generation,
            &post.title,
            &post.body,
            post.sound,
            post.icon.as_ref(),
            ActionAvailability {
                copy: post.copy.is_some(),
                reveal: post
                    .popup_actions
                    .as_ref()
                    .is_some_and(|actions| actions.enabled() && actions.reveal),
                reply: post
                    .popup_actions
                    .as_ref()
                    .is_some_and(|actions| actions.enabled() && actions.reply),
            },
            signal.clone(),
        )?;
        self.entries.push_back(Entry {
            version,
            revealing: false,
            activation: post.activation,
            cancel: post.cancel,
            signal,
            copy: post.copy,
            popup_actions: post.popup_actions,
            handle,
        });
        Ok(())
    }

    fn replace(
        &mut self,
        expected: &PopupVersion,
        post: Post,
        replacement: PopupVersion,
    ) -> Result<(), PostError> {
        if replacement.request_id != expected.request_id
            || replacement.presentation_generation <= expected.presentation_generation
        {
            return Err(PostError::Failed);
        }
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.version == *expected)
        else {
            return Err(PostError::Failed);
        };
        let old = self
            .entries
            .remove(index)
            .expect("replacement entry exists");
        if let Err(error) = self.post_versioned_with_authorization(post, replacement, true) {
            self.entries.insert(index, old);
            return Err(error);
        }
        let fresh = self.entries.pop_back().expect("replacement was posted");
        self.entries.insert(index, fresh);
        // The fresh entry was registered before retiring old callbacks. Never run
        // ordinary cleanup here: it may delete the replacement sharing this tag.
        let _ = self.backend.retire_replaced(&old.handle);
        Ok(())
    }

    /// Returns each delivered terminal event in sequence order. `None` is a click
    /// (navigation); `Some(target)` is a Copy action that must never navigate.
    fn reconcile(&mut self) -> Vec<DeliveredNotification> {
        let mut delivered = Vec::new();
        let mut index = 0;
        while index < self.entries.len() {
            let entry = &mut self.entries[index];
            if entry.revealing && !entry.cancel.is_cancelled() {
                index += 1;
                continue;
            }
            let event = entry.signal.event.load(Ordering::Acquire);
            if event != 0 || entry.cancel.is_cancelled() {
                if !entry.cancel.is_cancelled() {
                    if let Some((copy, popup_action)) = delivery(event, &entry.copy, &entry.signal)
                    {
                        let is_reveal = matches!(popup_action, Some(PopupAction::Reveal));
                        if is_reveal {
                            entry.revealing = true;
                        }
                        if let Some(activation) = &entry.activation {
                            delivered.push((
                                event >> SEQUENCE_SHIFT,
                                activation.clone(),
                                entry.cancel.clone(),
                                copy,
                                popup_action.and_then(|action| {
                                    entry
                                        .popup_actions
                                        .as_ref()
                                        .map(|actions| (actions.handler.clone(), action))
                                }),
                            ));
                        }
                        // Cleanup failure must not suppress a genuine click or copy or replay
                        // it on the next wake. Keep its registration until removal succeeds.
                        entry.signal.event.store(event | RETIRE, Ordering::Release);
                        if is_reveal {
                            index += 1;
                            continue;
                        }
                    }
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
        delivered.sort_by_key(|(sequence, _, _, _, _)| *sequence);
        delivered
    }
}

/// Maps a stored terminal event to its delivery. A retired event carries RETIRE in the
/// mask, so it matches neither arm and is never delivered as a click or a copy.
fn delivery(
    event: u64,
    copy: &Option<OneTimeCodeTarget>,
    signal: &Signal,
) -> Option<(Option<OneTimeCodeTarget>, Option<PopupAction>)> {
    match event & EVENT_MASK {
        CLICK => Some((None, Some(PopupAction::Open))),
        COPY => copy
            .clone()
            .map(|target| (Some(target), Some(PopupAction::Copy))),
        REVEAL | REPLY => signal
            .popup_action
            .lock()
            .unwrap()
            .take()
            .map(|action| (None, Some(action))),
        _ => None,
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
    mut activate: impl FnMut(u64, NotificationActivation, CancellationToken, Option<OneTimeCodeTarget>),
) {
    while let Ok(command) = receiver.recv() {
        if stop.load(Ordering::Acquire) {
            break;
        }
        for (sequence, activation, cancel, copy, popup_action) in registry.reconcile() {
            let activates_existing_flow = popup_action
                .as_ref()
                .is_none_or(|(_, action)| matches!(action, PopupAction::Open | PopupAction::Copy));
            if let Some((handler, action)) = popup_action {
                let _ = std::thread::Builder::new()
                    .name("notification-popup-action".into())
                    .spawn(move || handler(action));
            }
            if activates_existing_flow {
                activate(sequence, activation, cancel, copy);
            }
        }
        let (result, reply) = match command {
            Command::Post(post, version, reply) => (registry.post_versioned(*post, version), reply),
            Command::Replace(expected, post, replacement, reply) => {
                (registry.replace(&expected, *post, replacement), reply)
            }
            Command::Wake => continue,
        };
        let _ = reply.try_send(if stop.load(Ordering::Acquire) {
            Err(PostError::Failed)
        } else {
            result
        });
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
                serve(
                    registry,
                    receiver,
                    stop,
                    |sequence, activation, cancel, copy| {
                        super::activate(&app, activation, &cancel, sequence, copy)
                    },
                );
            })
            .ok()?;
        Some(Self {
            sender,
            stopped,
            thread,
            generations: AtomicU64::new(0),
        })
    }

    pub fn submit(&self, mut post: Post) -> Result<Acceptance, PostError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(PostError::Failed);
        }
        post.cancel = post.cancel.child_token();
        let version = PopupVersion {
            request_id: post.activation.as_ref().map_or_else(
                || uuid::Uuid::new_v4().simple().to_string(),
                |activation| activation.request_id.clone(),
            ),
            presentation_generation: self.generations.fetch_add(1, Ordering::AcqRel) + 1,
        };
        let (reply, result) = mpsc::sync_channel(1);
        let acceptance = Acceptance {
            result,
            cancel: post.cancel.clone(),
            wake: self.sender.clone(),
            stopped: self.stopped.clone(),
            accepted: false,
            version: version.clone(),
        };
        self.sender
            .try_send(Command::Post(Box::new(post), version, reply))
            .map_err(|_| PostError::Failed)?;
        Ok(acceptance)
    }

    pub(crate) fn replace(
        &self,
        expected: PopupVersion,
        mut post: Post,
    ) -> Result<Acceptance, PostError> {
        if self.stopped.load(Ordering::Acquire)
            || post
                .activation
                .as_ref()
                .is_none_or(|activation| activation.request_id != expected.request_id)
        {
            return Err(PostError::Failed);
        }
        post.cancel = post.cancel.child_token();
        let replacement = PopupVersion {
            request_id: expected.request_id.clone(),
            presentation_generation: self.generations.fetch_add(1, Ordering::AcqRel) + 1,
        };
        let (reply, result) = mpsc::sync_channel(1);
        let acceptance = Acceptance {
            result,
            cancel: post.cancel.clone(),
            wake: self.sender.clone(),
            stopped: self.stopped.clone(),
            accepted: false,
            version: replacement.clone(),
        };
        self.sender
            .try_send(Command::Replace(
                expected,
                Box::new(post),
                replacement,
                reply,
            ))
            .map_err(|_| PostError::Failed)?;
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
    use std::sync::atomic::AtomicUsize;

    type BackendAcceptanceGate = (SyncSender<()>, mpsc::Receiver<Result<(), ()>>);

    #[derive(Default)]
    struct TestBackend {
        callbacks: HashMap<String, Signal>,
        sounds: Vec<bool>,
        copy_actions: Vec<bool>,
        popup_actions: Vec<ActionAvailability>,
        titles: Vec<String>,
        bodies: Vec<String>,
        icons: Vec<&'static str>,
        log: Arc<Mutex<Vec<String>>>,
        fail_post: bool,
        post_outcomes: VecDeque<Result<(), PostError>>,
        fail_remove: bool,
        reject_after: Option<usize>,
        acceptance: Option<BackendAcceptanceGate>,
        retired: Option<SyncSender<()>>,
        lease_root: Option<std::path::PathBuf>,
        leases: Vec<std::path::PathBuf>,
        post_attempts: Arc<AtomicUsize>,
        lease_paths: Arc<Mutex<Vec<std::path::PathBuf>>>,
        callback_leases: Option<Arc<Mutex<HashMap<String, crate::notification_icon::Lease>>>>,
        posted_signals: Option<SyncSender<Signal>>,
        full_authorization_checks: usize,
        replacement_authorization_checks: usize,
        replacement_authorization_error: Option<PostError>,
    }
    struct TestItem {
        id: String,
        log: Arc<Mutex<Vec<String>>>,
        _icon: Option<crate::notification_icon::Lease>,
    }
    impl Drop for TestItem {
        fn drop(&mut self) {
            self.log.lock().unwrap().push(format!("drop:{}", self.id));
        }
    }
    impl Backend for TestBackend {
        type Item = TestItem;
        fn ensure_authorized(&mut self) -> Result<(), PostError> {
            self.full_authorization_checks += 1;
            Ok(())
        }
        fn ensure_replacement_authorized(&mut self) -> Result<(), PostError> {
            self.replacement_authorization_checks += 1;
            self.replacement_authorization_error.map_or(Ok(()), Err)
        }
        #[allow(clippy::too_many_arguments)]
        fn post(
            &mut self,
            id: &str,
            _: u64,
            title: &str,
            body: &str,
            sound: bool,
            icon: Option<&crate::notification_icon::Icon>,
            actions: ActionAvailability,
            signal: Signal,
        ) -> Result<TestItem, PostError> {
            self.post_attempts.fetch_add(1, Ordering::AcqRel);
            self.copy_actions.push(actions.copy);
            self.popup_actions.push(actions);
            self.titles.push(title.into());
            self.bodies.push(body.into());
            self.sounds.push(sound);
            self.icons.push(match icon {
                None => "none",
                Some(crate::notification_icon::Icon::Message) => "message",
                Some(crate::notification_icon::Icon::App(_)) => "app",
            });
            if let Some(outcome) = self.post_outcomes.pop_front() {
                outcome?;
            }
            if self.fail_post || self.reject_after == Some(self.callbacks.len()) {
                signal.retire();
                return Err(PostError::Failed);
            }
            if let Some((started, result)) = self.acceptance.take() {
                started.send(()).unwrap();
                await_platform_acceptance(&result, PLATFORM_TIMEOUT)
                    .map_err(|_| PostError::Failed)?
                    .map_err(|_| PostError::Failed)?;
            }
            let icon = self.lease_root.as_ref().and_then(|root| {
                icon.and_then(|icon| crate::notification_icon::materialize_for_test(root, icon))
            });
            if let Some(icon) = &icon {
                self.leases.push(icon.path().to_path_buf());
                self.lease_paths
                    .lock()
                    .unwrap()
                    .push(icon.path().to_path_buf());
                if let Some(callback_leases) = &self.callback_leases {
                    callback_leases
                        .lock()
                        .unwrap()
                        .insert(id.into(), icon.clone());
                }
            }
            self.callbacks.insert(id.into(), signal);
            if let Some(posted_signals) = &self.posted_signals {
                let _ = posted_signals.try_send(self.callbacks[id].clone());
            }
            Ok(TestItem {
                id: id.into(),
                log: self.log.clone(),
                _icon: icon,
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
            activation: Some(NotificationActivation {
                request_id: id.to_string(),
                session_generation: "instance".into(),
                destination: Destination::Main,
            }),
            cancel: CancellationToken::new(),
            title: "not retained".into(),
            body: "not retained".into(),
            sound: false,
            icon: Some(crate::notification_icon::Icon::Message),
            copy: None,
            popup_actions: None,
        }
    }

    fn popup_post(id: usize, handler: Arc<dyn Fn(PopupAction) + Send + Sync>) -> Post {
        let mut post = post(id);
        post.popup_actions = Some(PopupActions {
            reveal: true,
            reply: false,
            handler,
        });
        post
    }

    #[test]
    fn synthetic_test_presentations_have_unique_identities_and_terminal_cleanup() {
        let mut registry = registry();
        let full = super::super::test_notification::presentation(
            super::super::test_notification::TestPrivacy::Full,
        );
        let hidden = super::super::test_notification::presentation(
            super::super::test_notification::TestPrivacy::Hidden,
        );
        let first_id = super::super::test_notification::request_id();
        let second_id = super::super::test_notification::request_id();
        assert_ne!(first_id, second_id);
        let delivered = Arc::new(AtomicUsize::new(0));
        for (id, presentation) in [(&first_id, full), (&second_id, hidden)] {
            let delivered = delivered.clone();
            let post = Post {
                activation: Some(NotificationActivation {
                    request_id: id.clone(),
                    session_generation: "synthetic".into(),
                    destination: Destination::Main,
                }),
                cancel: CancellationToken::new(),
                title: presentation.title.into(),
                body: presentation.body.into(),
                sound: false,
                icon: Some(crate::notification_icon::Icon::Message),
                copy: None,
                popup_actions: Some(PopupActions {
                    reveal: presentation.reveal,
                    reply: !presentation.reveal,
                    handler: Arc::new(move |_| {
                        delivered.fetch_add(1, Ordering::AcqRel);
                    }),
                }),
            };
            registry
                .post_versioned(
                    post,
                    PopupVersion {
                        request_id: id.clone(),
                        presentation_generation: 1,
                    },
                )
                .unwrap();
        }
        assert_eq!(
            registry.backend.titles,
            vec!["Peppy test message", "New notification"]
        );
        assert_eq!(
            registry.backend.bodies,
            vec!["This is a test message. Reply here to try quick reply.", ""]
        );
        assert!(!registry.backend.popup_actions[0].copy);
        assert!(!registry.backend.popup_actions[0].reveal);
        assert_eq!(
            registry.backend.popup_actions[0].reply,
            cfg!(feature = "notification-quick-reply")
        );
        assert!(!registry.backend.popup_actions[1].copy);
        assert_eq!(
            registry.backend.popup_actions[1].reveal,
            cfg!(feature = "notification-quick-reply")
        );
        assert!(!registry.backend.popup_actions[1].reply);

        registry.entries[0]
            .signal
            .reply("ignored test reply".into());
        registry.entries[1].signal.reveal();
        for (_, _, _, _, action) in registry.reconcile() {
            let (handler, action) = action.unwrap();
            handler(action);
        }
        assert_eq!(delivered.load(Ordering::Acquire), 2);
        assert_eq!(
            registry.entries.len(),
            1,
            "Reveal keeps the hidden version for replacement"
        );

        let replacement = PopupVersion {
            request_id: second_id.clone(),
            presentation_generation: 2,
        };
        let delivered_after_reveal = delivered.clone();
        registry
            .replace(
                &PopupVersion {
                    request_id: second_id.clone(),
                    presentation_generation: 1,
                },
                Post {
                    activation: Some(NotificationActivation {
                        request_id: second_id,
                        session_generation: "synthetic".into(),
                        destination: Destination::Main,
                    }),
                    cancel: CancellationToken::new(),
                    title: full.title.into(),
                    body: full.body.into(),
                    sound: false,
                    icon: Some(crate::notification_icon::Icon::Message),
                    copy: None,
                    popup_actions: Some(PopupActions {
                        reveal: false,
                        reply: true,
                        handler: Arc::new(move |_| {
                            delivered_after_reveal.fetch_add(1, Ordering::AcqRel);
                        }),
                    }),
                },
                replacement,
            )
            .unwrap();
        registry.entries[0].signal.reply("ignored again".into());
        for (_, _, _, _, action) in registry.reconcile() {
            let (handler, action) = action.unwrap();
            handler(action);
        }
        assert_eq!(delivered.load(Ordering::Acquire), 3);
        assert!(
            registry.entries.is_empty(),
            "terminal synthetic popups must retire"
        );
    }

    #[test]
    fn replacement_fences_old_callback_and_preserves_fresh_generation() {
        let delivered = Arc::new(AtomicUsize::new(0));
        let first_delivered = delivered.clone();
        let second_delivered = delivered.clone();
        let mut registry = registry();
        let first = PopupVersion {
            request_id: "message".into(),
            presentation_generation: 1,
        };
        registry
            .post_versioned(
                popup_post(
                    1,
                    Arc::new(move |_| {
                        first_delivered.fetch_add(1, Ordering::AcqRel);
                    }),
                ),
                first.clone(),
            )
            .unwrap();
        let stale = registry.entries[0].signal.clone();
        let fresh = PopupVersion {
            request_id: "message".into(),
            presentation_generation: 2,
        };
        registry
            .replace(
                &first,
                popup_post(
                    1,
                    Arc::new(move |_| {
                        second_delivered.fetch_add(1, Ordering::AcqRel);
                    }),
                ),
                fresh,
            )
            .unwrap();

        stale.reveal();
        assert!(registry.reconcile().is_empty());
        registry.entries[0].signal.reveal();
        let delivered_actions = registry.reconcile();
        assert_eq!(delivered_actions.len(), 1);
        let (handler, action) = delivered_actions[0].4.clone().unwrap();
        handler(action);
        assert_eq!(delivered.load(Ordering::Acquire), 1);
    }

    #[test]
    fn replacement_uses_replacement_authorization_check() {
        let mut registry = registry();
        let initial = PopupVersion {
            request_id: "message".into(),
            presentation_generation: 1,
        };
        registry.post_versioned(post(1), initial.clone()).unwrap();

        registry
            .replace(
                &initial,
                post(1),
                PopupVersion {
                    request_id: "message".into(),
                    presentation_generation: 2,
                },
            )
            .unwrap();

        assert_eq!(registry.backend.full_authorization_checks, 1);
        assert_eq!(registry.backend.replacement_authorization_checks, 1);
    }

    #[test]
    fn initial_post_uses_full_authorization_check() {
        let mut registry = registry();

        registry.post(post(1)).unwrap();

        assert_eq!(registry.backend.full_authorization_checks, 1);
        assert_eq!(registry.backend.replacement_authorization_checks, 0);
    }

    #[test]
    fn replacement_not_allowed_restores_old_entry() {
        let mut registry = registry();
        let initial = PopupVersion {
            request_id: "message".into(),
            presentation_generation: 1,
        };
        registry.post_versioned(post(1), initial.clone()).unwrap();
        registry.backend.replacement_authorization_error = Some(PostError::NotAllowed);

        assert_eq!(
            registry.replace(
                &initial,
                post(1),
                PopupVersion {
                    request_id: "message".into(),
                    presentation_generation: 2,
                },
            ),
            Err(PostError::NotAllowed)
        );
        assert_eq!(registry.entries.len(), 1);
        assert_eq!(registry.entries[0].version, initial);
    }

    #[test]
    fn replacement_native_not_allowed_restores_old_entry_and_callback() {
        let mut registry = registry();
        registry.backend.post_outcomes = VecDeque::from([Ok(()), Err(PostError::NotAllowed)]);
        let initial = PopupVersion {
            request_id: "message".into(),
            presentation_generation: 1,
        };
        registry.post_versioned(post(1), initial.clone()).unwrap();

        assert_eq!(
            registry.replace(
                &initial,
                post(1),
                PopupVersion {
                    request_id: "message".into(),
                    presentation_generation: 2,
                },
            ),
            Err(PostError::NotAllowed)
        );
        assert_eq!(registry.entries.len(), 1);
        assert_eq!(registry.entries[0].version, initial);
        assert_eq!(registry.entries[0].signal.event.load(Ordering::Acquire), 0);
        assert!(registry.backend.callbacks.contains_key("message"));
    }

    #[test]
    fn replacement_rejects_stale_or_same_generation() {
        let mut registry = registry();
        let version = PopupVersion {
            request_id: "message".into(),
            presentation_generation: 3,
        };
        registry
            .post_versioned(popup_post(1, Arc::new(|_| {})), version.clone())
            .unwrap();
        assert_eq!(
            registry.replace(&version, popup_post(1, Arc::new(|_| {})), version.clone()),
            Err(PostError::Failed)
        );
        assert_eq!(registry.entries.len(), 1);
    }

    #[test]
    fn reveal_callback_keeps_its_version_registered_until_replacement() {
        let (posted, signals) = mpsc::sync_channel(1);
        let (actions, received) = mpsc::sync_channel(1);
        let owner = test_owner(
            TestBackend {
                posted_signals: Some(posted),
                ..Default::default()
            },
            0,
        );
        let acceptance = owner
            .submit(popup_post(
                1,
                Arc::new(move |action| actions.send(action).unwrap()),
            ))
            .unwrap();
        let version = acceptance.version();
        acceptance.wait().unwrap();

        signals.recv_timeout(PLATFORM_TIMEOUT).unwrap().reveal();
        assert_eq!(
            received.recv_timeout(PLATFORM_TIMEOUT).unwrap(),
            PopupAction::Reveal
        );
        owner
            .replace(version, popup_post(1, Arc::new(|_| {})))
            .unwrap()
            .wait()
            .unwrap();
        owner.stop();
    }

    #[test]
    fn failed_reveal_lookup_cancellation_retires_owner_and_releases_reservation() {
        let recovery = Arc::new(super::super::recovery_store::RecoveryStore::new(1));
        let reservation = recovery.reserve().unwrap();
        let (posted, signals) = mpsc::sync_channel(1);
        let (revealed, reveal_received) = mpsc::sync_channel(1);
        let (retired, retirement) = mpsc::sync_channel(1);
        let owner = test_owner(
            TestBackend {
                posted_signals: Some(posted),
                retired: Some(retired),
                ..Default::default()
            },
            0,
        );
        let original = popup_post(
            1,
            Arc::new(move |_| {
                let _reservation = &reservation;
                revealed.send(()).unwrap();
            }),
        );
        let cancellation = original.cancel.clone();
        let acceptance = owner.submit(original).unwrap();
        acceptance.wait().unwrap();

        signals.recv_timeout(PLATFORM_TIMEOUT).unwrap().reveal();
        reveal_received.recv_timeout(PLATFORM_TIMEOUT).unwrap();
        cancellation.cancel();
        owner.wake();

        retirement.recv_timeout(PLATFORM_TIMEOUT).unwrap();
        owner.stop();
        assert!(recovery.reserve().is_some());
    }

    #[test]
    fn failed_reveal_replacement_cancellation_retires_owner_and_releases_reservation() {
        let recovery = Arc::new(super::super::recovery_store::RecoveryStore::new(1));
        let reservation = recovery.reserve().unwrap();
        let (posted, signals) = mpsc::sync_channel(1);
        let (revealed, reveal_received) = mpsc::sync_channel(1);
        let (retired, retirement) = mpsc::sync_channel(1);
        let owner = test_owner(
            TestBackend {
                post_outcomes: VecDeque::from([Ok(()), Err(PostError::Failed)]),
                posted_signals: Some(posted),
                retired: Some(retired),
                ..Default::default()
            },
            0,
        );
        let original = popup_post(
            1,
            Arc::new(move |_| {
                let _reservation = &reservation;
                revealed.send(()).unwrap();
            }),
        );
        let cancellation = original.cancel.clone();
        let acceptance = owner.submit(original).unwrap();
        let version = acceptance.version();
        acceptance.wait().unwrap();

        signals.recv_timeout(PLATFORM_TIMEOUT).unwrap().reveal();
        reveal_received.recv_timeout(PLATFORM_TIMEOUT).unwrap();
        assert!(owner
            .replace(version, popup_post(1, Arc::new(|_| {})))
            .unwrap()
            .wait()
            .is_err());

        cancellation.cancel();
        owner.wake();
        retirement.recv_timeout(PLATFORM_TIMEOUT).unwrap();
        owner.stop();
        assert!(recovery.reserve().is_some());
    }

    #[test]
    fn replacement_at_capacity_preserves_the_replaced_slot_and_rolls_back_on_failure() {
        for index in [0, CAPACITY / 2, CAPACITY - 1] {
            let mut registry = registry();
            for id in 0..CAPACITY {
                registry.post(post(id)).unwrap();
            }
            let expected = registry.entries[index].version.clone();
            let replacement = PopupVersion {
                request_id: expected.request_id.clone(),
                presentation_generation: 2,
            };
            registry
                .replace(&expected, post(index), replacement.clone())
                .unwrap();
            assert_eq!(registry.entries.len(), CAPACITY);
            assert_eq!(registry.entries[index].version, replacement);

            registry.backend.fail_post = true;
            let expected = registry.entries[index].version.clone();
            assert_eq!(
                registry.replace(
                    &expected,
                    post(index),
                    PopupVersion {
                        request_id: expected.request_id.clone(),
                        presentation_generation: 3,
                    },
                ),
                Err(PostError::Failed)
            );
            assert_eq!(registry.entries.len(), CAPACITY);
            assert_eq!(registry.entries[index].version, expected);
        }
    }

    #[test]
    fn notification_anonymous_test_waits_for_native_acceptance_without_session() {
        for outcome in [Ok(()), Err(())] {
            let (started, observing) = mpsc::sync_channel(1);
            let (complete, result) = mpsc::sync_channel(1);
            let owner = test_owner(
                TestBackend {
                    acceptance: Some((started, result)),
                    ..Default::default()
                },
                0,
            );
            let receipt = owner.submit(anonymous_post()).unwrap();
            observing.recv_timeout(PLATFORM_TIMEOUT).unwrap();
            assert!(matches!(
                receipt.result.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));
            complete.send(outcome).unwrap();
            assert_eq!(receipt.wait().map_err(|_| ()), outcome);
            owner.stop();
        }
    }

    fn anonymous_post() -> Post {
        Post {
            activation: None,
            cancel: CancellationToken::new(),
            title: "Peppy".into(),
            body: "Test notification".into(),
            sound: true,
            icon: Some(crate::notification_icon::Icon::Message),
            copy: None,
            popup_actions: None,
        }
    }

    #[test]
    fn notification_anonymous_timeout_cleans_up_late_native_acceptance() {
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
        let receipt = owner.submit(anonymous_post()).unwrap();
        observing.recv_timeout(PLATFORM_TIMEOUT).unwrap();
        assert!(receipt.wait_for(std::time::Duration::ZERO).is_err());
        complete.send(Ok(())).unwrap();
        retirement.recv_timeout(PLATFORM_TIMEOUT).unwrap();
        owner.stop();
    }

    #[test]
    fn notification_anonymous_test_has_unique_ids_and_never_navigates() {
        let mut registry = registry();
        registry.post(anonymous_post()).unwrap();
        registry.post(anonymous_post()).unwrap();
        assert_eq!(registry.backend.callbacks.len(), 2);
        assert_eq!(registry.backend.sounds, [true, true]);
        for (id, callback) in &registry.backend.callbacks {
            assert!(uuid::Uuid::parse_str(id).is_ok());
            callback.click();
        }
        registry.backend.fail_remove = true;
        assert!(registry.reconcile().is_empty());
        assert_eq!(registry.entries.len(), 2);
        registry.backend.fail_remove = false;
        assert!(registry.reconcile().is_empty());
        assert!(registry.entries.is_empty());
        assert!(registry.backend.callbacks.is_empty());
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

    #[test]
    fn notification_icon_choice_reaches_the_backend_without_changing_delivery() {
        let mut registry = registry();
        let mut app_icon = post(0);
        app_icon.icon = Some(crate::notification_icon::Icon::App(vec![1, 2, 3]));
        registry.post(app_icon).unwrap();
        assert_eq!(registry.backend.icons, ["app"]);
        assert_eq!(registry.entries.len(), 1);
    }

    #[test]
    fn notification_icon_lease_is_released_when_eviction_retires_a_real_owner_item() {
        let directory = tempfile::tempdir().unwrap();
        let mut registry = Registry {
            backend: TestBackend {
                lease_root: Some(directory.path().to_path_buf()),
                ..Default::default()
            },
            entries: VecDeque::new(),
            sequence: Arc::new(AtomicU64::new(0)),
            wake: mpsc::sync_channel(1).0,
        };
        registry.post(post(0)).unwrap();
        let first = registry.backend.leases[0].clone();
        assert!(first.exists());
        for id in 1..=CAPACITY {
            registry.post(post(id)).unwrap();
        }
        assert!(!first.exists());
    }

    #[test]
    fn notification_failed_eviction_removal_retains_the_real_owner_lease() {
        let directory = tempfile::tempdir().unwrap();
        let mut registry = Registry {
            backend: TestBackend {
                lease_root: Some(directory.path().to_path_buf()),
                ..Default::default()
            },
            entries: VecDeque::new(),
            sequence: Arc::new(AtomicU64::new(0)),
            wake: mpsc::sync_channel(1).0,
        };
        for id in 0..CAPACITY {
            registry.post(post(id)).unwrap();
        }
        let first = registry.backend.leases[0].clone();
        registry.backend.fail_remove = true;

        assert_eq!(registry.post(post(CAPACITY)), Err(PostError::Failed));
        assert!(first.exists());
        assert_eq!(registry.entries.len(), CAPACITY);
    }

    #[test]
    fn notification_icon_write_failure_does_not_retry_or_block_native_submission() {
        let directory = tempfile::tempdir().unwrap();
        let root_file = directory.path().join("not-a-directory");
        std::fs::write(&root_file, b"file").unwrap();
        let mut registry = Registry {
            backend: TestBackend {
                lease_root: Some(root_file),
                ..Default::default()
            },
            entries: VecDeque::new(),
            sequence: Arc::new(AtomicU64::new(0)),
            wake: mpsc::sync_channel(1).0,
        };
        registry.post(post(0)).unwrap();
        assert_eq!(registry.entries.len(), 1);
        assert!(registry.backend.leases.is_empty());
        assert_eq!(registry.backend.post_attempts.load(Ordering::Acquire), 1);
    }

    #[test]
    fn notification_icon_write_failure_posts_once_and_acknowledges_the_candidate() {
        let (_fixture, session, settings) = durable_candidates(1);
        let directory = tempfile::tempdir().unwrap();
        let root_file = directory.path().join("not-a-directory");
        std::fs::write(&root_file, b"file").unwrap();
        let post_attempts = Arc::new(AtomicUsize::new(0));
        let owner = test_owner(
            TestBackend {
                lease_root: Some(root_file),
                post_attempts: post_attempts.clone(),
                ..Default::default()
            },
            0,
        );

        assert!(!drain_to_owner(&session, &settings, &owner).unwrap());
        owner.stop();
        assert!(session
            .client
            .pending_banner_candidates(100)
            .unwrap()
            .is_empty());
        assert_eq!(post_attempts.load(Ordering::Acquire), 1);
    }

    #[test]
    fn notification_icon_lease_is_released_when_cancellation_retires_the_owner_item() {
        let directory = tempfile::tempdir().unwrap();
        let mut registry = Registry {
            backend: TestBackend {
                lease_root: Some(directory.path().to_path_buf()),
                ..Default::default()
            },
            entries: VecDeque::new(),
            sequence: Arc::new(AtomicU64::new(0)),
            wake: mpsc::sync_channel(1).0,
        };
        let cancelled = post(0);
        let cancellation = cancelled.cancel.clone();
        registry.post(cancelled).unwrap();
        let path = registry.backend.leases[0].clone();
        cancellation.cancel();
        assert!(registry.reconcile().is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn notification_icon_lease_is_released_when_registry_shutdown_retires_owner_items() {
        let directory = tempfile::tempdir().unwrap();
        let path = {
            let mut registry = Registry {
                backend: TestBackend {
                    lease_root: Some(directory.path().to_path_buf()),
                    ..Default::default()
                },
                entries: VecDeque::new(),
                sequence: Arc::new(AtomicU64::new(0)),
                wake: mpsc::sync_channel(1).0,
            };
            registry.post(post(0)).unwrap();
            registry.backend.leases[0].clone()
        };
        assert!(!path.exists());
    }

    #[test]
    fn notification_timeout_late_acceptance_keeps_real_lease_until_callback_releases_it() {
        let directory = tempfile::tempdir().unwrap();
        let (started, observing) = mpsc::sync_channel(1);
        let (complete, result) = mpsc::sync_channel(1);
        let (retired, retirement) = mpsc::sync_channel(1);
        let callback_leases = Arc::new(Mutex::new(HashMap::new()));
        let lease_paths = Arc::new(Mutex::new(Vec::new()));
        let owner = test_owner(
            TestBackend {
                acceptance: Some((started, result)),
                retired: Some(retired),
                lease_root: Some(directory.path().to_path_buf()),
                lease_paths: lease_paths.clone(),
                callback_leases: Some(callback_leases.clone()),
                ..Default::default()
            },
            0,
        );
        let receipt = owner.submit(anonymous_post()).unwrap();
        observing.recv_timeout(PLATFORM_TIMEOUT).unwrap();
        assert!(receipt.wait_for(std::time::Duration::ZERO).is_err());

        complete.send(Ok(())).unwrap();
        retirement.recv_timeout(PLATFORM_TIMEOUT).unwrap();
        owner.stop();
        let path = lease_paths.lock().unwrap()[0].clone();
        assert!(path.exists());
        let (_, lease) = callback_leases.lock().unwrap().drain().next().unwrap();
        drop(lease);
        assert!(!path.exists());
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
        let thread = std::thread::spawn(move || serve(registry, receiver, stop, |_, _, _, _| {}));
        Owner {
            sender,
            stopped,
            thread,
            generations: AtomicU64::new(0),
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
            |destination, title, body, icon, _copy| {
                owner
                    .submit(Post {
                        activation: Some(NotificationActivation {
                            request_id: uuid::Uuid::new_v4().to_string(),
                            session_generation: session.generation.clone(),
                            destination,
                        }),
                        cancel: session.cancel.clone(),
                        sound: settings.sound_enabled(),
                        title,
                        body,
                        icon,
                        copy: None,
                        popup_actions: None,
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
    fn notification_drain_not_allowed_acknowledges_and_posts_the_next_candidate() {
        let (_fixture, session, settings) = durable_candidates(2);
        let owner = test_owner(
            TestBackend {
                post_outcomes: [Err(PostError::NotAllowed), Ok(())].into(),
                ..Default::default()
            },
            0,
        );
        assert!(!drain_to_owner(&session, &settings, &owner).unwrap());
        owner.stop();
        assert!(session
            .client
            .pending_banner_candidates(100)
            .unwrap()
            .is_empty());
        assert_eq!(session.status.lock().unwrap().work_error, None);
    }

    #[test]
    fn notification_drain_awaiting_authorization_holds_candidate_without_error() {
        let (_fixture, session, settings) = durable_candidates(1);
        assert!(!crate::notifications::drain_with_delivery(
            &session,
            &settings,
            || false,
            |_, _| false,
            |_, _, _, _, _| Err(PostError::AwaitingAuthorization),
        )
        .unwrap());
        assert_eq!(
            session.client.pending_banner_candidates(100).unwrap().len(),
            1
        );
        assert_eq!(session.status.lock().unwrap().work_error, None);
    }

    #[test]
    fn notification_drain_awaiting_authorization_holds_later_candidates() {
        let (_fixture, session, settings) = durable_candidates(2);
        let posts = std::cell::Cell::new(0);
        assert!(!crate::notifications::drain_with_delivery(
            &session,
            &settings,
            || false,
            |_, _| false,
            |_, _, _, _, _| {
                posts.set(posts.get() + 1);
                Err(PostError::AwaitingAuthorization)
            },
        )
        .unwrap());
        assert_eq!(posts.get(), 1);
        assert_eq!(
            session.client.pending_banner_candidates(100).unwrap().len(),
            2
        );
        assert_eq!(session.status.lock().unwrap().work_error, None);
    }

    #[test]
    fn notification_drain_posts_held_candidate_after_authorization_grants() {
        let (_fixture, session, settings) = durable_candidates(1);
        let await_authorization = || Err(PostError::AwaitingAuthorization);
        assert!(!crate::notifications::drain_with_delivery(
            &session,
            &settings,
            || false,
            |_, _| false,
            |_, _, _, _, _| await_authorization(),
        )
        .unwrap());
        let posted = std::cell::Cell::new(0);
        assert!(!crate::notifications::drain_with_delivery(
            &session,
            &settings,
            || false,
            |_, _| false,
            |_, _, _, _, _| {
                posted.set(posted.get() + 1);
                Ok(())
            },
        )
        .unwrap());
        assert_eq!(posted.get(), 1);
        assert!(session
            .client
            .pending_banner_candidates(100)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn notification_drain_acknowledges_held_candidate_after_authorization_denies() {
        let (_fixture, session, settings) = durable_candidates(1);
        assert!(!crate::notifications::drain_with_delivery(
            &session,
            &settings,
            || false,
            |_, _| false,
            |_, _, _, _, _| Err(PostError::AwaitingAuthorization),
        )
        .unwrap());
        assert!(!crate::notifications::drain_with_delivery(
            &session,
            &settings,
            || false,
            |_, _| false,
            |_, _, _, _, _| Err(PostError::NotAllowed),
        )
        .unwrap());
        assert!(session
            .client
            .pending_banner_candidates(100)
            .unwrap()
            .is_empty());
        assert_eq!(session.status.lock().unwrap().work_error, None);
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
            |destination, title, body, icon, _copy| {
                let receipt = owner.submit(Post {
                    activation: Some(NotificationActivation {
                        request_id: "late-post".into(),
                        session_generation: session.generation.clone(),
                        destination,
                    }),
                    cancel: session.cancel.clone(),
                    sound: settings.sound_enabled(),
                    title,
                    body,
                    icon,
                    copy: None,
                    popup_actions: None,
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
                .map(|(_, value, _, _, _)| value.request_id.as_str())
                .collect::<Vec<_>>(),
            ["1", "0"]
        );
        assert!(clicks[0].0 < clicks[1].0);
    }

    fn copy_target(id: &str) -> OneTimeCodeTarget {
        OneTimeCodeTarget::Message {
            message_id: id.into(),
        }
    }

    fn copy_post(id: usize) -> Post {
        Post {
            copy: Some(copy_target(&id.to_string())),
            ..post(id)
        }
    }

    #[test]
    fn notification_copy_is_delivered_once_with_its_target_and_registers_the_action() {
        let mut owner = registry();
        owner.post(copy_post(0)).unwrap();
        owner.post(post(1)).unwrap();
        assert_eq!(owner.backend.copy_actions, [true, false]);
        owner.backend.callbacks["0"].copy();
        let delivered = owner.reconcile();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].1.request_id, "0");
        assert_eq!(delivered[0].3, Some(copy_target("0")));
        assert_eq!(
            owner.entries.len(),
            1,
            "the click-only post stays registered"
        );
        assert!(owner.reconcile().is_empty());
    }

    #[test]
    fn notification_first_terminal_event_wins_between_copy_and_click() {
        let mut owner = registry();
        owner.post(copy_post(0)).unwrap();
        let signal = owner.backend.callbacks["0"].clone();
        signal.copy();
        signal.click();
        let delivered = owner.reconcile();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].3, Some(copy_target("0")));
        assert!(owner.reconcile().is_empty());
        assert!(owner.entries.is_empty());
    }

    #[test]
    fn notification_retired_copy_is_neither_a_click_nor_a_copy() {
        let mut owner = registry();
        owner.post(copy_post(0)).unwrap();
        owner.backend.callbacks["0"].retire();
        owner.backend.callbacks["0"].copy();
        assert!(owner.reconcile().is_empty());
        assert!(owner.entries.is_empty());
    }

    #[test]
    fn notification_copy_is_not_redelivered_while_cleanup_is_retried() {
        let mut owner = registry();
        owner.post(copy_post(0)).unwrap();
        owner.backend.fail_remove = true;
        owner.backend.callbacks["0"].copy();
        assert_eq!(owner.reconcile().len(), 1);
        assert_eq!(owner.entries.len(), 1);
        assert!(owner.reconcile().is_empty());
        owner.backend.fail_remove = false;
        assert!(owner.reconcile().is_empty());
        assert!(owner.entries.is_empty());
    }

    #[test]
    fn notification_cancelled_session_ignores_late_copy() {
        let mut owner = registry();
        let first = copy_post(0);
        let cancel = first.cancel.clone();
        owner.post(first).unwrap();
        let delayed = owner.backend.callbacks["0"].clone();
        cancel.cancel();
        delayed.copy();
        assert!(owner.reconcile().is_empty());
        assert!(owner.entries.is_empty());
    }

    #[test]
    fn notification_copy_and_click_keep_sequence_order_in_the_three_bit_event_field() {
        let mut owner = registry();
        owner.post(copy_post(0)).unwrap();
        owner.post(post(1)).unwrap();
        owner.backend.callbacks["1"].click();
        owner.backend.callbacks["0"].copy();
        let delivered = owner.reconcile();
        assert_eq!(
            delivered
                .iter()
                .map(|(_, value, _, copy, _)| (value.request_id.as_str(), copy.is_some()))
                .collect::<Vec<_>>(),
            [("1", false), ("0", true)]
        );
        assert_eq!(delivered[0].0, 1);
        assert_eq!(delivered[1].0, 2);
    }
}
