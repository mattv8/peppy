//! Native rounded window chrome for the main and standalone composer windows.
use std::cell::RefCell;
use tauri::{Manager, Webview, WebviewWindow};

thread_local! {
    static MATERIALS: RefCell<crate::material_state::Materials> = RefCell::default();
}

/// Called on the event-loop thread when a window is destroyed.
pub fn forget_material(label: &str) {
    MATERIALS.with(|materials| materials.borrow_mut().forget(label));
}

pub fn show_when_ready(window: &WebviewWindow) -> tauri::Result<()> {
    let target = window.clone();
    window.run_on_main_thread(move || {
        let window = target.as_ref().window();
        let mut operations = TauriShowOperations { window: &window };
        let ready = MATERIALS.with(|state| show_requested(&mut state.borrow_mut(), target.label()));
        if ready {
            show_window(&mut operations);
        }
    })
}

pub fn page_loaded(webview: &Webview) {
    let target = webview.clone();
    let _ = webview.run_on_main_thread(move || {
        let window = target.window();
        let mut operations = TauriShowOperations { window: &window };
        let ready = MATERIALS.with(|state| show_loaded(&mut state.borrow_mut(), target.label()));
        if ready {
            show_window(&mut operations);
        }
    });
}

/// Restores a window after a failed lifecycle action without waiting for another page load.
pub(crate) fn recover_window(window: &WebviewWindow) -> tauri::Result<()> {
    let target = window.clone();
    window.run_on_main_thread(move || {
        let window = target.as_ref().window();
        let mut operations = TauriShowOperations { window: &window };
        restore_window(&mut operations);
    })
}

trait ShowOperations {
    fn prepare(&mut self) -> Result<(), &'static str>;
    fn report_preparation_failure(&mut self, error: &'static str);
    fn show(&mut self);
    fn unminimize(&mut self);
    fn focus(&mut self);
}

struct TauriShowOperations<'a> {
    window: &'a tauri::Window,
}

impl ShowOperations for TauriShowOperations<'_> {
    fn prepare(&mut self) -> Result<(), &'static str> {
        prepare_composer_chrome(self.window)
    }

    fn report_preparation_failure(&mut self, error: &'static str) {
        const MESSAGE: &str = "Peppy could not prepare this conversation window. Reopen it from the main conversation or floating bubble.";
        if !crate::dialogs::inform(
            self.window.app_handle(),
            "Could not open conversation",
            MESSAGE,
        ) {
            eprintln!("peppy: could not show composer chrome error dialog: {error}");
        }
    }

    fn show(&mut self) {
        let _ = self.window.show();
    }

    fn unminimize(&mut self) {
        let _ = self.window.unminimize();
    }

    fn focus(&mut self) {
        let _ = self.window.set_focus();
    }
}

fn show_requested(materials: &mut crate::material_state::Materials, label: &str) -> bool {
    materials.request_show(label)
}

fn show_loaded(materials: &mut crate::material_state::Materials, label: &str) -> bool {
    materials.page_loaded(label)
}

fn show_window(operations: &mut impl ShowOperations) {
    if let Err(error) = operations.prepare() {
        operations.report_preparation_failure(error);
        return;
    }
    operations.show();
    operations.unminimize();
    operations.focus();
}

fn restore_window(operations: &mut impl ShowOperations) {
    if let Err(error) = operations.prepare() {
        operations.report_preparation_failure(error);
        return;
    }
    operations.show();
    operations.focus();
}

#[cfg(any(target_os = "macos", test))]
fn is_composer_window(label: &str) -> bool {
    crate::tray::composer_conversation(label).is_some()
}

#[cfg(target_os = "macos")]
#[allow(deprecated, unexpected_cfgs)]
fn prepare_composer_chrome(window: &tauri::Window) -> Result<(), &'static str> {
    use cocoa::{
        appkit::{NSWindow, NSWindowButton},
        base::{id, nil, YES},
    };
    use objc::{msg_send, sel, sel_impl};

    if !is_composer_window(window.label()) {
        return Ok(());
    }
    let native = window
        .ns_window()
        .map_err(|_| "The native conversation window is unavailable.")?;
    if native.is_null() {
        return Err("The native conversation window is unavailable.");
    }
    unsafe {
        let native: id = native.cast();
        for kind in [
            NSWindowButton::NSWindowCloseButton,
            NSWindowButton::NSWindowMiniaturizeButton,
            NSWindowButton::NSWindowZoomButton,
            NSWindowButton::NSWindowFullScreenButton,
        ] {
            let button = native.standardWindowButton_(kind);
            if button != nil {
                let _: () = msg_send![button, setHidden: YES];
            }
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn prepare_composer_chrome(_: &tauri::Window) -> Result<(), &'static str> {
    Ok(())
}

/// Sync window geometry before showing a new window or changing panel mode.
pub fn prepare_material(window: &WebviewWindow, head_panel: bool) {
    sync_webview_window(window, head_panel);
}

/// Equivalent hook for dynamically-created composer webview windows.
pub fn sync_webview_window(window: &WebviewWindow, head_panel: bool) {
    sync_webview(window.as_ref(), head_panel);
}

/// Native window access is always dispatched to the main thread, including composer
/// calls originating in asynchronous native commands.
pub fn sync_webview(webview: &Webview, head_panel: bool) {
    let chrome_webview = webview.clone();
    let _ = webview.run_on_main_thread(move || {
        let window = chrome_webview.window();
        let _ = prepare_composer_chrome(&window);
        let rounded = head_panel
            || (!window.is_maximized().unwrap_or(false)
                && !window.is_fullscreen().unwrap_or(false));
        let masked_corners = apply_window(&window, rounded);
        let value = if masked_corners { "true" } else { "false" };
        let _ = chrome_webview.eval(format!(
            "document.documentElement.dataset.windowRounded='{value}';"
        ));
    });
}

#[cfg(target_os = "windows")]
fn apply_window(window: &tauri::Window, rounded: bool) -> bool {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND,
    };

    let Ok(hwnd) = window.hwnd() else {
        return false;
    };
    let preference = if rounded {
        DWMWCP_ROUND
    } else {
        DWMWCP_DONOTROUND
    };
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const _ as _,
            std::mem::size_of_val(&preference) as u32,
        )
    };
    // DWM clips the native window itself. Keep the webview square to avoid a
    // second CSS radius with opaque pixels behind it, including when maximized.
    false
}

