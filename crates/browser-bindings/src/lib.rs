//! Narrow, host-neutral JSON boundary for the browser's single Rust core owner.
//!
//! This crate neither fetches nor persists browser data. The SharedWorker supplies a controlled
//! filesystem and must checkpoint it before acknowledging mutating calls.
use peppy_client_core::{
    AttachmentId, Client, ClientConfig, ConversationId, DatabaseKey, DeviceId, Error as CoreError,
    MessageId, VaultId,
};
use peppy_crypto::{KeyProfile, VaultCheckHeader};
use peppy_desktop_api::{
    self as dto, GatewayView, NotificationPreferences,
    compose::{self, ComposeError, DraftInput},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, value::RawValue};
use std::{
    path::{Component, Path, PathBuf},
    str::FromStr,
    sync::{Mutex, OnceLock},
};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

mod enrollment;
mod identity_session;
pub mod local_identity;
mod origin;

mod contacts;
mod context;
mod media;
mod notifications;
mod sync;

const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
struct Request<'a> {
    command: String,
    #[serde(default, borrow)]
    args: Option<&'a RawValue>,
}

struct SecretBytes(Zeroizing<Vec<u8>>);

impl<'de> Deserialize<'de> for SecretBytes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<u8>::deserialize(deserializer).map(|value| Self(Zeroizing::new(value)))
    }
}

struct SecretString(Zeroizing<String>);

impl<'de> Deserialize<'de> for SecretString {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(|value| Self(Zeroizing::new(value)))
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Failure {
    code: &'static str,
    message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_revision: Option<String>,
}

impl Failure {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self {
            code,
            message,
            current_revision: None,
        }
    }
}

fn response(value: Result<Value, Failure>) -> String {
    match value {
        Ok(value) => json!({"ok": true, "value": value}),
        Err(error) => json!({"ok": false, "error": error}),
    }
    .to_string()
}

pub(crate) fn invalid() -> Failure {
    Failure::new("invalid-request", "The request is invalid.")
}
pub(crate) fn unavailable() -> Failure {
    Failure::new("credentials-required", "Open a local vault first.")
}
pub(crate) fn core(error: CoreError) -> Failure {
    match error {
        CoreError::WrongDatabaseKey => Failure::new(
            "database-key-mismatch",
            "The protected database key does not open the local database. Nothing was reset.",
        ),
        CoreError::WrongPassphrase | CoreError::InvalidPassphrase => Failure::new(
            "unlock-failed",
            "The passphrase did not unlock this vault. No local data was changed.",
        ),
        CoreError::IdentityMismatch => Failure::new(
            "credential-mismatch",
            "The local database belongs to a different vault or device. Nothing was reset.",
        ),
        CoreError::KeysUnavailable => Failure::new("locked", "Unlock sync before this operation."),
        CoreError::NotFound => {
            Failure::new("not-found", "The requested stored item was not found.")
        }
        CoreError::StaleDraft { current_revision } => Failure {
            code: "stale-draft",
            message: "The draft changed elsewhere.",
            current_revision: Some(current_revision.to_string()),
        },
        CoreError::InvalidMedia => Failure::new(
            "attachment-invalid",
            "The attachment could not be verified.",
        ),
        _ => Failure::new("core", "The local core rejected the operation."),
    }
}

