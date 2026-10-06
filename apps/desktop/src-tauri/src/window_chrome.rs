//! Native rounded window chrome for the main and standalone composer windows.
#![cfg_attr(target_os = "macos", allow(deprecated, unexpected_cfgs))]
use tauri::{Webview, WebviewWindow};

#[cfg(target_os = "macos")]
const CORNER_RADIUS: f64 = 12.0;

/// Equivalent hook for dynamically-created composer webview windows.
pub fn sync_webview_window(window: &WebviewWindow, head_panel: bool) {
    sync_webview(window.as_ref(), head_panel);
}

/// AppKit access is always dispatched to the main thread, including composer calls
/// originating in asynchronous native commands.
pub fn sync_webview(webview: &Webview, head_panel: bool) {
    let chrome_webview = webview.clone();
    let _ = webview.run_on_main_thread(move || {
        let window = chrome_webview.window();
        let rounded = !head_panel
            && !window.is_maximized().unwrap_or(false)
            && !window.is_fullscreen().unwrap_or(false);
        let masked_corners = apply_window(&window, rounded);
        let value = if masked_corners { "true" } else { "false" };
        let _ = chrome_webview.eval(format!(
            "document.documentElement.dataset.windowRounded='{value}';"
        ));
    });
}

#[cfg(target_os = "macos")]
fn apply_window(window: &tauri::Window, rounded: bool) -> bool {
    let Ok(native) = window.ns_window() else {
        return false;
    };
    unsafe { apply_macos_window(native, rounded) };
    rounded
}

#[cfg(target_os = "macos")]
unsafe fn apply_macos_window(window: *mut std::ffi::c_void, rounded: bool) {
    use cocoa::base::{id, NO, YES};
    use objc::{class, msg_send, sel, sel_impl};

    let window = window as id;
    let _: () = msg_send![window, setOpaque: NO];
    let clear: id = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![window, setBackgroundColor: clear];
    let _: () = msg_send![window, setHasShadow: YES];
    let content_view: id = msg_send![window, contentView];
    let _: () = msg_send![content_view, setWantsLayer: YES];
    let layer: id = msg_send![content_view, layer];
    let _: () = msg_send![layer, setCornerRadius: if rounded { CORNER_RADIUS } else { 0.0 }];
    let _: () = msg_send![layer, setMasksToBounds: if rounded { YES } else { NO }];
    let _: () = msg_send![window, invalidateShadow];
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

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn apply_window(_: &tauri::Window, _: bool) -> bool {
    false
}
