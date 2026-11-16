import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  NOTIFICATIONS_PREFERENCE_ITEM,
  persistNotificationArrangement,
  readNotificationArrangement,
  useNotificationArrangement,
} from "./notificationViewPreference";

describe("notification view preference", () => {
  beforeEach(() => {
    const values = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      clear: () => values.clear(),
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("defaults to time arrangement for missing, invalid, or unknown storage", () => {
    expect(readNotificationArrangement()).toBe("time");
    localStorage.setItem(NOTIFICATIONS_PREFERENCE_ITEM, "not json");
    expect(readNotificationArrangement()).toBe("time");
    localStorage.setItem(NOTIFICATIONS_PREFERENCE_ITEM, '{"arrangement":"sideways"}');
    expect(readNotificationArrangement()).toBe("time");
  });

  it("reads and writes the arrangement under the versioned key", () => {
    persistNotificationArrangement("app");
    expect(localStorage.getItem(NOTIFICATIONS_PREFERENCE_ITEM)).toBe('{"arrangement":"app"}');
    expect(readNotificationArrangement()).toBe("app");
  });

  it("persists updates and follows storage events", () => {
    const { result } = renderHook(() => useNotificationArrangement());
    act(() => result.current[1]("app"));
    expect(result.current[0]).toBe("app");
    expect(readNotificationArrangement()).toBe("app");
    persistNotificationArrangement("time");
    act(() => window.dispatchEvent(new StorageEvent("storage", { key: NOTIFICATIONS_PREFERENCE_ITEM })));
    expect(result.current[0]).toBe("time");
  });

  it("restores the default after another tab clears storage", () => {
    persistNotificationArrangement("app");
    const { result } = renderHook(() => useNotificationArrangement());
    expect(result.current[0]).toBe("app");
    localStorage.clear();
    act(() => window.dispatchEvent(new StorageEvent("storage", { key: null })));
    expect(result.current[0]).toBe("time");
  });
});
