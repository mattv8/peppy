//! Native-owned runtime for floating conversation heads.
use crate::{
    credentials::Binding,
    diagnostics::{self, head_typing_outcome, EVENT_HEAD_TYPING, FIELD_OUTCOME},
    error::{core_error, BridgeError, BridgeResult},
    heads::{HeadLayout, HeadLayoutStore, Pin, PinStore, MAX_HEADS},
    tray,
    windows::{
        self, HeadCallback, HeadEvent, HeadFrame, HeadGlyph, HeadPhoto, HeadPosition, HeadSpec,
    },
    AppState, STATE_EVENT,
};
use peppy_client_core::{ConversationId, Direction, SendState};
use peppy_desktop_api::{
    avatar::{conversation_avatar, initials, palette_index, AvatarKind},
    images::avatar_rgba,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    str::FromStr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Listener, Manager, WindowEvent};
use tokio::sync::{mpsc, oneshot};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PopoutResult {
    pub head_created: bool,
    pub warning: Option<String>,
}

#[derive(Clone)]
struct ActiveHead {
    position: HeadPosition,
    frame: Option<HeadFrame>,
    generation: u64,
    panel: bool,
    name: String,
    glyph: HeadGlyph,
    color: [u8; 3],
    label: String,
    unread: u64,
    failed_message: Option<String>,
}

const TYPING_MIN_VISIBLE: Duration = Duration::from_millis(600);
const TYPING_LINGER: Duration = Duration::from_millis(300);

/// The typing flag the native head shows, kept apart from the raw flag the typing actor reports.
/// Raw is true exactly while `stopped_at` is `None`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TypingDisplay {
    shown_at: Instant,
    /// When raw went false; `None` while raw is true.
    stopped_at: Option<Instant>,
    /// Drawn from `TypingDisplays::next_epoch` whenever `stopped_at` changes, so a pending hide
    /// can tell it was superseded.
    epoch: u64,
}

/// Displayed typing state per conversation id. Independent of `heads`, so a head created or
/// re-popped while typing shows the pill, and timers hold no head clone. An entry exists exactly
/// while the displayed flag is true.
#[derive(Default)]
struct TypingDisplays {
    entries: HashMap<String, TypingDisplay>,
    /// Global and never reused, so a stale timer cannot match a newer entry's epoch.
    next_epoch: u64,
}

#[derive(Debug, PartialEq)]
enum TypingTransition {
    /// A new entry was created: the displayed flag became true.
    Shown,
    /// The entry was removed without a linger (app locked): the displayed flag became false.
    Hidden,
    Unchanged,
    /// Raw went false: expire the entry after `delay` if `epoch` is still current.
    HideAfter {
        delay: Duration,
        epoch: u64,
    },
}

struct RuntimeState {
    binding: Option<Binding>,
    session: Option<Arc<crate::session::Session>>,
    heads: HashMap<String, ActiveHead>,
    typing: TypingDisplays,
    send_failures: SendFailures,
    generation: u64,
    refreshing: bool,
    queued: bool,
    stopped: bool,
    menu_entries: Option<Vec<(String, String)>>,
    recent_entries: Option<Vec<(String, String)>>,
    applied_recent_entries: Option<Vec<(String, String)>>,
    /// Layout cache keyed by conversation_id for the active binding.
    /// Loaded on popout/refresh/expand and updated on every write.
    /// Cleared on binding change/reset.
    layout_cache: HashMap<String, Option<HeadLayout>>,
}

impl RuntimeState {
    fn new(generation: u64) -> Self {
        Self {
            binding: None,
            session: None,
            heads: HashMap::new(),
            typing: TypingDisplays::default(),
            send_failures: SendFailures::default(),
            generation,
            refreshing: false,
            queued: false,
            stopped: false,
            menu_entries: None,
            recent_entries: None,
            applied_recent_entries: None,
            layout_cache: HashMap::new(),
        }
    }
}

enum ActivationUi {
    Collapse(BridgeResult<()>),
    Expand(BridgeResult<()>),
    Focus(BridgeResult<()>),
}

#[derive(Clone, Copy)]
enum ActivationMode {
    Toggle,
    ExpandOrFocus,
}

fn expansion_failed(result: &BridgeResult<Option<ActivationUi>>) -> bool {
    matches!(result, Ok(Some(ActivationUi::Expand(Err(_)))))
}

fn adopt_active_head(
    existing: Option<&ActiveHead>,
    position: HeadPosition,
    generation: u64,
    head: &HeadData,
) -> ActiveHead {
    ActiveHead {
        position,
        frame: existing.and_then(|active| active.frame.clone()),
        generation,
        panel: existing.is_some_and(|active| active.panel),
        name: head.name.clone(),
        glyph: head.glyph.clone(),
        color: head.color,
        label: head.label.clone(),
        unread: head.unread,
        failed_message: head.failed_message.clone(),
    }
}

/// Time left before a pill shown at `shown_at` and stopped at `stopped_at` may hide.
fn typing_hide_delay(shown_at: Instant, stopped_at: Instant, now: Instant) -> Duration {
    (shown_at + TYPING_MIN_VISIBLE)
        .max(stopped_at + TYPING_LINGER)
        .saturating_duration_since(now)
}

impl TypingDisplays {
    fn displayed(&self, conversation_id: &str) -> bool {
        self.entries.contains_key(conversation_id)
    }

    fn fresh_epoch(&mut self) -> u64 {
        self.next_epoch += 1;
        self.next_epoch
    }

    /// Applies one raw typing report.
    fn apply_raw(&mut self, conversation_id: &str, typing: bool, now: Instant) -> TypingTransition {
        let Some(entry) = self.entries.get(conversation_id).copied() else {
            if !typing {
                return TypingTransition::Unchanged;
            }
            let epoch = self.fresh_epoch();
            let entry = TypingDisplay {
                shown_at: now,
                stopped_at: None,
                epoch,
            };
            self.entries.insert(conversation_id.to_owned(), entry);
            return TypingTransition::Shown;
        };
        if typing {
            // Resuming during a linger gets a new epoch, which cancels the pending hide.
            if entry.stopped_at.is_some() {
                let epoch = self.fresh_epoch();
                let resumed = TypingDisplay {
                    stopped_at: None,
                    epoch,
                    ..entry
                };
                self.entries.insert(conversation_id.to_owned(), resumed);
            }
            return TypingTransition::Unchanged;
        }
        if entry.stopped_at.is_some() {
            // A hide is already pending for this stop.
            return TypingTransition::Unchanged;
        }
        let epoch = self.fresh_epoch();
        let stopped = TypingDisplay {
            stopped_at: Some(now),
            epoch,
            ..entry
        };
        self.entries.insert(conversation_id.to_owned(), stopped);
        TypingTransition::HideAfter {
            delay: typing_hide_delay(entry.shown_at, now, now),
            epoch,
        }
    }

    /// While the app is locked no typing reaches the displayed state: an entry is dropped and the
    /// report is ignored, so nothing reappears after unlock without fresh raw typing.
    fn apply_report(
        &mut self,
        locked: bool,
        conversation_id: &str,
        typing: bool,
        now: Instant,
    ) -> TypingTransition {
        if locked {
            return if self.entries.remove(conversation_id).is_some() {
                TypingTransition::Hidden
            } else {
                TypingTransition::Unchanged
            };
        }
        self.apply_raw(conversation_id, typing, now)
    }

    /// Removes the entry if its pending hide still applies.
    fn expire(&mut self, conversation_id: &str, epoch: u64, now: Instant) -> bool {
        let Some(entry) = self.entries.get(conversation_id) else {
            return false;
        };
        let Some(stopped_at) = entry.stopped_at else {
            return false;
        };
        if entry.epoch != epoch || !typing_hide_delay(entry.shown_at, stopped_at, now).is_zero() {
            return false;
        }
        self.entries.remove(conversation_id);
        true
    }

    /// An unread change during the linger ends it at once, so the native update that carries the
    /// new count also carries `typing = false`. Returns whether the entry was removed.
    fn end_linger_on_unread_change(
        &mut self,
        conversation_id: &str,
        previous_unread: u64,
        unread: u64,
    ) -> bool {
        let lingering = self
            .entries
            .get(conversation_id)
            .is_some_and(|entry| entry.stopped_at.is_some());
        if lingering && previous_unread != unread {
            self.entries.remove(conversation_id);
            return true;
        }
        false
    }

    /// Drops every entry. The epoch counter keeps counting, so stale timers stay unmatched.
    fn clear(&mut self) {
        self.entries.clear();
    }

    /// Drops every entry when the binding, session or generation was replaced. Returns the
    /// conversations whose pill was showing, so their native heads can be cleared.
    fn reset_for_replacement(&mut self, replaced: bool) -> Vec<String> {
        if !replaced {
            return Vec::new();
        }
        let ids = self.entries.keys().cloned().collect();
        self.entries.clear();
        ids
    }
}

#[derive(Default)]
struct SendFailures {
    refused: HashSet<String>,
    acknowledged: HashMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewingScope {
    Panel,
    MainWindow,
}

fn viewing_scope(
    panel_visible: bool,
    collapse_pending: bool,
    main_viewing: bool,
) -> Option<ViewingScope> {
    if panel_visible && !collapse_pending {
        Some(ViewingScope::Panel)
    } else if main_viewing {
        Some(ViewingScope::MainWindow)
    } else {
        None
    }
}

impl SendFailures {
    fn seed(&mut self, id: &str, message: Option<&str>) {
        if let Some(message) = message {
            self.acknowledged
                .entry(id.to_owned())
                .or_insert_with(|| message.to_owned());
        }
    }

    fn displayed(&self, id: &str, failed_message: Option<&str>) -> bool {
        self.refused.contains(id)
            || failed_message.is_some_and(|message| {
                self.acknowledged.get(id).map(String::as_str) != Some(message)
            })
    }

    fn refuse(&mut self, id: &str) -> bool {
        self.refused.insert(id.to_owned())
    }

    fn accept(&mut self, id: &str) -> bool {
        self.refused.remove(id)
    }

    fn settle(
        &mut self,
        id: &str,
        current: bool,
        accepted: bool,
        hidden: bool,
        panel: bool,
    ) -> bool {
        if !current || (!accepted && (!hidden || !panel)) {
            return false;
        }
        if accepted {
            self.accept(id)
        } else {
            self.refuse(id)
        }
    }

    fn acknowledge(&mut self, id: &str, failed_message: Option<&str>) -> bool {
        let changed = self.displayed(id, failed_message);
        self.refused.remove(id);
        if let Some(message) = failed_message {
            self.acknowledged.insert(id.to_owned(), message.to_owned());
        }
        changed
    }

    fn acknowledge_main(&mut self, id: &str, failed_message: Option<&str>) -> bool {
        let before = self.displayed(id, failed_message);
        if let Some(message) = failed_message {
            self.acknowledged.insert(id.to_owned(), message.to_owned());
        }
        before != self.displayed(id, failed_message)
    }

    fn acknowledge_for_scope(
        &mut self,
        id: &str,
        failed_message: Option<&str>,
        scope: ViewingScope,
    ) -> bool {
        match scope {
            ViewingScope::Panel => self.acknowledge(id, failed_message),
            ViewingScope::MainWindow => self.acknowledge_main(id, failed_message),
        }
    }

    fn forget(&mut self, id: &str) -> bool {
        let refused = self.refused.remove(id);
        let acknowledged = self.acknowledged.remove(id).is_some();
        refused || acknowledged
    }

    fn remap(&mut self, from: &str, to: &str) -> bool {
        if from == to {
            return false;
        }
        let refused = self.refused.remove(from);
        if refused {
            self.refused.insert(to.to_owned());
        }
        let acknowledged = self.acknowledged.remove(from);
        let changed = refused || acknowledged.is_some();
        if let Some(message) = acknowledged {
            self.acknowledged.entry(to.to_owned()).or_insert(message);
        }
        changed
    }

    fn clear(&mut self) -> bool {
        let changed = !self.refused.is_empty() || !self.acknowledged.is_empty();
        self.refused.clear();
        self.acknowledged.clear();
        changed
    }
}

fn is_failed_send(direction: Direction, state: Option<SendState>) -> bool {
    direction == Direction::Outgoing
        && matches!(
            state,
            Some(
                SendState::FailedBeforeSubmission
                    | SendState::FailedConfirmed
                    | SendState::OutcomeUnknown
            )
        )
}

/// A failed newest send wins; the failed reaction is only looked up without one.
fn select_head_failure(
    failed_send: Option<String>,
    failed_reaction: impl FnOnce() -> BridgeResult<Option<String>>,
) -> BridgeResult<Option<String>> {
    failed_send.map_or_else(failed_reaction, |id| Ok(Some(id)))
}

fn head_failure(
    session: &crate::session::Session,
    conversation: ConversationId,
    latest: Option<&peppy_client_core::Message>,
) -> BridgeResult<Option<String>> {
    let failed_send = latest
        .filter(|message| is_failed_send(message.payload.direction, message.send_state))
        .map(|message| message.payload.record.message_id.to_string());
    select_head_failure(failed_send, || {
        session
            .client
            .latest_failed_reaction_request(conversation)
            .map(|id| id.map(|id| id.to_string()))
            .map_err(core_error)
    })
}

/// The spec a native head draws, with displayed state from the maps.
fn head_spec(
    typing: &TypingDisplays,
    failures: &SendFailures,
    conversation_id: &str,
    head: &ActiveHead,
) -> HeadSpec {
    HeadSpec {
        conversation_id: conversation_id.to_owned(),
        generation: head.generation,
        glyph: head.glyph.clone(),
        color: head.color,
        label: head.label.clone(),
        unread: head.unread,
        typing: typing.displayed(conversation_id),
        send_failed: failures.displayed(conversation_id, head.failed_message.as_deref()),
        position: head.position.clone(),
    }
}

fn pin_limit_reached(persisted: usize, active: usize, already_persisted: bool) -> bool {
    !already_persisted && persisted.max(active) >= MAX_HEADS
}

enum RuntimeAction {
    Activate {
        conversation_id: String,
        generation: u64,
        mode: ActivationMode,
    },
    Dismiss {
        conversation_id: String,
        generation: u64,
    },
    PersistMove {
        conversation_id: String,
        generation: u64,
        position: HeadPosition,
    },
}

pub struct HeadRuntime {
    pins: PinStore,
    failure_persistence: Mutex<()>,
    layouts: HeadLayoutStore,
    state: Mutex<RuntimeState>,
    actions: mpsc::UnboundedSender<RuntimeAction>,
    panel_writes: Mutex<HashMap<String, PhysicalRect>>,
    panel_debounce: Mutex<HashMap<String, u64>>,
    photos: PhotoCache,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PhysicalRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone)]
struct RuntimeToken {
    binding: Binding,
    generation: u64,
    session: Arc<crate::session::Session>,
}

enum RefreshVisibility<T> {
    Continue(T),
    Clear,
    Preserve { clear_recents: bool },
}

enum RefreshRevalidation {
    Current,
    Clear,
    Stale,
}

#[derive(Clone)]
struct HeadData {
    id: String,
    name: String,
    color: [u8; 3],
    glyph: HeadGlyph,
    label: String,
    unread: u64,
    failed_message: Option<String>,
}

struct RawHead {
    id: String,
    name: String,
    participants: Option<Vec<String>>,
    recipientless: bool,
    unread: u64,
    failed_message: Option<String>,
}

#[derive(Clone)]
struct ResolvedFace {
    display_name: String,
    photo: Option<HeadPhoto>,
}

type PhotoCache = Arc<Mutex<PhotoCacheEntries>>;

struct PhotoCacheEntries {
    entries: HashMap<peppy_client_core::AttachmentId, Option<HeadPhoto>>,
    epoch: u64,
}

impl PhotoCacheEntries {
    fn clear(&mut self) {
        self.entries.clear();
        self.epoch = self.epoch.wrapping_add(1);
    }
}

