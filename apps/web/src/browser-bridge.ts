import type {
  AppLockStatus, AttachmentView, BridgeError, ComputerClaimantView, ContactBookView, ContactEditOutcome,
  ContactEditRequestInput, ContactEditStatus, ContactScanStatus, ContactSyncStatus, ContactView,
  DesktopBridge, DesktopSnapshot, DeviceRosterView, Draft, DraftInput, HistoryPage,
  HistorySearchAttemptInput, HistorySearchSnapshot, LocalSearchPage, LocalSearchRequest, MaximizeButtonRegion, MediaCursor, MediaOpen, MediaPage, MessageWindow,
  MessageWindowRequest, NotificationCheck, NotificationPreferences, NotificationTarget, OneTimeCodeTarget, PublicCopy,
  RecipientSuggestion, RestorableContact, SearchIndexProgress, SendDraftInput, SendResultView, StartHistorySearchInput,
  BillingSummaryView, HostedSignInProvider, PurchaseOptions, StartupStatus, StoreProduct,
  AccountAccessResult, AttachmentRetentionStateDto, AttachmentRetentionStatusResult, ConversationFolder, NotificationPersistence, NotificationPersistenceSettings, PastedImage,
} from "@peppy/desktop-ui/bridge-contracts";
import { DEFAULT_NOTIFICATION_PERSISTENCE, notificationPersistence } from "@peppy/desktop-ui/bridge-contracts";
import { peppyCopy } from "../../desktop/src/generated/peppyCopy";
import { validateAccountUrl } from "./account-navigation";
import { beginHostedAuthorization, fetchHostedWebConfig } from "./newhosted-app-auth";
import { appSessionRenewal } from "./app-session-renewal";
import { inlineMediaType, safeSuggestedFilename } from "./attachment-safety";
import { groupDraftFields, normalizeGroupSnapshot, normalizeMessageWindow, parseSendResult } from "./group-payload";

/** Runs a bridge call whose input validation may throw, reporting the failure as a rejected promise. */
function callChecked<T>(call: () => Promise<T>): Promise<T> {
  try {
    return call();
  } catch (error) {
    return Promise.reject(error);
  }
}

type RpcReply = { id: number; ok: true; value: unknown } | { id: number; ok: false; error: BridgeError };
type WorkerEvent = { event: "changed" } | { event: "ready"; version: 1 } | { event: "stopped"; reason?: string } | { event: "banner"; candidate: { id: string; title: string; body: string; icon?: string } };
type PendingRequest = { resolve(value: unknown): void; reject(error: Error): void; timer: number };
type EnrollmentStage = "server" | "setup" | "signin" | "locked" | "ready" | "waiting_phone" | "verify_phone" | "phone_approved" | "phone_connected" | "expired" | "error" | "invite_request" | "invite_waiting" | "invite_approved" | "invite_denied" | "invite_expired";
export type EnrollmentView = { mode: "community" | "hosted"; stage: EnrollmentStage; origin?: string; owner?: boolean; hasGateway?: boolean; qrPayload?: string; verificationCode?: string; expiresInSeconds?: number; errorCode?: string; setupCodeRequired?: boolean; vaultOwnerRequired?: boolean; displayName?: string; username?: string };
export type InstanceManagement = { vault_id: string; devices: Array<{ device_id: string; name: string; kind: "browser" | "desktop" | "android" | "ios"; role: "owner" | "device" | "gateway"; revoked: boolean; gateway_online: boolean; presence?: { state: "connected" | "seen" | "never"; last_seen_age_seconds: number | null }; wake_route?: boolean }>; storage_bytes: number; quota_bytes: number; replay_retention_days: number; isAdmin: boolean; username: string };
export type VaultSummary = { vaultId: string; username: string; isAdmin: boolean; deviceCount: number; storageBytes: number; createdAt: string };
export type VaultInviteView = { inviteId: string; state: "active" | "used" | "revoked" | "expired"; expiresInSeconds: number; createdAt: string };
export type VaultRequestView = { requestId: string; displayName: string; client: "browser" | "desktop" | "android" | "ios"; state: "pending" | "approved" | "denied" | "completed" | "expired"; createdAt: string; expiresInSeconds: number };
type EnrollmentSecretField = { name: string; label: string; autocomplete?: string; value?: string };
type OwnerClaimDraft = { passphrase: string; setupToken?: string; adminPassword?: string };
type VaultClaimDraft = { username: string; adminPassword: string; passphrase: string };

const REQUEST_TIMEOUT_MS = 30_000;
const MAX_FILE_BYTES = 32 * 1024 * 1024;
const MAX_OPEN_MEDIA_URLS = 4;
const MAX_OPEN_MEDIA_BYTES = 64 * 1024 * 1024;
const MIN_ADMIN_PASSWORD_BYTES = 12;
const MAX_ADMIN_PASSWORD_BYTES = 1024;
const IMAGE_TYPES = new Set(["image/jpeg", "image/png", "image/webp", "image/gif"]);
const CONVERSATION_FOLDERS: readonly ConversationFolder[] = ["inbox", "archived", "spam"];
const NOTIFICATION_PERSISTENCE_KEY = "peppy.notification-persistence";
let activeSecretDialog: (() => void) | undefined;
let capturedInviteLink = captureInviteLink();

function captureInviteLink(): string | null {
  if (typeof window === "undefined" || !window.location.hash.startsWith("#invite=")) return null;
  const token = window.location.hash.slice("#invite=".length);
  const link = `${window.location.origin}/#invite=${token}`;
  window.history.replaceState(window.history.state, "", `${window.location.pathname}${window.location.search}`);
  return link;
}

export function parseVaultRequestSnapshot(data: string): VaultRequestView[] {
  const parsed: unknown = JSON.parse(data);
  if (!isRecord(parsed) || parsed.type !== "snapshot" || !Array.isArray(parsed.requests)) throw new Error("Invalid vault request snapshot.");
  return parsed.requests.map(request => {
    if (!isRecord(request) || typeof request.request_id !== "string" || typeof request.display_name !== "string" || !isClient(request.client) || !isRequestState(request.state) || typeof request.created_at !== "string" || !isSafeInteger(request.expires_in_seconds)) throw new Error("Invalid vault request snapshot.");
    return { requestId: request.request_id, displayName: request.display_name, client: request.client, state: request.state, createdAt: request.created_at, expiresInSeconds: request.expires_in_seconds };
  });
}

function isRecord(value: unknown): value is Record<string, unknown> { return typeof value === "object" && value !== null && !Array.isArray(value); }
function isSafeInteger(value: unknown): value is number { return typeof value === "number" && Number.isSafeInteger(value) && value >= 0; }
function isClient(value: unknown): value is VaultRequestView["client"] { return value === "browser" || value === "desktop" || value === "android" || value === "ios"; }
function isRequestState(value: unknown): value is VaultRequestView["state"] { return value === "pending" || value === "approved" || value === "denied" || value === "completed" || value === "expired"; }

class BrowserBridgeError extends Error implements BridgeError {
  public constructor(
    public readonly code: string,
    message: string,
    public readonly currentRevision?: string,
    public readonly conversationId?: string,
    public readonly draftId?: string,
  ) { super(message); }
}

