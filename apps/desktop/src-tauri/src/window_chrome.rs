//! Native rounded window chrome for the main and standalone composer windows.
#![cfg_attr(target_os = "macos", allow(deprecated, unexpected_cfgs))]
use std::cell::RefCell;
use tauri::{Webview, WebviewWindow};

thread_local! {
    static MATERIALS: RefCell<crate::material_state::Materials> = RefCell::default();
}

/// Called on the event-loop thread when a window is destroyed. Reusing its label
/// must not reuse the old HWND/NSWindow's material result.
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

/// Queue before showing a new window, or when a composer changes panel mode.
/// Resize and page-load hooks only publish the cached result, never reinstall it.
pub fn prepare_material(window: &WebviewWindow, head_panel: bool) {
    let target = window.clone();
    let _ = window.run_on_main_thread(move || {
        let change = MATERIALS.with(|materials| {
            materials
                .borrow_mut()
                .begin_configure(target.label(), head_panel)
        });
        if let Some(enabled) = change {
            let installed = apply_material(&target, enabled);
            MATERIALS.with(|materials| {
                materials
                    .borrow_mut()
                    .finish_configure(target.label(), head_panel, installed)
            });
        }
        sync_webview_window(&target, head_panel);
    });
}

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
        let material = MATERIALS.with(|materials| materials.borrow().active(chrome_webview.label()));
        let material = if material && !head_panel { "active" } else { "opaque" };
        let _ = chrome_webview.eval(format!(
            "document.documentElement.dataset.windowRounded='{value}';document.documentElement.dataset.vibrancy='{material}';"
        ));
    });
}

#[cfg(target_os = "macos")]
fn apply_material(window: &WebviewWindow, enabled: bool) -> bool {
    use cocoa::base::{id, nil, BOOL, YES};
    use objc::{class, msg_send, sel, sel_impl};
    use tauri::window::{Effect, EffectState, EffectsBuilder};

    let effects = enabled.then(|| {
        EffectsBuilder::new()
            .effect(Effect::UnderWindowBackground)
            .state(EffectState::FollowsWindowActiveState)
            .build()
    });
    if window.set_effects(effects).is_err() || !enabled {
        return false;
    }
    // Tauri's set_effects reports dispatch success, not material installation.
    // Check the actual AppKit view before allowing transparent CSS surfaces.
    let Ok(native) = window.ns_window() else {
        return false;
    };
    unsafe {
        let view: id = msg_send![native as id, contentView];
        if view == nil {
            return false;
        }
        let children: id = msg_send![view, subviews];
        let count: usize = msg_send![children, count];
        (0..count).any(|index| {
            let child: id = msg_send![children, objectAtIndex: index];
            let material: BOOL = msg_send![child, isKindOfClass: class!(NSVisualEffectView)];
            material == YES
        })
    }
}

// Windows remains opaque until transparent WebView2/Mica rendering and resize
// behavior are verified on Windows. A set DWM attribute alone is not evidence.
#[cfg(not(target_os = "macos"))]
fn apply_material(_: &WebviewWindow, _: bool) -> bool {
    false
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