/// A single browser-core owner. `root` is selected by the host, never by RPC input.
pub struct BrowserCore {
    root: PathBuf,
    client: Option<Client>,
    device_id: Option<DeviceId>,
    context: HostContext,
    gateways_known: bool,
    preferences: NotificationPreferences,
    trusted_origin: Option<String>,
    identity_session: Option<identity_session::IdentitySession>,
    join: Option<enrollment::JoinSession>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostContext {
    connection: Option<String>,
    origin: Option<String>,
    device_role: Option<String>,
    gateways: Option<Vec<GatewayView>>,
}

impl BrowserCore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            client: None,
            device_id: None,
            context: HostContext::default(),
            gateways_known: false,
            preferences: NotificationPreferences::default(),
            trusted_origin: None,
            identity_session: None,
            join: None,
        }
    }

    /// Dispatches one JSON request. Errors are always sanitized envelope values.
    pub fn dispatch(&mut self, request: &str) -> String {
        if request.len() > MAX_REQUEST_BYTES {
            return response(Err(Failure::new(
                "request-too-large",
                "The request is too large.",
            )));
        }
        let request: Request<'_> = match serde_json::from_str(request) {
            Ok(value) => value,
            Err(_) => return response(Err(invalid())),
        };
        response(self.call(&request.command, request.args))
    }

    fn client(&self) -> Result<&Client, Failure> {
        self.client.as_ref().ok_or_else(unavailable)
    }
    fn call(&mut self, command: &str, raw_args: Option<&RawValue>) -> Result<Value, Failure> {
        match command {
            "_worker_initialize"
            | "_worker_enroll_identity"
            | "_worker_unlock_identity"
            | "_worker_checkpoint_identity"
            | "_worker_identity_metadata"
            | "_worker_transport_token"
            | "_worker_rotate_identity"
            | "_worker_parse_credential_file"
            | "_worker_portable_identity_metadata"
            | "_worker_export_credential" => {
                self.identity_command(command, raw_args.ok_or_else(invalid)?)
            }
            "_worker_join_start"
            | "_worker_join_request"
            | "_worker_join_open_offer"
            | "_worker_join_claim"
            | "_worker_join_challenge"
            | "_worker_join_confirm"
            | "_worker_join_cancel" => self.join_command(command, raw_args.ok_or_else(invalid)?),
            #[cfg(test)]
            "open" => self.open(args_value(raw_args)?),
            "close" => {
                self.client = None;
                self.device_id = None;
                self.identity_session = None;
                self.context = HostContext::default();
                self.gateways_known = false;
                Ok(json!({}))
            }
            #[cfg(test)]
            "unlock" => self.unlock(args_value(raw_args)?),
            "_worker_apply_server_context" => self.context_command(command, args_value(raw_args)?),
            "set_host_context" => self.set_host_context(args_value(raw_args)?),
            "snapshot" => self.snapshot(args_value(raw_args)?),
            "save_draft" => self.save_draft(args_value(raw_args)?),
            "send_draft" => self.send_draft(args_value(raw_args)?),
            "mark_seen" => self.mark_seen(args_value(raw_args)?),
            _ if media::handles(command) => self.media_command(command, args_value(raw_args)?),
            _ if contacts::handles(command) => {
                self.contacts_command(command, args_value(raw_args)?)
            }
            _ if sync::handles(command) => self.sync_command(command, args_value(raw_args)?),
            _ if notifications::handles(command) => {
                self.notifications_command(command, args_value(raw_args)?)
            }
            _ => Err(unknown_command()),
        }
    }

    #[cfg(test)]
    fn open(&mut self, args: Value) -> Result<Value, Failure> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Open {
            vault_id: String,
            device_id: String,
            database_key: SecretBytes,
            origin: String,
            device_role: String,
        }
        let input: Open = serde_json::from_value(args).map_err(|_| invalid())?;
        if self.client.is_some() {
            return Err(Failure::new(
                "already-open",
                "Close the local vault before opening it again.",
            ));
        }
        let vault = VaultId(Uuid::parse_str(&input.vault_id).map_err(|_| invalid())?);
        let device = DeviceId(Uuid::parse_str(&input.device_id).map_err(|_| invalid())?);
        let origin = canonical_origin(&input.origin)?;
        if !matches!(input.device_role.as_str(), "owner" | "device" | "gateway") {
            return Err(invalid());
        }
        let key = DatabaseKey::new(&input.database_key.0).map_err(core);
        let client = Client::open(
            ClientConfig {
                database_path: self.root.join("client.db"),
                vault_id: vault,
                device_id: device,
            },
            key?,
        )
        .map_err(core)?;
        self.context.origin = Some(origin);
        self.context.device_role = Some(input.device_role);
        self.device_id = Some(device);
        self.client = Some(client);
        Ok(json!({"vaultId": input.vault_id, "deviceId": input.device_id, "locked": true}))
    }

    #[cfg(test)]
    fn unlock(&mut self, args: Value) -> Result<Value, Failure> {
        #[derive(Deserialize)]
        struct Unlock {
            profile: KeyProfile,
            header: VaultCheckHeader,
            passphrase: SecretString,
        }
        let input: Unlock = serde_json::from_value(args).map_err(|_| invalid())?;
        let result = self
            .client()?
            .unlock(&input.profile, &input.header, &input.passphrase.0)
            .map_err(core);
        result.map(|_| json!({}))
    }
    fn set_host_context(&mut self, args: Value) -> Result<Value, Failure> {
        let update: HostContext = serde_json::from_value(args).map_err(|_| invalid())?;
        if let Some(connection) = update.connection {
            if !matches!(connection.as_str(), "connected" | "offline" | "error") {
                return Err(invalid());
            }
            self.context.connection = Some(connection);
        }
        if let Some(origin) = update.origin {
            let origin = canonical_origin(&origin)?;
            if self
                .context
                .origin
                .as_deref()
                .is_some_and(|bound| bound != origin)
            {
                return Err(invalid());
            }
            self.context.origin = Some(origin);
        }
        if let Some(role) = update.device_role {
            if !matches!(role.as_str(), "owner" | "device" | "gateway")
                || self
                    .context
                    .device_role
                    .as_deref()
                    .is_some_and(|bound| bound != role)
            {
                return Err(invalid());
            }
            self.context.device_role = Some(role);
        }
        if let Some(gateways) = update.gateways {
            self.context.gateways = Some(gateways);
            self.gateways_known = true;
        }
        Ok(json!({}))
    }
    fn snapshot(&self, args: Value) -> Result<Value, Failure> {
        let requested = args
            .get("conversationId")
            .and_then(Value::as_str)
            .and_then(|id| ConversationId::from_str(id).ok());
        let client = self.client()?;
        let conversations = client.list_conversations().map_err(core)?;
        let drafts = client.compose_drafts().map_err(core)?;
        let requested = requested.filter(|id| {
            conversations.iter().any(|item| item.conversation_id == *id)
                || drafts.iter().any(|draft| draft.conversation_id == *id)
        });
        let active = requested
            .or_else(|| conversations.first().map(|c| c.conversation_id))
            .or_else(|| drafts.first().map(|d| d.conversation_id));
        let mut views = Vec::new();
        for conversation in &conversations {
            let history = client
                .messages(conversation.conversation_id)
                .map_err(core)?;
            let latest = history.last();
            let draft = drafts
                .iter()
                .find(|draft| draft.conversation_id == conversation.conversation_id);
            let name = latest
                .map(dto::counterpart)
                .or_else(|| draft.map(|draft| draft.recipients.join(", ")))
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "Conversation".into());
            let preview = latest
                .map(|message| {
                    if message.payload.body.is_empty()
                        && !message.payload.record.attachments.is_empty()
                    {
                        "Attachment".into()
                    } else {
                        message.payload.body.clone()
                    }
                })
                .unwrap_or_default();
            let messages = if Some(conversation.conversation_id) == active {
                history
                    .iter()
                    .skip(history.len().saturating_sub(500))
                    .map(|message| {
                        let attachments = message
                            .payload
                            .record
                            .attachments
                            .iter()
                            .filter_map(|id| {
                                client
                                    .attachment_info(id.attachment_id)
                                    .ok()
                                    .map(|info| dto::attachment_view(&info, None, None))
                            })
                            .collect();
                        dto::message_view(message, attachments)
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let reply_context = client.mms_reply_context(conversation.conversation_id).ok();
            views.push(dto::ConversationView {
                id: conversation.conversation_id.to_string(),
                name,
                preview,
                unread: conversation.unread_count,
                messages,
                participants: reply_context
                    .as_ref()
                    .map(|context| context.recipients.clone()),
                reply_blocked_reason: reply_context.and_then(|context| context.blocked_reason),
            });
        }
        for draft in drafts.iter().filter(|draft| {
            !conversations
                .iter()
                .any(|conversation| conversation.conversation_id == draft.conversation_id)
        }) {
            views.push(dto::ConversationView {
                id: draft.conversation_id.to_string(),
                name: if draft.recipients.is_empty() {
                    "New message".into()
                } else {
                    draft.recipients.join(", ")
                },
                preview: if draft.text.is_empty() {
                    "Draft".into()
                } else {
                    format!("Draft: {}", draft.text.chars().take(80).collect::<String>())
                },
                unread: 0,
                messages: Vec::new(),
                participants: None,
                reply_blocked_reason: None,
            });
        }
        let keys = client.key_status().map_err(core)?;
        let unlocked = keys
            .active_epoch
            .is_some_and(|epoch| keys.unlocked_epochs.contains(&epoch));
        let notices = client.notification_snapshot().map_err(core)?;
        serde_json::to_value(dto::Snapshot {
            version: "1",
            mode: "browser",
            connection: dto::Connection {
                state: match self.context.connection.as_deref() {
                    Some("connected") => "connected",
                    Some("error") => "error",
                    _ => "offline",
                },
                origin: self.context.origin.clone(),
                error_code: None,
            },
            encryption: dto::Encryption {
                state: if unlocked { "unlocked" } else { "locked" },
                profile_fingerprint: None,
            },
            gateways: self.context.gateways.clone().unwrap_or_default(),
            conversations: views,
            notifications: notices.notifications,
            app_filters: notices.app_filters,
            notification_preferences: self.preferences.clone(),
            active_conversation_id: active.map(|id| id.to_string()),
            draft: active
                .and_then(|id| drafts.iter().find(|d| d.conversation_id == id))
                .map(dto::draft_view),
            head: dto::Head {
                enabled: false,
                capability: "unsupported",
                note: Some("Browser window controls are unavailable.".into()),
                pinned_conversation_ids: None,
                panel: None,
            },
            desktop: Some(dto::Desktop {
                tray_available: false,
                start_at_login: false,
                startup_supported: false,
                background: false,
            }),
            device_role: self.context.device_role.clone(),
            pending_count: client.pending_outbox_batch(1000).map_err(core)?.len() as u64,
            quarantine_count: client.quarantined().map_err(core)?.len() as u64,
            contact_resolution: None,
            contact_books: None,
            contacts_pending_count: None,
            contact_sync: None,
            credential_export_available: Some(unlocked && self.identity_session.is_some()),
        })
        .map_err(|_| core(CoreError::Database))
    }
    fn save_draft(&self, args: Value) -> Result<Value, Failure> {
        let input: DraftInput = serde_json::from_value(args).map_err(|_| invalid())?;
        parse_revision(&input.expected_revision)?;
        serde_json::to_value(compose::save_draft(self.client()?, &input).map_err(compose_failure)?)
            .map_err(|_| core(CoreError::Database))
    }
    fn send_draft(&self, args: Value) -> Result<Value, Failure> {
        let input: DraftInput = serde_json::from_value(args).map_err(|_| invalid())?;
        parse_revision(&input.expected_revision)?;
        let gateways = self
            .gateways_known
            .then(|| self.context.gateways.as_deref().unwrap_or(&[]));
        serde_json::to_value(
            compose::send_draft(self.client()?, &input, gateways).map_err(compose_failure)?,
        )
        .map_err(|_| core(CoreError::Database))
    }
    fn mark_seen(&self, args: Value) -> Result<Value, Failure> {
        for id in args
            .get("visibleMessageIds")
            .and_then(Value::as_array)
            .ok_or_else(invalid)?
        {
            self.client()?
                .mark_seen(
                    MessageId::from_str(id.as_str().ok_or_else(invalid)?).map_err(|_| invalid())?,
                )
                .map_err(core)?;
        }
        Ok(json!({}))
    }
    pub(crate) fn safe_worker_file(&self, name: &str) -> Result<PathBuf, Failure> {
        let path = Path::new(name);
        if path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(invalid());
        }
        let path = self.root.join("worker-input").join(path);
        if !path.is_file() {
            return Err(Failure::new(
                "attachment-local",
                "The selected file could not be read.",
            ));
        }
        Ok(path)
    }
}
fn args_value(raw: Option<&RawValue>) -> Result<Value, Failure> {
    raw.map_or(Ok(Value::Null), |raw| {
        serde_json::from_str(raw.get()).map_err(|_| invalid())
    })
}
pub(crate) fn attachment_id(args: &Value) -> Result<AttachmentId, Failure> {
    AttachmentId::from_str(
        args.get("id")
            .or_else(|| args.get("attachmentId"))
            .and_then(Value::as_str)
            .ok_or_else(invalid)?,
    )
    .map_err(|_| invalid())
}

