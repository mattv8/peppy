//! Narrow, host-neutral JSON boundary for the browser's single Rust core owner.
//!
//! This crate neither fetches nor persists browser data. The SharedWorker supplies a controlled
//! filesystem and must checkpoint it before acknowledging mutating calls.
use peppy_client_core::{
    AttachmentId, Client, ClientConfig, ConversationId, DatabaseKey, DeviceId, Error as CoreError,
    Message, MessageId, VaultId,
};
use peppy_crypto::{KeyProfile, VaultCheckHeader};
use peppy_desktop_api::{
    self as dto, GatewayView, NotificationPreferences,
    compose::{self, ComposeError, DiscardDraftInput, DraftInput},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, value::RawValue};
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    str::FromStr,
    sync::{Mutex, OnceLock},
};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

mod identity_session;
pub mod local_identity;
mod newenrollment;
mod origin;

mod contacts;
mod context;
mod conversation;
mod media;
mod notifications;
mod search_history;
mod sync;

const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const HISTORY_PAGE_SIZE: usize = 50;

#[derive(Deserialize)]
struct Request<'a> {
    command: String,
    #[serde(default, borrow)]
    args: Option<&'a RawValue>,
}

#[cfg(test)]
struct SecretBytes(Zeroizing<Vec<u8>>);

#[cfg(test)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    draft_id: Option<String>,
}

