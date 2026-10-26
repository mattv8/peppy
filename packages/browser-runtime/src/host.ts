import { IndexedDbCheckpointStore } from "./checkpoint.js";
import { BrowserCore, CoreRejectedError, type CoreResult, type EmscriptenCoreModule } from "./core.js";
import { type EmscriptenFilesystem } from "./filesystem.js";
import { acquireWorkerOwner, type OwnerLockManager, type WorkerOwner } from "./owner.js";
import { BrowserSession, BrowserSessionError, type BrowserSessionPhase } from "./session.js";
import { BrowserEnrollment, type EnrollmentView, type InstanceManagement, type VaultInviteView, type VaultRequestView, type VaultSummary } from "./enrollment.js";
import { BrowserScheduler } from "./scheduler.js";
import { OriginTransport, TransportError } from "./transport.js";
import { BannerPresenter, BrowserNotificationAdapter, IndexedDbNotificationSettings, notificationPreferences, type BannerContext, type NotificationAdapter, type NotificationSettings } from "./presentation.js";
import { publishAttachment, type PublicCopyTransport } from "./publication.js";

const MAX_RPC_BYTES = 1024 * 1024;
const MAX_CONTACT_ADDRESSES = 500;
const MAX_PREVIEWS_PER_SNAPSHOT = 32;
const MAX_PREVIEWS_PER_HISTORY_PAGE = 12;
const MAX_INLINE_MEDIA_BYTES = 32 * 1024 * 1024;
const OWNER_NAME = "peppy-browser-v1";
const SEARCH_INDEX_DELAY_MS = 1_000;
const SEARCH_INDEX_MAX_RETRIES = 3;
const SEARCH_INDEX_IDLE_RETRY_MS = 100;

export type RpcError = { code: string; message: string; conversationId?: string; currentRevision?: string; draftId?: string };
export type RpcResponse = { id: number; ok: true; value: unknown } | { id: number; ok: false; error: RpcError };
export type RpcRequest = { id: number; command: string; args: Record<string, unknown> };
export type WorkerMessage = RpcRequest | { event: "changed" } | { event: "ready"; version: 1 } | { event: "stopped"; reason: string } | { event: "banner"; candidate: Record<string, unknown> };

export interface BrowserPort {
  postMessage(message: WorkerMessage | RpcResponse): void;
  start?(): void;
  close(): void;
  onmessage: ((event: MessageEvent<unknown>) => void) | null;
}

export interface HostSession {
  readonly phase: BrowserSessionPhase;
  query(command: string, args: Record<string, unknown>): Promise<CoreResult>;
  mutate(command: string, args: Record<string, unknown>): Promise<CoreResult>;
  unlock(passphrase: string): Promise<void>;
  enroll(metadata: unknown, deviceToken: string, passphrase: string): Promise<void>;
  shutdown(): void;
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
  start(): Promise<void>;
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
  subscribeChanges?(listener: () => void): () => void;
  retryAttachment?(id: string): void;
  wake?(): void;
  unlockNewEpoch?(passphrase: string): Promise<void>;
  epochStatus?(): "epoch-mismatch" | "needs-unlock" | undefined;
}

export interface BrowserHostDependencies {
  boot(signal: AbortSignal, onFatal: (error: Error) => void): Promise<HostSession>;
  locks?: OwnerLockManager;
  terminate?(): void;
  network?: BrowserNetwork;
  settings?: NotificationSettings;
  publisher?: PublicCopyTransport & { origin: string };
  notification?: NotificationAdapter;
}

type Invocation = { coreCommand: string; mutation: boolean };

const INVOCATIONS: Readonly<Record<string, Invocation>> = {
  load_state: { coreCommand: "snapshot", mutation: false },
  load_history: { coreCommand: "load_history", mutation: false },
  save_draft: { coreCommand: "save_draft", mutation: true },
  send_draft: { coreCommand: "send_draft", mutation: true },
  discard_draft: { coreCommand: "discard_draft", mutation: true },
  mark_seen: { coreCommand: "mark_seen", mutation: true },
  evict_attachment_ciphertext: { coreCommand: "evict_attachment_ciphertext", mutation: true },
  dismiss_notification: { coreCommand: "dismiss_notification", mutation: true },
  dismiss_all_notifications: { coreCommand: "dismiss_all_notifications", mutation: true },
  set_app_muted: { coreCommand: "set_app_muted", mutation: true },
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
};