fn canonical_origin(input: &str) -> Result<String, Failure> {
    origin::canonical_origin(input).map_err(|_| invalid())
}

fn compose_failure(error: ComposeError) -> Failure {
    match error {
        ComposeError::Ui { code, message } => Failure::new(code, message),
        ComposeError::ReplyBlocked(_) => {
            Failure::new("mms-reply-blocked", "Replying to this MMS is unavailable.")
        }
        ComposeError::Core(error) => core(error),
        ComposeError::AfterRoute {
            source,
            current_revision,
        } => {
            let mut failure = core(source);
            failure.current_revision = Some(current_revision.to_string());
            failure
        }
    }
}

fn parse_revision(input: &str) -> Result<u64, Failure> {
    if input.is_empty()
        || (input.len() > 1 && input.starts_with('0'))
        || !input.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid());
    }
    input.parse().map_err(|_| invalid())
}

fn unknown_command() -> Failure {
    Failure::new(
        "unknown-command",
        "This command is not available from the browser core.",
    )
}

static CABI_CORE: OnceLock<Mutex<BrowserCore>> = OnceLock::new();

fn cabi_error() -> String {
    response(Err(invalid()))
}

fn poisoned_core() -> String {
    response(Err(Failure::new(
        "core-poisoned",
        "The local core must be reopened from its last checkpoint.",
    )))
}

