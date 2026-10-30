//! Durable normal-window placement, independent of vault and renderer state.
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

#[cfg(not(target_os = "windows"))]
use tauri::LogicalPosition;
#[cfg(target_os = "windows")]
use tauri::PhysicalPosition;
use tauri::{LogicalSize, Manager, WebviewWindow, WindowEvent};

use crate::window_geometry::{Monitor, Placement, Rect, Store};
#[path = "window_placement/policy.rs"]
mod policy;
use policy::{Action, Insets, Recovery, SaveQueue, Target};

const SETTLE_DELAY: Duration = Duration::from_millis(250);
const TOPOLOGY_POLL: Duration = Duration::from_secs(5);

struct Layout {
    monitors: Vec<Monitor>,
    fingerprint: Vec<Monitor>,
}

struct Runtime {
    store: Store,
    saves: Mutex<SaveQueue>,
    recovery: Mutex<Recovery>,
    writes: Mutex<()>,
    write_generation: AtomicU64,
    last_normal: Mutex<Option<Placement>>,
    initial_saved: Mutex<Option<Placement>>,
    minimum: (f64, f64),
    applied_minimum: Mutex<Option<(f64, f64)>>,
    warned_save: AtomicBool,
    warned_apply: AtomicBool,
}

impl Runtime {
    fn new(root: &Path, minimum: (f64, f64)) -> Self {
        Self {
            store: Store::new(root),
            saves: Mutex::new(SaveQueue::default()),
            recovery: Mutex::new(Recovery::default()),
            writes: Mutex::new(()),
            write_generation: AtomicU64::new(0),
            last_normal: Mutex::new(None),
            initial_saved: Mutex::new(None),
            minimum,
            applied_minimum: Mutex::new(None),
            warned_save: AtomicBool::new(false),
            warned_apply: AtomicBool::new(false),
        }
    }

    fn save(&self, placement: Placement, generation: u64) {
        let Ok(_write) = self.writes.lock() else {
            return;
        };
        if self.write_generation.load(Ordering::Acquire) == generation
            && self.store.save(placement).is_err()
            && !self.warned_save.swap(true, Ordering::AcqRel)
        {
            eprintln!("peppy: could not save main-window placement");
        }
    }

    fn remember(&self, placement: Placement) {
        if let Ok(mut latest) = self.last_normal.lock() {
            *latest = Some(placement);
        }
    }

    fn set_minimum(&self, window: &WebviewWindow, minimum: (f64, f64)) -> bool {
        if self
            .applied_minimum
            .lock()
            .ok()
            .is_some_and(|last| *last == Some(minimum))
        {
            return true;
        }
        if window
            .set_min_size(Some(LogicalSize::new(minimum.0, minimum.1)))
            .is_err()
        {
            return false;
        }
        if let Ok(mut applied) = self.applied_minimum.lock() {
            *applied = Some(minimum);
        }
        true
    }

    fn apply(&self, window: &WebviewWindow, target: Target) {
        let applied = (|| {
            if !normal(window) {
                return None;
            }
            position(window, target.placement).ok()?;
            // On worker passes these getters run after the queued move/DPI change.
            let insets = frame_insets(window)?;
            let size = policy::sizing(target, self.minimum, insets);
            if !self.set_minimum(window, size.minimum) {
                return None;
            }
            // Logical size preserves the target's intent even if a DPI change is queued.
            window
                .set_size(LogicalSize::new(size.content.0, size.content.1))
                .ok()?;
            // AppKit keeps the bottom edge on resize. Re-anchor the queued frame's top.
            position(window, target.placement).ok()
        })();
        if applied.is_none() {
            self.warn_apply();
        }
    }

    fn warn_apply(&self) {
        if !self.warned_apply.swap(true, Ordering::AcqRel) {
            eprintln!("peppy: the window manager could not apply main-window placement");
        }
    }

    fn begin(&self, window: &WebviewWindow, target: Target) {
        let start = self
            .recovery
            .lock()
            .ok()
            .is_some_and(|mut recovery| recovery.begin(target));
        self.apply(window, target);
        if start {
            spawn_recovery(window.app_handle());
        }
    }

