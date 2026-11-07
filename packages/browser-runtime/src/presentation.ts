const SETTINGS_KEY = "notification-preferences";
const LEGACY_VAULT_KEY = "notification-preferences-vault";
const BANNER_LIMIT = 20;
const STALE_BANNER_MS = 2 * 60 * 1000;
const MAX_NOTIFICATION_KEY_BYTES = 512;
const MAX_NOTIFICATION_TARGET_COMPONENT_BYTES = 128;
const MAX_NOTIFICATION_TARGET_ID_BYTES = MAX_NOTIFICATION_KEY_BYTES * 6 + 256;
const MAX_ICON_BASE64_BYTES = 10_924;
const MAX_ICON_DECODED_BYTES = 8_192;
const MESSAGE_GLYPH = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'%3E%3Cpath fill='%236b7280' d='M4 6h24v16H10l-6 5V6z'/%3E%3C/svg%3E";

export type NotificationPreferences = {
  messageBanners: boolean;
  mirroredBanners: boolean;
  preview: "full" | "hidden";
  showSource?: boolean;
  muteWithHeads?: boolean;
};

export interface NotificationSettings {
  load(): Promise<NotificationPreferences | undefined>;
}

export type BannerCandidate = {
  id: string;
  kind: "message" | "notification";
  conversationId?: string;
  notificationTarget?: { sourceDeviceId: string; notificationKey: string; lifetime: string };
  title: string;
  body: string;
  createdAt: number;
  sourceName?: string;
  appIconPngBase64?: string;
  icon?: string;
};

export type BannerContext = { view: "conversations" | "notifications" | "settings" | "contacts"; conversationId?: string; notificationPermission?: "granted" | "denied" | "default" | "unsupported"; focused?: boolean; requireInteraction?: boolean };

export interface BannerSession {
  query(command: string, args: Record<string, unknown>): Promise<unknown>;
  mutate(command: string, args: Record<string, unknown>): Promise<unknown>;
}

/** Reads the vault-wide preferences owned by Rust core. */
export class CoreNotificationSettings implements NotificationSettings {
  public constructor(private readonly session: Pick<BannerSession, "query">) {}

  public async load(): Promise<NotificationPreferences | undefined> {
    const value = await this.session.query("notification_preferences", {});
    return value === null || value === undefined ? undefined : notificationPreferences(value);
  }
}

export interface NotificationAdapter {
  readonly available: boolean;
  readonly permission: "granted" | "denied" | "default";
  show(notification: { title: string; body: string; tag: string; icon?: string; requireInteraction?: true }): void | { close(): void };
}

export type BannerDelivery = (candidate: BannerCandidate) => void;

/** Drains the core's one-use banner ledger without keeping plaintext outside the Worker. */
export class BannerPresenter {
  private context?: BannerContext;
  private draining?: Promise<void>;
  private readonly awaitingDisplay = new Set<string>();
  private readonly displayedNotifications = new Set<{ close(): void }>();

  public constructor(
    private readonly session: BannerSession,
    private readonly settings: NotificationSettings,
    private readonly notification: NotificationAdapter,
    private readonly deliverToRenderer: () => BannerDelivery | undefined,
    private readonly now: () => number = Date.now,
  ) {}

  public setContext(context: BannerContext): void { this.context = context; }

  public drain(): Promise<void> {
    this.draining ??= this.drainCandidates().finally(() => { this.draining = undefined; });
    return this.draining;
  }

  public async acknowledgeDisplayed(ids: readonly string[]): Promise<void> {
    const acknowledged = ids.filter(id => this.awaitingDisplay.delete(id));
    if (acknowledged.length > 0) await this.session.mutate("ack_banner_candidates", { ids: acknowledged });
  }

  public clear(): void {
    this.context = undefined;
    this.awaitingDisplay.clear();
    this.displayedNotifications.forEach(notification => notification.close());
    this.displayedNotifications.clear();
  }