/// Installs one serialized refresh listener and one ordered native-event lane.
pub fn install(app: &AppHandle) -> BridgeResult<()> {
    if app.try_state::<HeadRuntime>().is_some() {
        return Ok(());
    }
    let root = app.state::<AppState>().root.clone();
    let generation = app
        .state::<AppState>()
        .head_generation
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .current();
    let (actions, mut receiver) = mpsc::unbounded_channel();
    app.manage(HeadRuntime {
        pins: PinStore::new(&root),
        failure_persistence: Mutex::new(()),
        layouts: HeadLayoutStore::new(&root),
        state: Mutex::new(RuntimeState::new(generation)),
        actions,
        panel_writes: Mutex::new(HashMap::new()),
        panel_debounce: Mutex::new(HashMap::new()),
        photos: Arc::new(Mutex::new(PhotoCacheEntries {
            entries: HashMap::new(),
            epoch: 0,
        })),
    });
    let action_app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(action) = receiver.recv().await {
            handle_action(&action_app, action).await;
        }
    });
    let handle = app.clone();
    app.listen(STATE_EVENT, move |_| schedule_refresh(&handle));
    schedule_refresh(app);
    Ok(())
}

fn same_geometry(left: PhysicalRect, right: PhysicalRect) -> bool {
    (left.x - right.x).abs() <= 2
        && (left.y - right.y).abs() <= 2
        && (left.width as i64 - right.width as i64).abs() <= 2
        && (left.height as i64 - right.height as i64).abs() <= 2
}

fn skip_panel_persist(recorded: Option<PhysicalRect>, actual: PhysicalRect) -> bool {
    recorded.is_some_and(|written| same_geometry(written, actual))
}

#[cfg(any(target_os = "macos", test))]
fn should_restore_panel_topmost(fullscreen: Option<bool>, topmost: Option<bool>) -> bool {
    fullscreen == Some(false) && topmost == Some(false)
}

#[cfg(target_os = "macos")]
fn prepare_windowed_panel(window: &tauri::Window) -> bool {
    let (Ok(fullscreen), Some(topmost)) = (
        window.is_fullscreen(),
        crate::window_level::is_floating(window),
    ) else {
        return false;
    };
    if fullscreen {
        return false;
    }
    if should_restore_panel_topmost(Some(fullscreen), Some(topmost)) {
        return crate::window_level::set_floating(window, true).is_ok();
    }
    true
}

fn panel_layout_from_physical(frame: &HeadFrame, rect: PhysicalRect) -> crate::heads::PanelLayout {
    let scale = if frame.scale_factor.is_finite() {
        frame.scale_factor.max(1.0)
    } else {
        1.0
    };
    crate::heads::PanelLayout {
        monitor: None,
        width: rect.width as f64 / scale,
        height: rect.height as f64 / scale,
        offset_x: (rect.x as f64 - frame.x) / scale,
        offset_y: (rect.y as f64 - frame.y) / scale,
    }
}

pub(crate) fn record_panel_geometry(app: &AppHandle, label: &str, rect: PhysicalRect) {
    if let Ok(mut writes) = runtime(app).panel_writes.lock() {
        writes.insert(label.to_owned(), rect);
    }
}

pub(crate) fn panel_window_event(window: &tauri::Window, event: &WindowEvent) {
    if tray::composer_conversation(window.label()).is_none()
        || window.app_handle().try_state::<HeadRuntime>().is_none()
    {
        return;
    }
    if matches!(event, WindowEvent::Destroyed) {
        let runtime = runtime(window.app_handle());
        if let Ok(mut writes) = runtime.panel_writes.lock() {
            writes.remove(window.label());
        }
        if let Ok(mut pending) = runtime.panel_debounce.lock() {
            pending.remove(window.label());
        }
        return;
    }
    if !matches!(event, WindowEvent::Moved(_) | WindowEvent::Resized(_))
        || !is_panel(window.app_handle(), window.label())
    {
        return;
    }
    #[cfg(target_os = "macos")]
    if !prepare_windowed_panel(window) {
        return;
    }
    let Ok(position) = window.outer_position() else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let rect = PhysicalRect {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    };
    let app = window.app_handle().clone();
    let label = window.label().to_owned();
    let head_runtime = runtime(&app);
    if head_runtime
        .panel_writes
        .lock()
        .ok()
        .and_then(|writes| writes.get(&label).copied())
        .is_some_and(|written| skip_panel_persist(Some(written), rect))
    {
        return;
    }
    let serial = {
        let Ok(mut pending) = head_runtime.panel_debounce.lock() else {
            return;
        };
        let serial = pending.get(&label).copied().unwrap_or(0).wrapping_add(1);
        pending.insert(label.clone(), serial);
        serial
    };
    tauri::async_runtime::spawn(async move {
        let Ok(_operation) = crate::admit_update_operation(&app.state::<AppState>()) else {
            return;
        };
        tokio::time::sleep(Duration::from_millis(400)).await;
        let head_runtime = runtime(&app);
        if head_runtime
            .panel_debounce
            .lock()
            .ok()
            .and_then(|pending| pending.get(&label).copied())
            != Some(serial)
        {
            return;
        }
        let Some(window) = app.get_webview_window(&label) else {
            return;
        };
        if !window.is_visible().unwrap_or(false) || window.is_minimized().unwrap_or(false) {
            return;
        }
        #[cfg(target_os = "macos")]
        if window.is_fullscreen().ok() != Some(false) {
            return;
        }
        let Ok(position) = window.outer_position() else {
            return;
        };
        let Ok(size) = window.outer_size() else {
            return;
        };
        let rect = PhysicalRect {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
        };
        if skip_panel_persist(
            head_runtime
                .panel_writes
                .lock()
                .ok()
                .and_then(|writes| writes.get(&label).copied()),
            rect,
        ) {
            return;
        }
        let Some(conversation) = tray::composer_conversation(&label) else {
            return;
        };
        let (binding, generation, frame, head_position) = {
            let Ok(state) = head_runtime.state.lock() else {
                return;
            };
            let Some(head) = state.heads.get(&conversation.to_string()) else {
                return;
            };
            (
                state.binding.clone(),
                head.generation,
                head.frame.clone(),
                head.position.clone(),
            )
        };
        let (Some(binding), Some(frame)) = (binding, frame) else {
            return;
        };
        if !app
            .state::<AppState>()
            .head_generation
            .lock()
            .ok()
            .is_some_and(|fence| fence.accepts(generation))
        {
            return;
        }
        let conv_str = conversation.to_string();
        if let Ok(layout) = head_runtime.layouts.save_panel(
            &binding,
            &conv_str,
            head_position,
            panel_layout_from_physical(&frame, rect),
        ) {
            update_layout_cache(&app, &binding, generation, &conv_str, Some(layout));
        }
    });
}

fn runtime(app: &AppHandle) -> tauri::State<'_, HeadRuntime> {
    app.state::<HeadRuntime>()
}

/// Pushes the displayed typing flag of one head to the native layer on the main thread.
fn push_typing_update(app: &AppHandle, token: RuntimeToken, conversation_id: String) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if !token_valid_sync(&handle, &token) {
            return;
        }
        let spec = runtime(&handle).state.lock().ok().and_then(|state| {
            state
                .heads
                .get(&conversation_id)
                .filter(|head| head.generation == token.generation)
                .map(|head| head_spec(&state.typing, &state.send_failures, &conversation_id, head))
        });
        if let Some(spec) = spec {
            let _ = windows::update_head(&handle, &spec);
        }
    });
}

fn persist_acknowledgement(app: &AppHandle, token: &RuntimeToken, id: &str) {
    let rt = runtime(app);
    let Ok(_guard) = rt.failure_persistence.lock() else {
        return;
    };
    let message = {
        let Ok(state) = rt.state.lock() else { return };
        if !token_matches_state(app, &state, token)
            || !state
                .session
                .as_ref()
                .is_some_and(|session| Arc::ptr_eq(session, &token.session))
        {
            return;
        }
        state.send_failures.acknowledged.get(id).cloned()
    };
    let Some(message) = message else { return };
    let _ = rt
        .pins
        .set_acknowledged_failure(&token.binding, id, Some(&message));
}

fn acknowledge_failure(app: &AppHandle, token: &RuntimeToken, id: &str, scope: ViewingScope) {
    let result = runtime(app).state.lock().ok().and_then(|mut state| {
        if !token_matches_state(app, &state, token) {
            return None;
        }
        let failed = state
            .heads
            .get(id)
            .and_then(|head| head.failed_message.clone());
        let persist = failed
            .as_ref()
            .is_some_and(|message| state.send_failures.acknowledged.get(id) != Some(message));
        let changed = state
            .send_failures
            .acknowledge_for_scope(id, failed.as_deref(), scope);
        Some((changed, persist))
    });
    if let Some((changed, persist)) = result {
        if persist {
            persist_acknowledgement(app, token, id);
        }
        if changed {
            push_typing_update(app, token.clone(), id.to_owned());
        }
    }
}

pub(crate) fn send_settled(
    app: &AppHandle,
    label: &str,
    conversation_id: &str,
    originating_session: &Arc<crate::session::Session>,
    accepted: bool,
) {
    let id = Some(conversation_id)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .or_else(|| tray::composer_conversation(label).map(|id| id.to_string()));
    let Some(id) = id else { return };
    let hidden = if accepted {
        false
    } else {
        tray::composer_conversation(label).is_some()
            && (crate::lifecycle::collapse_pending(app, label)
                || app
                    .get_webview_window(label)
                    .and_then(|window| window.is_visible().ok())
                    == Some(false))
    };
    let update = runtime(app).state.lock().ok().and_then(|mut state| {
        if state.stopped {
            return None;
        }
        let (Some(binding), Some(session)) = (state.binding.clone(), state.session.clone()) else {
            return None;
        };
        let panel = state.heads.get(&id).is_some_and(|head| head.panel);
        let changed = state.send_failures.settle(
            &id,
            same_session(&session, originating_session),
            accepted,
            hidden,
            panel,
        );
        (changed && state.heads.contains_key(&id)).then_some(RuntimeToken {
            binding,
            session,
            generation: state.generation,
        })
    });
    if let Some(token) = update {
        push_typing_update(app, token, id);
    }
}

/// The main window switched to this conversation.
pub(crate) fn conversation_viewed(app: &AppHandle, conversation_id: &str) {
    if !main_window_focused(app) || app.try_state::<HeadRuntime>().is_none() {
        return;
    }
    let token = runtime(app).state.lock().ok().and_then(|state| {
        Some(RuntimeToken {
            binding: state.binding.clone()?,
            session: state.session.clone()?,
            generation: state.generation,
        })
    });
    if let Some(token) = token {
        acknowledge_failure(app, &token, conversation_id, ViewingScope::MainWindow);
    }
}

fn main_window_focused(app: &AppHandle) -> bool {
    app.get_webview_window(tray::MAIN).is_some_and(|window| {
        window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false)
    })
}

fn same_session<T>(current: &Arc<T>, originating: &Arc<T>) -> bool {
    Arc::ptr_eq(current, originating)
}

/// Ends a linger after `delay` unless raw typing resumed or the runtime moved on.
fn schedule_typing_expiry(
    app: &AppHandle,
    token: RuntimeToken,
    conversation_id: String,
    delay: Duration,
    epoch: u64,
) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(delay).await;
        if !token_valid_sync(&handle, &token) {
            return;
        }
        let has_head = {
            let runtime = runtime(&handle);
            let Ok(mut state) = runtime.state.lock() else {
                return;
            };
            if !state.typing.expire(&conversation_id, epoch, Instant::now()) {
                return;
            }
            state.heads.contains_key(&conversation_id)
        };
        if has_head {
            record_head_typing(false);
            push_typing_update(&handle, token, conversation_id);
        }
    });
}

/// Records a displayed-flag change pushed to a native head (event 33).
fn record_head_typing(shown: bool) {
    let outcome = if shown {
        head_typing_outcome::SHOWN
    } else {
        head_typing_outcome::HIDDEN
    };
    diagnostics::record(EVENT_HEAD_TYPING, &[(FIELD_OUTCOME, outcome)]);
}

/// Records raw typing for a conversation, whether or not it has a head, and pushes the displayed
/// flag only when a head exists and the displayed flag changed.
pub(crate) fn typing_changed(app: &AppHandle, conversation_id: &str, typing: bool) {
    let (transition, token, has_head) = {
        let runtime = runtime(app);
        let Ok(mut state) = runtime.state.lock() else {
            return;
        };
        if state.stopped {
            return;
        }
        let (Some(binding), Some(session)) = (state.binding.clone(), state.session.clone()) else {
            return;
        };
        let locked = app
            .state::<AppState>()
            .app_locked
            .load(std::sync::atomic::Ordering::Acquire);
        let transition = state
            .typing
            .apply_report(locked, conversation_id, typing, Instant::now());
        if transition == TypingTransition::Unchanged {
            return;
        }
        let token = RuntimeToken {
            binding,
            session,
            generation: state.generation,
        };
        (transition, token, state.heads.contains_key(conversation_id))
    };
    if has_head
        && matches!(
            transition,
            TypingTransition::Shown | TypingTransition::Hidden
        )
    {
        let shown = matches!(transition, TypingTransition::Shown);
        record_head_typing(shown);
        push_typing_update(app, token.clone(), conversation_id.to_owned());
    }
    if let TypingTransition::HideAfter { delay, epoch } = transition {
        schedule_typing_expiry(app, token, conversation_id.to_owned(), delay, epoch);
    }
}

fn schedule_refresh(app: &AppHandle) {
    if app
        .state::<AppState>()
        .app_locked
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return;
    }
    let start = {
        let runtime = runtime(app);
        let Ok(mut state) = runtime.state.lock() else {
            return;
        };
        if state.stopped {
            return;
        }
        if state.refreshing {
            state.queued = true;
            false
        } else {
            state.refreshing = true;
            true
        }
    };
    if !start {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let state = app.state::<AppState>();
            let Ok(_operation) = crate::admit_update_operation(&state) else {
                if let Ok(mut runtime_state) = runtime(&app).state.lock() {
                    runtime_state.refreshing = false;
                }
                return;
            };
            refresh(&app, &_operation).await;
            drop(_operation);
            let again = {
                let runtime = runtime(&app);
                let Ok(mut state) = runtime.state.lock() else {
                    return;
                };
                if state.stopped {
                    state.refreshing = false;
                    state.queued = false;
                    false
                } else if state.queued {
                    state.queued = false;
                    true
                } else {
                    state.refreshing = false;
                    false
                }
            };
            if !again {
                break;
            }
        }
    });
}

