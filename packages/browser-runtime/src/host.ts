import { IndexedDbCheckpointStore } from "./checkpoint.js";
import { BrowserCore, type CoreResult, type EmscriptenCoreModule } from "./core.js";
import { type EmscriptenFilesystem } from "./filesystem.js";
import { acquireWorkerOwner, type OwnerLockManager, type WorkerOwner } from "./owner.js";
import { BrowserSession, BrowserSessionError, type BrowserSessionPhase } from "./session.js";
import { BrowserEnrollment, type EnrollmentView, type InstanceManagement } from "./enrollment.js";
import { BrowserScheduler } from "./scheduler.js";
import { OriginTransport } from "./transport.js";
import { BannerPresenter, BrowserNotificationAdapter, IndexedDbNotificationSettings, notificationPreferences, type BannerContext, type NotificationAdapter, type NotificationSettings } from "./presentation.js";
import { publishAttachment, type PublicCopyTransport } from "./publication.js";

const MAX_RPC_BYTES = 1024 * 1024;
const MAX_CONTACT_ADDRESSES = 500;
const OWNER_NAME = "peppy-browser-v1";

export type RpcError = { code: string; message: string; currentRevision?: string };
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
  enrollmentCreateOwner(secrets: { setupToken?: string; adminPassword?: string; passphrase: string }): Promise<EnrollmentView>;
  enrollmentSignIn(adminPassword: string): Promise<EnrollmentView>;
  enrollmentSetHostedCsrf(csrf: string): Promise<void>;
  enrollmentConnectSharedSecret(passphrase: string): Promise<EnrollmentView>;
  enrollmentAddPhone(method: "transfer" | "shared_secret"): Promise<EnrollmentView>;
  enrollmentStatus(): Promise<EnrollmentView>;
  enrollmentApprove(passphrase: string): Promise<EnrollmentView>;
  enrollmentCancel(): Promise<void>;
  enrollmentSignOut(): Promise<void>;
  enrollmentGenerateSecret(): Promise<string>;
  enrollmentManagement(): Promise<InstanceManagement>;
  enrollmentRevokeDevice(deviceId: string): Promise<InstanceManagement>;
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
  save_draft: { coreCommand: "save_draft", mutation: true },
  send_draft: { coreCommand: "send_draft", mutation: true },
  mark_seen: { coreCommand: "mark_seen", mutation: true },
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
  list_restorable_contacts: { coreCommand: "list_restorable_contacts", mutation: false },
  restore_contact: { coreCommand: "restore_contact", mutation: true },
  contact_snapshot: { coreCommand: "contact_snapshot", mutation: false },
};

const NATIVE_ONLY = new Set([
  "configure_server", "get_app_lock_status", "set_app_lock", "unlock_app", "pick_attachments", "save_attachment",
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
]);

const ENROLLMENT_ACTIVATIONS = new Set([
  "enrollment_create_owner",
  "enrollment_connect_shared_secret",
]);

function isReadyEnrollmentView(value: unknown): boolean {
  return typeof value === "object" && value !== null && !Array.isArray(value) && (value as Record<string, unknown>).stage === "ready";
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
  if (error instanceof BrowserSessionError) return { code: error.code, message: "Browser session is not ready." };
  if (typeof error === "object" && error !== null && "code" in error && typeof error.code === "string") {
    const revision = "details" in error && typeof error.details === "object" && error.details !== null &&
      "currentRevision" in error.details && typeof error.details.currentRevision === "string" ? error.details.currentRevision : undefined;
    const codes = new Set(["already-open", "attachment-invalid", "attachment-local", "core", "database-key-mismatch", "gateway-offline", "gateway-unavailable", "gateway-unsupported", "gateways-unknown", "invalid-request", "locked", "not-found", "request-too-large", "stale-draft", "unknown-command", "unlock-failed", "invalid-contact-edit", "contact-book-read-only", "contact-field-read-only", "contact-photo-invalid", "identity-exists", "identity-unavailable", "epoch-mismatch", "invalid-rotation", "unavailable", "invalid-draft", "invalid-route", "invalid-recipient", "invalid-attachment", "empty-message", "message-too-long", "mms-unsupported", "mms-too-many-recipients", "mms-too-large", "mms-reply-blocked", "public-copy-unsupported", "public-copy-too-large", "host-state", "device-revoked", "unsupported", "http-401", "http-403", "invalid_setup_token", "instance_already_claimed"]);
    const code = codes.has(error.code) ? error.code : "unavailable";
    return { code, message: "Browser operation failed.", currentRevision: revision };
  }
  return { code: "unavailable", message: "Browser operation failed." };
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
  const wrapper = command === "save_draft" || command === "send_draft" || command === "submit_contact_edit" ? "input"
    : command === "set_notification_preferences" ? "preferences" : undefined;
  if (!wrapper) return args;
  const value = args[wrapper];
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw { code: "invalid-request" };
  return value as Record<string, unknown>;
}

