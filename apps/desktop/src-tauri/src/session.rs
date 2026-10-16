//! One native session = one origin+vault+device binding, one client-core owner, one network
//! supervisor. All functions here are synchronous (they call into client-core / the filesystem)
//! and are invoked from `spawn_blocking`, never directly on an async worker or the UI thread.
use crate::{
    credentials::{ensure_database_key, prepare_data_dir, stored_credential, Binding},
    dto::{
        self, AttachmentView, ConversationView, DraftView, GatewayView, Head, HistoryPage, Snapshot,
    },
    error::{core_error, BridgeError, BridgeResult},
    media,
    net::Api,
    notifications::NotificationPreferences,
    secure_store::SecretStore,
};
use peppy_client_core::{
    AttachmentId, Client, ClientConfig, ConversationId, DatabaseKey, DeviceId, DraftId, Message,
    MessageId, NativeKeyCache, VaultId,
};
use peppy_desktop_api::compose::{self, ComposeError};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub type Notifier = Arc<dyn Fn() + Send + Sync>;

const INITIAL_HISTORY_MESSAGES: usize = 50;
const MAX_PREVIEWS_PER_SNAPSHOT: usize = 12;
const MAX_PREVIEW_CACHE: usize = 256;
const MAX_SEEN_IDS: usize = 1000;
const PENDING_COUNT_CAP: usize = 200;
pub const GATEWAYS_FILE: &str = "gateways.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultSummary {
    pub epoch: u32,
    pub fingerprint: String,
}

#[derive(Default)]
pub struct NetStatus {
    /// A live WebSocket is negotiated (`ready` received).
    pub live: bool,
    pub error_code: Option<&'static str>,
    /// Last non-transport outbox/media problem, shown while otherwise connected.
    pub work_error: Option<&'static str>,
    pub revoked: bool,
    pub gateways: Vec<GatewayView>,
    pub gateways_known: bool,
    pub vault: Option<VaultSummary>,
    pub device_role: Option<String>,
    pub revoked_device_ids: HashSet<String>,
}

/// Local attachment ID -> server object ID, learned when this host uploads or downloads an
/// object. Core keeps the authoritative binding in SQLCipher but exposes it only for pending
/// work; this owner-only sidecar lets the owning device request a public copy later. It holds
/// no key material.
pub struct RemoteIds {
    path: PathBuf,
    map: HashMap<String, String>,
}
impl RemoteIds {
    fn load(path: PathBuf) -> Self {
        let map = fs::read(&path)
            .ok()
            .filter(|bytes| bytes.len() <= 4 * 1024 * 1024)
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { path, map }
    }
    pub fn get(&self, local: AttachmentId) -> Option<String> {
        self.map.get(&local.to_string()).cloned()
    }
    pub fn insert(&mut self, local: AttachmentId, remote: &str) {
        if self.map.get(&local.to_string()).map(String::as_str) == Some(remote)
            || uuid::Uuid::parse_str(remote).is_err()
        {
            return;
        }
        self.map.insert(local.to_string(), remote.to_owned());
        if let Ok(bytes) = serde_json::to_vec(&self.map) {
            let _ = crate::fsutil::write_private_atomic(&self.path, &bytes);
        }
    }
}

