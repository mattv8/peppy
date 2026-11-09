import { IndexedDbCheckpointStore } from "./checkpoint.js";
import { BrowserCore, CoreRejectedError, type CoreResult, type EmscriptenCoreModule } from "./core.js";
import { type EmscriptenFilesystem } from "./filesystem.js";
import { acquireWorkerOwner, type OwnerLockManager, type WorkerOwner } from "./owner.js";
import { BrowserSession, BrowserSessionError, type BrowserSessionPhase } from "./session.js";
import { BrowserEnrollment, type EnrollmentView, type InstanceManagement, type VaultInviteView, type VaultRequestView, type VaultSummary } from "./enrollment.js";
import { BrowserScheduler, type SchedulerSyncProgress } from "./scheduler.js";
import { OriginTransport, TransportError } from "./transport.js";
import { BannerPresenter, BrowserNotificationAdapter, CoreNotificationSettings, DEFAULT_NOTIFICATION_PREFERENCES, IndexedDbNotificationSettings, notificationPreferences, type BannerContext, type NotificationAdapter, type NotificationPreferences, type NotificationSettings } from "./presentation.js";
import { publishAttachment, type PublicCopyTransport } from "./publication.js";
import { RememberedUnlock } from "./remembered-unlock.js";
import { AttachmentRetentionDriver, AttachmentRetentionStatusFacade } from "./attachment-retention.js";
import { AccountAccessDriver } from "./account-access.js";
import { LiveSupervisor } from "./live.js";
import { DraftTransport } from "./drafts.js";
import type { AccountAccessResult } from "@peppy/generated/account-access";
import type { AttachmentRetentionStatusResult } from "@peppy/generated/attachment-retention";
import type { McpAuthorizationDecision, McpAuthorizationView, McpDeviceStatus, McpGrantView, McpScope, WebhookCreated, WebhookEvent, WebhookFilterEntry, WebhookFilterMode, WebhookInput, WebhookList, WebhookSecret, WebhookTestResult, WebhookView } from "@peppy/generated/mcp";

const MAX_RPC_BYTES = 1024 * 1024;
const MAX_CONTACT_ADDRESSES = 500;
const MAX_PREVIEWS_PER_SNAPSHOT = 32;
const MAX_PREVIEWS_PER_HISTORY_PAGE = 12;
const MAX_ATTACHMENT_RETENTION_STATES_PER_PROJECTION = 100;
const MAX_INLINE_MEDIA_BYTES = 32 * 1024 * 1024;
const MAX_MEDIA_SYNC_ATTACHMENTS = 32;
const MAX_WEBHOOK_INPUT_BYTES = 64 * 1024;
const MAX_WEBHOOK_FILTER_ENTRIES = 50;
const MAX_WEBHOOK_URL_BYTES = 2048;
const MAX_WEBHOOK_NAME_BYTES = 320;
const MAX_WEBHOOK_ADDRESS_BYTES = 64;
const MAX_WEBHOOK_FIELD_BYTES = 128;
const WEBHOOK_INPUT_KEYS = ["name", "url", "events", "filterMode", "filter", "paused"];
const WEBHOOK_FILTER_ENTRY_KEYS = ["address", "label", "contactId", "bookId"];
const WEBHOOK_EVENTS: readonly WebhookEvent[] = ["message.received"];
const WEBHOOK_FILTER_MODES: readonly WebhookFilterMode[] = ["all", "only", "except"];
const OWNER_NAME = "peppy-browser-v1";
const SEARCH_INDEX_DELAY_MS = 1_000;
const SEARCH_INDEX_MAX_RETRIES = 3;
const SEARCH_INDEX_IDLE_RETRY_MS = 100;
const REMEMBERED_UNLOCK_VALIDATION_INTERVAL_MS = 60_000;
const DRAFT_SYNC_DEBOUNCE_MS = 2_000;
const RECOVERY_CLEAR_ERROR: RpcError = { code: "recovery-clear-failed", message: "Browser recovery could not be forgotten." };

export type RpcError = { code: string; message: string; conversationId?: string; currentRevision?: string; draftId?: string };
export type RpcResponse = { id: number; ok: true; value: unknown } | { id: number; ok: false; error: RpcError };
export type RpcRequest = { id: number; command: string; args: Record<string, unknown> };
export type BrowserDispatchContext = { sessionScopeId?: string };
export type WorkerMessage = RpcRequest | { event: "changed" } | { event: "ready"; version: 1 } | { event: "stopped"; reason: string } | { event: "banner"; candidate: Record<string, unknown> };

export interface BrowserPort {
  postMessage(message: WorkerMessage | RpcResponse): void;
  start?(): void;
  close(): void;
  onmessage: ((event: MessageEvent<unknown>) => void) | null;
}

type PresentationContext = { context: BannerContext; sequence: number };

export interface HostSession {
  readonly phase: BrowserSessionPhase;
  query(command: string, args: Record<string, unknown>): Promise<CoreResult>;
  mutate(command: string, args: Record<string, unknown>): Promise<CoreResult>;
  unlock(passphrase: string, signal?: AbortSignal, options?: { remember?: boolean }): Promise<void>;
  enroll(metadata: unknown, deviceToken: string, passphrase: string): Promise<void>;
  clearRememberedUnlock?(): Promise<void>;
  validateRememberedUnlock?(): Promise<void>;
  shutdown(): void | Promise<void>;
  drain?(): Promise<void>;
  tokenForTransport?(): string;
  stageInput?(bytes: Uint8Array): Promise<string>;
  readWorkerOutput?(id: string, expectedBytes: number): Promise<Uint8Array>;
  exportAttachment?(id: string): Promise<Record<string, unknown> & { bytes: Uint8Array }>;
  openMediaAttachment?(id: string, maxBytes: number): Promise<Record<string, unknown> & { bytes: Uint8Array }>;
  prepareAttachment?(bytes: Uint8Array, displayName: string, mediaType: string): Promise<CoreResult>;
  publicImage?(id: string): Promise<Record<string, unknown> & { bytes: Uint8Array; name: string }>;
  previewAttachment?(id: string): Promise<string | undefined>;
  cleanupStagedDownload?(source: string): Promise<void>;
  readonly generation?: number;
}

export interface BrowserNetwork {
  start(): Promise<boolean | void>;
  stop(): Promise<void>;
  enrollmentDiscover(origin?: string): Promise<EnrollmentView>;
  enrollmentCreateOwner(secrets: { setupToken?: string; adminPassword?: string; passphrase: string }, username?: string): Promise<EnrollmentView>;
  enrollmentSignIn(adminPassword: string, username?: string): Promise<EnrollmentView>;
  enrollmentSetHostedCsrf(csrf: string): Promise<void>;
  enrollmentConnectSharedSecret(passphrase: string, vaultOwner?: string): Promise<EnrollmentView>;
  enrollmentBeginVaultRequest(inviteLink: string, displayName: string): Promise<EnrollmentView>;
  enrollmentVaultRequestStatus(): Promise<EnrollmentView>;
  enrollmentCreateVault(username: string, secrets: { adminPassword: string; passphrase: string }): Promise<EnrollmentView>;
  enrollmentPendingVaultRequest(): Promise<{ origin: string; requestId: string; displayName: string } | null>;
  enrollmentAddPhone(method: "transfer" | "shared_secret"): Promise<EnrollmentView>;
  enrollmentConnectComputer(): Promise<EnrollmentView>;
  enrollmentComputerStatus(): Promise<EnrollmentView>;
  enrollmentConfirmComputer(): Promise<EnrollmentView>;
  enrollmentCancelComputer(): Promise<EnrollmentView>;
  enrollmentStatus(): Promise<EnrollmentView>;
  enrollmentApprove(): Promise<EnrollmentView>;
  enrollmentCancel(options?: { waitingOnly?: boolean }): Promise<EnrollmentView | undefined>;
  enrollmentSignOut(): Promise<void>;
  enrollmentGenerateSecret(): Promise<string>;
  enrollmentManagement(): Promise<InstanceManagement>;
  enrollmentRevokeDevice(deviceId: string): Promise<InstanceManagement>;
  enrollmentListVaults(): Promise<VaultSummary[]>;
  enrollmentCreateInvite(): Promise<{ inviteId: string; inviteLink: string; expiresInSeconds: number }>;
  enrollmentListInvites(): Promise<VaultInviteView[]>;
  enrollmentRevokeInvite(inviteId: string): Promise<void>;
  enrollmentListVaultRequests(): Promise<VaultRequestView[]>;
  enrollmentApproveVaultRequest(requestId: string): Promise<void>;
  enrollmentDenyVaultRequest(requestId: string): Promise<void>;
  listDevices(): Promise<unknown>;
  revokeDevice(deviceId: string): Promise<void>;
  mcpStatus(): Promise<McpDeviceStatus>;
  mcpEnable(): Promise<McpDeviceStatus>;
  mcpGrants(): Promise<McpGrantView[]>;
  mcpRevokeGrant(id: string): Promise<void>;
  webhooksList(): Promise<WebhookList>;
  webhookCreate(input: WebhookInput): Promise<WebhookCreated>;
  webhookUpdate(id: string, input: WebhookInput): Promise<WebhookView>;
  webhookDelete(id: string): Promise<void>;
  webhookRevealSecret(id: string): Promise<WebhookSecret>;
  webhookRotateSecret(id: string): Promise<WebhookSecret>;
  webhookTest(id: string): Promise<WebhookTestResult>;
  mcpAuthorization(id: string): Promise<McpAuthorizationView>;
  mcpApproveAuthorization(id: string, scopes: McpScope[]): Promise<McpAuthorizationDecision>;
  mcpDenyAuthorization(id: string): Promise<McpAuthorizationDecision>;
  subscribeChanges?(listener: () => void): () => void;
  retryAttachment?(id: string): void;
  wake?(): void;
  startHistorySearch?(attemptId: string): void;
  stopHistorySearch?(attemptId: string): void;
  unlockNewEpoch?(passphrase: string): Promise<void>;
  epochStatus?(): "epoch-mismatch" | "needs-unlock" | undefined;
  syncProgress?(): SchedulerSyncProgress | undefined;
  producerReady?(): boolean;
  attachmentRetentionStatus?(): Promise<AttachmentRetentionStatusResult>;
  accountAccess?(): Promise<AccountAccessResult>;
  sendDraft?(args: Record<string, unknown>): Promise<unknown>;
  draftSaved?(): void;
  flushDraftSync?(): void;
}

export interface BrowserHostDependencies {
  boot(signal: AbortSignal, onFatal: (error: Error) => void): Promise<HostSession>;
  locks?: OwnerLockManager;
  terminate?(): void;
  network?: BrowserNetwork;
  legacySettings?: LegacyNotificationSettings;
  publisher?: PublicCopyTransport & { origin: string };
  notification?: NotificationAdapter;
}

export class DraftSyncPrompt {
  private timer?: ReturnType<typeof setTimeout>;
  private pending = false;

  public constructor(
    private readonly canRun: () => boolean,
    private readonly run: () => void,
  ) {}

  public schedule(): void {
    this.pending = true;
    this.cancel();
    this.timer = setTimeout(() => {
      this.timer = undefined;
      this.runPending();
    }, DRAFT_SYNC_DEBOUNCE_MS);
  }

  public flush(): void {
    if (!this.pending) return;
    this.cancel();
    this.runPending();
  }

  public stop(): void {
    this.cancel();
    this.pending = false;
  }

  private runPending(): void {
    if (!this.pending) return;
    this.pending = false;
    if (this.canRun()) this.run();
  }

  private cancel(): void {
    if (this.timer !== undefined) clearTimeout(this.timer);
    this.timer = undefined;
  }
}

type LegacyNotificationSettings = {
  claimForVault(vaultId: string): Promise<NotificationPreferences | undefined>;
  clear(): Promise<void>;
};

type Invocation = { coreCommand: string; mutation: boolean };

const INVOCATIONS: Readonly<Record<string, Invocation>> = {
  load_state: { coreCommand: "snapshot", mutation: false },
  load_history: { coreCommand: "load_history", mutation: false },
  save_draft: { coreCommand: "save_draft", mutation: true },
  send_draft: { coreCommand: "send_draft", mutation: true },
  discard_draft: { coreCommand: "discard_draft", mutation: true },
  resolve_draft_conflict: { coreCommand: "resolve_draft_conflict", mutation: true },
  acknowledge_draft_notice: { coreCommand: "acknowledge_draft_notice", mutation: true },
  mark_seen: { coreCommand: "mark_seen", mutation: true },
  unread_message_batches: { coreCommand: "unread_message_batches", mutation: false },
  mark_read_batch: { coreCommand: "mark_read_batch", mutation: true },
  evict_attachment_ciphertext: { coreCommand: "evict_attachment_ciphertext", mutation: true },
  dismiss_notification: { coreCommand: "dismiss_notification", mutation: true },
  dismiss_all_notifications: { coreCommand: "dismiss_all_notifications", mutation: true },
  set_app_muted: { coreCommand: "set_app_muted", mutation: true },
  set_conversation_pinned: { coreCommand: "set_conversation_pinned", mutation: true },
  request_sender_unblock: { coreCommand: "request_sender_unblock", mutation: true },
  set_conversation_folder: { coreCommand: "set_conversation_folder", mutation: true },
  set_conversation_tone: { coreCommand: "set_conversation_tone", mutation: true },
  mark_notifications_seen: { coreCommand: "mark_notifications_seen", mutation: true },
  set_notification_preferences: { coreCommand: "set_notification_preferences", mutation: true },
  list_contact_books: { coreCommand: "list_contact_books", mutation: false },
  forget_contact_book: { coreCommand: "forget_contact_book", mutation: true },
  list_contacts: { coreCommand: "list_contacts", mutation: false },
  submit_contact_edit: { coreCommand: "submit_contact_edit", mutation: true },
  list_contact_edits: { coreCommand: "list_contact_edits", mutation: false },
  search_contact_recipients: { coreCommand: "search_contact_recipients", mutation: false },
  request_contact_repair: { coreCommand: "request_contact_repair", mutation: true },
  request_contact_sync: { coreCommand: "request_contact_sync", mutation: true },
  get_contact_sync_request: { coreCommand: "get_contact_sync_request", mutation: false },
  list_restorable_contacts: { coreCommand: "list_restorable_contacts", mutation: false },
  restore_contact: { coreCommand: "restore_contact", mutation: true },
  contact_snapshot: { coreCommand: "contact_snapshot", mutation: false },
  search_local: { coreCommand: "search_local", mutation: false },
  advance_search_index: { coreCommand: "advance_search_index", mutation: true },
  load_message_window: { coreCommand: "load_message_window", mutation: false },
  media_sync_request: { coreCommand: "media_sync_request", mutation: true },
};

