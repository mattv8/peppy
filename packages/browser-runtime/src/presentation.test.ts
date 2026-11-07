import "fake-indexeddb/auto";
import { describe, expect, it, vi } from "vitest";
import { BannerPresenter, BrowserNotificationAdapter, CoreNotificationSettings, IndexedDbNotificationSettings, notificationPreferences, type BannerCandidate, type NotificationAdapter, type NotificationPreferences, type NotificationSettings } from "./presentation.js";

const preferences: NotificationSettings = {
  load: async () => ({ messageBanners: true, mirroredBanners: true, preview: "full" }),
};
const VALID_APP_ICON = "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAAAaUlEQVR42mPQrPz/fyAxw6gDkDmXXiAwLg3UUoMzBOjtCAZaWkCMGgZaW0BIDd5ESA9HEMwFtHYEUdmQ2o7Amw3p4QiC5QC9yoDRgmi0IBotiEYLotGCaLQgGi2IRguiwVcQjfYNBwIDAAwPd9Z4B+yXAAAAAElFTkSuQmCC";
const MESSAGE_GLYPH = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'%3E%3Cpath fill='%236b7280' d='M4 6h24v16H10l-6 5V6z'/%3E%3C/svg%3E";

function pngChunk(type: string, data: Uint8Array): Uint8Array {
  const chunk = new Uint8Array(data.length + 12);
  const view = new DataView(chunk.buffer);
  view.setUint32(0, data.length);
  chunk.set([...type].map(character => character.charCodeAt(0)), 4);
  chunk.set(data, 8);
  view.setUint32(data.length + 8, fixtureCrc(chunk.subarray(4, data.length + 8)));
  return chunk;
}

function fixtureCrc(bytes: Uint8Array): number {
  let crc = 0xffffffff;
  for (const value of bytes) {
    crc ^= value;
    for (let bit = 0; bit < 8; bit += 1) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function paddedPng(decodedLength: number): string {
  const header = new Uint8Array(13);
  const view = new DataView(header.buffer);
  view.setUint32(0, 32);
  view.setUint32(4, 32);
  header.set([8, 6, 0, 0, 0], 8);
  const chunks = [
    new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10]),
    pngChunk("IHDR", header),
    pngChunk("tEST", new Uint8Array(decodedLength - 69)),
    pngChunk("IDAT", new Uint8Array()),
    pngChunk("IEND", new Uint8Array()),
  ];
  const bytes = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.length, 0));
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return btoa(String.fromCharCode(...bytes));
}

function png(...chunks: Array<[string, Uint8Array]>): string {
  const encodedChunks = [
    new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10]),
    ...chunks.map(([type, data]) => pngChunk(type, data)),
  ];
  const bytes = new Uint8Array(encodedChunks.reduce((total, chunk) => total + chunk.length, 0));
  let offset = 0;
  for (const chunk of encodedChunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return btoa(String.fromCharCode(...bytes));
}

function pngHeader(bitDepth = 8, colorType = 6): Uint8Array {
  const header = new Uint8Array(13);
  const view = new DataView(header.buffer);
  view.setUint32(0, 32);
  view.setUint32(4, 32);
  header.set([bitDepth, colorType, 0, 0, 0], 8);
  return header;
}

const PNG_AT_DECODED_BOUNDARY = paddedPng(8192);
const PNG_OVER_DECODED_BOUNDARY = paddedPng(8193);
const CORE_NOTIFICATION_TARGET = { sourceDeviceId: "00000000-0000-4000-8000-000000000001", notificationKey: "notification-1", lifetime: "00000000-0000-4000-8000-000000000002" };
const CORE_NOTIFICATION_ID = '{"sourceDeviceId":"00000000-0000-4000-8000-000000000001","notificationKey":"notification-1","lifetime":"00000000-0000-4000-8000-000000000002"}';

function candidate(overrides: Partial<BannerCandidate> = {}): BannerCandidate {
  return { id: "candidate-1", kind: "message", conversationId: "conversation-1", title: "Private title", body: "Private body", createdAt: Date.now(), ...overrides };
}

function adapter(overrides: Partial<NotificationAdapter> = {}): NotificationAdapter & { shown: Array<{ title: string; body: string; tag: string; icon?: string; requireInteraction?: true }> } {
  const result = {
    available: true,
    permission: "granted" as const,
    shown: [] as Array<{ title: string; body: string; tag: string; icon?: string; requireInteraction?: true }>,
    show(notification: { title: string; body: string; tag: string; icon?: string; requireInteraction?: true }) { result.shown.push(notification); },
    ...overrides,
  };
  return result;
}

