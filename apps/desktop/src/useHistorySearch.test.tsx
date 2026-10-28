import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DesktopBridge, HistorySearchSnapshot } from "@peppy/desktop-ui/bridge-contracts";
import { useHistorySearch } from "./useHistorySearch";

type HistoryBridge = Pick<DesktopBridge, "get_history_search" | "start_history_search" | "cancel_history_search" | "subscribe">;

const inactive = (overrides: Partial<HistorySearchSnapshot> = {}): HistorySearchSnapshot => ({
  state: "idle",
  canSearchFurther: true,
  progressVersion: "0",
  indexVersion: "0",
  ...overrides,
});

const active = (overrides: Partial<HistorySearchSnapshot> = {}): HistorySearchSnapshot => inactive({
  attemptId: "attempt-1",
  state: "catchingUp",
  ...overrides,
});

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((accept, decline) => { resolve = accept; reject = decline; });
  return { promise, resolve, reject };
};

function stubOperationIds() {
  let next = 0;
  const randomUUID = vi.fn(() => `operation-${++next}`);
  vi.stubGlobal("crypto", { randomUUID });
  return randomUUID;
}

function bridgeFixture(snapshot = inactive()) {
  let listener: (() => void) | undefined;
  const bridge: HistoryBridge = {
    get_history_search: vi.fn().mockResolvedValue(snapshot),
    start_history_search: vi.fn().mockResolvedValue(active()),
    cancel_history_search: vi.fn().mockResolvedValue(inactive({ state: "cancelled" })),
    subscribe: vi.fn((next) => {
      listener = next;
      return () => { listener = undefined; };
    }),
  };
  return { bridge, notify: () => listener?.() };
}

