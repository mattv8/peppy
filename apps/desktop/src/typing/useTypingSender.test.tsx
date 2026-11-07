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
    visible: true,
    sharingEnabled: true,
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
    act(() => vi.advanceTimersByTime(3_000));
    act(() => result.current.noteInput("message"));
    act(() => vi.advanceTimersByTime(200));
    expect(sendTyping).toHaveBeenNthCalledWith(2, { conversationId: "conversation-1", state: "started" });

    act(() => vi.advanceTimersByTime(6_000));
    expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "stopped" });
  });

  it("resets the idle deadline when input continues", () => {
    vi.useFakeTimers();
    const { result, sendTyping } = renderSender();
    act(() => result.current.noteInput("message"));
    act(() => vi.advanceTimersByTime(5_000));
    act(() => result.current.noteInput("message"));
    act(() => vi.advanceTimersByTime(1_001));
    expect(sendTyping).not.toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "stopped" });
    act(() => vi.advanceTimersByTime(4_999));
    expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "stopped" });
  });

  it("stops once when sending", () => {
    const { result, sendTyping } = renderSender();
    act(() => result.current.noteInput("message"));
    act(() => result.current.noteSend());
    act(() => result.current.noteSend());
    expect(sendTyping).toHaveBeenCalledTimes(2);
    expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "stopped" });
  });

  it.each([
    ["conversation switch", { conversationId: "conversation-2" }],
    ["route eligibility loss", { current: false }],
    ["send becomes unavailable", { sendSupported: false }],
    ["composer becomes read-only", { readOnly: true }],
  ])("stops on %s", (_reason, next) => {
    const { result, rerender, sendTyping } = renderSender();
    act(() => result.current.noteInput("message"));
    rerender({ bridge: { send_typing: sendTyping } as unknown as DesktopBridge, conversationId: "conversation-1", current: true, existing: true, sendSupported: true, readOnly: false, visible: true, sharingEnabled: true, ...next });
    expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "stopped" });
  });

  it.each(["visibilitychange", "pagehide", "focusout"])("stops on %s", eventType => {
    const { result, sendTyping } = renderSender();
    act(() => result.current.noteInput("message"));
    if (eventType === "visibilitychange") {
      Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
      act(() => document.dispatchEvent(new Event(eventType)));
      Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    } else if (eventType === "focusout") {
      const textarea = document.createElement("textarea"); textarea.id = "composer-textarea"; document.body.append(textarea);
      act(() => textarea.dispatchEvent(new FocusEvent(eventType, { bubbles: true })));
      textarea.remove();
    } else act(() => window.dispatchEvent(new Event(eventType)));
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

  it("reads a disabled sharing preference before another window sends an event", () => {
    const { result, sendTyping } = renderSender();
    localStorage.setItem(LAYOUT_KEY, JSON.stringify({ shareTyping: false }));
    act(() => result.current.noteInput("message"));
    expect(sendTyping).not.toHaveBeenCalled();
    expect(result.current.state).toBe("suppressed");
    localStorage.setItem(LAYOUT_KEY, JSON.stringify({ shareTyping: true }));
    act(() => result.current.noteInput("message"));
    expect(result.current.state).toBe("active");
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

  it("rearms refresh after input resumes and returns suppressed state to idle", () => {
    vi.useFakeTimers();
    const { result, sendTyping } = renderSender();
    act(() => result.current.noteInput("message"));
    act(() => vi.advanceTimersByTime(3_000));
    act(() => result.current.noteInput("message"));
    act(() => vi.advanceTimersByTime(200));
    expect(sendTyping).toHaveBeenCalledTimes(2);
    expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "started" });
    act(() => result.current.noteSend());
    expect(result.current.state).toBe("idle");
    act(() => result.current.noteInput(" "));
    expect(result.current.state).toBe("idle");
  });

  it.each([3_200, 4_000, 5_999])("restarts immediately when input resumes after %i ms", (elapsed) => {
    vi.useFakeTimers();
    const { result, sendTyping } = renderSender();

    act(() => result.current.noteInput("message"));
    act(() => vi.advanceTimersByTime(elapsed));
    act(() => result.current.noteInput("message"));
    act(() => result.current.noteInput("message again"));

    expect(sendTyping).toHaveBeenCalledTimes(2);
    expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "started" });
    act(() => vi.advanceTimersByTime(3_199));
    expect(sendTyping).toHaveBeenCalledTimes(2);
    act(() => vi.advanceTimersByTime(2_801));
    expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "stopped" });
  });

  it("does not bypass the refresh interval for repeated input", () => {
    vi.useFakeTimers();
    const { result, sendTyping } = renderSender();
    act(() => result.current.noteInput("message"));
    act(() => vi.advanceTimersByTime(3_199));
    act(() => result.current.noteInput("message again"));
    expect(sendTyping).toHaveBeenCalledTimes(1);
    act(() => vi.advanceTimersByTime(1));
    expect(sendTyping).toHaveBeenCalledTimes(2);
    act(() => result.current.noteInput("at refresh boundary"));
    expect(sendTyping).toHaveBeenCalledTimes(2);
  });

  it.each(["visible", "sharingEnabled"] as const)("stops once on %s loss and requires input after restoration", (property) => {
    vi.useFakeTimers();
    const { result, rerender, sendTyping, unmount } = renderSender();
    const props = { bridge: { send_typing: sendTyping } as unknown as DesktopBridge, conversationId: "conversation-1", current: true, existing: true, sendSupported: true, readOnly: false, visible: true, sharingEnabled: true };
    act(() => result.current.noteInput("message"));
    rerender({ ...props, [property]: false });
    act(() => result.current.noteInput("while unavailable"));
    act(() => vi.advanceTimersByTime(10_000));
    rerender(props);
    act(() => vi.advanceTimersByTime(10_000));
    expect(sendTyping).toHaveBeenCalledTimes(2);
    expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "stopped" });
    unmount();
    expect(sendTyping).toHaveBeenCalledTimes(2);
  });

  it("stops once when the composer surface is hidden", () => {
    const { result, rerender, sendTyping, unmount } = renderSender();
    act(() => result.current.noteInput("message"));

    rerender({ bridge: { send_typing: sendTyping } as unknown as DesktopBridge, conversationId: "conversation-1", current: true, existing: true, sendSupported: true, readOnly: false, visible: false, sharingEnabled: true });
    act(() => document.dispatchEvent(new FocusEvent("focusout", { bubbles: true })));
    unmount();

    expect(sendTyping).toHaveBeenCalledTimes(2);
    expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: "conversation-1", state: "stopped" });
  });
});
