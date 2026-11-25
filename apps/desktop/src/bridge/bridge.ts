/// <reference types="vite/client" />
import type { LaunchProgress } from "@peppy/generated/launch-progress";

/**
 * UI-only native boundary. These DTOs intentionally contain no credentials,
 * cryptographic material, root or device keys, device tokens, or raw filesystem paths.
 * Renderer-visible invitation and webhook secrets are explicit, narrowly scoped exceptions.
 */
import type {
  AttachmentView,
  Draft,
  BridgeError,
  EnrollmentView as SharedEnrollmentView,
  ComputerClaimantView,
  TypingSignal,
  DesktopBridge as SharedDesktopBridge,
  DiagnosticsExportResult,
  DiagnosticsState,
} from "@peppy/desktop-ui/bridge-contracts";
import { errorText } from "./bridgeErrors";
import { fixtureBridge } from "./fixtureBridge";

export {
  addFixtureMessageForTests,
  fixtureBridge,
  fixtureReactionConversations,
  mergeFixtureConversation,
  resetFixtureDiagnosticsForTests,
  setFixtureContactDeleteGateForTests,
} from "./fixtureBridge";

export {
  type ConnectionState,
  type MessageStatus,
  type AttachmentState,
  type AttachmentView,
  type MessageView,
  type ConversationView,
  type HistoryPage,
  type GatewayView,
  type Draft,
  type DesktopSnapshot,
  type DraftInput,
  type DiscardDraftInput,
  type SendDraftInput,
  type PublicCopy,
  type NotificationTarget,
  type MirroredNotification,
  type AppFilter,
  type NotificationPreferences,
  type NotificationCheck,
  type NotificationPersistence,
  type NotificationPersistenceSettings,
  type NotificationQuickReplySettings,
  type NotificationReplyRecovery,
  type NotificationReplyRecoveries,
  type NotificationPermissionStatus,
  type ContactBookState,
  type BookCapabilities,
  type ContactBookView,
  type ContactPhone,
  type ContactEmail,
  type ContactAddress,
  type ContactBirthday,
  type ContactSort,
  type ContactView,
  type ContactEditRequestInput,
  type ContactEditOutcome,
  type ContactEditStatus,
  type ContactSyncStatus,
  type RecipientSuggestion,
  type RestorableContact,
  type ResolvedContact,
  type ContactResolutionMap,
  type BridgeError,
  type BridgeErrorCode,
  type BillingSummaryView,
  type BillingProvider,
  type BillingManageableIn,
  type HostedSignInProvider,
  type HostedAccountView,
  type ComputerClaimantView,
  type HostedPreviewView,
  type AppLockStatus,
  type MaximizeButtonRegion,
  type ManagedDeviceView,
  type DeviceRosterView,
  type ContactScanStatus,
  type MediaCursor,
  type MediaMetadataView,
  type MediaPageItem,
  type MediaPage,
  type MediaOpen,
  type LocalSearchRequest,
  type LocalSearchPage,
  type SearchIndexProgress,
  type HistorySearchAttemptInput,
  type HistorySearchSnapshot,
  type StartHistorySearchInput,
  type MessageWindowRequest,
  type MessageWindow,
  type AttachmentRetentionStatusResult,
  type PurchaseOptions,
  type StartupStatus,
  type StoreProduct,
  type TypingSignal,
} from "@peppy/desktop-ui/bridge-contracts";

/** Extends the sanitized enrollment DTO; the QR payload may intentionally render an invitation secret, never root/private keys or device credentials. */
export type EnrollmentView = SharedEnrollmentView & { computerClaimant?: ComputerClaimantView };
export type DesktopBridge = Omit<SharedDesktopBridge, "enrollment_discover"> & {
  enrollment_discover(origin?: string): Promise<EnrollmentView>;
  launch_progress?: () => Promise<LaunchProgress>;
  subscribe_launch_progress?: (listener: (progress: LaunchProgress) => void) => (() => void) & { ready?: Promise<void> };
};