    fn recover(&self, window: &WebviewWindow) {
        if !position_supported() || !normal(window) {
            return;
        }
        // Startup/show must not replace a queued restore with the old default frame.
        if self
            .recovery
            .lock()
            .ok()
            .and_then(|recovery| recovery.target())
            .is_some()
        {
            return;
        }
        let Some(layout) = layout(window) else { return };
        let Some(actual) = capture(window, false) else {
            return;
        };
        let Some(insets) = frame_insets(window) else {
            return;
        };
        let saved = self.initial_saved.lock().ok().and_then(|saved| *saved);
        let Some(target) = policy::plan(
            saved.unwrap_or(actual),
            &layout.monitors,
            self.minimum,
            insets,
        ) else {
            return;
        };
        if let Ok(mut saved) = self.initial_saved.lock() {
            *saved = None;
        }
        if target.placement != actual {
            self.begin(window, target);
        } else {
            let size = policy::sizing(target, self.minimum, insets);
            if !self.set_minimum(window, size.minimum) {
                self.warn_apply();
            }
        }
    }

    fn refresh_minimum(&self, window: &WebviewWindow, current: Placement) {
        let (Some(layout), Some(insets)) = (layout(window), frame_insets(window)) else {
            return;
        };
        let Some(target) = policy::plan(current, &layout.monitors, self.minimum, insets) else {
            return;
        };
        let size = policy::sizing(target, self.minimum, insets);
        // Restoring a larger minimum may grow the frame; fit that growth as well.
        let needs_growth = policy::minimum_growth(current, target, self.minimum, insets);
        if needs_growth {
            self.begin(window, target);
        } else if !self.set_minimum(window, size.minimum) {
            self.warn_apply();
        }
    }
}

fn normal(window: &WebviewWindow) -> bool {
    !window.is_minimized().unwrap_or(true)
        && !window.is_maximized().unwrap_or(true)
        && !window.is_fullscreen().unwrap_or(true)
}

fn position(window: &WebviewWindow, placement: Placement) -> tauri::Result<()> {
    // AppKit and GTK expose logical desktop coordinates scaled per window/monitor.
    #[cfg(not(target_os = "windows"))]
    {
        window.set_position(LogicalPosition::new(
            f64::from(placement.bounds.x),
            f64::from(placement.bounds.y),
        ))
    }
    #[cfg(target_os = "windows")]
    {
        window.set_position(PhysicalPosition::new(
            placement.bounds.x,
            placement.bounds.y,
        ))
    }
}

pub(crate) fn media_viewer_placement(
    parent: Rect,
    work_area: Rect,
    scale: f64,
    cascade: i32,
) -> Option<Placement> {
    policy::media_viewer_placement(
        parent,
        work_area,
        scale,
        cfg!(not(target_os = "windows")),
        cascade,
    )
}

fn capture(window: &WebviewWindow, visible_only: bool) -> Option<Placement> {
    if !position_supported() || !normal(window) || (visible_only && !window.is_visible().ok()?) {
        return None;
    }
    let position = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    let scale = window.scale_factor().ok()?;
    if !scale.is_finite() || !(0.1..=16.0).contains(&scale) || size.width == 0 || size.height == 0 {
        return None;
    }
    // Recheck after getters: an intervening minimize must not save sentinel coordinates.
    if !normal(window) || (visible_only && !window.is_visible().ok()?) {
        return None;
    }
    Some(policy::window_bounds(
        Rect {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
        },
        scale,
        cfg!(not(target_os = "windows")),
    ))
}

fn frame_insets(window: &WebviewWindow) -> Option<Insets> {
    let outer = window.outer_size().ok()?;
    let inner = window.inner_size().ok()?;
    let scale = window.scale_factor().ok()?;
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    #[cfg(target_os = "macos")]
    let height = (i64::from(window.inner_position().ok()?.y)
        - i64::from(window.outer_position().ok()?.y))
    .max(0) as f64;
    #[cfg(not(target_os = "macos"))]
    let height = f64::from(outer.height.saturating_sub(inner.height));
    Some(Insets {
        width: f64::from(outer.width.saturating_sub(inner.width)) / scale,
        height: height / scale,
    })
}