  private async drainCandidates(): Promise<void> {
    const candidates = bannerCandidates(await this.session.query("pending_banner_candidates", { limit: BANNER_LIMIT }));
    const preferences = await this.settings.load() ?? DEFAULT_NOTIFICATION_PREFERENCES;
    const acknowledge: string[] = [];
    for (const candidate of candidates) {
      if (this.awaitingDisplay.has(candidate.id)) continue;
      if (this.suppress(candidate, preferences)) {
        acknowledge.push(candidate.id);
        continue;
      }
      if (this.notification.available && this.notification.permission === "granted") {
        try {
          const notification = this.notification.show({ ...bannerText(candidate, preferences), tag: `peppy-${candidate.id}`, ...(this.context?.requireInteraction === true ? { requireInteraction: true } : {}) });
          if (notification) this.displayedNotifications.add(notification);
          acknowledge.push(candidate.id);
        } catch {
          // A failed browser post remains in the core ledger for a later retry.
        }
        continue;
      }
      if (this.notification.available || this.context?.notificationPermission !== "granted") {
        // The renderer must not bypass a user's browser permission decision.
        acknowledge.push(candidate.id);
        continue;
      }
      {
        const renderer = this.deliverToRenderer();
        if (renderer) {
          renderer(redactedCandidate(candidate, preferences));
          this.awaitingDisplay.add(candidate.id);
        } else {
          // Unsupported/denied browser notifications are an intentional policy drop.
          acknowledge.push(candidate.id);
        }
      }
    }
    if (acknowledge.length > 0) await this.session.mutate("ack_banner_candidates", { ids: acknowledge });
  }

  private suppress(candidate: BannerCandidate, preferences: NotificationPreferences): boolean {
    const enabled = candidate.kind === "message" ? preferences.messageBanners : preferences.mirroredBanners;
    return !enabled || this.now() - candidate.createdAt > STALE_BANNER_MS ||
      (this.context?.focused === true && ((this.context.view === "notifications" && candidate.kind === "notification") ||
        (this.context.view === "conversations" && this.context.conversationId === candidate.conversationId)));
  }
}

export class BrowserNotificationAdapter implements NotificationAdapter {
  public constructor(private readonly notification: typeof Notification | undefined = globalThis.Notification) {}
  public get available(): boolean { return this.notification !== undefined; }
  public get permission(): "granted" | "denied" | "default" { return this.notification?.permission ?? "denied"; }
  public show(notification: { title: string; body: string; tag: string; icon?: string; requireInteraction?: true }): { close(): void } {
    if (!this.notification || this.notification.permission !== "granted") throw new Error("Notifications are unavailable");
    return new this.notification(notification.title, { body: notification.body, tag: notification.tag, ...(notification.icon ? { icon: notification.icon } : {}), ...(notification.requireInteraction ? { requireInteraction: true } : {}) });
  }
}

export const DEFAULT_NOTIFICATION_PREFERENCES: NotificationPreferences = {
  messageBanners: true,
  mirroredBanners: true,
  preview: "full",
  showSource: false,
  muteWithHeads: true,
};

export function notificationPreferences(value: unknown): NotificationPreferences {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error("Invalid notification preferences");
  const preferences = value as Record<string, unknown>;
  if (preferences.messageBanners === undefined && preferences.mirroredBanners === undefined && preferences.preview === undefined) {
    throw new Error("Invalid notification preferences");
  }
  const merged = { ...DEFAULT_NOTIFICATION_PREFERENCES, ...preferences };
  if (typeof merged.messageBanners !== "boolean" || typeof merged.mirroredBanners !== "boolean" || (merged.preview !== "full" && merged.preview !== "hidden") || typeof merged.showSource !== "boolean" || (merged.muteWithHeads !== undefined && typeof merged.muteWithHeads !== "boolean")) {
    throw new Error("Invalid notification preferences");
  }
  return merged;
}

function bannerCandidates(value: unknown): BannerCandidate[] {
  if (!Array.isArray(value)) return [];
  return value.slice(0, BANNER_LIMIT).flatMap(candidate => {
    if (typeof candidate !== "object" || candidate === null || Array.isArray(candidate)) return [];
    const value = candidate as Record<string, unknown>;
    if ((value.kind !== "message" && value.kind !== "notification") || typeof value.title !== "string" ||
      typeof value.body !== "string" || !Number.isSafeInteger(value.createdAt) || !validNotificationTarget(value.notificationTarget) ||
      typeof value.id !== "string" || !validBannerCandidateId(value.id, value.kind, value.notificationTarget)) return [];
    const target = value.notificationTarget;
    return [{ id: value.id, kind: value.kind, ...(typeof value.conversationId === "string" ? { conversationId: value.conversationId } : {}), ...(validNotificationTarget(target) && target ? { notificationTarget: target } : {}), title: value.title, body: value.body, createdAt: value.createdAt as number, ...(nonEmptyString(value.sourceName) ? { sourceName: value.sourceName } : {}), ...(validPngBase64(value.appIconPngBase64) ? { appIconPngBase64: value.appIconPngBase64 } : {}) }];
  });
}

