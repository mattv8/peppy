import type {
  AttachmentView, BridgeError, ContactBookView, ContactEditOutcome, ContactEditRequestInput,
  ContactEditStatus, ContactSyncStatus, ContactView, DesktopBridge, DesktopSnapshot, Draft, HistoryPage,
  AppLockStatus, DraftInput, NotificationPreferences, NotificationTarget, PublicCopy,
  RecipientSuggestion, RestorableContact, SendDraftInput,
} from "../../desktop/src/bridge";
import { peppyCopy } from "../../desktop/src/generated/peppyCopy";
import { validateAccountUrl } from "./account-navigation";
import { beginHostedAuthorization, fetchHostedWebConfig } from "./newhosted-app-auth";

type RpcReply = { id: number; ok: true; value: unknown } | { id: number; ok: false; error: BridgeError };
type WorkerEvent = { event: "changed" } | { event: "ready"; version: 1 } | { event: "stopped"; reason?: string } | { event: "banner"; candidate: { id: string; title: string; body: string } };
type PendingRequest = { resolve(value: unknown): void; reject(error: Error): void; timer: number };
type EnrollmentStage = "server" | "setup" | "signin" | "locked" | "ready" | "waiting_phone" | "verify_phone" | "phone_connected" | "expired" | "error" | "invite_request" | "invite_waiting" | "invite_approved" | "invite_denied" | "invite_expired";
export type EnrollmentView = { mode: "community" | "hosted"; stage: EnrollmentStage; origin?: string; owner?: boolean; hasGateway?: boolean; qrPayload?: string; verificationCode?: string; expiresInSeconds?: number; errorCode?: string; setupCodeRequired?: boolean; vaultOwnerRequired?: boolean; displayName?: string; username?: string };
export type InstanceManagement = { vault_id: string; devices: Array<{ device_id: string; name: string; kind: "browser" | "desktop" | "android" | "ios"; role: "owner" | "device" | "gateway"; revoked: boolean; gateway_online: boolean }>; storage_bytes: number; quota_bytes: number; replay_retention_days: number; isAdmin: boolean; username: string };
export type VaultSummary = { vaultId: string; username: string; isAdmin: boolean; deviceCount: number; storageBytes: number; createdAt: string };
export type VaultInviteView = { inviteId: string; state: "active" | "used" | "revoked" | "expired"; expiresInSeconds: number; createdAt: string };
export type VaultRequestView = { requestId: string; displayName: string; client: "browser" | "desktop" | "android" | "ios"; state: "pending" | "approved" | "denied" | "completed" | "expired"; createdAt: string; expiresInSeconds: number };
type EnrollmentSecretField = { name: string; label: string; autocomplete?: string; value?: string };
type OwnerClaimDraft = { passphrase: string; setupToken?: string; adminPassword?: string };
type VaultClaimDraft = { username: string; adminPassword: string; passphrase: string };

const REQUEST_TIMEOUT_MS = 30_000;
const MAX_FILE_BYTES = 32 * 1024 * 1024;
const MIN_ADMIN_PASSWORD_BYTES = 12;
const MAX_ADMIN_PASSWORD_BYTES = 1024;
const IMAGE_TYPES = new Set(["image/jpeg", "image/png", "image/webp", "image/gif"]);
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

function unsupported(method: string): Promise<never> {
  return Promise.reject(new BrowserBridgeError("unsupported", `${method} is unavailable in the browser.`));
}

