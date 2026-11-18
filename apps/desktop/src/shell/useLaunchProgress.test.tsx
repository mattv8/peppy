import { act, renderHook, waitFor } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import type { LaunchProgress } from "@peppy/generated/launch-progress";
import { useLaunchProgress } from "./useLaunchProgress";
import { StrictMode } from "react";

it("subscribes first, ignores an older snapshot, and unsubscribes", async () => {
  let listener!: (progress: LaunchProgress) => void;
  let resolve!: (progress: LaunchProgress) => void;
  const stop = vi.fn();
  const bridge = {
    subscribe_launch_progress: (callback: typeof listener) => { listener = callback; return stop; },
    launch_progress: () => new Promise<LaunchProgress>(done => { resolve = done; }),
  };
  const { result, unmount } = renderHook(() => useLaunchProgress(bridge));
  await waitFor(() => expect(resolve).toBeDefined());
  act(() => listener({ phase: "loading", elapsedMs: 200, steps: [] }));
  await act(async () => resolve({ phase: "opening", elapsedMs: 100, steps: [] }));
  expect(result.current?.phase).toBe("loading");
  unmount();
  expect(stop).toHaveBeenCalledOnce();
});

it("waits for subscription readiness and unsubscribes upon ready", async () => {
  let finish!: () => void;
  let listener!: (progress: LaunchProgress) => void;
  const ready = new Promise<void>(resolve => { finish = resolve; });
  const stop = vi.fn();
  const fetch = vi.fn(async () => ({ phase: "opening" as const, elapsedMs: 1, steps: [] }));
  const subscribe = vi.fn((callback: typeof listener) => { listener = callback; return Object.assign(stop, { ready }); });
  const bridge = { launch_progress: fetch, subscribe_launch_progress: subscribe };
  const { unmount } = renderHook(() => useLaunchProgress(bridge));
  expect(subscribe).toHaveBeenCalledOnce();
  expect(fetch).not.toHaveBeenCalled();
  await act(async () => finish());
  expect(fetch).toHaveBeenCalledOnce();
  act(() => listener({ phase: "ready", elapsedMs: 2, steps: [] }));
  expect(stop).toHaveBeenCalledOnce();
  unmount();
  expect(stop).toHaveBeenCalledOnce();
});

it("ignores a snapshot after unmount", async () => {
  let resolve!: (progress: LaunchProgress) => void;
  const stop = vi.fn();
  const bridge = {
    subscribe_launch_progress: () => stop,
    launch_progress: () => new Promise<LaunchProgress>(done => { resolve = done; }),
  };
  const { result, unmount } = renderHook(() => useLaunchProgress(bridge));
  await waitFor(() => expect(resolve).toBeDefined());
  unmount();
  await act(async () => resolve({ phase: "ready", elapsedMs: 1, steps: [] }));
  expect(result.current).toBeUndefined();
  expect(stop).toHaveBeenCalledOnce();
});

it("cleans up both StrictMode subscriptions", async () => {
  const stops = [vi.fn(), vi.fn()];
  const subscribe = vi.fn(() => stops[subscribe.mock.calls.length - 1]);
  const bridge = { subscribe_launch_progress: subscribe, launch_progress: vi.fn(async () => ({ phase: "opening" as const, elapsedMs: 1, steps: [] })) };
  const { unmount } = renderHook(() => useLaunchProgress(bridge), { wrapper: StrictMode });
  await act(async () => {});
  expect(subscribe).toHaveBeenCalledTimes(2);
  unmount();
  expect(stops[0]).toHaveBeenCalledOnce();
  expect(stops[1]).toHaveBeenCalledOnce();
});

it("tolerates a missing bridge and a rejected snapshot", async () => {
  const absent = renderHook(() => useLaunchProgress({}));
  expect(absent.result.current).toBeUndefined();
  absent.unmount();
  const stop = vi.fn();
  const bridge = {
    subscribe_launch_progress: () => stop,
    launch_progress: () => Promise.reject(new Error("unavailable")),
  };
  const rejected = renderHook(() => useLaunchProgress(bridge));
  await act(async () => {});
  expect(rejected.result.current).toBeUndefined();
  rejected.unmount();
  expect(stop).toHaveBeenCalledOnce();
});