function validBannerCandidateId(id: string, kind: BannerCandidate["kind"], target: unknown): boolean {
  if (/^[a-zA-Z0-9-]{1,128}$/.test(id)) return true;
  if (kind !== "notification" || !validNotificationTarget(target) || !target || !boundedUtf8(id, MAX_NOTIFICATION_TARGET_ID_BYTES)) return false;
  try {
    const parsed = JSON.parse(id) as unknown;
    if (!validNotificationTarget(parsed) || !parsed || Object.keys(parsed).length !== 3) return false;
    return parsed.sourceDeviceId === target.sourceDeviceId && parsed.notificationKey === target.notificationKey && parsed.lifetime === target.lifetime;
  } catch {
    return false;
  }
}

function validNotificationTarget(value: unknown): value is { sourceDeviceId: string; notificationKey: string; lifetime: string } | null | undefined {
  if (value === undefined || value === null) return true;
  if (typeof value !== "object" || Array.isArray(value)) return false;
  const target = value as Record<string, unknown>;
  return boundedUtf8(target.sourceDeviceId, MAX_NOTIFICATION_TARGET_COMPONENT_BYTES) &&
    boundedUtf8(target.notificationKey, MAX_NOTIFICATION_KEY_BYTES) &&
    boundedUtf8(target.lifetime, MAX_NOTIFICATION_TARGET_COMPONENT_BYTES);
}

function boundedUtf8(value: unknown, maxBytes: number): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= maxBytes && new TextEncoder().encode(value).byteLength <= maxBytes;
}

function bannerText(candidate: BannerCandidate, preferences: NotificationPreferences): { title: string; body: string; icon?: string } {
  if (preferences.preview === "full") {
    const icon = candidateIcon(candidate, true);
    return { title: candidate.title, body: candidate.body, ...(icon ? { icon } : {}) };
  }
  const title = preferences.showSource && candidate.sourceName
    ? candidate.sourceName
    : candidate.kind === "message" ? "New message" : "New notification";
  const icon = candidateIcon(candidate, preferences.showSource === true);
  return { title, body: "", ...(icon ? { icon } : {}) };
}

function redactedCandidate(candidate: BannerCandidate, preferences: NotificationPreferences): BannerCandidate {
  const text = bannerText(candidate, preferences);
  return {
    id: candidate.id,
    kind: candidate.kind,
    ...(candidate.conversationId ? { conversationId: candidate.conversationId } : {}),
    ...(candidate.notificationTarget ? { notificationTarget: candidate.notificationTarget } : {}),
    title: text.title,
    body: text.body,
    createdAt: candidate.createdAt,
    ...(text.icon ? { icon: text.icon } : {}),
  };
}

function candidateIcon(candidate: BannerCandidate, allowAppIcon: boolean): string | undefined {
  if (candidate.kind === "message") return MESSAGE_GLYPH;
  return allowAppIcon && candidate.appIconPngBase64 ? `data:image/png;base64,${candidate.appIconPngBase64}` : undefined;
}

function nonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0;
}

function validPngBase64(value: unknown): value is string {
  if (typeof value !== "string" || value.length > MAX_ICON_BASE64_BYTES || !/^[A-Za-z0-9+/]*={0,2}$/.test(value) || value.length % 4 !== 0) return false;
  try {
    const encoded = atob(value);
    if (encoded.length > MAX_ICON_DECODED_BYTES || encoded.length < 45) return false;
    const bytes = Uint8Array.from(encoded, character => character.charCodeAt(0));
    if (bytes[0] !== 137 || bytes[1] !== 80 || bytes[2] !== 78 || bytes[3] !== 71 || bytes[4] !== 13 || bytes[5] !== 10 || bytes[6] !== 26 || bytes[7] !== 10) return false;
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    let offset = 8;
    let sawIdat = false;
    while (offset < bytes.length) {
      if (bytes.length - offset < 12) return false;
      const length = view.getUint32(offset);
      if (length > bytes.length - offset - 12) return false;
      const typeOffset = offset + 4;
      const dataOffset = offset + 8;
      const end = dataOffset + length;
      if (!chunkType(bytes, typeOffset) || view.getUint32(end) !== pngCrc(bytes, typeOffset, end)) return false;
      const type = String.fromCharCode(bytes[typeOffset], bytes[typeOffset + 1], bytes[typeOffset + 2], bytes[typeOffset + 3]);
      if (offset === 8) {
        if (type !== "IHDR" || length !== 13 || view.getUint32(dataOffset) !== 32 || view.getUint32(dataOffset + 4) !== 32 || !validIhdr(bytes, dataOffset)) return false;
      } else if (type === "IHDR") return false;
      if (type === "IDAT") sawIdat = true;
      if (type === "IEND") return length === 0 && sawIdat && end + 4 === bytes.length;
      offset = end + 4;
    }
    return false;
  } catch {
    return false;
  }
}