/** The browser has no hosted billing data source, so it reports a website-only, unsubscribed default. */
const BROWSER_BILLING_SUMMARY: BillingSummaryView = { provider: "none", manageableIn: "website", planActive: false, storagePacks: 0, storageRenewsWithoutPlan: false, deletionPending: false };

function unsupported(method: string): Promise<never> {
  return Promise.reject(new BrowserBridgeError("unsupported", `${method} is unavailable in the browser.`));
}

function unreadMessageBatches(value: unknown): string[][] {
  if (!Array.isArray(value) || value.some(batch => !Array.isArray(batch) || batch.length === 0 || batch.some(id => typeof id !== "string"))) {
    throw new BrowserBridgeError("unavailable", "The browser worker returned invalid unread message batches.");
  }
  return value as string[][];
}

function canonicalChangedCount(value: unknown): string {
  if (typeof value !== "string" || !/^(0|[1-9][0-9]*)$/.test(value)) {
    throw new BrowserBridgeError("unavailable", "The browser worker returned an invalid changed count.");
  }
  return value;
}

function secretDialog(options: { id: string; title: string; description: string; confirm: string; fields: EnrollmentSecretField[]; checkbox?: { name: string; label: string; description?: string }; setupMode?: "open" | "protected"; errorMessage?: string }): Promise<Record<string, string> | null> {
  activeSecretDialog?.();
  return new Promise((resolve) => {
    const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : undefined;
    const dialog = document.createElement("dialog");
    dialog.id = options.id;
    dialog.setAttribute("data-browser-screen", options.id);
    dialog.setAttribute("aria-labelledby", `${options.id}-heading`);
    dialog.setAttribute("aria-describedby", `${options.id}-description`);
    dialog.setAttribute("aria-modal", "true");
    const form = document.createElement("form");
    form.id = `${options.id}-form`;
    if (options.setupMode) form.dataset.setupMode = options.setupMode;
    form.method = "dialog";
    const heading = document.createElement("h2");
    heading.id = `${options.id}-heading`;
    heading.textContent = options.title;
    form.append(heading);
    const description = document.createElement("p");
    description.id = `${options.id}-description`;
    description.textContent = options.description;
    form.append(description);
    const inputs = options.fields.map((field) => {
      const label = document.createElement("label");
      const input = document.createElement("input");
      input.id = `${options.id}-${field.name}`;
      input.name = field.name;
      input.type = "password";
      input.required = true;
      input.setAttribute("autocomplete", field.autocomplete ?? "off");
      input.value = field.value ?? "";
      label.htmlFor = input.id;
      label.textContent = field.label;
      label.append(input);
      form.append(label);
      return input;
    });
    let checkbox: HTMLInputElement | undefined;
    if (options.checkbox) {
      const group = document.createElement("div");
      group.id = `${options.id}-${options.checkbox.name}-group`;
      const label = document.createElement("label");
      label.className = "browser-dialog-checkbox";
      checkbox = document.createElement("input");
      checkbox.id = `${options.id}-${options.checkbox.name}`;
      checkbox.name = options.checkbox.name;
      checkbox.type = "checkbox";
      label.htmlFor = checkbox.id;
      label.append(checkbox, options.checkbox.label);
      group.append(label);
      if (options.checkbox.description) {
        const description = document.createElement("p");
        description.id = `${checkbox.id}-description`;
        description.textContent = options.checkbox.description;
        checkbox.setAttribute("aria-describedby", description.id);
        group.append(description);
      }
      form.append(group);
    }
    if (options.errorMessage) {
      const error = document.createElement("p");
      error.id = `${options.id}-error`;
      error.setAttribute("role", "alert");
      error.style.color = "var(--peppy-error-text, red)";
      error.style.fontSize = "inherit";
      error.textContent = options.errorMessage;
      form.append(error);
      dialog.setAttribute("aria-describedby", `${options.id}-description ${error.id}`);
    }
    const cancel = document.createElement("button");
    cancel.id = `${options.id}-cancel`;
    cancel.type = "button";
    cancel.textContent = "Cancel";
    const submit = document.createElement("button");
    submit.id = `${options.id}-submit`;
    submit.type = "submit";
    submit.className = "primary-button";
    submit.textContent = options.confirm;
    form.append(cancel, submit);
    const finish = (result: Record<string, string> | null) => {
      inputs.forEach((input) => { input.value = ""; });
      if (checkbox) checkbox.checked = false;
      if (activeSecretDialog === close) activeSecretDialog = undefined;
      dialog.remove();
      previousFocus?.focus();
      resolve(result);
    };
    const close = () => finish(null);
    activeSecretDialog = close;
    cancel.onclick = close;
    form.onsubmit = (event) => {
      event.preventDefault();
      finish(Object.fromEntries([
        ...inputs.map((input) => [input.name, input.value]),
        ...(checkbox ? [[checkbox.name, checkbox.checked ? "true" : "false"]] : []),
      ]));
    };
    dialog.oncancel = (event) => { event.preventDefault(); close(); };
    dialog.append(form);
    document.body.append(dialog);
    dialog.showModal();
    inputs[0]?.focus();
  });
}

function generatedSecretDialog(passphrase: string): Promise<boolean> {
  const id = "browser-enrollment-owner-secret";
  activeSecretDialog?.();
  return new Promise((resolve) => {
    const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : undefined;
    const dialog = document.createElement("dialog");
    dialog.id = id;
    dialog.setAttribute("data-browser-screen", id);
    dialog.setAttribute("aria-labelledby", `${id}-heading`);
    dialog.setAttribute("aria-describedby", `${id}-description`);
    dialog.setAttribute("aria-modal", "true");
    const form = document.createElement("form");
    form.id = `${id}-form`;
    form.method = "dialog";
    const heading = document.createElement("h2");
    heading.id = `${id}-heading`;
    heading.textContent = "Save your Vault key phrase";
    const description = document.createElement("p");
    description.id = `${id}-description`;
    description.textContent = "Save your Vault key phrase before continuing. Generated phrases usually have six words. This is not your account or admin password. You need it to recover and connect other devices.";
    const label = document.createElement("label");
    label.htmlFor = `${id}-passphrase`;
    label.textContent = "Vault key phrase";
    const secret = document.createElement("input");
    secret.id = `${id}-passphrase`;
    secret.name = "passphrase";
    secret.type = "text";
    secret.readOnly = true;
    secret.value = passphrase;
    secret.setAttribute("autocomplete", "off");
    label.append(secret);
    const copy = document.createElement("button");
    copy.id = `${id}-copy`;
    copy.type = "button";
    copy.textContent = "Copy Vault key phrase";
    const copyStatus = document.createElement("span");
    copyStatus.id = `${id}-copy-status`;
    copyStatus.className = "visually-hidden";
    copyStatus.setAttribute("role", "status");
    copyStatus.setAttribute("aria-live", "polite");
    const acknowledgement = document.createElement("label");
    const checkbox = document.createElement("input");
    checkbox.id = `${id}-acknowledged`;
    checkbox.type = "checkbox";
    acknowledgement.htmlFor = checkbox.id;
    acknowledgement.append(checkbox, " I saved my Vault key phrase");
    const cancel = document.createElement("button");
    cancel.id = `${id}-cancel`;
    cancel.type = "button";
    cancel.textContent = "Cancel";
    const submit = document.createElement("button");
    submit.id = `${id}-submit`;
    submit.type = "submit";
    submit.className = "primary-button";
    submit.disabled = true;
    submit.textContent = "Continue";
    form.append(heading, description, label, copy, copyStatus, acknowledgement, cancel, submit);
    let open = true;
    const finish = (acknowledged: boolean) => {
      open = false;
      secret.value = "";
      checkbox.checked = false;
      if (activeSecretDialog === close) activeSecretDialog = undefined;
      dialog.remove();
      previousFocus?.focus();
      resolve(acknowledged);
    };
    const close = () => finish(false);
    activeSecretDialog = close;
    copy.onclick = async () => {
      try {
        if (!navigator.clipboard?.writeText) throw new Error("Clipboard is unavailable.");
        await navigator.clipboard.writeText(passphrase);
        if (!open) return;
        copy.textContent = "Copied";
        copyStatus.textContent = "Copied";
      } catch {
        if (!open) return;
        copy.textContent = "Could not copy. Try again.";
        copyStatus.textContent = "Could not copy. Try again.";
      }
    };
    checkbox.onchange = () => { submit.disabled = !checkbox.checked; };
    cancel.onclick = close;
    form.onsubmit = (event) => {
      event.preventDefault();
      if (checkbox.checked) finish(true);
    };
    dialog.oncancel = (event) => { event.preventDefault(); close(); };
    dialog.append(form);
    document.body.append(dialog);
    dialog.showModal();
    secret.focus();
    secret.select();
  });
}

