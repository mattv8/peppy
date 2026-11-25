//! Keeps the floating chat panel, and the media viewers it opens, above ordinary windows.
//!
//! Tauri's always-on-top puts a macOS window at level 5: tao passes the index
//! `kCGFloatingWindowLevelKey` where AppKit expects a level. AppKit's floating level is 3,
//! which the floating heads and other apps' floating windows use. At level 5 the panel hid
//! them, such as the browser picker Choosy opens under the pointer after a link click.

// objc's macros still emit the legacy cargo-clippy feature check.
#![allow(unexpected_cfgs)]

#[cfg(target_os = "macos")]
use objc::{msg_send, sel, sel_impl};

/// AppKit's `NSFloatingWindowLevel`; the floating heads use it too.
#[cfg(target_os = "macos")]
pub(crate) const FLOATING_WINDOW_LEVEL: i64 = 3;
/// AppKit's `NSNormalWindowLevel`.
#[cfg(target_os = "macos")]
const NORMAL_WINDOW_LEVEL: i64 = 0;

/// Floats `window` above ordinary windows, or returns it among them. Callable from any thread:
/// the level is set on the main thread, in order with `window_chrome::show_when_ready`.
#[cfg(target_os = "macos")]
pub(crate) fn set_floating(window: &tauri::Window, floating: bool) -> tauri::Result<()> {
    let level = if floating {
        FLOATING_WINDOW_LEVEL
    } else {
        NORMAL_WINDOW_LEVEL
    };
    let target = window.clone();
    window.run_on_main_thread(move || {
        if let Some(native) = native_window(&target) {
            let _: () = unsafe { msg_send![native, setLevel: level] };
        }
    })
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn set_floating(window: &tauri::Window, floating: bool) -> tauri::Result<()> {
    window.set_always_on_top(floating)
}

/// Whether `window` is at the floating level; `None` without a native window. Main thread only.
#[cfg(target_os = "macos")]
pub(crate) fn is_floating(window: &tauri::Window) -> Option<bool> {
    let native = native_window(window)?;
    let level: i64 = unsafe { msg_send![native, level] };
    Some(level == FLOATING_WINDOW_LEVEL)
}

#[cfg(target_os = "macos")]
fn native_window(window: &tauri::Window) -> Option<*mut objc::runtime::Object> {
    let native = window.ns_window().ok()?;
    (!native.is_null()).then_some(native.cast())
}
