const SETTINGS_KEY = "notification-preferences";
const BANNER_LIMIT = 20;
const STALE_BANNER_MS = 2 * 60 * 1000;

export type NotificationPreferences = {
  messageBanners: boolean;
  mirroredBanners: boolean;
  preview: "full" | "hidden";
};

export interface NotificationSettings {
  load(): Promise<NotificationPreferences | undefined>;
  save(preferences: NotificationPreferences): Promise<void>;
}

export type BannerCandidate = {
  id: string;
  kind: "message" | "notification";
  conversationId?: string;
  title: string;
  body: string;
  createdAt: number;
};

export type BannerContext = { view: "conversations" | "notifications" | "settings" | "contacts"; conversationId?: string; notificationPermission?: "granted" | "denied" | "default" | "unsupported"; focused?: boolean };

export interface BannerSession {
  query(command: string, args: Record<string, unknown>): Promise<unknown>;
  mutate(command: string, args: Record<string, unknown>): Promise<unknown>;
}

export interface NotificationAdapter {
  readonly available: boolean;
  readonly permission: "granted" | "denied" | "default";
  show(notification: { title: string; body: string; tag: string }): void | { close(): void };
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
    const preferences = await this.settings.load() ?? DEFAULT_NOTIFICATION_PREFERENCES;
    const candidates = bannerCandidates(await this.session.query("pending_banner_candidates", { limit: BANNER_LIMIT }));
    const acknowledge: string[] = [];
    for (const candidate of candidates) {
      if (this.awaitingDisplay.has(candidate.id)) continue;
      if (this.suppress(candidate, preferences)) {
        acknowledge.push(candidate.id);
        continue;
      }
      if (this.notification.available && this.notification.permission === "granted") {
        try {
          const notification = this.notification.show({ ...bannerText(candidate, preferences), tag: `peppy-${candidate.id}` });
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
  public show(notification: { title: string; body: string; tag: string }): { close(): void } {
    if (!this.notification || this.notification.permission !== "granted") throw new Error("Notifications are unavailable");
    return new this.notification(notification.title, { body: notification.body, tag: notification.tag });
  }
}

export const DEFAULT_NOTIFICATION_PREFERENCES: NotificationPreferences = {
  messageBanners: true,
  mirroredBanners: true,
  preview: "full",
};

export function notificationPreferences(value: unknown): NotificationPreferences {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error("Invalid notification preferences");
  const preferences = value as Record<string, unknown>;
  if (preferences.messageBanners === undefined && preferences.mirroredBanners === undefined && preferences.preview === undefined) {
    throw new Error("Invalid notification preferences");
  }
  const merged = { ...DEFAULT_NOTIFICATION_PREFERENCES, ...preferences };
  if (typeof merged.messageBanners !== "boolean" || typeof merged.mirroredBanners !== "boolean" || (merged.preview !== "full" && merged.preview !== "hidden")) {
    throw new Error("Invalid notification preferences");
  }
  return merged;
}

function bannerCandidates(value: unknown): BannerCandidate[] {
  if (!Array.isArray(value)) return [];
  return value.slice(0, BANNER_LIMIT).flatMap(candidate => {
    if (typeof candidate !== "object" || candidate === null || Array.isArray(candidate)) return [];
    const value = candidate as Record<string, unknown>;
    if (typeof value.id !== "string" || !/^[a-zA-Z0-9-]{1,128}$/.test(value.id) ||
      (value.kind !== "message" && value.kind !== "notification") || typeof value.title !== "string" ||
      typeof value.body !== "string" || !Number.isSafeInteger(value.createdAt) || !validNotificationTarget(value.notificationTarget)) return [];
    return [{ id: value.id, kind: value.kind, ...(typeof value.conversationId === "string" ? { conversationId: value.conversationId } : {}), title: value.title, body: value.body, createdAt: value.createdAt as number }];
  });
}

function validNotificationTarget(value: unknown): boolean {
  if (value === undefined || value === null) return true;
  if (typeof value !== "object" || Array.isArray(value)) return false;
  const target = value as Record<string, unknown>;
  return typeof target.sourceDeviceId === "string" && typeof target.notificationKey === "string" && typeof target.lifetime === "string";
}

function bannerText(candidate: BannerCandidate, preferences: NotificationPreferences): { title: string; body: string } {
  if (preferences.preview === "full") return { title: candidate.title, body: candidate.body };
  return candidate.kind === "message"
    ? { title: "New message", body: "Open Peppy to view it." }
    : { title: "New notification", body: "Open Peppy to view it." };
}

function redactedCandidate(candidate: BannerCandidate, preferences: NotificationPreferences): BannerCandidate {
  return { ...candidate, ...bannerText(candidate, preferences) };
}

/** Stores only nonsecret presentation flags in a Worker-owned IndexedDB record. */
export class IndexedDbNotificationSettings implements NotificationSettings {
  private database?: IDBDatabase;

  public constructor(private readonly name: string, private readonly indexedDB: IDBFactory = globalThis.indexedDB) {}

  public async load(): Promise<NotificationPreferences | undefined> {
    try {
      const database = await this.open();
      const transaction = database.transaction("settings", "readonly");
      const value = await request(transaction.objectStore("settings").get(SETTINGS_KEY));
      await transactionDone(transaction);
      return value === undefined ? undefined : notificationPreferences(value);
    } catch {
      throw new Error("Presentation settings are unavailable");
    }
  }

  public async save(preferences: NotificationPreferences): Promise<void> {
    try {
      const database = await this.open();
      const transaction = database.transaction("settings", "readwrite");
      transaction.objectStore("settings").put(notificationPreferences(preferences), SETTINGS_KEY);
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