function validIhdr(bytes: Uint8Array, offset: number): boolean {
  const bitDepth = bytes[offset + 8];
  const colorType = bytes[offset + 9];
  return bytes[offset + 10] === 0 && bytes[offset + 11] === 0 && (bytes[offset + 12] === 0 || bytes[offset + 12] === 1)
    && ((colorType === 0 && [1, 2, 4, 8, 16].includes(bitDepth)) || (colorType === 2 && [8, 16].includes(bitDepth)) || (colorType === 3 && [1, 2, 4, 8].includes(bitDepth)) || (colorType === 4 && [8, 16].includes(bitDepth)) || (colorType === 6 && [8, 16].includes(bitDepth)));
}

function chunkType(bytes: Uint8Array, offset: number): boolean {
  return [bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]].every(value => (value >= 65 && value <= 90) || (value >= 97 && value <= 122));
}

function pngCrc(bytes: Uint8Array, start: number, end: number): number {
  let crc = 0xffffffff;
  for (let index = start; index < end; index += 1) {
    crc ^= bytes[index];
    for (let bit = 0; bit < 8; bit += 1) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

/** Legacy pre-sync presentation flags, bound to one vault before adoption and then cleared. */
export class IndexedDbNotificationSettings {
  private database?: IDBDatabase;

  public constructor(private readonly name: string, private readonly indexedDB: IDBFactory = globalThis.indexedDB) {}

  /** Atomically claims this origin's legacy flags for exactly one authenticated vault. */
  public async claimForVault(vaultId: string): Promise<NotificationPreferences | undefined> {
    try {
      const database = await this.open();
      const transaction = database.transaction("settings", "readwrite");
      const store = transaction.objectStore("settings");
      const value = await request(store.get(SETTINGS_KEY));
      if (value === undefined) {
        await transactionDone(transaction);
        return undefined;
      }
      const preferences = notificationPreferences(value);
      const boundVault = await request(store.get(LEGACY_VAULT_KEY));
      if (boundVault !== undefined && boundVault !== vaultId) {
        await transactionDone(transaction);
        return undefined;
      }
      if (boundVault === undefined) store.put(vaultId, LEGACY_VAULT_KEY);
      await transactionDone(transaction);
      return preferences;
    } catch {
      throw new Error("Presentation settings are unavailable");
    }
  }

  public async clear(): Promise<void> {
    try {
      const database = await this.open();
      const transaction = database.transaction("settings", "readwrite");
      const store = transaction.objectStore("settings");
      store.delete(SETTINGS_KEY);
      store.delete(LEGACY_VAULT_KEY);
      await transactionDone(transaction);
    } catch {
      throw new Error("Presentation settings are unavailable");
    }
  }

  private open(): Promise<IDBDatabase> {
    if (this.database) return Promise.resolve(this.database);
    return new Promise((resolve, reject) => {
      const opening = this.indexedDB.open(this.name, 1);
      opening.onupgradeneeded = () => opening.result.createObjectStore("settings");
      opening.onerror = () => reject(opening.error);
      opening.onsuccess = () => { this.database = opening.result; resolve(opening.result); };
    });
  }
}

function request<T>(value: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => { value.onsuccess = () => resolve(value.result); value.onerror = () => reject(value.error); });
}

function transactionDone(transaction: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => { transaction.oncomplete = () => resolve(); transaction.onabort = () => reject(transaction.error); transaction.onerror = () => reject(transaction.error); });
}