async function writeClipboardText(text: string): Promise<void> {
  try {
    if (!navigator.clipboard?.writeText) throw new Error("Clipboard is unavailable.");
    await navigator.clipboard.writeText(text);
  } catch {
    throw new BrowserBridgeError("clipboard-unavailable", "The code could not be copied.");
  }
}

function chooseFile(accept: string, multiple = false): Promise<File[]> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = accept;
    input.multiple = multiple;
    const finish = (files: File[]) => { input.remove(); resolve(files); };
    input.onchange = () => finish(Array.from(input.files ?? []));
    input.addEventListener("cancel", () => finish([]), { once: true });
    input.click();
  });
}

async function readContactPhoto(): Promise<{ dataUrl: string; naturalWidth: number; naturalHeight: number } | null> {
  const [file] = await chooseFile([...IMAGE_TYPES].join(","));
  if (!file) return null;
  if (file.size > MAX_FILE_BYTES || !IMAGE_TYPES.has(file.type)) throw new BrowserBridgeError("invalid-photo", "Choose an image smaller than 32 MiB.");
  const dataUrl = await new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new BrowserBridgeError("invalid-photo", "The image could not be read."));
    reader.onload = () => typeof reader.result === "string" ? resolve(reader.result) : reject(new BrowserBridgeError("invalid-photo", "The image could not be read."));
    reader.readAsDataURL(file);
  });
  const dimensions = await new Promise<{ naturalWidth: number; naturalHeight: number }>((resolve, reject) => {
    const image = new Image();
    image.onerror = () => reject(new BrowserBridgeError("invalid-photo", "The image could not be decoded."));
    image.onload = () => resolve({ naturalWidth: image.naturalWidth, naturalHeight: image.naturalHeight });
    image.src = dataUrl;
  });
  return { dataUrl, ...dimensions };
}

export class BrowserBridge implements DesktopBridge {
  private nextId = 1;
  private stopped = false;
  private readonly pending = new Map<number, PendingRequest>();
  private readonly listeners = new Set<() => void>();
  private readonly readyListeners = new Set<() => void>();
  private readonly stoppedListeners = new Set<(reason: string) => void>();
  private readonly blobUrls = new Set<string>();
  private readonly mediaBlobUrls = new Map<string, { attachmentId: string; byteSize: number }>();
  private readonly banners = new Set<Notification>();
  private notificationContext?: { view: "conversations" | "notifications" | "settings" | "contacts"; conversationId?: string };
  private accountUrl?: string;
  private enrollmentMode: "community" | "hosted" = "community";
  private setupCodeRequired = true;
  private ownerClaimDraft?: OwnerClaimDraft;
  private vaultClaimDraft?: VaultClaimDraft;

  public constructor(private readonly port: MessagePort, private readonly timeoutMs = REQUEST_TIMEOUT_MS) {
    port.onmessage = (event: MessageEvent<RpcReply | WorkerEvent>) => this.receive(event.data);
    port.onmessageerror = () => this.stop("disconnected");
    port.start();
    window.addEventListener("focus", this.refreshNotificationContext);
    window.addEventListener("blur", this.refreshNotificationContext);
    window.addEventListener("storage", this.refreshNotificationContextOnStorage);
    document.addEventListener("visibilitychange", this.refreshNotificationContext);
    document.addEventListener("visibilitychange", this.flushDraftSyncWhenHidden);
    window.addEventListener("pagehide", this.flushDraftSync);
  }

  public onStopped(listener: (reason: string) => void): () => void {
    this.stoppedListeners.add(listener);
    return () => this.stoppedListeners.delete(listener);
  }

  public onReady(listener: () => void): () => void {
    this.readyListeners.add(listener);
    return () => this.readyListeners.delete(listener);
  }

  public workerError(): void { this.stop("unavailable"); }

  public setAccountUrl(value: unknown): void {
    this.accountUrl = validateAccountUrl(value);
  }

  private receive(message: RpcReply | WorkerEvent): void {
    if ("event" in message) {
      if (message.event === "changed") this.listeners.forEach((listener) => listener());
      else if (message.event === "ready") {
        if (message.version !== 1) this.stop("unavailable");
        else this.readyListeners.forEach((listener) => listener());
      }
      else if (message.event === "stopped") this.stop(message.reason ?? "stopped");
      else if (message.event === "banner") this.showBanner(message.candidate);
      return;
    }
    if (message.id === -1) return this.stop(message.ok ? "unavailable" : message.error.code);
    const request = this.pending.get(message.id);
    if (!request) return;
    this.pending.delete(message.id);
    window.clearTimeout(request.timer);
    if (message.ok) request.resolve(message.value);
    else request.reject(new BrowserBridgeError(message.error.code, message.error.message, message.error.currentRevision, message.error.conversationId, message.error.draftId));
  }

  private stop(reason: string): void {
    if (this.stopped) return;
    this.stopped = true;
    activeSecretDialog?.();
    this.ownerClaimDraft = undefined;
    this.vaultClaimDraft = undefined;
    this.failPending(reason, "Peppy locked or stopped. Reconnect to continue.");
    this.revokeBlobUrls();
    this.banners.forEach(notification => notification.close());
    this.banners.clear();
    window.removeEventListener("focus", this.refreshNotificationContext);
    window.removeEventListener("blur", this.refreshNotificationContext);
    window.removeEventListener("storage", this.refreshNotificationContextOnStorage);
    document.removeEventListener("visibilitychange", this.refreshNotificationContext);
    document.removeEventListener("visibilitychange", this.flushDraftSyncWhenHidden);
    window.removeEventListener("pagehide", this.flushDraftSync);
    this.stoppedListeners.forEach((listener) => listener(reason));
    this.port.close();
  }

  private failPending(code: string, message: string): void {
    this.pending.forEach((request) => { window.clearTimeout(request.timer); request.reject(new BrowserBridgeError(code, message)); });
    this.pending.clear();
  }