describe("useHistorySearch", () => {
  beforeEach(() => {
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("performs a readonly availability read when the search opens", async () => {
    const fixture = bridgeFixture();
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));

    await waitFor(() => expect(result.current.historySearch).toEqual(inactive()));
    expect(fixture.bridge.get_history_search).toHaveBeenCalledTimes(1);
    expect(fixture.bridge.start_history_search).not.toHaveBeenCalled();
    expect(fixture.bridge.cancel_history_search).not.toHaveBeenCalled();
  });

  it("starts only explicitly with an operation ID and no query arguments", async () => {
    const fixture = bridgeFixture();
    stubOperationIds();
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));
    await waitFor(() => expect(result.current.historySearch).toBeDefined());

    await act(() => result.current.onSearchFurtherBack());

    expect(fixture.bridge.start_history_search).toHaveBeenCalledWith({ operationId: "operation-1" });
    expect(vi.mocked(fixture.bridge.start_history_search).mock.calls[0]).toHaveLength(1);
  });

  it("deduplicates a pending start for the active view", async () => {
    const fixture = bridgeFixture();
    const starting = deferred<HistorySearchSnapshot>();
    vi.mocked(fixture.bridge.start_history_search).mockReturnValue(starting.promise);
    stubOperationIds();
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));
    await waitFor(() => expect(result.current.historySearch).toBeDefined());

    const first = result.current.onSearchFurtherBack();
    const second = result.current.onSearchFurtherBack();
    expect(second).toBe(first);
    expect(fixture.bridge.start_history_search).toHaveBeenCalledTimes(1);
    await act(async () => starting.resolve(active()));
  });

  it("polls active work only while the view and document are visible, then cleans up", async () => {
    vi.useFakeTimers();
    const fixture = bridgeFixture(active());
    const { result, rerender, unmount } = renderHook(
      ({ visible }) => useHistorySearch({ sessionKey: "a", enabled: true, visible, bridge: fixture.bridge }),
      { initialProps: { visible: true } },
    );
    await act(async () => { await Promise.resolve(); });
    expect(result.current.historySearch?.state).toBe("catchingUp");
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
    expect(fixture.bridge.get_history_search).toHaveBeenCalledTimes(2);

    Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
    act(() => document.dispatchEvent(new Event("visibilitychange")));
    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(fixture.bridge.get_history_search).toHaveBeenCalledTimes(2);

    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    act(() => document.dispatchEvent(new Event("visibilitychange")));
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
    expect(fixture.bridge.get_history_search).toHaveBeenCalledTimes(4);

    rerender({ visible: false });
    await act(async () => { await vi.advanceTimersByTimeAsync(2_000); });
    expect(fixture.bridge.get_history_search).toHaveBeenCalledTimes(4);
    expect(fixture.bridge.subscribe).toHaveBeenCalledTimes(1);
    unmount();
  });

  it("ignores late reads and starts after a session switch", async () => {
    const fixture = bridgeFixture();
    const oldRead = deferred<HistorySearchSnapshot>();
    vi.mocked(fixture.bridge.get_history_search).mockReturnValueOnce(oldRead.promise).mockResolvedValue(inactive({ progressVersion: "new" }));
    const oldStart = deferred<HistorySearchSnapshot>();
    vi.mocked(fixture.bridge.start_history_search).mockReturnValue(oldStart.promise);
    stubOperationIds();
    const { result, rerender } = renderHook(
      ({ sessionKey }) => useHistorySearch({ sessionKey, enabled: true, visible: true, bridge: fixture.bridge }),
      { initialProps: { sessionKey: "old" } },
    );
    const start = result.current.onSearchFurtherBack();
    rerender({ sessionKey: "new" });
    await act(async () => { oldRead.resolve(active({ progressVersion: "old" })); oldStart.resolve(active({ progressVersion: "old" })); await start; });

    await waitFor(() => expect(result.current.historySearch?.progressVersion).toBe("new"));
  });

  it("reuses an operation ID after an unknown transport rejection", async () => {
    const fixture = bridgeFixture();
    vi.mocked(fixture.bridge.start_history_search).mockRejectedValueOnce(new Error("transport lost")).mockResolvedValueOnce(active());
    const randomUUID = stubOperationIds();
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));
    await waitFor(() => expect(result.current.historySearch).toBeDefined());

    await expect(result.current.onSearchFurtherBack()).rejects.toThrow("transport lost");
    await act(() => result.current.onSearchFurtherBack());

    expect(randomUUID).toHaveBeenCalledTimes(1);
    expect(fixture.bridge.start_history_search).toHaveBeenNthCalledWith(1, { operationId: "operation-1" });
    expect(fixture.bridge.start_history_search).toHaveBeenNthCalledWith(2, { operationId: "operation-1" });
    expect(result.current.historySearch?.state).toBe("catchingUp");
  });

  it("does not cancel without an owned attempt", async () => {
    const fixture = bridgeFixture();
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));
    await waitFor(() => expect(result.current.historySearch).toBeDefined());

    await result.current.onCancelHistorySearch();

    expect(fixture.bridge.cancel_history_search).not.toHaveBeenCalled();
  });

  it("cancels an attempt reported by a read when this view never started it", async () => {
    const fixture = bridgeFixture(active({ attemptId: "attempt-a" }));
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));
    await waitFor(() => expect(result.current.historySearch?.attemptId).toBe("attempt-a"));

    await act(() => result.current.onCancelHistorySearch());

    expect(fixture.bridge.start_history_search).not.toHaveBeenCalled();
    expect(fixture.bridge.cancel_history_search).toHaveBeenCalledTimes(1);
    expect(fixture.bridge.cancel_history_search).toHaveBeenCalledWith({ attemptId: "attempt-a" });
  });

  it("adopts a reported attempt after an unknown start, keeps the operation until its terminal result, then starts anew", async () => {
    const fixture = bridgeFixture(inactive());
    vi.mocked(fixture.bridge.start_history_search).mockRejectedValueOnce(new Error("transport lost")).mockResolvedValueOnce(active({ attemptId: "attempt-b" }));
    vi.mocked(fixture.bridge.get_history_search).mockResolvedValueOnce(inactive()).mockResolvedValueOnce(active({ attemptId: "attempt-a" }));
    const randomUUID = stubOperationIds();
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));
    await waitFor(() => expect(result.current.historySearch).toBeDefined());
    await expect(result.current.onSearchFurtherBack()).rejects.toThrow("transport lost");

    act(() => fixture.notify());
    await waitFor(() => expect(result.current.historySearch?.attemptId).toBe("attempt-a"));
    vi.mocked(fixture.bridge.cancel_history_search).mockResolvedValueOnce(inactive({ state: "cancelled", attemptId: "attempt-a" }));
    await act(() => result.current.onCancelHistorySearch());
    expect(fixture.bridge.cancel_history_search).toHaveBeenCalledWith({ attemptId: "attempt-a" });
    await waitFor(() => expect(result.current.historySearch?.state).toBe("cancelled"));

    await act(() => result.current.onSearchFurtherBack());
    expect(randomUUID).toHaveBeenCalledTimes(2);
    expect(fixture.bridge.start_history_search).toHaveBeenNthCalledWith(2, { operationId: "operation-2" });
  });

  it("does not adopt an attempt from a terminal read, so Cancel stays a no-op", async () => {
    const fixture = bridgeFixture(inactive());
    vi.mocked(fixture.bridge.get_history_search).mockResolvedValueOnce(inactive()).mockResolvedValueOnce(inactive({ state: "partial", attemptId: "attempt-t" }));
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));
    await waitFor(() => expect(result.current.historySearch).toBeDefined());

    act(() => fixture.notify());
    await waitFor(() => expect(result.current.historySearch?.state).toBe("partial"));
    await act(() => result.current.onCancelHistorySearch());

    expect(fixture.bridge.cancel_history_search).not.toHaveBeenCalled();
  });

  it("coalesces subscribed changes and preserves current index/status revisions", async () => {
    const fixture = bridgeFixture(active());
    vi.mocked(fixture.bridge.get_history_search).mockResolvedValueOnce(active()).mockResolvedValueOnce(active({ state: "indexing", progressVersion: "2", indexVersion: "3" }));
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));
    await waitFor(() => expect(result.current.historySearch?.progressVersion).toBe("0"));

    act(() => { fixture.notify(); fixture.notify(); });
    await act(async () => { await Promise.resolve(); });
    await waitFor(() => expect(result.current.historySearch).toMatchObject({ state: "indexing", progressVersion: "2", indexVersion: "3" }));
    expect(fixture.bridge.get_history_search).toHaveBeenCalledTimes(2);
  });

  it("does not let stale reads overwrite a newer start or cancel", async () => {
    const fixture = bridgeFixture(inactive());
    const beforeStart = deferred<HistorySearchSnapshot>();
    const beforeCancel = deferred<HistorySearchSnapshot>();
    vi.mocked(fixture.bridge.get_history_search)
      .mockResolvedValueOnce(inactive())
      .mockReturnValueOnce(beforeStart.promise)
      .mockReturnValueOnce(beforeCancel.promise);
    vi.mocked(fixture.bridge.start_history_search).mockResolvedValue(active({ attemptId: "owned" }));
    vi.mocked(fixture.bridge.cancel_history_search).mockResolvedValue(inactive({ attemptId: "owned", state: "cancelled" }));
    stubOperationIds();
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));
    await waitFor(() => expect(result.current.historySearch?.state).toBe("idle"));

    act(() => fixture.notify());
    await act(async () => { await Promise.resolve(); });
    await act(() => result.current.onSearchFurtherBack());
    await act(async () => beforeStart.resolve(inactive({ state: "partial", progressVersion: "old" })));
    expect(result.current.historySearch).toMatchObject({ state: "catchingUp", attemptId: "owned" });

    act(() => fixture.notify());
    await act(async () => { await Promise.resolve(); });
    await act(() => result.current.onCancelHistorySearch());
    await act(async () => beforeCancel.resolve(inactive({ attemptId: "owned", state: "partial", progressVersion: "older" })));
    expect(result.current.historySearch).toMatchObject({ state: "cancelled", attemptId: "owned" });
  });

  it("retains a hidden start acknowledgement for the same-session reopen cancel", async () => {
    const fixture = bridgeFixture(inactive());
    const started = deferred<HistorySearchSnapshot>();
    vi.mocked(fixture.bridge.start_history_search).mockReturnValue(started.promise);
    vi.mocked(fixture.bridge.get_history_search).mockResolvedValueOnce(inactive()).mockResolvedValueOnce(active({ attemptId: "owned" }));
    stubOperationIds();
    const { result, rerender } = renderHook(
      ({ visible }) => useHistorySearch({ sessionKey: "a", enabled: true, visible, bridge: fixture.bridge }),
      { initialProps: { visible: true } },
    );
    await waitFor(() => expect(result.current.historySearch?.state).toBe("idle"));

    const start = result.current.onSearchFurtherBack();
    rerender({ visible: false });
    await act(async () => { started.resolve(active({ attemptId: "owned" })); await start; });
    expect(result.current.historySearch?.state).toBe("idle");

    rerender({ visible: true });
    await waitFor(() => expect(result.current.historySearch?.attemptId).toBe("owned"));
    await act(() => result.current.onCancelHistorySearch());
    expect(fixture.bridge.cancel_history_search).toHaveBeenCalledWith({ attemptId: "owned" });
  });

  it("rejects callbacks retained from a hidden or replaced session", async () => {
    const fixture = bridgeFixture(inactive());
    stubOperationIds();
    const { result, rerender } = renderHook(
      ({ sessionKey, visible }) => useHistorySearch({ sessionKey, enabled: true, visible, bridge: fixture.bridge }),
      { initialProps: { sessionKey: "a", visible: true } },
    );
    await waitFor(() => expect(result.current.historySearch).toBeDefined());
    const retainedStart = result.current.onSearchFurtherBack;

    rerender({ sessionKey: "a", visible: false });
    await retainedStart();
    expect(fixture.bridge.start_history_search).not.toHaveBeenCalled();

    vi.mocked(fixture.bridge.get_history_search).mockResolvedValue(inactive({ progressVersion: "new" }));
    rerender({ sessionKey: "b", visible: true });
    await waitFor(() => expect(result.current.historySearch).toMatchObject({ state: "idle", canSearchFurther: true, progressVersion: "new" }));
    await retainedStart();
    expect(fixture.bridge.start_history_search).not.toHaveBeenCalled();
  });

  it("does not start a new session from a callback held across a session switch", async () => {
    const fixture = bridgeFixture(inactive());
    const { result, rerender } = renderHook(
      ({ sessionKey }) => useHistorySearch({ sessionKey, enabled: true, visible: true, bridge: fixture.bridge }),
      { initialProps: { sessionKey: "old" } },
    );
    await waitFor(() => expect(result.current.historySearch).toBeDefined());
    const heldStart = result.current.onSearchFurtherBack;

    vi.mocked(fixture.bridge.get_history_search).mockResolvedValue(inactive({ progressVersion: "new" }));
    rerender({ sessionKey: "new" });
    await waitFor(() => expect(result.current.historySearch).toMatchObject({ state: "idle", canSearchFurther: true, progressVersion: "new" }));

    await act(() => heldStart());
    expect(fixture.bridge.start_history_search).not.toHaveBeenCalled();
  });

  it("does not let a read begun during a pending start overwrite the acknowledged start", async () => {
    const fixture = bridgeFixture(inactive());
    const started = deferred<HistorySearchSnapshot>();
    const staleRead = deferred<HistorySearchSnapshot>();
    vi.mocked(fixture.bridge.get_history_search).mockResolvedValueOnce(inactive()).mockReturnValueOnce(staleRead.promise);
    vi.mocked(fixture.bridge.start_history_search).mockReturnValue(started.promise);
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));
    await waitFor(() => expect(result.current.historySearch?.state).toBe("idle"));

    const start = result.current.onSearchFurtherBack();
    act(() => fixture.notify());
    await act(async () => { await Promise.resolve(); });
    expect(fixture.bridge.get_history_search).toHaveBeenCalledTimes(2);

    await act(async () => { started.resolve(active({ attemptId: "owned" })); await start; });
    expect(result.current.historySearch).toMatchObject({ state: "catchingUp", attemptId: "owned" });
    await act(async () => { staleRead.resolve(inactive({ progressVersion: "stale" })); await Promise.resolve(); });
    expect(result.current.historySearch).toMatchObject({ state: "catchingUp", attemptId: "owned" });
  });

  it("does not let a read begun during a pending cancel overwrite the acknowledged cancel", async () => {
    const fixture = bridgeFixture(inactive());
    const cancelled = deferred<HistorySearchSnapshot>();
    const staleRead = deferred<HistorySearchSnapshot>();
    vi.mocked(fixture.bridge.get_history_search).mockResolvedValueOnce(inactive()).mockReturnValueOnce(staleRead.promise);
    vi.mocked(fixture.bridge.start_history_search).mockResolvedValue(active({ attemptId: "owned" }));
    vi.mocked(fixture.bridge.cancel_history_search).mockReturnValue(cancelled.promise);
    stubOperationIds();
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));
    await waitFor(() => expect(result.current.historySearch?.state).toBe("idle"));
    await act(() => result.current.onSearchFurtherBack());

    const cancel = result.current.onCancelHistorySearch();
    act(() => fixture.notify());
    await act(async () => { await Promise.resolve(); });
    expect(fixture.bridge.get_history_search).toHaveBeenCalledTimes(2);

    await act(async () => { cancelled.resolve(inactive({ attemptId: "owned", state: "cancelled" })); await cancel; });
    expect(result.current.historySearch).toMatchObject({ state: "cancelled", attemptId: "owned" });
    await act(async () => { staleRead.resolve(active({ attemptId: "owned", progressVersion: "stale" })); await Promise.resolve(); });
    expect(result.current.historySearch).toMatchObject({ state: "cancelled", attemptId: "owned" });
  });

  it("keeps real active state and polling after readonly or cancel failures", async () => {
    vi.useFakeTimers();
    const fixture = bridgeFixture(active({ attemptId: "owned" }));
    vi.mocked(fixture.bridge.get_history_search).mockResolvedValueOnce(active({ attemptId: "owned" })).mockRejectedValueOnce(new Error("read lost")).mockResolvedValue(active({ attemptId: "owned", progressVersion: "2" }));
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));
    await act(async () => { await Promise.resolve(); });
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
    expect(result.current.historySearch?.state).toBe("catchingUp");
    await act(async () => { await vi.advanceTimersByTimeAsync(1_000); });
    expect(fixture.bridge.get_history_search).toHaveBeenCalledTimes(3);

    vi.useRealTimers();
    const startedFixture = bridgeFixture(inactive());
    vi.mocked(startedFixture.bridge.start_history_search).mockResolvedValue(active({ attemptId: "owned" }));
    vi.mocked(startedFixture.bridge.cancel_history_search).mockRejectedValue(new Error("cancel lost"));
    stubOperationIds();
    const startedHook = renderHook(() => useHistorySearch({ sessionKey: "b", enabled: true, visible: true, bridge: startedFixture.bridge }));
    await waitFor(() => expect(startedHook.result.current.historySearch).toBeDefined());
    await act(() => startedHook.result.current.onSearchFurtherBack());
    await expect(startedHook.result.current.onCancelHistorySearch()).rejects.toThrow("cancel lost");
    expect(startedHook.result.current.historySearch?.state).toBe("catchingUp");
  });

  it("starts a new operation after a terminal start acknowledgement without an attempt", async () => {
    const fixture = bridgeFixture(inactive());
    vi.mocked(fixture.bridge.start_history_search).mockResolvedValueOnce(inactive({ state: "blocked" })).mockResolvedValueOnce(active());
    const randomUUID = stubOperationIds();
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));
    await waitFor(() => expect(result.current.historySearch?.state).toBe("idle"));

    await act(() => result.current.onSearchFurtherBack());
    expect(result.current.historySearch?.state).toBe("blocked");
    await act(() => result.current.onSearchFurtherBack());

    expect(randomUUID).toHaveBeenCalledTimes(2);
    expect(fixture.bridge.start_history_search).toHaveBeenNthCalledWith(1, { operationId: "operation-1" });
    expect(fixture.bridge.start_history_search).toHaveBeenNthCalledWith(2, { operationId: "operation-2" });
  });

  it("keeps an unknown operation ID through an unrelated terminal read", async () => {
    const fixture = bridgeFixture(inactive());
    vi.mocked(fixture.bridge.start_history_search).mockRejectedValueOnce(new Error("transport lost")).mockResolvedValueOnce(active());
    vi.mocked(fixture.bridge.get_history_search).mockResolvedValueOnce(inactive()).mockResolvedValueOnce(inactive({ state: "partial" }));
    const randomUUID = stubOperationIds();
    const { result } = renderHook(() => useHistorySearch({ sessionKey: "a", enabled: true, visible: true, bridge: fixture.bridge }));
    await waitFor(() => expect(result.current.historySearch).toBeDefined());

    await expect(result.current.onSearchFurtherBack()).rejects.toThrow("transport lost");
    act(() => fixture.notify());
    await waitFor(() => expect(result.current.historySearch?.state).toBe("partial"));
    await act(() => result.current.onSearchFurtherBack());
    expect(randomUUID).toHaveBeenCalledTimes(1);
    expect(fixture.bridge.start_history_search).toHaveBeenNthCalledWith(2, { operationId: "operation-1" });
  });

  it("deduplicates an owned cancel and never auto-cancels hidden or disabled work", async () => {
    const disabledFixture = bridgeFixture(active());
    renderHook(() => useHistorySearch({ sessionKey: "disabled", enabled: false, visible: true, bridge: disabledFixture.bridge }));
    expect(disabledFixture.bridge.get_history_search).not.toHaveBeenCalled();
    expect(disabledFixture.bridge.subscribe).not.toHaveBeenCalled();

    const fixture = bridgeFixture(inactive());
    const cancelled = deferred<HistorySearchSnapshot>();
    vi.mocked(fixture.bridge.start_history_search).mockResolvedValue(active({ attemptId: "owned" }));
    vi.mocked(fixture.bridge.cancel_history_search).mockReturnValue(cancelled.promise);
    stubOperationIds();
    const { result, rerender } = renderHook(
      ({ enabled, visible }) => useHistorySearch({ sessionKey: "a", enabled, visible, bridge: fixture.bridge }),
      { initialProps: { enabled: true, visible: true } },
    );
    await waitFor(() => expect(result.current.historySearch).toBeDefined());
    await act(() => result.current.onSearchFurtherBack());
    const first = result.current.onCancelHistorySearch();
    const duplicate = result.current.onCancelHistorySearch();
    expect(duplicate).toBe(first);
    expect(fixture.bridge.cancel_history_search).toHaveBeenCalledTimes(1);

    rerender({ enabled: true, visible: false });
    rerender({ enabled: false, visible: false });
    expect(fixture.bridge.cancel_history_search).toHaveBeenCalledTimes(1);
    await act(async () => cancelled.resolve(inactive({ attemptId: "owned", state: "cancelled" })));
  });
});
