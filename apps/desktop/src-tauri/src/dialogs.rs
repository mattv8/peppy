//! Native dialogs. File choice and confirmations use the maintained Tauri dialog plugin (native
//! panels on macOS/Windows/Linux). The sync passphrase is collected only by a native secure
//! field (AppKit `NSAlert` + `NSSecureTextField` on macOS); it is never requested in the webview,
//! never passed through IPC, and never placed on a process argv.
use crate::error::{BridgeError, BridgeResult};
use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
};
use tauri::AppHandle;
use tauri_plugin_dialog::{
    DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PassphrasePurpose {
    Unlock,
    ResumeOwnerSecret,
    RecoverVaultSecret,
    Create,
    SetupToken,
    AdminPassword,
    AdminPasswordCreate,
    AdminPasswordConfirm,
    AdminPasswordMismatch,
    AppLockCreate,
    AppLockConfirm,
    AppLockUnlock,
    AppLockDisable,
}

fn dialog_error() -> BridgeError {
    BridgeError::new("native-dialog", "The native dialog could not be shown.")
}

const VAULT_SECRET_TITLE: &str = "Vault shared secret";
const VAULT_SECRET_INSTRUCTIONS: &str =
    "Use this secret to connect a phone or another device to your vault. Keep it private.";
const VAULT_SECRET_DONE: &str = "Done";
const VAULT_SECRET_COPY: &str = "Copy secret";
// Escape and the window close box reach this slot on Windows and Linux (see
// `vault_secret_buttons`), so its label must not be the copy action.
const VAULT_SECRET_CLOSE: &str = "Close";
const COPY_SUCCEEDED: &str = "Copied";
const COPY_FAILED: &str = "Could not copy. Try again.";

static SECRET_CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();

trait SecretClipboard {
    fn copy_secret(&mut self, secret: &str) -> bool;
}

struct NativeSecretClipboard;

impl SecretClipboard for NativeSecretClipboard {
    fn copy_secret(&mut self, secret: &str) -> bool {
        let Ok(mut clipboard) = SECRET_CLIPBOARD.get_or_init(|| Mutex::new(None)).lock() else {
            return false;
        };
        if clipboard.is_none() {
            let Ok(new_clipboard) = arboard::Clipboard::new() else {
                return false;
            };
            *clipboard = Some(new_clipboard);
        }
        clipboard
            .as_mut()
            .is_some_and(|clipboard| set_sensitive_clipboard_text(clipboard, secret))
    }
}

#[cfg(target_os = "macos")]
fn set_sensitive_clipboard_text(clipboard: &mut arboard::Clipboard, secret: &str) -> bool {
    use arboard::SetExtApple;

    clipboard.set().exclude_from_history().text(secret).is_ok()
}

#[cfg(target_os = "windows")]
fn set_sensitive_clipboard_text(clipboard: &mut arboard::Clipboard, secret: &str) -> bool {
    use arboard::SetExtWindows;

    clipboard
        .set()
        .exclude_from_history()
        .exclude_from_cloud()
        .text(secret)
        .is_ok()
}

#[cfg(target_os = "linux")]
fn set_sensitive_clipboard_text(clipboard: &mut arboard::Clipboard, secret: &str) -> bool {
    use arboard::SetExtLinux;

    clipboard.set().exclude_from_history().text(secret).is_ok()
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn set_sensitive_clipboard_text(_: &mut arboard::Clipboard, _: &str) -> bool {
    false
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SecretDialogAction {
    Done,
    Copy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SecretDialogStep {
    Finish,
    ShowFeedback(&'static str),
}

/// Maps the plugin result of the vault secret dialog. Only the copy label copies; every other
/// result (Done, Close, Escape, or the window close box) means Done.
fn vault_secret_action(result: MessageDialogResult) -> SecretDialogAction {
    match result {
        MessageDialogResult::Custom(label) if label == VAULT_SECRET_COPY => {
            SecretDialogAction::Copy
        }
        _ => SecretDialogAction::Done,
    }
}

fn vault_secret_buttons() -> MessageDialogButtons {
    MessageDialogButtons::YesNoCancelCustom(
        VAULT_SECRET_DONE.into(),
        VAULT_SECRET_COPY.into(),
        VAULT_SECRET_CLOSE.into(),
    )
}

fn handle_secret_dialog_action(
    clipboard: &mut impl SecretClipboard,
    secret: &str,
    action: SecretDialogAction,
) -> SecretDialogStep {
    match action {
        SecretDialogAction::Done => SecretDialogStep::Finish,
        SecretDialogAction::Copy => {
            let feedback = if clipboard.copy_secret(secret) {
                COPY_SUCCEEDED
            } else {
                COPY_FAILED
            };
            SecretDialogStep::ShowFeedback(feedback)
        }
    }
}

fn secret_message(instructions: &str, secret: &str, feedback: Option<&str>) -> Zeroizing<String> {
    let mut message = Zeroizing::new(format!("{instructions}\n\nVault shared secret:\n{secret}"));
    if let Some(feedback) = feedback {
        message.push_str("\n\n");
        message.push_str(feedback);
    }
    message
}

async fn show_secret_dialog(
    app: &AppHandle,
    secret: &str,
    feedback: Option<&str>,
    title: &str,
    instructions: &str,
    buttons: MessageDialogButtons,
    action: fn(MessageDialogResult) -> SecretDialogAction,
) -> BridgeResult<SecretDialogAction> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let message = secret_message(instructions, secret, feedback);
    app.dialog()
        .message(message.as_str())
        .title(title)
        .kind(MessageDialogKind::Info)
        .buttons(buttons)
        .show_with_result(move |result| {
            let _ = sender.send(action(result));
        });
    receiver.await.map_err(|_| dialog_error())
}

/// Displays a native-only vault secret. Done, Close, Escape, and the close box all mean Done;
/// only the copy button writes the clipboard.
pub async fn show_vault_secret(app: &AppHandle, secret: Zeroizing<String>) -> BridgeResult<()> {
    let mut clipboard = NativeSecretClipboard;
    let mut feedback = None;
    loop {
        let action = show_secret_dialog(
            app,
            secret.as_str(),
            feedback,
            VAULT_SECRET_TITLE,
            VAULT_SECRET_INSTRUCTIONS,
            vault_secret_buttons(),
            vault_secret_action,
        )
        .await?;
        match handle_secret_dialog_action(&mut clipboard, secret.as_str(), action) {
            SecretDialogStep::Finish => return Ok(()),
            SecretDialogStep::ShowFeedback(next_feedback) => feedback = Some(next_feedback),
        }
    }
}

pub async fn pick_file(app: &AppHandle, title: &str) -> BridgeResult<Option<PathBuf>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title(title)
        .pick_file(move |picked| {
            let _ = sender.send(picked);
        });
    let picked = receiver.await.map_err(|_| dialog_error())?;
    picked
        .map(|path| {
            path.into_path().map_err(|_| {
                BridgeError::new("native-dialog", "The selected item is not a local file.")
            })
        })
        .transpose()
}

pub async fn pick_files(app: &AppHandle, title: &str) -> BridgeResult<Vec<PathBuf>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title(title)
        .pick_files(move |picked| {
            let _ = sender.send(picked);
        });
    let picked = receiver
        .await
        .map_err(|_| dialog_error())?
        .unwrap_or_default();
    picked
        .into_iter()
        .map(|path| {
            path.into_path().map_err(|_| {
                BridgeError::new("native-dialog", "The selected item is not a local file.")
            })
        })
        .collect()
}

/// Native save panel; the webview never receives or supplies a filesystem path.
pub async fn save_file(
    app: &AppHandle,
    title: &str,
    suggested_name: &str,
) -> BridgeResult<Option<PathBuf>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title(title)
        .set_file_name(suggested_name)
        .save_file(move |picked| {
            let _ = sender.send(picked);
        });
    receiver
        .await
        .map_err(|_| dialog_error())?
        .map(|path| {
            path.into_path().map_err(|_| {
                BridgeError::new("native-dialog", "The selected item is not a local file.")
            })
        })
        .transpose()
}

pub async fn confirm(
    app: &AppHandle,
    title: &str,
    message: &str,
    accept: &str,
) -> BridgeResult<bool> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(message)
        .title(title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            accept.into(),
            "Cancel".into(),
        ))
        .show(move |accepted| {
            let _ = sender.send(accepted);
        });
    receiver.await.map_err(|_| dialog_error())
}

