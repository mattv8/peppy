import { renderHook, act } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { persistShowReactions, REACTIONS_PREFERENCE_KEY, readShowReactions, useShowReactions } from "./preference";

describe("reaction preference", () => {
  beforeEach(() => {
    const values = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      clear: () => values.clear(),
    });
  });

  it("defaults on for missing or invalid storage", () => {
    expect(readShowReactions()).toBe(true);
    localStorage.setItem(REACTIONS_PREFERENCE_KEY, '{"show":"no"}');
    expect(readShowReactions()).toBe(true);
  });

  it("persists updates and synchronizes storage events", () => {
    const { result } = renderHook(() => useShowReactions());
    act(() => result.current[1](false));
    expect(result.current[0]).toBe(false);
    expect(readShowReactions()).toBe(false);
    persistShowReactions(true);
    act(() => window.dispatchEvent(new StorageEvent("storage", { key: REACTIONS_PREFERENCE_KEY })));
    expect(result.current[0]).toBe(true);
  });

  it("restores the default after another tab clears storage", () => {
    persistShowReactions(false);
    const { result } = renderHook(() => useShowReactions());
    expect(result.current[0]).toBe(false);
    localStorage.clear();
    act(() => window.dispatchEvent(new StorageEvent("storage", { key: null })));
    expect(result.current[0]).toBe(true);
  });
});