  private call<T>(command: string, args: Record<string, unknown> = {}, transfer: Transferable[] = []): Promise<T> {
    if (this.stopped) return Promise.reject(new BrowserBridgeError("stopped", "Peppy locked or stopped. Reconnect to continue."));
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      const timer = window.setTimeout(() => { this.pending.delete(id); reject(new BrowserBridgeError("timeout", "The browser worker did not respond. Reload Peppy and try again.")); }, this.timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      try { this.port.postMessage({ id, command, args }, transfer); }
      catch { window.clearTimeout(timer); this.pending.delete(id); reject(new BrowserBridgeError("disconnected", "The browser worker is unavailable.")); }
    });
  }

  private revokeBlobUrls(): void {
    this.blobUrls.forEach((url) => URL.revokeObjectURL(url));
    this.blobUrls.clear();
    this.mediaBlobUrls.forEach((_, url) => URL.revokeObjectURL(url));
    this.mediaBlobUrls.clear();
  }
  private revokeMediaUrl(url: string): void {
    if (!this.mediaBlobUrls.delete(url)) return;
    URL.revokeObjectURL(url);
  }
  private async releaseMediaUrl(url: string): Promise<void> {
    const media = this.mediaBlobUrls.get(url);
    if (!media) return;
    this.revokeMediaUrl(url);
    await this.call("close_media", { id: media.attachmentId, url });
  }
  private readonly refreshNotificationContext = (): void => {
    if (!this.notificationContext || this.stopped) return;
    void this.call("set_notification_context", this.notificationContextArgs()).catch(() => undefined);
  };
  private readonly flushDraftSyncWhenHidden = (): void => {
    if (document.visibilityState === "hidden") this.flushDraftSync();
  };
  private readonly flushDraftSync = (): void => {
    if (this.stopped) return;
    void this.call("flush_draft_sync").catch(() => undefined);
  };
  private readonly refreshNotificationContextOnStorage = (event: StorageEvent): void => {
    if (event.key === null || event.key === NOTIFICATION_PERSISTENCE_KEY) this.refreshNotificationContext();
  };
  private notificationContextArgs(): Record<string, unknown> {
    const permission = typeof Notification === "undefined" ? "unsupported" : Notification.permission;
    return { ...this.notificationContext, notificationPermission: permission, focused: document.visibilityState === "visible" && document.hasFocus(), requireInteraction: this.requiresNotificationInteraction() };
  }
  private notificationPersistence(): NotificationPersistence {
    try {
      const stored = localStorage.getItem(NOTIFICATION_PERSISTENCE_KEY);
      return stored === null ? DEFAULT_NOTIFICATION_PERSISTENCE : notificationPersistence(JSON.parse(stored)) ?? DEFAULT_NOTIFICATION_PERSISTENCE;
    } catch {
      return DEFAULT_NOTIFICATION_PERSISTENCE;
    }
  }
  private notificationPersistenceSupported(): boolean {
    return typeof Notification !== "undefined" && "requireInteraction" in Notification.prototype;
  }
  private requiresNotificationInteraction(): boolean {
    return this.notificationPersistenceSupported() && this.notificationPersistence().seconds === null;
  }
  private showBanner(candidate: { id: string; title: string; body: string; icon?: string }): void {
    if (typeof Notification === "undefined" || Notification.permission !== "granted") return;
    try {
      const notification = new Notification(candidate.title, { body: candidate.body, tag: `peppy-${candidate.id}`, ...(candidate.icon ? { icon: candidate.icon } : {}), ...(this.requiresNotificationInteraction() ? { requireInteraction: true } : {}) });
      this.banners.add(notification);
      notification.onclose = () => this.banners.delete(notification);
      void this.call("display_ack", { ids: [candidate.id] }).catch(() => undefined);
    } catch {
      void this.call("display_failed", { ids: [candidate.id] }).catch(() => undefined);
    }
  }
  public dispose(): void {
    activeSecretDialog?.();
    this.ownerClaimDraft = undefined;
    this.vaultClaimDraft = undefined;
    if (!this.stopped) {
      try { this.port.postMessage({ id: this.nextId++, command: "disconnect", args: {} }); } catch {}
    }
    this.stop("disconnected");
  }
  public load_state(conversationId?: string, historyFromMessageId?: string, options?: { metadataOnly?: boolean }): Promise<DesktopSnapshot> {
    return this.call<DesktopSnapshot>("load_state", {
      ...(conversationId ? { conversationId } : {}),
      ...(historyFromMessageId ? { historyFromMessageId } : {}),
      ...(options?.metadataOnly === undefined ? {} : { metadataOnly: options.metadataOnly }),
    }).then(normalizeGroupSnapshot);
  }
  public search_local(input: LocalSearchRequest): Promise<LocalSearchPage> { return this.call("search_local", input); }
  public advance_search_index(input?: { budgetRows?: number }): Promise<SearchIndexProgress> { return this.call("advance_search_index", input ?? {}); }
  /** Query-free: each command forwards only the operation or attempt identity, never extra renderer fields. */
  public get_history_search(): Promise<HistorySearchSnapshot> { return this.call("get_history_search"); }
  public start_history_search(input: StartHistorySearchInput): Promise<HistorySearchSnapshot> { return this.call("start_history_search", { operationId: input.operationId }); }
  public cancel_history_search(input: HistorySearchAttemptInput): Promise<HistorySearchSnapshot> { return this.call("cancel_history_search", { attemptId: input.attemptId }); }
  public load_message_window(input: MessageWindowRequest): Promise<MessageWindow> { return this.call<MessageWindow>("load_message_window", input).then(normalizeMessageWindow); }
  public load_history(conversationId: string, beforeMessageId: string): Promise<HistoryPage> { return this.call("load_history", { conversationId, beforeMessageId }); }
  public save_draft(input: DraftInput): Promise<Draft> { return callChecked(() => this.call<Draft>("save_draft", { input: { ...input, ...groupDraftFields(input) } })); }
  public send_draft(input: SendDraftInput): Promise<SendResultView> { return callChecked(() => this.call<unknown>("send_draft", { input: { ...input, ...groupDraftFields(input) } }).then(parseSendResult)); }
  public discard_draft(input: { id: string; conversationId: string; expectedRevision: string }): Promise<void> { return this.call("discard_draft", { input }); }
  public mark_seen(visibleMessageIds: string[]): Promise<void> { return this.call("mark_seen", { visibleMessageIds }); }
  /** Copies first; the read mark runs only after the clipboard accepts the code. */
  public async copy_one_time_code(target: OneTimeCodeTarget, code: string): Promise<void> {
    await writeClipboardText(code);
    try {
      if (target.kind === "message") await this.mark_seen([target.messageId]);
      else await this.mark_notifications_seen([target.target]);
    } catch {
      // The code is already on the clipboard; a failed read mark must not report the copy as failed.
    }
  }
  public async mark_all_read(): Promise<string> {
    const batches = unreadMessageBatches(await this.call("unread_message_batches"));
    let changed = 0n;
    for (const messageIds of batches) {
      changed += BigInt(canonicalChangedCount(await this.call("mark_read_batch", { messageIds })));
    }
    return changed.toString();
  }
  public set_conversation_pinned(conversationId: string, pinned: boolean): Promise<void> { return this.call("set_conversation_pinned", { conversationId, pinned }); }
  public set_conversation_folder(conversationId: string, folder: ConversationFolder): Promise<void> {
    if (!CONVERSATION_FOLDERS.includes(folder)) return Promise.reject(new BrowserBridgeError("invalid-request", "Invalid conversation folder."));
    return this.call("set_conversation_folder", { conversationId, folder });
  }
  public request_sender_unblock(conversationId: string): Promise<void> { return this.call("request_sender_unblock", { conversationId }); }
  public set_conversation_tone(seed: string, paletteIndex: number | null): Promise<void> { return this.call("set_conversation_tone", { seed, paletteIndex }); }
  public dismiss_notification(target: NotificationTarget): Promise<void> { return this.call("dismiss_notification", { target }); }
  public dismiss_all_notifications(): Promise<void> { return this.call("dismiss_all_notifications"); }
  public set_app_muted(sourceDeviceId: string, packageName: string, appName: string, muted: boolean): Promise<void> { return this.call("set_app_muted", { sourceDeviceId, packageName, appName, muted }); }
  public mark_notifications_seen(targets: NotificationTarget[]): Promise<void> { return this.call("mark_notifications_seen", { targets }); }
  public set_notification_preferences(preferences: NotificationPreferences): Promise<void> { return this.call("set_notification_preferences", { preferences }); }
  public async get_notification_persistence(): Promise<NotificationPersistenceSettings> { return { ...this.notificationPersistence(), supported: this.notificationPersistenceSupported(), platform: "browser" }; }
  public async set_notification_persistence(value: NotificationPersistence): Promise<void> {
    const persistence = notificationPersistence(value);
    if (!persistence) throw new BrowserBridgeError("invalid-request", "Invalid notification persistence.");
    try {
      localStorage.setItem(NOTIFICATION_PERSISTENCE_KEY, JSON.stringify(persistence));
    } catch {
      throw new BrowserBridgeError("notification-persistence", "Notification persistence could not be saved.");
    }
    this.refreshNotificationContext();
  }
  public set_notification_context(view: "conversations" | "notifications" | "settings" | "contacts", conversationId?: string): Promise<void> {
    this.notificationContext = { view, ...(conversationId ? { conversationId } : {}) };
    return this.call("set_notification_context", this.notificationContextArgs());
  }
  public list_contact_books(): Promise<ContactBookView[]> { return this.call("list_contact_books"); }
  public forget_contact_book(bookId: string): Promise<void> { return this.call("forget_contact_book", { bookId }); }
  public list_contacts(bookId: string, query?: string, offset?: number, sort?: import("@peppy/desktop-ui/bridge-contracts").ContactSort): Promise<ContactView[]> { return this.call("list_contacts", { bookId, query, offset, sort }); }
  public submit_contact_edit(input: ContactEditRequestInput): Promise<ContactEditOutcome> { return this.call("submit_contact_edit", { input }); }
  public list_contact_edits(bookId?: string): Promise<ContactEditStatus[]> { return this.call("list_contact_edits", { bookId }); }
  public search_contact_recipients(query: string, sourceDeviceId?: string): Promise<RecipientSuggestion[]> { return this.call("search_contact_recipients", { query, sourceDeviceId }); }
  public request_contact_repair(): Promise<ContactSyncStatus> { return this.call("request_contact_repair"); }
  public list_devices(): Promise<DeviceRosterView> { return this.call("list_devices"); }
  public revoke_device(deviceId: string): Promise<void> { return this.call("revoke_device", { deviceId }); }
  public mcp_status(): Promise<import("@peppy/desktop-ui/bridge-contracts").McpDeviceStatus> { return this.call("mcp_status"); }
  public mcp_enable(): Promise<import("@peppy/desktop-ui/bridge-contracts").McpDeviceStatus> { return this.call("mcp_enable"); }
  public mcp_grants(): Promise<import("@peppy/desktop-ui/bridge-contracts").McpGrantView[]> { return this.call("mcp_grants"); }
  public mcp_revoke_grant(id: string): Promise<void> { return this.call("mcp_revoke_grant", { id }); }
  public webhooks_list(): Promise<import("@peppy/desktop-ui/bridge-contracts").WebhookList> { return this.call("webhooks_list"); }
  public webhook_create(input: import("@peppy/desktop-ui/bridge-contracts").WebhookInput): Promise<import("@peppy/desktop-ui/bridge-contracts").WebhookCreated> { return this.call("webhook_create", { input }); }
  public webhook_update(id: string, input: import("@peppy/desktop-ui/bridge-contracts").WebhookInput): Promise<import("@peppy/desktop-ui/bridge-contracts").WebhookView> { return this.call("webhook_update", { id, input }); }
  public webhook_delete(id: string): Promise<void> { return this.call("webhook_delete", { id }); }
  public webhook_reveal_secret(id: string): Promise<import("@peppy/desktop-ui/bridge-contracts").WebhookSecret> { return this.call("webhook_reveal_secret", { id }); }
  public webhook_rotate_secret(id: string): Promise<import("@peppy/desktop-ui/bridge-contracts").WebhookSecret> { return this.call("webhook_rotate_secret", { id }); }
  public webhook_test(id: string): Promise<import("@peppy/desktop-ui/bridge-contracts").WebhookTestResult> { return this.call("webhook_test", { id }); }
  public mcp_authorization(id: string): Promise<import("@peppy/desktop-ui/bridge-contracts").McpAuthorizationView> { return this.call("mcp_authorization", { id }); }
  public mcp_approve_authorization(id: string, scopes: import("@peppy/desktop-ui/bridge-contracts").McpScope[]): Promise<import("@peppy/desktop-ui/bridge-contracts").McpAuthorizationDecision> { return this.call("mcp_approve_authorization", { id, scopes }); }
  public mcp_deny_authorization(id: string): Promise<import("@peppy/desktop-ui/bridge-contracts").McpAuthorizationDecision> { return this.call("mcp_deny_authorization", { id }); }
  public request_contact_sync(bookId: string): Promise<ContactScanStatus> { return this.call("request_contact_sync", { bookId }); }
  public get_contact_sync_request(bookId: string): Promise<ContactScanStatus | null> { return this.call("get_contact_sync_request", { bookId }); }
  public list_restorable_contacts(bookId: string): Promise<RestorableContact[]> { return this.call("list_restorable_contacts", { bookId }); }
  public restore_contact(bookId: string, contactId: string): Promise<ContactEditOutcome> { return this.call("restore_contact", { bookId, contactId }); }
  public subscribe(listener: () => void): () => void { this.listeners.add(listener); return () => this.listeners.delete(listener); }
  public set_maximize_button_region(_region: MaximizeButtonRegion | null): Promise<void> { return Promise.resolve(); }
  public subscribe_maximize_button_hover(_listener: (hovered: boolean) => void): () => void { return () => undefined; }
  public subscribe_lifecycle(): () => void { return () => undefined; }
  public subscribe_lifecycle_finished(): () => void { return () => undefined; }
  public lock_sync(): Promise<void> { return this.call("lock_sync"); }

  public async unlock_sync(): Promise<void> { const secret = await secretDialog({ id: "browser-unlock", title: "Unlock Peppy", description: "Enter your Vault key phrase to access your messages. This is not your account or admin password.", confirm: "Unlock", fields: [{ name: "passphrase", label: "Vault key phrase", autocomplete: "current-password" }], checkbox: { name: "remember", label: "Remember on this browser", description: "Anyone with access to this browser profile can unlock your vault without your Vault key phrase." } }); if (secret) await this.call("unlock", { passphrase: secret.passphrase, remember: secret.remember === "true" }); }
  public configure_server(origin: string): Promise<void> { return origin === location.origin ? Promise.resolve() : Promise.reject(new BrowserBridgeError("invalid-origin", "The browser uses its own secure origin.")); }
  public hosted_origin_url(): Promise<string> { return Promise.resolve(location.origin); }
  public get_app_lock_status(): Promise<AppLockStatus> { return unsupported("App lock"); }
  public retry_app_lock_check(): Promise<AppLockStatus> { return unsupported("App lock"); }
  public set_app_lock(_enabled: boolean): Promise<boolean> { return unsupported("App lock"); }
  public unlock_app(): Promise<boolean> { return unsupported("App lock"); }
  public async request_notification_permission(): Promise<NotificationCheck> {
    if (!("Notification" in window)) return { status: "unknown", testSent: false, canOpenSettings: false };
    const status = await Notification.requestPermission();
    if (status !== "granted") return { status: status === "default" ? "unknown" : status, testSent: false, canOpenSettings: false };
    try {
      new Notification("New notification", { body: "" });
      return { status, testSent: true, canOpenSettings: false };
    } catch {
      return { status, testSent: false, canOpenSettings: false };
    }
  }
  public open_notification_settings(): Promise<void> { return unsupported("Notification settings"); }
  public async pick_attachments(): Promise<AttachmentView[]> {
    const files = await chooseFile("*/*", true);
    if (files.some((file) => file.size > MAX_FILE_BYTES)) throw new BrowserBridgeError("attachment-too-large", "Attachments must be 32 MiB or smaller.");
    return Promise.all(files.map(async (file) => { const bytes = await file.arrayBuffer(); return this.call<AttachmentView>("prepare_attachment", { bytes, displayName: file.name, mediaType: file.type || "application/octet-stream" }, [bytes]); }));
  }
  public async prepare_pasted_attachments(files: PastedImage[]) {
    const prepared = await Promise.allSettled(files.map(({ bytes, name, mediaType }) => {
      if (!IMAGE_TYPES.has(mediaType)) return Promise.reject(new BrowserBridgeError("invalid-attachment", "Only PNG, JPEG, GIF, and WebP images can be pasted."));
      if (bytes.byteLength > MAX_FILE_BYTES) return Promise.reject(new BrowserBridgeError("attachment-too-large", "Images must be 32 MiB or smaller."));
      return this.call<AttachmentView>("prepare_attachment", { bytes, displayName: name, mediaType }, [bytes]);
    }));
    return {
      views: prepared.flatMap(result => result.status === "fulfilled" ? [result.value] : []),
      failures: prepared.flatMap(result => result.status === "rejected" ? [result.reason instanceof Error ? result.reason.message : "Could not add pasted image."] : []),
    };
  }
  public resolve_draft_conflict(input: { draftId: string; keepBranchId: string | null }): Promise<Draft> { return this.call("resolve_draft_conflict", input); }
  public acknowledge_draft_notice(draftId: string): Promise<void> { return this.call("acknowledge_draft_notice", { draftId }); }
  public retry_attachment(id: string): Promise<void> { return this.call("retry_attachment", { id }); }
  public retryRcsMediaDownload(messageId: string, attachmentIndex: number): Promise<void> {
    if (typeof messageId !== "string" || messageId.length === 0) return Promise.reject(new BrowserBridgeError("invalid-request", "Invalid message ID."));
    if (!Number.isInteger(attachmentIndex) || attachmentIndex < 0) return Promise.reject(new BrowserBridgeError("invalid-request", "Invalid attachment index."));
    return this.call("retry_rcs_media_download", { messageId, attachmentIndex });
  }
  public attachment_retention_state(attachmentId: string): Promise<AttachmentRetentionStateDto> { return this.call("attachment_retention_state", { attachmentId }); }
  public attachment_retention_status(): Promise<AttachmentRetentionStatusResult> { return this.call("attachment_retention_status"); }
  public account_access(): Promise<AccountAccessResult> { return this.call("account_access"); }
  public request_attachment_download(id: string): Promise<void> { return this.call("request_attachment_download", { id }); }
  public media_sync_request(attachmentIds: string[]): Promise<void> { return this.call("media_sync_request", { attachmentIds }); }
  public media_page(conversationId: string, cursor?: MediaCursor, limit?: number): Promise<MediaPage> { return this.call("media_page", { conversationId, cursor, limit }); }
  public media_page_around(conversationId: string, attachmentId: string, messageId?: string, limit?: number): ReturnType<DesktopBridge["media_page_around"]> { return this.call("media_page_around", { conversationId, attachmentId, messageId, limit }); }
  public media_page_newer(conversationId: string, cursor: MediaCursor, limit?: number): Promise<MediaPage> { return this.call("media_page_newer", { conversationId, cursor, limit }); }
  private async mediaOpen(command: "open_media" | "load_media", id: string): Promise<MediaOpen> {
    const result = await this.call<{ state: unknown; bytes?: unknown; mediaType?: unknown; byteSize?: unknown }>(command, { id });
    if (this.stopped) throw new BrowserBridgeError("stopped", "Peppy locked or stopped. Reconnect to continue.");
    if (result.state === "pending") return { state: "pending" };
    const bytes = result.bytes;
    const byteSize = result.byteSize;
    if (result.state !== "ready" || !(bytes instanceof ArrayBuffer) || typeof result.mediaType !== "string" || typeof byteSize !== "number" || !Number.isSafeInteger(byteSize) || byteSize <= 0 || byteSize !== bytes.byteLength || byteSize > MAX_FILE_BYTES) throw new BrowserBridgeError("unavailable", "The media response was invalid.");
    const mediaType = inlineMediaType(result.mediaType);
    if (!mediaType) {
      await this.call("close_media", { id }).catch(() => undefined);
      throw new BrowserBridgeError("unsupported", "Save this media to open it in another application.");
    }
    while (this.mediaBlobUrls.size >= MAX_OPEN_MEDIA_URLS || [...this.mediaBlobUrls.values()].reduce((total, media) => total + media.byteSize, 0) + byteSize > MAX_OPEN_MEDIA_BYTES) {
      const oldest = this.mediaBlobUrls.keys().next().value as string | undefined;
      if (!oldest) break;
      await this.releaseMediaUrl(oldest);
    }
    if (this.stopped) throw new BrowserBridgeError("stopped", "Peppy locked or stopped. Reconnect to continue.");
    const url = URL.createObjectURL(new Blob([bytes], { type: mediaType }));
    this.mediaBlobUrls.set(url, { attachmentId: id, byteSize });
    return { state: "ready", url, mediaType, byteSize };
  }
  public open_media(id: string): Promise<MediaOpen> { return this.mediaOpen("open_media", id); }
  public load_media(id: string): Promise<MediaOpen> { return this.mediaOpen("load_media", id); }
  public async close_media(id: string, url?: string): Promise<void> {
    if (url) {
      const media = this.mediaBlobUrls.get(url);
      if (!media || media.attachmentId !== id) return;
      await this.releaseMediaUrl(url);
      return;
    }
    [...this.mediaBlobUrls.entries()].filter(([, media]) => media.attachmentId === id).forEach(([ownedUrl]) => this.revokeMediaUrl(ownedUrl));
    await this.call("close_media", { id });
  }
  public async save_attachment(id: string): Promise<boolean> { const file = await this.call<{ bytes: ArrayBuffer; displayName: string; mediaType: string }>("save_attachment", { id }); const url = URL.createObjectURL(new Blob([file.bytes], { type: file.mediaType })); this.blobUrls.add(url); const link = document.createElement("a"); link.href = url; link.download = safeSuggestedFilename(file.displayName); link.click(); window.setTimeout(() => { URL.revokeObjectURL(url); this.blobUrls.delete(url); }, 0); return true; }
  public async publish_attachment(id: string): Promise<PublicCopy | null> { if (!window.confirm("Create a public plaintext copy of this attachment? Anyone with the link may access it.")) return null; return this.call("publish_attachment", { id, confirmed: true }); }
  public pick_contact_photo(): Promise<{ dataUrl: string; naturalWidth: number; naturalHeight: number } | null> { return readContactPhoto(); }
  public async enrollment_discover(): Promise<EnrollmentView> {
    const view = await this.call<EnrollmentView>("enrollment_discover");
    this.enrollmentMode = view.mode;
    this.setupCodeRequired = view.setupCodeRequired !== false;
    return this.finishOwnerClaim(view);
  }
  public async enrollment_status(): Promise<EnrollmentView> { return this.finishOwnerClaim(await this.call("enrollment_status")); }
  public enrollment_start_over(): Promise<EnrollmentView> { return this.enrollment_status(); }
  public enrollment_cancel(): Promise<void> { this.ownerClaimDraft = undefined; this.vaultClaimDraft = undefined; return this.call("enrollment_cancel"); }
  public enrollment_begin_vault_request(input: { inviteLink: string; displayName: string }): Promise<EnrollmentView> { return this.call("enrollment_begin_vault_request", input); }
  public enrollment_vault_request_status(): Promise<EnrollmentView> { return this.call("enrollment_vault_request_status"); }
  public async enrollment_create_vault(input: { username: string }): Promise<EnrollmentView> {
    let draft = this.vaultClaimDraft;
    if (!draft) {
      const credentials = await this.collectConfirmedAdminPassword({
        id: "browser-enrollment-vault",
        title: "Create your vault",
        description: "Set a password for this vault.",
      });
      if (!credentials) return this.enrollment_status();
      const generated = await this.call<{ passphrase: string }>("enrollment_generate_secret");
      if (!await generatedSecretDialog(generated.passphrase)) return this.enrollment_status();
      draft = { username: input.username, adminPassword: credentials.adminPassword, passphrase: generated.passphrase };
      this.vaultClaimDraft = draft;
    }
    if (!draft.adminPassword) {
      const credentials = await this.collectConfirmedAdminPassword({
        id: "browser-enrollment-vault-password",
        title: "Create your vault",
        description: "Enter a corrected password for this vault.",
      });
      if (!credentials) return this.enrollment_status();
      draft = { ...draft, adminPassword: credentials.adminPassword };
    }
    draft = { ...draft, username: input.username };
    this.vaultClaimDraft = draft;
    const view = await this.call<EnrollmentView>("enrollment_create_vault", draft);
    if (view.stage === "ready") this.vaultClaimDraft = undefined;
    else if (view.stage === "error" && ["invalid_request", "invalid-request", "invalid_admin_password"].includes(view.errorCode ?? "")) {
      this.vaultClaimDraft = { ...draft, adminPassword: "" };
    }
    return view;
  }
  public enrollment_pending_invite(): Promise<string | null> {
    const inviteLink = capturedInviteLink;
    capturedInviteLink = null;
    return Promise.resolve(inviteLink);
  }
  public enrollment_sign_out(): Promise<void> { return appSessionRenewal.duringAuthTransition(() => this.call("enrollment_sign_out")); }
  public enrollment_set_hosted_csrf(csrf: string): Promise<void> { return this.call("enrollment_set_hosted_csrf", { csrf }); }
  public enrollment_management(): Promise<InstanceManagement> { return this.call("enrollment_management"); }
  public enrollment_revoke_device(deviceId: string): Promise<InstanceManagement> { return this.call("enrollment_revoke_device", { deviceId }); }
  public enrollment_list_vaults(): Promise<VaultSummary[]> { return this.call("enrollment_list_vaults"); }
  public enrollment_create_invite(): Promise<{ inviteId: string; inviteLink: string; expiresInSeconds: number }> { return this.call("enrollment_create_invite"); }
  public enrollment_list_invites(): Promise<VaultInviteView[]> { return this.call("enrollment_list_invites"); }
  public enrollment_revoke_invite(inviteId: string): Promise<void> { return this.call("enrollment_revoke_invite", { inviteId }); }
  public enrollment_list_vault_requests(): Promise<VaultRequestView[]> { return this.call("enrollment_list_vault_requests"); }
  public enrollment_approve_vault_request(requestId: string): Promise<void> { return this.call("enrollment_approve_vault_request", { requestId }); }
  public enrollment_deny_vault_request(requestId: string): Promise<void> { return this.call("enrollment_deny_vault_request", { requestId }); }
  public async enrollment_add_phone(method: "transfer" | "shared_secret"): Promise<EnrollmentView> { return this.finishOwnerClaim(await this.call("enrollment_add_phone", { method })); }
  public enrollment_connect_computer(): Promise<ComputerClaimantView> { return this.call("enrollment_connect_computer"); }
  public enrollment_computer_status(): Promise<ComputerClaimantView> { return this.call("enrollment_computer_status"); }
  public enrollment_confirm_computer(): Promise<ComputerClaimantView> { return this.call("enrollment_confirm_computer"); }
  public enrollment_cancel_computer(): Promise<ComputerClaimantView> { return this.call("enrollment_cancel_computer"); }
  public async enrollment_rotate_phone(method: "transfer" | "shared_secret"): Promise<EnrollmentView> {
    const view = await this.enrollment_status();
    if (view.stage !== "waiting_phone" && view.stage !== "expired") return view;
    const cancelled = await this.call<EnrollmentView>("enrollment_cancel", { waitingOnly: true });
    if (cancelled.stage !== "ready") return this.finishOwnerClaim(cancelled);
    return this.finishOwnerClaim(await this.call("enrollment_add_phone", { method }));
  }
  public async enrollment_cancel_phone(): Promise<EnrollmentView> {
    const view = await this.enrollment_status();
    if (view.stage !== "waiting_phone" && view.stage !== "expired") return view;
    return this.finishOwnerClaim(await this.call("enrollment_cancel", { waitingOnly: true }));
  }
  public reveal_vault_secret(): Promise<void> { return Promise.reject(new BrowserBridgeError("unavailable", "Vault key phrase reveal is unavailable in the browser.")); }
  public async enrollment_create_owner(input?: { username?: string }): Promise<EnrollmentView> {
    let draft = this.ownerClaimDraft;
    if (draft && this.enrollmentMode === "community" && this.setupCodeRequired && !draft.setupToken) {
      const correction = await secretDialog({
        id: "browser-enrollment-owner-code",
        title: "Enter setup code",
        description: peppyCopy.enrollment_setup_protected,
        confirm: "Continue",
        fields: [{ name: "setupToken", label: "Setup code", autocomplete: "off" }],
        setupMode: "protected",
      });
      if (!correction) return this.enrollment_status();
      draft = { ...draft, setupToken: correction.setupToken };
    }
    draft ??= await this.createOwnerClaimDraft();
    if (!draft) return this.enrollment_status();
    this.ownerClaimDraft = draft;
    return this.finishOwnerClaim(await this.call("enrollment_create_owner", { ...draft, ...(input?.username ? { username: input.username } : {}) }));
  }
  public async enrollment_sign_in(input?: { username?: string }): Promise<EnrollmentView> {
    if (this.enrollmentMode === "hosted") {
      const config = await fetchHostedWebConfig();
      if (!config) throw new BrowserBridgeError("unavailable", "Peppy could not start hosted sign-in.");
      await appSessionRenewal.duringAuthTransition(() => beginHostedAuthorization(config), { persistsUntilUnload: true });
      return this.enrollment_status();
    }
    return this.enrollmentSecretAction("enrollment_sign_in", "browser-enrollment-signin", "Sign in to Peppy", "Enter your account password.", "Sign in", [{ name: "adminPassword", label: "Password", autocomplete: "current-password" }], input);
  }
  public async enrollment_connect_shared_secret(input?: { vaultOwner?: string }): Promise<EnrollmentView> {
    return this.enrollmentSecretAction("enrollment_connect_shared_secret", "browser-enrollment-secret", "Enter Vault key phrase", "Enter the Vault key phrase provided by an owner. This is not your account or admin password.", "Connect", [{ name: "passphrase", label: "Vault key phrase", autocomplete: "one-time-code" }], input);
  }
  public async enrollment_approve(): Promise<EnrollmentView> {
    return this.call<EnrollmentView>("enrollment_approve");
  }
  public open_composer(): Promise<void> { return unsupported("Additional composer windows"); }
  public set_start_at_login(): Promise<void> { return unsupported("Start at login"); }
  public startup_status(): Promise<StartupStatus> { return unsupported("Startup status"); }
  public set_open_in_background(): Promise<void> { return unsupported("Open in the background"); }
  public open_login_items(): Promise<void> { return unsupported("Login Items settings"); }
  public popout_conversation(): Promise<{ headCreated: boolean; warning?: string }> { return unsupported("Floating conversations"); }
  public hide_head(): Promise<void> { return unsupported("Floating conversations"); }
  public close_composer(): Promise<void> { return unsupported("Closing a composer window"); }
  public close_head_panel(): Promise<void> { return unsupported("Closing a floating conversation"); }
  public acknowledge_lifecycle(): Promise<void> { return unsupported("Window lifecycle controls"); }
  public window(): Promise<void> { return unsupported("Window controls"); }
  public hosted_account(): Promise<never> { return unsupported("Hosted accounts"); }
  public hosted_sign_in(_provider: HostedSignInProvider): Promise<never> { return unsupported("Hosted sign-in"); }
  public hosted_sign_out(): Promise<void> { return unsupported("Hosted sign-out"); }
  public hosted_delete_account(): Promise<"deleted" | "pending"> { return unsupported("Account deletion"); }
  public hosted_open_billing(): Promise<void> {
    if (!this.accountUrl) return unsupported("Hosted billing");
    window.open(this.accountUrl, "_blank", "noopener,noreferrer");
    return Promise.resolve();
  }
  public hosted_provision(): Promise<void> { return unsupported("Hosted provisioning"); }
  public billing_summary(): Promise<BillingSummaryView> { return Promise.resolve({ ...BROWSER_BILLING_SUMMARY }); }
  public purchase_options(): Promise<PurchaseOptions> { return Promise.resolve({ appStore: false, website: true }); }
  public store_products(): Promise<StoreProduct[]> { return unsupported("App Store purchases"); }
  public store_purchase(_productId: string): Promise<"purchased" | "pending" | "cancelled"> { return unsupported("App Store purchases"); }
  public store_restore(): Promise<void> { return unsupported("Restore purchases"); }
  public open_store_subscriptions(): Promise<void> { return unsupported("App Store subscriptions"); }

  private async enrollmentSecretAction(command: string, id: string, title: string, description: string, confirm: string, fields: EnrollmentSecretField[], input: Record<string, unknown> = {}): Promise<EnrollmentView> {
    const secret = await secretDialog({ id, title, description, confirm, fields });
    if (!secret) return this.enrollment_status();
    return this.finishOwnerClaim(await this.call(command, { ...secret, ...input }));
  }

  private async createOwnerClaimDraft(): Promise<OwnerClaimDraft | undefined> {
    const protectedSetup = this.setupCodeRequired;
    const credentials = this.enrollmentMode === "community"
      ? await this.collectConfirmedAdminPassword({
        id: "browser-enrollment-owner",
        title: "Set up your Peppy server",
        description: protectedSetup ? peppyCopy.enrollment_setup_protected : "",
        setupMode: protectedSetup ? "protected" : "open",
        includeSetupToken: protectedSetup,
      })
      : {};
    if (!credentials) return undefined;
    const generated = await this.call<{ passphrase: string }>("enrollment_generate_secret");
    if (!await generatedSecretDialog(generated.passphrase)) return undefined;
    return { ...credentials, passphrase: generated.passphrase };
  }

  private async collectConfirmedAdminPassword(options: { id: string; title: string; description: string; setupMode?: "open" | "protected"; includeSetupToken?: boolean }): Promise<{ adminPassword: string; setupToken?: string } | undefined> {
    let setupToken: string | undefined;
    let errorMessage: string | undefined;
    while (true) {
      const credentials = await secretDialog({
        ...options,
        confirm: "Continue",
        errorMessage,
        fields: [
          ...(options.includeSetupToken ? [{ name: "setupToken", label: "Setup code", autocomplete: "off", value: setupToken }] : []),
          { name: "adminPassword", label: peppyCopy.enrollment_admin_password, autocomplete: "new-password" },
          { name: "adminPasswordConfirm", label: peppyCopy.enrollment_admin_password_confirm, autocomplete: "new-password" },
        ],
      });
      if (!credentials) return undefined;
      setupToken = credentials.setupToken;
      if (credentials.adminPassword !== credentials.adminPasswordConfirm) {
        errorMessage = peppyCopy.admin_password_mismatch;
        continue;
      }
      if (!this.isValidAdminPassword(credentials.adminPassword)) {
        errorMessage = peppyCopy.admin_password_length;
        continue;
      }
      return { adminPassword: credentials.adminPassword, ...(setupToken ? { setupToken } : {}) };
    }
  }

  private isValidAdminPassword(password: string): boolean {
    const byteLength = new TextEncoder().encode(password).byteLength;
    return byteLength >= MIN_ADMIN_PASSWORD_BYTES && byteLength <= MAX_ADMIN_PASSWORD_BYTES;
  }

  private finishOwnerClaim(view: EnrollmentView): EnrollmentView {
    if (view.stage === "error" && view.errorCode === "invalid_setup_token") {
      this.setupCodeRequired = true;
      if (this.ownerClaimDraft) {
        const { setupToken: _setupToken, ...draft } = this.ownerClaimDraft;
        this.ownerClaimDraft = draft;
      }
    }
    if (view.stage === "ready" || view.stage === "phone_connected") { this.ownerClaimDraft = undefined; this.vaultClaimDraft = undefined; }
    return view;
  }

}