const invoke = async <T>(command: string, args?: Record<string, unknown>) => (await import("@tauri-apps/api/core")).invoke<T>(command, args);
const invokeRaw = async <T>(command: string, bytes: ArrayBuffer, headers: Record<string, string>) => {
  const { invoke: nativeInvoke } = await import("@tauri-apps/api/core");
  return nativeInvoke<T>(command, bytes, { headers });
};
/** Native claimant response, normalized before it reaches shared UI. */
type NativeComputerClaimantView = {
  stage: "waiting_owner" | "compare_codes" | "finishing" | "connected" | "denied" | "expired" | "error" | string;
  invitationId: string;
  qrPayload: string | null;
  awaitingOwner: boolean;
  state: "waiting" | "claimed" | "approved" | "confirmed" | "consumed" | "denied" | "expired";
  expiresInSeconds: number;
  verificationCode: string | null;
  errorCode?: ComputerClaimantView["errorCode"];
};
export const normalizeComputerClaimant = (view: NativeComputerClaimantView): ComputerClaimantView => ({
  stage: view.stage === "waiting_owner" ? "computer_waiting"
    : view.stage === "compare_codes" ? "computer_approved"
      : view.stage === "finishing" ? "computer_finishing"
        : view.stage === "connected" ? "computer_connected"
          : view.stage === "denied" ? "computer_denied"
            : view.stage === "expired" ? "computer_expired" : "computer_error",
  qrPayload: view.qrPayload ?? undefined,
  verificationCode: view.verificationCode ?? undefined,
  expiresInSeconds: view.expiresInSeconds,
  awaitingOwner: view.awaitingOwner,
  computerClaimant: true,
  errorCode: view.stage === "error" || !["waiting_owner", "compare_codes", "finishing", "connected", "denied", "expired"].includes(view.stage) ? view.errorCode ?? "computer_verification_failed" : view.errorCode,
});
const discoverEnrollment = async (origin?: string): Promise<EnrollmentView> => {
  const { computerClaimant, ...view } = await invoke<SharedEnrollmentView & { computerClaimant?: NativeComputerClaimantView | null }>("enrollment_discover", { origin });
  return { ...view, ...(computerClaimant ? { computerClaimant: normalizeComputerClaimant(computerClaimant) } : {}) };
};
const beginComputerClaimant = () => invoke<NativeComputerClaimantView>("enrollment_begin_computer_claimant").then(normalizeComputerClaimant);
const computerClaimantStatus = () => invoke<NativeComputerClaimantView>("enrollment_computer_claimant_status").then(normalizeComputerClaimant);
const confirmComputerClaimant = () => invoke<NativeComputerClaimantView>("enrollment_confirm_computer_claimant").then(normalizeComputerClaimant);
const cancelComputerClaimant = () => invoke<void>("enrollment_cancel_computer_claimant");
export const discardComputerClaimant = () => "__TAURI_INTERNALS__" in window
  ? invoke<void>("enrollment_discard_computer_claimant")
  : bridge.enrollment_cancel_computer();
export const renewHostedSession = () => "__TAURI_INTERNALS__" in window
  ? invoke<void>("hosted_session_renew")
  : Promise.resolve();
