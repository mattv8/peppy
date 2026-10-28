import "fake-indexeddb/auto";
import { describe, expect, it, vi } from "vitest";
import { BannerPresenter, CoreNotificationSettings, IndexedDbNotificationSettings, notificationPreferences, type BannerCandidate, type NotificationAdapter, type NotificationPreferences, type NotificationSettings } from "./presentation.js";

const preferences: NotificationSettings = {
  load: async () => ({ messageBanners: true, mirroredBanners: true, preview: "full" }),
};

function candidate(overrides: Partial<BannerCandidate> = {}): BannerCandidate {
  return { id: "candidate-1", kind: "message", conversationId: "conversation-1", title: "Private title", body: "Private body", createdAt: Date.now(), ...overrides };
}

function adapter(overrides: Partial<NotificationAdapter> = {}): NotificationAdapter & { shown: Array<{ title: string; body: string; tag: string }> } {
  const result = {
    available: true,
    permission: "granted" as const,
    shown: [] as Array<{ title: string; body: string; tag: string }>,
    show(notification: { title: string; body: string; tag: string }) { result.shown.push(notification); },
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
    await expect(new IndexedDbNotificationSettings(name).claimForVault("00000000-0000-4000-8000-000000000001")).resolves.toEqual({ messageBanners: true, mirroredBanners: false, preview: "full", muteWithHeads: true });
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
      .mockResolvedValueOnce({ messageBanners: true, mirroredBanners: true, preview: "full" })
      .mockResolvedValueOnce([candidate({ id: "first" })])
      .mockResolvedValueOnce({ messageBanners: false, mirroredBanners: true, preview: "full" })
      .mockResolvedValueOnce([candidate({ id: "second" })]);
    const notification = adapter();
    const presenter = new BannerPresenter({ query, mutate: async () => ({}) }, new CoreNotificationSettings({ query }), notification, () => undefined);
    await presenter.drain();
    await presenter.drain();
    expect(notification.shown).toEqual([{ title: "Private title", body: "Private body", tag: "peppy-first" }]);
  });

  it("keeps hidden core previews hidden after legacy settings are retired", async () => {
    const query = vi.fn()
      .mockResolvedValueOnce({ messageBanners: true, mirroredBanners: true, preview: "hidden" })
      .mockResolvedValueOnce([candidate()]);
    const notification = adapter();
    const presenter = new BannerPresenter({ query, mutate: async () => ({}) }, new CoreNotificationSettings({ query }), notification, () => undefined);
    await presenter.drain();
    expect(notification.shown).toEqual([{ title: "New message", body: "Open Peppy to view it.", tag: "peppy-candidate-1" }]);
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
    expect(notification.shown).toEqual([{ title: "Private title", body: "Private body", tag: "peppy-candidate-1" }]);
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
    expect(delivered).toEqual([expect.objectContaining({ title: "New message", body: "Open Peppy to view it." })]);
    expect(calls).toEqual([]);
    await presenter.acknowledgeDisplayed(["candidate-1"]);
    expect(calls).toEqual([{ ids: ["candidate-1"] }]);
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
});
