import "fake-indexeddb/auto";
import { describe, expect, it } from "vitest";
import { BannerPresenter, IndexedDbNotificationSettings, notificationPreferences, type BannerCandidate, type NotificationAdapter, type NotificationSettings } from "./presentation.js";

const preferences: NotificationSettings = {
  load: async () => ({ messageBanners: true, mirroredBanners: true, preview: "full" }),
  save: async () => undefined,
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

describe("Worker notification presentation settings", () => {
  it("persists only validated display flags across a Worker settings reload", async () => {
    const name = `presentation-${crypto.randomUUID()}`;
    const first = new IndexedDbNotificationSettings(name);
    await first.save({ messageBanners: true, mirroredBanners: false, preview: "full" });
    await expect(new IndexedDbNotificationSettings(name).load()).resolves.toEqual({ messageBanners: true, mirroredBanners: false, preview: "full" });
  });

  it("rejects malformed settings values", () => {
    expect(() => notificationPreferences({ messageBanners: "yes" })).toThrow("Invalid notification preferences");
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
