import "@testing-library/jest-dom/vitest";
import { act, renderHook } from "@testing-library/react";
import type { MirroredNotification, NotificationDismissResult, NotificationTarget } from "@peppy/desktop-ui/bridge-contracts";
import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { notificationKey } from "./notificationSections";
import { useNotificationDismissals, type NotificationDismissalOptions } from "./useNotificationDismissals";

const row = (id: string, overrides: Partial<MirroredNotification> = {}): MirroredNotification => ({
  target: { sourceDeviceId: "phone", notificationKey: id, lifetime: "1" },
  packageName: "com.example.chat",
  appName: "Chat",
  title: `Title ${id}`,
  text: "body",
  postedAt: 1,
  dismissible: true,
  seen: false,
  dismissalPending: false,
  ...overrides,
});

const a = row("a");
const b = row("b");
const keyA = notificationKey(a.target);
const keyB = notificationKey(b.target);
const recorded = (target: NotificationTarget): NotificationDismissResult => ({ target, outcome: "recorded" });
const rejectWith = (code: string) => Promise.reject(Object.assign(new Error(code), { code }));

type DismissMock = Mock<(targets: NotificationTarget[]) => Promise<NotificationDismissResult[]>>;
type Bridge = { dismiss_notifications: DismissMock };

function setup(overrides: Partial<NotificationDismissalOptions> = {}) {
  const bridge: Bridge = { dismiss_notifications: vi.fn(async (targets: NotificationTarget[]) => targets.map(recorded)) as DismissMock };
  const refresh = vi.fn(async () => {});
  const announce = vi.fn();
  const onBulkFailure = vi.fn();
  const label = (item: MirroredNotification) => ({ title: item.title, appName: item.appName });
  const initial: NotificationDismissalOptions = { bridge, notifications: [a, b], refresh, writesPaused: false, announce, onBulkFailure, label, ...overrides };
  const hook = renderHook((props: NotificationDismissalOptions) => useNotificationDismissals(props), { initialProps: initial });
  const rerenderWith = (patch: Partial<NotificationDismissalOptions>) => hook.rerender({ ...initial, ...patch });
  return { ...hook, bridge, refresh, announce, onBulkFailure, rerenderWith };
}

const settle = () => act(async () => {
  await vi.advanceTimersByTimeAsync(0);
});

