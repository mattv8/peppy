import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DesktopBridge } from "@peppy/desktop-ui/bridge-contracts";
import { useTypingSender } from "./useTypingSender";

const LAYOUT_KEY = "peppy.layout.v1";

function renderSender(overrides: Partial<Parameters<typeof useTypingSender>[0]> = {}) {
  const sendTyping = vi.fn().mockResolvedValue(undefined);
  const props = {
    bridge: { send_typing: sendTyping } as Pick<DesktopBridge, "send_typing"> as DesktopBridge,
    conversationId: "conversation-1",
    current: true,
    existing: true,
    sendSupported: true,
    readOnly: false,
    ...overrides,
  };
  return { sendTyping, ...renderHook((next) => useTypingSender(next), { initialProps: props }) };
}

describe("useTypingSender", () => {
  beforeEach(() => {
    const values = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      clear: () => values.clear(),
      getItem: (key: string) => values.get(key) ?? null,
      removeItem: (key: string) => values.delete(key),
      setItem: (key: string, value: string) => values.set(key, value),
    });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("starts only for non-whitespace user input in a current existing conversation", () => {
    const { result, sendTyping } = renderSender();

    act(() => result.current.noteInput("   "));
    expect(sendTyping).not.toHaveBeenCalled();

    act(() => result.current.noteInput("message"));
    expect(sendTyping).toHaveBeenCalledWith({ conversationId: "conversation-1", state: "started" });
  });

  it.each([
    ["route is not current", { current: false }],
    ["conversation is a draft", { existing: false }],
    ["sending is unavailable", { sendSupported: false }],
    ["composer is read-only", { readOnly: true }],
    ["bridge cannot send typing", { bridge: {} as DesktopBridge }],
  ])("does not start when %s", (_reason, overrides) => {
    const { result, sendTyping } = renderSender(overrides);
    act(() => result.current.noteInput("message"));
    expect(sendTyping).not.toHaveBeenCalled();
  });

  it("refreshes an active signal and stops after idle", () => {
    vi.useFakeTimers();
    const { result, sendTyping } = renderSender();

    act(() => result.current.noteInput("message"));
    act(() => vi.advanceTimersByTime(3_200));
    expect(sendTyping).toHaveBeenNthCalledWith(2, { conversationId: "conversation-1", state: "started" });

    act(() => vi.advanceTimersByTime(2_800));
    expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "stopped" });
  });

  it.each(["noteSend", "setFocused"] as const)("stops once on %s", (method) => {
    const { result, sendTyping } = renderSender();
    act(() => result.current.noteInput("message"));
    act(() => method === "noteSend" ? result.current.noteSend() : result.current.setFocused(false));
    act(() => method === "noteSend" ? result.current.noteSend() : result.current.setFocused(false));
    expect(sendTyping).toHaveBeenCalledTimes(2);
    expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "stopped" });
  });

  it.each([
    ["conversation switch", { conversationId: "conversation-2" }],
    ["route eligibility loss", { current: false }],
  ])("stops on %s", (_reason, next) => {
    const { result, rerender, sendTyping } = renderSender();
    act(() => result.current.noteInput("message"));
    rerender({ bridge: { send_typing: sendTyping } as unknown as DesktopBridge, conversationId: "conversation-1", current: true, existing: true, sendSupported: true, readOnly: false, ...next });
    expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "stopped" });
  });

  it("stops when a storage event turns sharing off and suppresses future starts", () => {
    const { result, sendTyping } = renderSender();
    act(() => result.current.noteInput("message"));
    localStorage.setItem(LAYOUT_KEY, JSON.stringify({ shareTyping: false }));
    act(() => window.dispatchEvent(new StorageEvent("storage", { key: LAYOUT_KEY })));
    act(() => result.current.noteInput("message"));
    expect(sendTyping).toHaveBeenCalledTimes(2);
    expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "stopped" });
  });

  it("stops on window blur and unmount without retrying rejected sends", () => {
    const rejected = vi.fn().mockRejectedValue(new Error("failed"));
    const { result, unmount } = renderSender({ bridge: { send_typing: rejected } as unknown as DesktopBridge });
    act(() => result.current.noteInput("message"));
    act(() => window.dispatchEvent(new Event("blur")));
    unmount();
    expect(rejected).toHaveBeenCalledTimes(2);
    expect(rejected).toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "stopped" });
  });
});
