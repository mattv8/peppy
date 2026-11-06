import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useTypingDisplay } from "./useTypingDisplay";

it("shows only selected typing and clears on stop, expiry, and unmount", () => {
  vi.useFakeTimers(); vi.setSystemTime(0);
  const listener = vi.fn(); const unsubscribe = vi.fn();
  const bridge = { subscribe_typing: (next: typeof listener) => { listener.mockImplementation(next); return unsubscribe; } } as never;
  const { result, unmount } = renderHook(() => useTypingDisplay(bridge, "selected", true));
  act(() => listener({ conversationId: "other", state: "started", expiresAtMs: "100" }));
  expect(result.current.visible).toBe(false);
  act(() => listener({ conversationId: "selected", state: "started", expiresAtMs: "20000" }));
  expect(result.current.visible).toBe(true);
  act(() => vi.advanceTimersByTime(15000));
  expect(result.current.visible).toBe(false);
  unmount(); expect(unsubscribe).toHaveBeenCalled();
  vi.useRealTimers();
});