async fn refresh(app: &AppHandle, operation: &crate::update_safety::OperationGuard) {
    let app_state = app.state::<AppState>();
    if app_state
        .app_locked
        .load(std::sync::atomic::Ordering::Acquire)
    {
        clear_visible(app).await;
        return;
    }
    let binding = match app_state
        .config()
        .ok()
        .and_then(|config| config.active_binding().cloned())
    {
        Some(binding) => binding,
        None => {
            clear_visible(app).await;
            return;
        }
    };
    let session =
        match admitted_refresh_session(&binding, app_state.session_admitted(operation).await) {
            RefreshVisibility::Continue(session) => session,
            RefreshVisibility::Clear => {
                clear_visible(app).await;
                return;
            }
            RefreshVisibility::Preserve { .. } => return,
        };
    let (token, stale, clear_recent_menu) =
        match begin_binding(app, binding.clone(), session.clone()) {
            Ok(value) => value,
            Err(_) => return,
        };
    if !stale.is_empty() {
        let stale_for_ui = stale.clone();
        let _ = on_main_if_current(app, token.clone(), move |app| {
            for id in stale_for_ui {
                crate::close_media_viewers_for_parent(
                    app,
                    &format!("{}{}", tray::COMPOSER_PREFIX, id),
                );
                let _ = windows::hide_head(app, &id);
            }
        })
        .await;
    }
    if clear_recent_menu {
        update_recent_tray_menu(app, Some(&token)).await;
    }

    let mut pins = match runtime(app).pins.pins(&binding) {
        Ok(pins) => pins,
        Err(_) => return,
    };
    let mut recent_ids = match app.state::<AppState>().recents.list(&token.binding) {
        Ok(ids) => ids,
        Err(_) => {
            clear_recent_entries(app, &token).await;
            Vec::new()
        }
    };
    let ids = pins
        .iter()
        .map(|pin| pin.conversation_id.clone())
        .chain(recent_ids.iter().cloned())
        .collect::<HashSet<_>>();
    let (redirects, mut protected) = match crate::sync::blocking({
        let session = session.clone();
        move || {
            Ok(collect_redirects(ids, |source| {
                session.redirected_conversation(source)
            }))
        }
    })
    .await
    {
        Ok(redirects) => redirects,
        Err(_) => return,
    };
    if !revalidate_after_await(app, &token).await {
        return;
    }
    for (from, to) in redirects {
        let pin_failed = runtime(app).pins.remap(&binding, &from, &to).is_err();
        let recent_failed = app_state.recents.remap(&binding, &from, &to).is_err();
        if !pin_failed {
            let _ = runtime(app).state.lock().map(|mut state| {
                if token_matches_state(app, &state, &token) {
                    state.send_failures.remap(&from, &to);
                }
            });
        }
        if pin_failed || recent_failed {
            protected.insert(from);
        }
    }
    pins = match runtime(app).pins.pins(&binding) {
        Ok(pins) => pins,
        Err(_) => return,
    };
    recent_ids = match app_state.recents.list(&binding) {
        Ok(ids) => ids,
        Err(_) => return,
    };
    let wanted_ids = pins
        .iter()
        .map(|pin| pin.conversation_id.clone())
        .chain(recent_ids.iter().cloned())
        .collect::<HashSet<_>>();
    let photos = runtime(app).photos.clone();
    let data = match resolved_refresh_visibility(
        &session,
        crate::sync::blocking({
            let session = session.clone();
            move || resolve_heads(&session, &wanted_ids, &photos)
        }),
        async {
            if app_state
                .app_locked
                .load(std::sync::atomic::Ordering::Acquire)
            {
                RefreshRevalidation::Clear
            } else if revalidate_after_await(app, &token).await {
                RefreshRevalidation::Current
            } else {
                RefreshRevalidation::Stale
            }
        },
    )
    .await
    {
        RefreshVisibility::Continue(data) => data,
        RefreshVisibility::Clear => {
            clear_visible(app).await;
            return;
        }
        RefreshVisibility::Preserve { clear_recents } => {
            if clear_recents {
                clear_recent_entries(app, &token).await;
            }
            return; // transient query failures leave the last good registry intact
        }
    };
    if app
        .state::<AppState>()
        .app_locked
        .load(std::sync::atomic::Ordering::Acquire)
    {
        clear_visible(app).await;
        return;
    }
    let data: HashMap<String, HeadData> = data
        .into_iter()
        .map(|head| (head.id.clone(), head))
        .collect();
    refresh_recent_conversations(app, &token, &recent_ids, &data, &protected).await;
    // The core resolution above is authoritative for this binding. Only now may stale persisted
    // pins be pruned; doing it before a successful resolution makes transient failures consume
    // capacity permanently.
    for stale_pin in pins.iter().filter(|pin| {
        !data.contains_key(&pin.conversation_id) && !protected.contains(&pin.conversation_id)
    }) {
        if runtime(app)
            .pins
            .remove(&binding, &stale_pin.conversation_id)
            .is_err()
        {
            return;
        }
    }
    let wanted: HashMap<String, (Pin, HeadData)> = pins
        .into_iter()
        .filter_map(|pin| {
            data.get(&pin.conversation_id)
                .cloned()
                .map(|head| (pin.conversation_id.clone(), (pin, head)))
        })
        .collect();
    let visible: HashSet<String> = wanted
        .keys()
        .filter(|id| {
            app.get_webview_window(&format!("{}{}", tray::COMPOSER_PREFIX, id))
                .and_then(|window| window.is_visible().ok())
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    let collapsing: HashSet<String> = wanted
        .keys()
        .filter(|id| {
            crate::lifecycle::collapse_pending(app, &format!("{}{}", tray::COMPOSER_PREFIX, id))
        })
        .cloned()
        .collect();
    let viewed = main_window_focused(app)
        .then(|| app_state.notifications.viewed_conversation())
        .flatten();
    let (to_hide, specs, acknowledgements) = {
        let runtime = runtime(app);
        let Ok(mut state) = runtime.state.lock() else {
            return;
        };
        if !token_matches_state(app, &state, &token) {
            return;
        }
        let to_hide = state
            .heads
            .keys()
            .filter(|id| !wanted.contains_key(*id) && !protected.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in &to_hide {
            state.heads.remove(id);
            state.send_failures.forget(id);
        }
        let mut specs = Vec::with_capacity(wanted.len());
        let mut acknowledgements = Vec::new();
        for (id, (pin, head)) in wanted {
            let existing = state.heads.get(&id).cloned();
            let previous_failed = existing.as_ref().is_some_and(|active| {
                state
                    .send_failures
                    .displayed(&id, active.failed_message.as_deref())
            });
            state
                .send_failures
                .seed(&id, pin.acknowledged_failure.as_deref());
            let scope = viewing_scope(
                existing.as_ref().is_some_and(|active| active.panel) && visible.contains(&id),
                collapsing.contains(&id),
                viewed.as_deref() == Some(&id),
            );
            let acknowledged = if let Some(scope) = scope {
                if let Some(message) = head.failed_message.as_ref() {
                    if state.send_failures.acknowledged.get(&id) != Some(message) {
                        acknowledgements.push(id.clone());
                    }
                }
                state.send_failures.acknowledge_for_scope(
                    &id,
                    head.failed_message.as_deref(),
                    scope,
                )
            } else {
                false
            };
            if let Some(active) = existing.as_ref() {
                state
                    .typing
                    .end_linger_on_unread_change(&id, active.unread, head.unread);
            }
            let position = existing
                .as_ref()
                .map(|active| active.position.clone())
                .unwrap_or(pin.position);
            let active = adopt_active_head(existing.as_ref(), position, token.generation, &head);
            let send_failed = state
                .send_failures
                .displayed(&id, active.failed_message.as_deref());
            let changed = existing.as_ref().is_none_or(|active| {
                active.glyph != head.glyph
                    || active.color != head.color
                    || active.label != head.label
                    || active.unread != head.unread
                    || active.failed_message != head.failed_message
            }) || previous_failed != send_failed;
            if changed || acknowledged {
                specs.push((
                    head_spec(&state.typing, &state.send_failures, &id, &active),
                    existing.is_some(),
                ));
            }
            state.heads.insert(id, active);
        }
        (to_hide, specs, acknowledgements)
    };

    for id in acknowledgements {
        persist_acknowledgement(app, &token, &id);
    }
    for id in to_hide {
        let hide_id = id.clone();
        let _ = on_main_if_current(app, token.clone(), move |app| {
            crate::close_media_viewers_for_parent(
                app,
                &format!("{}{}", tray::COMPOSER_PREFIX, hide_id),
            );
            windows::hide_head(app, &hide_id)
        })
        .await;
    }
    for (mut spec, existed) in specs {
        let spec_id = spec.conversation_id.clone();
        let result = on_main_if_current(app, token.clone(), move |app| {
            if let Ok(state) = runtime(app).state.lock() {
                spec.typing = state.typing.displayed(&spec.conversation_id);
                if let Some(head) = state.heads.get(&spec.conversation_id) {
                    spec.send_failed = state
                        .send_failures
                        .displayed(&spec.conversation_id, head.failed_message.as_deref());
                }
            }
            if existed {
                windows::update_head(app, &spec)
            } else {
                windows::show_head(app, &spec, callback(app))
            }
        })
        .await;
        if !existed && matches!(result, Ok(Some(Err(_)))) {
            let _ = runtime(app).state.lock().map(|mut state| {
                if state
                    .heads
                    .get(&spec_id)
                    .is_some_and(|head| head.generation == token.generation)
                {
                    state.heads.remove(&spec_id);
                }
            });
        }
    }
    update_tray_menu(app, Some(&token)).await;
}

fn collect_redirects(
    ids: HashSet<String>,
    mut lookup: impl FnMut(ConversationId) -> BridgeResult<Option<ConversationId>>,
) -> (Vec<(String, String)>, HashSet<String>) {
    let mut redirects = Vec::new();
    let mut protected = HashSet::new();
    for id in ids {
        let Ok(source) = ConversationId::from_str(&id) else {
            continue;
        };
        match lookup(source) {
            Ok(Some(target)) if source != target => redirects.push((id, target.to_string())),
            Err(_) => {
                protected.insert(id);
            }
            _ => {}
        }
    }
    (redirects, protected)
}

fn resolve_heads(
    session: &Arc<crate::session::Session>,
    wanted: &HashSet<String>,
    photos: &PhotoCache,
) -> BridgeResult<Option<Vec<HeadData>>> {
    if !session_displayable(session)? {
        return Ok(None);
    }
    if wanted.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let tone_overrides = unlocked_tone_overrides(session)?;
    let conversations = session.client.list_conversations().map_err(core_error)?;
    let drafts = session.client.compose_drafts().map_err(core_error)?;
    let drafts_by_conversation = drafts
        .iter()
        .map(|draft| (draft.conversation_id, draft))
        .collect::<HashMap<_, _>>();
    let mut raw_heads = Vec::with_capacity(wanted.len());
    let mut emitted_ids = HashSet::with_capacity(wanted.len());
    for conversation in conversations {
        let id = conversation.conversation_id.to_string();
        if !wanted.contains(&id) {
            continue;
        }
        let latest = session
            .client
            .latest_message(conversation.conversation_id)
            .map_err(core_error)?;
        let failed_message = head_failure(session, conversation.conversation_id, latest.as_ref())?;
        let name = latest
            .as_ref()
            .map(crate::dto::counterpart)
            .or_else(|| {
                drafts_by_conversation
                    .get(&conversation.conversation_id)
                    .map(|draft| draft.recipients.join(", "))
            })
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Conversation".into());
        let participants = session
            .client
            .mms_reply_context(conversation.conversation_id)
            .ok()
            .map(|context| context.recipients);
        raw_heads.push(RawHead {
            id: id.clone(),
            name,
            participants,
            recipientless: false,
            unread: conversation.unread_count,
            failed_message,
        });
        emitted_ids.insert(id);
    }
    for draft in drafts {
        let id = draft.conversation_id.to_string();
        if !wanted.contains(&id) || emitted_ids.contains(&id) {
            continue;
        }
        let name = draft.recipients.join(", ");
        raw_heads.push(RawHead {
            id: id.clone(),
            name,
            participants: Some(draft.recipients.clone()),
            recipientless: draft.recipients.is_empty(),
            unread: 0,
            failed_message: None,
        });
        emitted_ids.insert(id);
    }
    Ok(Some(resolve_raw_heads(
        session,
        raw_heads,
        photos,
        &tone_overrides,
    )))
}

fn resolve_one(
    session: &Arc<crate::session::Session>,
    conversation: ConversationId,
    photos: &PhotoCache,
) -> BridgeResult<HeadData> {
    if !session_displayable(session)? {
        return Err(BridgeError::new(
            "session-unavailable",
            "The local account is no longer available for floating conversations.",
        ));
    }
    let tone_overrides = unlocked_tone_overrides(session)?;
    let drafts = session.client.compose_drafts().map_err(core_error)?;
    if let Some(found) = session
        .client
        .list_conversations()
        .map_err(core_error)?
        .into_iter()
        .find(|item| item.conversation_id == conversation)
    {
        let latest = session
            .client
            .latest_message(conversation)
            .map_err(core_error)?;
        let failed_message = head_failure(session, conversation, latest.as_ref())?;
        let name = latest
            .as_ref()
            .map(crate::dto::counterpart)
            .or_else(|| {
                drafts
                    .iter()
                    .find(|draft| draft.conversation_id == conversation)
                    .map(|draft| draft.recipients.join(", "))
            })
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Conversation".into());
        return Ok(resolve_one_raw_head(
            session,
            RawHead {
                id: conversation.to_string(),
                name,
                participants: session
                    .client
                    .mms_reply_context(conversation)
                    .ok()
                    .map(|context| context.recipients),
                recipientless: false,
                unread: found.unread_count,
                failed_message,
            },
            photos,
            &tone_overrides,
        ));
    }
    if let Some(draft) = drafts
        .into_iter()
        .find(|draft| draft.conversation_id == conversation)
    {
        let name = draft.recipients.join(", ");
        return Ok(resolve_one_raw_head(
            session,
            RawHead {
                id: conversation.to_string(),
                name,
                participants: Some(draft.recipients.clone()),
                recipientless: draft.recipients.is_empty(),
                unread: 0,
                failed_message: None,
            },
            photos,
            &tone_overrides,
        ));
    }
    Err(BridgeError::new(
        "not-found",
        "The conversation is not available locally.",
    ))
}

fn resolve_raw_heads(
    session: &crate::session::Session,
    heads: Vec<RawHead>,
    photos: &PhotoCache,
    tone_overrides: &BTreeMap<String, u8>,
) -> Vec<HeadData> {
    let addresses = heads
        .iter()
        .flat_map(raw_addresses)
        .map(|address| (address, None))
        .collect::<Vec<_>>();
    let resolved = crate::contacts::resolve_addresses(&session.client, &addresses);
    heads
        .into_iter()
        .map(|head| resolve_raw_head(session, head, &resolved, photos, tone_overrides))
        .collect()
}

fn resolve_one_raw_head(
    session: &crate::session::Session,
    head: RawHead,
    photos: &PhotoCache,
    tone_overrides: &BTreeMap<String, u8>,
) -> HeadData {
    let addresses = raw_addresses(&head)
        .into_iter()
        .map(|address| (address, None))
        .collect::<Vec<_>>();
    let resolved = crate::contacts::resolve_addresses(&session.client, &addresses);
    resolve_raw_head(session, head, &resolved, photos, tone_overrides)
}

fn raw_addresses(head: &RawHead) -> Vec<String> {
    if head.recipientless {
        return Vec::new();
    }
    head.participants
        .as_ref()
        .filter(|participants| !participants.is_empty())
        .cloned()
        .unwrap_or_else(|| {
            head.name
                .split(", ")
                .filter(|address| !address.is_empty())
                .map(str::to_owned)
                .collect()
        })
}

fn resolve_raw_head(
    session: &crate::session::Session,
    head: RawHead,
    resolved: &HashMap<String, crate::contacts::Resolved>,
    photos: &PhotoCache,
    tone_overrides: &BTreeMap<String, u8>,
) -> HeadData {
    let avatar = conversation_avatar(
        &head.id,
        &head.name,
        head.participants.as_deref(),
        head.recipientless,
    );
    let addresses = raw_addresses(&head);
    let single_address = addresses.first().map(String::as_str);
    let resolved_face = matches!(avatar.kind, AvatarKind::Person)
        .then(|| {
            single_address.and_then(|address| {
                resolved.get(address).map(|contact| ResolvedFace {
                    display_name: contact.display_name.clone(),
                    photo: contact
                        .photo
                        .filter(|id| {
                            session
                                .client
                                .attachment_info(*id)
                                .is_ok_and(|info| info.state.is_local())
                        })
                        .and_then(|id| cached_photo(session, id, photos)),
                })
            })
        })
        .flatten();
    let group_addresses = addresses
        .iter()
        .map(|address| {
            resolved
                .get(address)
                .map(|contact| contact.display_name.clone())
                .unwrap_or_else(|| address.clone())
        })
        .collect::<Vec<_>>();
    let (glyph, label) = head_face(
        avatar.kind,
        resolved_face,
        single_address,
        &group_addresses,
        head.recipientless,
    );
    HeadData {
        id: head.id,
        name: label.clone(),
        color: crate::head_palette::head_color(
            tone_overrides
                .get(&avatar.seed)
                .copied()
                .map(usize::from)
                .unwrap_or_else(|| palette_index(&avatar.seed)),
        ),
        glyph,
        label,
        unread: head.unread,
        failed_message: head.failed_message,
    }
}

fn cached_photo(
    session: &crate::session::Session,
    id: peppy_client_core::AttachmentId,
    photos: &PhotoCache,
) -> Option<HeadPhoto> {
    let epoch = {
        let cache = photos.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(photo) = cache.entries.get(&id) {
            return photo.clone();
        }
        cache.epoch
    };
    let photo = crate::contacts::local_photo_bytes(session, id).and_then(|bytes| {
        avatar_rgba(&bytes, 96).map(|(edge, rgba)| HeadPhoto {
            edge,
            rgba: Arc::from(rgba),
        })
    });
    let mut cache = photos.lock().unwrap_or_else(|poison| poison.into_inner());
    if cache.epoch != epoch {
        return photo;
    }
    if cache.entries.len() >= 16 {
        cache.clear();
    }
    cache.entries.insert(id, photo.clone());
    photo
}

fn clear_photos(app: &AppHandle) {
    runtime(app)
        .photos
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clear();
}

fn head_face(
    kind: AvatarKind,
    resolved: Option<ResolvedFace>,
    single_address: Option<&str>,
    group_addresses: &[String],
    recipientless: bool,
) -> (HeadGlyph, String) {
    let generic_label = if recipientless {
        "New message"
    } else {
        "Conversation"
    };
    if matches!(kind, AvatarKind::Group) {
        return (HeadGlyph::Group, group_addresses.join(", "));
    }
    match resolved {
        Some(ResolvedFace {
            display_name,
            photo: Some(photo),
        }) => (HeadGlyph::Photo(photo), display_name),
        Some(ResolvedFace {
            display_name,
            photo: None,
        }) => {
            let glyph = initials(&display_name, 2);
            if glyph.is_empty() {
                (HeadGlyph::Person, display_name)
            } else {
                (HeadGlyph::Initials(glyph), display_name)
            }
        }
        None => (
            HeadGlyph::Person,
            single_address.unwrap_or(generic_label).to_owned(),
        ),
    }
}

fn session_displayable(session: &crate::session::Session) -> BridgeResult<bool> {
    // Heads display the same local data as the main window. Sync key state
    // controls transport decryption, not access to an already-open database.
    if session.cancel.is_cancelled() {
        return Ok(false);
    }
    let status = session
        .status
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    Ok(!status.revoked && !session.cancel.is_cancelled())
}

fn session_revoked(session: &crate::session::Session) -> bool {
    session.status.lock().is_ok_and(|status| status.revoked)
}

fn admitted_refresh_session(
    binding: &Binding,
    admitted: BridgeResult<Option<Arc<crate::session::Session>>>,
) -> RefreshVisibility<Arc<crate::session::Session>> {
    match admitted {
        Ok(Some(session)) if session.binding == *binding && !session_revoked(&session) => {
            RefreshVisibility::Continue(session)
        }
        Ok(None)
        | Ok(Some(_))
        | Err(BridgeError {
            code: "revoked", ..
        }) => RefreshVisibility::Clear,
        Err(_) => RefreshVisibility::Preserve {
            clear_recents: false,
        },
    }
}

async fn resolved_refresh_visibility<T, R, V>(
    session: &Arc<crate::session::Session>,
    resolution: R,
    revalidated: V,
) -> RefreshVisibility<T>
where
    R: std::future::Future<Output = BridgeResult<Option<T>>>,
    V: std::future::Future<Output = RefreshRevalidation>,
{
    match resolution.await {
        Ok(Some(data)) => match revalidated.await {
            RefreshRevalidation::Current => RefreshVisibility::Continue(data),
            RefreshRevalidation::Clear => RefreshVisibility::Clear,
            RefreshRevalidation::Stale if session_revoked(session) => RefreshVisibility::Clear,
            RefreshRevalidation::Stale => RefreshVisibility::Preserve {
                clear_recents: false,
            },
        },
        Ok(None) => RefreshVisibility::Clear,
        Err(_) => RefreshVisibility::Preserve {
            clear_recents: true,
        },
    }
}

fn unlocked_tone_overrides(
    session: &crate::session::Session,
) -> BridgeResult<BTreeMap<String, u8>> {
    let keys = session.client.key_status().map_err(core_error)?;
    if keys
        .active_epoch
        .is_some_and(|epoch| keys.unlocked_epochs.contains(&epoch))
    {
        session.client.conversation_tones().map_err(core_error)
    } else {
        Ok(BTreeMap::new())
    }
}

fn begin_binding(
    app: &AppHandle,
    binding: Binding,
    session: Arc<crate::session::Session>,
) -> BridgeResult<(RuntimeToken, Vec<String>, bool)> {
    let runtime = runtime(app);
    let mut state = runtime
        .state
        .lock()
        .map_err(|_| BridgeError::host_state())?;
    if state.stopped {
        return Err(BridgeError::new(
            "heads",
            "Floating conversations are stopping.",
        ));
    }
    let binding_changed = state.binding.as_ref() != Some(&binding);
    let session_changed = state
        .session
        .as_ref()
        .is_none_or(|previous| !Arc::ptr_eq(previous, &session));
    let generation = {
        let app_state = app.state::<AppState>();
        let mut fence = app_state
            .head_generation
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        if binding_changed {
            fence.invalidate()
        } else {
            fence.current()
        }
    };
    // A replaced session takes its pending typing with it: its linger timers carry the old token
    // and would bail, so the entries must go now, and heads that kept a pill get a native clear.
    let replaced = binding_changed || session_changed || state.generation != generation;
    let stale = if binding_changed || state.generation != generation {
        let stale = state.heads.keys().cloned().collect();
        state.generation = generation;
        state.binding = Some(binding.clone());
        state.heads.clear();
        state.layout_cache.clear(); // Clear cache on binding change (N2)
        stale
    } else {
        Vec::new()
    };
    let typing_cleared = state.typing.reset_for_replacement(replaced);
    let failures_cleared = if replaced {
        let ids = state.heads.keys().cloned().collect::<Vec<_>>();
        state.send_failures.clear();
        ids
    } else {
        Vec::new()
    };
    if binding_changed || session_changed {
        state.recent_entries = None;
    }
    state.session = Some(session.clone());
    let token = RuntimeToken {
        binding,
        generation: state.generation,
        session,
    };
    drop(state);
    if binding_changed || session_changed {
        clear_photos(app);
    }
    for id in typing_cleared {
        push_typing_update(app, token.clone(), id);
    }
    for id in failures_cleared {
        push_typing_update(app, token.clone(), id);
    }
    Ok((token, stale, binding_changed || session_changed))
}

async fn revalidate_after_await(app: &AppHandle, token: &RuntimeToken) -> bool {
    if !token_valid_sync(app, token) {
        return false;
    }
    let current = match app.state::<AppState>().session().await {
        Ok(Some(session)) => session,
        _ => return false,
    };
    if !Arc::ptr_eq(&current, &token.session) || current.binding != token.binding {
        return false;
    }
    crate::sync::blocking({
        let session = current.clone();
        move || session_displayable(&session)
    })
    .await
    .unwrap_or(false)
}

fn token_matches_state(app: &AppHandle, state: &RuntimeState, token: &RuntimeToken) -> bool {
    !state.stopped
        && state.generation == token.generation
        && state.binding.as_ref() == Some(&token.binding)
        && app
            .state::<AppState>()
            .head_generation
            .lock()
            .ok()
            .is_some_and(|fence| fence.accepts(token.generation))
}

fn token_valid_sync(app: &AppHandle, token: &RuntimeToken) -> bool {
    if token.session.cancel.is_cancelled() {
        return false;
    }
    if session_revoked(&token.session) {
        return false;
    }
    if app
        .state::<AppState>()
        .config()
        .ok()
        .and_then(|config| config.active_binding().cloned())
        .as_ref()
        != Some(&token.binding)
    {
        return false;
    }
    runtime(app)
        .state
        .lock()
        .ok()
        .is_some_and(|state| token_matches_state(app, &state, token))
}

async fn on_main_if_current<T, F>(
    app: &AppHandle,
    token: RuntimeToken,
    operation: F,
) -> BridgeResult<Option<T>>
where
    T: Send + 'static,
    F: FnOnce(&AppHandle) -> T + Send + 'static,
{
    let handle = app.clone();
    let (sender, receiver) = oneshot::channel();
    app.run_on_main_thread(move || {
        let result = token_valid_sync(&handle, &token).then(|| operation(&handle));
        let _ = sender.send(result);
    })
    .map_err(|_| BridgeError::new("heads", "Could not schedule floating conversation UI."))?;
    receiver
        .await
        .map_err(|_| BridgeError::new("heads", "Floating conversation UI did not respond."))
}

async fn on_main<T, F>(app: &AppHandle, operation: F) -> BridgeResult<T>
where
    T: Send + 'static,
    F: FnOnce(&AppHandle) -> T + Send + 'static,
{
    let handle = app.clone();
    let (sender, receiver) = oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = sender.send(operation(&handle));
    })
    .map_err(|_| BridgeError::new("heads", "Could not schedule floating conversation UI."))?;
    receiver
        .await
        .map_err(|_| BridgeError::new("heads", "Floating conversation UI did not respond."))
}

fn callback(app: &AppHandle) -> HeadCallback {
    let app = app.clone();
    Arc::new(move |event| match event {
        HeadEvent::Moved {
            conversation_id,
            generation,
            position,
            frame,
            settled,
        } => handle_moved_inline(&app, conversation_id, generation, position, frame, settled),
        HeadEvent::Activate {
            conversation_id,
            generation,
        } => {
            let _ = runtime(&app).actions.send(RuntimeAction::Activate {
                conversation_id,
                generation,
                mode: ActivationMode::Toggle,
            });
        }
        HeadEvent::Dismiss {
            conversation_id,
            generation,
        } => {
            let _ = runtime(&app).actions.send(RuntimeAction::Dismiss {
                conversation_id,
                generation,
            });
        }
    })
}

fn handle_moved_inline(
    app: &AppHandle,
    conversation_id: String,
    generation: u64,
    position: HeadPosition,
    frame: HeadFrame,
    settled: bool,
) {
    let Ok(_operation) = crate::admit_update_operation(&app.state::<AppState>()) else {
        return;
    };
    let valid = {
        let runtime = runtime(app);
        let Ok(mut state) = runtime.state.lock() else {
            return;
        };
        if state.stopped
            || state.generation != generation
            || !app
                .state::<AppState>()
                .head_generation
                .lock()
                .ok()
                .is_some_and(|fence| fence.accepts(generation))
        {
            return;
        }
        let Some(head) = state.heads.get_mut(&conversation_id) else {
            return;
        };
        if head.generation != generation {
            return;
        }
        head.position = position.clone();
        head.frame = Some(frame.clone());
        true
    };
    if valid {
        tray::place_head_panel(app, &conversation_id, &frame);
        if settled {
            let _ = runtime(app).actions.send(RuntimeAction::PersistMove {
                conversation_id,
                generation,
                position,
            });
        }
    }
}

async fn handle_action(app: &AppHandle, action: RuntimeAction) {
    let recent_action = matches!(
        action,
        RuntimeAction::Activate {
            mode: ActivationMode::ExpandOrFocus,
            ..
        }
    );
    let Ok(_operation) = crate::admit_update_operation(&app.state::<AppState>()) else {
        if recent_action {
            show_main_fallback(app).await;
        }
        return;
    };
    match action {
        RuntimeAction::Activate {
            conversation_id,
            generation,
            mode,
        } => {
            if recent_activation_failed(
                mode,
                activate(app, &conversation_id, generation, mode).await,
            ) {
                show_main_fallback(app).await;
            }
        }
        RuntimeAction::Dismiss {
            conversation_id,
            generation,
        } => {
            if let Err(error) = dismiss_generation(app, &conversation_id, generation).await {
                let message = error.message;
                let _ = on_main(app, move |app| {
                    crate::dialogs::inform(app, "Could not dismiss conversation", &message)
                })
                .await;
            }
        }
        RuntimeAction::PersistMove {
            conversation_id,
            generation,
            position,
        } => {
            let binding = {
                let runtime = runtime(app);
                let Ok(state) = runtime.state.lock() else {
                    return;
                };
                if state.stopped
                    || state.generation != generation
                    || !state.heads.get(&conversation_id).is_some_and(|head| {
                        head.generation == generation
                            && head.position.x == position.x
                            && head.position.y == position.y
                    })
                {
                    return;
                }
                state.binding.clone()
            };
            if let Some(binding) = binding {
                let _ =
                    runtime(app)
                        .pins
                        .update_position(&binding, &conversation_id, position.clone());
                if let Ok(new_layout) =
                    runtime(app)
                        .layouts
                        .save_head(&binding, &conversation_id, position)
                {
                    update_layout_cache(
                        app,
                        &binding,
                        generation,
                        &conversation_id,
                        Some(new_layout),
                    );
                }
            }
        }
    }
}

fn recent_activation_failed(mode: ActivationMode, activated: bool) -> bool {
    matches!(mode, ActivationMode::ExpandOrFocus) && !activated
}

async fn activate(
    app: &AppHandle,
    conversation_id: &str,
    generation: u64,
    mode: ActivationMode,
) -> bool {
    let _window_permit = match crate::lifecycle::permit_window_creation(app) {
        Ok(permit) => permit,
        Err(_) => return false,
    };
    let (binding, frame) = {
        let runtime = runtime(app);
        let Ok(state) = runtime.state.lock() else {
            return false;
        };
        if state.stopped || state.generation != generation {
            return false;
        }
        let Some(head) = state.heads.get(conversation_id) else {
            return false;
        };
        if head.generation != generation {
            return false;
        }
        let Some(binding) = state.binding.clone() else {
            return false;
        };
        (binding, head.frame.clone())
    };
    let session = match app.state::<AppState>().session().await {
        Ok(Some(session)) if session.binding == binding => session,
        _ => return false,
    };
    let token = RuntimeToken {
        binding,
        generation,
        session,
    };
    if !revalidate_after_await(app, &token).await {
        return false;
    }
    if load_layout_cache(app, &token, conversation_id)
        .await
        .is_err()
    {
        return false;
    }
    {
        let rt = runtime(app);
        let Ok(mut state) = rt.state.lock() else {
            return false;
        };
        if !token_matches_state(app, &state, &token) {
            return false;
        }
        let Some(head) = state.heads.get_mut(conversation_id) else {
            return false;
        };
        if head.generation != generation {
            return false;
        }
        head.panel = true;
    }
    let Ok(conversation) = ConversationId::from_str(conversation_id) else {
        return false;
    };
    let id = conversation_id.to_owned();
    let recent_binding = token.binding.clone();
    let result = on_main_if_current(app, token.clone(), move |app| {
        let label = format!("{}{}", tray::COMPOSER_PREFIX, id);
        if app
            .get_webview_window(&label)
            .and_then(|window| window.is_visible().ok())
            .unwrap_or(false)
        {
            match mode {
                ActivationMode::Toggle => ActivationUi::Collapse(crate::lifecycle::request_window(
                    app,
                    &label,
                    crate::lifecycle::Action::Collapse,
                )),
                ActivationMode::ExpandOrFocus => ActivationUi::Focus(
                    app.get_webview_window(&label)
                        .ok_or_else(|| {
                            BridgeError::new("not-found", "The conversation panel is unavailable.")
                        })
                        .and_then(|window| {
                            crate::window_chrome::show_when_ready(&window).map_err(|_| {
                                BridgeError::new(
                                    "heads",
                                    "Could not focus the floating conversation.",
                                )
                            })
                        }),
                ),
            }
        } else {
            let opened = tray::open_head_panel(app, conversation).map(|()| {
                if let Some(frame) = frame.as_ref() {
                    tray::place_head_panel(app, &id, frame);
                }
            });
            ActivationUi::Expand(opened)
        }
    })
    .await;
    if expansion_failed(&result) {
        set_panel(app, conversation_id, false);
    }
    match result {
        Ok(Some(ActivationUi::Expand(Err(_)))) => false,
        Ok(Some(ActivationUi::Expand(Ok(())))) => {
            acknowledge_failure(app, &token, conversation_id, ViewingScope::Panel);
            app.state::<AppState>()
                .record_recent(&recent_binding, conversation_id);
            let _ = app.emit(STATE_EVENT, ());
            true
        }
        Ok(Some(ActivationUi::Focus(Ok(())))) => {
            acknowledge_failure(app, &token, conversation_id, ViewingScope::Panel);
            let _ = app.emit(STATE_EVENT, ());
            true
        }
        Ok(Some(ActivationUi::Collapse(Ok(())))) => {
            let _ = app.emit(STATE_EVENT, ());
            true
        }
        // Busy/failed collapse keeps panel mode so a later click still uses the
        // draft-safe collapse path. Stale tokens are handled by the fence.
        Ok(Some(ActivationUi::Collapse(Err(_)) | ActivationUi::Focus(Err(_))))
        | Ok(None)
        | Err(_) => false,
    }
}

async fn show_main_fallback(app: &AppHandle) {
    let _ = on_main(app, tray::show_main).await;
}

fn schedule_main_fallback(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { show_main_fallback(&app).await });
}