#[cfg(not(target_os = "windows"))]
fn apply_window(_: &tauri::Window, _: bool) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::{
        is_composer_window, restore_window, show_loaded, show_requested, show_window,
        ShowOperations,
    };
    use crate::material_state::Materials;

    #[derive(Default)]
    struct FakeWindow {
        preparation_results: VecDeque<bool>,
        events: Vec<&'static str>,
    }

    impl FakeWindow {
        fn with_preparation_results(results: impl IntoIterator<Item = bool>) -> Self {
            Self {
                preparation_results: results.into_iter().collect(),
                events: Vec::new(),
            }
        }
    }

    impl ShowOperations for FakeWindow {
        fn prepare(&mut self) -> Result<(), &'static str> {
            self.events.push("prepare");
            self.preparation_results
                .pop_front()
                .unwrap_or(true)
                .then_some(())
                .ok_or("native preparation failed")
        }

        fn report_preparation_failure(&mut self, _: &'static str) {
            self.events.push("report");
        }

        fn show(&mut self) {
            self.events.push("show");
        }

        fn unminimize(&mut self) {
            self.events.push("unminimize");
        }

        fn focus(&mut self) {
            self.events.push("focus");
        }
    }

    #[test]
    fn composer_preparation_targets_only_valid_composer_labels() {
        let composer = format!("composer-{}", uuid::Uuid::nil());

        assert!(is_composer_window(&composer));
        assert!(!is_composer_window("main"));
        assert!(!is_composer_window("composer-not-a-conversation"));
        assert!(!is_composer_window(
            "media-composer-00000000-0000-0000-0000-000000000000"
        ));
    }

    #[test]
    fn normal_load_retries_preparation_before_showing_and_focusing() {
        let mut materials = Materials::default();
        let mut window = FakeWindow::with_preparation_results([false, true]);

        if show_requested(&mut materials, "composer") {
            show_window(&mut window);
        }
        if show_loaded(&mut materials, "composer") {
            show_window(&mut window);
        }
        if show_requested(&mut materials, "composer") {
            show_window(&mut window);
        }

        assert_eq!(
            window.events,
            [
                "prepare",
                "report",
                "prepare",
                "show",
                "unminimize",
                "focus"
            ]
        );
    }

    #[test]
    fn failed_explicit_retry_stays_hidden_and_late_load_cannot_focus() {
        let mut materials = Materials::default();
        let mut window = FakeWindow::with_preparation_results([false, true]);

        if show_requested(&mut materials, "composer") {
            show_window(&mut window);
        }
        if show_requested(&mut materials, "composer") {
            show_window(&mut window);
        }
        if show_loaded(&mut materials, "composer") {
            show_window(&mut window);
        }
        if show_requested(&mut materials, "composer") {
            show_window(&mut window);
        }

        assert_eq!(
            window.events,
            [
                "prepare",
                "report",
                "prepare",
                "show",
                "unminimize",
                "focus"
            ]
        );
    }

    #[test]
    fn repeated_preparation_failures_suppress_visibility_until_a_retry_succeeds() {
        let mut materials = Materials::default();
        let mut window = FakeWindow::with_preparation_results([false, false, true]);

        if show_requested(&mut materials, "composer") {
            show_window(&mut window);
        }
        if show_loaded(&mut materials, "composer") {
            show_window(&mut window);
        }
        if show_requested(&mut materials, "composer") {
            show_window(&mut window);
        }
        if show_requested(&mut materials, "composer") {
            show_window(&mut window);
        }

        assert_eq!(
            window.events,
            [
                "prepare",
                "report",
                "prepare",
                "report",
                "prepare",
                "show",
                "unminimize",
                "focus"
            ]
        );
    }

    #[test]
    fn background_page_load_does_not_prepare_or_show_a_window() {
        let mut materials = Materials::default();
        let mut window = FakeWindow::with_preparation_results([true]);

        if show_loaded(&mut materials, "background") {
            show_window(&mut window);
        }

        assert!(window.events.is_empty());
    }

    #[test]
    fn recovery_prepares_before_restoring_without_waiting_for_page_load() {
        let mut window = FakeWindow::with_preparation_results([false, true]);

        restore_window(&mut window);
        restore_window(&mut window);

        assert_eq!(
            window.events,
            ["prepare", "report", "prepare", "show", "focus"]
        );
    }
}
