//! Clipboard writes for secrets (vault key phrases, one-time codes). One long-lived owner keeps
//! Linux selection ownership alive, and every write asks the platform to keep the text out of
//! clipboard history (and cloud sync on Windows).
use std::sync::{Mutex, OnceLock};

static SENSITIVE_CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();

/// Returns `false` when the clipboard is unavailable or the write fails.
pub(crate) fn write_sensitive_text(text: &str) -> bool {
    let Ok(mut owner) = SENSITIVE_CLIPBOARD.get_or_init(|| Mutex::new(None)).lock() else {
        return false;
    };
    if owner.is_none() {
        let Ok(clipboard) = arboard::Clipboard::new() else {
            return false;
        };
        *owner = Some(clipboard);
    }
    owner
        .as_mut()
        .is_some_and(|clipboard| set_sensitive_clipboard_text(clipboard, text))
}

#[cfg(target_os = "macos")]
fn set_sensitive_clipboard_text(clipboard: &mut arboard::Clipboard, text: &str) -> bool {
    use arboard::SetExtApple;

    clipboard.set().exclude_from_history().text(text).is_ok()
}

#[cfg(target_os = "windows")]
fn set_sensitive_clipboard_text(clipboard: &mut arboard::Clipboard, text: &str) -> bool {
    use arboard::SetExtWindows;

    clipboard
        .set()
        .exclude_from_history()
        .exclude_from_cloud()
        .text(text)
        .is_ok()
}

#[cfg(target_os = "linux")]
fn set_sensitive_clipboard_text(clipboard: &mut arboard::Clipboard, text: &str) -> bool {
    use arboard::SetExtLinux;

    clipboard.set().exclude_from_history().text(text).is_ok()
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn set_sensitive_clipboard_text(_: &mut arboard::Clipboard, _: &str) -> bool {
    false
}