fn layout(window: &WebviewWindow) -> Option<Layout> {
    let mut monitors = Vec::new();
    let mut fingerprint = Vec::new();
    for monitor in window.available_monitors().ok()? {
        let work = monitor.work_area();
        let area = Rect {
            x: work.position.x,
            y: work.position.y,
            width: work.size.width,
            height: work.size.height,
        };
        if let Some(normalized) = policy::monitor_bounds(
            area,
            monitor.scale_factor(),
            cfg!(not(target_os = "windows")),
        ) {
            monitors.push(normalized);
            // Retain native scale on logical desktops, where policy scales are one.
            fingerprint.push(Monitor {
                work_area: area,
                scale_factor: monitor.scale_factor(),
            });
        }
    }
    if monitors.is_empty() {
        return None;
    }
    fingerprint.sort_by(|left, right| {
        let a = left.work_area;
        let b = right.work_area;
        (a.x, a.y, a.width, a.height)
            .cmp(&(b.x, b.y, b.width, b.height))
            .then_with(|| left.scale_factor.total_cmp(&right.scale_factor))
    });
    Some(Layout {
        monitors,
        fingerprint,
    })
}

fn position_supported() -> bool {
    #[cfg(target_os = "linux")]
    {
        policy::position_supported(
            std::env::var("GDK_BACKEND").ok().as_deref(),
            std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
            std::env::var_os("WAYLAND_DISPLAY").is_some(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

pub(crate) fn install(app: &tauri::AppHandle, root: &Path) {
    let minimum = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == crate::tray::MAIN)
        .map(|window| {
            (
                window.min_width.unwrap_or(0.0),
                window.min_height.unwrap_or(0.0),
            )
        })
        .unwrap_or((0.0, 0.0));
    app.manage(Runtime::new(root, minimum));
    if !position_supported() {
        return;
    }
    if let Some(window) = app.get_webview_window(crate::tray::MAIN) {
        let layout = layout(&window);
        let is_normal = normal(&window);
        if let Ok(mut recovery) = app.state::<Runtime>().recovery.lock() {
            recovery.prime(
                layout.as_ref().map(|layout| layout.fingerprint.as_slice()),
                is_normal,
            );
        }
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(TOPOLOGY_POLL).await;
            let Some(window) = app.get_webview_window(crate::tray::MAIN) else {
                return;
            };
            let Some(runtime) = app.try_state::<Runtime>() else {
                return;
            };
            let layout = layout(&window);
            let is_normal = normal(&window);
            let start = runtime.recovery.lock().ok().is_some_and(|mut recovery| {
                recovery.observe_environment(
                    layout.as_ref().map(|layout| layout.fingerprint.as_slice()),
                    is_normal,
                )
            });
            if start {
                spawn_recovery(&app);
            }
        }
    });
}

pub(crate) fn restore_initial(window: &WebviewWindow) {
    let Some(runtime) = window.app_handle().try_state::<Runtime>() else {
        return;
    };
    if !position_supported() {
        return;
    }
    if let Ok(Some(saved)) = runtime.store.load() {
        if let Ok(mut initial) = runtime.initial_saved.lock() {
            *initial = Some(saved);
        }
    }
    runtime.recover(window);
}

pub(crate) fn recover_before_show(app: &tauri::AppHandle) {
    if let (Some(runtime), Some(window)) = (
        app.try_state::<Runtime>(),
        app.get_webview_window(crate::tray::MAIN),
    ) {
        runtime.recover(&window);
    }
}

pub(crate) fn flush(app: &tauri::AppHandle) {
    if !position_supported() {
        return;
    }
    let Some(runtime) = app.try_state::<Runtime>() else {
        return;
    };
    // Cancel under the same queue lock used by the single-flight worker.
    let generation = {
        let Ok(mut saves) = runtime.saves.lock() else {
            return;
        };
        saves.cancel();
        runtime
            .write_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    };
    let pending = runtime
        .recovery
        .lock()
        .ok()
        .and_then(|recovery| recovery.target())
        .map(|target| target.placement);
    let initial = runtime.initial_saved.lock().ok().and_then(|saved| *saved);
    let current = app
        .get_webview_window(crate::tray::MAIN)
        .and_then(|window| capture(&window, true));
    let cached = runtime.last_normal.lock().ok().and_then(|saved| *saved);
    if let Some(placement) = policy::preferred_save(initial, pending, current, cached) {
        runtime.remember(placement);
        runtime.save(placement, generation);
    }
}

pub(crate) fn window_event(window: &tauri::Window, event: &WindowEvent) {
    if window.label() != crate::tray::MAIN
        || !position_supported()
        || !matches!(
            event,
            WindowEvent::Moved(_)
                | WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
        )
    {
        return;
    }
    let Some(runtime) = window.app_handle().try_state::<Runtime>() else {
        return;
    };
    let Some(webview) = window.app_handle().get_webview_window(crate::tray::MAIN) else {
        return;
    };
    let is_normal = normal(&webview);
    let (start, applying) = {
        let Ok(mut recovery) = runtime.recovery.lock() else {
            return;
        };
        let start = recovery.observe_environment(None, is_normal);
        (start, recovery.target().is_some())
    };
    if start {
        spawn_recovery(window.app_handle());
    }
    if !is_normal || applying {
        return;
    }
    if matches!(event, WindowEvent::ScaleFactorChanged { .. }) {
        // Crossing an existing DPI boundary is a drag, not a display-layout change.
        let layout = layout(&webview);
        let changed = runtime.recovery.lock().ok().is_some_and(|mut recovery| {
            recovery.observe_environment(
                layout.as_ref().map(|layout| layout.fingerprint.as_slice()),
                is_normal,
            )
        });
        if changed {
            spawn_recovery(window.app_handle());
        }
    }
    if let Some(placement) = capture(&webview, true) {
        runtime.remember(placement);
        let start = runtime.saves.lock().ok().is_some_and(|mut saves| {
            saves.request(
                Instant::now(),
                runtime.write_generation.load(Ordering::Acquire),
            )
        });
        if start {
            spawn_save(window.app_handle());
        }
    }
}

fn spawn_recovery(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            // Native moves/resizes and DPI changes are queued on several backends.
            tokio::time::sleep(SETTLE_DELAY).await;
            let (Some(runtime), Some(window)) = (
                app.try_state::<Runtime>(),
                app.get_webview_window(crate::tray::MAIN),
            ) else {
                return;
            };
            let is_normal = normal(&window);
            let actual = capture(&window, false);
            let action = runtime
                .recovery
                .lock()
                .map(|mut recovery| recovery.next(actual, is_normal))
                .unwrap_or(Action::Idle);
            match action {
                Action::Apply(target) => runtime.apply(&window, target),
                Action::Check => runtime.recover(&window),
                Action::Exhausted => runtime.warn_apply(),
                Action::Idle => return,
            }
        }
    });
}