export const tauriBridge: DesktopBridge = {
  launch_progress: () => invoke("launch_progress"),
  subscribe_launch_progress: listener => subscribeWindowEvent("peppy://launch-progress", payload => listener(payload as LaunchProgress)),
  prepare_pasted_attachments: async files => {
    const views: AttachmentView[] = [];
    const failures: string[] = [];
    for (const file of files) {
      try {
        views.push(await invokeRaw<AttachmentView>("prepare_pasted_attachment", file.bytes, {
          "peppy-attachment-name": encodeURIComponent(file.name || "pasted-image"),
          "peppy-attachment-type": file.mediaType,
        }));
      } catch (error) {
        failures.push(`${file.name || "Pasted image"}: ${errorText(error)}`);
      }
    }
    return { views, failures };
  },
  resolve_draft_conflict: (input) => invoke("resolve_draft_conflict", { draftId: input.draftId, keepBranchId: input.keepBranchId }),
  acknowledge_draft_notice: (draftId) => invoke("acknowledge_draft_notice", { draftId }),
  get_notification_sound: () => invoke("get_notification_sound"),
  get_notification_persistence: () => invoke("get_notification_persistence"),
  startup_status: () => invoke("startup_status"),
  set_open_in_background: enabled => invoke("set_open_in_background", { enabled }),
  open_login_items: () => invoke("open_login_items"),
  hosted_delete_account: () => invoke("hosted_delete_account"),
  hosted_deletion_status: () => invoke("hosted_deletion_status"),
  hosted_cancel_account_deletion: () => invoke("hosted_cancel_account_deletion"),
  purchase_options: () => invoke("purchase_options"),
  store_products: () => invoke("store_products"),
  store_purchase: productId => invoke("store_purchase", { productId }),
  store_restore: () => invoke("store_restore"),
  open_store_subscriptions: () => invoke("open_store_subscriptions"),
  open_play_subscriptions: () => invoke("open_play_subscriptions"),
  set_notification_sound: enabled => invoke("set_notification_sound", { enabled }),
  set_notification_persistence: value => invoke("set_notification_persistence", { value }),
  get_notification_quick_reply: () => invoke("get_notification_quick_reply"),
  set_notification_quick_reply: enabled => invoke("set_notification_quick_reply", { enabled }),
  notification_reply_recoveries: conversationId => invoke("notification_reply_recoveries", { conversationId }),
  claim_notification_reply_restore: input => invoke("claim_notification_reply_restore", { input }),
  release_notification_reply_restore: input => invoke("release_notification_reply_restore", { input }),
  ack_notification_reply_restore: input => invoke("ack_notification_reply_restore", { input }),
  discard_notification_reply: input => invoke("discard_notification_reply", { input }),
  media_sync_request: attachmentIds => invoke("media_sync_request", { attachmentIds }),
  open_media_viewer: (conversationId, attachmentId, messageId) => invoke("open_media_viewer", { conversationId, attachmentId, messageId }),
  media_page_around: (conversationId, attachmentId, messageId, limit) => invoke("media_page_around", { conversationId, attachmentId, messageId, limit }),
  media_page_newer: (conversationId, cursor, limit) => invoke("media_page_newer", { conversationId, cursor, limit }),
  hydrate_media_preview: (messageId, id) => invoke("hydrate_media_preview", { messageId, id }),
  copy_attachment_image: id => invoke("copy_attachment_image", { id }),
  attachment_retention_status: () => invoke("attachment_retention_status"),
  account_access: () => invoke("account_access"),
  rename_device: (deviceId, name) => invoke("rename_device", { deviceId, name }),
  attachment_retention_state: attachmentId => invoke("attachment_retention_state", { attachmentId }),
  retryRcsMediaDownload: (messageId, attachmentIndex) => invoke("retry_rcs_media_download", { messageId, attachmentIndex }),
  load_state: (conversationId, historyFromMessageId, options) => invoke("load_state", { conversationId, historyFromMessageId, metadataOnly: options?.metadataOnly }), search_local: input => invoke("search_local", { input }), advance_search_index: input => invoke("advance_search_index", { input }), get_history_search: () => invoke("get_history_search"), start_history_search: ({ operationId }) => invoke("start_history_search", { input: { operationId } }), cancel_history_search: ({ attemptId }) => invoke("cancel_history_search", { input: { attemptId } }), load_message_window: input => invoke("load_message_window", { input }), open_external_url: url => invoke("open_external_url", { url }), load_history: (conversationId, beforeMessageId) => invoke("load_history", { conversationId, beforeMessageId }), configure_server: origin => invoke("configure_server", { origin }), unlock_sync: () => invoke("unlock_sync"), get_app_lock_status: () => invoke("get_app_lock_status"), retry_app_lock_check: () => invoke("retry_app_lock_check"), set_app_lock: enabled => invoke("set_app_lock", { enabled }), unlock_app: () => invoke("unlock_app"), save_draft: input => invoke("save_draft", { input }), discard_draft: input => invoke("discard_draft", { input }), send_draft: input => invoke("send_draft", { input }), mark_seen: visibleMessageIds => invoke("mark_seen", { visibleMessageIds }), copy_one_time_code: target => invoke("copy_one_time_code", { target }), mark_all_read: () => invoke("mark_all_read"), dismiss_notifications: targets => invoke("dismiss_notifications", { targets }), set_app_muted: (sourceDeviceId, packageName, appName, muted) => invoke("set_app_muted", { sourceDeviceId, packageName, appName, muted }), mark_notifications_seen: targets => invoke("mark_notifications_seen", { targets }), set_notification_preferences: preferences => invoke("set_notification_preferences", { preferences }), set_notification_context: (view, conversationId) => invoke("set_notification_context", { view, conversationId }), request_notification_permission: () => invoke("request_notification_permission"), open_notification_settings: () => invoke("open_notification_settings"), pick_attachments: () => invoke("pick_attachments"), retry_attachment: id => invoke("retry_attachment", { id }), retry_snapshot_import: async () => undefined, save_attachment: id => invoke("save_attachment", { id }), request_attachment_download: id => invoke("request_attachment_download", { id }), media_page: (conversationId, cursor, limit) => invoke("media_page", { conversationId, cursor, limit }), open_media: id => invoke("open_media", { id }), close_media: (id, url) => invoke("close_media", { id, url }), load_media: id => invoke("load_media", { id }), publish_attachment: id => invoke("publish_attachment", { id }), open_composer: conversationId => invoke("open_composer", { conversationId }), set_start_at_login: enabled => invoke("set_start_at_login", { enabled }), popout_conversation: conversationId => invoke("popout_conversation", { conversationId }), hide_head: conversationId => invoke("hide_head", { conversationId }), close_composer: () => invoke("close_composer"), close_head_panel: () => invoke("close_head_panel"), set_maximize_button_region: region => invoke("set_maximize_button_region", { region }), subscribe: listener => subscribeEvent("peppy://state", () => listener(), listener), subscribe_maximize_button_hover: listener => subscribeWindowEvent("peppy://maximize-button-hover", payload => { if (typeof payload === "boolean") listener(payload); }), subscribe_drafts: listener => subscribeWindowEvent("peppy://draft", payload => { if (isDraft(payload)) listener(payload); }), subscribe_typing: listener => subscribeWindowEvent("peppy://typing", payload => { if (isTypingSignal(payload)) listener(payload); }), subscribe_lifecycle: listener => subscribeWindowEvent("peppy://lifecycle-request", (payload: unknown) => { if (isLifecycleRequest(payload)) listener(payload); }), subscribe_lifecycle_finished: listener => subscribeWindowEvent("peppy://lifecycle-finished", (payload: unknown) => { if (isLifecycleFinished(payload)) listener(payload); }), acknowledge_lifecycle: (id, ok, reason) => invoke("acknowledge_lifecycle", { id, ok, reason }),
   list_devices: () => invoke("list_devices"), revoke_device: deviceId => invoke("revoke_device", { deviceId }), list_contact_books: () => invoke("list_contact_books"), forget_contact_book: bookId => invoke("forget_contact_book", { bookId }), delete_contact_book: bookId => invoke("delete_contact_book", { bookId }), set_contact_book_lookup: (bookId, enabled) => invoke("set_contact_book_lookup", { bookId, enabled }), list_contact_name_candidates: (address, sourceDeviceId) => invoke("list_contact_name_candidates", { address, sourceDeviceId }), set_contact_name_pin: (address, bookId, contactId, sourceDeviceId) => invoke("set_contact_name_pin", { address, sourceDeviceId, bookId, contactId }), clear_contact_name_pin: (address, sourceDeviceId) => invoke("clear_contact_name_pin", { address, sourceDeviceId }), list_contacts: (bookId, query, offset, sort) => invoke("list_contacts", { bookId, query, offset, sort }), list_contacts_page: (bookId, request) => invoke("list_contacts_page", { bookId, request }), submit_contact_edit: input => invoke("submit_contact_edit", { input }), list_contact_edits: bookId => invoke("list_contact_edits", { bookId }), search_contact_recipients: (query, sourceDeviceId) => invoke("search_contact_recipients", { query, sourceDeviceId }), request_contact_repair: () => invoke("request_contact_repair"), request_contact_sync: bookId => invoke("request_contact_sync", { bookId }), get_contact_sync_request: bookId => invoke("get_contact_sync_request", { bookId }), pick_contact_photo: () => invoke("pick_contact_photo"), list_restorable_contacts: bookId => invoke("list_restorable_contacts", { bookId }), restore_contact: (bookId, contactId) => invoke("restore_contact", { bookId, contactId }), hosted_account: () => invoke("hosted_account"), hosted_origin_url: () => invoke("hosted_origin_url"), hosted_sign_in: provider => invoke("hosted_sign_in", { provider }), hosted_sign_out: () => invoke("hosted_sign_out"), hosted_open_billing: () => invoke("hosted_open_billing"), hosted_provision: () => invoke("hosted_provision"), billing_summary: () => invoke("billing_summary"), enrollment_discover: discoverEnrollment, enrollment_start_over: () => invoke("enrollment_start_over"), enrollment_create_owner: input => invoke("enrollment_create_owner", { input }), enrollment_sign_in: input => invoke("enrollment_sign_in", { input }), enrollment_connect_shared_secret: input => invoke("enrollment_connect_shared_secret", { input }), enrollment_begin_vault_request: input => invoke("enrollment_begin_vault_request", { input }), enrollment_vault_request_status: () => invoke("enrollment_vault_request_status"), enrollment_create_vault: input => invoke("enrollment_create_vault", { input }), enrollment_add_phone: method => invoke("enrollment_add_phone", { method }), enrollment_rotate_phone: method => invoke("enrollment_rotate_phone", { method }), enrollment_cancel_phone: () => invoke("enrollment_cancel_phone"), reveal_vault_secret: () => invoke("reveal_vault_secret"), enrollment_status: () => invoke("enrollment_status"), enrollment_approve: () => invoke("enrollment_approve"), enrollment_cancel: () => invoke("enrollment_cancel"), enrollment_sign_out: () => invoke("enrollment_sign_out"),
  open_mcp_oauth_redirect: url => invoke("open_mcp_oauth_redirect", { url }),
  mcp_status: () => invoke("mcp_status"),
  mcp_enable: () => invoke("mcp_enable"),
  mcp_grants: () => invoke("mcp_grants"),
  mcp_revoke_grant: id => invoke("mcp_revoke_grant", { id }),
  webhooks_list: () => invoke("webhooks_list"),
  webhook_create: input => invoke("webhook_create", { input }),
  webhook_update: (id, input) => invoke("webhook_update", { id, input }),
  webhook_delete: id => invoke("webhook_delete", { id }),
  webhook_reveal_secret: id => invoke("webhook_reveal_secret", { id }),
  webhook_rotate_secret: id => invoke("webhook_rotate_secret", { id }),
  webhook_test: id => invoke("webhook_test", { id }),
  mcp_authorization: id => invoke("mcp_authorization", { id }),
  mcp_approve_authorization: (id, scopes) => invoke("mcp_approve_authorization", { id, scopes }),
  mcp_deny_authorization: id => invoke("mcp_deny_authorization", { id }),
  enrollment_connect_computer: beginComputerClaimant,
  enrollment_computer_status: computerClaimantStatus,
  enrollment_confirm_computer: confirmComputerClaimant,
  enrollment_cancel_computer: cancelComputerClaimant,
  set_conversation_pinned: (conversationId, pinned) => invoke("set_conversation_pinned", { conversationId, pinned }),
  set_conversation_folder: (conversationId, folder) => invoke("set_conversation_folder", { conversationId, folder }),
  request_sender_unblock: conversationId => invoke("request_sender_unblock", { conversationId }),
  send_typing: ({ conversationId, state }) => invoke("send_typing", { conversationId, state }),
  set_typing_sharing: ({ enabled }) => invoke("set_typing_sharing", { enabled }),
  get_diagnostics_state: () => invoke<DiagnosticsState>("get_diagnostics_state"),
  set_diagnostics_enabled: ({ enabled }) => invoke<DiagnosticsState>("set_diagnostics_enabled", { enabled }),
  export_diagnostics_log: () => invoke<DiagnosticsExportResult>("export_diagnostics_log"),
  clear_diagnostics_log: () => invoke<DiagnosticsState>("clear_diagnostics_log"),
  set_conversation_tone: (seed, paletteIndex) => invoke("set_conversation_tone", { seed, paletteIndex }),
  cancel_send: ({ messageId }) => invoke("cancel_send", { messageId }),
  send_again: input => invoke("send_again", { input }),
  react_to_message: ({ messageId, emoji }) => invoke("react_to_message", { messageId, emoji }),
  dismiss_reaction_request: ({ requestId }) => invoke("dismiss_reaction_request", { requestId }),
  open_emoji_picker: () => invoke("open_emoji_picker"),
  async window(action) { const w = (await import("@tauri-apps/api/window")).getCurrentWindow(); if (action === "minimize") await w.minimize(); else if (action === "maximize") await w.toggleMaximize(); else await w.close(); },
};