const HISTORY_COMMANDS = new Set(["get_history_search", "start_history_search", "cancel_history_search"]);

const NATIVE_ONLY = new Set([
  "configure_server", "get_app_lock_status", "retry_app_lock_check", "set_app_lock", "unlock_app", "pick_attachments",
  "open_composer", "set_start_at_login", "popout_conversation", "hide_head",
  "close_composer", "close_head_panel", "window", "request_notification_permission", "open_notification_settings", "set_notification_context",
  "subscribe_lifecycle", "subscribe_lifecycle_finished", "acknowledge_lifecycle", "hosted_account",
  "hosted_sign_in", "hosted_sign_out", "hosted_open_billing", "hosted_provision",
]);

const ENROLLMENT_MUTATIONS = new Set([
  "enrollment_create_owner",
  "enrollment_sign_in",
  "enrollment_set_hosted_csrf",
  "enrollment_connect_shared_secret",
  "enrollment_add_phone",
  "enrollment_connect_computer",
  "enrollment_confirm_computer",
  "enrollment_cancel_computer",
  "enrollment_approve",
  "enrollment_cancel",
  "enrollment_sign_out",
  "enrollment_revoke_device",
  "enrollment_begin_vault_request",
  "enrollment_create_vault",
  "enrollment_revoke_invite",
  "enrollment_approve_vault_request",
  "enrollment_deny_vault_request",
]);

const ENROLLMENT_ACTIVATIONS = new Set([
  "enrollment_create_owner",
  "enrollment_connect_shared_secret",
  "enrollment_create_vault",
  "enrollment_confirm_computer",
]);

function isReadyEnrollmentView(value: unknown): boolean {
  return typeof value === "object" && value !== null && !Array.isArray(value) && (value as Record<string, unknown>).stage === "ready";
}

function isActivationView(command: string, value: unknown): boolean {
  return isReadyEnrollmentView(value)
    || command === "enrollment_confirm_computer"
      && typeof value === "object"
      && value !== null
      && !Array.isArray(value)
      && (value as Record<string, unknown>).stage === "computer_connected";
}

function isIndexing(value: unknown): boolean {
  return typeof value === "object" && value !== null && !Array.isArray(value) && (value as Record<string, unknown>).indexState === "indexing";
}

function isSearchIndexState(value: unknown): value is { indexState: "indexing" | "ready" } {
  return typeof value === "object" && value !== null && !Array.isArray(value) && ((value as Record<string, unknown>).indexState === "indexing" || (value as Record<string, unknown>).indexState === "ready");
}

/** Only handled core rejections and busy sessions retry; poisoned or corrupt runtime failures stop maintenance. */
function isTransientSearchIndexError(error: unknown): boolean {
  return error instanceof BrowserSessionError && error.code === "busy"
    || error instanceof CoreRejectedError && error.code === "unavailable";
}

function fatalDiagnostic(error: Error): "checkpoint-conflict" | "checkpoint-invalid" | "storage-unavailable" | "core-runtime-failure" {
  if (error instanceof CheckpointError) {
    if (error.code === "storage-conflict") return "checkpoint-conflict";
    if (error.code === "invalid-checkpoint") return "checkpoint-invalid";
    return "storage-unavailable";
  }
  // BrowserCore deliberately normalizes malformed ABI, poison, and WASM traps to BrowserCoreError.
  return "core-runtime-failure";
}

function safeSnapshot(phase: BrowserSessionPhase): Record<string, unknown> {
  return {
    version: "1", mode: "browser", connection: { state: "offline" },
    encryption: { state: phase === "unenrolled" ? "preview" : "locked" }, gateways: [], conversations: [],
    head: { enabled: false, capability: "unsupported" }, pendingCount: 0, quarantineCount: 0,
    notifications: [], appFilters: [], notificationPreferences: { messageBanners: true, mirroredBanners: true, preview: "full", muteWithHeads: true },
  };
}

function errorFor(error: unknown): RpcError {
  if (typeof error === "object" && error !== null && (error as { code?: unknown }).code === "draft-reconnect") {
    return { code: "unavailable", message: "Reconnect to check this draft before sending." };
  }
  if (typeof error === "object" && error !== null && (error as { code?: unknown }).code === "draft-not-synced") {
    return { code: "draft-not-synced", message: "This draft is still syncing. Wait for sync to finish, then send again.", ...errorDetails(error) };
  }
  if (typeof error === "object" && error !== null && typeof (error as { code?: unknown }).code === "string") {
    const code = (error as { code: string }).code;
    const terminalMessages: Record<string, string> = {
      draft_already_sent: "This draft was already sent from another device.",
      draft_discarded: "This draft was discarded on another device.",
      draft_expired: "This draft expired on another device.",
      draft_revision_not_head: "A newer draft version exists on another device.",
      draft_sent: "This draft was already sent from another device.",
    };
    if (terminalMessages[code]) return { code, message: terminalMessages[code], ...errorDetails(error) };
  }
  if (error instanceof TransportError) {
    return { code: error.kind === "offline" ? "offline" : "unavailable", message: "Browser operation failed." };
  }
  if (error instanceof BrowserSessionError) return { code: error.code, message: "Browser session is not ready." };
  if (typeof error === "object" && error !== null && "code" in error && typeof error.code === "string") {
    const details = errorDetails(error);
    const codes = new Set(["already-open", "attachment-invalid", "attachment-local", "busy", "core", "database-key-mismatch", "gateway-offline", "gateway-unavailable", "gateway-unsupported", "gateways-unknown", "invalid-request", "locked", "not-found", "request-too-large", "stale-draft", "draft-moved", "draft-not-discardable", "recipientless-draft-exists", "draft-merge-overflow", "unknown-command", "unlock-failed", "invalid-contact-edit", "contact-book-read-only", "contact-field-read-only", "contact-photo-invalid", "identity-exists", "identity-unavailable", "epoch-mismatch", "invalid-rotation", "unavailable", "invalid-draft", "invalid-route", "invalid-recipient", "invalid-attachment", "empty-message", "message-too-long", "mms-unsupported", "mms-too-many-recipients", "mms-too-large", "media-pending", "public-copy-unsupported", "public-copy-too-large", "host-state", "device-revoked", "unsupported", "http-401", "http-403", "invalid_setup_token", "instance_already_claimed", "setup_window_closed"]);
    const code = codes.has(error.code) ? error.code : "unavailable";
    return { code, message: "Browser operation failed.", ...details };
  }
  return { code: "unavailable", message: "Browser operation failed." };
}

function mcpErrorFor(error: unknown): RpcError {
  if (isMcpCancellation(error)) return { code: "cancelled", message: "MCP status check was cancelled. Try again." };
  if (error instanceof Error && error.message === "enrollment-timeout") return { code: "timeout", message: "MCP status check timed out. Try again." };
  if (error instanceof Error && error.message === "session-expired") return { code: "session-expired", message: "Your sign-in session expired. Sign in again." };
  const httpError = mcpHttpError(error);
  if (httpError) return httpError;
  if (error instanceof TypeError) return { code: "network", message: "Peppy could not connect to the MCP server. Try again." };
  if (error instanceof Error && isMcpLocalCode(error.message)) return { code: error.message, message: "The MCP operation could not be completed. Try again." };
  return errorFor(error);
}

function isMcpLocalCode(value: string): value is "owner_required" | "mcp-response" | "enrollment-state" {
  return value === "owner_required" || value === "mcp-response" || value === "enrollment-state";
}

function isMcpCancellation(error: unknown): boolean {
  return error instanceof Error && (error.name === "AbortError" || error.message === "enrollment-cancelled");
}

function mcpHttpError(error: unknown): RpcError | undefined {
  if (typeof error !== "object" || error === null || !("status" in error) || typeof error.status !== "number" || !Number.isInteger(error.status)) return undefined;
  const status = error.status;
  if (status < 100 || status > 599) return undefined;
  const code = "code" in error && isMcpServerCode(error.code) ? error.code : `http-${status}`;
  return { code, message: "The MCP server could not complete the request. Try again." };
}

function isMcpServerCode(value: unknown): value is string {
  return typeof value === "string" && /^[a-z][a-z0-9_-]{0,63}$/.test(value);
}

function deviceManagementError(error: unknown): RpcError {
  if (error instanceof TransportError) {
    if (error.kind === "revoked") {
      return { code: "invalid_bearer", message: "The server rejected this device credential." };
    }
    if (error.kind === "status" && error.code && ["owner_required", "last_active_owner", "device_already_revoked", "device_not_found", "invalid_bearer"].includes(error.code)) {
      return { code: error.code, message: "The server rejected the device request." };
    }
  }
  return errorFor(error);
}

function shouldForgetRememberedUnlock(error: unknown, request: RpcRequest, currentDeviceId: string | undefined): boolean {
  if (!(error instanceof TransportError)) return false;
  if (error.kind === "revoked" || error.code === "invalid_bearer") return true;
  return error.code === "device_not_found" && request.command === "revoke_device" && request.args.deviceId === currentDeviceId;
}

function errorDetails(error: object): Pick<RpcError, "conversationId" | "currentRevision" | "draftId"> {
  const details = "details" in error && typeof error.details === "object" && error.details !== null ? error.details : undefined;
  if (!details) return {};
  const values = details as Record<string, unknown>;
  return {
    ...(safeErrorId(values.conversationId) ? { conversationId: values.conversationId } : {}),
    ...(safeRevision(values.currentRevision) ? { currentRevision: values.currentRevision } : {}),
    ...(safeErrorId(values.draftId) ? { draftId: values.draftId } : {}),
  };
}

function safeErrorId(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(value);
}

function safeRevision(value: unknown): value is string {
  return typeof value === "string" && /^(0|[1-9][0-9]{0,19})$/.test(value) && BigInt(value) <= 18_446_744_073_709_551_615n;
}

function canonicalUuid(value: unknown): string | undefined {
  if (typeof value !== "string" || !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value)) return undefined;
  return value === "00000000-0000-0000-0000-000000000000" ? undefined : value;
}

function historyAttemptId(value: unknown): string | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
  return canonicalUuid((value as Record<string, unknown>).attemptId);
}

function requestId(value: unknown): number {
  return typeof value === "object" && value !== null && Number.isSafeInteger((value as { id?: unknown }).id) ? (value as { id: number }).id : -1;
}

function validRequest(value: unknown): value is RpcRequest {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const request = value as Partial<RpcRequest>;
  if (!Number.isSafeInteger(request.id) || typeof request.command !== "string" || request.command.length === 0 || request.command.length > 128 ||
    typeof request.args !== "object" || request.args === null || Array.isArray(request.args)) return false;
  if (!Object.prototype.hasOwnProperty.call(request, "id") || !Object.prototype.hasOwnProperty.call(request, "command") || !Object.prototype.hasOwnProperty.call(request, "args")) return false;
  try { return new TextEncoder().encode(JSON.stringify(value)).byteLength <= MAX_RPC_BYTES; } catch { return false; }
}

function secretString(args: Record<string, unknown>, key: string): string | undefined {
  const value = args[key];
  return typeof value === "string" && value.length > 0 && value.length <= 64 * 1024 ? value : undefined;
}

function coreArgs(command: string, args: Record<string, unknown>): Record<string, unknown> {
  const wrapper = command === "save_draft" || command === "send_draft" || command === "discard_draft" || command === "submit_contact_edit" ? "input"
    : command === "set_notification_preferences" ? "preferences" : undefined;
  if (command === "media_sync_request") return { attachmentIds: mediaSyncAttachmentIds(args) };
  if (!wrapper) return args;
  const value = args[wrapper];
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw { code: "invalid-request" };
  return value as Record<string, unknown>;
}

function mediaSyncAttachmentIds(args: Record<string, unknown>): string[] {
  const attachmentIds = args.attachmentIds;
  if (!Array.isArray(attachmentIds) || attachmentIds.length < 1 || attachmentIds.length > MAX_MEDIA_SYNC_ATTACHMENTS || attachmentIds.some(id => typeof id !== "string" || !/^[0-9a-f-]{36}$/.test(id))) throw { code: "invalid-request" };
  return attachmentIds;
}

/** Dispatches a renderer request without exposing a generic Rust command or worker-private APIs. */
type MediaLeases = {
  acquire(id: string): boolean;
  release(id: string): boolean;
};

export async function dispatchBrowserRpc(session: HostSession, request: unknown, legacySettings?: LegacyNotificationSettings, publisher?: PublicCopyTransport & { origin: string }, mediaLeases?: MediaLeases, syncProgress?: SchedulerSyncProgress, context?: BrowserDispatchContext): Promise<RpcResponse> {
  if (!validRequest(request)) return { id: requestId(request), ok: false, error: { code: "invalid-request", message: "The request is invalid." } };
  return dispatchValidatedBrowserRpc(session, request, legacySettings, publisher, mediaLeases, syncProgress, context);
}

