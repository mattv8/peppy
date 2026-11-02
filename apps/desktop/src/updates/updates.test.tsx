import { act, cleanup, render, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useUpdateActivity, useUpdateStatus, updateApi } from "./updates";
import { UPDATE_STATUS_SERDE_FIXTURE, type UpdateStatus } from "./update-contract";

const invokeMock = vi.hoisted(() => vi.fn());
const listenMock = vi.hoisted(() => vi.fn(async () => vi.fn()));

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));

function ActivityReporter({ enabled = true }: { enabled?: boolean }) {
  useUpdateActivity(enabled);
  return null;
}

describe("update bridge and activity reporter", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-16T12:00:00Z"));
    invokeMock.mockReset().mockResolvedValue(undefined);
    listenMock.mockClear();
    Object.defineProperty(window, "__TAURI_INTERNALS__", { configurable: true, value: {} });
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.useRealTimers();
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  it("uses only the frozen update commands and argument shapes", async () => {
    await updateApi.status();
    await updateApi.setAutomatic(true);
    await updateApi.reportActivity(false);

    expect(invokeMock).toHaveBeenNthCalledWith(1, "update_status", undefined);
    expect(invokeMock).toHaveBeenNthCalledWith(2, "set_automatic_updates", { enabled: true });
    expect(invokeMock).toHaveBeenNthCalledWith(3, "report_update_activity", { active: false });
  });

  it("reports mount and genuine interaction, but heartbeat remains inactive and stops on teardown", async () => {
    const { unmount } = render(<ActivityReporter />);
    await act(async () => {});
    expect(invokeMock).toHaveBeenCalledWith("report_update_activity", { active: true });

    for (const [target, event] of [[document, "pointerdown"], [document, "keydown"], [document, "input"], [document, "focusin"], [document, "wheel"], [window, "focus"]] as const) {
      await act(async () => { vi.advanceTimersByTime(1_000); });
      fireEvent(target, event);
      await act(async () => {});
    }
    expect(invokeMock).toHaveBeenCalledTimes(7);
    fireEvent(document, "wheel");
    await act(async () => {});
    expect(invokeMock).toHaveBeenCalledTimes(7);
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
    fireEvent(document, "visibilitychange");
    await act(async () => {});
    expect(invokeMock).toHaveBeenLastCalledWith("report_update_activity", { active: false });
    await act(async () => { vi.advanceTimersByTime(30_000); });
    expect(invokeMock).toHaveBeenLastCalledWith("report_update_activity", { active: false });

    const callsBeforeUnmount = invokeMock.mock.calls.length;
    unmount();
    fireEvent(window, "focus");
    fireEvent(document, "wheel");
    fireEvent(document, "visibilitychange");
    await act(async () => { vi.advanceTimersByTime(60_000); });
    expect(invokeMock).toHaveBeenCalledTimes(callsBeforeUnmount);
  });

  it("does not report from a browser host", async () => {
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    render(<ActivityReporter />);
    await act(async () => {});
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("fences stale refreshes while allowing an action's final result to win", async () => {
    const beforeAction = deferred<UpdateStatus>();
    const duringAction = deferred<UpdateStatus>();
    const firstAction = deferred<UpdateStatus>();
    const afterAction = deferred<UpdateStatus>();
    const secondAction = deferred<UpdateStatus>();
    vi.spyOn(updateApi, "status")
      .mockResolvedValueOnce({ ...UPDATE_STATUS_SERDE_FIXTURE, currentVersion: "1.0.0" })
      .mockReturnValueOnce(beforeAction.promise)
      .mockReturnValueOnce(duringAction.promise)
      .mockReturnValueOnce(afterAction.promise);
    const { result } = renderHook(() => useUpdateStatus(true));
    await act(async () => {});
    await act(async () => { void result.current.refresh(); });
    await act(async () => { void result.current.run(() => firstAction.promise); });

    await act(async () => { beforeAction.resolve({ ...UPDATE_STATUS_SERDE_FIXTURE, currentVersion: "stale" }); });
    expect(result.current.status?.currentVersion).toBe("1.0.0");
    await act(async () => { void result.current.refresh(); });
    await act(async () => { duringAction.resolve({ ...UPDATE_STATUS_SERDE_FIXTURE, currentVersion: "2.0.0" }); });
    expect(result.current.status?.currentVersion).toBe("2.0.0");
    await act(async () => { firstAction.resolve({ ...UPDATE_STATUS_SERDE_FIXTURE, currentVersion: "3.0.0" }); });
    expect(result.current.status?.currentVersion).toBe("3.0.0");

    await act(async () => { void result.current.run(() => secondAction.promise); });
    await act(async () => { void result.current.refresh(); });
    await act(async () => { secondAction.resolve({ ...UPDATE_STATUS_SERDE_FIXTURE, currentVersion: "4.0.0", automatic: false, phase: "deferred" }); });
    await act(async () => { afterAction.resolve({ ...UPDATE_STATUS_SERDE_FIXTURE, currentVersion: "stale-policy", automatic: true, phase: "idle" }); });
    expect(result.current.status).toMatchObject({ currentVersion: "4.0.0", automatic: false, phase: "deferred" });
  });

  it("keeps an action BridgeError through automatic refresh and clears it on explicit retry", async () => {
    vi.spyOn(updateApi, "status")
      .mockResolvedValueOnce(UPDATE_STATUS_SERDE_FIXTURE)
      .mockResolvedValueOnce({ ...UPDATE_STATUS_SERDE_FIXTURE, phase: "ready" })
      .mockResolvedValueOnce({ ...UPDATE_STATUS_SERDE_FIXTURE, phase: "idle" });
    const { result } = renderHook(() => useUpdateStatus(true));
    await act(async () => {});
    await act(async () => { await result.current.run(async () => { throw { code: "update-busy", message: "An update is already in progress." }; }); });
    await act(async () => { await result.current.refresh(); });

    expect(result.current.error).toBe("An update is already in progress. (update-busy)");
    await act(async () => { await result.current.refresh(true); });
    expect(result.current.error).toBe("");
  });

  it("applies an action error after a refresh that completed while the action was pending", async () => {
    const pendingAction = deferred<UpdateStatus>();
    const pendingRefresh = deferred<UpdateStatus>();
    vi.spyOn(updateApi, "status")
      .mockResolvedValueOnce(UPDATE_STATUS_SERDE_FIXTURE)
      .mockReturnValueOnce(pendingRefresh.promise);
    const { result } = renderHook(() => useUpdateStatus(true));
    await act(async () => {});
    await act(async () => { void result.current.run(() => pendingAction.promise); });
    await act(async () => { void result.current.refresh(); });
    await act(async () => { pendingRefresh.resolve({ ...UPDATE_STATUS_SERDE_FIXTURE, phase: "downloading" }); });
    await act(async () => { pendingAction.reject({ code: "app-locked", message: "Unlock Peppy before restarting." }); });

    expect(result.current.error).toBe("Unlock Peppy before restarting. (app-locked)");
  });
});

function fireEvent(target: EventTarget, type: string) {
  target.dispatchEvent(new Event(type, { bubbles: true }));
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((complete, fail) => { resolve = complete; reject = fail; });
  return { promise, resolve, reject };
}
