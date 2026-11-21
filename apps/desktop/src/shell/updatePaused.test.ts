import { act, renderHook } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { isWritesPaused, useDelayedWritesPaused } from "./updatePaused";

it("pauses only for an explicit true snapshot flag", () => {
  expect(isWritesPaused(undefined)).toBe(false);
  expect(isWritesPaused({})).toBe(false);
  expect(isWritesPaused({ writesPaused: false })).toBe(false);
  expect(isWritesPaused({ writesPaused: true })).toBe(true);
  expect(isWritesPaused({ writesPaused: "true" } as never)).toBe(false);
});

it("disables immediately but delays visible notices, suppressing short pauses", () => {
  vi.useFakeTimers();
  try {
    const { result, rerender } = renderHook(({ paused }) => useDelayedWritesPaused(paused), { initialProps: { paused: false } });
    rerender({ paused: true });
    expect(result.current).toEqual({ paused: true, visibleNotice: false });
    act(() => vi.advanceTimersByTime(400));
    rerender({ paused: false });
    act(() => vi.advanceTimersByTime(200));
    expect(result.current).toEqual({ paused: false, visibleNotice: false });
    rerender({ paused: true });
    act(() => vi.advanceTimersByTime(500));
    expect(result.current).toEqual({ paused: true, visibleNotice: true });
    rerender({ paused: false });
    expect(result.current.visibleNotice).toBe(false);
  } finally {
    vi.useRealTimers();
  }
});