pub struct Session {
    pub binding: Binding,
    pub client: Client,
    pub api: Api,
    pub data_dir: PathBuf,
    pub status: Mutex<NetStatus>,
    pub previews: Mutex<HashMap<AttachmentId, Option<String>>>,
    pub transfer_errors: Mutex<HashMap<AttachmentId, String>>,
    pub remote_ids: Mutex<RemoteIds>,
    /// Wakes the outbound/media worker only (send, read state, unlock).
    pub work_wake: Notify,
    /// Wakes a pending reconnect wait (for example after unlock).
    pub reconnect_wake: Notify,
    pub cancel: CancellationToken,
    pub notifier: Notifier,
    pub mismatch: AtomicBool,
    /// Wakes the live loop to run a requested contact-projection repair snapshot now.
    pub repair_wake: Notify,
    /// One fenced repair snapshot per session (or per manual request) while the latch is set.
    pub repair_attempted: AtomicBool,
    /// A staged snapshot generation known to be impossible to complete; never resumed.
    pub abandoned_snapshot: Mutex<Option<u64>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn host() -> BridgeError {
    BridgeError::host_state()
}

/// Opens (or re-attaches to) the binding's database with its existing protected key, and
/// restores cached purpose keys without a passphrase when their integrity check passes.
pub fn open_session(
    root: &Path,
    store: &dyn SecretStore,
    binding: &Binding,
    cached_epochs: &[u32],
    notifier: Notifier,
) -> BridgeResult<Session> {
    let credential = stored_credential(store, binding)?.ok_or_else(BridgeError::no_session)?;
    let data_dir = prepare_data_dir(root, binding)?;
    let database = binding.database_path(root);
    let key = ensure_database_key(store, binding, &database)?;
    let config = ClientConfig {
        database_path: database,
        vault_id: VaultId::from_str(&binding.vault_id).map_err(|_| host())?,
        device_id: DeviceId::from_str(&binding.device_id).map_err(|_| host())?,
    };
    let client =
        Client::open(config, DatabaseKey::new(&key).map_err(core_error)?).map_err(core_error)?;
    for epoch in cached_epochs {
        if let Some(bytes) = store.get(&binding.key_cache_account(*epoch))? {
            match client
                .import_native_key_cache(&NativeKeyCache::from_native_storage(bytes.to_vec()))
            {
                // A stale or unverifiable cache simply leaves this epoch locked.
                Ok(())
                | Err(
                    peppy_client_core::Error::InvalidKeyCache
                    | peppy_client_core::Error::InvalidProfile,
                ) => {}
                Err(error) => return Err(core_error(error)),
            }
        }
    }
    let api = Api::new(&credential.origin, &credential.device_token)?;
    // Last server-reported routes, so an offline send can still be checked against them.
    let gateways: Option<Vec<GatewayView>> = fs::read(data_dir.join(GATEWAYS_FILE))
        .ok()
        .filter(|b| b.len() <= 256 * 1024)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .map(sanitize_cached_gateways);
    let status = NetStatus {
        gateways_known: gateways.is_some(),
        gateways: gateways.unwrap_or_default(),
        ..NetStatus::default()
    };
    Ok(Session {
        binding: binding.clone(),
        client,
        api,
        remote_ids: Mutex::new(RemoteIds::load(data_dir.join("remote-objects.json"))),
        data_dir,
        status: Mutex::new(status),
        previews: Mutex::new(HashMap::new()),
        transfer_errors: Mutex::new(HashMap::new()),
        work_wake: Notify::new(),
        reconnect_wake: Notify::new(),
        cancel: CancellationToken::new(),
        notifier,
        mismatch: AtomicBool::new(false),
        abandoned_snapshot: Mutex::new(None),
        repair_wake: Notify::new(),
        repair_attempted: AtomicBool::new(false),
    })
}

fn sanitize_cached_gateways(mut gateways: Vec<GatewayView>) -> Vec<GatewayView> {
    for gateway in &mut gateways {
        gateway.supports_mms &= gateway.mms_content_version.unwrap_or(0) >= 2;
        gateway.availability = peppy_desktop_api::GatewayAvailability::Unknown;
        gateway.last_seen_age_seconds = None;
        gateway.presence = None;
        gateway.wake_route = false;
        gateway.online = false;
    }
    gateways
}

pub use peppy_desktop_api::compose::DraftInput;

fn compose_error(error: ComposeError) -> BridgeError {
    match error {
        ComposeError::Ui { code, message } => BridgeError::new(code, message),
        ComposeError::ReplyBlocked(message) => BridgeError::new("mms-reply-blocked", message),
        ComposeError::Core(error) => core_error(error),
        ComposeError::AfterRoute {
            source,
            current_revision,
        } => match source {
            peppy_client_core::Error::DraftMoved { .. } => core_error(source),
            source => core_error(source).with_revision(current_revision),
        },
    }
}

impl Session {
    pub fn request_work(&self) {
        self.work_wake.notify_one();
    }

    pub fn notify(&self) {
        (self.notifier)();
    }