async function dispatchValidatedBrowserRpc(session: HostSession, request: RpcRequest, legacySettings?: LegacyNotificationSettings, publisher?: PublicCopyTransport & { origin: string }, mediaLeases?: MediaLeases, syncProgress?: SchedulerSyncProgress, context?: BrowserDispatchContext): Promise<RpcResponse> {
  const { id, command, args } = request;
  try {
    if (command === "load_state" && session.phase !== "ready") return { id, ok: true, value: safeSnapshot(session.phase) };
    if (command === "load_history" && session.phase !== "ready") throw { code: "locked" };
    if (command === "load_state") {
      if (legacySettings) await adoptLegacyPreferences(session, legacySettings).catch(() => undefined);
      const snapshot = await session.query("snapshot", args);
      if (typeof snapshot !== "object" || snapshot === null || Array.isArray(snapshot)) throw { code: "invalid-request" };
      const metadataOnly = args.metadataOnly === true;
      const contacts = await session.query("contact_snapshot", { addresses: contactAddresses(snapshot as Record<string, unknown>) });
      if (typeof contacts !== "object" || contacts === null || Array.isArray(contacts)) throw { code: "invalid-request" };
      const enriched = metadataOnly
        ? { ...(snapshot as Record<string, unknown>), draft: await addDraftPreviews(session, (snapshot as Record<string, unknown>).draft, { remaining: MAX_PREVIEWS_PER_SNAPSHOT }) }
        : await addVisiblePreviews(session, snapshot as Record<string, unknown>, MAX_PREVIEWS_PER_SNAPSHOT);
      return { id, ok: true, value: { ...enriched, ...contacts as Record<string, unknown>, ...(syncProgress ? { syncProgress } : {}), ...(context?.sessionScopeId ? { sessionScopeId: context.sessionScopeId } : {}) } };
    }
    if (command === "load_history" || command === "load_message_window") {
      const page = await session.query(command, args);
      if (typeof page !== "object" || page === null || Array.isArray(page)) throw { code: "invalid-request" };
      return { id, ok: true, value: await addHistoryPreviews(session, page as Record<string, unknown>) };
    }
    if (command === "unlock") {
      const passphrase = secretString(args, "passphrase");
      if (!passphrase) throw { code: "invalid-request" };
      await session.unlock(passphrase, undefined, { remember: args.remember === true });
      return { id, ok: true, value: undefined };
    }
    if (command === "prepare_attachment") return { id, ok: true, value: await addAttachmentPreview(session, await prepareAttachment(session, args)) };
    if (command === "attachment_retention_state") {
      const attachmentId = requiredId({ id: args.attachmentId });
      return { id, ok: true, value: await session.query("attachment_retention_state", { attachmentId }) };
    }
    if (command === "media_page") {
      const page = await session.query("media_page", mediaPageArgs(args));
      return { id, ok: true, value: await addMediaPagePreviews(session, page) };
    }
    if (command === "media_page_around") {
      const page = await session.query("media_page_around", mediaPageAroundArgs(args));
      return { id, ok: true, value: await addAroundPreviews(session, page) };
    }
    if (command === "media_page_newer") {
      const page = await session.query("media_page_newer", mediaPageArgs(args));
      return { id, ok: true, value: await addMediaPagePreviews(session, page) };
    }
    if (command === "open_media") {
      const value = await openMedia(session, args);
      if (value.state === "ready" && mediaLeases && !mediaLeases.acquire(requiredId(args))) {
        await session.mutate("release_attachment_ciphertext", { id: requiredId(args) });
      }
      return { id, ok: true, value };
    }
    if (command === "request_attachment_download") {
      return { id, ok: true, value: { demanded: await demandAttachmentIfNeeded(session, requiredId(args)) } };
    }
    if (command === "load_media") {
      await demandAttachmentIfNeeded(session, requiredId(args));
      const value = await openMedia(session, args);
      if (value.state === "ready" && mediaLeases && !mediaLeases.acquire(requiredId(args))) {
        await session.mutate("release_attachment_ciphertext", { id: requiredId(args) });
      }
      return { id, ok: true, value };
    }
    if (command === "close_media") {
      const attachmentId = requiredId(args);
      if (!mediaLeases || mediaLeases.release(attachmentId)) await session.mutate("release_attachment_ciphertext", { id: attachmentId });
      return { id, ok: true, value: undefined };
    }
    if (command === "export_attachment") return { id, ok: true, value: await exportAttachment(session, args) };
    if (command === "save_attachment") {
      const attachmentId = requiredId(args);
      try {
        return { id, ok: true, value: await exportAttachment(session, args) };
      } catch (error: unknown) {
        if (!isMediaPending(error)) throw error;
        await demandAttachmentIfNeeded(session, attachmentId);
        throw { code: "media-pending" };
      }
    }
    if (command === "retry_attachment") { await session.query("attachment_info", { id: requiredId(args) }); return { id, ok: true, value: undefined }; }
    if (command === "publish_attachment") {
      if (!publisher) throw { code: "unavailable" };
      const value = await publishAttachment(session, publisher, args);
      return { id, ok: true, value: { ...value, url: new URL(value.url, publisher.origin).toString() } };
    }
    if (command === "set_notification_context") return { id, ok: true, value: undefined };
    if (NATIVE_ONLY.has(command)) return { id, ok: false, error: { code: "unsupported", message: "This action is unavailable in the browser." } };
    const invocation = Object.prototype.hasOwnProperty.call(INVOCATIONS, command) ? INVOCATIONS[command] : undefined;
    if (!invocation) return { id, ok: false, error: { code: "invalid-request", message: "The request is invalid." } };
    const invocationArgs = coreArgs(command, args);
    if (command === "set_notification_preferences") {
      const preferences = notificationPreferences(invocationArgs);
      const value = await session.mutate(invocation.coreCommand, preferences);
      return { id, ok: true, value };
    }
    const value = invocation.mutation
      ? await session.mutate(invocation.coreCommand, invocationArgs)
      : await session.query(invocation.coreCommand, invocationArgs);
    if (command === "save_draft") {
      return { id, ok: true, value: await addDraftPreviews(session, value, { remaining: MAX_PREVIEWS_PER_SNAPSHOT }) };
    }
    return { id, ok: true, value };
  } catch (error: unknown) {
    return { id, ok: false, error: errorFor(error) };
  }
}

async function adoptLegacyPreferences(session: HostSession, legacy: LegacyNotificationSettings): Promise<boolean> {
  const identity = object(await session.query("_worker_identity_metadata", {}));
  const vaultId = canonicalUuid(identity.vaultId);
  if (!vaultId) throw { code: "unavailable" };
  const preferences = await legacy.claimForVault(vaultId);
  if (!preferences) return false;
  if (sameNotificationPreferences(preferences, DEFAULT_NOTIFICATION_PREFERENCES)) {
    await legacy.clear().catch(() => undefined);
    return false;
  }
  const hasVaultValue = async (): Promise<boolean> => (await session.query("notification_preferences", {})) != null;
  let adopted = false;
  if (!(await hasVaultValue())) {
    const result = await session.mutate("adopt_legacy_notification_preferences", preferences) as { adopted?: boolean } | undefined;
    adopted = result?.adopted === true;
    if (!adopted && !(await hasVaultValue())) throw { code: "unavailable" };
  }
  await legacy.clear().catch(() => undefined);
  return adopted;
}

function sameNotificationPreferences(left: NotificationPreferences, right: NotificationPreferences): boolean {
  return left.messageBanners === right.messageBanners && left.mirroredBanners === right.mirroredBanners
    && left.preview === right.preview && left.showSource === right.showSource && left.muteWithHeads === right.muteWithHeads;
}

class MigratingCoreNotificationSettings implements NotificationSettings {
  private migration?: Promise<boolean>;

  public constructor(
    private readonly session: HostSession,
    private readonly legacy?: LegacyNotificationSettings,
    private readonly onAdopted?: () => void,
  ) {}

  public async load(): Promise<NotificationPreferences | undefined> {
    if (this.legacy) {
      this.migration ??= adoptLegacyPreferences(this.session, this.legacy);
      try {
        if (await this.migration) this.onAdopted?.();
      } finally {
        this.migration = undefined;
      }
    }
    return new CoreNotificationSettings(this.session).load();
  }
}

function contactAddresses(snapshot: Record<string, unknown>): Array<{ address: string; sourceDeviceId?: string }> {
  const addresses = new Map<string, { address: string; sourceDeviceId?: string }>();
  const add = (address: unknown, sourceDeviceId?: unknown) => {
    if (typeof address !== "string" || address.length === 0 || address.length > 512 || addresses.size >= MAX_CONTACT_ADDRESSES) return;
    const source = typeof sourceDeviceId === "string" && sourceDeviceId.length <= 512 ? sourceDeviceId : undefined;
    addresses.set(`${source ?? ""}\u0000${address}`, { address, ...(source ? { sourceDeviceId: source } : {}) });
  };
  if (Array.isArray(snapshot.conversations)) {
    for (const conversation of snapshot.conversations) {
      if (typeof conversation !== "object" || conversation === null || Array.isArray(conversation)) continue;
      const view = conversation as Record<string, unknown>;
      add(view.name);
      if (Array.isArray(view.participants)) view.participants.forEach(participant => add(participant));
    }
  }
  if (typeof snapshot.draft === "object" && snapshot.draft !== null && !Array.isArray(snapshot.draft)) {
    const draft = snapshot.draft as Record<string, unknown>;
    if (Array.isArray(draft.recipientIds)) draft.recipientIds.forEach(recipient => add(recipient, draft.gatewayId));
  }
  if (Array.isArray(snapshot.notifications)) {
    for (const notification of snapshot.notifications) {
      if (typeof notification !== "object" || notification === null || Array.isArray(notification)) continue;
      const value = notification as Record<string, unknown>;
      const target = typeof value.target === "object" && value.target !== null && !Array.isArray(value.target) ? value.target as Record<string, unknown> : undefined;
      add(value.title, target?.sourceDeviceId);
    }
  }
  return [...addresses.values()];
}

type PreviewBudget = { remaining: number };

async function addVisiblePreviews(session: HostSession, snapshot: Record<string, unknown>, limit: number): Promise<Record<string, unknown>> {
  const budget = { remaining: limit };
  const draft = await addDraftPreviews(session, snapshot.draft, budget);
  const enrichedSnapshot = draft === snapshot.draft ? snapshot : { ...snapshot, draft };
  if (!Array.isArray(snapshot.conversations)) return enrichedSnapshot;
  const activeId = typeof snapshot.activeConversationId === "string" ? snapshot.activeConversationId : undefined;
  const conversation = snapshot.conversations.find(value => typeof value === "object" && value !== null && (value as Record<string, unknown>).id === activeId);
  if (!conversation || typeof conversation !== "object" || conversation === null) return enrichedSnapshot;
  const view = conversation as Record<string, unknown>;
  if (!Array.isArray(view.messages)) return enrichedSnapshot;
  const messages = await addMessagePreviews(session, view.messages, budget);
  return { ...enrichedSnapshot, conversations: snapshot.conversations.map(value => value === conversation ? { ...view, messages } : value) };
}

async function addHistoryPreviews(session: HostSession, page: Record<string, unknown>): Promise<Record<string, unknown>> {
  if (!Array.isArray(page.messages)) return page;
  return { ...page, messages: await addMessagePreviews(session, page.messages, { remaining: MAX_PREVIEWS_PER_HISTORY_PAGE }) };
}

async function addDraftPreviews(session: HostSession, draft: unknown, budget: PreviewBudget): Promise<unknown> {
  const view = objectOrUndefined(draft);
  if (!view || !Array.isArray(view.attachments)) return draft;
  const attachments = await Promise.all(view.attachments.map(async attachment => {
    if (budget.remaining <= 0 || !previewableAttachment(attachment)) return attachment;
    budget.remaining -= 1;
    return addAttachmentPreview(session, attachment);
  }));
  return { ...view, attachments };
}

async function addMessagePreviews(session: HostSession, messages: unknown[], budget: PreviewBudget): Promise<unknown[]> {
  const enrichRetention = attachmentRetentionEnricher(session);
  const previewedMessages = await Promise.all(messages.map(async message => {
    if (typeof message !== "object" || message === null) return message;
    const messageView = message as Record<string, unknown>;
    if (!Array.isArray(messageView.attachments)) return message;
    const attachments = await Promise.all(messageView.attachments.map(async attachment => {
      const view = objectOrUndefined(attachment);
      if (!view) return attachment;
      const mediaMeta = objectOrUndefined(view.mediaMeta);
      const thumbnail = mediaMeta?.thumbnail;
      const previewTarget = previewableAttachment(thumbnail) ? thumbnail : attachment;
      const preview = budget.remaining > 0 && previewableAttachment(previewTarget);
      if (preview) budget.remaining -= 1;
      const enriched = await enrichRetention(attachment);
      if (!preview) return enriched;
      if (previewTarget === attachment) return addAttachmentPreview(session, enriched);
      const enrichedView = objectOrUndefined(enriched) ?? view;
      return { ...enrichedView, mediaMeta: { ...mediaMeta, thumbnail: await addAttachmentPreview(session, thumbnail) } };
    }));
    return { ...messageView, attachments };
  }));
  return previewedMessages;
}

function attachmentRetentionEnricher(session: HostSession): (attachment: unknown) => Promise<unknown> {
  let remaining = MAX_ATTACHMENT_RETENTION_STATES_PER_PROJECTION;
  const states = new Map<string, Promise<{ remoteState: "available" | "removed_by_storage_policy"; localAvailable: boolean } | undefined>>();
  return async attachment => {
    const view = objectOrUndefined(attachment);
    if (!view || typeof view.id !== "string" || !/^[0-9a-f-]{36}$/.test(view.id)) return attachment;
    let state = states.get(view.id);
    if (!state) {
      if (remaining === 0) return attachment;
      remaining -= 1;
      state = session.query("attachment_retention_state", { attachmentId: view.id })
        .then(attachmentRetentionProjection)
        .catch(() => undefined);
      states.set(view.id, state);
    }
    const projection = await state;
    return projection ? { ...view, ...projection } : attachment;
  };
}

function attachmentRetentionProjection(value: unknown): { remoteState: "available" | "removed_by_storage_policy"; localAvailable: boolean } | undefined {
  const state = objectOrUndefined(value);
  if (!state || (state.remote !== "available" && state.remote !== "removed_by_storage_policy") || typeof state.localAvailable !== "boolean") return undefined;
  return { remoteState: state.remote, localAvailable: state.localAvailable };
}