fn spawn_save(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let Some(runtime) = app.try_state::<Runtime>() else {
                return;
            };
            // The check and relinquishing worker ownership are atomic with new requests.
            let deadline = runtime
                .saves
                .lock()
                .ok()
                .and_then(|mut saves| saves.deadline_or_finish());
            let Some(deadline) = deadline else { return };
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            let generation = runtime
                .saves
                .lock()
                .ok()
                .and_then(|mut saves| saves.take_due(Instant::now()));
            let Some(generation) = generation else {
                continue;
            };
            if runtime
                .recovery
                .lock()
                .ok()
                .and_then(|recovery| recovery.target())
                .is_some()
            {
                continue;
            }
            if let Some(window) = app.get_webview_window(crate::tray::MAIN) {
                if let Some(placement) = capture(&window, true) {
                    runtime.save(placement, generation);
                    runtime.refresh_minimum(&window, placement);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_debounced_writer_cannot_replace_final_flush_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = Runtime::new(dir.path(), (760.0, 560.0));
        let old = Placement {
            bounds: Rect {
                x: 100,
                y: 100,
                width: 900,
                height: 650,
            },
            scale_factor: 1.0,
        };
        let latest = Placement {
            bounds: Rect {
                x: 300,
                y: 200,
                ..old.bounds
            },
            ..old
        };
        let delayed_generation = runtime.write_generation.load(Ordering::Acquire);
        runtime.write_generation.fetch_add(1, Ordering::AcqRel);
        runtime.save(latest, runtime.write_generation.load(Ordering::Acquire));
        runtime.save(old, delayed_generation);
        assert_eq!(runtime.store.load().unwrap(), Some(latest));
    }
}