pub async fn popout(app: &AppHandle, conversation: ConversationId) -> BridgeResult<PopoutResult> {
    let _operation = crate::admit_update_operation(&app.state::<AppState>())?;
    let app_state = app.state::<AppState>();
    let binding = app_state
        .config()?
        .active_binding()
        .cloned()
        .ok_or_else(BridgeError::no_session)?;
    let session = app_state.require_session().await?;
    if session.binding != binding {
        return Err(BridgeError::no_session());
    }
    let (token, stale, _) = begin_binding(app, binding.clone(), session.clone())?;
    if !stale.is_empty() {
        let _ = on_main_if_current(app, token.clone(), move |app| {
            for id in stale {
                crate::close_media_viewers_for_parent(
                    app,
                    &format!("{}{}", tray::COMPOSER_PREFIX, id),
                );
                let _ = windows::hide_head(app, &id);
            }
        })
        .await?;
    }
    let photos = runtime(app).photos.clone();
    let head = crate::sync::blocking({
        let session = session.clone();
        move || resolve_one(&session, conversation, &photos)
    })
    .await?;
    if !revalidate_after_await(app, &token).await {
        return Err(BridgeError::new(
            "session-changed",
            "The active account changed while opening the floating conversation.",
        ));
    }

    let saved = runtime(app)
        .pins
        .pins(&binding)
        .map_err(|message| BridgeError::new("heads", message))?;
    let persisted = saved
        .iter()
        .find(|pin| pin.conversation_id == head.id)
        .cloned();
    let saved_layout = load_layout_cache(app, &token, &head.id).await?;
    let prior = runtime(app)
        .state
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .heads
        .get(&head.id)
        .cloned();
    if pin_limit_reached(
        saved.len(),
        runtime(app)
            .state
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .heads
            .len(),
        persisted.is_some(),
    ) {
        return Err(BridgeError::new(
            "limit",
            "You can pin at most eight floating conversations.",
        ));
    }
    let position = prior
        .as_ref()
        .map(|active| active.position.clone())
        .or_else(|| persisted.as_ref().map(|pin| pin.position.clone()))
        .or_else(|| saved_layout.and_then(|layout| layout.head))
        .unwrap_or_else(|| next_position(saved.len()));
    let spec = {
        let rt = runtime(app);
        let mut state = rt.state.lock().map_err(|_| BridgeError::host_state())?;
        if !token_matches_state(app, &state, &token) {
            return Err(BridgeError::new(
                "session-changed",
                "The active account changed.",
            ));
        }
        let failed_message = head.failed_message.clone();
        state.send_failures.seed(
            &head.id,
            persisted
                .as_ref()
                .and_then(|pin| pin.acknowledged_failure.as_deref()),
        );
        state
            .send_failures
            .acknowledge(&head.id, failed_message.as_deref());
        state.heads.insert(
            head.id.clone(),
            ActiveHead {
                position: position.clone(),
                frame: prior.as_ref().and_then(|active| active.frame.clone()),
                generation: token.generation,
                panel: true,
                name: head.name,
                glyph: head.glyph,
                color: head.color,
                label: head.label,
                unread: head.unread,
                failed_message: head.failed_message,
            },
        );
        head_spec(
            &state.typing,
            &state.send_failures,
            &head.id,
            &state.heads[&head.id],
        )
    };
    persist_acknowledgement(app, &token, &spec.conversation_id);

    let mut spec_for_ui = spec.clone();
    let native = on_main_if_current(app, token.clone(), move |app| {
        if let Ok(state) = runtime(app).state.lock() {
            spec_for_ui.typing = state.typing.displayed(&spec_for_ui.conversation_id);
            if let Some(head) = state.heads.get(&spec_for_ui.conversation_id) {
                spec_for_ui.send_failed = state
                    .send_failures
                    .displayed(&spec_for_ui.conversation_id, head.failed_message.as_deref());
            }
        }
        windows::show_head(app, &spec_for_ui, callback(app))
    })
    .await?;
    let native_error = match native {
        Some(Ok(())) => None,
        Some(Err(message)) => Some(message),
        None => {
            restore_prior(app, &spec.conversation_id, prior.clone());
            return Err(BridgeError::new(
                "session-changed",
                "The active account changed.",
            ));
        }
    };
    if let Some(message) = native_error {
        restore_prior(app, &spec.conversation_id, prior);
        let _ = on_main_if_current(app, token, move |app| {
            tray::open_composer(app, conversation)
        })
        .await?;
        return Ok(PopoutResult {
            head_created: false,
            warning: Some(format!(
                "Could not show a floating conversation; opened a composer instead. {message}"
            )),
        });
    }

    let created = persisted.is_none();
    if created {
        if let Err(message) = runtime(app).pins.upsert(
            &binding,
            Pin {
                conversation_id: spec.conversation_id.clone(),
                position: spec.position.clone(),
                acknowledged_failure: None,
            },
        ) {
            if prior.is_none() {
                let id = spec.conversation_id.clone();
                let _ = on_main_if_current(app, token.clone(), move |app| {
                    crate::close_media_viewers_for_parent(
                        app,
                        &format!("{}{}", tray::COMPOSER_PREFIX, id),
                    );
                    windows::hide_head(app, &id)
                })
                .await;
            }
            restore_prior(app, &spec.conversation_id, prior);
            return Err(BridgeError::new("heads", message));
        }
        persist_acknowledgement(app, &token, &spec.conversation_id);
    }

    let id = spec.conversation_id.clone();
    let frame = runtime(app)
        .state
        .lock()
        .ok()
        .and_then(|state| state.heads.get(&id).and_then(|head| head.frame.clone()));
    let panel = on_main_if_current(app, token.clone(), move |app| {
        tray::open_head_panel(app, conversation)?;
        if let Some(frame) = frame.as_ref() {
            tray::place_head_panel(app, &id, frame);
        }
        Ok::<_, BridgeError>(())
    })
    .await?;
    if !matches!(panel, Some(Ok(()))) {
        set_panel(app, &spec.conversation_id, false);
        let _ = app.emit(STATE_EVENT, ());
        return match panel {
            Some(Err(error)) => Err(error),
            _ => Err(BridgeError::new(
                "session-changed",
                "The active account changed.",
            )),
        };
    }
    app.state::<AppState>()
        .record_recent(&binding, &spec.conversation_id);
    update_tray_menu(app, Some(&token)).await;
    let _ = app.emit(STATE_EVENT, ());
    Ok(PopoutResult {
        head_created: created,
        warning: None,
    })
}