async function addAttachmentPreview(session: HostSession, attachment: unknown): Promise<unknown> {
  if (!session.previewAttachment || typeof attachment !== "object" || attachment === null || Array.isArray(attachment)) return attachment;
  const view = attachment as Record<string, unknown>;
  if (typeof view.id !== "string") return attachment;
  const previewUrl = await session.previewAttachment(view.id);
  return previewUrl ? { ...view, previewUrl } : view;
}

function previewableAttachment(attachment: unknown): boolean {
  if (typeof attachment !== "object" || attachment === null || Array.isArray(attachment)) return false;
  const view = attachment as Record<string, unknown>;
  return typeof view.id === "string" && typeof view.mediaType === "string" && /^image\/(jpeg|png|webp|gif)$/.test(view.mediaType) && (view.state === "uploading" || view.state === "ready");
}
function mediaPageArgs(args: Record<string, unknown>): Record<string, unknown> {
  const conversationId = args.conversationId;
  const requestedLimit = args.limit;
  const limit = requestedLimit === undefined ? 50 : requestedLimit;
  if (typeof conversationId !== "string" || !/^[0-9a-f-]{36}$/.test(conversationId) || typeof limit !== "number" || !Number.isSafeInteger(limit) || limit < 1 || limit > 100) throw { code: "invalid-request" };
  if (args.cursor !== undefined && (!isMediaCursor(args.cursor))) throw { code: "invalid-request" };
  return { conversationId, limit, ...(args.cursor === undefined ? {} : { cursor: args.cursor }) };
}
function mediaPageAroundArgs(args: Record<string, unknown>): Record<string, unknown> {
  const page = mediaPageArgs({ ...args, cursor: undefined });
  const attachmentId = args.attachmentId;
  const messageId = args.messageId;
  if (typeof attachmentId !== "string" || !/^[0-9a-f-]{36}$/.test(attachmentId) || (messageId !== undefined && (typeof messageId !== "string" || !/^[0-9a-f-]{36}$/.test(messageId)))) throw { code: "invalid-request" };
  return { ...page, attachmentId, ...(typeof messageId === "string" ? { messageId } : {}) };
}
function isMediaCursor(value: unknown): boolean {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const cursor = value as Record<string, unknown>;
  return typeof cursor.sortAtMs === "string" && /^[0-9]+$/.test(cursor.sortAtMs)
    && typeof cursor.localOrder === "string" && /^[0-9]+$/.test(cursor.localOrder)
    && typeof cursor.messageId === "string" && /^[0-9a-f-]{36}$/.test(cursor.messageId)
    && Number.isSafeInteger(cursor.attachmentPosition) && (cursor.attachmentPosition as number) >= 0;
}
async function addMediaPagePreviews(session: HostSession, value: CoreResult): Promise<CoreResult> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw { code: "invalid-request" };
  const page = value as Record<string, unknown>;
  if (!Array.isArray(page.items)) throw { code: "invalid-request" };
  const enrichRetention = attachmentRetentionEnricher(session);
  const items = await Promise.all(page.items.map(async item => {
    if (typeof item !== "object" || item === null || Array.isArray(item)) return item;
    const view = item as Record<string, unknown>;
    const attachment = await enrichRetention(view.attachment);
    const attachmentField = Object.prototype.hasOwnProperty.call(view, "attachment") ? { attachment } : {};
    const meta = typeof view.mediaMeta === "object" && view.mediaMeta !== null && !Array.isArray(view.mediaMeta) ? view.mediaMeta as Record<string, unknown> : undefined;
    const thumbnail = meta?.thumbnail;
    if (!thumbnail || !previewableAttachment(thumbnail)) return { ...view, ...attachmentField };
    return { ...view, ...attachmentField, mediaMeta: { ...meta, thumbnail: await addAttachmentPreview(session, thumbnail) } };
  }));
  return { ...page, items };
}
async function addAroundPreviews(session: HostSession, value: CoreResult): Promise<CoreResult> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw { code: "invalid-request" };
  const page = value as Record<string, unknown>;
  if (!page.target || !Array.isArray(page.newer) || !Array.isArray(page.older)) throw { code: "invalid-request" };
  const items = await addMediaPagePreviews(session, { items: [page.target, ...page.newer, ...page.older] });
  if (typeof items !== "object" || items === null || !Array.isArray((items as Record<string, unknown>).items)) throw { code: "invalid-request" };
  const [target, ...neighbors] = (items as Record<string, unknown>).items as unknown[];
  return { ...page, target, newer: neighbors.slice(0, page.newer.length), older: neighbors.slice(page.newer.length) };
}
async function openMedia(session: HostSession, args: Record<string, unknown>): Promise<Record<string, unknown>> {
  const id = requiredId(args);
  if (session.openMediaAttachment) {
    try {
      return mediaOutput(await session.openMediaAttachment(id, MAX_INLINE_MEDIA_BYTES));
    } catch (error: unknown) {
      if (isMediaPending(error)) return { state: "pending" };
      throw error;
    }
  }
  const info = await session.query("attachment_info", { id });
  if (typeof info !== "object" || info === null || Array.isArray(info)) throw { code: "invalid-request" };
  if ((info as Record<string, unknown>).state !== "ready") return { state: "pending" };
  if (!session.exportAttachment) throw { code: "unavailable" };
  return mediaOutput(await session.exportAttachment(id));
}
function isMediaPending(error: unknown): boolean {
  return typeof error === "object" && error !== null && "code" in error && error.code === "media-pending";
}
function mediaOutput(output: Record<string, unknown> & { bytes: Uint8Array }): Record<string, unknown> {
  if (output.bytes.byteLength > MAX_INLINE_MEDIA_BYTES) throw { code: "unsupported" };
  return { state: "ready", bytes: output.bytes.buffer.slice(output.bytes.byteOffset, output.bytes.byteOffset + output.bytes.byteLength), mediaType: output.type, byteSize: output.bytes.byteLength };
}
function objectOrUndefined(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : undefined;
}
async function demandAttachmentIfNeeded(session: HostSession, id: string): Promise<boolean> {
  const info = await session.query("attachment_info", { id });
  if (typeof info !== "object" || info === null || Array.isArray(info)) throw { code: "invalid-request" };
  if ((info as Record<string, unknown>).downloadRequested === true) return false;
  await session.mutate("request_attachment_download", { id });
  return true;
}

function attachmentBytes(args: Record<string, unknown>): Uint8Array {
  const bytes = args.bytes;
  if (!(bytes instanceof ArrayBuffer) || bytes.byteLength === 0 || bytes.byteLength > 32 * 1024 * 1024) throw { code: "invalid-request" };
  return new Uint8Array(bytes);
}
function requiredId(args: Record<string, unknown>): string {
  const id = args.id;
  if (typeof id !== "string" || !/^[0-9a-f-]{36}$/.test(id)) throw { code: "invalid-request" };
  return id;
}
function requiredString(value: unknown): string {
  if (typeof value !== "string" || value.length === 0) throw { code: "invalid-request" };
  return value;
}
async function prepareAttachment(session: HostSession, args: Record<string, unknown>): Promise<CoreResult> {
  const displayName = args.displayName;
  const mediaType = args.mediaType;
  if (typeof displayName !== "string" || displayName.length === 0 || displayName.length > 255 || typeof mediaType !== "string" || mediaType.length === 0 || mediaType.length > 255 || !session.stageInput) throw { code: "invalid-request" };
  const bytes = attachmentBytes(args);
  if (session.prepareAttachment) return session.prepareAttachment(bytes, displayName, mediaType);
  const temporaryInputFilename = await session.stageInput(bytes);
  return session.mutate("prepare_attachment", { temporaryInputFilename, displayName, mediaType });
}
async function exportAttachment(session: HostSession, args: Record<string, unknown>): Promise<Record<string, unknown>> {
  const id = requiredId(args);
  if (session.exportAttachment) {
    const output = await session.exportAttachment(id);
    if (typeof output.displayName !== "string" || typeof output.type !== "string") throw { code: "invalid-request" };
    return { bytes: output.bytes.buffer, displayName: output.displayName, mediaType: output.type };
  }
  if (!session.readWorkerOutput) throw { code: "unavailable" };
  const outputId = crypto.randomUUID();
  const result = await session.query("export_attachment", { id, outputId });
  if (typeof result !== "object" || result === null || Array.isArray(result)) throw { code: "invalid-request" };
  const output = result as Record<string, unknown>;
  if (output.file !== `worker-output/${outputId}` || typeof output.length !== "number" || typeof output.displayName !== "string" || typeof output.type !== "string") throw { code: "invalid-request" };
  return { bytes: (await session.readWorkerOutput(outputId, output.length)).buffer, displayName: output.displayName, mediaType: output.type };
}

function producerCommand(request: RpcRequest): boolean {
  if (request.command === "prepare_attachment" || request.command === "publish_attachment" || request.command === "restore_contact") return true;
  const input = objectOrUndefined(request.args.input);
  if (request.command === "send_draft") return Array.isArray(input?.attachmentIds) && input.attachmentIds.length > 0;
  if (request.command === "submit_contact_edit") return objectOrUndefined(input?.photo)?.kind === "set";
  return false;
}

/** SharedWorker coordinator: all renderer ports share the same session and serialized checkpoint owner. */
export class BrowserWorkerHost {
  private readonly ports = new Set<BrowserPort>();
  private owner?: WorkerOwner<HostSession>;
  private starting?: Promise<void>;
  private stopping = false;
  private unsubscribeNetwork?: () => void;
  private presenter?: BannerPresenter;
  private readonly presentationContexts = new Map<BrowserPort, PresentationContext>();
  private readonly presentationRecipients = new Map<string, BrowserPort>();
  private presentationSequence = 0;
  private readonly mediaLeases = new Map<BrowserPort, Map<string, number>>();
  /** View-local interest only; core retains admitted work after a port detaches. */
  private readonly historyAttempts = new Map<BrowserPort, string>();
  private searchIndexTimer?: ReturnType<typeof setTimeout>;
  private searchIndexInFlight = false;
  private searchIndexLastCompletedAt?: number;
  private searchIndexLastMutationDurationMs = 0;
  private searchIndexRetries = 0;
  private searchIndexState?: "indexing" | "ready";
  /** Generation commits made by index-only maintenance; subtracted so they never look like visible changes. */
  private searchIndexCommits = 0;
  /** Dispatched RPCs in flight; maintenance waits for zero so its generation delta is exact. */
  private dispatchesInFlight = 0;
  private rememberedUnlockValidation?: Promise<void>;
  private deviceSessionScope?: { binding: string; id: string };
  private producerAdmissionReady = false;
  private networkStarted = false;
  private networkStarting?: Promise<void>;
  private lastRememberedUnlockValidationAt = 0;

  public constructor(private readonly dependencies: BrowserHostDependencies) {}

  public async attach(port: BrowserPort): Promise<void> {
    if (this.stopping) {
      port.postMessage({ event: "stopped", reason: "closed" });
      port.close();
      return;
    }
    try {
      await this.start();
      if (this.stopping) {
        port.postMessage({ event: "stopped", reason: "closed" });
        port.close();
        return;
      }
      this.ports.add(port);
      this.mediaLeases.set(port, new Map());
      port.onmessage = event => { void this.handle(port, event.data); };
      port.start?.();
      port.postMessage({ event: "ready", version: 1 });
    } catch (error: unknown) {
      port.postMessage({ id: -1, ok: false, error: errorFor(error) });
      port.postMessage({ event: "stopped", reason: "unavailable" });
      port.close();
    }
  }

  public async stop(reason = "closed", acknowledge?: () => void): Promise<void> {
    if (!this.beginStop()) return;
    try {
      if (reason === "revoked") {
        try { await this.owner?.owner.clearRememberedUnlock?.(); } catch {}
      }
      await this.finishStop(reason, acknowledge);
    } catch {
      await this.finishStop(reason, acknowledge);
    }
  }

  private beginStop(): boolean {
    if (this.stopping) return false;
    this.stopping = true;
    this.presenter?.clear();
    this.presentationContexts.clear();
    this.presentationRecipients.clear();
    return true;
  }

  private async finishStop(reason: string, acknowledge?: () => void): Promise<void> {
    try {
      try { await this.dependencies.network?.stop(); } catch {}
      this.unsubscribeNetwork?.();
      this.cancelSearchIndex();
      this.owner?.owner.shutdown();
      try { await this.owner?.owner.drain?.(); } catch {}
      acknowledge?.();
      for (const port of this.ports) { port.postMessage({ event: "stopped", reason }); port.close(); }
      this.ports.clear();
      this.mediaLeases.clear();
      this.historyAttempts.clear();
      this.deviceSessionScope = undefined;
      try { await this.owner?.release(); } catch {}
    } finally { this.dependencies.terminate?.(); }
  }

  private async start(): Promise<void> {
    if (this.owner) return;
    this.starting ??= this.acquire();
    try {
      await this.starting;
    } finally {
      if (!this.owner) this.starting = undefined;
    }
  }

  private async acquire(): Promise<void> {
    const owner = await acquireWorkerOwner({ locks: this.dependencies.locks, name: OWNER_NAME, start: signal => this.dependencies.boot(signal, error => { void this.fatal(error); }), stop: async session => { await session.shutdown(); await session.drain?.(); } });
    this.owner = owner;
    try {
      this.presenter = new BannerPresenter(owner.owner, new MigratingCoreNotificationSettings(owner.owner, this.dependencies.legacySettings, () => { this.changed(); this.dependencies.network?.wake?.(); }), this.dependencies.notification ?? new BrowserNotificationAdapter(), () => {
        const recipient = this.selectedPresentationPort();
        return recipient ? candidate => {
          this.presentationRecipients.set(candidate.id, recipient);
          recipient.postMessage({ event: "banner", candidate });
        } : undefined;
      }, Date.now, ids => ids.forEach(id => this.presentationRecipients.delete(id)));
      this.unsubscribeNetwork = this.dependencies.network?.subscribeChanges?.(() => this.changed());
      if (owner.owner.phase === "ready") await this.startNetwork();
    } catch (error) {
      if (error instanceof TransportError && error.kind === "revoked") {
        await this.stop("revoked");
        throw error;
      }
      this.unsubscribeNetwork?.();
      owner.owner.shutdown();
      try { await owner.owner.drain?.(); } catch {}
      try { await owner.release(); } catch {}
      if (this.owner === owner) this.owner = undefined;
      throw error;
    }
  }