function secretDialog(options: { id: string; title: string; description: string; confirm: string; fields: EnrollmentSecretField[]; setupMode?: "open" | "protected"; errorMessage?: string }): Promise<Record<string, string> | null> {
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
    if (options.errorMessage) {
      const error = document.createElement("p");
      error.id = `${options.id}-error`;
      error.setAttribute("role", "alert");
      error.style.color = "var(--peppy-text-error, red)";
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
      finish(Object.fromEntries(inputs.map((input) => [input.name, input.value])));
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
    heading.textContent = "Save your vault shared secret";
    const description = document.createElement("p");
    description.id = `${id}-description`;
    description.textContent = "Save this shared secret before continuing. You need it to recover and connect other devices.";
    const label = document.createElement("label");
    label.htmlFor = `${id}-passphrase`;
    label.textContent = "Vault shared secret";
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
    copy.textContent = "Copy secret";
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
    acknowledgement.append(checkbox, " I saved this shared secret");
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
    document.addEventListener("visibilitychange", this.refreshNotificationContext);
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
    this.failPending(reason, "Peppy locked or stopped. Reconnect to continue.");
    this.revokeBlobUrls();
    this.banners.forEach(notification => notification.close());
    this.banners.clear();
    window.removeEventListener("focus", this.refreshNotificationContext);
    window.removeEventListener("blur", this.refreshNotificationContext);
    document.removeEventListener("visibilitychange", this.refreshNotificationContext);
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

  private revokeBlobUrls(): void { this.blobUrls.forEach((url) => URL.revokeObjectURL(url)); this.blobUrls.clear(); }
  private readonly refreshNotificationContext = (): void => {
    if (!this.notificationContext || this.stopped) return;
    void this.call("set_notification_context", this.notificationContextArgs()).catch(() => undefined);
  };
  private notificationContextArgs(): Record<string, unknown> {
    const permission = typeof Notification === "undefined" ? "unsupported" : Notification.permission;
    return { ...this.notificationContext, notificationPermission: permission, focused: document.visibilityState === "visible" && document.hasFocus() };
  }
  private showBanner(candidate: { id: string; title: string; body: string }): void {
    if (typeof Notification === "undefined" || Notification.permission !== "granted") return;
    try {
      const notification = new Notification(candidate.title, { body: candidate.body, tag: `peppy-${candidate.id}` });
      this.banners.add(notification);
      notification.onclose = () => this.banners.delete(notification);
      void this.call("display_ack", { ids: [candidate.id] }).catch(() => undefined);
    } catch { /* The worker retains the candidate when a browser post throws. */ }
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
  public load_state(conversationId?: string, historyFromMessageId?: string): Promise<DesktopSnapshot> { return this.call("load_state", { conversationId, historyFromMessageId }); }
  public load_history(conversationId: string, beforeMessageId: string): Promise<HistoryPage> { return this.call("load_history", { conversationId, beforeMessageId }); }
  public save_draft(input: DraftInput): Promise<Draft> { return this.call("save_draft", { input }); }
  public send_draft(input: SendDraftInput): Promise<{ accepted: boolean; status: "queued-local" | "server-accepted" | "gateway-persisted" | "preparing" | "submitted" | "sent" | "delivery-confirmed" | "failed-before-submit" | "failed-confirmed" | "unknown"; reason?: string; revision?: string }> { return this.call("send_draft", { input }); }
  public discard_draft(input: { id: string; conversationId: string; expectedRevision: string }): Promise<void> { return this.call("discard_draft", { input }); }
  public mark_seen(visibleMessageIds: string[]): Promise<void> { return this.call("mark_seen", { visibleMessageIds }); }
  public dismiss_notification(target: NotificationTarget): Promise<void> { return this.call("dismiss_notification", { target }); }
  public dismiss_all_notifications(): Promise<void> { return this.call("dismiss_all_notifications"); }
  public set_app_muted(sourceDeviceId: string, packageName: string, appName: string, muted: boolean): Promise<void> { return this.call("set_app_muted", { sourceDeviceId, packageName, appName, muted }); }
  public mark_notifications_seen(targets: NotificationTarget[]): Promise<void> { return this.call("mark_notifications_seen", { targets }); }
  public set_notification_preferences(preferences: NotificationPreferences): Promise<void> { return this.call("set_notification_preferences", { preferences }); }
  public set_notification_context(view: "conversations" | "notifications" | "settings" | "contacts", conversationId?: string): Promise<void> {
    this.notificationContext = { view, ...(conversationId ? { conversationId } : {}) };
    return this.call("set_notification_context", this.notificationContextArgs());
  }
  public list_contact_books(): Promise<ContactBookView[]> { return this.call("list_contact_books"); }
  public forget_contact_book(bookId: string): Promise<void> { return this.call("forget_contact_book", { bookId }); }
  public list_contacts(bookId: string, query?: string, offset?: number): Promise<ContactView[]> { return this.call("list_contacts", { bookId, query, offset }); }
  public submit_contact_edit(input: ContactEditRequestInput): Promise<ContactEditOutcome> { return this.call("submit_contact_edit", { input }); }
  public list_contact_edits(bookId?: string): Promise<ContactEditStatus[]> { return this.call("list_contact_edits", { bookId }); }
  public search_contact_recipients(query: string, sourceDeviceId?: string): Promise<RecipientSuggestion[]> { return this.call("search_contact_recipients", { query, sourceDeviceId }); }
  public request_contact_repair(): Promise<ContactSyncStatus> { return this.call("request_contact_repair"); }
  public list_restorable_contacts(bookId: string): Promise<RestorableContact[]> { return this.call("list_restorable_contacts", { bookId }); }
  public restore_contact(bookId: string, contactId: string): Promise<ContactEditOutcome> { return this.call("restore_contact", { bookId, contactId }); }
  public subscribe(listener: () => void): () => void { this.listeners.add(listener); return () => this.listeners.delete(listener); }
  public subscribe_lifecycle(): () => void { return () => undefined; }
  public subscribe_lifecycle_finished(): () => void { return () => undefined; }
  public lock_sync(): Promise<void> { return this.call("lock_sync"); }

  public async unlock_sync(): Promise<void> { const secret = await secretDialog({ id: "browser-unlock", title: "Unlock Peppy", description: "Enter your device sync passphrase to access your messages.", confirm: "Unlock", fields: [{ name: "passphrase", label: "Passphrase", autocomplete: "current-password" }] }); if (secret) await this.call("unlock", secret); }
  public configure_server(origin: string): Promise<void> { return origin === location.origin ? Promise.resolve() : Promise.reject(new BrowserBridgeError("invalid-origin", "The browser uses its own secure origin.")); }
  public get_app_lock_status(): Promise<AppLockStatus> { return unsupported("App lock"); }
  public set_app_lock(_enabled: boolean): Promise<boolean> { return unsupported("App lock"); }
  public unlock_app(): Promise<boolean> { return unsupported("App lock"); }
  public async request_notification_permission(): Promise<"granted" | "denied" | "unknown"> { if (!("Notification" in window)) return "unknown"; const permission = await Notification.requestPermission(); return permission === "default" ? "unknown" : permission; }
  public async pick_attachments(): Promise<AttachmentView[]> {
    const files = await chooseFile("*/*", true);
    if (files.some((file) => file.size > MAX_FILE_BYTES)) throw new BrowserBridgeError("attachment-too-large", "Attachments must be 32 MiB or smaller.");
    return Promise.all(files.map(async (file) => { const bytes = await file.arrayBuffer(); return this.call<AttachmentView>("prepare_attachment", { bytes, displayName: file.name, mediaType: file.type || "application/octet-stream" }, [bytes]); }));
  }
  public retry_attachment(id: string): Promise<void> { return this.call("retry_attachment", { id }); }
  public async save_attachment(id: string): Promise<boolean> { const file = await this.call<{ bytes: ArrayBuffer; displayName: string; mediaType: string }>("export_attachment", { id }); const url = URL.createObjectURL(new Blob([file.bytes], { type: file.mediaType })); this.blobUrls.add(url); const link = document.createElement("a"); link.href = url; link.download = file.displayName; link.click(); window.setTimeout(() => { URL.revokeObjectURL(url); this.blobUrls.delete(url); }, 0); return true; }
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
  public enrollment_sign_out(): Promise<void> { return this.call("enrollment_sign_out"); }
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
  public async enrollment_rotate_phone(method: "transfer" | "shared_secret"): Promise<EnrollmentView> {
    const view = await this.enrollment_status();
    if (view.stage !== "waiting_phone" && view.stage !== "expired") return view;
    await this.call("enrollment_cancel");
    return this.finishOwnerClaim(await this.call("enrollment_add_phone", { method }));
  }
  public async enrollment_cancel_phone(): Promise<EnrollmentView> {
    const view = await this.enrollment_status();
    if (view.stage !== "waiting_phone" && view.stage !== "expired") return view;
    await this.call("enrollment_cancel");
    return this.enrollment_status();
  }
  public reveal_vault_secret(): Promise<void> { return Promise.reject(new BrowserBridgeError("unavailable", "Vault shared secret reveal is unavailable in the browser.")); }
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
      await beginHostedAuthorization(config);
      return this.enrollment_status();
    }
    return this.enrollmentSecretAction("enrollment_sign_in", "browser-enrollment-signin", "Sign in to Peppy", "Enter your account password.", "Sign in", [{ name: "adminPassword", label: "Password", autocomplete: "current-password" }], input);
  }
  public async enrollment_connect_shared_secret(input?: { vaultOwner?: string }): Promise<EnrollmentView> {
    return this.enrollmentSecretAction("enrollment_connect_shared_secret", "browser-enrollment-secret", "Enter device secret", "Enter the vault shared secret provided by an owner.", "Connect", [{ name: "passphrase", label: "Vault shared secret", autocomplete: "one-time-code" }], input);
  }
  public async enrollment_approve(): Promise<EnrollmentView> {
    return this.call<EnrollmentView>("enrollment_approve");
  }
  public open_composer(): Promise<void> { return unsupported("Additional composer windows"); }
  public set_start_at_login(): Promise<void> { return unsupported("Start at login"); }
  public popout_conversation(): Promise<{ headCreated: boolean; warning?: string }> { return unsupported("Floating conversations"); }
  public hide_head(): Promise<void> { return unsupported("Floating conversations"); }
  public close_composer(): Promise<void> { return unsupported("Closing a composer window"); }
  public close_head_panel(): Promise<void> { return unsupported("Closing a floating conversation"); }
  public acknowledge_lifecycle(): Promise<void> { return unsupported("Window lifecycle controls"); }
  public window(): Promise<void> { return unsupported("Window controls"); }
  public hosted_account(): Promise<never> { return unsupported("Hosted accounts"); }
  public hosted_sign_in(): Promise<never> { return unsupported("Hosted sign-in"); }
  public hosted_sign_out(): Promise<void> { return unsupported("Hosted sign-out"); }
  public hosted_open_billing(): Promise<void> {
    if (!this.accountUrl) return unsupported("Hosted billing");
    window.open(this.accountUrl, "_blank", "noopener,noreferrer");
    return Promise.resolve();
  }
  public hosted_provision(): Promise<void> { return unsupported("Hosted provisioning"); }

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