    pub fn gateways(&self) -> (Vec<GatewayView>, bool) {
        let status = self
            .status
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        (status.gateways.clone(), status.gateways_known)
    }

    pub fn revoked_devices(&self) -> HashSet<String> {
        self.status.lock().unwrap_or_else(|poison| poison.into_inner()).revoked_device_ids.clone()
    }

    /// CAS draft save. Text, recipients, attachments and route are persisted together.
    pub fn save_draft(&self, input: &DraftInput) -> BridgeResult<DraftView> {
        let previous = DraftId::from_str(&input.id)
            .ok()
            .and_then(|id| self.client.compose_draft(id).ok().flatten());
        let draft = compose::save_draft(&self.client, input).map_err(compose_error)?;
        if previous.as_ref().is_none_or(|old| {
            old.draft_id.to_string() != draft.id
                || old.conversation_id.to_string() != draft.conversation_id
        }) {
            self.notify();
        }
        Ok(draft)
    }

    pub fn discard_draft(
        &self,
        input: &peppy_desktop_api::compose::DiscardDraftInput,
    ) -> BridgeResult<()> {
        compose::discard_draft(&self.client, input).map_err(compose_error)?;
        self.notify();
        Ok(())
    }

    /// Queues locally only after shared composition policy accepts the stored draft.
    pub fn send_draft(
        &self,
        input: &DraftInput,
    ) -> BridgeResult<peppy_desktop_api::SendResultView> {
        let (gateways, known) = self.gateways();
        let result = compose::send_draft(&self.client, input, known.then_some(gateways.as_slice()))
            .map_err(compose_error)?;
        self.request_work();
        Ok(result)
    }

    pub fn mark_seen(&self, ids: &[String]) -> BridgeResult<()> {
        self.mark_seen_scoped(ids, None)
    }

    pub fn redirected_conversation(
        &self,
        source: ConversationId,
    ) -> BridgeResult<Option<ConversationId>> {
        self.client
            .conversation_redirect(source)
            .map_err(core_error)
    }

    /// Validates a complete batch before applying any read marks. Composer
    /// ownership loads its transcript exactly once, so mixed-conversation IDs
    /// cannot partially acknowledge messages before the scope error is found.
    pub fn mark_seen_scoped(
        &self,
        ids: &[String],
        conversation: Option<ConversationId>,
    ) -> BridgeResult<()> {
        if ids.len() > MAX_SEEN_IDS {
            return Err(BridgeError::new(
                "invalid-message",
                "Too many message IDs in one request.",
            ));
        }
        let parsed = ids
            .iter()
            .map(|id| {
                MessageId::from_str(id)
                    .map_err(|_| BridgeError::new("invalid-message", "The message ID is invalid."))
            })
            .collect::<BridgeResult<Vec<_>>>()?;
        if let Some(conversation) = conversation {
            let allowed = self
                .client
                .messages(conversation)
                .map_err(core_error)?
                .into_iter()
                .map(|message| message.payload.record.message_id)
                .collect::<HashSet<_>>();
            if parsed.iter().any(|id| !allowed.contains(id)) {
                return Err(BridgeError::new(
                    "window-context",
                    "A composer window can only mark its own conversation messages as seen.",
                ));
            }
        }
        let mut changed = false;
        for id in parsed {
            changed |= self.client.mark_seen(id).map_err(core_error)?;
        }
        if changed {
            self.notify();
            self.request_work();
        }
        Ok(())
    }

    /// Encrypts a natively picked file into core-owned storage; JS receives a sanitized handle.
    pub fn prepare_attachment(&self, path: &Path) -> BridgeResult<AttachmentView> {
        let metadata = fs::metadata(path).map_err(|_| {
            BridgeError::new("attachment-local", "The selected file could not be read.")
        })?;
        if !metadata.is_file() {
            return Err(BridgeError::new(
                "attachment-local",
                "Select a regular file.",
            ));
        }
        let prefix = media::read_prefix(path).map_err(|_| {
            BridgeError::new("attachment-local", "The selected file could not be read.")
        })?;
        let media_type = media::sniff_media_type(&prefix);
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into());
        let info = self
            .client
            .prepare_attachment(path, media_type, &name)
            .map_err(core_error)?;
        let preview = media::is_previewable(media_type)
            .then(|| media::read_capped(path).and_then(|bytes| media::preview_data_url(&bytes)))
            .flatten();
        self.cache_preview(info.attachment_id, preview.clone());
        Ok(dto::attachment_view(&info, None, preview))
    }