const advance = (ms: number) => act(async () => {
  await vi.advanceTimersByTimeAsync(ms);
});

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("useNotificationDismissals", () => {
  it("hides the row immediately, before the bridge answers", () => {
    const { result, bridge } = setup();
    bridge.dismiss_notifications.mockImplementationOnce(() => new Promise(() => {}));
    act(() => result.current.dismiss([a.target], { single: true }));
    expect(result.current.hiddenKeys.has(keyA)).toBe(true);
    expect(bridge.dismiss_notifications).toHaveBeenCalledWith([a.target]);
  });

  it("refreshes the snapshot after a successful call and announces the dismissal", async () => {
    const { result, refresh, announce } = setup();
    act(() => result.current.dismiss([a.target], { single: true }));
    await settle();
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(announce).toHaveBeenCalledWith("Dismissed Title a from Chat.");
    expect(result.current.hiddenKeys.has(keyA)).toBe(true);
  });

  it("announces a refused row and shows it again", async () => {
    const { result, bridge, announce } = setup();
    bridge.dismiss_notifications.mockResolvedValueOnce([{ target: a.target, outcome: "notDismissible" }]);
    act(() => result.current.dismiss([a.target], { single: true }));
    await settle();
    expect(announce).toHaveBeenLastCalledWith("Title a can't be dismissed. It's back in the list.");
    expect(result.current.hiddenKeys.has(keyA)).toBe(false);
    expect(result.current.failures.has(keyA)).toBe(false);
  });

  it("retries update-in-progress three times with the contract delays, then fails the row", async () => {
    const { result, bridge } = setup();
    bridge.dismiss_notifications.mockImplementation(() => rejectWith("update-in-progress"));
    act(() => result.current.dismiss([a.target], { single: true }));
    await settle();
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(1);
    expect(result.current.hiddenKeys.has(keyA)).toBe(true);

    await advance(1999);
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(1);
    await advance(1);
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(2);
    await advance(8000);
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(3);
    await advance(20000);
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(4);

    expect(result.current.hiddenKeys.has(keyA)).toBe(false);
    expect(result.current.failures.get(keyA)).toEqual({ message: "Peppy is updating your messages.", canRetry: true });
  });

  it("fails update-busy without a retry", async () => {
    const { result, bridge } = setup();
    bridge.dismiss_notifications.mockImplementationOnce(() => rejectWith("update-busy"));
    act(() => result.current.dismiss([a.target], { single: true }));
    await settle();
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(1);
    expect(result.current.failures.get(keyA)?.canRetry).toBe(false);
    expect(result.current.failures.get(keyA)?.message).toContain("about to restart");
  });

  it("fails a locked vault with a retry and no automatic attempt", async () => {
    const { result, bridge } = setup();
    bridge.dismiss_notifications.mockImplementationOnce(() => rejectWith("locked"));
    act(() => result.current.dismiss([a.target], { single: true }));
    await settle();
    await advance(60000);
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(1);
    expect(result.current.failures.get(keyA)).toEqual({ message: "Peppy is locked. Unlock it, then try again.", canRetry: true });
  });

  it("reports a bulk failure without per-row errors and makes the rows visible again", async () => {
    const { result, bridge, onBulkFailure } = setup();
    bridge.dismiss_notifications.mockImplementationOnce(() => rejectWith("locked"));
    act(() => result.current.dismiss([a.target, b.target], { single: false }));
    await settle();
    expect(result.current.hiddenKeys.has(keyA)).toBe(false);
    expect(result.current.hiddenKeys.has(keyB)).toBe(false);
    expect(result.current.failures.size).toBe(0);
    expect(onBulkFailure).toHaveBeenLastCalledWith({
      message: "Couldn't dismiss 2 notifications. Peppy is locked. Unlock it, then try again.",
      keys: [keyA, keyB],
      action: expect.objectContaining({ label: "Try again" }),
    });
  });

  it("Try again from a bulk notice re-hides the rows and clears the notice", async () => {
    const { result, bridge, onBulkFailure } = setup();
    bridge.dismiss_notifications.mockImplementationOnce(() => rejectWith("locked"));
    act(() => result.current.dismiss([a.target, b.target], { single: false }));
    await settle();
    const notice = onBulkFailure.mock.lastCall?.[0];
    act(() => notice.action.onAction());
    expect(onBulkFailure).toHaveBeenLastCalledWith(undefined);
    expect(result.current.hiddenKeys.has(keyA)).toBe(true);
    expect(result.current.hiddenKeys.has(keyB)).toBe(true);
    await settle();
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(2);
  });

  it("retry re-dismisses a failed single row", async () => {
    const { result, bridge } = setup();
    bridge.dismiss_notifications.mockImplementationOnce(() => rejectWith("locked"));
    act(() => result.current.dismiss([a.target], { single: true }));
    await settle();
    expect(result.current.failures.has(keyA)).toBe(true);
    act(() => result.current.retry(keyA));
    expect(result.current.hiddenKeys.has(keyA)).toBe(true);
    expect(result.current.failures.has(keyA)).toBe(false);
    await settle();
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(2);
    expect(result.current.hiddenKeys.has(keyA)).toBe(true);
  });

  it("cancels pending retries on unmount", async () => {
    const { result, bridge, unmount } = setup();
    bridge.dismiss_notifications.mockImplementation(() => rejectWith("update-in-progress"));
    act(() => result.current.dismiss([a.target], { single: true }));
    await settle();
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(1);
    unmount();
    await advance(60000);
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(1);
  });

  it("does nothing while writes are paused", async () => {
    const { result, bridge } = setup({ writesPaused: true });
    act(() => result.current.dismiss([a.target], { single: true }));
    await settle();
    expect(bridge.dismiss_notifications).not.toHaveBeenCalled();
    expect(result.current.hiddenKeys.size).toBe(0);
  });

  it("drops recorded entries once the snapshot no longer lists the row", async () => {
    const { result, bridge, rerenderWith } = setup();
    act(() => result.current.dismiss([a.target], { single: true }));
    await settle();
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(1);
    expect(result.current.hiddenKeys.has(keyA)).toBe(true);

    rerenderWith({ notifications: [b] });
    expect(result.current.hiddenKeys.has(keyA)).toBe(false);
  });

  it("returns true when the dismissal is sent and false while writes are paused or nothing is given", () => {
    const { result, rerenderWith } = setup();
    let accepted = false;
    act(() => { accepted = result.current.dismiss([a.target], { single: true }); });
    expect(accepted).toBe(true);
    rerenderWith({ writesPaused: true });
    act(() => { accepted = result.current.dismiss([b.target], { single: true }); });
    expect(accepted).toBe(false);
    act(() => { accepted = result.current.dismiss([], { single: false }); });
    expect(accepted).toBe(false);
  });

  it("sends only the first 100 targets, without an error, and announces that count", async () => {
    const rows = Array.from({ length: 101 }, (_, index) => row(`n${index}`));
    const { result, bridge, announce } = setup({ notifications: rows });
    act(() => result.current.dismiss(rows.map(item => item.target), { single: false }));
    await settle();
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(1);
    expect(bridge.dismiss_notifications.mock.calls[0][0]).toHaveLength(100);
    expect(bridge.dismiss_notifications.mock.calls[0][0][99]).toEqual(rows[99].target);
    expect(announce).toHaveBeenCalledWith("Dismissed 100 notifications.");
    expect(result.current.failures.size).toBe(0);
  });

  it("drops a result that arrives after the snapshot already shows the row pending", async () => {
    const { result, bridge, rerenderWith } = setup();
    bridge.dismiss_notifications.mockImplementationOnce(async () => {
      rerenderWith({ notifications: [{ ...a, dismissalPending: true }, b] });
      return [recorded(a.target)];
    });
    act(() => result.current.dismiss([a.target], { single: true }));
    await settle();
    expect(result.current.hiddenKeys.has(keyA)).toBe(true);
    rerenderWith({ notifications: [a, b] });
    expect(result.current.hiddenKeys.has(keyA)).toBe(false);
  });

  it("drops a result for a row the snapshot already removed", async () => {
    const { result, bridge, rerenderWith } = setup();
    bridge.dismiss_notifications.mockImplementationOnce(async () => {
      rerenderWith({ notifications: [b] });
      return [recorded(a.target)];
    });
    act(() => result.current.dismiss([a.target], { single: true }));
    await settle();
    rerenderWith({ notifications: [a, b] });
    expect(result.current.hiddenKeys.has(keyA)).toBe(false);
  });

  it("still ends a recorded entry on the next snapshot after a refresh rejects", async () => {
    const { result, refresh, rerenderWith } = setup();
    refresh.mockRejectedValueOnce(new Error("offline"));
    act(() => result.current.dismiss([a.target], { single: true }));
    await settle();
    expect(result.current.hiddenKeys.has(keyA)).toBe(true);
    rerenderWith({ notifications: [{ ...a, dismissalPending: true }, b] });
    rerenderWith({ notifications: [a, b] });
    expect(result.current.hiddenKeys.has(keyA)).toBe(false);
  });

  it("marks a target with no result as a failed row with Try again", async () => {
    const { result, bridge } = setup();
    bridge.dismiss_notifications.mockResolvedValueOnce([]);
    act(() => result.current.dismiss([a.target], { single: true }));
    await settle();
    expect(result.current.hiddenKeys.has(keyA)).toBe(false);
    expect(result.current.failures.get(keyA)).toEqual({ message: "Peppy couldn't save this.", canRetry: true });
    act(() => result.current.retry(keyA));
    await settle();
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(2);
  });

  it("announces a single dismissal and a refusal with the display labels", async () => {
    const { result, bridge, announce } = setup({
      label: item => ({ title: `Shown ${item.title}`, appName: "Shown app" }),
    });
    act(() => result.current.dismiss([a.target], { single: true }));
    await settle();
    expect(announce).toHaveBeenLastCalledWith("Dismissed Shown Title a from Shown app.");
    bridge.dismiss_notifications.mockResolvedValueOnce([{ target: b.target, outcome: "notDismissible" }]);
    act(() => result.current.dismiss([b.target], { single: true }));
    await settle();
    expect(announce).toHaveBeenLastCalledWith("Shown Title b can't be dismissed. It's back in the list.");
  });

  it("keeps a bulk failure notice when Try again runs while writes are paused", async () => {
    const { result, bridge, onBulkFailure, rerenderWith } = setup();
    bridge.dismiss_notifications.mockImplementationOnce(() => rejectWith("locked"));
    act(() => result.current.dismiss([a.target, b.target], { single: false }));
    await settle();
    const notice = onBulkFailure.mock.lastCall?.[0];
    expect(notice.keys).toEqual([keyA, keyB]);
    rerenderWith({ writesPaused: true });
    act(() => notice.action.onAction());
    expect(onBulkFailure).not.toHaveBeenCalledWith(undefined);
    expect(bridge.dismiss_notifications).toHaveBeenCalledTimes(1);
  });
});