fn next_position(count: usize) -> HeadPosition {
    let slot = count.min(MAX_HEADS - 1) as f64;
    HeadPosition {
        monitor: None,
        x: 24.0 + slot * 68.0,
        y: 80.0 + (slot % 3.0) * 12.0,
    }
}

fn restore_prior(app: &AppHandle, conversation_id: &str, prior: Option<ActiveHead>) {
    if let Ok(mut state) = runtime(app).state.lock() {
        if let Some(prior) = prior {
            state.heads.insert(conversation_id.to_owned(), prior);
        } else {
            state.heads.remove(conversation_id);
            state.send_failures.forget(conversation_id);
        }
    }
}

pub async fn dismiss(app: &AppHandle, conversation_id: &str) -> BridgeResult<()> {
    let _operation = crate::admit_update_operation(&app.state::<AppState>())?;
    let generation = runtime(app)
        .state
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .heads
        .get(conversation_id)
        .map(|head| head.generation)
        .ok_or_else(|| BridgeError::new("not-found", "The floating conversation is not open."))?;
    dismiss_generation(app, conversation_id, generation).await
}

/// A deleted local draft cannot retain a persistent floating-head pin. The normal state hint
/// refresh hides any native head and updates the tray after this durable cleanup.
pub fn forget_deleted_conversation(
    app: &AppHandle,
    binding: &crate::credentials::Binding,
    conversation_id: &str,
) -> BridgeResult<()> {
    let _operation = crate::admit_update_operation(&app.state::<AppState>())?;
    runtime(app)
        .pins
        .remove(binding, conversation_id)
        .map_err(|message| BridgeError::new("heads", message))?;
    if let Ok(mut state) = runtime(app).state.lock() {
        state.heads.remove(conversation_id);
        state.send_failures.forget(conversation_id);
    }
    Ok(())
}

pub(crate) async fn dismiss_generation(
    app: &AppHandle,
    conversation_id: &str,
    generation: u64,
) -> BridgeResult<()> {
    let binding = {
        let rt = runtime(app);
        let state = rt.state.lock().map_err(|_| BridgeError::host_state())?;
        state
            .heads
            .get(conversation_id)
            .filter(|head| head.generation == generation)
            .ok_or_else(|| {
                BridgeError::new("not-found", "The floating conversation is not open.")
            })?;
        state.binding.clone().ok_or_else(BridgeError::no_session)?
    };
    let session = app
        .state::<AppState>()
        .session()
        .await?
        .filter(|session| session.binding == binding)
        .ok_or_else(BridgeError::no_session)?;
    let token = RuntimeToken {
        binding: binding.clone(),
        generation,
        session,
    };
    if !revalidate_after_await(app, &token).await {
        return Err(BridgeError::new(
            "session-changed",
            "The active account changed.",
        ));
    }
    let removed = runtime(app)
        .pins
        .remove(&binding, conversation_id)
        .map_err(|message| BridgeError::new("heads", message))?;
    let id = conversation_id.to_owned();
    let hidden = on_main_if_current(app, token.clone(), move |app| {
        crate::close_media_viewers_for_parent(app, &format!("{}{}", tray::COMPOSER_PREFIX, id));
        windows::hide_head(app, &id)
    })
    .await?;
    if !matches!(hidden, Some(Ok(()))) {
        if let Some(pin) = removed {
            let _ = runtime(app).pins.upsert(&binding, pin);
        }
        return match hidden {
            Some(Err(message)) => Err(BridgeError::new("heads", message)),
            _ => Err(BridgeError::new(
                "session-changed",
                "The active account changed.",
            )),
        };
    }
    if let Ok(mut state) = runtime(app).state.lock() {
        if state
            .heads
            .get(conversation_id)
            .is_some_and(|head| head.generation == generation)
        {
            state.heads.remove(conversation_id);
            state.send_failures.forget(conversation_id);
        }
    }
    let id = conversation_id.to_owned();
    let cleanup = on_main_if_current(app, token.clone(), move |app| {
        let label = format!("{}{}", tray::COMPOSER_PREFIX, id);
        let hidden_composer = app
            .get_webview_window(&label)
            .is_some_and(|window| !window.is_visible().unwrap_or(false));
        tray::set_head_panel(app, &id, false);
        if hidden_composer {
            if let Err(error) =
                crate::lifecycle::request_window(app, &label, crate::lifecycle::Action::Close)
            {
                if let Some(window) = app.get_webview_window(&label) {
                    let _ = crate::window_chrome::recover_window(&window);
                }
                return Err(error);
            }
        }
        Ok(())
    })
    .await?;
    update_tray_menu(app, Some(&token)).await;
    let _ = app.emit(STATE_EVENT, ());
    match cleanup {
        Some(result) => result,
        None => Err(BridgeError::new(
            "session-changed",
            "The active account changed while cleaning up the composer.",
        )),
    }
}

pub fn snapshot(app: &AppHandle, window_label: &str) -> (Vec<String>, bool) {
    let runtime = runtime(app);
    let Ok(state) = runtime.state.lock() else {
        return (Vec::new(), false);
    };
    let panel = tray::composer_conversation(window_label).is_some_and(|id| {
        state
            .heads
            .get(&id.to_string())
            .is_some_and(|head| head.panel)
    });
    let ids = state
        .binding
        .as_ref()
        .and_then(|binding| runtime.pins.pins(binding).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|pin| state.heads.contains_key(&pin.conversation_id))
        .map(|pin| pin.conversation_id)
        .collect();
    (ids, panel)
}

/// Load once before expansion, including activation of a pin restored at startup.
/// Native drag callbacks subsequently consult memory only.
async fn load_layout_cache(
    app: &AppHandle,
    token: &RuntimeToken,
    conversation_id: &str,
) -> BridgeResult<Option<HeadLayout>> {
    let handle = app.clone();
    let binding = token.binding.clone();
    let id = conversation_id.to_owned();
    let layout = crate::sync::blocking(move || {
        Ok(runtime(&handle)
            .layouts
            .layout(&binding, &id)
            .ok()
            .flatten())
    })
    .await?;
    if !revalidate_after_await(app, token).await {
        return Err(BridgeError::new(
            "session-changed",
            "The active account changed.",
        ));
    }
    update_layout_cache(
        app,
        &token.binding,
        token.generation,
        conversation_id,
        layout.clone(),
    );
    Ok(layout)
}

/// Update the cache after a write.
fn update_layout_cache(
    app: &AppHandle,
    binding: &Binding,
    generation: u64,
    conversation_id: &str,
    layout: Option<crate::heads::HeadLayout>,
) {
    if let Ok(mut state) = runtime(app).state.lock() {
        cache_layout(&mut state, binding, generation, conversation_id, layout);
    }
}

fn cache_layout(
    state: &mut RuntimeState,
    binding: &Binding,
    generation: u64,
    id: &str,
    layout: Option<HeadLayout>,
) {
    if state.stopped || state.generation != generation || state.binding.as_ref() != Some(binding) {
        return;
    }
    let cached = state.layout_cache.get(id).and_then(Option::as_ref);
    if cached.is_some_and(|cached| {
        layout
            .as_ref()
            .is_none_or(|new| new.updated < cached.updated)
    }) {
        return;
    }
    state.layout_cache.insert(id.to_owned(), layout);
}

pub fn panel_layout(app: &AppHandle, conversation_id: &str) -> Option<crate::heads::PanelLayout> {
    let runtime = runtime(app);
    let state = runtime.state.lock().ok()?;
    // Check cache first (N2: no disk I/O on follow path)
    if let Some(cached) = state.layout_cache.get(conversation_id) {
        return cached.as_ref().and_then(|layout| layout.panel.clone());
    }
    None
}

pub fn is_panel(app: &AppHandle, window_label: &str) -> bool {
    let Some(id) = tray::composer_conversation(window_label) else {
        return false;
    };
    runtime(app)
        .state
        .lock()
        .ok()
        .and_then(|state| state.heads.get(&id.to_string()).map(|head| head.panel))
        .unwrap_or(false)
}

pub(crate) fn head_generation(app: &AppHandle, conversation_id: &str) -> BridgeResult<u64> {
    runtime(app)
        .state
        .lock()
        .map_err(|_| BridgeError::host_state())?
        .heads
        .get(conversation_id)
        .map(|head| head.generation)
        .ok_or_else(|| BridgeError::new("not-found", "The floating conversation is not open."))
}

pub fn conversation_is_panel(app: &AppHandle, conversation_id: &str) -> bool {
    runtime(app)
        .state
        .lock()
        .ok()
        .and_then(|state| state.heads.get(conversation_id).map(|head| head.panel))
        .unwrap_or(false)
}

/// True while `conversation_id` has a live chat head (collapsed or expanded) for the active
/// binding. Safe to call when the head runtime is not managed (returns false).
pub fn has_head(app: &AppHandle, conversation_id: &str) -> bool {
    app.try_state::<HeadRuntime>()
        .and_then(|runtime| {
            runtime
                .state
                .lock()
                .ok()
                .map(|state| !state.stopped && state.heads.contains_key(conversation_id))
        })
        .unwrap_or(false)
}