/** Dispatches a renderer request without exposing a generic Rust command or worker-private APIs. */
export async function dispatchBrowserRpc(session: HostSession, request: unknown, settings?: NotificationSettings, publisher?: PublicCopyTransport & { origin: string }): Promise<RpcResponse> {
  if (!validRequest(request)) return { id: requestId(request), ok: false, error: { code: "invalid-request", message: "The request is invalid." } };
  const { id, command, args } = request;
  try {
    if (command === "load_state" && session.phase !== "ready") return { id, ok: true, value: safeSnapshot(session.phase) };
    if (command === "load_state") {
      // Settings are restored before the Rust snapshot is presented, never as message state.
      const savedPreferences = await settings?.load();
       const snapshot = await session.query("snapshot", args);
       if (typeof snapshot !== "object" || snapshot === null || Array.isArray(snapshot)) throw { code: "invalid-request" };
       const contacts = await session.query("contact_snapshot", { addresses: contactAddresses(snapshot as Record<string, unknown>) });
       if (typeof contacts !== "object" || contacts === null || Array.isArray(contacts)) throw { code: "invalid-request" };
        return { id, ok: true, value: { ...await addVisiblePreviews(session, snapshot as Record<string, unknown>), ...contacts as Record<string, unknown>, ...(savedPreferences ? { notificationPreferences: savedPreferences } : {}) } };
    }
    if (command === "unlock") {
      const passphrase = secretString(args, "passphrase");
      if (!passphrase) throw { code: "invalid-request" };
      await session.unlock(passphrase);
      return { id, ok: true, value: undefined };
    }
    if (command === "prepare_attachment") return { id, ok: true, value: await addAttachmentPreview(session, await prepareAttachment(session, args)) };
    if (command === "export_attachment") return { id, ok: true, value: await exportAttachment(session, args) };
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

async function addVisiblePreviews(session: HostSession, snapshot: Record<string, unknown>): Promise<Record<string, unknown>> {
  if (!session.previewAttachment || !Array.isArray(snapshot.conversations)) return snapshot;
  const activeId = typeof snapshot.activeConversationId === "string" ? snapshot.activeConversationId : undefined;
  const conversation = snapshot.conversations.find(value => typeof value === "object" && value !== null && (value as Record<string, unknown>).id === activeId);
  if (!conversation || typeof conversation !== "object" || conversation === null) return snapshot;
  const view = conversation as Record<string, unknown>;
  if (!Array.isArray(view.messages)) return snapshot;
  let remainingPreviews = 32;
  const messages = await Promise.all(view.messages.map(async message => {
    if (typeof message !== "object" || message === null) return message;
    const messageView = message as Record<string, unknown>;
    if (!Array.isArray(messageView.attachments)) return message;
    const attachments = await Promise.all(messageView.attachments.map(async attachment => {
      const preview = remainingPreviews > 0 && previewableAttachment(attachment);
      if (preview) remainingPreviews -= 1;
      return preview ? addAttachmentPreview(session, attachment) : attachment;
    }));
    return { ...messageView, attachments };
  }));
  return { ...snapshot, conversations: snapshot.conversations.map(value => value === conversation ? { ...view, messages } : value) };
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
      this.owner?.owner.shutdown();
      try { await this.owner?.owner.drain?.(); } catch {}
      acknowledge?.();
      for (const port of this.ports) { port.postMessage({ event: "stopped", reason }); port.close(); }
      this.ports.clear();
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
  }

  private async handle(port: BrowserPort, request: unknown): Promise<void> {
    if (!this.owner || this.stopping) return;
    if (validRequest(request) && request.command === "lock_sync") {
      await this.stop("locked", () => port.postMessage({ id: request.id, ok: true, value: undefined }));
      return;
    }
    if (validRequest(request) && request.command === "disconnect") {
      this.ports.delete(port);
      if (this.presentationPort === port) { this.presentationPort = undefined; this.presenter?.clear(); }
      port.postMessage({ event: "stopped", reason: "disconnected" });
      port.close();
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
      try { await this.dependencies.network.unlockNewEpoch(passphrase); port.postMessage({ id: request.id, ok: true, value: undefined }); this.changed(); this.dependencies.network.wake?.(); } catch (error) { port.postMessage({ id: request.id, ok: false, error: errorFor(error) }); }
      return;
    }
    if (validRequest(request) && request.command.startsWith("enrollment_")) {
      const response = await this.enrollment(request);
      port.postMessage(response);
      if (response.ok && ENROLLMENT_MUTATIONS.has(request.command)) {
        this.changed();
        if (ENROLLMENT_ACTIVATIONS.has(request.command) && this.owner.owner.phase === "ready" && isReadyEnrollmentView(response.value)) {
          await this.dependencies.network?.start();
        }
      }
      if (this.owner.owner.phase === "closed") await this.stop("closed");
      return;
    }
    const generation = this.owner.owner.generation;
    const command = validRequest(request) ? request.command : undefined;
    let response = await dispatchBrowserRpc(this.owner.owner, request, this.dependencies.settings, this.dependencies.publisher);
    if (response.ok && command === "load_state" && this.dependencies.network?.epochStatus?.()) {
      const value = response.value;
      if (typeof value === "object" && value !== null && !Array.isArray(value)) {
        response = { ...response, value: { ...value as Record<string, unknown>, encryption: { ...(value as Record<string, unknown>).encryption as Record<string, unknown>, state: "mismatch" } } };
      }
    }
    port.postMessage(response);
    if (this.owner.owner.phase === "closed") {
      await this.stop("closed");
      return;
    }
    if ((response.ok && command !== undefined && (INVOCATIONS[command]?.mutation || command === "unlock" || command === "publish_attachment" || command === "retry_attachment")) || generation !== this.owner.owner.generation) {
      this.changed();
      if (command === "unlock") await this.dependencies.network?.start();
      if (command === "retry_attachment" && validRequest(request)) this.dependencies.network?.retryAttachment?.(requiredId(request.args));
      if (command === "send_draft" || command === "retry_attachment") this.dependencies.network?.wake?.();
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

  private async enrollment(request: RpcRequest): Promise<RpcResponse> {
    const network = this.dependencies.network;
    if (!network) return { id: request.id, ok: false, error: { code: "unavailable", message: "Browser operation failed." } };
    try {
      const setupToken = secretString(request.args, "setupToken");
      const adminPassword = secretString(request.args, "adminPassword");
      const csrf = secretString(request.args, "csrf");
      const passphrase = secretString(request.args, "passphrase");
      const method = request.args.method;
      const value = request.command === "enrollment_discover" ? await network.enrollmentDiscover(typeof request.args.origin === "string" ? request.args.origin : undefined)
       : request.command === "enrollment_create_owner" && passphrase ? await network.enrollmentCreateOwner({ ...(setupToken ? { setupToken } : {}), ...(adminPassword ? { adminPassword } : {}), passphrase })
          : request.command === "enrollment_sign_in" ? await network.enrollmentSignIn(adminPassword ?? "")
            : request.command === "enrollment_set_hosted_csrf" && csrf ? (await network.enrollmentSetHostedCsrf(csrf), undefined)
            : request.command === "enrollment_connect_shared_secret" && passphrase ? await network.enrollmentConnectSharedSecret(passphrase)
              : request.command === "enrollment_add_phone" && (method === "transfer" || method === "shared_secret") ? await network.enrollmentAddPhone(method)
                : request.command === "enrollment_status" ? await network.enrollmentStatus()
                  : request.command === "enrollment_approve" && passphrase ? await network.enrollmentApprove(passphrase)
                    : request.command === "enrollment_cancel" ? (await network.enrollmentCancel(), undefined)
        : request.command === "enrollment_sign_out" ? (await network.enrollmentSignOut(), undefined)
                        : request.command === "enrollment_generate_secret" ? { passphrase: await network.enrollmentGenerateSecret() }
                          : request.command === "enrollment_management" ? await network.enrollmentManagement()
                            : request.command === "enrollment_revoke_device" && typeof request.args.deviceId === "string" ? await network.enrollmentRevokeDevice(request.args.deviceId)
                          : undefined;
      if (value === undefined && request.command !== "enrollment_cancel" && request.command !== "enrollment_sign_out") throw { code: "invalid-request" };
      return { id: request.id, ok: true, value };
    } catch (error: unknown) { return { id: request.id, ok: false, error: errorFor(error) }; }
  }

  private async fatal(_error: Error): Promise<void> { await this.stop("fatal"); }

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
      wake() { void scheduler?.poll(); },
      retryAttachment(id) { scheduler?.retryAttachment(id); },
      async unlockNewEpoch(passphrase) { if (!scheduler) throw { code: "unavailable" }; await scheduler.unlockNewEpoch(passphrase); },
      epochStatus() { const state = scheduler?.epochStatus?.state; return state === "epoch-mismatch" || state === "needs-unlock" ? state : undefined; },
      stop: stopNetwork,
      subscribeChanges(listener) { changeListeners.add(listener); return () => changeListeners.delete(listener); },
      async enrollmentDiscover(origin) { return enrollment?.discover(origin) ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
      async enrollmentCreateOwner(secrets) { return enrollment?.createOwner(secrets) ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
       async enrollmentSignIn(adminPassword) { return enrollment?.signIn(adminPassword) ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
       async enrollmentSetHostedCsrf(csrf) { enrollment?.setHostedCsrf(csrf); },
      async enrollmentConnectSharedSecret(passphrase) { return enrollment?.connectSharedSecret(passphrase) ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
      async enrollmentAddPhone(method) { return enrollment?.addPhone(method) ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
      async enrollmentStatus() { return enrollment?.status() ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
      async enrollmentApprove(passphrase) { return enrollment?.approve(passphrase) ?? { mode: "community", stage: "error", errorCode: "unavailable" }; },
      async enrollmentCancel() { await enrollment?.cancel(); },
      async enrollmentSignOut() { await enrollment?.signOut(); },
      async enrollmentGenerateSecret() { if (!enrollment) throw { code: "unavailable" }; return enrollment.generateSecret(); },
      async enrollmentManagement() { if (!enrollment) throw { code: "unavailable" }; return enrollment.management(); },
      async enrollmentRevokeDevice(deviceId) { if (!enrollment) throw { code: "unavailable" }; return enrollment.revokeDevice(deviceId); },
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