    /// Retries only a locally recorded transfer failure. It never changes message state or queues
    /// a carrier command; the normal worker owns both of those durable transitions.
    pub fn retry_attachment(&self, id: &str) -> BridgeResult<()> {
        let id = AttachmentId::from_str(id)
            .map_err(|_| BridgeError::new("invalid-attachment", "The attachment ID is invalid."))?;
        self.client.attachment_info(id).map_err(core_error)?;
        let removed = self
            .transfer_errors
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&id)
            .is_some();
        if !removed {
            return Err(BridgeError::new(
                "attachment-not-retryable",
                "This attachment has no retryable transfer failure.",
            ));
        }
        self.request_work();
        Ok(())
    }

    pub fn attachment_in_conversation(
        &self,
        id: &str,
        conversation: ConversationId,
    ) -> BridgeResult<bool> {
        let id = AttachmentId::from_str(id)
            .map_err(|_| BridgeError::new("invalid-attachment", "The attachment ID is invalid."))?;
        Ok(self
            .client
            .messages(conversation)
            .map_err(core_error)?
            .iter()
            .any(|message| {
                message
                    .payload
                    .record
                    .attachments
                    .iter()
                    .any(|reference| reference.attachment_id == id)
            }))
    }

    pub fn save_attachment(&self, id: &str, destination: &Path) -> BridgeResult<()> {
        let id = AttachmentId::from_str(id)
            .map_err(|_| BridgeError::new("invalid-attachment", "The attachment ID is invalid."))?;
        let info = self.client.attachment_info(id).map_err(core_error)?;
        if !info.state.is_local() {
            return Err(BridgeError::new(
                "attachment-unavailable",
                "This attachment is not verified and ready to save.",
            ));
        }
        let plaintext = self.client.open_native_plaintext(id).map_err(core_error)?;
        crate::fsutil::copy_new_atomic(plaintext.path(), destination, info.plaintext_bytes).map_err(
            |_| {
                BridgeError::new(
                    "attachment-save",
                    "The attachment could not be saved safely.",
                )
            },
        )
    }

    fn cache_preview(&self, id: AttachmentId, preview: Option<String>) {
        let mut cache = self
            .previews
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if cache.len() >= MAX_PREVIEW_CACHE {
            cache.clear();
        }
        cache.insert(id, preview);
    }

    fn attachment_views(
        &self,
        message: &Message,
        budget: &mut usize,
        deferred: &mut bool,
        eligible: Option<&HashSet<AttachmentId>>,
    ) -> Vec<AttachmentView> {
        let errors = self
            .transfer_errors
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        message
            .payload
            .record
            .attachments
            .iter()
            .map(|reference| {
                let id = reference.attachment_id;
                let Ok(info) = self.client.attachment_info(id) else {
                    return AttachmentView {
                        id: id.to_string(),
                        name: "Attachment".into(),
                        media_type: "application/octet-stream".into(),
                        byte_size: 0,
                        state: "pending",
                        error: None,
                        preview_url: None,
                        transfer: Some("download"),
                        retryable: None,
                    };
                };
                let mut preview = None;
                if media::is_previewable(&info.media_type) && info.state.is_local() {
                    let cached = self
                        .previews
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .get(&id)
                        .cloned();
                    preview = match cached {
                        Some(value) => value,
                        None if eligible.is_none_or(|ids| ids.contains(&id)) && *budget > 0 => {
                            *budget -= 1;
                            let value = self
                                .client
                                .open_native_plaintext(id)
                                .ok()
                                .and_then(|file| media::read_capped(file.path()))
                                .and_then(|bytes| media::preview_data_url(&bytes));
                            self.cache_preview(id, value.clone());
                            value
                        }
                        None => {
                            if eligible.is_none_or(|ids| ids.contains(&id)) {
                                *deferred = true;
                            }
                            None
                        }
                    };
                }
                dto::attachment_view(&info, errors.get(&id).cloned(), preview)
            })
            .collect()
    }

    fn message_views(
        &self,
        messages: Vec<Message>,
        budget: &mut usize,
        deferred: &mut bool,
        eligible: Option<&HashSet<AttachmentId>>,
    ) -> Vec<dto::MessageView> {
        messages
            .iter()
            .map(|message| {
                dto::message_view(
                    message,
                    self.attachment_views(message, budget, deferred, eligible),
                )
            })
            .collect()
    }

    pub fn history_page(
        &self,
        conversation_id: &str,
        before_message_id: &str,
    ) -> BridgeResult<HistoryPage> {
        let conversation = ConversationId::from_str(conversation_id)
            .map_err(|_| BridgeError::new("invalid-request", "The conversation ID is invalid."))?;
        let anchor = MessageId::from_str(before_message_id)
            .map_err(|_| BridgeError::new("invalid-request", "The message ID is invalid."))?;
        let page = self
            .client
            .messages_page(conversation, Some(anchor), INITIAL_HISTORY_MESSAGES)
            .map_err(core_error)?;
        let (mut budget, mut deferred) = (MAX_PREVIEWS_PER_SNAPSHOT, false);
        let messages = self.message_views(page.messages, &mut budget, &mut deferred, None);
        Ok(HistoryPage {
            conversation_id: conversation.to_string(),
            messages,
            has_more: page.has_more,
        })
    }

    /// Builds the sanitized UI snapshot from core state. Returns whether previews were deferred
    /// (the caller then emits another state hint).
    pub fn snapshot(
        &self,
        requested: Option<&str>,
        history_from_message_id: Option<&str>,
        head: Head,
        origin: Option<String>,
        notification_preferences: NotificationPreferences,
    ) -> BridgeResult<(Snapshot, bool)> {
        let conversations = self.client.list_conversations().map_err(core_error)?;
        let drafts = self.client.compose_drafts().map_err(core_error)?;
        let drafts_by_conversation: HashMap<ConversationId, _> = drafts
            .iter()
            .map(|draft| (draft.conversation_id, draft))
            .collect();
        let known: HashSet<ConversationId> = conversations
            .iter()
            .map(|c| c.conversation_id)
            .chain(drafts.iter().map(|d| d.conversation_id))
            .collect();
        let requested = requested
            .and_then(|id| ConversationId::from_str(id).ok())
            .filter(|id| known.contains(id));
        let active = requested
            .or_else(|| conversations.first().map(|c| c.conversation_id))
            .or_else(|| drafts.first().map(|d| d.conversation_id));
        let (mut budget, mut deferred) = (MAX_PREVIEWS_PER_SNAPSHOT, false);
        let mut views = Vec::new();
        for conversation in &conversations {
            let latest = self
                .client
                .latest_message(conversation.conversation_id)
                .map_err(core_error)?;
            let last = latest.as_ref();
            let draft = drafts_by_conversation
                .get(&conversation.conversation_id)
                .copied();
            let draft_only = draft
                .map(|draft| self.client.compose_draft_is_new(draft.draft_id))
                .transpose()
                .map_err(core_error)?
                .unwrap_or(false);
            let name = last
                .map(dto::counterpart)
                .or_else(|| draft.map(|d| d.recipients.join(", ")))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Conversation".into());
            let preview = last
                .map(|m| {
                    if m.payload.body.is_empty() && !m.payload.record.attachments.is_empty() {
                        "Attachment".into()
                    } else {
                        m.payload.body.clone()
                    }
                })
                .unwrap_or_default();
            let (message_views, has_older_messages) = if Some(conversation.conversation_id)
                == active
            {
                let history_from_message_id = (requested.is_some()
                    && Some(conversation.conversation_id) == active)
                    .then_some(history_from_message_id)
                    .flatten();
                let (messages, has_more) = match history_from_message_id {
                    Some(anchor) => {
                        let anchor = MessageId::from_str(anchor).map_err(|_| {
                            BridgeError::new("invalid-request", "The message ID is invalid.")
                        })?;
                        let has_more = !self
                            .client
                            .messages_page(conversation.conversation_id, Some(anchor), 1)
                            .map_err(core_error)?
                            .messages
                            .is_empty();
                        self.client
                            .messages_since(conversation.conversation_id, anchor)
                            .map(|messages| (messages, has_more))
                    }
                    None => self
                        .client
                        .messages_page(conversation.conversation_id, None, INITIAL_HISTORY_MESSAGES)
                        .map(|page| (page.messages, page.has_more)),
                }
                .map_err(core_error)?;
                let eligible = snapshot_preview_ids(&messages);
                (
                    self.message_views(messages, &mut budget, &mut deferred, Some(&eligible)),
                    Some(has_more),
                )
            } else {
                (Vec::new(), None)
            };
            let reply_context = self
                .client
                .mms_reply_context(conversation.conversation_id)
                .ok();
            views.push(ConversationView {
                id: conversation.conversation_id.to_string(),
                name,
                preview,
                unread: conversation.unread_count,
                messages: message_views,
                has_older_messages,
                participants: reply_context
                    .as_ref()
                    .map(|context| context.recipients.clone()),
                reply_blocked_reason: reply_context.and_then(|context| context.blocked_reason),
                recipientless_draft: false,
                draft_only,
            });
        }
        let listed: HashSet<ConversationId> =
            conversations.iter().map(|c| c.conversation_id).collect();
        for draft in drafts
            .iter()
            .filter(|d| !listed.contains(&d.conversation_id))
        {
            views.push(ConversationView {
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
                reply_blocked_reason: None,
                recipientless_draft: draft.recipients.is_empty(),
                draft_only: true,
            });
        }
        let draft = active
            .and_then(|id| drafts_by_conversation.get(&id).copied())
            .map(dto::draft_view);
        let pending = self
            .client
            .pending_outbox_batch(PENDING_COUNT_CAP)
            .map_err(core_error)?
            .len() as u64;
        let quarantine = self.client.quarantined().map_err(core_error)?.len() as u64;
        let notification_snapshot = self.client.notification_snapshot().map_err(core_error)?;
        let keys = self.client.key_status().map_err(core_error)?;
        let status = self
            .status
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let unlocked = keys
            .active_epoch
            .is_some_and(|epoch| keys.unlocked_epochs.contains(&epoch));
        let behind = matches!((&status.vault, keys.active_epoch), (Some(vault), Some(active)) if vault.epoch > active);
        let encryption = if self.mismatch.load(Ordering::Relaxed) {
            dto::Encryption {
                state: "mismatch",
                profile_fingerprint: None,
            }
        } else if unlocked && !behind {
            dto::Encryption {
                state: "unlocked",
                profile_fingerprint: status
                    .vault
                    .as_ref()
                    .filter(|v| Some(v.epoch) == keys.active_epoch)
                    .map(|v| v.fingerprint.clone()),
            }
        } else {
            dto::Encryption {
                state: "locked",
                profile_fingerprint: None,
            }
        };
        let connection = if status.revoked {
            dto::Connection {
                state: "error",
                origin,
                error_code: Some("revoked"),
            }
        } else if status.live {
            dto::Connection {
                state: "connected",
                origin,
                error_code: status.work_error,
            }
        } else {
            dto::Connection {
                state: "offline",
                origin,
                error_code: status.error_code.or(Some("connecting")),
            }
        };
        drop(status);
        let mut addresses: Vec<(String, Option<String>)> = Vec::new();
        for view in &views {
            for address in view.participants.iter().flatten().chain([&view.name]) {
                addresses.push((address.clone(), None));
            }
        }
        for draft in &drafts {
            let source = draft
                .route
                .as_ref()
                .map(|r| r.gateway_device_id.to_string());
            for recipient in &draft.recipients {
                addresses.push((recipient.clone(), source.clone()));
            }
        }
        for notification in &notification_snapshot.notifications {
            addresses.push((
                notification.title.clone(),
                Some(notification.target.source_device_id.clone()),
            ));
        }
        let contacts = crate::contacts::snapshot_contacts(self, &addresses);
        let status = self
            .status
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let snapshot = Snapshot {
            version: "1",
            mode: "native",
            connection,
            encryption,
            gateways: status.gateways.clone(),
            conversations: views,
            notifications: notification_snapshot.notifications,
            app_filters: notification_snapshot.app_filters,
            notification_preferences,
            active_conversation_id: active.map(|id| id.to_string()),
            draft,
            head,
            desktop: None,
            device_role: status.device_role.clone(),
            pending_count: pending,
            quarantine_count: quarantine,
            contact_resolution: contacts.resolution,
            contact_books: contacts.books,
            contacts_pending_count: contacts.pending_count,
            contact_sync: contacts.sync,
        };
        Ok((snapshot, deferred))
    }

    pub fn snapshot_scoped(
        &self,
        requested: &str,
        history_from_message_id: Option<&str>,
        head: Head,
        origin: Option<String>,
        notification_preferences: NotificationPreferences,
    ) -> BridgeResult<(Snapshot, bool)> {
        let snapshot = self.snapshot(
            Some(requested),
            history_from_message_id,
            head,
            origin,
            notification_preferences,
        )?;
        if snapshot.0.active_conversation_id.as_deref() != Some(requested) {
            return Err(BridgeError::new(
                "not-found",
                "The requested stored item was not found.",
            ));
        }
        Ok(snapshot)
    }

    /// Persists the last reported gateway views (non-secret display/routing data, mode 0600).
    pub fn persist_gateways(&self, views: &[GatewayView]) {
        if let Ok(bytes) = serde_json::to_vec(views) {
            let _ = crate::fsutil::write_private_atomic(&self.data_dir.join(GATEWAYS_FILE), &bytes);
        }
    }

    pub fn set_status(&self, update: impl FnOnce(&mut NetStatus)) {
        update(
            &mut self
                .status
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        );
    }
}