  private async handle(port: BrowserPort, request: unknown): Promise<void> {
    if (!this.owner || this.stopping) return;
    if (!validRequest(request)) {
      port.postMessage({ id: requestId(request), ok: false, error: { code: "invalid-request", message: "The request is invalid." } });
      return;
    }
    if (request.command === "lock_sync") {
      if (!this.beginStop()) return;
      let clearFailed = false;
      try { await this.owner.owner.clearRememberedUnlock?.(); } catch { clearFailed = true; }
      await this.finishStop(clearFailed ? "locked-recovery-clear-failed" : "locked", () => port.postMessage(clearFailed ? { id: request.id, ok: false, error: RECOVERY_CLEAR_ERROR } : { id: request.id, ok: true, value: undefined }));
      return;
    }
    if (request.command === "disconnect") {
      try { await this.releasePortMedia(port); } catch {} finally {
        this.ports.delete(port);
        this.mediaLeases.delete(port);
        this.detachHistoryAttempt(port);
        this.removePresentationPort(port);
        port.postMessage({ event: "stopped", reason: "disconnected" });
        port.close();
      }
      return;
    }
    if (request.command === "set_notification_context") {
      const context = notificationContext(request.args);
      if (!context) { port.postMessage({ id: request.id, ok: false, error: { code: "invalid-request", message: "The request is invalid." } }); return; }
      this.presentationContexts.set(port, { context, sequence: ++this.presentationSequence });
      this.updatePresentationContext();
      port.postMessage({ id: request.id, ok: true, value: undefined });
      this.drainPresentation();
      return;
    }
    if (request.command === "flush_draft_sync") {
      if (Object.keys(request.args).length !== 0) { port.postMessage({ id: request.id, ok: false, error: { code: "invalid-request", message: "The request is invalid." } }); return; }
      this.dependencies.network?.flushDraftSync?.();
      port.postMessage({ id: request.id, ok: true, value: undefined });
      return;
    }
    if (request.command === "display_ack") {
      const ids = request.args.ids;
      if (!validDisplayIds(ids) || ids.some(id => this.presentationRecipients.get(id) !== port)) { port.postMessage({ id: request.id, ok: false, error: { code: "invalid-request", message: "The request is invalid." } }); return; }
      try { await this.presenter?.acknowledgeDisplayed(ids); ids.forEach(id => this.presentationRecipients.delete(id)); port.postMessage({ id: request.id, ok: true, value: undefined }); } catch (error) { port.postMessage({ id: request.id, ok: false, error: errorFor(error) }); }
      return;
    }
    if (request.command === "display_failed") {
      const ids = request.args.ids;
      if (!validDisplayIds(ids) || ids.some(id => this.presentationRecipients.get(id) !== port)) { port.postMessage({ id: request.id, ok: false, error: { code: "invalid-request", message: "The request is invalid." } }); return; }
      this.presenter?.releaseDisplayed(ids);
      ids.forEach(id => this.presentationRecipients.delete(id));
      port.postMessage({ id: request.id, ok: true, value: undefined });
      return;
    }
    if (request.command === "attachment_retention_status") {
      if (this.owner.owner.phase !== "ready") { port.postMessage({ id: request.id, ok: false, error: { code: "locked", message: "Browser session is not ready." } }); return; }
      if (Object.keys(request.args).length !== 0) { port.postMessage({ id: request.id, ok: false, error: { code: "invalid-request", message: "The request is invalid." } }); return; }
      try {
        const value = await this.dependencies.network?.attachmentRetentionStatus?.();
        if (value === undefined) throw { code: "unsupported" };
        port.postMessage({ id: request.id, ok: true, value });
      } catch (error) { port.postMessage({ id: request.id, ok: false, error: errorFor(error) }); }
      return;
    }
    if (request.command === "account_access") {
      if (this.owner.owner.phase !== "ready") { port.postMessage({ id: request.id, ok: false, error: { code: "locked", message: "Browser session is not ready." } }); return; }
      if (Object.keys(request.args).length !== 0) { port.postMessage({ id: request.id, ok: false, error: { code: "invalid-request", message: "The request is invalid." } }); return; }
      try {
        const value = await this.dependencies.network?.accountAccess?.() ?? { kind: "unsupported" };
        port.postMessage({ id: request.id, ok: true, value });
      } catch (error) { port.postMessage({ id: request.id, ok: false, error: errorFor(error) }); }
      return;
    }
    if (request.command === "unlock" && this.owner.owner.phase === "ready") {
      const passphrase = secretString(request.args, "passphrase");
      if (!passphrase || !this.dependencies.network?.unlockNewEpoch) { port.postMessage({ id: request.id, ok: false, error: { code: "invalid-request", message: "The request is invalid." } }); return; }
      try { await this.dependencies.network.unlockNewEpoch(passphrase); void this.startSearchIndexWhenReady(); port.postMessage({ id: request.id, ok: true, value: undefined }); this.changed(); this.dependencies.network.wake?.(); } catch (error) { port.postMessage({ id: request.id, ok: false, error: errorFor(error) }); }
      return;
    }
    if (request.command.startsWith("enrollment_")) {
      const response = await this.enrollment(request);
      if (response.ok && ENROLLMENT_ACTIVATIONS.has(request.command) && this.owner.owner.phase === "ready" && isActivationView(request.command, response.value)) {
        try {
          await this.startNetwork();
        } catch (error) {
          port.postMessage({ id: request.id, ok: false, error: errorFor(error) });
          await this.stop(error instanceof TransportError && error.kind === "revoked" ? "revoked" : "unavailable");
          return;
        }
      }
      port.postMessage(response);
      if (response.ok && ENROLLMENT_MUTATIONS.has(request.command)) this.changed();
      if (this.owner.owner.phase === "closed") await this.stop("closed");
      return;
    }
    if (request.command === "list_devices" || request.command === "revoke_device") {
      const response = await this.deviceManagement(request);
      port.postMessage(response);
      if (response.ok && request.command === "revoke_device") this.changed();
      return;
    }
    if (request.command.startsWith("mcp_") || request.command === "webhooks_list" || request.command.startsWith("webhook_")) {
      const response = await this.mcpManagement(request);
      port.postMessage(response);
      if (response.ok && !["mcp_status", "mcp_grants", "mcp_authorization", "webhooks_list", "webhook_reveal_secret", "webhook_test"].includes(request.command)) this.changed();
      return;
    }
    if (request.command === "advance_search_index") {
      const response = await this.explicitSearchIndexAdvance(request);
      if (this.ports.has(port)) port.postMessage(response);
      return;
    }
    if (HISTORY_COMMANDS.has(request.command)) {
      const response = await this.historySearch(port, request);
      if (this.ports.has(port)) port.postMessage(response);
      return;
    }
    if (request.command === "send_draft" && this.dependencies.network?.sendDraft) {
      if (producerCommand(request) && !(this.dependencies.network.producerReady?.() ?? this.producerAdmissionReady)) {
        port.postMessage({ id: request.id, ok: false, error: { code: "unavailable", message: "Browser session is not ready." } });
        return;
      }
      try {
        const value = await this.dependencies.network.sendDraft(request.args);
        port.postMessage({ id: request.id, ok: true, value });
        this.changed();
        this.dependencies.network.wake?.();
      } catch (error) {
        port.postMessage({ id: request.id, ok: false, error: errorFor(error) });
      }
      return;
    }
    const generation = this.visibleGeneration();
    const command = request.command;
    if (this.dependencies.network && producerCommand(request) && !(this.dependencies.network.producerReady?.() ?? this.producerAdmissionReady)) { port.postMessage({ id: request.id, ok: false, error: { code: "unavailable", message: "Browser session is not ready." } }); return; }
    let response: RpcResponse;
    this.dispatchesInFlight += 1;
    try {
      const sessionScopeId = command === "load_state" ? await this.currentSessionScopeId() : undefined;
      response = await dispatchValidatedBrowserRpc(this.owner.owner, request, this.dependencies.legacySettings, this.dependencies.publisher, this.portMediaLeases(port), this.dependencies.network?.syncProgress?.(), sessionScopeId ? { sessionScopeId } : undefined);
    } finally {
      this.dispatchesInFlight -= 1;
    }
    if (!response.ok && response.error.code === "device-revoked") {
      try { await this.owner.owner.clearRememberedUnlock?.(); } catch {}
    }
    if (response.ok && command === "load_state" && this.dependencies.network?.epochStatus?.()) {
      const value = response.value;
      if (typeof value === "object" && value !== null && !Array.isArray(value)) {
        response = { ...response, value: { ...value as Record<string, unknown>, encryption: { ...(value as Record<string, unknown>).encryption as Record<string, unknown>, state: "mismatch" } } };
      }
    }
    if (response.ok && command === "unlock") {
      try {
        await this.startNetwork();
      } catch (error) {
        response = { id: request.id, ok: false, error: errorFor(error) };
        if (this.ports.has(port)) port.postMessage(response);
        await this.stop(error instanceof TransportError && error.kind === "revoked" ? "revoked" : "unavailable");
        return;
      }
    }
    if (!this.ports.has(port)) return;
    port.postMessage(response);
    if (this.owner.owner.phase === "closed") {
      await this.stop("closed");
      return;
    }
    const mediaSyncNoop = command === "media_sync_request" && response.ok && !mediaSyncRequestAccepted(response.value);
    const mutationChanged = command !== "advance_search_index" && command !== undefined && INVOCATIONS[command]?.mutation && !mediaSyncNoop && (response.ok || (!response.ok && response.error.code === "draft-moved"));
    const successfulSideEffect = response.ok && (command === "unlock" || command === "publish_attachment" || command === "retry_attachment");
    const generationChanged = generation !== this.visibleGeneration();
    if (mutationChanged || successfulSideEffect || generationChanged) {
      this.changed();
      if (response.ok && command === "save_draft") this.dependencies.network?.draftSaved?.();
      if (command === "unlock") void this.startSearchIndexWhenReady();
      if (command === "load_state" && generationChanged) this.dependencies.network?.wake?.();
      if (command === "retry_attachment") this.dependencies.network?.retryAttachment?.(requiredId(request.args));
      if (command === "send_draft" || command === "retry_attachment" || command === "request_contact_sync" || command === "request_attachment_download" || command === "media_sync_request" || command === "load_media" || command === "save_attachment") this.dependencies.network?.wake?.();
    }
    if (response.ok && command === "search_local") {
      this.recordSearchIndexState(response.value);
      if (isIndexing(response.value)) this.scheduleSearchIndex();
    }
  }

  private async startNetwork(): Promise<void> {
    if (this.networkStarted) return;
    const owner = this.owner;
    if (!owner || this.stopping) return;
    this.networkStarting ??= Promise.resolve()
      .then(() => this.dependencies.network?.start())
      .then(ready => {
        if (this.owner !== owner || owner.owner.phase !== "ready" || this.stopping) return;
        this.networkStarted = true;
        this.producerAdmissionReady = ready !== false;
        void this.startSearchIndexWhenReady();
      });
    const starting = this.networkStarting;
    try {
      await starting;
    } finally {
      if (this.networkStarting === starting && !this.networkStarted) this.networkStarting = undefined;
    }
  }

  private async currentSessionScopeId(): Promise<string | undefined> {
    const session = this.owner?.owner;
    if (!session || session.phase !== "ready") return undefined;
    try {
      const identity = object(await session.query("_worker_identity_metadata", {}));
      if (typeof identity.vaultId !== "string" || identity.vaultId.length === 0 || typeof identity.deviceId !== "string" || identity.deviceId.length === 0) return undefined;
      const binding = `${identity.vaultId}\0${identity.deviceId}`;
      if (this.deviceSessionScope?.binding !== binding) this.deviceSessionScope = { binding, id: crypto.randomUUID() };
      return this.deviceSessionScope.id;
    } catch {
      return undefined;
    }
  }

  private async historySearch(port: BrowserPort, request: RpcRequest): Promise<RpcResponse> {
    const session = this.owner?.owner;
    if (!session || session.phase !== "ready") return { id: request.id, ok: false, error: { code: "locked", message: "Browser session is not ready." } };
    try {
      if (request.command === "get_history_search") {
        if (Object.keys(request.args).length !== 0) throw { code: "invalid-request" };
        return { id: request.id, ok: true, value: await session.query("get_history_search", this.historyArgs(port)) };
      }
      if (request.command === "start_history_search") {
        const operationId = canonicalUuid(request.args.operationId);
        if (Object.keys(request.args).length !== 1 || !operationId) throw { code: "invalid-request" };
        const value = await this.historyMutation(session, "start_history_search", { operationId });
        const attemptId = historyAttemptId(value);
        if (attemptId && this.ports.has(port)) {
          this.historyAttempts.set(port, attemptId);
          this.dependencies.network?.startHistorySearch?.(attemptId);
        }
        this.changed();
        return { id: request.id, ok: true, value };
      }
      const attemptId = canonicalUuid(request.args.attemptId);
      if (Object.keys(request.args).length !== 1 || !attemptId || this.historyAttempts.get(port) !== attemptId) throw { code: "invalid-request" };
      const value = await this.historyMutation(session, "cancel_history_search", { attemptId });
      this.detachHistoryAttempt(port);
      this.changed();
      return { id: request.id, ok: true, value };
    } catch (error: unknown) {
      return { id: request.id, ok: false, error: errorFor(error) };
    }
  }

  private historyArgs(port: BrowserPort): Record<string, unknown> {
    const attemptId = this.historyAttempts.get(port);
    return attemptId ? { attemptId } : {};
  }

  private async historyMutation(session: HostSession, command: string, args: Record<string, unknown>): Promise<CoreResult> {
    this.dispatchesInFlight += 1;
    try { return await session.mutate(command, args); }
    finally { this.dispatchesInFlight -= 1; }
  }