const isLifecycleRequest = (value: unknown): value is { id: string; action: "quit" | "close" | "collapse" } => typeof value === "object" && value !== null && typeof (value as { id?: unknown }).id === "string" && ["quit", "close", "collapse"].includes((value as { action?: unknown }).action as string);
const isLifecycleFinished = (value: unknown): value is { id: string; ok: boolean } => typeof value === "object" && value !== null && typeof (value as { id?: unknown }).id === "string" && typeof (value as { ok?: unknown }).ok === "boolean";
const MAX_U64 = 18446744073709551615n;
const isCanonicalU64 = (value: unknown): value is string => typeof value === "string" && /^(0|[1-9]\d*)$/.test(value) && BigInt(value) <= MAX_U64;
const isStringArray = (value: unknown): value is string[] => Array.isArray(value) && value.every(item => typeof item === "string");
const isDraft = (value: unknown): value is Draft => {
  if (typeof value !== "object" || value === null) return false;
  const draft = value as Partial<Draft>;
  return typeof draft.id === "string" && typeof draft.conversationId === "string" &&
    typeof draft.text === "string" && isStringArray(draft.recipientIds) &&
    isStringArray(draft.attachmentIds) && isCanonicalU64(draft.revision) &&
    (draft.gatewayId === undefined || typeof draft.gatewayId === "string") &&
    (draft.simId === undefined || typeof draft.simId === "string");
};
const isTypingSignal = (value: unknown): value is TypingSignal => {
  if (typeof value !== "object" || value === null || "origin" in value) return false;
  const signal = value as Partial<TypingSignal>;
  return typeof signal.conversationId === "string" &&
    (signal.state === "started" || signal.state === "stopped") &&
    (signal.expiresAtMs === undefined || isCanonicalU64(signal.expiresAtMs)) &&
    (signal.participant === undefined || typeof signal.participant === "string");
};
const subscribeEvent = (event: string, listener: (payload: unknown) => void, onRegistered?: () => void) => {
  let disposed = false;
  let unlisten: (() => void) | undefined;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  let retryDelay = 1000;
  const maxRetryDelay = 30_000;
  const register = () => {
    void import("@tauri-apps/api/event")
      .then(({ listen }) => listen(event, payload => listener(payload.payload)))
      .then(stop => {
        unlisten = stop;
        if (disposed) { stop(); return; }
        // A catch-up callback failure must not trigger a registration retry.
        try { onRegistered?.(); } catch { /* Listener errors do not affect registration. */ }
      }, () => {
        if (disposed) return;
        retryTimer = setTimeout(register, retryDelay);
        retryDelay = Math.min(retryDelay * 2, maxRetryDelay);
      }).catch(() => {});
  };
  register();
  return () => { disposed = true; clearTimeout(retryTimer); unlisten?.(); };
};
type WindowSubscription = (() => void) & { ready: Promise<void> };
const subscribeWindowEvent = (event: string, listener: (payload: unknown) => void): WindowSubscription => {
  let disposed = false;
  let unlisten: (() => void) | undefined;
  const ready = import("@tauri-apps/api/webviewWindow")
    .then(({ getCurrentWebviewWindow }) => getCurrentWebviewWindow().listen(event, payload => listener(payload.payload)))
    .then(stop => { unlisten = stop; if (disposed) stop(); });
  // Legacy callers ignore readiness; the attached handler keeps registration failures from surfacing as unhandled rejections.
  ready.catch(() => {});
  return Object.assign(() => { disposed = true; unlisten?.(); }, { ready });
};
const missingHost = (method: keyof DesktopBridge) => async () => { throw { code: "missing-native-host", message: `Native host is required for ${String(method)}.` } satisfies BridgeError; };
// Hosts without native diagnostics omit all four methods so the settings section is absent.
export const DIAGNOSTICS_METHOD_NAMES: ReadonlySet<string> = new Set(["get_diagnostics_state", "set_diagnostics_enabled", "export_diagnostics_log", "clear_diagnostics_log"]);
export const MISSING_HOST_METHOD_NAMES = [
  "load_state",
  "search_local",
  "advance_search_index",
  "get_history_search",
  "start_history_search",
  "cancel_history_search",
  "load_message_window",
  "cancel_send",
  "send_again",
  "react_to_message",
  "dismiss_reaction_request",
  "open_external_url",
  "open_mcp_oauth_redirect",
  "load_history",
  "configure_server",
  "unlock_sync",
  "get_app_lock_status",
  "retry_app_lock_check",
  "set_app_lock",
  "unlock_app",
  "save_draft",
  "discard_draft",
  "send_draft",
  "mark_seen",
  "copy_one_time_code",
  "mark_all_read",
  "set_conversation_pinned",
  "set_conversation_folder",
  "request_sender_unblock",
  "set_conversation_tone",
  "dismiss_notifications",
  "set_app_muted",
  "mark_notifications_seen",
  "set_notification_preferences",
  "set_notification_context",
  "request_notification_permission",
  "open_notification_settings",
  "pick_attachments",
  "prepare_pasted_attachments",
  "resolve_draft_conflict",
  "acknowledge_draft_notice",
  "retry_attachment",
  "retry_snapshot_import",
  "retryRcsMediaDownload",
  "save_attachment",
  "request_attachment_download",
  "media_sync_request",
  "media_page",
  "media_page_around",
  "media_page_newer",
  "open_media",
  "close_media",
  "load_media",
  "publish_attachment",
  "open_composer",
  "set_start_at_login",
  "startup_status",
  "set_open_in_background",
  "open_login_items",
  "get_notification_sound",
  "set_notification_sound",
  "get_notification_persistence",
  "set_notification_persistence",
  "get_notification_quick_reply",
  "set_notification_quick_reply",
  "notification_reply_recoveries",
  "claim_notification_reply_restore",
  "release_notification_reply_restore",
  "ack_notification_reply_restore",
  "discard_notification_reply",
  "popout_conversation",
  "hide_head",
  "close_composer",
  "close_head_panel",
  "set_maximize_button_region",
  "subscribe",
  "subscribe_maximize_button_hover",
  "subscribe_drafts",
  "send_typing",
  "set_typing_sharing",
  "subscribe_typing",
  "subscribe_lifecycle",
  "subscribe_lifecycle_finished",
  "acknowledge_lifecycle",
  "window",
  "list_devices",
  "rename_device",
  "revoke_device",
  "mcp_status",
  "mcp_enable",
  "mcp_grants",
  "mcp_revoke_grant",
  "webhooks_list",
  "webhook_create",
  "webhook_update",
  "webhook_delete",
  "webhook_reveal_secret",
  "webhook_rotate_secret",
  "webhook_test",
  "mcp_authorization",
  "mcp_approve_authorization",
  "mcp_deny_authorization",
  "list_contact_books",
  "forget_contact_book",
  "delete_contact_book",
  "set_contact_book_lookup",
  "list_contact_name_candidates",
  "set_contact_name_pin",
  "clear_contact_name_pin",
  "list_contacts",
  "list_contacts_page",
  "submit_contact_edit",
  "list_contact_edits",
  "search_contact_recipients",
  "request_contact_repair",
  "request_contact_sync",
  "get_contact_sync_request",
  "pick_contact_photo",
  "list_restorable_contacts",
  "restore_contact",
  "hosted_account",
  "attachment_retention_status",
  "account_access",
  "attachment_retention_state",
  "hosted_origin_url",
  "hosted_sign_in",
  "hosted_sign_out",
  "hosted_delete_account",
  "hosted_deletion_status",
  "hosted_cancel_account_deletion",
  "hosted_open_billing",
  "hosted_provision",
  "billing_summary",
  "purchase_options",
  "store_products",
  "store_purchase",
  "store_restore",
  "open_store_subscriptions",
  "open_play_subscriptions",
  "enrollment_discover",
  "enrollment_start_over",
  "enrollment_create_owner",
  "enrollment_sign_in",
  "enrollment_connect_shared_secret",
  "enrollment_begin_vault_request",
  "enrollment_vault_request_status",
  "enrollment_create_vault",
  "enrollment_add_phone",
  "enrollment_rotate_phone",
  "enrollment_cancel_phone",
  "reveal_vault_secret",
  "enrollment_status",
  "enrollment_approve",
  "enrollment_cancel",
  "enrollment_sign_out",
  "enrollment_connect_computer",
  "enrollment_computer_status",
  "enrollment_confirm_computer",
  "enrollment_cancel_computer",
] as const satisfies readonly (keyof DesktopBridge)[];
export const missingHostBridge: DesktopBridge = Object.fromEntries(MISSING_HOST_METHOD_NAMES.map(key => [key, key === "subscribe" || key === "subscribe_maximize_button_hover" || key === "subscribe_drafts" || key === "subscribe_typing" || key === "subscribe_lifecycle" || key === "subscribe_lifecycle_finished" ? (() => () => undefined) : missingHost(key)])) as unknown as DesktopBridge;
/** Fixtures are development/test-only and are never chosen after a native error. */
export let bridge: DesktopBridge = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window ? tauriBridge : (import.meta.env.DEV || import.meta.env.MODE === "test" ? fixtureBridge : missingHostBridge);

/** Installs a non-native host before the shared UI is rendered. */
export function installBrowserBridge(browserBridge: DesktopBridge): void {
  bridge = browserBridge;
}