/// Opens a tray recent without changing a visible panel's layout or collapsing it.
pub fn open_recent_from_tray(app: &AppHandle, conversation_id: &str) {
    if app
        .state::<AppState>()
        .app_locked
        .load(std::sync::atomic::Ordering::Acquire)
    {
        schedule_main_fallback(app);
        return;
    }
    let Ok(conversation) = ConversationId::from_str(conversation_id) else {
        schedule_main_fallback(app);
        return;
    };
    let Some(runtime) = app.try_state::<HeadRuntime>() else {
        schedule_main_fallback(app);
        return;
    };
    let action = runtime.state.lock().ok().and_then(|state| {
        state
            .heads
            .get(conversation_id)
            .map(|head| RuntimeAction::Activate {
                conversation_id: conversation_id.to_owned(),
                generation: head.generation,
                mode: ActivationMode::ExpandOrFocus,
            })
    });
    if let Some(action) = action {
        if runtime.actions.send(action).is_err() {
            schedule_main_fallback(app);
        }
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let opened = match crate::lifecycle::permit_window_creation(&app) {
            Ok(_permit) => popout(&app, conversation).await,
            Err(error) => Err(error),
        };
        if opened.is_err() {
            show_main_fallback(&app).await;
        }
    });
}

pub fn activate_from_tray(app: &AppHandle, conversation_id: &str) {
    let event = runtime(app).state.lock().ok().and_then(|state| {
        state
            .heads
            .get(conversation_id)
            .map(|head| RuntimeAction::Activate {
                conversation_id: conversation_id.to_owned(),
                generation: head.generation,
                mode: ActivationMode::Toggle,
            })
    });
    if let Some(event) = event {
        let _ = runtime(app).actions.send(event);
    }
}

pub fn dismiss_from_tray(app: &AppHandle, conversation_id: &str) {
    let event = runtime(app).state.lock().ok().and_then(|state| {
        state
            .heads
            .get(conversation_id)
            .map(|head| RuntimeAction::Dismiss {
                conversation_id: conversation_id.to_owned(),
                generation: head.generation,
            })
    });
    if let Some(event) = event {
        let _ = runtime(app).actions.send(event);
    }
}

pub async fn reset(app: &AppHandle) -> BridgeResult<()> {
    let _operation = crate::admit_update_operation(&app.state::<AppState>())?;
    {
        let runtime = runtime(app);
        let mut state = runtime
            .state
            .lock()
            .map_err(|_| BridgeError::host_state())?;
        let generation = app
            .state::<AppState>()
            .head_generation
            .lock()
            .map_err(|_| BridgeError::host_state())?
            .invalidate();
        state.generation = generation;
        state.heads.clear();
        state.typing.clear();
        state.send_failures.clear();
        state.binding = None;
        state.session = None;
        state.queued = false;
        state.recent_entries = None;
        state.layout_cache.clear(); // Clear cache on binding change (N2)
    }
    clear_photos(app);
    on_main(app, windows::clear_heads).await?;
    update_tray_menu(app, None).await;
    update_recent_tray_menu(app, None).await;
    Ok(())
}

pub fn clear_on_exit(app: &AppHandle) {
    if let Ok(mut state) = runtime(app).state.lock() {
        if let Ok(mut fence) = app.state::<AppState>().head_generation.lock() {
            state.generation = fence.invalidate();
        }
        state.stopped = true;
        state.refreshing = false;
        state.queued = false;
        state.heads.clear();
        state.typing.clear();
        state.send_failures.clear();
        state.binding = None;
        state.session = None;
        state.recent_entries = None;
        state.layout_cache.clear(); // Clear cache on exit (N2)
    }
    clear_photos(app);
    let app = app.clone();
    let _ = app
        .clone()
        .run_on_main_thread(move || windows::clear_heads(&app));
}

async fn clear_visible(app: &AppHandle) {
    let labels = {
        let runtime = runtime(app);
        let Ok(mut state) = runtime.state.lock() else {
            return;
        };
        if state.stopped {
            return;
        }
        let generation = app
            .state::<AppState>()
            .head_generation
            .lock()
            .map(|mut fence| fence.invalidate())
            .unwrap_or(state.generation);
        clear_visible_state(&mut state, generation)
    };
    clear_photos(app);
    let _ = on_main(app, move |app| {
        windows::clear_heads(app);
        hide_visible_panels(&labels, |label| {
            crate::close_media_viewers_for_parent(app, label);
            if let Some(window) = app.get_webview_window(label) {
                let _ = window.hide();
            }
        });
    })
    .await;
    update_tray_menu(app, None).await;
    update_recent_tray_menu(app, None).await;
}

fn clear_visible_state(state: &mut RuntimeState, generation: u64) -> Vec<String> {
    state.generation = generation;
    let labels = state
        .heads
        .iter()
        .filter(|(_, head)| head.panel)
        .map(|(id, _)| format!("{}{}", tray::COMPOSER_PREFIX, id))
        .collect::<Vec<_>>();
    state.heads.clear();
    state.typing.clear();
    state.send_failures.clear();
    state.binding = None;
    state.session = None;
    state.recent_entries = None;
    state.layout_cache.clear();
    labels
}

fn hide_visible_panels<F>(labels: &[String], mut hide: F)
where
    F: FnMut(&str),
{
    for label in labels {
        hide(label);
    }
}

fn set_panel(app: &AppHandle, conversation_id: &str, panel: bool) {
    if let Ok(mut state) = runtime(app).state.lock() {
        if let Some(head) = state.heads.get_mut(conversation_id) {
            head.panel = panel;
        }
    }
}

async fn update_tray_menu(app: &AppHandle, token: Option<&RuntimeToken>) {
    let entries = runtime(app)
        .state
        .lock()
        .ok()
        .map(|state| {
            tray_entries(
                state
                    .heads
                    .iter()
                    .map(|(id, head)| (id.clone(), head.name.clone()))
                    .collect(),
            )
        })
        .unwrap_or_default();
    if runtime(app)
        .state
        .lock()
        .ok()
        .is_some_and(|state| state.menu_entries.as_ref() == Some(&entries))
    {
        return;
    }
    let applied = if let Some(token) = token {
        on_main_if_current(app, token.clone(), {
            let entries = entries.clone();
            move |app| tray::set_floating_conversations(app, &entries)
        })
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
    } else {
        on_main(app, {
            let entries = entries.clone();
            move |app| tray::set_floating_conversations(app, &entries)
        })
        .await
        .unwrap_or(false)
    };
    if applied {
        if let Ok(mut state) = runtime(app).state.lock() {
            state.menu_entries = Some(entries);
        }
    }
}

async fn refresh_recent_conversations(
    app: &AppHandle,
    token: &RuntimeToken,
    ids: &[String],
    heads: &HashMap<String, HeadData>,
    protected: &HashSet<String>,
) {
    let (entries, missing) = reconcile_recent_heads(ids, heads);
    for missing in missing {
        if protected.contains(&missing) {
            continue;
        }
        if app
            .state::<AppState>()
            .recents
            .remove(&token.binding, &missing)
            .is_err()
        {
            return;
        }
    }
    if let Ok(mut state) = runtime(app).state.lock() {
        if !token_matches_state(app, &state, token) {
            return;
        }
        state.recent_entries = Some(entries);
    }
    update_recent_tray_menu(app, Some(token)).await;
}

/// Resolved recent tray entries (ID, display name) paired with recent IDs whose conversation is missing.
type ResolvedRecentEntries = (Vec<(String, String)>, Vec<String>);

fn reconcile_recent_heads(
    ids: &[String],
    heads: &HashMap<String, HeadData>,
) -> ResolvedRecentEntries {
    let names = heads
        .iter()
        .map(|(id, head)| (id.clone(), head.name.clone()))
        .collect::<HashMap<_, _>>();
    let missing = ids
        .iter()
        .filter(|id| !names.contains_key(*id))
        .cloned()
        .collect();
    (recent_tray_entries(ids, &names), missing)
}

async fn clear_recent_entries(app: &AppHandle, token: &RuntimeToken) {
    let cleared = runtime(app).state.lock().ok().is_some_and(|mut state| {
        if state.stopped
            || state.generation != token.generation
            || state.binding.as_ref() != Some(&token.binding)
        {
            return false;
        }
        state.recent_entries = Some(Vec::new());
        true
    });
    if cleared {
        update_recent_tray_menu(app, None).await;
    }
}

async fn update_recent_tray_menu(app: &AppHandle, token: Option<&RuntimeToken>) {
    if let Some(token) = token {
        let token = token.clone();
        let _ = on_main_if_current(app, token.clone(), move |app| {
            let runtime = runtime(app);
            let Ok(mut state) = runtime.state.lock() else {
                return false;
            };
            if !token_matches_state(app, &state, &token) {
                return false;
            }
            let entries = state.recent_entries.clone().unwrap_or_default();
            if !recent_menu_changed(state.applied_recent_entries.as_deref(), &entries) {
                return false;
            }
            if !tray::set_recent_conversations(app, &entries) {
                return false;
            }
            state.applied_recent_entries = Some(entries);
            true
        })
        .await;
    } else {
        let _ = on_main(app, move |app| {
            let runtime = runtime(app);
            let Ok(mut state) = runtime.state.lock() else {
                return false;
            };
            let entries = state.recent_entries.clone().unwrap_or_default();
            if !recent_menu_changed(state.applied_recent_entries.as_deref(), &entries) {
                return false;
            }
            if !tray::set_recent_conversations(app, &entries) {
                return false;
            }
            state.applied_recent_entries = Some(entries);
            true
        })
        .await;
    }
}

fn recent_menu_changed(applied: Option<&[(String, String)]>, desired: &[(String, String)]) -> bool {
    applied != Some(desired)
}

fn recent_tray_entries(ids: &[String], names: &HashMap<String, String>) -> Vec<(String, String)> {
    let mut labels = HashMap::new();
    ids.iter()
        .filter_map(|id| {
            names.get(id).map(|name| {
                let count = labels.entry(name.clone()).or_insert(0usize);
                *count += 1;
                let label = if *count > 1 {
                    format!("{name} {count}")
                } else {
                    name.clone()
                };
                (id.clone(), label)
            })
        })
        .collect()
}