fn dispatch_to_owner(owner: &Mutex<BrowserCore>, request: &str) -> String {
    owner
        .lock()
        .map_or_else(|_| poisoned_core(), |mut core| core.dispatch(request))
}

/// Allocates a request buffer; release it with `peppy_browser_free_request` using this length.
#[unsafe(no_mangle)]
pub extern "C" fn peppy_browser_alloc(length: usize) -> *mut u8 {
    if length > MAX_REQUEST_BYTES {
        return std::ptr::null_mut();
    }
    let mut bytes = vec![0; length];
    let pointer = bytes.as_mut_ptr();
    std::mem::forget(bytes);
    pointer
}

/// Dispatches one request and returns an owned NUL-terminated UTF-8 response.
///
/// # Safety
/// `request` must point to a readable allocation containing exactly `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn peppy_browser_dispatch(request: *const u8, length: usize) -> *mut u8 {
    let result = std::panic::catch_unwind(|| {
        if request.is_null() || length > MAX_REQUEST_BYTES {
            return cabi_error();
        }
        // SAFETY: the C ABI requires a readable `length`-byte request buffer.
        let bytes = unsafe { std::slice::from_raw_parts(request, length) };
        let request = std::str::from_utf8(bytes).unwrap_or("");
        let owner = CABI_CORE.get_or_init(|| Mutex::new(BrowserCore::new(PathBuf::from("/peppy"))));
        dispatch_to_owner(owner, request)
    })
    .unwrap_or_else(|_| poisoned_core());
    std::ffi::CString::new(result)
        .map_or(std::ptr::null_mut(), std::ffi::CString::into_raw)
        .cast()
}