  private detachHistoryAttempt(port: BrowserPort): void {
    const attemptId = this.historyAttempts.get(port);
    this.historyAttempts.delete(port);
    if (attemptId && ![...this.historyAttempts.values()].includes(attemptId)) {
      this.dependencies.network?.stopHistorySearch?.(attemptId);
    }
  }

  private broadcastChanged(): void {
    for (const port of this.ports) port.postMessage({ event: "changed" });
  }

  private changed(): void {
    this.broadcastChanged();
    this.drainPresentation();
  }

  private drainPresentation(): void {
    if (this.stopping) return;
    void this.presenter?.drain().catch(() => undefined);
  }

  private selectedPresentationPort(): BrowserPort | undefined {
    const contexts = [...this.presentationContexts.entries()];
    const focused = contexts.filter(([, entry]) => entry.context.focused);
    return (focused.length > 0 ? focused : contexts)
      .sort(([, left], [, right]) => right.sequence - left.sequence)[0]?.[0];
  }

  private updatePresentationContext(): void {
    const port = this.selectedPresentationPort();
    this.presenter?.setContext(port ? this.presentationContexts.get(port)!.context : undefined);
  }

  private removePresentationPort(port: BrowserPort): void {
    this.presentationContexts.delete(port);
    for (const [id, recipient] of this.presentationRecipients) {
      if (recipient === port) this.presentationRecipients.delete(id);
    }
    this.updatePresentationContext();
  }

  /** Reads index metadata first so an already-indexed unlock writes no maintenance checkpoint. */
  private async startSearchIndexWhenReady(): Promise<void> {
    const session = this.owner?.owner;
    if (!session || session.phase !== "ready" || this.stopping) return;
    this.searchIndexLastCompletedAt ??= Date.now();
    this.searchIndexRetries = 0;
    const state = await this.readSearchIndexState(session);
    if (this.owner?.owner !== session || session.phase !== "ready" || this.stopping) return;
    this.recordSearchIndexState(state);
    if (!(isSearchIndexState(state) && state.indexState === "ready")) this.scheduleSearchIndex();
  }

  private async readSearchIndexState(session: HostSession): Promise<unknown> {
    try {
      return await session.query("search_local", { query: "" });
    } catch {
      return undefined;
    }
  }

  private scheduleSearchIndex(retryDelay = 0): void {
    if (this.searchIndexTimer || this.searchIndexInFlight || !this.owner || this.stopping || this.owner.owner.phase !== "ready") return;
    const nextAllowedAt = (this.searchIndexLastCompletedAt ?? Date.now()) + this.searchIndexDelayMs();
    const delay = Math.max(retryDelay, nextAllowedAt - Date.now(), 0);
    this.searchIndexTimer = setTimeout(() => {
      this.searchIndexTimer = undefined;
      void this.advanceSearchIndex();
    }, delay);
  }

  private async advanceSearchIndex(): Promise<void> {
    const session = this.owner?.owner;
    if (!session || this.searchIndexInFlight || this.stopping || session.phase !== "ready") return;
    if (this.dispatchesInFlight > 0) {
      this.scheduleSearchIndex(SEARCH_INDEX_IDLE_RETRY_MS);
      return;
    }
    this.searchIndexInFlight = true;
    let continueIndexing = false;
    let retryDelay: number | undefined;
    try {
      const startedAt = Date.now();
      const progress = await this.mutateSearchIndex(session, {});
      this.searchIndexLastMutationDurationMs = Date.now() - startedAt;
      this.searchIndexLastCompletedAt = Date.now();
      this.searchIndexRetries = 0;
      this.recordSearchIndexState(progress);
      continueIndexing = isIndexing(progress);
    } catch (error: unknown) {
      this.searchIndexLastCompletedAt = Date.now();
      if (session.phase !== "ready" || this.stopping) {
        this.cancelSearchIndex();
        return;
      }
      if (isTransientSearchIndexError(error) && this.searchIndexRetries < SEARCH_INDEX_MAX_RETRIES) {
        this.searchIndexRetries += 1;
        retryDelay = SEARCH_INDEX_DELAY_MS * 2 ** (this.searchIndexRetries - 1);
      } else {
        this.cancelSearchIndex();
      }
    } finally {
      this.searchIndexInFlight = false;
      if (continueIndexing) this.scheduleSearchIndex();
      else if (retryDelay !== undefined) this.scheduleSearchIndex(retryDelay);
    }
  }

  private async explicitSearchIndexAdvance(request: RpcRequest): Promise<RpcResponse> {
    const session = this.owner?.owner;
    if (!session || this.stopping || session.phase !== "ready") {
      const code = session?.phase === "locked" ? "locked" : "not-ready";
      return { id: request.id, ok: false, error: { code, message: "Search indexing is unavailable." } };
    }
    if (this.searchIndexInFlight || this.searchIndexTimer || this.dispatchesInFlight > 0 || !this.searchIndexRateAllowsNow()) {
      return { id: request.id, ok: false, error: { code: "busy", message: "Search indexing is already scheduled." } };
    }
    this.searchIndexInFlight = true;
    let continueIndexing = false;
    try {
      const startedAt = Date.now();
      const progress = await this.mutateSearchIndex(session, request.args);
      this.searchIndexLastMutationDurationMs = Date.now() - startedAt;
      this.searchIndexLastCompletedAt = Date.now();
      this.searchIndexRetries = 0;
      this.recordSearchIndexState(progress);
      continueIndexing = isIndexing(progress);
      return { id: request.id, ok: true, value: progress };
    } catch (error: unknown) {
      this.searchIndexLastCompletedAt = Date.now();
      if (session.phase !== "ready" || this.stopping) this.cancelSearchIndex();
      return { id: request.id, ok: false, error: errorFor(error) };
    } finally {
      this.searchIndexInFlight = false;
      if (continueIndexing) this.scheduleSearchIndex();
    }
  }

  /** Counts exactly the generations this maintenance commit added, including commits followed by a handled rejection. */
  private async mutateSearchIndex(session: HostSession, args: Record<string, unknown>): Promise<CoreResult> {
    const before = session.generation ?? 0;
    try {
      return await session.mutate("advance_search_index", args);
    } finally {
      this.searchIndexCommits += (session.generation ?? 0) - before;
    }
  }

  /** User-visible generation: index-only checkpoint bumps are excluded on both sides of a comparison. */
  private visibleGeneration(): number {
    return (this.owner?.owner.generation ?? 0) - this.searchIndexCommits;
  }

  private searchIndexRateAllowsNow(): boolean {
    return this.searchIndexLastCompletedAt === undefined || Date.now() - this.searchIndexLastCompletedAt >= this.searchIndexDelayMs();
  }

  private searchIndexDelayMs(): number {
    return Math.max(SEARCH_INDEX_DELAY_MS, this.searchIndexLastMutationDurationMs * 3);
  }

  private recordSearchIndexState(value: unknown): void {
    if (!isSearchIndexState(value)) return;
    const changed = this.searchIndexState !== undefined && this.searchIndexState !== value.indexState;
    this.searchIndexState = value.indexState;
    if (changed) this.changed();
  }

  private cancelSearchIndex(): void {
    if (this.searchIndexTimer) clearTimeout(this.searchIndexTimer);
    this.searchIndexTimer = undefined;
    this.searchIndexRetries = 0;
  }

  private scheduleRememberedUnlockValidation(): void {
    const session = this.owner?.owner;
    if (!session?.validateRememberedUnlock || this.rememberedUnlockValidation || Date.now() - this.lastRememberedUnlockValidationAt < REMEMBERED_UNLOCK_VALIDATION_INTERVAL_MS) return;
    this.lastRememberedUnlockValidationAt = Date.now();
    this.rememberedUnlockValidation = session.validateRememberedUnlock()
      .catch(() => undefined)
      .finally(() => { this.rememberedUnlockValidation = undefined; });
  }

  private async enrollment(request: RpcRequest): Promise<RpcResponse> {
    const network = this.dependencies.network;
    if (!network) return { id: request.id, ok: false, error: { code: "unavailable", message: "Browser operation failed." } };
    try {
      let recoveryClearFailed = false;
      if (request.command === "enrollment_sign_out") {
        try { await this.owner?.owner.clearRememberedUnlock?.(); } catch { recoveryClearFailed = true; }
      }
      const setupToken = secretString(request.args, "setupToken");
      const adminPassword = secretString(request.args, "adminPassword");
      const csrf = secretString(request.args, "csrf");
      const passphrase = secretString(request.args, "passphrase");
      const method = request.args.method;
      const username = typeof request.args.username === "string" ? request.args.username : undefined;
      const vaultOwner = typeof request.args.vaultOwner === "string" ? request.args.vaultOwner : undefined;
      const inviteLink = typeof request.args.inviteLink === "string" ? request.args.inviteLink : undefined;
      const displayName = typeof request.args.displayName === "string" ? request.args.displayName : undefined;
      const waitingOnly = request.args.waitingOnly;
      if (waitingOnly !== undefined && typeof waitingOnly !== "boolean") throw { code: "invalid-request" };
      if (request.command === "enrollment_set_hosted_csrf" && !csrf) throw { code: "invalid-request" };
      const value = request.command === "enrollment_discover" ? await network.enrollmentDiscover(typeof request.args.origin === "string" ? request.args.origin : undefined)
       : request.command === "enrollment_create_owner" && passphrase ? await network.enrollmentCreateOwner({ ...(setupToken ? { setupToken } : {}), ...(adminPassword ? { adminPassword } : {}), passphrase }, username)
          : request.command === "enrollment_sign_in" ? await network.enrollmentSignIn(adminPassword ?? "", username)
            : request.command === "enrollment_set_hosted_csrf" && csrf ? (await network.enrollmentSetHostedCsrf(csrf), undefined)
            : request.command === "enrollment_connect_shared_secret" && passphrase ? await network.enrollmentConnectSharedSecret(passphrase, vaultOwner)
              : request.command === "enrollment_begin_vault_request" && inviteLink && displayName ? await network.enrollmentBeginVaultRequest(inviteLink, displayName)
                : request.command === "enrollment_vault_request_status" ? await network.enrollmentVaultRequestStatus()
                  : request.command === "enrollment_create_vault" && username && passphrase && adminPassword ? await network.enrollmentCreateVault(username, { adminPassword, passphrase })
                    : request.command === "enrollment_pending_vault_request" ? await network.enrollmentPendingVaultRequest()
               : request.command === "enrollment_add_phone" && (method === "transfer" || method === "shared_secret") ? await network.enrollmentAddPhone(method)
                 : request.command === "enrollment_connect_computer" ? await network.enrollmentConnectComputer()
                   : request.command === "enrollment_computer_status" ? await network.enrollmentComputerStatus()
                     : request.command === "enrollment_confirm_computer" ? await network.enrollmentConfirmComputer()
                       : request.command === "enrollment_cancel_computer" ? await network.enrollmentCancelComputer()
                 : request.command === "enrollment_status" ? await network.enrollmentStatus()
                   : request.command === "enrollment_approve" ? await network.enrollmentApprove()
                     : request.command === "enrollment_cancel" ? await network.enrollmentCancel(waitingOnly === true ? { waitingOnly: true } : undefined)
        : request.command === "enrollment_sign_out" ? (await network.enrollmentSignOut(), undefined)
                        : request.command === "enrollment_generate_secret" ? { passphrase: await network.enrollmentGenerateSecret() }
                          : request.command === "enrollment_management" ? await network.enrollmentManagement()
                             : request.command === "enrollment_revoke_device" && typeof request.args.deviceId === "string" ? await network.enrollmentRevokeDevice(request.args.deviceId)
                               : request.command === "enrollment_list_vaults" ? await network.enrollmentListVaults()
                                 : request.command === "enrollment_create_invite" ? await network.enrollmentCreateInvite()
                                   : request.command === "enrollment_list_invites" ? await network.enrollmentListInvites()
                                     : request.command === "enrollment_revoke_invite" && typeof request.args.inviteId === "string" ? (await network.enrollmentRevokeInvite(request.args.inviteId), undefined)
                                       : request.command === "enrollment_list_vault_requests" ? await network.enrollmentListVaultRequests()
                                         : request.command === "enrollment_approve_vault_request" && typeof request.args.requestId === "string" ? (await network.enrollmentApproveVaultRequest(request.args.requestId), undefined)
                                           : request.command === "enrollment_deny_vault_request" && typeof request.args.requestId === "string" ? (await network.enrollmentDenyVaultRequest(request.args.requestId), undefined)
                           : undefined;
      if (value === undefined && !["enrollment_cancel", "enrollment_sign_out", "enrollment_set_hosted_csrf", "enrollment_revoke_invite", "enrollment_approve_vault_request", "enrollment_deny_vault_request"].includes(request.command)) throw { code: "invalid-request" };
      if (request.command === "enrollment_status") this.scheduleRememberedUnlockValidation();
      if (request.command === "enrollment_sign_out" && recoveryClearFailed) return { id: request.id, ok: false, error: RECOVERY_CLEAR_ERROR };
      return { id: request.id, ok: true, value };
    } catch (error: unknown) { return { id: request.id, ok: false, error: errorFor(error) }; }
  }

  private async deviceManagement(request: RpcRequest): Promise<RpcResponse> {
    const network = this.dependencies.network;
    if (!network) return { id: request.id, ok: false, error: { code: "unavailable", message: "Browser operation failed." } };
    let currentDeviceId: string | undefined;
    try {
      if (request.command === "list_devices") {
        const identity = object(await this.owner!.owner.query("_worker_identity_metadata", {}));
        currentDeviceId = requiredDeviceId(identity.deviceId);
        return { id: request.id, ok: true, value: deviceRoster(await network.listDevices(), currentDeviceId) };
      }
      const deviceId = requiredDeviceId(request.args.deviceId);
      const identity = object(await this.owner!.owner.query("_worker_identity_metadata", {}));
      currentDeviceId = requiredDeviceId(identity.deviceId);
      await network.revokeDevice(deviceId);
      return { id: request.id, ok: true, value: undefined };
    } catch (error: unknown) {
      if (shouldForgetRememberedUnlock(error, request, currentDeviceId)) {
        try { await this.owner?.owner.clearRememberedUnlock?.(); } catch {}
      }
      return { id: request.id, ok: false, error: deviceManagementError(error) };
    }
  }