fn tray_entries(mut entries: Vec<(String, String)>) -> Vec<(String, String)> {
    entries.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let mut labels = HashMap::new();
    for (_, label) in &mut entries {
        let count = labels.entry(label.clone()).or_insert(0usize);
        *count += 1;
        if *count > 1 {
            *label = format!("{label} {count}");
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    #[test]
    fn main_acknowledgement_keeps_refusals_and_panel_acknowledgement_clears_them() {
        let mut failures = SendFailures::default();
        failures.refuse("chat");
        failures.acknowledge_main("chat", Some("message"));
        assert!(failures.displayed("chat", Some("message")));
        failures.accept("chat");
        assert!(!failures.displayed("chat", Some("message")));
        failures.refuse("chat");
        failures.acknowledge("chat", Some("message"));
        assert!(!failures.displayed("chat", Some("message")));
    }

    #[test]
    fn collapse_pending_is_not_a_viewed_panel() {
        let mut failures = SendFailures::default();
        failures.refuse("chat");
        let scope = viewing_scope(true, true, false);
        assert_eq!(scope, None);
        if let Some(scope) = scope {
            failures.acknowledge_for_scope("chat", None, scope);
        }
        assert!(failures.displayed("chat", None));
        assert_eq!(
            viewing_scope(true, true, true),
            Some(ViewingScope::MainWindow)
        );
        assert_eq!(viewing_scope(true, false, true), Some(ViewingScope::Panel));
        assert_eq!(viewing_scope(false, false, false), None);
    }

    #[test]
    fn persisted_acknowledgement_does_not_override_memory() {
        let mut failures = SendFailures::default();
        failures.seed("chat", Some("first"));
        failures.seed("chat", Some("second"));
        assert!(!failures.displayed("chat", Some("first")));
        assert!(failures.displayed("chat", Some("second")));
    }

    #[test]
    fn originating_session_identity_fences_late_results() {
        let original = Arc::new(());
        let same = original.clone();
        let replacement = Arc::new(());
        assert!(same_session(&original, &same));
        assert!(!same_session(&original, &replacement));
        let mut failures = SendFailures::default();
        assert!(!failures.settle("chat", false, false, true, true));
        assert!(!failures.displayed("chat", None));
        assert!(!failures.settle("chat", true, false, true, false));
        assert!(!failures.settle("chat", true, false, false, true));
        assert!(!failures.displayed("chat", None));
        failures.refuse("chat");
        assert!(!failures.settle("chat", false, true, false, true));
        assert!(failures.displayed("chat", None));
        assert!(failures.settle("chat", true, true, false, true));
        assert!(!failures.displayed("chat", None));
    }

    #[test]
    fn send_failures_acknowledge_and_track_new_messages() {
        let mut failures = super::SendFailures::default();
        assert!(failures.refuse("chat"));
        assert!(failures.displayed("chat", None));
        failures.acknowledge("chat", Some("first"));
        assert!(!failures.displayed("chat", Some("first")));
        assert!(failures.displayed("chat", Some("second")));
        failures.refuse("chat");
        failures.accept("chat");
        assert!(failures.displayed("chat", Some("second")));
        failures.remap("chat", "other");
        assert!(!failures.displayed("other", Some("first")));
        failures.forget("other");
        assert!(failures.displayed("other", Some("first")));
        failures.refuse("chat");
        failures.clear();
        assert!(!failures.displayed("chat", None));
    }

    #[test]
    fn only_outgoing_failed_states_mark_the_latest_message() {
        use peppy_client_core::{Direction, SendState};
        for state in [
            SendState::FailedBeforeSubmission,
            SendState::FailedConfirmed,
            SendState::OutcomeUnknown,
        ] {
            assert!(super::is_failed_send(Direction::Outgoing, Some(state)));
            assert!(!super::is_failed_send(Direction::Incoming, Some(state)));
        }
        assert!(!super::is_failed_send(Direction::Outgoing, None));
        assert!(!super::is_failed_send(
            Direction::Outgoing,
            Some(SendState::Sent)
        ));
    }
    use super::*;

    #[test]
    fn failed_send_takes_priority_over_failed_reaction() {
        let reaction = || Ok(Some("reaction".to_owned()));
        assert_eq!(
            select_head_failure(Some("send".into()), || panic!("not looked up")).unwrap(),
            Some("send".into())
        );
        assert_eq!(
            select_head_failure(None, reaction).unwrap(),
            Some("reaction".into())
        );
        assert_eq!(select_head_failure(None, || Ok(None)).unwrap(), None);
    }

    #[test]
    fn host_redirect_lookup_failure_protects_only_failed_id() {
        let failed = ConversationId::new();
        let source = ConversationId::new();
        let target = ConversationId::new();
        let (redirects, protected) = collect_redirects(
            [failed.to_string(), source.to_string()]
                .into_iter()
                .collect(),
            |id| {
                if id == failed {
                    Err(BridgeError::host_state())
                } else {
                    Ok(Some(target))
                }
            },
        );
        assert_eq!(redirects, vec![(source.to_string(), target.to_string())]);
        assert_eq!(protected, [failed.to_string()].into_iter().collect());
    }
    use crate::heads::{GenerationFence, PanelLayout};
    use crate::tests::{fixture_at, input, open};
    use peppy_client_core::IncomingSms;
    use std::sync::atomic::Ordering;

    fn locally_saved_draft_without_sync_unlock() -> (
        crate::tests::Fixture,
        Arc<crate::session::Session>,
        ConversationId,
    ) {
        let fixture = fixture_at("http://127.0.0.1:9");
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        let draft = session
            .save_draft(&input(
                "draft-new",
                "",
                "offline draft",
                &["+15555550100"],
                "0",
            ))
            .unwrap();
        let conversation = ConversationId::from_str(&draft.conversation_id).unwrap();
        (fixture, session, conversation)
    }

    fn photo_cache() -> PhotoCache {
        Arc::new(Mutex::new(PhotoCacheEntries {
            entries: HashMap::new(),
            epoch: 0,
        }))
    }

    #[test]
    fn recent_entries_preserve_newest_order_and_disambiguate_labels() {
        let ids = vec!["newest".into(), "missing".into(), "older".into()];
        let names = HashMap::from([
            ("newest".into(), "Ada".into()),
            ("older".into(), "Ada".into()),
        ]);

        assert_eq!(
            recent_tray_entries(&ids, &names),
            vec![
                ("newest".into(), "Ada".into()),
                ("older".into(), "Ada 2".into()),
            ]
        );
    }

    #[test]
    fn nondisplayable_session_yields_no_authoritative_head_resolution() {
        let (_fixture, session, conversation) = locally_saved_draft_without_sync_unlock();
        let persisted = [conversation.to_string()];
        session.cancel.cancel();

        let resolution = resolve_heads(
            &session,
            &persisted.iter().cloned().collect(),
            &photo_cache(),
        )
        .unwrap();

        assert!(resolution.is_none());
    }

    #[test]
    fn unchanged_recent_menu_does_not_require_a_native_rebuild() {
        let entries = vec![("conversation".into(), "Ada".into())];

        assert!(!recent_menu_changed(Some(&entries), &entries));
        assert!(recent_menu_changed(Some(&entries), &[]));
        assert!(recent_menu_changed(None, &entries));
    }

    #[test]
    fn only_failed_recent_expansion_requires_main_window_fallback() {
        assert!(recent_activation_failed(
            ActivationMode::ExpandOrFocus,
            false
        ));
        assert!(!recent_activation_failed(
            ActivationMode::ExpandOrFocus,
            true
        ));
        assert!(!recent_activation_failed(ActivationMode::Toggle, false));
    }

    #[test]
    fn head_face_reveals_resolved_photo_and_group_names() {
        let photo = HeadPhoto {
            edge: 1,
            rgba: Arc::from([0, 0, 0, 255]),
        };
        assert!(matches!(
            head_face(
                AvatarKind::Person,
                Some(ResolvedFace {
                    display_name: "Ada Lovelace".into(),
                    photo: Some(photo),
                }),
                Some("+18015551234"),
                &[],
                false,
            ),
            (HeadGlyph::Photo(_), label) if label == "Ada Lovelace"
        ));
        assert_eq!(
            head_face(AvatarKind::Group, None, None, &["Ada".into()], false),
            (HeadGlyph::Group, "Ada".into())
        );
    }

    #[test]
    fn head_face_selects_glyphs_and_labels() {
        let photo = HeadPhoto {
            edge: 1,
            rgba: Arc::from([0, 0, 0, 255]),
        };
        assert!(matches!(
            head_face(
                AvatarKind::Person,
                Some(ResolvedFace {
                    display_name: "Ada".into(),
                    photo: Some(photo),
                }),
                None,
                &[],
                false,
            ),
            (HeadGlyph::Photo(_), label) if label == "Ada"
        ));
        assert_eq!(
            head_face(
                AvatarKind::Person,
                Some(ResolvedFace {
                    display_name: "Ada Lovelace".into(),
                    photo: None,
                }),
                None,
                &[],
                false,
            ),
            (HeadGlyph::Initials("AL".into()), "Ada Lovelace".into())
        );
        assert_eq!(
            head_face(
                AvatarKind::Person,
                Some(ResolvedFace {
                    display_name: "+18015551234".into(),
                    photo: None,
                }),
                None,
                &[],
                false,
            ),
            (HeadGlyph::Person, "+18015551234".into())
        );
        assert_eq!(
            head_face(AvatarKind::Person, None, None, &[], false),
            (HeadGlyph::Person, "Conversation".into())
        );
        assert_eq!(
            head_face(AvatarKind::Person, None, Some("+18015551234"), &[], false,),
            (HeadGlyph::Person, "+18015551234".into())
        );
        assert_eq!(
            head_face(
                AvatarKind::Group,
                None,
                None,
                &["Ada".into(), "+18015551234".into()],
                false,
            ),
            (HeadGlyph::Group, "Ada, +18015551234".into())
        );
    }

    #[test]
    fn recipientless_drafts_and_duplicate_labels_are_safe_in_the_tray() {
        assert_eq!(
            head_face(AvatarKind::Person, None, None, &[], true),
            (HeadGlyph::Person, "New message".into())
        );
        assert_eq!(
            tray_entries(vec![
                ("b".into(), "Conversation".into()),
                ("a".into(), "Conversation".into()),
                ("draft".into(), "New message".into()),
            ]),
            vec![
                ("a".into(), "Conversation".into()),
                ("b".into(), "Conversation 2".into()),
                ("draft".into(), "New message".into()),
            ]
        );
    }

    #[test]
    fn programmatic_geometry_guard_tolerates_two_physical_pixels() {
        let written = PhysicalRect {
            x: 10,
            y: 20,
            width: 680,
            height: 880,
        };
        assert!(same_geometry(
            written,
            PhysicalRect {
                x: 12,
                y: 18,
                width: 682,
                height: 878
            }
        ));
        assert!(!same_geometry(written, PhysicalRect { x: 13, ..written }));
    }

    #[test]
    fn matching_recorded_geometry_skips_persist() {
        let rect = PhysicalRect {
            x: 10,
            y: 20,
            width: 340,
            height: 440,
        };
        assert!(skip_panel_persist(Some(rect), rect));
        assert!(!skip_panel_persist(None, rect));
    }

    #[test]
    fn windowed_panel_without_topmost_restores_it() {
        assert!(should_restore_panel_topmost(Some(false), Some(false)));
        assert!(!should_restore_panel_topmost(Some(true), Some(false)));
        assert!(!should_restore_panel_topmost(Some(false), Some(true)));
        assert!(!should_restore_panel_topmost(None, Some(false)));
        assert!(!should_restore_panel_topmost(Some(false), None));
    }

    #[test]
    fn panel_geometry_is_persisted_in_logical_points_relative_to_head() {
        let frame = HeadFrame {
            x: 200.0,
            y: 100.0,
            width: 112.0,
            height: 112.0,
            work_x: 0.0,
            work_y: 0.0,
            work_width: 2000.0,
            work_height: 1600.0,
            scale_factor: 2.0,
        };
        let layout = panel_layout_from_physical(
            &frame,
            PhysicalRect {
                x: 240,
                y: 228,
                width: 680,
                height: 880,
            },
        );
        assert_eq!(
            (
                layout.width,
                layout.height,
                layout.offset_x,
                layout.offset_y
            ),
            (340.0, 440.0, 20.0, 64.0)
        );
    }

    #[test]
    fn resolved_heads_clear_unread_after_scoped_mark_seen() {
        let fixture = fixture_at("http://127.0.0.1:9");
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        session
            .client
            .unlock(&fixture.profile, &fixture.header, crate::tests::PHRASE)
            .unwrap();
        let draft = session
            .save_draft(&input("head-unread-draft", "", "", &["+15555550101"], "0"))
            .unwrap();
        let conversation = ConversationId::from_str(&draft.conversation_id).unwrap();
        let captured = session
            .client
            .capture_incoming(IncomingSms {
                conversation_id: Some(conversation),
                sender_address: "+15555550101".into(),
                body: "new incoming message".into(),
                provider_message_id: Some("head-unread-clear".into()),
                imported: false,
            })
            .unwrap();
        assert_eq!(captured.conversation_id, conversation);
        let id = conversation.to_string();
        let wanted = HashSet::from([id.clone()]);
        assert!(
            resolve_heads(&session, &wanted, &photo_cache())
                .unwrap()
                .unwrap()[0]
                .unread
                >= 1
        );
        let message = session.client.messages(conversation).unwrap()[0]
            .payload
            .record
            .message_id
            .to_string();
        session
            .mark_seen_scoped(&[message], Some(conversation))
            .unwrap();
        assert_eq!(
            resolve_heads(&session, &wanted, &photo_cache())
                .unwrap()
                .unwrap()[0]
                .unread,
            0
        );
    }

    #[test]
    fn locally_saved_draft_resolves_without_an_active_sync_epoch() {
        let (_fixture, session, conversation) = locally_saved_draft_without_sync_unlock();
        assert_eq!(session.client.key_status().unwrap().active_epoch, None);

        let head = resolve_one(&session, conversation, &photo_cache()).unwrap();

        assert_eq!(head.id, conversation.to_string());
        assert_eq!(head.name, "+15555550100");
        let restored = resolve_heads(
            &session,
            &HashSet::from([conversation.to_string()]),
            &photo_cache(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, conversation.to_string());
    }

    #[test]
    fn locally_saved_draft_resolves_with_a_locked_active_sync_epoch() {
        let fixture = fixture_at("http://127.0.0.1:9");
        let conversation = {
            let session = open(&fixture, &fixture.binding, &[]);
            session
                .client
                .unlock(&fixture.profile, &fixture.header, crate::tests::PHRASE)
                .unwrap();
            let draft = session
                .save_draft(&input(
                    "draft-new",
                    "",
                    "offline draft",
                    &["+15555550100"],
                    "0",
                ))
                .unwrap();
            let conversation = ConversationId::from_str(&draft.conversation_id).unwrap();
            let avatar = conversation_avatar(
                &conversation.to_string(),
                "+15555550100",
                Some(&["+15555550100".into()]),
                false,
            );
            let automatic_index = palette_index(&avatar.seed);
            let override_index = (automatic_index + 1) % 120;
            session
                .client
                .set_conversation_tone(&avatar.seed, Some(override_index as u8))
                .unwrap();
            conversation
        };
        let session = Arc::new(open(&fixture, &fixture.binding, &[]));
        let keys = session.client.key_status().unwrap();
        assert_eq!(keys.active_epoch, Some(1));
        assert!(!keys.unlocked_epochs.contains(&1));
        session.status.lock().unwrap().vault = Some(crate::session::VaultSummary {
            epoch: 2,
            fingerprint: "newer-sync-epoch".into(),
        });

        let automatic_color = crate::head_palette::head_color(palette_index(
            &conversation_avatar(
                &conversation.to_string(),
                "+15555550100",
                Some(&["+15555550100".into()]),
                false,
            )
            .seed,
        ));
        let head = resolve_one(&session, conversation, &photo_cache()).unwrap();
        assert_eq!(head.id, conversation.to_string());
        assert_eq!(head.color, automatic_color);
        let restored = resolve_heads(
            &session,
            &HashSet::from([conversation.to_string()]),
            &photo_cache(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, conversation.to_string());
    }

    #[test]
    fn locally_saved_draft_remains_available_after_sync_passphrase_mismatch() {
        let (_fixture, session, conversation) = locally_saved_draft_without_sync_unlock();
        session.mismatch.store(true, Ordering::Release);
        assert_eq!(
            resolve_one(&session, conversation, &photo_cache())
                .unwrap()
                .id,
            conversation.to_string()
        );
        let restored = resolve_heads(
            &session,
            &HashSet::from([conversation.to_string()]),
            &photo_cache(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, conversation.to_string());
    }

    #[test]
    fn local_head_resolution_rejects_cancelled_session() {
        let (_fixture, session, conversation) = locally_saved_draft_without_sync_unlock();
        session.cancel.cancel();
        assert!(!session_displayable(&session).unwrap());
        assert_eq!(
            resolve_one(&session, conversation, &photo_cache())
                .err()
                .unwrap()
                .code,
            "session-unavailable"
        );
    }

    #[test]
    fn local_head_resolution_rejects_revoked_session() {
        let (_fixture, session, conversation) = locally_saved_draft_without_sync_unlock();
        session.status.lock().unwrap().revoked = true;
        assert!(!session_displayable(&session).unwrap());
        assert_eq!(
            resolve_one(&session, conversation, &photo_cache())
                .err()
                .unwrap()
                .code,
            "session-unavailable"
        );
    }

    fn visible_runtime_state(
        binding: Binding,
        session: Arc<crate::session::Session>,
        conversation_id: &str,
        generation: u64,
    ) -> RuntimeState {
        let mut state = RuntimeState::new(generation);
        state.binding = Some(binding);
        state.session = Some(session);
        state.recent_entries = Some(vec![(conversation_id.into(), "Ada".into())]);
        state.layout_cache.insert(conversation_id.into(), None);
        state.heads.insert(
            conversation_id.into(),
            ActiveHead {
                position: position(),
                frame: None,
                generation,
                panel: true,
                name: "Ada".into(),
                glyph: HeadGlyph::Person,
                color: [1, 2, 3],
                label: "Ada".into(),
                unread: 1,
                failed_message: None,
            },
        );
        state
    }

    fn persisted_pin_store(
        binding: &Binding,
        conversation_id: &str,
    ) -> (tempfile::TempDir, PinStore) {
        let root = tempfile::tempdir().unwrap();
        let pins = PinStore::new(root.path());
        pins.upsert(
            binding,
            Pin {
                conversation_id: conversation_id.into(),
                acknowledged_failure: None,
                position: position(),
            },
        )
        .unwrap();
        (root, pins)
    }

    fn pin_ids(pins: &PinStore, binding: &Binding) -> Vec<String> {
        pins.pins(binding)
            .unwrap()
            .into_iter()
            .map(|pin| pin.conversation_id)
            .collect()
    }

    fn invalidate_generation(previous: u64) -> u64 {
        let mut fence = GenerationFence::default();
        for _ in 0..previous {
            fence.invalidate();
        }
        let cleared = fence.invalidate();
        assert!(!fence.accepts(previous));
        cleared
    }

    #[test]
    fn initial_revocation_clears_visible_runtime_and_hides_panel_without_unpinning() {
        let (fixture, session, _) = locally_saved_draft_without_sync_unlock();
        let conversation_id = uuid::Uuid::new_v4().to_string();
        let (_root, pins) = persisted_pin_store(&fixture.binding, &conversation_id);
        let original_pins = pin_ids(&pins, &fixture.binding);
        let mut state =
            visible_runtime_state(fixture.binding.clone(), session, &conversation_id, 4);

        let outcome = admitted_refresh_session(
            &fixture.binding,
            Err(BridgeError::new("revoked", "The device was revoked.")),
        );

        assert!(matches!(outcome, RefreshVisibility::Clear));
        let labels = clear_visible_state(&mut state, invalidate_generation(4));
        let mut hidden_panels = Vec::new();
        hide_visible_panels(&labels, |label| hidden_panels.push(label.to_owned()));
        assert_eq!(
            hidden_panels,
            vec![format!("{}{}", tray::COMPOSER_PREFIX, conversation_id)]
        );
        assert_eq!(state.generation, 5);
        assert!(state.heads.is_empty());
        assert!(state.session.is_none());
        assert_eq!(pin_ids(&pins, &fixture.binding), original_pins);
    }

    #[test]
    fn transient_admission_error_preserves_last_good_visible_runtime_and_pins() {
        let (fixture, session, _) = locally_saved_draft_without_sync_unlock();
        let conversation_id = uuid::Uuid::new_v4().to_string();
        let (_root, pins) = persisted_pin_store(&fixture.binding, &conversation_id);
        let original_pins = pin_ids(&pins, &fixture.binding);
        let state = visible_runtime_state(fixture.binding.clone(), session, &conversation_id, 4);

        let outcome = admitted_refresh_session(
            &fixture.binding,
            Err(BridgeError::new(
                "network",
                "The server is temporarily unavailable.",
            )),
        );

        assert!(matches!(
            outcome,
            RefreshVisibility::Preserve {
                clear_recents: false
            }
        ));
        assert!(state.heads.contains_key(&conversation_id));
        assert!(state.heads[&conversation_id].panel);
        assert_eq!(state.generation, 4);
        assert_eq!(pin_ids(&pins, &fixture.binding), original_pins);
    }

    #[test]
    fn revocation_during_resolution_clears_visible_runtime_and_hides_panel_without_unpinning() {
        let (fixture, session, _) = locally_saved_draft_without_sync_unlock();
        let conversation_id = uuid::Uuid::new_v4().to_string();
        let (_root, pins) = persisted_pin_store(&fixture.binding, &conversation_id);
        let original_pins = pin_ids(&pins, &fixture.binding);
        let mut state = visible_runtime_state(
            fixture.binding.clone(),
            session.clone(),
            &conversation_id,
            4,
        );
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let resolution_entered = entered.clone();
        let resolution_release = release.clone();
        let revoked_session = session.clone();
        let outcome = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                tokio::join!(
                    resolved_refresh_visibility(
                        &session,
                        async move {
                            resolution_entered.notify_one();
                            resolution_release.notified().await;
                            Ok::<_, BridgeError>(Some(()))
                        },
                        async { RefreshRevalidation::Stale },
                    ),
                    async move {
                        entered.notified().await;
                        revoked_session.status.lock().unwrap().revoked = true;
                        release.notify_one();
                    },
                )
                .0
            });

        assert!(matches!(outcome, RefreshVisibility::Clear));
        let labels = clear_visible_state(&mut state, invalidate_generation(4));
        let mut hidden_panels = Vec::new();
        hide_visible_panels(&labels, |label| hidden_panels.push(label.to_owned()));
        assert_eq!(
            hidden_panels,
            vec![format!("{}{}", tray::COMPOSER_PREFIX, conversation_id)]
        );
        assert_eq!(state.generation, 5);
        assert!(state.heads.is_empty());
        assert_eq!(pin_ids(&pins, &fixture.binding), original_pins);
    }

    #[test]
    fn definitive_revocation_clears_runtime_heads_without_touching_persisted_state() {
        let (_fixture, session, _) = locally_saved_draft_without_sync_unlock();
        let mut state = RuntimeState::new(4);
        state.session = Some(session);
        state.recent_entries = Some(vec![("conversation".into(), "Ada".into())]);
        state.layout_cache.insert("conversation".into(), None);
        state.heads.insert(
            "conversation".into(),
            ActiveHead {
                position: position(),
                frame: None,
                generation: 4,
                panel: true,
                name: "Ada".into(),
                glyph: HeadGlyph::Person,
                color: [1, 2, 3],
                label: "Ada".into(),
                unread: 1,
                failed_message: None,
            },
        );

        let labels = clear_visible_state(&mut state, 5);

        assert_eq!(
            labels,
            vec![format!("{}conversation", tray::COMPOSER_PREFIX)]
        );
        assert_eq!(state.generation, 5);
        assert!(state.heads.is_empty());
        assert!(state.session.is_none());
        assert!(state.recent_entries.is_none());
        assert!(state.layout_cache.is_empty());
    }

    #[test]
    fn local_head_resolution_rejects_unknown_conversation() {
        let (_fixture, session, _) = locally_saved_draft_without_sync_unlock();
        assert_eq!(
            resolve_one(&session, ConversationId::new(), &photo_cache())
                .err()
                .unwrap()
                .code,
            "not-found"
        );
    }

    fn position() -> HeadPosition {
        HeadPosition {
            monitor: None,
            x: 10.0,
            y: 20.0,
        }
    }

    #[test]
    fn same_binding_adoption_preserves_panel_and_authoritative_frame() {
        let frame = HeadFrame {
            x: 1.0,
            y: 2.0,
            width: 56.0,
            height: 56.0,
            work_x: 0.0,
            work_y: 0.0,
            work_width: 1000.0,
            work_height: 800.0,
            scale_factor: 2.0,
        };
        let existing = ActiveHead {
            position: position(),
            frame: Some(frame.clone()),
            generation: 4,
            panel: true,
            name: "Old".into(),
            glyph: HeadGlyph::Person,
            color: [1, 2, 3],
            label: "Old".into(),
            unread: 1,
            failed_message: None,
        };
        let adopted = adopt_active_head(
            Some(&existing),
            position(),
            4,
            &HeadData {
                id: "conversation".into(),
                name: "New".into(),
                glyph: HeadGlyph::Initials("N".into()),
                color: [4, 5, 6],
                label: "New".into(),
                unread: 2,
                failed_message: None,
            },
        );
        assert!(adopted.panel);
        assert_eq!(adopted.frame.unwrap().x, frame.x);
        assert_eq!(adopted.name, "New");
    }

    fn typing_at(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    fn test_head(generation: u64, unread: u64) -> ActiveHead {
        ActiveHead {
            position: position(),
            frame: None,
            generation,
            panel: false,
            name: "Ada".into(),
            glyph: HeadGlyph::Person,
            color: [1, 2, 3],
            label: "Ada".into(),
            unread,
            failed_message: None,
        }
    }

    #[test]
    fn typing_hide_delay_honors_minimum_visible_and_linger() {
        let base = Instant::now();
        // A 100 ms blip is held to the 600 ms minimum, then nothing more.
        assert_eq!(
            typing_hide_delay(base, typing_at(base, 100), typing_at(base, 100)),
            Duration::from_millis(500)
        );
        // A long burst only lingers 300 ms after the stop.
        assert_eq!(
            typing_hide_delay(base, typing_at(base, 5_000), typing_at(base, 5_000)),
            Duration::from_millis(300)
        );
        // Once both windows have passed the delay is zero.
        assert_eq!(
            typing_hide_delay(base, typing_at(base, 100), typing_at(base, 900)),
            Duration::ZERO
        );
    }

    #[test]
    fn typing_show_stop_expire_removes_entry() {
        let base = Instant::now();
        let mut map = TypingDisplays::default();
        assert_eq!(
            map.apply_raw("conversation", true, base),
            TypingTransition::Shown
        );
        let TypingTransition::HideAfter { delay, epoch } =
            map.apply_raw("conversation", false, typing_at(base, 2_000))
        else {
            panic!("raw false after a shown pill must schedule a hide");
        };
        assert_eq!(delay, TYPING_LINGER);
        assert!(!map.expire("conversation", epoch, typing_at(base, 2_299)));
        assert!(map.expire("conversation", epoch, typing_at(base, 2_300)));
        assert!(map.entries.is_empty());
    }

    #[test]
    fn stop_delays_for_a_blip_and_for_a_stop_at_the_minimum_visible_time() {
        let base = Instant::now();
        let mut map = TypingDisplays::default();
        map.apply_raw("blip", true, base);
        assert!(matches!(
            map.apply_raw("blip", false, typing_at(base, 100)),
            TypingTransition::HideAfter { delay, .. } if delay == Duration::from_millis(500)
        ));
        map.apply_raw("exact", true, base);
        assert!(matches!(
            map.apply_raw("exact", false, typing_at(base, 600)),
            TypingTransition::HideAfter { delay, .. } if delay == Duration::from_millis(300)
        ));
    }

    #[test]
    fn typing_resume_during_linger_cancels_the_pending_hide() {
        let base = Instant::now();
        let mut map = TypingDisplays::default();
        map.apply_raw("conversation", true, base);
        let TypingTransition::HideAfter { epoch: stale, .. } =
            map.apply_raw("conversation", false, typing_at(base, 2_000))
        else {
            panic!("expected a hide");
        };
        assert_eq!(
            map.apply_raw("conversation", true, typing_at(base, 2_100)),
            TypingTransition::Unchanged
        );
        // The old timer fires after the original linger but is a no-op: raw is true again.
        assert!(!map.expire("conversation", stale, typing_at(base, 2_400)));
        assert!(map.displayed("conversation"));
        // A later stop schedules a fresh hide with a new epoch that does expire.
        let TypingTransition::HideAfter { epoch, .. } =
            map.apply_raw("conversation", false, typing_at(base, 3_000))
        else {
            panic!("expected a second hide");
        };
        assert_ne!(epoch, stale);
        assert!(!map.expire("conversation", stale, typing_at(base, 3_300)));
        assert!(map.expire("conversation", epoch, typing_at(base, 3_300)));
    }

    #[test]
    fn stale_epoch_never_matches_a_newer_entry() {
        let base = Instant::now();
        let mut map = TypingDisplays::default();
        map.apply_raw("conversation", true, base);
        let TypingTransition::HideAfter { epoch: first, .. } =
            map.apply_raw("conversation", false, typing_at(base, 1_000))
        else {
            panic!("expected a hide");
        };
        // The entry goes away through an unread change, then a new one starts and stops.
        assert!(map.end_linger_on_unread_change("conversation", 1, 2));
        map.apply_raw("conversation", true, typing_at(base, 5_000));
        let TypingTransition::HideAfter { epoch: second, .. } =
            map.apply_raw("conversation", false, typing_at(base, 6_000))
        else {
            panic!("expected a hide");
        };
        assert_ne!(first, second);
        // Even once the newer entry is due, the first timer cannot expire it.
        assert!(!map.expire("conversation", first, typing_at(base, 9_000)));
        assert!(map.displayed("conversation"));
        assert!(map.expire("conversation", second, typing_at(base, 9_000)));
    }

    #[test]
    fn expire_is_a_no_op_while_raw_is_true_or_epoch_is_wrong() {
        let base = Instant::now();
        let mut map = TypingDisplays::default();
        map.apply_raw("conversation", true, base);
        assert!(!map.expire("conversation", 0, typing_at(base, 10_000)));
        let TypingTransition::HideAfter { epoch, .. } =
            map.apply_raw("conversation", false, typing_at(base, 10_000))
        else {
            panic!("expected a hide");
        };
        assert!(!map.expire("conversation", epoch + 1, typing_at(base, 20_000)));
        assert!(map.displayed("conversation"));
        assert!(!map.expire("missing", epoch, typing_at(base, 20_000)));
    }

    #[test]
    fn entry_without_head_shows_in_the_spec_a_later_refresh_or_popout_builds() {
        let base = Instant::now();
        let mut typing = TypingDisplays::default();
        assert_eq!(
            typing.apply_raw("conversation", true, base),
            TypingTransition::Shown
        );
        // refresh and popout both build their spec through head_spec, from the head they insert.
        let head = test_head(4, 0);
        assert!(head_spec(&typing, &SendFailures::default(), "conversation", &head).typing);
        assert!(!head_spec(&typing, &SendFailures::default(), "other", &head).typing);
    }

    #[test]
    fn unread_change_during_linger_ends_it_and_the_spec_carries_the_new_count() {
        let base = Instant::now();
        let mut map = TypingDisplays::default();
        map.apply_raw("conversation", true, base);
        // Raw true: an unread change keeps the pill.
        assert!(!map.end_linger_on_unread_change("conversation", 1, 2));
        assert!(map.displayed("conversation"));
        map.apply_raw("conversation", false, typing_at(base, 1_000));
        // Same unread during the linger keeps it until the timer runs out.
        assert!(!map.end_linger_on_unread_change("conversation", 2, 2));
        assert!(map.displayed("conversation"));
        // A changed unread during the linger ends it at once, and the spec for that update
        // carries typing = false with the new count.
        assert!(map.end_linger_on_unread_change("conversation", 2, 3));
        assert!(map.entries.is_empty());
        let updated = test_head(4, 3);
        let spec = head_spec(&map, &SendFailures::default(), "conversation", &updated);
        assert!(!spec.typing);
        assert_eq!(spec.unread, 3);
        assert!(!map.end_linger_on_unread_change("conversation", 3, 4));
    }

    #[test]
    fn locked_app_hides_typing_and_nothing_reappears_after_unlock() {
        let base = Instant::now();
        let mut map = TypingDisplays::default();
        map.apply_raw("conversation", true, base);
        // Locking removes the shown pill: the caller must push a native clear for it.
        assert_eq!(
            map.apply_report(true, "conversation", true, typing_at(base, 100)),
            TypingTransition::Hidden
        );
        assert!(map.entries.is_empty());
        // Typing reported while locked creates nothing, and a second locked report is quiet.
        assert_eq!(
            map.apply_report(true, "other", true, typing_at(base, 200)),
            TypingTransition::Unchanged
        );
        assert!(!map.displayed("other"));
        // After unlock a stale stop does not show anything, and only fresh raw true does.
        assert_eq!(
            map.apply_report(false, "conversation", false, typing_at(base, 300)),
            TypingTransition::Unchanged
        );
        assert!(map.entries.is_empty());
        assert_eq!(
            map.apply_report(false, "conversation", true, typing_at(base, 400)),
            TypingTransition::Shown
        );
    }

    #[test]
    fn session_replacement_drops_pending_typing_and_returns_the_pills_to_clear() {
        let base = Instant::now();
        let mut map = TypingDisplays::default();
        map.apply_raw("linger", true, base);
        let TypingTransition::HideAfter { epoch, .. } =
            map.apply_raw("linger", false, typing_at(base, 1_000))
        else {
            panic!("expected a hide");
        };
        map.apply_raw("live", true, base);
        // A session change with the same binding keeps the entries (the same-session case).
        assert!(map.reset_for_replacement(false).is_empty());
        assert!(map.displayed("linger") && map.displayed("live"));
        // A replaced session drops them and reports the conversations whose pill was showing.
        let mut cleared = map.reset_for_replacement(true);
        cleared.sort();
        assert_eq!(cleared, vec!["linger".to_owned(), "live".to_owned()]);
        assert!(map.entries.is_empty());
        // The old session's linger timer is now a no-op, and the new session starts fresh.
        assert!(!map.expire("linger", epoch, typing_at(base, 2_000)));
        assert_eq!(
            map.apply_raw("linger", false, typing_at(base, 2_000)),
            TypingTransition::Unchanged
        );
        assert_eq!(
            map.apply_raw("live", true, typing_at(base, 2_000)),
            TypingTransition::Shown
        );
    }

    #[test]
    fn head_removed_during_linger_expires_normally_without_a_head() {
        let (fixture, session, _) = locally_saved_draft_without_sync_unlock();
        let mut state = RuntimeState::new(4);
        state.binding = Some(fixture.binding);
        state.session = Some(session);
        state.heads.insert("conversation".into(), test_head(4, 0));
        let base = Instant::now();
        state.typing.apply_raw("conversation", true, base);
        let TypingTransition::HideAfter { delay, epoch } =
            state
                .typing
                .apply_raw("conversation", false, typing_at(base, 1_000))
        else {
            panic!("expected a hide");
        };
        assert_eq!(delay, TYPING_LINGER);
        state.heads.remove("conversation");
        // The timer still expires the entry on schedule; with no head the caller pushes nothing.
        assert!(state
            .typing
            .expire("conversation", epoch, typing_at(base, 1_300)));
        assert!(!state.typing.displayed("conversation"));
        assert!(!state.heads.contains_key("conversation"));
    }

    #[test]
    fn clear_visible_state_empties_typing_entries() {
        let (fixture, session, _) = locally_saved_draft_without_sync_unlock();
        let mut state = visible_runtime_state(fixture.binding, session, "conversation", 4);
        state.typing.apply_raw("conversation", true, Instant::now());
        state.typing.apply_raw("other", true, Instant::now());
        clear_visible_state(&mut state, 5);
        assert!(state.heads.is_empty());
        assert!(state.typing.entries.is_empty());
    }

    #[test]
    fn failed_collapse_keeps_panel_but_failed_expansion_clears_it() {
        let error = || BridgeError::new("window", "failed");
        assert!(!expansion_failed(&Ok(Some(ActivationUi::Collapse(Err(
            error()
        ))))));
        assert!(expansion_failed(&Ok(Some(ActivationUi::Expand(Err(
            error()
        ))))));
    }

    #[test]
    fn pin_limit_counts_both_persisted_and_active_heads() {
        assert!(pin_limit_reached(MAX_HEADS, 1, false));
        assert!(pin_limit_reached(1, MAX_HEADS, false));
        assert!(!pin_limit_reached(MAX_HEADS, MAX_HEADS, true));
    }

    #[test]
    fn restored_layout_cache_rejects_stale_generations_bindings_and_writes() {
        let root = tempfile::tempdir().unwrap();
        let binding = Binding {
            origin: "https://example.test".into(),
            vault_id: "vault".into(),
            device_id: "device".into(),
        };
        let id = uuid::Uuid::new_v4().to_string();
        let store = HeadLayoutStore::new(root.path());
        let saved = store
            .save_panel(
                &binding,
                &id,
                position(),
                PanelLayout {
                    monitor: None,
                    width: 620.0,
                    height: 500.0,
                    offset_x: -20.0,
                    offset_y: 64.0,
                },
            )
            .unwrap();
        drop(store);
        let restored = HeadLayoutStore::new(root.path())
            .layout(&binding, &id)
            .unwrap();
        let mut state = RuntimeState::new(1);
        state.binding = Some(binding.clone());
        cache_layout(&mut state, &binding, 1, &id, restored);
        let mut old = saved;
        old.updated -= 1;
        old.panel.as_mut().unwrap().width = 340.0;
        cache_layout(&mut state, &binding, 1, &id, Some(old.clone()));
        cache_layout(&mut state, &binding, 1, &id, None);
        assert_eq!(
            state.layout_cache[&id]
                .as_ref()
                .unwrap()
                .panel
                .as_ref()
                .unwrap()
                .width,
            620.0
        );
        state.layout_cache.clear();
        state.generation = 2;
        cache_layout(&mut state, &binding, 1, &id, Some(old.clone()));
        assert!(state.layout_cache.is_empty());
        let other = Binding {
            device_id: "other".into(),
            ..binding
        };
        cache_layout(&mut state, &other, 2, &id, Some(old));
        assert!(state.layout_cache.is_empty());
    }
}