const NATIVE_ONLY = new Set([
  "configure_server", "get_app_lock_status", "set_app_lock", "unlock_app", "pick_attachments",
  "open_composer", "set_start_at_login", "popout_conversation", "hide_head",
  "close_composer", "close_head_panel", "window", "request_notification_permission", "set_notification_context",
  "subscribe_lifecycle", "subscribe_lifecycle_finished", "acknowledge_lifecycle", "hosted_account",
  "hosted_sign_in", "hosted_sign_out", "hosted_open_billing", "hosted_provision",
]);

const ENROLLMENT_MUTATIONS = new Set([
  "enrollment_create_owner",
  "enrollment_sign_in",
  "enrollment_set_hosted_csrf",
  "enrollment_connect_shared_secret",
  "enrollment_add_phone",
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
]);

function isReadyEnrollmentView(value: unknown): boolean {
  return typeof value === "object" && value !== null && !Array.isArray(value) && (value as Record<string, unknown>).stage === "ready";
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

function safeSnapshot(phase: BrowserSessionPhase): Record<string, unknown> {
  return {
    version: "1", mode: "browser", connection: { state: "offline" },
    encryption: { state: phase === "unenrolled" ? "preview" : "locked" }, gateways: [], conversations: [],
    head: { enabled: false, capability: "unsupported" }, pendingCount: 0, quarantineCount: 0,
    notifications: [], appFilters: [], notificationPreferences: { messageBanners: true, mirroredBanners: true, preview: "full" },
  };
}

function errorFor(error: unknown): RpcError {
  if (error instanceof TransportError) {
    return { code: error.kind === "offline" ? "offline" : "unavailable", message: "Browser operation failed." };
  }
  if (error instanceof BrowserSessionError) return { code: error.code, message: "Browser session is not ready." };
  if (typeof error === "object" && error !== null && "code" in error && typeof error.code === "string") {
    const details = errorDetails(error);
    const codes = new Set(["already-open", "attachment-invalid", "attachment-local", "busy", "core", "database-key-mismatch", "gateway-offline", "gateway-unavailable", "gateway-unsupported", "gateways-unknown", "invalid-request", "locked", "not-found", "request-too-large", "stale-draft", "draft-moved", "draft-not-discardable", "recipientless-draft-exists", "draft-merge-overflow", "unknown-command", "unlock-failed", "invalid-contact-edit", "contact-book-read-only", "contact-field-read-only", "contact-photo-invalid", "identity-exists", "identity-unavailable", "epoch-mismatch", "invalid-rotation", "unavailable", "invalid-draft", "invalid-route", "invalid-recipient", "invalid-attachment", "empty-message", "message-too-long", "mms-unsupported", "mms-too-many-recipients", "mms-too-large", "mms-reply-blocked", "media-pending", "public-copy-unsupported", "public-copy-too-large", "host-state", "device-revoked", "unsupported", "http-401", "http-403", "invalid_setup_token", "instance_already_claimed"]);
    const code = codes.has(error.code) ? error.code : "unavailable";
    return { code, message: "Browser operation failed.", ...details };
  }
  return { code: "unavailable", message: "Browser operation failed." };
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
  if (!wrapper) return args;
  const value = args[wrapper];
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw { code: "invalid-request" };
  return value as Record<string, unknown>;
}

/** Dispatches a renderer request without exposing a generic Rust command or worker-private APIs. */
type MediaLeases = {
  acquire(id: string): boolean;
  release(id: string): boolean;
};

export async function dispatchBrowserRpc(session: HostSession, request: unknown, settings?: NotificationSettings, publisher?: PublicCopyTransport & { origin: string }, mediaLeases?: MediaLeases): Promise<RpcResponse> {
  if (!validRequest(request)) return { id: requestId(request), ok: false, error: { code: "invalid-request", message: "The request is invalid." } };
  const { id, command, args } = request;
  try {
    if (command === "load_state" && session.phase !== "ready") return { id, ok: true, value: safeSnapshot(session.phase) };
    if (command === "load_history" && session.phase !== "ready") throw { code: "locked" };
    if (command === "load_state") {
      // Settings are restored before the Rust snapshot is presented, never as message state.
      const savedPreferences = await settings?.load();
      const snapshot = await session.query("snapshot", args);
      if (typeof snapshot !== "object" || snapshot === null || Array.isArray(snapshot)) throw { code: "invalid-request" };
      const metadataOnly = args.metadataOnly === true;
      const contacts = await session.query("contact_snapshot", { addresses: contactAddresses(snapshot as Record<string, unknown>) });
      if (typeof contacts !== "object" || contacts === null || Array.isArray(contacts)) throw { code: "invalid-request" };
      const enriched = metadataOnly ? snapshot as Record<string, unknown> : await addVisiblePreviews(session, snapshot as Record<string, unknown>, MAX_PREVIEWS_PER_SNAPSHOT);
      return { id, ok: true, value: { ...enriched, ...contacts as Record<string, unknown>, ...(savedPreferences ? { notificationPreferences: savedPreferences } : {}) } };
    }
    if (command === "load_history") {
      const page = await session.query("load_history", args);
      if (typeof page !== "object" || page === null || Array.isArray(page)) throw { code: "invalid-request" };
      return { id, ok: true, value: await addHistoryPreviews(session, page as Record<string, unknown>) };
    }
    if (command === "unlock") {
      const passphrase = secretString(args, "passphrase");
      if (!passphrase) throw { code: "invalid-request" };
      await session.unlock(passphrase);
      return { id, ok: true, value: undefined };
    }
    if (command === "prepare_attachment") return { id, ok: true, value: await addAttachmentPreview(session, await prepareAttachment(session, args)) };
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
      if (settings) await settings.save(preferences);
      const value = await session.mutate(invocation.coreCommand, invocationArgs);
      return { id, ok: true, value };
    }
    const value = invocation.mutation
      ? await session.mutate(invocation.coreCommand, invocationArgs)
      : await session.query(invocation.coreCommand, invocationArgs);
    return { id, ok: true, value };
  } catch (error: unknown) {
    return { id, ok: false, error: errorFor(error) };
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

async function addVisiblePreviews(session: HostSession, snapshot: Record<string, unknown>, budget: number): Promise<Record<string, unknown>> {
  if (!session.previewAttachment || !Array.isArray(snapshot.conversations)) return snapshot;
  const activeId = typeof snapshot.activeConversationId === "string" ? snapshot.activeConversationId : undefined;
  const conversation = snapshot.conversations.find(value => typeof value === "object" && value !== null && (value as Record<string, unknown>).id === activeId);
  if (!conversation || typeof conversation !== "object" || conversation === null) return snapshot;
  const view = conversation as Record<string, unknown>;
  if (!Array.isArray(view.messages)) return snapshot;
  const messages = await addMessagePreviews(session, view.messages, budget);
  return { ...snapshot, conversations: snapshot.conversations.map(value => value === conversation ? { ...view, messages } : value) };
}

async function addHistoryPreviews(session: HostSession, page: Record<string, unknown>): Promise<Record<string, unknown>> {
  if (!Array.isArray(page.messages)) return page;
  return { ...page, messages: await addMessagePreviews(session, page.messages, MAX_PREVIEWS_PER_HISTORY_PAGE) };
}

async function addMessagePreviews(session: HostSession, messages: unknown[], budget: number): Promise<unknown[]> {
  let remainingPreviews = budget;
  const previewedMessages = await Promise.all(messages.map(async message => {
    if (typeof message !== "object" || message === null) return message;
    const messageView = message as Record<string, unknown>;
    if (!Array.isArray(messageView.attachments)) return message;
    const attachments = await Promise.all(messageView.attachments.map(async attachment => {
      const view = attachment as Record<string, unknown>;
      const mediaMeta = objectOrUndefined(view.mediaMeta);
      const thumbnail = mediaMeta?.thumbnail;
      const previewTarget = previewableAttachment(thumbnail) ? thumbnail : attachment;
      const preview = remainingPreviews > 0 && previewableAttachment(previewTarget);
      if (preview) remainingPreviews -= 1;
      if (!preview) return attachment;
      if (previewTarget === attachment) return addAttachmentPreview(session, attachment);
      return { ...view, mediaMeta: { ...mediaMeta, thumbnail: await addAttachmentPreview(session, thumbnail) } };
    }));
    return { ...messageView, attachments };
  }));
  return previewedMessages;
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
  return typeof view.id === "string" && typeof view.mediaType === "string" && /^image\/(jpeg|png|webp|gif)$/.test(view.mediaType) && view.state === "ready";
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
  const items = await Promise.all(page.items.map(async item => {
    if (typeof item !== "object" || item === null || Array.isArray(item)) return item;
    const view = item as Record<string, unknown>;
    const meta = typeof view.mediaMeta === "object" && view.mediaMeta !== null && !Array.isArray(view.mediaMeta) ? view.mediaMeta as Record<string, unknown> : undefined;
    const thumbnail = meta?.thumbnail;
    if (!thumbnail || !previewableAttachment(thumbnail)) return item;
    return { ...view, mediaMeta: { ...meta, thumbnail: await addAttachmentPreview(session, thumbnail) } };
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

/** SharedWorker coordinator: all renderer ports share the same session and serialized checkpoint owner. */
export class BrowserWorkerHost {
  private readonly ports = new Set<BrowserPort>();
  private owner?: WorkerOwner<HostSession>;
  private starting?: Promise<void>;
  private stopping = false;
  private unsubscribeNetwork?: () => void;
  private presenter?: BannerPresenter;
  private presentationPort?: BrowserPort;
  private readonly mediaLeases = new Map<BrowserPort, Map<string, number>>();
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
    if (this.stopping) return;
    this.stopping = true;
    try {
      try { await this.dependencies.network?.stop(); } catch {}
      this.unsubscribeNetwork?.();
      this.presenter?.clear();
      this.cancelSearchIndex();
      this.owner?.owner.shutdown();
      try { await this.owner?.owner.drain?.(); } catch {}
      acknowledge?.();
      for (const port of this.ports) { port.postMessage({ event: "stopped", reason }); port.close(); }
      this.ports.clear();
      this.mediaLeases.clear();
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
    this.owner = await acquireWorkerOwner({ locks: this.dependencies.locks, name: OWNER_NAME, start: signal => this.dependencies.boot(signal, error => { void this.fatal(error); }), stop: async session => { session.shutdown(); await session.drain?.(); } });
    this.presenter = new BannerPresenter(this.owner.owner, this.dependencies.settings ?? new IndexedDbNotificationSettings(`${OWNER_NAME}-presentation`), this.dependencies.notification ?? new BrowserNotificationAdapter(), () => this.presentationPort ? candidate => this.presentationPort?.postMessage({ event: "banner", candidate }) : undefined);
    this.unsubscribeNetwork = this.dependencies.network?.subscribeChanges?.(() => this.changed());
    void this.startSearchIndexWhenReady();
  }

  private async handle(port: BrowserPort, request: unknown): Promise<void> {
    if (!this.owner || this.stopping) return;
    if (validRequest(request) && request.command === "lock_sync") {
      await this.stop("locked", () => port.postMessage({ id: request.id, ok: true, value: undefined }));
      return;
    }
    if (validRequest(request) && request.command === "disconnect") {
      try { await this.releasePortMedia(port); } catch {} finally {
        this.ports.delete(port);
        this.mediaLeases.delete(port);
        if (this.presentationPort === port) { this.presentationPort = undefined; this.presenter?.clear(); }
        port.postMessage({ event: "stopped", reason: "disconnected" });
        port.close();
      }
      return;
    }
    if (validRequest(request) && request.command === "set_notification_context") {
      const context = notificationContext(request.args);
      if (!context) { port.postMessage({ id: request.id, ok: false, error: { code: "invalid-request", message: "The request is invalid." } }); return; }
      this.presentationPort = port;
      this.presenter?.setContext(context);
      port.postMessage({ id: request.id, ok: true, value: undefined });
      this.drainPresentation();
      return;
    }
    if (validRequest(request) && request.command === "display_ack") {
      const ids = request.args.ids;
      if (port !== this.presentationPort || !Array.isArray(ids) || ids.some(id => typeof id !== "string")) { port.postMessage({ id: request.id, ok: false, error: { code: "invalid-request", message: "The request is invalid." } }); return; }
      try { await this.presenter?.acknowledgeDisplayed(ids); port.postMessage({ id: request.id, ok: true, value: undefined }); } catch (error) { port.postMessage({ id: request.id, ok: false, error: errorFor(error) }); }
      return;
    }
    if (validRequest(request) && request.command === "unlock" && this.owner.owner.phase === "ready") {
      const passphrase = secretString(request.args, "passphrase");
      if (!passphrase || !this.dependencies.network?.unlockNewEpoch) { port.postMessage({ id: request.id, ok: false, error: { code: "invalid-request", message: "The request is invalid." } }); return; }
      try { await this.dependencies.network.unlockNewEpoch(passphrase); void this.startSearchIndexWhenReady(); port.postMessage({ id: request.id, ok: true, value: undefined }); this.changed(); this.dependencies.network.wake?.(); } catch (error) { port.postMessage({ id: request.id, ok: false, error: errorFor(error) }); }
      return;
    }
    if (validRequest(request) && request.command.startsWith("enrollment_")) {
      const response = await this.enrollment(request);
      port.postMessage(response);
      if (response.ok && ENROLLMENT_MUTATIONS.has(request.command)) {
        this.changed();
        if (ENROLLMENT_ACTIVATIONS.has(request.command) && this.owner.owner.phase === "ready" && isReadyEnrollmentView(response.value)) {
          await this.dependencies.network?.start();
          void this.startSearchIndexWhenReady();
        }
      }
      if (this.owner.owner.phase === "closed") await this.stop("closed");
      return;
    }
    if (validRequest(request) && (request.command === "list_devices" || request.command === "revoke_device")) {
      const response = await this.deviceManagement(request);
      port.postMessage(response);
      if (response.ok && request.command === "revoke_device") this.changed();
      return;
    }
    if (validRequest(request) && request.command === "advance_search_index") {
      const response = await this.explicitSearchIndexAdvance(request);
      if (this.ports.has(port)) port.postMessage(response);
      return;
    }
    const generation = this.visibleGeneration();
    const command = validRequest(request) ? request.command : undefined;
    let response: RpcResponse;
    this.dispatchesInFlight += 1;
    try {
      response = await dispatchBrowserRpc(this.owner.owner, request, this.dependencies.settings, this.dependencies.publisher, this.portMediaLeases(port));
    } finally {
      this.dispatchesInFlight -= 1;
    }
    if (response.ok && command === "load_state" && this.dependencies.network?.epochStatus?.()) {
      const value = response.value;
      if (typeof value === "object" && value !== null && !Array.isArray(value)) {
        response = { ...response, value: { ...value as Record<string, unknown>, encryption: { ...(value as Record<string, unknown>).encryption as Record<string, unknown>, state: "mismatch" } } };
      }
    }
    if (!this.ports.has(port)) return;
    port.postMessage(response);
    if (this.owner.owner.phase === "closed") {
      await this.stop("closed");
      return;
    }
    const mutationChanged = command !== "advance_search_index" && command !== undefined && INVOCATIONS[command]?.mutation && (response.ok || (!response.ok && response.error.code === "draft-moved"));
    const successfulSideEffect = response.ok && (command === "unlock" || command === "publish_attachment" || command === "retry_attachment");
    if (mutationChanged || successfulSideEffect || generation !== this.visibleGeneration()) {
      this.changed();
      if (command === "unlock") {
        await this.dependencies.network?.start();
        void this.startSearchIndexWhenReady();
      }
      if (command === "retry_attachment" && validRequest(request)) this.dependencies.network?.retryAttachment?.(requiredId(request.args));
      if (command === "send_draft" || command === "retry_attachment" || command === "request_contact_sync" || command === "request_attachment_download" || command === "load_media" || command === "save_attachment") this.dependencies.network?.wake?.();
    }
    if (response.ok && command === "search_local") {
      this.recordSearchIndexState(response.value);
      if (isIndexing(response.value)) this.scheduleSearchIndex();
    }
  }

  private broadcastChanged(): void {
    for (const port of this.ports) port.postMessage({ event: "changed" });
  }

  private changed(): void {
    this.broadcastChanged();
    this.drainPresentation();
  }

  private drainPresentation(): void { void this.presenter?.drain().catch(() => undefined); }

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

  private async enrollment(request: RpcRequest): Promise<RpcResponse> {
    const network = this.dependencies.network;
    if (!network) return { id: request.id, ok: false, error: { code: "unavailable", message: "Browser operation failed." } };
    try {
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
      if (value === undefined && !["enrollment_cancel", "enrollment_sign_out", "enrollment_revoke_invite", "enrollment_approve_vault_request", "enrollment_deny_vault_request"].includes(request.command)) throw { code: "invalid-request" };
      return { id: request.id, ok: true, value };
    } catch (error: unknown) { return { id: request.id, ok: false, error: errorFor(error) }; }
  }

  private async deviceManagement(request: RpcRequest): Promise<RpcResponse> {
    const network = this.dependencies.network;
    if (!network) return { id: request.id, ok: false, error: { code: "unavailable", message: "Browser operation failed." } };
    try {
      if (request.command === "list_devices") {
        const identity = object(await this.owner!.owner.query("_worker_identity_metadata", {}));
        return { id: request.id, ok: true, value: deviceRoster(await network.listDevices(), requiredDeviceId(identity.deviceId)) };
      }
      const deviceId = requiredDeviceId(request.args.deviceId);
      await network.revokeDevice(deviceId);
      return { id: request.id, ok: true, value: undefined };
    } catch (error: unknown) {
      return { id: request.id, ok: false, error: deviceManagementError(error) };
    }
  }

  private async fatal(_error: Error): Promise<void> { await this.stop("fatal"); }

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

function notificationContext(args: Record<string, unknown>): BannerContext | undefined {
  if (args.view !== "conversations" && args.view !== "notifications" && args.view !== "settings" && args.view !== "contacts") return undefined;
  if (args.conversationId !== undefined && typeof args.conversationId !== "string") return undefined;
  if (args.notificationPermission !== undefined && args.notificationPermission !== "granted" && args.notificationPermission !== "denied" && args.notificationPermission !== "default" && args.notificationPermission !== "unsupported") return undefined;
  if (args.focused !== undefined && typeof args.focused !== "boolean") return undefined;
  return { view: args.view, ...(typeof args.conversationId === "string" ? { conversationId: args.conversationId } : {}), ...(typeof args.notificationPermission === "string" ? { notificationPermission: args.notificationPermission } : {}), ...(typeof args.focused === "boolean" ? { focused: args.focused } : {}) };
}


export async function loadBrowserCore(): Promise<EmscriptenCoreModule> {
  const coreEntrypoint = "/core/peppy-browser-core.js";
  const module = await import(/* @vite-ignore */ coreEntrypoint) as { default?: (options: { locateFile(file: string): string }) => Promise<EmscriptenCoreModule> };
  if (!module.default) throw new Error("Browser core is unavailable");
  return module.default({ locateFile: file => file.endsWith(".wasm") ? "/core/peppy_browser_core.wasm" : `/core/${file}` });
}

async function browserOrigins(): Promise<{ logicalOrigin: string; fetchBase: string }> {
  const fetchBase = self.location.origin;
  try {
    const response = await fetch("/web/config.json", { credentials: "omit" });
    if (!response.ok) return { logicalOrigin: fetchBase, fetchBase };
    const config: unknown = await response.json();
    const apiOrigin = typeof config === "object" && config !== null ? (config as { apiOrigin?: unknown }).apiOrigin : undefined;
    const parsed = typeof apiOrigin === "string" ? new URL(apiOrigin) : undefined;
    if (!parsed || parsed.protocol !== "https:" || parsed.username || parsed.password) return { logicalOrigin: fetchBase, fetchBase };
    return { logicalOrigin: parsed.origin, fetchBase };
  } catch { return { logicalOrigin: fetchBase, fetchBase }; }
}

export function browserHostDependencies(locks: OwnerLockManager | undefined, terminate: () => void): BrowserHostDependencies {
  let enrollment: BrowserEnrollment | undefined;
  let scheduler: BrowserScheduler | undefined;
  let publicTransport: OriginTransport | undefined;
  let pollTimer: ReturnType<typeof setTimeout> | undefined;
  let polling = false;
  let pollPromise: Promise<void> | undefined;
  let pollDelay = 5_000;
  const changeListeners = new Set<() => void>();
  const stopNetwork = async () => {
    polling = false;
    if (pollTimer !== undefined) clearTimeout(pollTimer);
    pollTimer = undefined;
    scheduler?.stop();
    await pollPromise?.catch(() => undefined);
    enrollment?.cancel();
  };
  const schedulePoll = () => {
    if (!polling || !scheduler) return;
    pollPromise = scheduler.poll().then(status => {
      for (const listener of changeListeners) listener();
      if (status.connection === "revoked") polling = false;
      pollDelay = status.connection === "connected" ? 5_000 : Math.min(pollDelay * 2, 60_000);
    }).catch(() => {
      for (const listener of changeListeners) listener();
      pollDelay = Math.min(pollDelay * 2, 60_000);
    }).finally(() => {
      pollPromise = undefined;
      if (polling) pollTimer = setTimeout(schedulePoll, pollDelay);
    });
  };
  return {
    locks,
    terminate,
    network: {
      async start() { if (polling || !scheduler) return; polling = true; schedulePoll(); },
      wake() { void scheduler?.poll().catch(() => undefined); },
      retryAttachment(id) { scheduler?.retryAttachment(id); },
      async unlockNewEpoch(passphrase) { if (!scheduler) throw { code: "unavailable" }; await scheduler.unlockNewEpoch(passphrase); },
      epochStatus() { const state = scheduler?.epochStatus?.state; return state === "epoch-mismatch" || state === "needs-unlock" ? state : undefined; },
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
      },
    async boot(_signal, onFatal) {
      const modulePromise = loadBrowserCore();
      const core = new BrowserCore(() => modulePromise);
      const module = await modulePromise as EmscriptenCoreModule & { FS: EmscriptenFilesystem };
      const origins = await browserOrigins();
      const session = await BrowserSession.boot({ origin: origins.logicalOrigin, core, filesystem: module.FS, checkpoint: new IndexedDbCheckpointStore(OWNER_NAME), onFatal });
        enrollment = new BrowserEnrollment(core, session, origins.logicalOrigin, undefined, origins.fetchBase);
      publicTransport = new OriginTransport({ origin: self.location.origin, deviceToken: () => session.tokenForTransport() });
      scheduler = new BrowserScheduler(
        session,
        publicTransport,
        { media: { readCiphertext: (file, expectedBytes, signal) => session.readCiphertext(file, expectedBytes, signal), stageDownload: (bytes, signal) => session.stageDownload(bytes, signal), cleanupStagedDownload: source => session.cleanupStagedDownload(source) }, rotateEpoch: (profile, header, passphrase) => session.rotate(profile, header, passphrase) },
      );
      return session;
    },
    settings: new IndexedDbNotificationSettings(`${OWNER_NAME}-presentation`),
    publisher: {
      origin: self.location.origin,
      postPublicCopy: (path, bytes, request) => {
        if (!publicTransport) throw { code: "unavailable" };
        return publicTransport.postPublicCopy(path, bytes, request);
      },
    },
  };
}
