//! Native rounded window chrome for the main and standalone composer windows.
use std::cell::RefCell;
use tauri::{Webview, WebviewWindow};

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
        if MATERIALS.with(|state| state.borrow_mut().request_show(target.label())) {
            show_window(&target);
        }
    })
}

pub fn page_loaded(webview: &Webview) {
    let target = webview.clone();
    let _ = webview.run_on_main_thread(move || {
        if MATERIALS.with(|state| state.borrow_mut().page_loaded(target.label())) {
            let window = target.window();
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    });
}

fn show_window(window: &WebviewWindow) {
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
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