  private async mcpManagement(request: RpcRequest): Promise<RpcResponse> {
    const network = this.dependencies.network;
    if (!network) return { id: request.id, ok: false, error: { code: "unavailable", message: "Browser operation failed." } };
    try {
      if (request.command === "mcp_status" && Object.keys(request.args).length === 0) return { id: request.id, ok: true, value: await network.mcpStatus() };
      if (request.command === "mcp_enable" && Object.keys(request.args).length === 0) return { id: request.id, ok: true, value: await network.mcpEnable() };
      if (request.command === "mcp_grants" && Object.keys(request.args).length === 0) return { id: request.id, ok: true, value: await network.mcpGrants() };
      if (request.command === "mcp_revoke_grant") {
        await network.mcpRevokeGrant(requiredDeviceId(request.args.id));
        return { id: request.id, ok: true, value: undefined };
      }
      if (request.command === "webhooks_list" && Object.keys(request.args).length === 0) return { id: request.id, ok: true, value: await network.webhooksList() };
      if (request.command === "webhook_create" && exactKeys(request.args, ["input"])) return { id: request.id, ok: true, value: await network.webhookCreate(webhookInput(request.args.input)) };
      if (request.command === "webhook_update" && exactKeys(request.args, ["id", "input"])) return { id: request.id, ok: true, value: await network.webhookUpdate(requiredDeviceId(request.args.id), webhookInput(request.args.input)) };
      if (request.command === "webhook_delete" && exactKeys(request.args, ["id"])) { await network.webhookDelete(requiredDeviceId(request.args.id)); return { id: request.id, ok: true, value: undefined }; }
      if (request.command === "webhook_reveal_secret" && exactKeys(request.args, ["id"])) return { id: request.id, ok: true, value: await network.webhookRevealSecret(requiredDeviceId(request.args.id)) };
      if (request.command === "webhook_rotate_secret" && exactKeys(request.args, ["id"])) return { id: request.id, ok: true, value: await network.webhookRotateSecret(requiredDeviceId(request.args.id)) };
      if (request.command === "webhook_test" && exactKeys(request.args, ["id"])) return { id: request.id, ok: true, value: await network.webhookTest(requiredDeviceId(request.args.id)) };
      if (request.command === "mcp_authorization") return { id: request.id, ok: true, value: await network.mcpAuthorization(requiredMcpAuthorizationId(request.args.id)) };
      if (request.command === "mcp_approve_authorization") return { id: request.id, ok: true, value: await network.mcpApproveAuthorization(requiredMcpAuthorizationId(request.args.id), requiredMcpScopes(request.args.scopes)) };
      if (request.command === "mcp_deny_authorization") return { id: request.id, ok: true, value: await network.mcpDenyAuthorization(requiredMcpAuthorizationId(request.args.id)) };
      throw new TransportError("invalid");
    } catch (error) {
      return { id: request.id, ok: false, error: mcpErrorFor(error) };
    }
  }

  private async fatal(error: Error): Promise<void> {
    console.error("Browser worker fatal", fatalDiagnostic(error));
    await this.stop("fatal");
  }

  private portMediaLeases(port: BrowserPort): MediaLeases {
    return {
      acquire: id => {
        const leases = this.mediaLeases.get(port);
        if (!leases) return false;
        leases.set(id, (leases.get(id) ?? 0) + 1);
        return true;
      },
      release: id => {
        const leases = this.mediaLeases.get(port);
        const count = leases?.get(id) ?? 0;
        if (count > 1) {
          leases!.set(id, count - 1);
          return true;
        }
        if (count === 1) leases?.delete(id);
        return count === 1;
      },
    };
  }

  private async releasePortMedia(port: BrowserPort): Promise<void> {
    const leases = this.mediaLeases.get(port);
    let failure: unknown;
    try {
      for (const [id, count] of leases ?? []) {
        for (let lease = 0; lease < count; lease += 1) {
          try { await this.owner?.owner.mutate("release_attachment_ciphertext", { id }); } catch (error: unknown) { failure ??= error; }
        }
      }
    } finally { leases?.clear(); }
    if (failure) throw failure;
  }

}

function mediaSyncRequestAccepted(value: unknown): boolean {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    && Array.isArray((value as Record<string, unknown>).requested)
    && (value as { requested: unknown[] }).requested.length > 0;
}

function deviceRoster(value: unknown, currentDeviceId: string): Record<string, unknown> {
  const roster = object(value);
  if (!Array.isArray(roster.devices)) throw new TransportError("invalid");
  const devices = roster.devices.map(device => {
    const row = object(device);
    const id = requiredDeviceId(row.device_id);
    if (typeof row.role !== "string" || typeof row.revoked !== "boolean") throw new TransportError("invalid");
    const presence = optionalPresence(row.presence);
    const wakeRoute = optionalBoolean(row.wake_route);
    return {
      id,
      name: typeof row.name === "string" && row.name.length > 0 ? row.name : "Unnamed device",
      kind: typeof row.kind === "string" ? row.kind : null,
      role: row.role,
      revoked: row.revoked,
      ...(presence ? { presence } : {}),
      ...(wakeRoute === undefined ? {} : { wakeRoute }),
    };
  });
  return { currentDeviceId, devices };
}

function optionalPresence(value: unknown): { state: "connected" | "seen" | "never"; last_seen_age_seconds: number | null } | undefined {
  if (value === undefined) return undefined;
  const presence = object(value);
  if (presence.state !== "connected" && presence.state !== "seen" && presence.state !== "never") return undefined;
  if (presence.last_seen_age_seconds !== null && (!Number.isSafeInteger(presence.last_seen_age_seconds) || (presence.last_seen_age_seconds as number) < 0)) throw new TransportError("invalid");
  return { state: presence.state, last_seen_age_seconds: presence.last_seen_age_seconds as number | null };
}

function optionalBoolean(value: unknown): boolean | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "boolean") throw new TransportError("invalid");
  return value;
}

function object(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new TransportError("invalid");
  return value as Record<string, unknown>;
}

function requiredDeviceId(value: unknown): string {
  if (typeof value !== "string" || !/^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/i.test(value)) throw new TransportError("invalid");
  return value;
}

function requiredMcpAuthorizationId(value: unknown): string {
  // A 32-byte unpadded base64url value has two unused and four significant final sextet bits.
  if (typeof value !== "string" || !/^[A-Za-z0-9_-]{42}[AEIMQUYcgkosw048]$/.test(value)) throw new TransportError("invalid");
  return value;
}

function requiredMcpScopes(value: unknown): McpScope[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > 3) throw new TransportError("invalid");
  const scopes = value.map(scope => {
    if (scope === "conversations:read" || scope === "messages:send" || scope === "media:send") return scope;
    throw new TransportError("invalid");
  });
  if (new Set(scopes).size !== scopes.length) throw new TransportError("invalid");
  return scopes;
}

function exactKeys(value: Record<string, unknown>, keys: string[]): boolean {
  const actual = Object.keys(value);
  return actual.length === keys.length && keys.every(key => Object.hasOwn(value, key));
}

function webhookInput(value: unknown): WebhookInput {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw { code: "invalid-request" };
  const input = value as Record<string, unknown>;
  if (!exactKeys(input, WEBHOOK_INPUT_KEYS) || utf8ByteLength(JSON.stringify(input)) > MAX_WEBHOOK_INPUT_BYTES) throw { code: "invalid-request" };
  return {
    name: nullableBoundedString(input.name, MAX_WEBHOOK_NAME_BYTES),
    url: boundedString(input.url, MAX_WEBHOOK_URL_BYTES),
    events: webhookEvents(input.events),
    filterMode: enumValue(input.filterMode, WEBHOOK_FILTER_MODES),
    filter: webhookFilter(input.filter),
    paused: booleanValue(input.paused),
  };
}

function webhookEvents(value: unknown): WebhookEvent[] {
  if (!Array.isArray(value) || value.length === 0) throw { code: "invalid-request" };
  const events = value.map(event => enumValue(event, WEBHOOK_EVENTS));
  if (new Set(events).size !== events.length) throw { code: "invalid-request" };
  return events;
}

function webhookFilter(value: unknown): WebhookFilterEntry[] {
  if (!Array.isArray(value) || value.length > MAX_WEBHOOK_FILTER_ENTRIES) throw { code: "invalid-request" };
  return value.map(entry => {
    if (typeof entry !== "object" || entry === null || Array.isArray(entry)) throw { code: "invalid-request" };
    const row = entry as Record<string, unknown>;
    if (!exactKeys(row, WEBHOOK_FILTER_ENTRY_KEYS)) throw { code: "invalid-request" };
    return {
      address: boundedString(row.address, MAX_WEBHOOK_ADDRESS_BYTES),
      label: nullableBoundedString(row.label, MAX_WEBHOOK_FIELD_BYTES),
      contactId: nullableBoundedString(row.contactId, MAX_WEBHOOK_FIELD_BYTES),
      bookId: nullableBoundedString(row.bookId, MAX_WEBHOOK_FIELD_BYTES),
    };
  });
}

function enumValue<T extends string>(value: unknown, allowed: readonly T[]): T {
  if (typeof value !== "string" || !(allowed as readonly string[]).includes(value)) throw { code: "invalid-request" };
  return value as T;
}

function booleanValue(value: unknown): boolean {
  if (typeof value !== "boolean") throw { code: "invalid-request" };
  return value;
}

function boundedString(value: unknown, maxBytes: number): string {
  if (typeof value !== "string" || value.length === 0 || utf8ByteLength(value) > maxBytes) throw { code: "invalid-request" };
  return value;
}

function nullableBoundedString(value: unknown, maxBytes: number): string | null {
  if (value === null) return null;
  if (typeof value !== "string" || utf8ByteLength(value) > maxBytes) throw { code: "invalid-request" };
  return value;
}

function utf8ByteLength(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function notificationContext(args: Record<string, unknown>): BannerContext | undefined {
  if (args.view !== "conversations" && args.view !== "notifications" && args.view !== "settings" && args.view !== "contacts") return undefined;
  if (args.conversationId !== undefined && typeof args.conversationId !== "string") return undefined;
  if (args.notificationPermission !== undefined && args.notificationPermission !== "granted" && args.notificationPermission !== "denied" && args.notificationPermission !== "default" && args.notificationPermission !== "unsupported") return undefined;
  if (args.focused !== undefined && typeof args.focused !== "boolean") return undefined;
  return { view: args.view, ...(typeof args.conversationId === "string" ? { conversationId: args.conversationId } : {}), ...(typeof args.notificationPermission === "string" ? { notificationPermission: args.notificationPermission } : {}), ...(typeof args.focused === "boolean" ? { focused: args.focused } : {}), ...(typeof args.requireInteraction === "boolean" ? { requireInteraction: args.requireInteraction } : {}) };
}

function validDisplayIds(value: unknown): value is string[] {
  return Array.isArray(value) && value.length > 0 && value.length <= 20 && value.every(validDisplayId);
}

function validDisplayId(id: unknown): id is string {
  if (typeof id !== "string") return false;
  if (/^[a-zA-Z0-9-]{1,128}$/.test(id)) return true;
  if (new TextEncoder().encode(id).byteLength > 3_328) return false;
  try {
    const target = JSON.parse(id) as unknown;
    if (!target || typeof target !== "object" || Array.isArray(target) || Object.keys(target).length !== 3) return false;
    const value = target as Record<string, unknown>;
    return boundedDisplayIdComponent(value.sourceDeviceId, 128) && boundedDisplayIdComponent(value.notificationKey, 512) && boundedDisplayIdComponent(value.lifetime, 128);
  } catch {
    return false;
  }
}

function boundedDisplayIdComponent(value: unknown, maxBytes: number): value is string {
  return typeof value === "string" && value.length > 0 && new TextEncoder().encode(value).byteLength <= maxBytes;
}


export async function loadBrowserCore(): Promise<EmscriptenCoreModule> {
  const coreEntrypoint = "/core/peppy-browser-core.js";
  const module = await import(/* @vite-ignore */ coreEntrypoint) as { default?: (options: { locateFile(file: string): string }) => Promise<EmscriptenCoreModule> };
  if (!module.default) throw new Error("Browser core is unavailable");
  return module.default({ locateFile: file => file.endsWith(".wasm") ? "/core/peppy_browser_core.wasm" : `/core/${file}` });
}

async function browserOrigins(): Promise<{ logicalOrigin: string; fetchBase: string; accountOrigin?: string }> {
  const fetchBase = self.location.origin;
  try {
    const response = await fetch("/web/config.json", { credentials: "omit" });
    if (!response.ok) return { logicalOrigin: fetchBase, fetchBase };
    const config: unknown = await response.json();
    const apiOrigin = typeof config === "object" && config !== null ? (config as { apiOrigin?: unknown }).apiOrigin : undefined;
    const accountUrl = typeof config === "object" && config !== null ? (config as { accountUrl?: unknown }).accountUrl : undefined;
    const parsed = typeof apiOrigin === "string" ? new URL(apiOrigin) : undefined;
    if (!parsed || parsed.protocol !== "https:" || parsed.username || parsed.password) return { logicalOrigin: fetchBase, fetchBase };
    return { logicalOrigin: parsed.origin, fetchBase, accountOrigin: httpsOrigin(accountUrl) };
  } catch { return { logicalOrigin: fetchBase, fetchBase }; }
}

function httpsOrigin(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password ? url.origin : undefined;
  } catch { return undefined; }
}

export function browserRememberedUnlock(fetchBase: string, fetchImpl: typeof fetch = fetch): RememberedUnlock {
  return new RememberedUnlock(OWNER_NAME, { origin: fetchBase, fetch: fetchImpl });
}