async function seedLegacySettings(name: string, value: NotificationPreferences): Promise<void> {
  const database = await new Promise<IDBDatabase>((resolve, reject) => {
    const opening = indexedDB.open(name, 1);
    opening.onupgradeneeded = () => opening.result.createObjectStore("settings");
    opening.onerror = () => reject(opening.error);
    opening.onsuccess = () => resolve(opening.result);
  });
  await new Promise<void>((resolve, reject) => {
    const transaction = database.transaction("settings", "readwrite");
    transaction.objectStore("settings").put(value, "notification-preferences");
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error);
    transaction.onabort = () => reject(transaction.error);
  });
  database.close();
}

describe("Worker notification presentation settings", () => {
  it("claims validated legacy display flags across a Worker settings reload", async () => {
    const name = `presentation-${crypto.randomUUID()}`;
    await seedLegacySettings(name, { messageBanners: true, mirroredBanners: false, preview: "full" });
    await expect(new IndexedDbNotificationSettings(name).claimForVault("00000000-0000-4000-8000-000000000001")).resolves.toEqual({ messageBanners: true, mirroredBanners: false, preview: "full", showSource: false, muteWithHeads: true });
  });

  it("binds legacy settings to one vault before they can be adopted", async () => {
    const name = `presentation-${crypto.randomUUID()}`;
    const legacy = new IndexedDbNotificationSettings(name);
    await seedLegacySettings(name, { messageBanners: true, mirroredBanners: false, preview: "hidden" });
    await expect(legacy.claimForVault("00000000-0000-4000-8000-000000000001")).resolves.toMatchObject({ preview: "hidden" });
    await expect(legacy.claimForVault("00000000-0000-4000-8000-000000000002")).resolves.toBeUndefined();
  });

  it("reads preferences from core on every banner drain", async () => {
    const query = vi.fn()
      .mockResolvedValueOnce([candidate({ id: "first" })])
      .mockResolvedValueOnce({ messageBanners: true, mirroredBanners: true, preview: "full" })
      .mockResolvedValueOnce([candidate({ id: "second" })])
      .mockResolvedValueOnce({ messageBanners: false, mirroredBanners: true, preview: "full" });
    const notification = adapter();
    const presenter = new BannerPresenter({ query, mutate: async () => ({}) }, new CoreNotificationSettings({ query }), notification, () => undefined);
    await presenter.drain();
    await presenter.drain();
    expect(notification.shown).toEqual([expect.objectContaining({ title: "Private title", body: "Private body", tag: "peppy-first" })]);
  });

  it("applies preferences loaded after an awaited candidate query", async () => {
    let current: NotificationPreferences = { messageBanners: true, mirroredBanners: true, preview: "full" };
    let resolveCandidates!: (candidates: BannerCandidate[]) => void;
    let markQueryStarted!: () => void;
    const candidateQuery = new Promise<BannerCandidate[]>(resolve => { resolveCandidates = resolve; });
    const queryStarted = new Promise<void>(resolve => { markQueryStarted = resolve; });
    const notification = adapter();
    const presenter = new BannerPresenter({
      query: async () => { markQueryStarted(); return candidateQuery; },
      mutate: async () => ({}),
    }, { load: async () => current }, notification, () => undefined);

    const draining = presenter.drain();
    await queryStarted;
    current = { messageBanners: true, mirroredBanners: true, preview: "hidden", showSource: false };
    resolveCandidates([candidate()]);
    await draining;

    expect(notification.shown).toEqual([expect.objectContaining({ title: "New message", body: "" })]);
  });

  it("leaves queried candidates queued when the final preference load fails", async () => {
    const notification = adapter();
    const mutate = vi.fn(async () => ({}));
    const query = vi.fn(async () => [candidate()]);
    const presenter = new BannerPresenter({ query, mutate }, { load: async () => { throw new Error("settings unavailable"); } }, notification, () => undefined);

    await expect(presenter.drain()).rejects.toThrow("settings unavailable");

    expect(query).toHaveBeenCalledWith("pending_banner_candidates", { limit: 20 });
    expect(notification.shown).toEqual([]);
    expect(mutate).not.toHaveBeenCalled();
  });

  it("keeps hidden core previews hidden after legacy settings are retired", async () => {
    const query = vi.fn()
      .mockResolvedValueOnce([candidate()])
      .mockResolvedValueOnce({ messageBanners: true, mirroredBanners: true, preview: "hidden" });
    const notification = adapter();
    const presenter = new BannerPresenter({ query, mutate: async () => ({}) }, new CoreNotificationSettings({ query }), notification, () => undefined);
    await presenter.drain();
    expect(notification.shown).toEqual([expect.objectContaining({ title: "New message", body: "", tag: "peppy-candidate-1" })]);
  });

  it("shows only a structured sender with an empty body for source-only previews", async () => {
    const notification = adapter();
    const presenter = new BannerPresenter(
      { query: async () => [candidate({
        title: "Private title",
        body: "Private body",
        sourceName: "15551234567",
        appIconPngBase64: "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0",
      } as Partial<BannerCandidate>)] , mutate: async () => ({}) },
      { ...preferences, load: async () => ({ messageBanners: true, mirroredBanners: true, preview: "hidden", showSource: true } as NotificationPreferences) },
      notification,
      () => undefined,
    );

    await presenter.drain();

    expect(notification.shown).toEqual([expect.objectContaining({ title: "15551234567", body: "", tag: "peppy-candidate-1" })]);
  });

  it("passes the selected content icon to browser notifications", () => {
    const shown = vi.fn();
    class BrowserNotification {
      public static permission: NotificationPermission = "granted";
      public constructor(title: string, options: NotificationOptions) { shown(title, options); }
    }
    const notification = new BrowserNotificationAdapter(BrowserNotification as unknown as typeof Notification);

    notification.show({ title: "Source", body: "", tag: "peppy-candidate-1", icon: "data:image/png;base64,icon" } as never);

    expect(shown).toHaveBeenCalledWith("Source", { body: "", tag: "peppy-candidate-1", icon: "data:image/png;base64,icon" });
  });

  it("passes requireInteraction only from an enabled notification context", async () => {
    const enabled = adapter();
    const enabledPresenter = new BannerPresenter({ query: async () => [candidate()], mutate: async () => ({}) }, preferences, enabled, () => undefined);
    enabledPresenter.setContext({ view: "settings", requireInteraction: true });
    await enabledPresenter.drain();
    expect(enabled.shown).toEqual([expect.objectContaining({ requireInteraction: true })]);

    const disabled = adapter();
    const disabledPresenter = new BannerPresenter({ query: async () => [candidate()], mutate: async () => ({}) }, preferences, disabled, () => undefined);
    disabledPresenter.setContext({ view: "settings", requireInteraction: false });
    await disabledPresenter.drain();
    expect(disabled.shown[0]).not.toHaveProperty("requireInteraction");
  });

  it("omits message artwork for fully hidden direct and forwarded banners", async () => {
    const tiers = [
      { preferences: { preview: "full" as const, showSource: false }, title: "Private title", body: "Private body" },
      { preferences: { preview: "hidden" as const, showSource: true }, title: "15551234567", body: "" },
      { preferences: { preview: "hidden" as const, showSource: false }, title: "New message", body: "" },
    ];
    const message = candidate({ sourceName: "15551234567" });
    for (const tier of tiers) {
      const direct = adapter();
      const directPresenter = new BannerPresenter({ query: async () => [message], mutate: async () => ({}) }, { ...preferences, load: async () => ({ messageBanners: true, mirroredBanners: true, ...tier.preferences }) }, direct, () => undefined);
      await directPresenter.drain();
      expect(direct.shown).toEqual([{ title: tier.title, body: tier.body, tag: "peppy-candidate-1", ...(tier.preferences.preview === "full" || tier.preferences.showSource ? { icon: MESSAGE_GLYPH } : {}) }]);

      const forwarded: BannerCandidate[] = [];
      const forwardedPresenter = new BannerPresenter({ query: async () => [message], mutate: async () => ({}) }, { ...preferences, load: async () => ({ messageBanners: true, mirroredBanners: true, ...tier.preferences }) }, adapter({ available: false, permission: "denied" }), () => value => forwarded.push(value));
      forwardedPresenter.setContext({ view: "settings", notificationPermission: "granted" });
      await forwardedPresenter.drain();
      expect(forwarded).toEqual([{ id: "candidate-1", kind: "message", conversationId: "conversation-1", title: tier.title, body: tier.body, createdAt: message.createdAt, ...(tier.preferences.preview === "full" || tier.preferences.showSource ? { icon: MESSAGE_GLYPH } : {}) }]);
    }
  });

  it("selects only safe direct and forwarded fields for every preview tier", async () => {
    const notification = adapter();
    const delivered: BannerCandidate[] = [];
    const mirrored = candidate({ kind: "notification", title: "Private title", body: "Private body", sourceName: "Trusted app", appIconPngBase64: VALID_APP_ICON } as Partial<BannerCandidate>);
    const presenter = new BannerPresenter({ query: async () => [mirrored], mutate: async () => ({}) }, { ...preferences, load: async () => ({ messageBanners: true, mirroredBanners: true, preview: "full" }) }, notification, () => value => delivered.push(value));

    await presenter.drain();
    expect(notification.shown).toEqual([expect.objectContaining({ title: "Private title", body: "Private body", icon: `data:image/png;base64,${VALID_APP_ICON}` })]);

    const sourceOnly = new BannerPresenter({ query: async () => [mirrored], mutate: async () => ({}) }, { ...preferences, load: async () => ({ messageBanners: true, mirroredBanners: true, preview: "hidden", showSource: true }) }, adapter({ available: false, permission: "denied" }), () => value => delivered.push(value));
    sourceOnly.setContext({ view: "settings", notificationPermission: "granted" });
    await sourceOnly.drain();
    expect(delivered).toEqual([expect.objectContaining({ title: "Trusted app", body: "", icon: `data:image/png;base64,${VALID_APP_ICON}` })]);
    expect(delivered[0]).not.toHaveProperty("sourceName");
    expect(delivered[0]).not.toHaveProperty("appIconPngBase64");

    const hidden = adapter();
    const generic = new BannerPresenter({ query: async () => [mirrored], mutate: async () => ({}) }, { ...preferences, load: async () => ({ messageBanners: true, mirroredBanners: true, preview: "hidden", showSource: false }) }, hidden, () => undefined);
    await generic.drain();
    expect(hidden.shown).toEqual([expect.objectContaining({ title: "New notification", body: "" })]);
    expect(hidden.shown[0]).not.toHaveProperty("icon");
  });

  it("omits the icon for malformed, truncated, oversized, and wrong-sized PNG data", async () => {
    const invalidIcons = [
      "iVBORw0KGgoAAAANSUhEUgAAACAAAAAg",
      VALID_APP_ICON.repeat(100),
      `${VALID_APP_ICON.slice(0, -2)}${"A".repeat(10_800)}`,
      VALID_APP_ICON.replace("ACAAAAAg", "ABAAAAAg"),
    ];
    for (const appIconPngBase64 of invalidIcons) {
      const notification = adapter();
      const presenter = new BannerPresenter({ query: async () => [candidate({ kind: "notification", appIconPngBase64 } as Partial<BannerCandidate>)], mutate: async () => ({}) }, preferences, notification, () => undefined);
      await presenter.drain();
      expect(notification.shown[0]).not.toHaveProperty("icon");
    }
  });

  it("enforces the decoded PNG byte boundary independently of the encoded boundary", async () => {
    expect(Uint8Array.from(atob(PNG_AT_DECODED_BOUNDARY), character => character.charCodeAt(0))).toHaveLength(8192);
    expect(PNG_AT_DECODED_BOUNDARY).toHaveLength(10_924);
    expect(Uint8Array.from(atob(PNG_OVER_DECODED_BOUNDARY), character => character.charCodeAt(0))).toHaveLength(8193);
    expect(PNG_OVER_DECODED_BOUNDARY).toHaveLength(10_924);

    const accepted = adapter();
    const acceptedPresenter = new BannerPresenter({ query: async () => [candidate({ kind: "notification", appIconPngBase64: PNG_AT_DECODED_BOUNDARY } as Partial<BannerCandidate>)], mutate: async () => ({}) }, preferences, accepted, () => undefined);
    await acceptedPresenter.drain();
    expect(accepted.shown[0].icon).toBe(`data:image/png;base64,${PNG_AT_DECODED_BOUNDARY}`);

    const rejected = adapter();
    const rejectedPresenter = new BannerPresenter({ query: async () => [candidate({ kind: "notification", appIconPngBase64: PNG_OVER_DECODED_BOUNDARY } as Partial<BannerCandidate>)], mutate: async () => ({}) }, preferences, rejected, () => undefined);
    await rejectedPresenter.drain();
    expect(rejected.shown[0]).not.toHaveProperty("icon");
  });

  it("rejects unknown critical PNG chunks and invalid IDAT ordering", async () => {
    const invalidIcons = [
      png(["IHDR", pngHeader()], ["TEST", new Uint8Array()], ["IDAT", new Uint8Array()], ["IEND", new Uint8Array()]),
      png(["IHDR", pngHeader()], ["IDAT", new Uint8Array()], ["tEST", new Uint8Array()], ["IDAT", new Uint8Array()], ["IEND", new Uint8Array()]),
    ];
    for (const appIconPngBase64 of invalidIcons) {
      const notification = adapter();
      const presenter = new BannerPresenter({ query: async () => [candidate({ kind: "notification", appIconPngBase64 } as Partial<BannerCandidate>)], mutate: async () => ({}) }, preferences, notification, () => undefined);

      await presenter.drain();

      expect(notification.shown[0]).not.toHaveProperty("icon");
    }
  });

  it("rejects structurally invalid indexed and grayscale palettes", async () => {
    const invalidIcons = [
      png(["IHDR", pngHeader(2, 3)], ["IDAT", new Uint8Array()], ["IEND", new Uint8Array()]),
      png(["IHDR", pngHeader(1, 3)], ["PLTE", new Uint8Array(9)], ["IDAT", new Uint8Array()], ["IEND", new Uint8Array()]),
      png(["IHDR", pngHeader(8, 0)], ["PLTE", new Uint8Array(3)], ["IDAT", new Uint8Array()], ["IEND", new Uint8Array()]),
      png(["IHDR", pngHeader(8, 3)], ["PLTE", new Uint8Array(257 * 3)], ["IDAT", new Uint8Array()], ["IEND", new Uint8Array()]),
    ];
    for (const appIconPngBase64 of invalidIcons) {
      const notification = adapter();
      const presenter = new BannerPresenter({ query: async () => [candidate({ kind: "notification", appIconPngBase64 } as Partial<BannerCandidate>)], mutate: async () => ({}) }, preferences, notification, () => undefined);

      await presenter.drain();

      expect(notification.shown[0]).not.toHaveProperty("icon");
    }
  });

  it("uses the exact redacted contract in direct and forwarded delivery for every preview tier", async () => {
    const mirrored = candidate({
      id: CORE_NOTIFICATION_ID,
      kind: "notification",
      conversationId: "conversation-1",
      notificationTarget: CORE_NOTIFICATION_TARGET,
      title: "Private title",
      body: "Private body",
      sourceName: "Trusted app",
      appIconPngBase64: VALID_APP_ICON,
    } as Partial<BannerCandidate>);
    const tiers = [
      { preferences: { preview: "full" as const, showSource: false }, title: "Private title", body: "Private body", icon: `data:image/png;base64,${VALID_APP_ICON}` },
      { preferences: { preview: "hidden" as const, showSource: true }, title: "Trusted app", body: "", icon: `data:image/png;base64,${VALID_APP_ICON}` },
      { preferences: { preview: "hidden" as const, showSource: false }, title: "New notification", body: "" },
    ];
    for (const tier of tiers) {
      const direct = adapter();
      const directPresenter = new BannerPresenter({ query: async () => [mirrored], mutate: async () => ({}) }, { ...preferences, load: async () => ({ messageBanners: true, mirroredBanners: true, ...tier.preferences }) }, direct, () => undefined);
      await directPresenter.drain();
      expect(direct.shown).toEqual([{ title: tier.title, body: tier.body, tag: `peppy-${CORE_NOTIFICATION_ID}`, ...(tier.icon ? { icon: tier.icon } : {}) }]);

      const forwarded: BannerCandidate[] = [];
      const mutate = vi.fn(async () => ({}));
      const forwardedPresenter = new BannerPresenter({ query: async () => [mirrored], mutate }, { ...preferences, load: async () => ({ messageBanners: true, mirroredBanners: true, ...tier.preferences }) }, adapter({ available: false, permission: "denied" }), () => value => forwarded.push(value));
      forwardedPresenter.setContext({ view: "settings", notificationPermission: "granted" });
      await forwardedPresenter.drain();
      expect(forwarded).toEqual([{
        id: CORE_NOTIFICATION_ID,
        kind: "notification",
        conversationId: "conversation-1",
        notificationTarget: CORE_NOTIFICATION_TARGET,
        title: tier.title,
        body: tier.body,
        createdAt: mirrored.createdAt,
        ...(tier.icon ? { icon: tier.icon } : {}),
      }]);
      await forwardedPresenter.acknowledgeDisplayed([CORE_NOTIFICATION_ID]);
      expect(mutate).toHaveBeenCalledWith("ack_banner_candidates", { ids: [CORE_NOTIFICATION_ID] });
    }
  });

  it("matches bounded notification IDs to their structured target tuple", async () => {
    const reorderedId = '{"lifetime":"00000000-0000-4000-8000-000000000002","notificationKey":"notification-1","sourceDeviceId":"00000000-0000-4000-8000-000000000001"}';
    const boundaryKey = "x".repeat(512);
    const boundaryTarget = { ...CORE_NOTIFICATION_TARGET, notificationKey: boundaryKey };
    const boundaryId = JSON.stringify(boundaryTarget);
    const oversizedKey = "x".repeat(513);
    const values = [
      candidate({ id: reorderedId, kind: "notification", notificationTarget: CORE_NOTIFICATION_TARGET } as Partial<BannerCandidate>),
      candidate({ id: boundaryId, kind: "notification", notificationTarget: boundaryTarget } as Partial<BannerCandidate>),
      candidate({ id: `${CORE_NOTIFICATION_ID}broken`, kind: "notification", notificationTarget: CORE_NOTIFICATION_TARGET } as Partial<BannerCandidate>),
      candidate({ id: CORE_NOTIFICATION_ID, kind: "notification", notificationTarget: { ...CORE_NOTIFICATION_TARGET, notificationKey: "other" } } as Partial<BannerCandidate>),
      candidate({ id: JSON.stringify({ ...CORE_NOTIFICATION_TARGET, notificationKey: oversizedKey }), kind: "notification", notificationTarget: { ...CORE_NOTIFICATION_TARGET, notificationKey: oversizedKey } } as Partial<BannerCandidate>),
      candidate({ id: JSON.stringify({ ...CORE_NOTIFICATION_TARGET, extra: true }), kind: "notification", notificationTarget: CORE_NOTIFICATION_TARGET } as Partial<BannerCandidate>),
    ];
    const notification = adapter();
    const mutate = vi.fn(async () => ({}));
    const presenter = new BannerPresenter({ query: async () => values, mutate }, preferences, notification, () => undefined);

    await presenter.drain();

    expect(notification.shown).toEqual([
      expect.objectContaining({ tag: `peppy-${reorderedId}` }),
      expect.objectContaining({ tag: `peppy-${boundaryId}` }),
    ]);
    expect(mutate).toHaveBeenCalledWith("ack_banner_candidates", { ids: [reorderedId, boundaryId] });
  });

  it("keeps the captured app icon when source-only text falls back to the generic literal", async () => {
    for (const sourceName of [undefined, "   "]) {
      const mirrored = candidate({
        id: CORE_NOTIFICATION_ID,
        kind: "notification",
        notificationTarget: CORE_NOTIFICATION_TARGET,
        sourceName,
        appIconPngBase64: VALID_APP_ICON,
      } as Partial<BannerCandidate>);
      const sourceOnlyPreferences = { ...preferences, load: async () => ({ messageBanners: true, mirroredBanners: true, preview: "hidden" as const, showSource: true }) };
      const direct = adapter();
      const directPresenter = new BannerPresenter({ query: async () => [mirrored], mutate: async () => ({}) }, sourceOnlyPreferences, direct, () => undefined);

      await directPresenter.drain();

      expect(direct.shown).toEqual([expect.objectContaining({ title: "New notification", body: "", icon: `data:image/png;base64,${VALID_APP_ICON}` })]);

      const forwarded: BannerCandidate[] = [];
      const forwardedPresenter = new BannerPresenter({ query: async () => [mirrored], mutate: async () => ({}) }, sourceOnlyPreferences, adapter({ available: false, permission: "denied" }), () => value => forwarded.push(value));
      forwardedPresenter.setContext({ view: "settings", notificationPermission: "granted" });
      await forwardedPresenter.drain();

      expect(forwarded).toEqual([expect.objectContaining({ title: "New notification", body: "", icon: `data:image/png;base64,${VALID_APP_ICON}` })]);
    }
  });

  it("uses the generic literal when source-only data is missing or blank", async () => {
    for (const sourceName of [undefined, "   "]) {
      const notification = adapter();
      const presenter = new BannerPresenter({ query: async () => [candidate({ title: "Private title", body: "Private body", sourceName } as Partial<BannerCandidate>)], mutate: async () => ({}) }, { ...preferences, load: async () => ({ messageBanners: true, mirroredBanners: true, preview: "hidden", showSource: true }) }, notification, () => undefined);
      await presenter.drain();
      expect(notification.shown).toEqual([expect.objectContaining({ title: "New message", body: "" })]);
      expect(notification.shown[0].title).not.toBe("Private title");
    }
  });

  it("rejects malformed settings values", () => {
    expect(notificationPreferences({ messageBanners: true, muteWithHeads: false })).toMatchObject({ muteWithHeads: false });
    expect(() => notificationPreferences({ messageBanners: "yes" })).toThrow("Invalid notification preferences");
    expect(() => notificationPreferences({ muteWithHeads: "yes" })).toThrow("Invalid notification preferences");
    expect(() => notificationPreferences({ plaintext: "message body" })).toThrow("Invalid notification preferences");
  });

  it("posts valid core candidates before acknowledging them", async () => {
    const calls: string[] = [];
    const notification = adapter();
    const presenter = new BannerPresenter({
      query: async () => [{ ...candidate(), notificationTarget: null }],
      mutate: async command => { calls.push(command); return {}; },
    }, preferences, notification, () => undefined);
    await presenter.drain();
    expect(notification.shown).toEqual([expect.objectContaining({ title: "Private title", body: "Private body", tag: "peppy-candidate-1" })]);
    expect(calls).toEqual(["ack_banner_candidates"]);
  });

  it("acknowledges denied browser notifications as an intentional policy drop", async () => {
    const calls: string[] = [];
    const presenter = new BannerPresenter({
      query: async () => [candidate()],
      mutate: async command => { calls.push(command); return {}; },
    }, preferences, adapter({ available: true, permission: "denied" }), () => undefined);
    await presenter.drain();
    expect(calls).toEqual(["ack_banner_candidates"]);
  });

  it("redacts hidden previews and waits for the renderer display acknowledgement", async () => {
    const delivered: BannerCandidate[] = [];
    const calls: unknown[] = [];
    const presenter = new BannerPresenter({
      query: async () => [candidate()],
      mutate: async (_command, args) => { calls.push(args); return {}; },
    }, { ...preferences, load: async () => ({ messageBanners: true, mirroredBanners: true, preview: "hidden" }) }, adapter({ available: false, permission: "denied" }), () => value => delivered.push(value));
    presenter.setContext({ view: "settings", notificationPermission: "granted" });
    await presenter.drain();
    expect(delivered).toEqual([expect.objectContaining({ title: "New message", body: "" })]);
    expect(calls).toEqual([]);
    await presenter.acknowledgeDisplayed(["candidate-1"]);
    expect(calls).toEqual([{ ids: ["candidate-1"] }]);
  });

  it("keeps a displayed candidate awaiting a durable acknowledgement and retries only the acknowledgement", async () => {
    const delivered: BannerCandidate[] = [];
    const mutate = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValue({});
    const presenter = new BannerPresenter({
      query: async () => [candidate()],
      mutate,
    }, preferences, adapter({ available: false, permission: "denied" }), () => value => delivered.push(value));
    presenter.setContext({ view: "settings", notificationPermission: "granted" });

    await presenter.drain();
    await expect(presenter.acknowledgeDisplayed(["candidate-1"])).rejects.toThrow("offline");
    await presenter.drain();

    expect(delivered).toHaveLength(1);
    expect(mutate).toHaveBeenCalledTimes(2);
  });

  it("suppresses visible conversations without a browser emission", async () => {
    const notification = adapter();
    const calls: unknown[] = [];
    const presenter = new BannerPresenter({
      query: async () => [candidate()],
      mutate: async (_command, args) => { calls.push(args); return {}; },
    }, preferences, notification, () => undefined);
    presenter.setContext({ view: "conversations", conversationId: "conversation-1", focused: true });
    await presenter.drain();
    expect(notification.shown).toEqual([]);
    expect(calls).toEqual([{ ids: ["candidate-1"] }]);
  });

  it("emits each fallback candidate once until it is acknowledged or cleared", async () => {
    const delivered: BannerCandidate[] = [];
    const presenter = new BannerPresenter({ query: async () => [candidate()], mutate: async () => ({}) }, preferences, adapter({ available: false, permission: "denied" }), () => value => delivered.push(value));
    presenter.setContext({ view: "settings", notificationPermission: "granted" });
    await presenter.drain();
    await presenter.drain();
    expect(delivered).toHaveLength(1);
    presenter.clear();
    await presenter.acknowledgeDisplayed(["candidate-1"]);
  });

  it("abandons a queried candidate when cleared before settings resolve", async () => {
    let resolveSettings!: (value: NotificationPreferences) => void;
    const settings = new Promise<NotificationPreferences>(resolve => { resolveSettings = resolve; });
    const notification = adapter();
    const mutate = vi.fn(async () => ({}));
    const presenter = new BannerPresenter({ query: async () => [candidate()], mutate }, { load: async () => settings }, notification, () => undefined);

    const draining = presenter.drain();
    await Promise.resolve();
    presenter.clear();
    resolveSettings({ messageBanners: true, mirroredBanners: true, preview: "full" });
    await draining;

    expect(notification.shown).toEqual([]);
    expect(mutate).not.toHaveBeenCalled();
  });

  it("does not load settings after clear while candidate query is pending", async () => {
    let resolveCandidates!: (value: BannerCandidate[]) => void;
    const candidates = new Promise<BannerCandidate[]>(resolve => { resolveCandidates = resolve; });
    const load = vi.fn(async () => ({ messageBanners: true, mirroredBanners: true, preview: "full" as const }));
    const presenter = new BannerPresenter({ query: async () => candidates, mutate: async () => ({}) }, { load }, adapter(), () => undefined);

    const draining = presenter.drain();
    presenter.clear();
    resolveCandidates([candidate()]);
    await draining;

    expect(load).not.toHaveBeenCalled();
  });

  it("does not acknowledge direct or forwarded banners cleared during delivery", async () => {
    const directMutate = vi.fn(async () => ({}));
    let directPresenter!: BannerPresenter;
    const directHandle = { close: vi.fn(), release: vi.fn() };
    directPresenter = new BannerPresenter({ query: async () => [candidate()], mutate: directMutate }, preferences, adapter({ show: () => { directPresenter.clear(); return directHandle; } }), () => undefined);
    await directPresenter.drain();

    const forwardedMutate = vi.fn(async () => ({}));
    let forwardedPresenter!: BannerPresenter;
    forwardedPresenter = new BannerPresenter({ query: async () => [candidate()], mutate: forwardedMutate }, preferences, adapter({ available: false, permission: "denied" }), () => () => forwardedPresenter.clear());
    forwardedPresenter.setContext({ view: "settings", notificationPermission: "granted" });
    await forwardedPresenter.drain();

    expect(directMutate).not.toHaveBeenCalled();
    expect(directHandle.close).toHaveBeenCalledOnce();
    expect(directHandle.release).toHaveBeenCalledOnce();
    expect(forwardedMutate).not.toHaveBeenCalled();
  });

  it("releases direct notification handles after closing them", async () => {
    const close = vi.fn();
    const release = vi.fn();
    const presenter = new BannerPresenter({ query: async () => [candidate()], mutate: async () => ({}) }, preferences, adapter({ show: () => ({ close, release }) }), () => undefined);

    await presenter.drain();

    presenter.clear();

    expect(close).toHaveBeenCalledOnce();
    expect(release).toHaveBeenCalledOnce();
  });

  it("releases a dismissed browser notification and only closes remaining live handles", async () => {
    const notifications: TestNotification[] = [];
    class TestNotification extends EventTarget {
      public static permission: NotificationPermission = "granted";
      public close = vi.fn(() => this.dispatchEvent(new Event("close")));
      public constructor() { super(); notifications.push(this); }
      public dismiss(): void { this.dispatchEvent(new Event("close")); }
    }
    const presenter = new BannerPresenter({
      query: async () => [candidate({ id: "first" }), candidate({ id: "second" })],
      mutate: async () => ({}),
    }, preferences, new BrowserNotificationAdapter(TestNotification as unknown as typeof Notification), () => undefined);

    await presenter.drain();
    notifications[0].dismiss();
    presenter.clear();

    expect(notifications[0].close).not.toHaveBeenCalled();
    expect(notifications[1].close).toHaveBeenCalledOnce();
  });
});
