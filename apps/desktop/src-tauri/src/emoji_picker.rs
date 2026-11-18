//! Asks the operating system to show its own emoji panel for the reaction "More" picker. Peppy
//! ships no emoji dataset; the OS panel is the only picker.
use tauri::AppHandle;

#[cfg(any(target_os = "macos", target_os = "windows"))]
const PICKER_FAILED: &str = "emoji-picker-failed";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const UNSUPPORTED: &str = "unsupported";

#[tauri::command]
pub async fn open_emoji_picker(app: AppHandle) -> Result<(), String> {
    show_emoji_panel(app).await
}

#[cfg(target_os = "macos")]
async fn show_emoji_panel(app: AppHandle) -> Result<(), String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        // SAFETY: AppKit is used only on the main thread, where this closure runs.
        let _ = sender.send(unsafe { macos::order_front_character_palette() });
    })
    .map_err(|_| PICKER_FAILED.to_string())?;
    receiver.await.map_err(|_| PICKER_FAILED.to_string())?
}

#[cfg(target_os = "windows")]
async fn show_emoji_panel(_app: AppHandle) -> Result<(), String> {
    windows_picker::send_win_period()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
async fn show_emoji_panel(_app: AppHandle) -> Result<(), String> {
    Err(UNSUPPORTED.to_string())
}

#[cfg(target_os = "macos")]
mod macos {
    use super::PICKER_FAILED;
    use cocoa::base::{id, nil};
    use objc::{class, msg_send, sel, sel_impl};

    /// # Safety
    /// Must run on the AppKit main thread.
    pub unsafe fn order_front_character_palette() -> Result<(), String> {
        let application: id = msg_send![class!(NSApplication), sharedApplication];
        if application == nil {
            return Err(PICKER_FAILED.to_string());
        }
        let _: () = msg_send![application, orderFrontCharacterPalette: nil];
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod windows_picker {
    use super::PICKER_FAILED;
    use std::mem::size_of;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_LWIN, VK_OEM_PERIOD,
    };

    /// Win+. opens the Windows emoji panel.
    pub fn send_win_period() -> Result<(), String> {
        let inputs = [
            key_event(VK_LWIN, KEYBD_EVENT_FLAGS(0)),
            key_event(VK_OEM_PERIOD, KEYBD_EVENT_FLAGS(0)),
            key_event(VK_OEM_PERIOD, KEYEVENTF_KEYUP),
            key_event(VK_LWIN, KEYEVENTF_KEYUP),
        ];
        let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            Ok(())
        } else {
            Err(PICKER_FAILED.to_string())
        }
    }

    fn key_event(key: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: key,
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
}