export async function sendBrowserDraft(options: {
  session: Pick<BrowserSession, "mutate" | "signal">;
  transport: ConstructorParameters<typeof DraftTransport>[0];
  synchronizeDraft(): Promise<void>;
  args: Record<string, unknown>;
}): Promise<unknown> {
  const { session, transport, synchronizeDraft, args } = options;
  const input = object(args.input);
  const prepared = object(await session.mutate("_worker_draft_prepare_send", { input }));
  const draftId = requiredId(prepared);
  const expectedRevision = requiredString(prepared.expectedRevision);
  const revisionChanged = expectedRevision !== requiredString(input.expectedRevision);
  if (revisionChanged) await synchronizeDraft();
  let gate: Record<string, unknown>;
  try {
    gate = object(await session.mutate("_worker_draft_begin_send", { draftId, expectedRevision }));
  } catch (error) {
    if (!(revisionChanged && error instanceof CoreRejectedError && error.code === "draft-not-synced")) throw error;
    await synchronizeDraft();
    gate = object(await session.mutate("_worker_draft_begin_send", { draftId, expectedRevision }));
  }
  if (gate.remote === null) return session.mutate("send_draft", prepared);
  const remote = object(gate.remote);
  try {
    const accepted = await new DraftTransport(transport).acceptDraftSend({
      draftId: requiredString(remote.draftId),
      revisionId: requiredString(remote.revisionId),
      commandId: requiredString(remote.commandId),
    }, session.signal);
    return session.mutate("_worker_draft_send_accepted", { input: prepared, commandId: accepted.commandId });
  } catch (error) {
    if (isTerminalDraftSendRejection(error)) {
      await session.mutate("_worker_draft_abort_send_intent", { draftId, commandId: requiredString(remote.commandId) });
      throw { code: error.code };
    }
    if (error instanceof TransportError && (error.kind === "offline" || error.kind === "cancelled" || error.kind === "status")) throw { code: "draft-reconnect" };
    throw error;
  }
}

export function browserHostDependencies(locks: OwnerLockManager | undefined, terminate: () => void): BrowserHostDependencies {
  let enrollment: BrowserEnrollment | undefined;
  let enrollmentAbort: AbortController | undefined;
  let scheduler: BrowserScheduler | undefined;
  let publicTransport: OriginTransport | undefined;
  let retention: AttachmentRetentionDriver | undefined;
  let retentionStatus: AttachmentRetentionStatusFacade | undefined;
  let accountAccess: AccountAccessDriver | undefined;
  let retentionGeneration = 0;
  let retentionAbort: AbortController | undefined;
  let pollTimer: ReturnType<typeof setTimeout> | undefined;
  let polling = false;
  let pollPromise: Promise<void> | undefined;
  let pollDelay = 5_000;
  let syncProgress: SchedulerSyncProgress | undefined;
  let activeSession: BrowserSession | undefined;
  let liveSupervisor: LiveSupervisor | undefined;
  const changeListeners = new Set<() => void>();
  const draftSyncPrompt = new DraftSyncPrompt(
    () => polling && scheduler?.isDraftSyncEnabled === true && scheduler.status.connection === "connected",
    () => { void scheduler?.poll().catch(() => undefined); },
  );
  const stopNetwork = async () => {
    polling = false;
    if (pollTimer !== undefined) clearTimeout(pollTimer);
    pollTimer = undefined;
    draftSyncPrompt.stop();
    scheduler?.stop();
    await liveSupervisor?.stop();
    enrollmentAbort?.abort();
    retentionAbort?.abort();
    retentionStatus?.clear();
    await pollPromise?.catch(() => undefined);
  };
  const schedulePoll = () => {
    if (!polling || !scheduler) return;
    pollPromise = scheduler.poll().then(status => {
      for (const listener of changeListeners) listener();
      if (status.connection === "revoked") {
        polling = false;
        void activeSession?.clearRememberedUnlock().catch(() => undefined);
      }
      pollDelay = status.connection === "connected" ? 5_000 : Math.min(pollDelay * 2, 60_000);
    }).catch(() => {
      for (const listener of changeListeners) listener();
      pollDelay = Math.min(pollDelay * 2, 60_000);
    }).finally(() => {
      pollPromise = undefined;
      if (polling) pollTimer = setTimeout(schedulePoll, pollDelay);
    });
  };
  const sendDraft = async (args: Record<string, unknown>): Promise<unknown> => {
    const session = activeSession;
    const sendScheduler = scheduler;
    if (!session || !publicTransport || !sendScheduler) throw { code: "draft-reconnect" };
    return sendBrowserDraft({ session, transport: publicTransport, synchronizeDraft: async () => { await sendScheduler.poll(); }, args });
  };
  return {
    locks,
    terminate,
    network: {
      async start() { if (!scheduler || !retentionAbort) return false; const ready = await scheduler.ensureRetentionNegotiated(retentionAbort.signal); if (!polling) { polling = true; schedulePoll(); } return ready; },
      wake() { void scheduler?.poll().catch(() => undefined); },
      draftSaved() { draftSyncPrompt.schedule(); },
      flushDraftSync() { draftSyncPrompt.flush(); },
       startHistorySearch(attemptId) { scheduler?.trackHistorySearch(attemptId); void scheduler?.poll().catch(() => undefined); },
       stopHistorySearch(attemptId) { scheduler?.untrackHistorySearch(attemptId); },
      retryAttachment(id) { scheduler?.retryAttachment(id); },
      async unlockNewEpoch(passphrase) { if (!scheduler) throw { code: "unavailable" }; await scheduler.unlockNewEpoch(passphrase); },
      epochStatus() { const state = scheduler?.epochStatus?.state; return state === "epoch-mismatch" || state === "needs-unlock" ? state : undefined; },
      syncProgress() { return syncProgress; },
      producerReady() { return scheduler?.producersReady ?? false; },
      async attachmentRetentionStatus() {
        return retentionStatus?.status() ?? { kind: "temporarily_unavailable" };
      },
      stop: stopNetwork,
      subscribeChanges(listener) { changeListeners.add(listener); return () => changeListeners.delete(listener); },
      async enrollmentDiscover(origin) { return enrollment?.discover(origin) ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
       async enrollmentCreateOwner(secrets, username) { return enrollment?.createOwner(secrets, username) ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
        async enrollmentSignIn(adminPassword, username) { return enrollment?.signIn(adminPassword, username) ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
       async enrollmentSetHostedCsrf(csrf) { enrollment?.setHostedCsrf(csrf); },
       async enrollmentConnectSharedSecret(passphrase, vaultOwner) { return enrollment?.connectSharedSecret(passphrase, vaultOwner) ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
       async enrollmentBeginVaultRequest(inviteLink, displayName) { return enrollment?.beginVaultRequest(inviteLink, displayName) ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
       async enrollmentVaultRequestStatus() { return enrollment?.vaultRequestStatus() ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
       async enrollmentCreateVault(username, secrets) { return enrollment?.createVault(username, secrets) ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
       async enrollmentPendingVaultRequest() { return enrollment?.pendingVaultRequest() ?? null; },
       async enrollmentAddPhone(method) { return enrollment?.addPhone(method) ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
       async enrollmentConnectComputer() { return enrollment?.connectComputer() ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
       async enrollmentComputerStatus() { return enrollment?.computerStatus() ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
       async enrollmentConfirmComputer() { return enrollment?.confirmComputer() ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
       async enrollmentCancelComputer() { return enrollment?.cancelComputer() ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
      async enrollmentStatus() { return enrollment?.status() ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
       async enrollmentApprove() { return enrollment?.approve() ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
       async enrollmentCancel(options) { return enrollment?.cancel(options); },
      async enrollmentSignOut() { await enrollment?.signOut(); },
      async enrollmentGenerateSecret() { if (!enrollment) throw { code: "unavailable" }; return enrollment.generateSecret(); },
      async enrollmentManagement() { if (!enrollment) throw { code: "unavailable" }; return enrollment.management(); },
       async enrollmentRevokeDevice(deviceId) { if (!enrollment) throw { code: "unavailable" }; return enrollment.revokeDevice(deviceId); },
       async enrollmentListVaults() { if (!enrollment) throw { code: "unavailable" }; return enrollment.listVaults(); },
       async enrollmentCreateInvite() { if (!enrollment) throw { code: "unavailable" }; return enrollment.createInvite(); },
       async enrollmentListInvites() { if (!enrollment) throw { code: "unavailable" }; return enrollment.listInvites(); },
       async enrollmentRevokeInvite(inviteId) { if (!enrollment) throw { code: "unavailable" }; await enrollment.revokeInvite(inviteId); },
       async enrollmentListVaultRequests() { if (!enrollment) throw { code: "unavailable" }; return enrollment.listVaultRequests(); },
       async enrollmentApproveVaultRequest(requestId) { if (!enrollment) throw { code: "unavailable" }; await enrollment.approveVaultRequest(requestId); },
        async enrollmentDenyVaultRequest(requestId) { if (!enrollment) throw { code: "unavailable" }; await enrollment.denyVaultRequest(requestId); },
        async listDevices() { if (!publicTransport) throw new TransportError("invalid"); return publicTransport.json("/v1/devices", { method: "GET" }); },
       async revokeDevice(deviceId) { if (!publicTransport) throw new TransportError("invalid"); await publicTransport.discard(`/v1/devices/${deviceId}/revoke`, { method: "POST" }); },
       async mcpStatus() { if (!enrollment) throw new TransportError("invalid"); return enrollment.mcpStatus(); },
       async mcpEnable() { if (!enrollment) throw new TransportError("invalid"); return enrollment.mcpEnable(); },
        async mcpGrants() { if (!enrollment) throw new TransportError("invalid"); return enrollment.mcpGrants(); },
        async mcpRevokeGrant(id) { if (!enrollment) throw new TransportError("invalid"); await enrollment.mcpRevokeGrant(id); },
        async webhooksList() { if (!enrollment) throw new TransportError("invalid"); return enrollment.webhooksList(); },
        async webhookCreate(input) { if (!enrollment) throw new TransportError("invalid"); return enrollment.webhookCreate(input); },
        async webhookUpdate(id, input) { if (!enrollment) throw new TransportError("invalid"); return enrollment.webhookUpdate(id, input); },
        async webhookDelete(id) { if (!enrollment) throw new TransportError("invalid"); await enrollment.webhookDelete(id); },
        async webhookRevealSecret(id) { if (!enrollment) throw new TransportError("invalid"); return enrollment.webhookRevealSecret(id); },
        async webhookRotateSecret(id) { if (!enrollment) throw new TransportError("invalid"); return enrollment.webhookRotateSecret(id); },
        async webhookTest(id) { if (!enrollment) throw new TransportError("invalid"); return enrollment.webhookTest(id); },
       async mcpAuthorization(id) { if (!enrollment) throw new TransportError("invalid"); return enrollment.mcpAuthorization(id); },
       async mcpApproveAuthorization(id, scopes) { if (!enrollment) throw new TransportError("invalid"); return enrollment.mcpApproveAuthorization(id, scopes); },
       async mcpDenyAuthorization(id) { if (!enrollment) throw new TransportError("invalid"); return enrollment.mcpDenyAuthorization(id); },
      async accountAccess() {
         return accountAccess?.status() ?? { kind: "temporarily_unavailable" };
       },
      sendDraft,
       },
    async boot(_signal, onFatal) {
      const modulePromise = loadBrowserCore();
      const core = new BrowserCore(() => modulePromise);
      const module = await modulePromise as EmscriptenCoreModule & { FS: EmscriptenFilesystem };
      const origins = await browserOrigins();
      const session = await BrowserSession.boot({ origin: origins.logicalOrigin, core, filesystem: module.FS, checkpoint: new IndexedDbCheckpointStore(OWNER_NAME), rememberedUnlock: browserRememberedUnlock(origins.fetchBase), liveTeardown: () => liveSupervisor?.stop() ?? Promise.resolve(), onFatal });
      activeSession = session;
        enrollmentAbort = new AbortController();
        enrollment = new BrowserEnrollment(session.enrollmentCore(), session, origins.logicalOrigin, undefined, origins.fetchBase, enrollmentAbort.signal);
       publicTransport = new OriginTransport({ origin: self.location.origin, deviceToken: () => session.tokenForTransport() });
       liveSupervisor = new LiveSupervisor({
         transport: publicTransport,
         origin: self.location.origin,
         socket: (url, protocol) => new WebSocket(url, protocol),
       });
       retentionGeneration += 1;
       retentionAbort = new AbortController();
       retention = new AttachmentRetentionDriver(session, { retention: (request, signal) => publicTransport!.retention(request, signal) });
       retentionStatus = new AttachmentRetentionStatusFacade(session, { retention: (request, signal) => publicTransport!.retention(request, signal) }, () => retentionGeneration, () => retentionAbort!.signal);
       accountAccess = new AccountAccessDriver(session, { retention: (request, signal) => publicTransport!.retention(request, signal) }, () => [origins.logicalOrigin, origins.accountOrigin].filter((origin): origin is string => origin !== undefined), () => retentionGeneration, () => retentionAbort!.signal);
       scheduler = new BrowserScheduler(
        session,
        publicTransport,
        {
          media: { readCiphertext: (file, expectedBytes, signal) => session.readCiphertext(file, expectedBytes, signal), stageDownload: (bytes, signal) => session.stageDownload(bytes, signal), cleanupStagedDownload: source => session.cleanupStagedDownload(source) },
          rotateEpoch: (profile, header, passphrase) => session.rotate(profile, header, passphrase),
           onProgress: progress => {
            syncProgress = progress;
            for (const listener of changeListeners) listener();
            },
           retention,
        },
       );

       return session;
    },
    legacySettings: new IndexedDbNotificationSettings(`${OWNER_NAME}-presentation`),
    publisher: {
      origin: self.location.origin,
      postPublicCopy: (path, bytes, request) => {
        if (!publicTransport) throw { code: "unavailable" };
        return publicTransport.postPublicCopy(path, bytes, request);
      },
    },
  };
}

function isTerminalDraftSendRejection(error: unknown): error is TransportError {
  return error instanceof TransportError
    && error.kind === "status"
    && ((error.status === 409 && ["draft_already_sent", "draft_revision_not_head"].includes(error.code ?? ""))
      || (error.status === 410 && ["draft_sent", "draft_discarded", "draft_expired"].includes(error.code ?? "")));
}