impl Failure {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self {
            code,
            message,
            current_revision: None,
            conversation_id: None,
            draft_id: None,
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
            conversation_id: None,
            draft_id: None,
        },
        CoreError::DraftMoved {
            draft_id,
            conversation_id,
            current_revision,
        } => Failure {
            code: "draft-moved",
            message: "The draft moved to another conversation.",
            current_revision: Some(current_revision.to_string()),
            conversation_id: Some(conversation_id.to_string()),
            draft_id: Some(draft_id.to_string()),
        },
        CoreError::DraftNotDiscardable => Failure::new(
            "draft-not-discardable",
            "Only an empty recipientless draft can be deleted.",
        ),
        CoreError::RecipientlessDraftExists => Failure::new(
            "recipientless-draft-exists",
            "A recipientless draft already exists.",
        ),
        CoreError::DraftMergeOverflow => Failure::new(
            "draft-merge-overflow",
            "Combining these drafts would exceed a compose limit.",
        ),
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
    enrollment: Option<peppy_hosted_client::newenrollment::EnrollmentClient>,
    hosted_provisioning: Option<peppy_hosted_client::HostedProvisioning>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostContext {
    connection: Option<String>,
    origin: Option<String>,
    device_role: Option<String>,
    gateways: Option<Vec<GatewayView>>,
    #[serde(skip)]
    revoked_device_ids: std::collections::HashSet<String>,
}

fn connection_snapshot(context: &HostContext) -> (&'static str, Option<&'static str>) {
    match context.connection.as_deref() {
        Some("connected") => ("connected", None),
        Some("offline") => ("offline", Some("offline")),
        Some("revoked") => ("error", Some("revoked")),
        Some("key-mismatch") => ("error", Some("key-mismatch")),
        Some("error") => ("error", None),
        _ => ("offline", Some("connecting")),
    }
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
            enrollment: None,
            hosted_provisioning: None,
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
            | "_worker_rotate_identity" => {
                self.identity_command(command, raw_args.ok_or_else(invalid)?)
            }
            "_worker_enrollment_generate_secret"
            | "_worker_enrollment_gateway_presence"
            | "_worker_enrollment_new"
            | "_worker_enrollment_new_owner_from_identity"
            | "_worker_enrollment_checkpoint"
            | "_worker_enrollment_create_owner"
            | "_worker_enrollment_parse_vault_invite_link"
            | "_worker_enrollment_begin_vault_request"
            | "_worker_enrollment_accept_vault_request_created"
            | "_worker_enrollment_vault_request_header"
            | "_worker_enrollment_parse_vault_request_status"
            | "_worker_enrollment_create_vault"
            | "_worker_enrollment_clear_vault_request"
            | "_worker_enrollment_pending_vault_request"
            | "_worker_enrollment_manual_challenge"
            | "_worker_enrollment_complete_manual_challenge"
            | "_worker_enrollment_finish_owner"
            | "_worker_enrollment_finish_manual"
            | "_worker_enrollment_create_phone_invitation"
            | "_worker_enrollment_consume_created_phone_invitation"
            | "_worker_enrollment_get_phone_qr"
            | "_worker_enrollment_claim_phone_invitation"
            | "_worker_enrollment_consume_phone_claim"
            | "_worker_enrollment_phone_status"
            | "_worker_enrollment_approve_phone_invitation"
            | "_worker_enrollment_confirm_phone_invitation" => {
                self.new_enrollment_command(command, raw_args.ok_or_else(invalid)?)
            }
            "_worker_enrollment_prepare_hosted_owner"
            | "_worker_enrollment_accept_hosted_grant"
            | "_worker_enrollment_finish_hosted_owner" => {
                self.new_enrollment_command(command, raw_args.ok_or_else(invalid)?)
            }
            #[cfg(test)]
            "open" => self.open(args_value(raw_args)?),
            "close" => {
                self.client = None;
                self.device_id = None;
                self.identity_session = None;
                self.enrollment = None;
                self.hosted_provisioning = None;
                self.context = HostContext::default();
                self.gateways_known = false;
                Ok(json!({}))
            }
            #[cfg(test)]
            "unlock" => self.unlock(args_value(raw_args)?),
            "_worker_apply_server_context" => self.context_command(command, args_value(raw_args)?),
            "set_host_context" => self.set_host_context(args_value(raw_args)?),
            "snapshot" => self.snapshot(args_value(raw_args)?),
            "load_history" => self.load_history(args_value(raw_args)?),
            "search_local" => self.search_local(args_value(raw_args)?),
            "advance_search_index" => self.advance_search_index(args_value(raw_args)?),
            "load_message_window" => self.load_message_window(args_value(raw_args)?),
            "save_draft" => self.save_draft(args_value(raw_args)?),
            "discard_draft" => self.discard_draft(args_value(raw_args)?),
            "send_draft" => self.send_draft(args_value(raw_args)?),
            "mark_seen" => self.mark_seen(args_value(raw_args)?),
            "unread_message_batches" => self.unread_message_batches(args_value(raw_args)?),
            "mark_read_batch" => self.mark_read_batch(args_value(raw_args)?),
            _ if search_history::handles(command) => {
                self.search_history_command(command, args_value(raw_args)?)
            }
            _ if conversation::handles(command) => {
                self.conversation_command(command, args_value(raw_args)?)
            }
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
            if !matches!(
                connection.as_str(),
                "connected" | "offline" | "revoked" | "key-mismatch" | "error"
            ) {
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
        let metadata_only = match args.get("metadataOnly") {
            Some(Value::Bool(value)) => *value,
            Some(_) => return Err(invalid()),
            None => false,
        };
        let requested = args
            .get("conversationId")
            .and_then(Value::as_str)
            .and_then(|id| ConversationId::from_str(id).ok());
        let client = self.client()?;
        let conversations = client.list_conversations().map_err(core)?;
        let drafts = client.compose_drafts().map_err(core)?;
        let drafts_by_conversation: HashMap<ConversationId, _> = drafts
            .iter()
            .map(|draft| (draft.conversation_id, draft))
            .collect();
        let requested = requested.filter(|id| {
            conversations.iter().any(|item| item.conversation_id == *id)
                || drafts.iter().any(|draft| draft.conversation_id == *id)
        });
        let history_from_message_id = if requested.is_some() {
            args.get("historyFromMessageId")
                .and_then(Value::as_str)
                .map(|id| MessageId::from_str(id).map_err(|_| invalid()))
                .transpose()?
        } else {
            None
        };
        let active = requested
            .or_else(|| conversations.first().map(|c| c.conversation_id))
            .or_else(|| drafts.first().map(|d| d.conversation_id));
        let mut views = Vec::new();
        for conversation in &conversations {
            let latest = client
                .latest_message(conversation.conversation_id)
                .map_err(core)?;
            let draft = drafts_by_conversation
                .get(&conversation.conversation_id)
                .copied();
            let draft_only = draft
                .map(|draft| client.compose_draft_is_new(draft.draft_id))
                .transpose()
                .map_err(core)?
                .unwrap_or(false);
            let name = latest
                .as_ref()
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
            let (messages, has_older_messages) =
                if !metadata_only && Some(conversation.conversation_id) == active {
                    let (history, has_more) = match history_from_message_id {
                        Some(anchor) => {
                            let has_more = !client
                                .messages_page(conversation.conversation_id, Some(anchor), 1)
                                .map_err(core)?
                                .messages
                                .is_empty();
                            client
                                .messages_since(conversation.conversation_id, anchor)
                                .map(|messages| (messages, has_more))
                        }
                        None => client
                            .messages_page(conversation.conversation_id, None, HISTORY_PAGE_SIZE)
                            .map(|page| (page.messages, page.has_more)),
                    }
                    .map_err(core)?;
                    let messages = history
                        .iter()
                        .map(|message| message_view(client, message))
                        .collect();
                    (messages, Some(has_more))
                } else {
                    (Vec::new(), None)
                };
            let reply_context = client.mms_reply_context(conversation.conversation_id).ok();
            views.push(dto::ConversationView {
                id: conversation.conversation_id.to_string(),
                name,
                preview,
                unread: conversation.unread_count,
                messages,
                has_older_messages,
                participants: reply_context
                    .as_ref()
                    .map(|context| context.recipients.clone()),
                recipientless_draft: false,
                draft_only,
                pinned: conversation.pinned,
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
                has_older_messages: None,
                participants: None,
                // Old recipientless compose rows predate the persisted intent bit. A row that
                // has no history is still a draft-only row for presentation purposes.
                recipientless_draft: draft.recipients.is_empty(),
                draft_only: true,
                pinned: false,
            });
        }
        let keys = client.key_status().map_err(core)?;
        let unlocked = keys
            .active_epoch
            .is_some_and(|epoch| keys.unlocked_epochs.contains(&epoch));
        let notices = client.notification_snapshot().map_err(core)?;
        let (connection_state, connection_error_code) = connection_snapshot(&self.context);
        serde_json::to_value(dto::Snapshot {
            version: "1",
            mode: "browser",
            connection: dto::Connection {
                state: connection_state,
                origin: self.context.origin.clone(),
                error_code: connection_error_code,
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
                .and_then(|id| drafts_by_conversation.get(&id).copied())
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
            tone_overrides: unlocked
                .then(|| client.conversation_tones().map_err(core))
                .transpose()?,
        })
        .map_err(|_| core(CoreError::Database))
    }
    fn load_history(&self, args: Value) -> Result<Value, Failure> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct HistoryRequest {
            conversation_id: String,
            before_message_id: String,
        }
        let request: HistoryRequest = serde_json::from_value(args).map_err(|_| invalid())?;
        let conversation =
            ConversationId::from_str(&request.conversation_id).map_err(|_| invalid())?;
        let before = MessageId::from_str(&request.before_message_id).map_err(|_| invalid())?;
        let page = self
            .client()?
            .messages_page(conversation, Some(before), HISTORY_PAGE_SIZE)
            .map_err(core)?;
        let client = self.client()?;
        let messages = page
            .messages
            .iter()
            .map(|message| message_view(client, message))
            .collect();
        serde_json::to_value(dto::HistoryPage {
            conversation_id: conversation.to_string(),
            messages,
            has_more: page.has_more,
        })
        .map_err(|_| core(CoreError::Database))
    }
    fn search_local(&self, args: Value) -> Result<Value, Failure> {
        let input: dto::LocalSearchRequestView =
            serde_json::from_value(args).map_err(|_| invalid())?;
        let page = self.client()?.search_local(input.into()).map_err(core)?;
        serde_json::to_value(dto::local_search_page_view(page))
            .map_err(|_| core(CoreError::Database))
    }
    fn advance_search_index(&self, args: Value) -> Result<Value, Failure> {
        let input: dto::AdvanceSearchIndexInputView =
            serde_json::from_value(args).map_err(|_| invalid())?;
        let progress = self
            .client()?
            .advance_search_index(input.budget_rows)
            .map_err(core)?;
        serde_json::to_value(dto::search_index_progress_view(progress))
            .map_err(|_| core(CoreError::Database))
    }
    fn load_message_window(&self, args: Value) -> Result<Value, Failure> {
        let input: dto::MessageWindowRequestView =
            serde_json::from_value(args).map_err(|_| invalid())?;
        let client = self.client()?;
        let window = client.load_message_window(input.into()).map_err(core)?;
        serde_json::to_value(dto::message_window_view(window, |message| {
            message_view(client, &message)
        }))
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
    fn discard_draft(&self, args: Value) -> Result<Value, Failure> {
        let input: DiscardDraftInput = serde_json::from_value(args).map_err(|_| invalid())?;
        parse_revision(&input.expected_revision)?;
        compose::discard_draft(self.client()?, &input).map_err(compose_failure)?;
        Ok(json!({}))
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
    fn unread_message_batches(&self, args: Value) -> Result<Value, Failure> {
        no_args(args)?;
        self.client()?
            .unread_message_batches()
            .map(|batches| {
                json!(
                    batches
                        .into_iter()
                        .map(|batch| batch
                            .into_iter()
                            .map(|id| id.to_string())
                            .collect::<Vec<_>>())
                        .collect::<Vec<_>>()
                )
            })
            .map_err(core)
    }
    fn mark_read_batch(&self, args: Value) -> Result<Value, Failure> {
        self.client()?
            .mark_read_batch(&message_ids(args)?)
            .map(|count| json!(count.to_string()))
            .map_err(core)
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

fn message_view(client: &Client, message: &Message) -> dto::MessageView {
    let attachments = message
        .payload
        .record
        .attachments
        .iter()
        .filter_map(|reference| {
            client
                .attachment_info(reference.attachment_id)
                .ok()
                .map(|info| {
                    let mut view = dto::snapshot_attachment_view(
                        &info,
                        None,
                        client.deferred_media_status(info.attachment_id),
                    );
                    view.media_meta = dto::message_attachment_media_metadata_view(
                        client,
                        message.payload.record.message_id,
                        info.attachment_id,
                    )
                    .ok()
                    .flatten()
                    .map(Box::new);
                    view
                })
        })
        .collect();
    dto::message_view(message, attachments)
}
fn args_value(raw: Option<&RawValue>) -> Result<Value, Failure> {
    raw.map_or(Ok(Value::Null), |raw| {
        serde_json::from_str(raw.get()).map_err(|_| invalid())
    })
}
fn no_args(args: Value) -> Result<(), Failure> {
    match args {
        Value::Null => Ok(()),
        Value::Object(values) if values.is_empty() => Ok(()),
        _ => Err(invalid()),
    }
}
fn message_ids(args: Value) -> Result<Vec<MessageId>, Failure> {
    args.get("messageIds")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?
        .iter()
        .map(|id| MessageId::from_str(id.as_str().ok_or_else(invalid)?).map_err(|_| invalid()))
        .collect()
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
    use peppy_client_core::{ComposeDraftUpdate, DraftId, IncomingSms};
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
    fn unread_batches_and_read_batch_dispatch_through_the_bounded_core_api() {
        let root = tempfile::tempdir().unwrap();
        let vault = VaultId::new();
        let device = DeviceId::new();
        let mut core = BrowserCore::new(root.path().to_path_buf());
        open(&mut core, vault, device, [7; 32]);
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("passphrase", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        value(&core.dispatch(
            &json!({"command":"unlock","args":{"profile":profile,"header":header,"passphrase":"passphrase"}}).to_string(),
        ));
        let message = core
            .client()
            .unwrap()
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: "+15555550101".into(),
                body: "unread".into(),
                provider_message_id: Some("mark-all-read".into()),
                imported: false,
            })
            .unwrap();

        assert_eq!(
            value(&core.dispatch(r#"{"command":"unread_message_batches","args":{}}"#)),
            json!([[message.message_id.to_string()]])
        );
        assert_eq!(
            value(&core.dispatch(
                &json!({"command":"mark_read_batch","args":{"messageIds":[message.message_id.to_string()]}}).to_string(),
            )),
            json!("1")
        );
        assert_eq!(
            value(&core.dispatch(
                &json!({"command":"mark_read_batch","args":{"messageIds":[message.message_id.to_string()]}}).to_string(),
            )),
            json!("0")
        );
        let rejected: Value = serde_json::from_str(
            &core.dispatch(r#"{"command":"unread_message_batches","args":{"unexpected":true}}"#),
        )
        .unwrap();
        assert_eq!(rejected["ok"], false);
        assert_eq!(rejected["error"]["code"], "invalid-request");
        let invalid_id: Value = serde_json::from_str(
            &core.dispatch(r#"{"command":"mark_read_batch","args":{"messageIds":["not-an-id"]}}"#),
        )
        .unwrap();
        assert_eq!(invalid_id["ok"], false);
        assert_eq!(invalid_id["error"]["code"], "invalid-request");
        let oversized = (0..1001)
            .map(|_| MessageId::new().to_string())
            .collect::<Vec<_>>();
        let oversized: Value = serde_json::from_str(&core.dispatch(
            &json!({"command":"mark_read_batch","args":{"messageIds":oversized}}).to_string(),
        ))
        .unwrap();
        assert_eq!(oversized["ok"], false);
        assert_eq!(oversized["error"]["code"], "core");
    }

    #[test]
    fn search_and_message_window_commands_are_typed_read_only_routes_and_locked_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let vault = VaultId::new();
        let device = DeviceId::new();
        let mut core = BrowserCore::new(root.path().to_path_buf());
        open(&mut core, vault, device, [7; 32]);

        for request in [
            json!({"command":"search_local","args":{"query":"hello"}}),
            json!({"command":"load_message_window","args":{"direction":"refresh","conversationId":ConversationId::new(),"messageIds":[]}}),
        ] {
            let response: Value =
                serde_json::from_str(&core.dispatch(&request.to_string())).unwrap();
            assert_eq!(response["error"]["code"], "locked");
        }
        let malformed: Value = serde_json::from_str(
            &core.dispatch(r#"{"command":"advance_search_index","args":{"budgetRows":"100"}}"#),
        )
        .unwrap();
        assert_eq!(malformed["error"]["code"], "invalid-request");
    }

    #[test]
    fn sqlcipher_search_and_refresh_window_return_canonical_views() {
        let root = tempfile::tempdir().unwrap();
        let vault = VaultId::new();
        let device = DeviceId::new();
        let mut core = BrowserCore::new(root.path().to_path_buf());
        open(&mut core, vault, device, [7; 32]);
        let profile = KeyProfile::new(vault.0, 1).unwrap();
        let header = create_vault_check_header(
            &derive_root_key("passphrase", &profile).unwrap(),
            profile.clone(),
        )
        .unwrap();
        value(&core.dispatch(&json!({"command":"unlock","args":{"profile":profile,"header":header,"passphrase":"passphrase"}}).to_string()));
        let message = core
            .client()
            .unwrap()
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: "+15555550101".into(),
                body: "needle message".into(),
                provider_message_id: Some("search-window".into()),
                imported: false,
            })
            .unwrap();

        let search = value(&core.dispatch(
            &json!({"command":"search_local","args":{"query":"needle","kind":"message","limit":1}}).to_string(),
        ));
        assert_eq!(search["items"][0]["kind"], "message");
        assert_eq!(
            search["items"][0]["messageId"],
            message.message_id.to_string()
        );
        let window = value(&core.dispatch(
            &json!({"command":"load_message_window","args":{"direction":"refresh","conversationId":message.conversation_id,"messageIds":[message.message_id]}}).to_string(),
        ));
        assert_eq!(
            window["conversationId"],
            message.conversation_id.to_string()
        );
        assert_eq!(window["messages"].as_array().unwrap().len(), 1);

        let full = value(
            &core.dispatch(
                &json!({"command":"snapshot","args":{"conversationId":message.conversation_id}})
                    .to_string(),
            ),
        );
        let metadata = value(&core.dispatch(
            &json!({"command":"snapshot","args":{"conversationId":message.conversation_id,"metadataOnly":true}})
                .to_string(),
        ));
        let full_conversation = full["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|conversation| conversation["id"] == message.conversation_id.to_string())
            .unwrap();
        let metadata_conversation = metadata["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|conversation| conversation["id"] == message.conversation_id.to_string())
            .unwrap();
        for field in ["name", "preview", "participants"] {
            assert_eq!(metadata_conversation[field], full_conversation[field]);
        }
        assert!(
            metadata_conversation["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let invalid_metadata: Value = serde_json::from_str(
            &core.dispatch(r#"{"command":"snapshot","args":{"metadataOnly":"true"}}"#),
        )
        .unwrap();
        assert_eq!(invalid_metadata["error"]["code"], "invalid-request");
    }

    #[test]
    fn host_context_accepts_all_scheduler_connection_classifications() {
        let mut core = BrowserCore::new(tempfile::tempdir().unwrap().path().to_path_buf());
        for connection in ["connected", "offline", "revoked", "key-mismatch", "error"] {
            value(&core.dispatch(
                &json!({"command":"set_host_context","args":{"connection":connection}}).to_string(),
            ));
        }
    }

    #[test]
    fn generated_enrollment_secret_is_private_and_has_six_words() {
        let mut core = BrowserCore::new(tempfile::tempdir().unwrap().path().to_path_buf());
        let public: Value = serde_json::from_str(
            &core.dispatch(r#"{"command":"enrollment_generate_secret","args":{}}"#),
        )
        .unwrap();
        assert_eq!(public["ok"], false);
        assert_eq!(public["error"]["code"], "unknown-command");

        let private: Value = serde_json::from_str(
            &core.dispatch(r#"{"command":"_worker_enrollment_generate_secret","args":{}}"#),
        )
        .unwrap();
        assert_eq!(private["ok"], true);
        let passphrase = private["value"]["passphrase"].as_str().unwrap();
        assert_eq!(passphrase.split_whitespace().count(), 6);
        assert!(peppy_crypto::passphrase::passphrase_acceptable(passphrase));
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
        let conversation = core
            .client()
            .unwrap()
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: "+15555550101".into(),
                body: "0".into(),
                provider_message_id: Some("history-0".into()),
                imported: false,
            })
            .unwrap()
            .conversation_id;
        for index in 1..52 {
            core.client()
                .unwrap()
                .capture_incoming(IncomingSms {
                    conversation_id: Some(conversation),
                    sender_address: "+15555550101".into(),
                    body: index.to_string(),
                    provider_message_id: Some(format!("history-{index}")),
                    imported: false,
                })
                .unwrap();
        }
        let initial = value(&core.dispatch(
            &json!({"command":"snapshot","args":{"conversationId":conversation}}).to_string(),
        ));
        let view = initial["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|view| view["id"] == conversation.to_string())
            .unwrap();
        assert_eq!(
            view["messages"].as_array().unwrap().len(),
            HISTORY_PAGE_SIZE
        );
        assert_eq!(view["hasOlderMessages"], true);
        let anchor = view["messages"][0]["id"].as_str().unwrap();
        let page = value(&core.dispatch(
            &json!({"command":"load_history","args":{"conversationId":conversation,"beforeMessageId":anchor}}).to_string(),
        ));
        assert_eq!(page["messages"].as_array().unwrap().len(), 2);
        assert_eq!(page["hasMore"], false);
        let refreshed = value(&core.dispatch(
            &json!({"command":"snapshot","args":{"conversationId":conversation,"historyFromMessageId":anchor}}).to_string(),
        ));
        let refreshed_view = refreshed["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|view| view["id"] == conversation.to_string())
            .unwrap();
        assert_eq!(refreshed_view["hasOlderMessages"], true);
        let missing: Value = serde_json::from_str(&core.dispatch(
            &json!({"command":"load_history","args":{"conversationId":conversation,"beforeMessageId":Uuid::new_v4()}}).to_string(),
        ))
        .unwrap();
        assert_eq!(missing["ok"], false);
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

    #[test]
    fn initial_blank_save_reuses_the_recipientless_draft_without_overwriting_it() {
        let root = tempfile::tempdir().unwrap();
        let mut core = BrowserCore::new(root.path().to_owned());
        let device = DeviceId::new();
        open(&mut core, VaultId::new(), device, [7; 32]);

        let initial = value(&core.dispatch(
            r#"{"command":"save_draft","args":{"id":"new","conversationId":"","text":"","recipientIds":[],"attachmentIds":[],"expectedRevision":"0"}}"#,
        ));
        let saved = value(&core.dispatch(
            &json!({"command":"save_draft","args":{"id":initial["id"],"conversationId":initial["conversationId"],"text":"keep this","recipientIds":[],"attachmentIds":[],"expectedRevision":initial["revision"]}}).to_string(),
        ));
        let reopened = value(&core.dispatch(
            r#"{"command":"save_draft","args":{"id":"new","conversationId":"","text":"","recipientIds":[],"attachmentIds":[],"expectedRevision":"0"}}"#,
        ));

        assert_eq!(reopened["id"], saved["id"]);
        assert_eq!(reopened["text"], "keep this");
        let text_reopen: Value = serde_json::from_str(&core.dispatch(
            r#"{"command":"save_draft","args":{"id":"new","conversationId":"","text":"new content","recipientIds":[],"attachmentIds":[],"expectedRevision":"0"}}"#,
        ))
        .unwrap();
        assert_eq!(text_reopen["error"]["code"], "draft-moved");
        assert_eq!(text_reopen["error"]["draftId"], saved["id"]);
        assert_eq!(
            text_reopen["error"]["conversationId"],
            saved["conversationId"]
        );
        let snapshot = value(&core.dispatch(r#"{"command":"snapshot"}"#));
        assert_eq!(snapshot["conversations"][0]["recipientlessDraft"], true);
        assert_eq!(snapshot["conversations"][0]["draftOnly"], true);
    }

    #[test]
    fn discard_draft_is_dispatched_and_removes_an_empty_recipientless_row() {
        let root = tempfile::tempdir().unwrap();
        let mut core = BrowserCore::new(root.path().to_owned());
        open(&mut core, VaultId::new(), DeviceId::new(), [7; 32]);
        let draft = value(&core.dispatch(
            r#"{"command":"save_draft","args":{"id":"new","conversationId":"","text":"","recipientIds":[],"attachmentIds":[],"expectedRevision":"0"}}"#,
        ));

        value(&core.dispatch(
            &json!({"command":"discard_draft","args":{"id":draft["id"],"conversationId":draft["conversationId"],"expectedRevision":draft["revision"]}}).to_string(),
        ));
        assert!(
            value(&core.dispatch(r#"{"command":"snapshot"}"#))["conversations"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn invalid_new_draft_input_does_not_allocate_a_recipientless_draft() {
        let root = tempfile::tempdir().unwrap();
        let mut core = BrowserCore::new(root.path().to_owned());
        open(&mut core, VaultId::new(), DeviceId::new(), [7; 32]);

        let rejected: Value = serde_json::from_str(&core.dispatch(
            r#"{"command":"save_draft","args":{"id":"new","conversationId":"","text":"","recipientIds":["not a phone"],"attachmentIds":[],"expectedRevision":"0"}}"#,
        ))
        .unwrap();

        assert_eq!(rejected["error"]["code"], "invalid-recipient");
        assert!(
            value(&core.dispatch(r#"{"command":"snapshot"}"#))["conversations"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn history_rows_are_never_recipientless_drafts() {
        let root = tempfile::tempdir().unwrap();
        let mut core = BrowserCore::new(root.path().to_owned());
        open(&mut core, VaultId::new(), DeviceId::new(), [7; 32]);
        let client = core.client().unwrap();
        let history = client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: "+14155552671".into(),
                body: "history".into(),
                provider_message_id: Some("history-row".into()),
                imported: false,
            })
            .unwrap();
        let source = client.recipientless_compose_draft().unwrap();
        client
            .save_compose_draft(
                source.draft_id,
                source.revision,
                ComposeDraftUpdate {
                    text: "draft".into(),
                    recipients: vec!["+14155552671".into()],
                    ..Default::default()
                },
            )
            .unwrap();

        let snapshot = value(&core.dispatch(r#"{"command":"snapshot"}"#));
        let row = snapshot["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == history.conversation_id.to_string())
            .unwrap();
        assert_eq!(row["recipientlessDraft"], false);
        assert_eq!(row["draftOnly"], true);
    }
}