/// Zeroizes and releases a request buffer returned by `peppy_browser_alloc`.
///
/// # Safety
/// `request` must be returned by `peppy_browser_alloc` and `length` must be the allocated size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn peppy_browser_free_request(request: *mut u8, length: usize) {
    if request.is_null() || length > MAX_REQUEST_BYTES {
        return;
    }
    // SAFETY: allocation and capacity are established by `peppy_browser_alloc`.
    let mut bytes = unsafe { Vec::from_raw_parts(request, length, length) };
    bytes.zeroize();
}

/// Releases a response returned by `peppy_browser_dispatch`.
///
/// # Safety
/// `response` must be a pointer returned by `peppy_browser_dispatch` and not previously freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn peppy_browser_free_response(response: *mut u8) {
    if !response.is_null() {
        // SAFETY: this accepts only the pointer returned by `CString::into_raw`.
        let mut bytes =
            unsafe { std::ffi::CString::from_raw(response.cast()) }.into_bytes_with_nul();
        bytes.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_client_core::DraftId;
    use peppy_crypto::{create_vault_check_header, derive_root_key};

    fn value(response: &str) -> Value {
        let response: Value = serde_json::from_str(response).unwrap();
        assert_eq!(response["ok"], true, "{response}");
        response["value"].clone()
    }

    fn open(core: &mut BrowserCore, vault: VaultId, device: DeviceId, key: [u8; 32]) {
        value(&core.dispatch(&json!({"command":"open","args":{"vaultId":vault.to_string(),"deviceId":device.to_string(),"databaseKey":key,"origin":"https://peppy.test/","deviceRole":"device"}}).to_string()));
    }

    #[test]
    fn malformed_and_unknown_requests_are_sanitized() {
        let mut core = BrowserCore::new(tempfile::tempdir().unwrap().path().to_path_buf());
        assert!(core.dispatch("not json").contains("invalid-request"));
        assert!(
            core.dispatch(r#"{"command":"pairing"}"#)
                .contains("unknown-command")
        );
    }

    #[test]
    fn ffi_round_trip_returns_a_nul_terminated_error() {
        let request = br#"{"command":"not-supported","args":{}}"#;
        let pointer = peppy_browser_alloc(request.len());
        assert!(!pointer.is_null());
        // SAFETY: `pointer` is an allocation of `request.len()` bytes from this module.
        unsafe { std::ptr::copy_nonoverlapping(request.as_ptr(), pointer, request.len()) };
        let response = unsafe { peppy_browser_dispatch(pointer, request.len()) };
        assert!(!response.is_null());
        // SAFETY: response is a NUL-terminated C string returned by this module.
        let text = unsafe { std::ffi::CStr::from_ptr(response.cast()) }
            .to_str()
            .unwrap();
        assert!(text.contains("unknown-command"));
        // SAFETY: these pointers are returned by their corresponding allocation functions.
        unsafe { peppy_browser_free_request(pointer, request.len()) };
        // SAFETY: response is returned by `peppy_browser_dispatch`.
        unsafe { peppy_browser_free_response(response) };
    }

    #[test]
    fn a_poisoned_owner_cannot_be_mistaken_for_a_handled_request_error() {
        let root = tempfile::tempdir().unwrap();
        let owner = Mutex::new(BrowserCore::new(root.path().to_owned()));
        let _ = std::panic::catch_unwind(|| {
            let _guard = owner.lock().unwrap();
            panic!("fixture owner trap");
        });
        let result: Value =
            serde_json::from_str(&dispatch_to_owner(&owner, r#"{"command":"snapshot"}"#)).unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"]["code"], "core-poisoned");
        assert!(!result.to_string().contains("fixture owner trap"));
    }

    #[test]
    fn real_sqlcipher_open_unlock_send_and_wrong_key_refusal() {
        let root = tempfile::tempdir().unwrap();
        let vault = VaultId::new();
        let device = DeviceId::new();
        let mut core = BrowserCore::new(root.path().to_path_buf());
        assert!(core.dispatch(&json!({"command":"open","args":{"vaultId":vault,"deviceId":device,"databaseKey":vec![7; 32],"origin":"https://user:secret@peppy.test/","deviceRole":"device"}}).to_string()).contains("invalid-request"));
        open(&mut core, vault, device, [7; 32]);
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("passphrase", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        value(&core.dispatch(&json!({"command":"unlock","args":{"profile":profile,"header":header,"passphrase":"passphrase"}}).to_string()));
        value(&core.dispatch(&json!({"command":"set_host_context","args":{"connection":"connected","origin":"https://peppy.test/","deviceRole":"device","gateways":[{"id":device,"name":"Gateway","simId":"sim-1","online":true,"simulated":true,"supportsSms":true,"supportsMms":true}]}}).to_string()));
        let draft = value(&core.dispatch(&json!({"command":"save_draft","args":{"id":"new","conversationId":"","text":"hello","recipientIds":["+15555550100"],"attachmentIds":[],"gatewayId":device,"simId":"sim-1","expectedRevision":"0"}}).to_string()));
        let sent = value(&core.dispatch(&json!({"command":"send_draft","args":{"id":draft["id"],"conversationId":draft["conversationId"],"text":draft["text"],"recipientIds":draft["recipientIds"],"attachmentIds":draft["attachmentIds"],"gatewayId":device,"simId":"sim-1","expectedRevision":draft["revision"]}}).to_string()));
        assert_eq!(sent["status"], "queued-local");
        assert_eq!(core.client().unwrap().pending_outbox().unwrap().len(), 1);
        value(&core.dispatch(r#"{"command":"close"}"#));
        assert!(core.dispatch(&json!({"command":"open","args":{"vaultId":vault,"deviceId":device,"databaseKey":vec![8; 32],"origin":"https://peppy.test/","deviceRole":"device"}}).to_string()).contains("database-key-mismatch"));
    }

    #[test]
    fn draft_snapshot_and_cas_match_the_existing_ui_contract() {
        let root = tempfile::tempdir().unwrap();
        let mut core = BrowserCore::new(root.path().to_owned());
        let device = DeviceId::new();
        open(&mut core, VaultId::new(), device, [7; 32]);
        let draft = value(&core.dispatch(&json!({"command":"save_draft","args":{"id":"new","conversationId":"","text":"keep this draft","recipientIds":["+15555550100"],"attachmentIds":[],"gatewayId":device,"simId":"sim-1","expectedRevision":"0"}}).to_string()));
        let snapshot = value(&core.dispatch(r#"{"command":"snapshot"}"#));
        assert_eq!(snapshot["conversations"][0]["id"], draft["conversationId"]);
        assert_eq!(snapshot["conversations"][0]["name"], "+15555550100");
        assert_eq!(
            snapshot["conversations"][0]["preview"],
            "Draft: keep this draft"
        );
        let update = json!({"id":draft["id"],"conversationId":draft["conversationId"],"text":"edited","recipientIds":["+15555550100"],"attachmentIds":[],"expectedRevision":draft["revision"]});
        let saved =
            value(&core.dispatch(&json!({"command":"save_draft","args":update}).to_string()));
        assert_eq!(saved["gatewayId"], device.to_string());
        assert_eq!(saved["simId"], "sim-1");
        let stale: Value = serde_json::from_str(
            &core.dispatch(&json!({"command":"save_draft","args":update}).to_string()),
        )
        .unwrap();
        assert_eq!(stale["error"]["code"], "stale-draft");
        assert_eq!(stale["error"]["currentRevision"], saved["revision"]);
        let mut wrong_conversation = update;
        wrong_conversation["expectedRevision"] = saved["revision"].clone();
        wrong_conversation["conversationId"] = json!(ConversationId::new());
        let rejected: Value = serde_json::from_str(
            &core.dispatch(&json!({"command":"save_draft","args":wrong_conversation}).to_string()),
        )
        .unwrap();
        assert_eq!(rejected["ok"], false);
        assert_eq!(
            core.client()
                .unwrap()
                .compose_draft(DraftId::from_str(saved["id"].as_str().unwrap()).unwrap())
                .unwrap()
                .unwrap()
                .revision
                .to_string(),
            saved["revision"].as_str().unwrap()
        );
    }
}
