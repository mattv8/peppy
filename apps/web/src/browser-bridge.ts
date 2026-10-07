import type {
  AttachmentView, BridgeError, ContactBookView, ContactEditOutcome, ContactEditRequestInput,
  ContactEditStatus, ContactSyncStatus, ContactView, DesktopBridge, DesktopSnapshot, Draft,
  DraftInput, JoinView, NotificationPreferences, NotificationTarget, PairingIntent, PairingStatus,
  PublicCopy, RecipientSuggestion, RestorableContact, SendDraftInput,
} from "../../desktop/src/bridge";

type RpcReply = { id: number; ok: true; value: unknown } | { id: number; ok: false; error: BridgeError };
type WorkerEvent = { event: "changed" } | { event: "ready"; version: 1 } | { event: "stopped"; reason?: string } | { event: "banner"; candidate: { id: string; title: string; body: string } };
type PendingRequest = { resolve(value: unknown): void; reject(error: Error): void; timer: number };

const REQUEST_TIMEOUT_MS = 30_000;
const MAX_FILE_BYTES = 32 * 1024 * 1024;
const MAX_IDENTITY_BYTES = 1024 * 1024;
const IMAGE_TYPES = new Set(["image/jpeg", "image/png", "image/webp", "image/gif"]);
let activeSecretDialog: (() => void) | undefined;

class BrowserBridgeError extends Error implements BridgeError {
  public constructor(public readonly code: string, message: string, public readonly currentRevision?: string) { super(message); }
}

function unsupported(method: string): Promise<never> {
  return Promise.reject(new BrowserBridgeError("unsupported", `${method} is unavailable in the browser.`));
}

function secretDialog(options: { id: string; title: string; description: string; confirm: string; fields: Array<{ name: string; label: string; autocomplete?: string }> }): Promise<Record<string, string> | null> {
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
      label.htmlFor = input.id;
      label.textContent = field.label;
      label.append(input);
      form.append(label);
      return input;
    });
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

async function chooseIdentityFile(): Promise<{ metadata: unknown; deviceToken: string } | null> {
  const [file] = await chooseFile("application/json");
  if (!file) return null;
  if (file.size > MAX_IDENTITY_BYTES) throw new BrowserBridgeError("invalid-identity", "The identity file is invalid.");
  let identity: unknown;
  try { identity = JSON.parse(await file.text()); }
  catch { throw new BrowserBridgeError("invalid-identity", "The identity file is invalid."); }
  if (typeof identity !== "object" || identity === null || !("metadata" in identity) || !("deviceToken" in identity) || typeof identity.deviceToken !== "string") {
    throw new BrowserBridgeError("invalid-identity", "The identity file is invalid.");
  }
  return { metadata: identity.metadata, deviceToken: identity.deviceToken };
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
    else request.reject(new BrowserBridgeError(message.error.code, message.error.message, message.error.currentRevision));
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
  public dispose(): void { activeSecretDialog?.(); this.stop("disconnected"); }
  public load_state(conversationId?: string): Promise<DesktopSnapshot> { return this.call("load_state", { conversationId }); }
  public save_draft(input: DraftInput): Promise<Draft> { return this.call("save_draft", { input }); }
  public send_draft(input: SendDraftInput): Promise<{ accepted: boolean; status: "queued-local" | "server-accepted" | "gateway-persisted" | "preparing" | "submitted" | "sent" | "delivery-confirmed" | "failed-before-submit" | "failed-confirmed" | "unknown"; reason?: string; revision?: string }> { return this.call("send_draft", { input }); }
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
  public async import_credentials(): Promise<void> { const identity = await chooseIdentityFile(); if (!identity) return; const secret = await secretDialog({ id: "browser-import", title: "Import credentials", description: "Enter the passphrase for this encrypted identity file.", confirm: "Import", fields: [{ name: "passphrase", label: "Passphrase", autocomplete: "new-password" }] }); if (secret) await this.call("import_identity", { ...identity, ...secret }); }
  public configure_server(origin: string): Promise<void> { return origin === location.origin ? Promise.resolve() : Promise.reject(new BrowserBridgeError("invalid-origin", "The browser uses its own secure origin.")); }
  public create_pairing_intent(): Promise<PairingIntent> { return this.call("create_pairing_intent"); }
  public pairing_intent_status(intentToken: string): Promise<PairingStatus> { return this.call("pairing_intent_status", { intentToken }); }
  public approve_pairing_intent(intentToken: string, keyDigest: string): Promise<void> { return this.call("approve_pairing_intent", { intentToken, keyDigest }); }
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
  public join_start(origin: string | null): Promise<JoinView> { return this.call("join_start", { origin: origin ?? location.origin }); }
  public join_status(): Promise<JoinView> { return this.call("join_status"); }
  public join_cancel(): Promise<void> { return this.call("join_cancel"); }
  public async join_confirm(): Promise<JoinView> { const secret = await secretDialog({ id: "browser-join-confirm", title: "Confirm pairing", description: "Enter your device sync passphrase to confirm this pairing.", confirm: "Confirm", fields: [{ name: "passphrase", label: "Passphrase", autocomplete: "current-password" }] }); if (!secret) return this.join_status(); return this.call("join_confirm", secret); }
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
  public hosted_open_billing(): Promise<void> { return unsupported("Hosted billing"); }
  public hosted_provision(): Promise<void> { return unsupported("Hosted provisioning"); }
}