pub fn inform(app: &AppHandle, title: &str, message: &str) {
    app.dialog()
        .message(message)
        .title(title)
        .kind(MessageDialogKind::Info)
        .show(|_| {});
}

/// Returns `None` when the person cancels. The passphrase is collected only by a native secure
/// control and is never sent through the webview or process argv.
#[cfg(target_os = "macos")]
pub async fn passphrase(
    app: &AppHandle,
    purpose: PassphrasePurpose,
) -> BridgeResult<Option<Zeroizing<String>>> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        // SAFETY: AppKit objects are created, used and released on the main thread only.
        let _ = sender.send(unsafe { macos::secure_passphrase_alert(purpose) });
    })
    .map_err(|_| dialog_error())?;
    receiver.await.map_err(|_| dialog_error())
}

#[cfg(target_os = "windows")]
pub async fn passphrase(
    _: &AppHandle,
    purpose: PassphrasePurpose,
) -> BridgeResult<Option<Zeroizing<String>>> {
    tokio::task::spawn_blocking(move || windows_dialog::secure_passphrase(purpose))
        .await
        .map_err(|_| dialog_error())?
}

#[cfg(target_os = "linux")]
pub async fn passphrase(
    _: &AppHandle,
    purpose: PassphrasePurpose,
) -> BridgeResult<Option<Zeroizing<String>>> {
    tokio::task::spawn_blocking(move || linux::secure_passphrase(purpose))
        .await
        .map_err(|_| dialog_error())?
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub async fn passphrase(
    _: &AppHandle,
    _: PassphrasePurpose,
) -> BridgeResult<Option<Zeroizing<String>>> {
    Err(BridgeError::new(
        "platform-gate",
        "This platform does not yet have a reviewed native secure passphrase dialog; Peppy will not ask for it in the webview.",
    ))
}

#[cfg(any(target_os = "windows", target_os = "linux", test))]
const MAX_PASSPHRASE_BYTES: usize = 4 * 1024;

#[cfg(any(target_os = "windows", target_os = "linux", test))]
fn passphrase_from_utf8(mut bytes: Vec<u8>) -> BridgeResult<Zeroizing<String>> {
    if bytes.len() > MAX_PASSPHRASE_BYTES {
        use zeroize::Zeroize;
        bytes.zeroize();
        return Err(BridgeError::new(
            "native-dialog",
            "The native passphrase dialog returned too much input.",
        ));
    }

    match String::from_utf8(bytes) {
        Ok(value) => Ok(Zeroizing::new(value)),
        Err(error) => {
            use zeroize::Zeroize;
            let mut bytes = error.into_bytes();
            bytes.zeroize();
            Err(BridgeError::new(
                "native-dialog",
                "The native passphrase dialog returned invalid text.",
            ))
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn trim_helper_terminator(value: &mut Vec<u8>) {
    use zeroize::Zeroize;

    let length = value.len();
    let terminator = if value.ends_with(b"\r\n") {
        2
    } else if value.ends_with(b"\n") {
        1
    } else {
        0
    };
    value[length - terminator..].zeroize();
    value.truncate(length - terminator);
}

#[cfg(target_os = "windows")]
mod windows_dialog {
    use super::{
        passphrase_from_utf8, BridgeError, BridgeResult, PassphrasePurpose, Zeroizing,
        MAX_PASSPHRASE_BYTES,
    };
    use windows::{
        core::w,
        Win32::{
            Foundation::{ERROR_CANCELLED, HWND, WIN32_ERROR},
            Graphics::Gdi::HBITMAP,
            Security::Credentials::{
                CredUIPromptForCredentialsW, CREDUI_FLAGS_ALWAYS_SHOW_UI,
                CREDUI_FLAGS_DO_NOT_PERSIST, CREDUI_FLAGS_GENERIC_CREDENTIALS,
                CREDUI_FLAGS_PASSWORD_ONLY_OK, CREDUI_INFOW, CREDUI_MAX_USERNAME_LENGTH,
                CRED_MAX_CREDENTIAL_BLOB_SIZE,
            },
        },
    };
    use zeroize::Zeroize;

    /// `wincred.h`: `#define CREDUI_MAX_PASSWORD_LENGTH (CRED_MAX_CREDENTIAL_BLOB_SIZE / 2)`.
    /// The `windows` crate exports the blob size but not this derived macro.
    const CREDUI_MAX_PASSWORD_LENGTH: usize = CRED_MAX_CREDENTIAL_BLOB_SIZE as usize / 2;
    // CredUI documents both buffers as (maximum length + 1) WCHARs, including the terminator.
    const PASSWORD_BUFFER: usize = CREDUI_MAX_PASSWORD_LENGTH + 1;
    const USERNAME_BUFFER: usize = CREDUI_MAX_USERNAME_LENGTH as usize + 1;
    // Every accepted UTF-16 passphrase must fit the shared UTF-8 bound after conversion.
    const _: () = assert!(CREDUI_MAX_PASSWORD_LENGTH * 3 <= MAX_PASSPHRASE_BYTES);

    pub fn secure_passphrase(
        purpose: PassphrasePurpose,
    ) -> BridgeResult<Option<Zeroizing<String>>> {
        let mut username = Zeroizing::new([0u16; USERNAME_BUFFER]);
        let mut password = Zeroizing::new([0u16; PASSWORD_BUFFER]);
        let (message, caption) = match purpose {
            PassphrasePurpose::Unlock => (
                w!(
                    "Enter the vault passphrase. It is verified on this device and never sent to the server. If successful, Peppy may save it to this device's protected secure storage for phone setup."
                ),
                w!("Unlock Peppy sync"),
            ),
            PassphrasePurpose::ResumeOwnerSecret => (
                w!(
                    "Enter the vault shared secret you saved during your earlier setup attempt. This is not your admin password. It is verified only on this device and is never sent to the server."
                ),
                w!("Resume Peppy server setup"),
            ),
            PassphrasePurpose::RecoverVaultSecret => (
                w!(
                    "Enter the vault shared secret you saved when this vault was created. This is not your admin password. It is verified only on this device and is never sent to the server."
                ),
                w!("Complete vault setup"),
            ),
            PassphrasePurpose::Create => (
                w!(
                    "Choose the encryption passphrase for your Peppy server. Enter the same passphrase on every device. It stays on this device, is never sent to Peppy, and cannot be recovered."
                ),
                w!("Create encryption passphrase"),
            ),
            PassphrasePurpose::AppLockCreate => (
                w!("Choose a password required when Peppy opens on this computer."),
                w!("Require a password to open Peppy"),
            ),
            PassphrasePurpose::AppLockConfirm => (
                w!("Enter the same password again."),
                w!("Confirm Peppy password"),
            ),
            PassphrasePurpose::AppLockUnlock => (
                w!("Enter the password required to open Peppy on this computer."),
                w!("Unlock Peppy"),
            ),
            PassphrasePurpose::AppLockDisable => (
                w!("Enter your current Peppy password to turn off the app lock."),
                w!("Turn off Peppy app lock"),
            ),
            PassphrasePurpose::SetupToken => (
                w!(
                    "Enter the server setup token. It is sent only to this server and is never exposed to the Peppy window."
                ),
                w!("Claim Peppy server"),
            ),
            PassphrasePurpose::AdminPassword => (
                w!(
                    "Enter the server administrator password. It must be 12–1024 UTF-8 bytes. It is sent only to this server and is never exposed to the Peppy window."
                ),
                w!("Peppy server administrator"),
            ),
            PassphrasePurpose::AdminPasswordCreate => (
                w!(
                    "Choose the administrator password for this server. It must be 12–1024 UTF-8 bytes. It is sent only to this server and is never exposed to the Peppy window."
                ),
                w!("Set server administrator password"),
            ),
            PassphrasePurpose::AdminPasswordConfirm => (
                w!("Enter the same administrator password again."),
                w!("Confirm server administrator password"),
            ),
            PassphrasePurpose::AdminPasswordMismatch => (
                w!("The administrator passwords you entered do not match. Try again."),
                w!("Passwords do not match"),
            ),
        };
        let info = CREDUI_INFOW {
            cbSize: std::mem::size_of::<CREDUI_INFOW>() as u32,
            hwndParent: HWND::default(),
            pszMessageText: message,
            pszCaptionText: caption,
            hbmBanner: HBITMAP::default(),
        };
        // DO_NOT_PERSIST prevents Credential Manager storage; PASSWORD_ONLY_OK retains the
        // native password control without accepting a username.
        let result = unsafe {
            CredUIPromptForCredentialsW(
                Some(&info),
                match purpose {
                    PassphrasePurpose::Unlock
                    | PassphrasePurpose::ResumeOwnerSecret
                    | PassphrasePurpose::RecoverVaultSecret => w!("Peppy sync vault"),
                    PassphrasePurpose::Create => w!("Peppy encryption passphrase"),
                    PassphrasePurpose::AppLockCreate
                    | PassphrasePurpose::AppLockConfirm
                    | PassphrasePurpose::AppLockUnlock
                    | PassphrasePurpose::AppLockDisable => w!("Peppy app lock"),
                    PassphrasePurpose::SetupToken
                    | PassphrasePurpose::AdminPassword
                    | PassphrasePurpose::AdminPasswordCreate
                    | PassphrasePurpose::AdminPasswordConfirm
                    | PassphrasePurpose::AdminPasswordMismatch => {
                        w!("Peppy server")
                    }
                },
                None,
                0,
                &mut username[..],
                &mut password[..],
                None,
                CREDUI_FLAGS_DO_NOT_PERSIST
                    | CREDUI_FLAGS_GENERIC_CREDENTIALS
                    | CREDUI_FLAGS_ALWAYS_SHOW_UI
                    | CREDUI_FLAGS_PASSWORD_ONLY_OK,
            )
        };
        username.zeroize();

        if result == ERROR_CANCELLED {
            password.zeroize();
            return Ok(None);
        }
        if result != WIN32_ERROR(0) {
            password.zeroize();
            return Err(BridgeError::new(
                "native-dialog",
                "The Windows Credential UI could not show the passphrase dialog.",
            ));
        }

        let length = password
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(password.len());
        let value = String::from_utf16(&password[..length]).map_err(|_| {
            BridgeError::new(
                "native-dialog",
                "The Windows Credential UI returned invalid text.",
            )
        });
        password.zeroize();
        value
            .and_then(|value| passphrase_from_utf8(value.into_bytes()))
            .map(Some)
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{
        passphrase_from_utf8, trim_helper_terminator, BridgeError, BridgeResult, PassphrasePurpose,
        Zeroizing, MAX_PASSPHRASE_BYTES,
    };
    use std::{
        io::{Read, Result as IoResult},
        process::{Command, Stdio},
    };

    const UNLOCK_TITLE: &str = "Unlock Peppy sync";
    const UNLOCK_MESSAGE: &str =
        "Enter the vault passphrase. It is verified on this device and never sent to the server. If successful, Peppy may save it to this device's protected secure storage for phone setup.";
    const RESUME_OWNER_SECRET_TITLE: &str = "Resume Peppy server setup";
    const RESUME_OWNER_SECRET_MESSAGE: &str = "Enter the vault shared secret you saved during your earlier setup attempt. This is not your admin password. It is verified only on this device and is never sent to the server.";
    const RECOVER_VAULT_SECRET_TITLE: &str = "Complete vault setup";
    const RECOVER_VAULT_SECRET_MESSAGE: &str = "Enter the vault shared secret you saved when this vault was created. This is not your admin password. It is verified only on this device and is never sent to the server.";
    const CREATE_TITLE: &str = "Create encryption passphrase";
    const CREATE_MESSAGE: &str = "Choose the encryption passphrase for your Peppy server. Enter the same passphrase on every device. It stays on this device, is never sent to Peppy, and cannot be recovered.";
    const APP_LOCK_MESSAGE: &str = "Enter the password required to open Peppy on this computer.";

    pub fn secure_passphrase(
        purpose: PassphrasePurpose,
    ) -> BridgeResult<Option<Zeroizing<String>>> {
        let (title, message) = match purpose {
            PassphrasePurpose::Unlock => (UNLOCK_TITLE, UNLOCK_MESSAGE),
            PassphrasePurpose::ResumeOwnerSecret => {
                (RESUME_OWNER_SECRET_TITLE, RESUME_OWNER_SECRET_MESSAGE)
            }
            PassphrasePurpose::RecoverVaultSecret => {
                (RECOVER_VAULT_SECRET_TITLE, RECOVER_VAULT_SECRET_MESSAGE)
            }
            PassphrasePurpose::Create => (CREATE_TITLE, CREATE_MESSAGE),
            PassphrasePurpose::AppLockCreate => (
                "Require a password to open Peppy",
                "Choose a password required when Peppy opens on this computer.",
            ),
            PassphrasePurpose::AppLockConfirm => {
                ("Confirm Peppy password", "Enter the same password again.")
            }
            PassphrasePurpose::AppLockUnlock => ("Unlock Peppy", APP_LOCK_MESSAGE),
            PassphrasePurpose::AppLockDisable => (
                "Turn off Peppy app lock",
                "Enter your current Peppy password to turn off the app lock.",
            ),
            PassphrasePurpose::SetupToken => (
                "Claim Peppy server",
                "Enter the server setup token. It is sent only to this server and is never exposed to the Peppy window.",
            ),
            PassphrasePurpose::AdminPassword => (
                "Peppy server administrator",
                "Enter the server administrator password. It must be 12–1024 UTF-8 bytes. It is sent only to this server and is never exposed to the Peppy window.",
            ),
            PassphrasePurpose::AdminPasswordCreate => (
                "Set server administrator password",
                "Choose the administrator password for this server. It must be 12–1024 UTF-8 bytes. It is sent only to this server and is never exposed to the Peppy window.",
            ),
            PassphrasePurpose::AdminPasswordConfirm => (
                "Confirm server administrator password",
                "Enter the same administrator password again.",
            ),
            PassphrasePurpose::AdminPasswordMismatch => (
                "Passwords do not match",
                "The administrator passwords you entered do not match. Try again.",
            ),
        };
        for (program, arguments) in [
            (
                "zenity",
                vec!["--password", "--title", title, "--text", message],
            ),
            ("kdialog", vec!["--title", title, "--password", message]),
        ] {
            match run_helper(program, &arguments) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Ok(value) => return value,
                Err(_) => {
                    return Err(BridgeError::new(
                        "native-dialog",
                        "The native passphrase dialog could not be shown.",
                    ));
                }
            }
        }
        Err(BridgeError::new(
            "platform-gate",
            "No reviewed native passphrase dialog helper is installed. Install zenity or kdialog; Peppy will not ask for it in the webview.",
        ))
    }

    fn run_helper(
        program: &str,
        arguments: &[&str],
    ) -> IoResult<BridgeResult<Option<Zeroizing<String>>>> {
        let mut child = Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        // Preallocated so reading never reallocates (leaving unzeroed copies), and zeroized on
        // every exit path, including a failing `wait`.
        let mut output = Zeroizing::new(Vec::with_capacity(MAX_PASSPHRASE_BYTES + 1));
        let read_result = child
            .stdout
            .take()
            .expect("stdout was configured as piped")
            .take((MAX_PASSPHRASE_BYTES + 1) as u64)
            .read_to_end(&mut *output);
        if read_result.is_err() || output.len() > MAX_PASSPHRASE_BYTES {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Err(BridgeError::new(
                "native-dialog",
                "The native passphrase dialog returned too much input.",
            )));
        }
        let status = child.wait()?;
        if status.success() {
            trim_helper_terminator(&mut output);
            return Ok(passphrase_from_utf8(std::mem::take(&mut *output)).map(Some));
        }
        if status.code() == Some(1) {
            Ok(Ok(None))
        } else {
            Ok(Err(BridgeError::new(
                "native-dialog",
                "The native passphrase dialog could not be shown.",
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        handle_secret_dialog_action, passphrase_from_utf8, secret_message, trim_helper_terminator,
        vault_secret_action, vault_secret_buttons, SecretClipboard, SecretDialogAction,
        SecretDialogStep, COPY_FAILED, COPY_SUCCEEDED, VAULT_SECRET_CLOSE, VAULT_SECRET_COPY,
        VAULT_SECRET_DONE, VAULT_SECRET_INSTRUCTIONS,
    };
    use tauri_plugin_dialog::{MessageDialogButtons, MessageDialogResult};

    struct FakeClipboard {
        copied: Vec<String>,
        results: Vec<bool>,
    }

    impl SecretClipboard for FakeClipboard {
        fn copy_secret(&mut self, secret: &str) -> bool {
            self.copied.push(secret.to_owned());
            self.results.remove(0)
        }
    }

    #[test]
    fn vault_secret_copy_then_done_copies_only_the_phrase() {
        let secret = "synthetic secret phrase";
        let mut clipboard = FakeClipboard {
            copied: Vec::new(),
            results: vec![true],
        };

        assert_eq!(
            handle_secret_dialog_action(&mut clipboard, secret, SecretDialogAction::Copy),
            SecretDialogStep::ShowFeedback(COPY_SUCCEEDED)
        );
        assert_eq!(
            handle_secret_dialog_action(&mut clipboard, secret, SecretDialogAction::Done),
            SecretDialogStep::Finish
        );
        assert_eq!(clipboard.copied, [secret]);
        assert!(
            secret_message(VAULT_SECRET_INSTRUCTIONS, secret, Some(COPY_SUCCEEDED))
                .contains(secret)
        );
    }

    #[test]
    fn vault_secret_retries_after_a_failed_copy() {
        let mut clipboard = FakeClipboard {
            copied: Vec::new(),
            results: vec![false, true],
        };

        assert_eq!(
            handle_secret_dialog_action(&mut clipboard, "synthetic", SecretDialogAction::Copy),
            SecretDialogStep::ShowFeedback(COPY_FAILED)
        );
        assert_eq!(
            handle_secret_dialog_action(&mut clipboard, "synthetic", SecretDialogAction::Copy),
            SecretDialogStep::ShowFeedback(COPY_SUCCEEDED)
        );
        assert_eq!(clipboard.copied, ["synthetic", "synthetic"]);
    }

    #[test]
    fn vault_secret_done_without_copy_never_touches_the_clipboard() {
        let mut clipboard = FakeClipboard {
            copied: Vec::new(),
            results: Vec::new(),
        };

        assert_eq!(
            handle_secret_dialog_action(&mut clipboard, "synthetic", SecretDialogAction::Done),
            SecretDialogStep::Finish
        );
        assert!(clipboard.copied.is_empty());
    }

    #[test]
    fn vault_secret_maps_only_the_copy_label_to_copy() {
        assert_eq!(
            vault_secret_action(MessageDialogResult::Custom(VAULT_SECRET_COPY.into())),
            SecretDialogAction::Copy
        );
        assert_eq!(
            vault_secret_action(MessageDialogResult::Custom(VAULT_SECRET_DONE.into())),
            SecretDialogAction::Done
        );
        assert_eq!(
            vault_secret_action(MessageDialogResult::Custom(VAULT_SECRET_CLOSE.into())),
            SecretDialogAction::Done
        );
        // Escape and the close box surface as a raw Cancel on some platforms.
        assert_eq!(
            vault_secret_action(MessageDialogResult::Cancel),
            SecretDialogAction::Done
        );
    }

    #[test]
    fn vault_secret_escape_and_close_never_reach_the_copy_label() {
        let MessageDialogButtons::YesNoCancelCustom(done, copy, close) = vault_secret_buttons()
        else {
            panic!("vault secret must use three custom buttons");
        };
        assert_eq!(done, VAULT_SECRET_DONE);
        assert_eq!(copy, VAULT_SECRET_COPY);
        // The plugin reports Escape/close as Custom(cancel label) on Windows and Linux.
        assert_eq!(close, VAULT_SECRET_CLOSE);
        assert_ne!(close, VAULT_SECRET_COPY);
        assert_eq!(
            vault_secret_action(MessageDialogResult::Custom(close)),
            SecretDialogAction::Done
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn passphrase_purpose_preserves_unlock_copy() {
        let unlock = super::macos::copy(super::PassphrasePurpose::Unlock);
        let create = super::macos::copy(super::PassphrasePurpose::Create);
        let admin_password = super::macos::copy(super::PassphrasePurpose::AdminPassword);
        let admin_password_create =
            super::macos::copy(super::PassphrasePurpose::AdminPasswordCreate);
        let admin_password_confirm =
            super::macos::copy(super::PassphrasePurpose::AdminPasswordConfirm);
        let admin_password_mismatch =
            super::macos::copy(super::PassphrasePurpose::AdminPasswordMismatch);
        let resume = super::macos::copy(super::PassphrasePurpose::ResumeOwnerSecret);
        let recover = super::macos::copy(super::PassphrasePurpose::RecoverVaultSecret);
        assert_eq!(unlock.0, "Unlock Peppy sync");
        assert_eq!(
            unlock.1,
            "Enter the vault passphrase. It is verified on this device and never sent to the server. If successful, Peppy may save it to this device's protected secure storage for phone setup."
        );
        assert_eq!(unlock.2, "Unlock");
        assert_ne!(unlock, create);
        assert_eq!(create.2, "Create");
        assert!(admin_password.1.contains("12–1024 UTF-8 bytes"));
        assert!(admin_password_create.1.contains("12–1024 UTF-8 bytes"));
        assert_ne!(admin_password_create, admin_password_confirm);
        assert_ne!(admin_password_create, admin_password);
        assert!(admin_password_mismatch.1.contains("do not match"));
        assert_eq!(resume.0, "Resume Peppy server setup");
        assert!(resume.1.contains("earlier setup attempt"));
        assert_eq!(recover.0, "Complete vault setup");
        assert!(recover.1.contains("when this vault was created"));
    }

    #[test]
    fn accepts_utf8_passphrases_within_the_bound() {
        assert_eq!(
            passphrase_from_utf8("sëcret".into()).unwrap().as_str(),
            "sëcret"
        );
    }

    #[test]
    fn rejects_invalid_or_oversize_dialog_output() {
        assert!(passphrase_from_utf8(vec![0xff]).is_err());
        assert!(passphrase_from_utf8(vec![b'a'; super::MAX_PASSPHRASE_BYTES + 1]).is_err());
    }

    #[test]
    fn removes_only_the_dialog_helper_line_terminator() {
        let mut output = b"s\xc3\xabcret\r\n".to_vec();
        trim_helper_terminator(&mut output);
        assert_eq!(output, "sëcret".as_bytes());
    }
}

#[cfg(target_os = "macos")]
// `objc` 0.2 macros probe a legacy `cargo-clippy` cfg.
#[allow(unexpected_cfgs, deprecated)]
mod macos {
    use cocoa::{
        base::{id, nil, YES},
        foundation::{NSAutoreleasePool, NSPoint, NSRect, NSSize, NSString},
    };
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::CStr;
    use zeroize::Zeroizing;

    /// `NSAlertFirstButtonReturn`.
    const FIRST_BUTTON: isize = 1000;

    unsafe fn text(value: &str) -> id {
        let string = NSString::alloc(nil).init_str(value);
        msg_send![string, autorelease]
    }

    pub(super) fn copy(
        purpose: super::PassphrasePurpose,
    ) -> (&'static str, &'static str, &'static str) {
        match purpose {
            super::PassphrasePurpose::Unlock => (
                "Unlock Peppy sync",
                "Enter the vault passphrase. It is verified on this device and never sent to the server. If successful, Peppy may save it to this device's protected secure storage for phone setup.",
                "Unlock",
            ),
            super::PassphrasePurpose::ResumeOwnerSecret => (
                "Resume Peppy server setup",
                "Enter the vault shared secret you saved during your earlier setup attempt. This is not your admin password. It is verified only on this device and is never sent to the server.",
                "Continue",
            ),
            super::PassphrasePurpose::RecoverVaultSecret => (
                "Complete vault setup",
                "Enter the vault shared secret you saved when this vault was created. This is not your admin password. It is verified only on this device and is never sent to the server.",
                "Continue",
            ),
            super::PassphrasePurpose::Create => (
                "Create encryption passphrase",
                "Choose the encryption passphrase for your Peppy server. Enter the same passphrase on every device. It stays on this device, is never sent to Peppy, and cannot be recovered.",
                "Create",
            ),
            super::PassphrasePurpose::SetupToken => (
                "Claim Peppy server",
                "Enter the server setup token. It is sent only to this server and is never exposed to the Peppy window.",
                "Continue",
            ),
            super::PassphrasePurpose::AdminPassword => (
                "Peppy server administrator",
                "Enter the server administrator password. It must be 12–1024 UTF-8 bytes. It is sent only to this server and is never exposed to the Peppy window.",
                "Continue",
            ),
            super::PassphrasePurpose::AdminPasswordCreate => (
                "Set server administrator password",
                "Choose the administrator password for this server. It must be 12–1024 UTF-8 bytes. It is sent only to this server and is never exposed to the Peppy window.",
                "Continue",
            ),
            super::PassphrasePurpose::AdminPasswordConfirm => (
                "Confirm server administrator password",
                "Enter the same administrator password again.",
                "Continue",
            ),
            super::PassphrasePurpose::AdminPasswordMismatch => (
                "Passwords do not match",
                "The administrator passwords you entered do not match. Try again.",
                "Continue",
            ),
            super::PassphrasePurpose::AppLockCreate => (
                "Require a password to open Peppy",
                "Choose a password required when Peppy opens on this computer.",
                "Continue",
            ),
            super::PassphrasePurpose::AppLockConfirm => (
                "Confirm Peppy password",
                "Enter the same password again.",
                "Continue",
            ),
            super::PassphrasePurpose::AppLockUnlock => (
                "Unlock Peppy",
                "Enter the password required to open Peppy on this computer.",
                "Unlock",
            ),
            super::PassphrasePurpose::AppLockDisable => (
                "Turn off Peppy app lock",
                "Enter your current Peppy password to turn off the app lock.",
                "Continue",
            ),
        }
    }

    pub unsafe fn secure_passphrase_alert(
        purpose: super::PassphrasePurpose,
    ) -> Option<Zeroizing<String>> {
        let pool = NSAutoreleasePool::new(nil);
        let alert: id = msg_send![class!(NSAlert), new];
        let (title, message, primary) = copy(purpose);
        let _: () = msg_send![alert, setMessageText: text(title)];
        let _: () = msg_send![alert, setInformativeText: text(message)];
        let _: id = msg_send![alert, addButtonWithTitle: text(primary)];
        let _: id = msg_send![alert, addButtonWithTitle: text("Cancel")];
        let field: id = msg_send![class!(NSSecureTextField), alloc];
        let field: id = msg_send![field, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, 24.0))];
        let _: () = msg_send![alert, setAccessoryView: field];
        let window: id = msg_send![alert, window];
        let _: () = msg_send![window, setInitialFirstResponder: field];
        // The person explicitly asked to unlock, so bringing the modal forward is expected.
        let application: id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![application, activateIgnoringOtherApps: YES];
        let response: isize = msg_send![alert, runModal];
        let result = if response == FIRST_BUTTON {
            let value: id = msg_send![field, stringValue];
            let pointer: *const std::os::raw::c_char = msg_send![value, UTF8String];
            Some(Zeroizing::new(if pointer.is_null() {
                String::new()
            } else {
                CStr::from_ptr(pointer).to_string_lossy().into_owned()
            }))
        } else {
            None
        };
        let _: () = msg_send![field, setStringValue: text("")];
        let _: () = msg_send![field, release];
        let _: () = msg_send![alert, release];
        pool.drain();
        result
    }
}