fn snapshot_preview_ids(messages: &[Message]) -> HashSet<AttachmentId> {
    bounded_preview_ids(
        messages
            .iter()
            .rev()
            .take(INITIAL_HISTORY_MESSAGES)
            .flat_map(|message| {
                message
                    .payload
                    .record
                    .attachments
                    .iter()
                    .map(|attachment| attachment.attachment_id)
            }),
    )
}

fn bounded_preview_ids(ids: impl IntoIterator<Item = AttachmentId>) -> HashSet<AttachmentId> {
    let mut eligible = HashSet::with_capacity(MAX_PREVIEW_CACHE);
    for id in ids {
        eligible.insert(id);
        if eligible.len() == MAX_PREVIEW_CACHE {
            break;
        }
    }
    eligible
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{fixture_at, PHRASE};
    use peppy_client_core::IncomingSms;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn cached_gateways_never_restore_live_presence() {
        let gateway: GatewayView = serde_json::from_value(serde_json::json!({
            "id": "phone", "name": "Pixel", "simId": "sim", "online": true,
            "simulated": false, "supportsSms": true, "supportsMms": true,
            "availability": "offline", "lastSeenAgeSeconds": 12,
            "presence": { "state": "connected", "last_seen_age_seconds": 12 },
            "wakeRoute": true
        })).unwrap();
        let cached = sanitize_cached_gateways(vec![gateway]);
        assert_eq!(cached[0].availability, peppy_desktop_api::GatewayAvailability::Unknown);
        assert_eq!(cached[0].last_seen_age_seconds, None);
        assert_eq!(cached[0].presence, None);
        assert!(!cached[0].online);
    }

    #[test]
    fn mark_seen_scoped_notifies_once_when_messages_change() {
        let fixture = fixture_at("http://127.0.0.1:9");
        let count = Arc::new(AtomicUsize::new(0));
        let notifier_count = count.clone();
        let session = open_session(
            fixture.dir.path(),
            &fixture.store,
            &fixture.binding,
            &[],
            Arc::new(move || {
                notifier_count.fetch_add(1, Ordering::Relaxed);
            }),
        )
        .unwrap();
        session
            .client
            .unlock(&fixture.profile, &fixture.header, PHRASE)
            .unwrap();
        let draft = session
            .save_draft(
                &serde_json::from_value(serde_json::json!({
                    "id": "mark-seen-draft", "conversationId": "", "text": "",
                    "recipientIds": ["+15555550100"], "attachmentIds": [], "expectedRevision": "0"
                }))
                .unwrap(),
            )
            .unwrap();
        let conversation = ConversationId::from_str(&draft.conversation_id).unwrap();
        count.store(0, Ordering::Relaxed);
        let captured = session
            .client
            .capture_incoming(IncomingSms {
                conversation_id: Some(conversation),
                sender_address: "+15555550100".into(),
                body: "unread".into(),
                provider_message_id: Some("mark-seen-notify".into()),
                imported: false,
            })
            .unwrap();
        assert_eq!(captured.conversation_id, conversation);
        let id = session.client.messages(conversation).unwrap()[0]
            .payload
            .record
            .message_id
            .to_string();

        session.mark_seen_scoped(&[id], Some(conversation)).unwrap();
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn snapshots_and_pages_keep_history_continuous_beyond_the_initial_window() {
        let fixture = fixture_at("http://127.0.0.1:9");
        let session = open_session(
            fixture.dir.path(),
            &fixture.store,
            &fixture.binding,
            &[],
            Arc::new(|| {}),
        )
        .unwrap();
        session
            .client
            .unlock(&fixture.profile, &fixture.header, PHRASE)
            .unwrap();
        let conversation = session
            .client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: "+15555550100".into(),
                body: "0".into(),
                provider_message_id: Some("history-0".into()),
                imported: false,
            })
            .unwrap()
            .conversation_id;
        for index in 1..52 {
            session
                .client
                .capture_incoming(IncomingSms {
                    conversation_id: Some(conversation),
                    sender_address: "+15555550100".into(),
                    body: index.to_string(),
                    provider_message_id: Some(format!("history-{index}")),
                    imported: false,
                })
                .unwrap();
        }
        let initial = session
            .snapshot(
                Some(&conversation.to_string()),
                None,
                crate::head(),
                None,
                NotificationPreferences::default(),
            )
            .unwrap()
            .0;
        let view = initial
            .conversations
            .iter()
            .find(|view| view.id == conversation.to_string())
            .unwrap();
        assert_eq!(view.messages.len(), INITIAL_HISTORY_MESSAGES);
        assert_eq!(view.has_older_messages, Some(true));
        let anchor = view.messages[0].id.clone();
        let page = session
            .history_page(&conversation.to_string(), &anchor)
            .unwrap();
        assert_eq!(page.messages.len(), 2);
        assert!(!page.has_more);
        let refreshed = session
            .snapshot(
                Some(&conversation.to_string()),
                Some(&anchor),
                crate::head(),
                None,
                NotificationPreferences::default(),
            )
            .unwrap()
            .0;
        assert_eq!(refreshed.conversations[0].has_older_messages, Some(true));
        assert!(session
            .history_page(&conversation.to_string(), &uuid::Uuid::new_v4().to_string())
            .is_err());
        let other = session
            .client
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: "+15555550101".into(),
                body: "other".into(),
                provider_message_id: Some("history-other".into()),
                imported: false,
            })
            .unwrap();
        let other_id = session.client.messages(other.conversation_id).unwrap()[0]
            .payload
            .record
            .message_id
            .to_string();
        assert!(session
            .history_page(&conversation.to_string(), &other_id)
            .is_err());
    }

    #[test]
    fn snapshot_preview_ids_bound_auto_hydration_to_the_newest_window() {
        let ids = (0..300)
            .map(|_| AttachmentId::from_str(&uuid::Uuid::new_v4().to_string()).unwrap())
            .collect::<Vec<_>>();
        let eligible = bounded_preview_ids(ids.iter().copied());
        assert_eq!(eligible.len(), MAX_PREVIEW_CACHE);
        assert!(ids[..MAX_PREVIEW_CACHE]
            .iter()
            .all(|id| eligible.contains(id)));
        assert!(ids[MAX_PREVIEW_CACHE..]
            .iter()
            .all(|id| !eligible.contains(id)));
    }
}
