import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DesktopBridge, TypingSignal } from "@peppy/desktop-ui/bridge-contracts";
import { useTypingDisplay } from "./useTypingDisplay";

function display(overrides: { conversationId?: string; current?: boolean; local?: boolean; subscribe?: boolean } = {}) {
  let receive: (signal: TypingSignal) => void = () => undefined;
  const unsubscribe = vi.fn();
  const bridge = { subscribe_typing: overrides.subscribe === false ? undefined : vi.fn(listener => { receive = listener; return unsubscribe; }) } as unknown as DesktopBridge;
  const hook = renderHook(({ conversationId = "chat-1", current = true, local = false }) => useTypingDisplay(bridge, conversationId, current, local), { initialProps: overrides });
  return { ...hook, receive: (signal: TypingSignal) => act(() => receive(signal)), unsubscribe };
}

describe("useTypingDisplay", () => {
  afterEach(() => vi.useRealTimers());

  it("keeps a refreshed signal visible beyond its first deadline and announces once", () => {
    vi.useFakeTimers(); vi.setSystemTime(0);
    const { result, receive } = display();
    receive({ conversationId: "chat-1", state: "started", expiresAtMs: "10000" });
    act(() => vi.advanceTimersByTime(3000));
    receive({ conversationId: "chat-1", state: "started", expiresAtMs: "13000" });
    act(() => vi.advanceTimersByTime(7001));
    expect(result.current.visible).toBe(true);
    expect(result.current.announcement).toBe(false);
  });

  it("ignores another conversation and clears only its own stopped signal", () => {
    const { result, receive } = display();
    receive({ conversationId: "chat-1", state: "started" });
    receive({ conversationId: "chat-2", state: "stopped" });
    expect(result.current.visible).toBe(true);
    receive({ conversationId: "chat-1", state: "stopped" });
    expect(result.current.visible).toBe(false);
  });

  it("caps expiry and ignores invalid or past deadlines", () => {
    vi.useFakeTimers(); vi.setSystemTime(100);
    const { result, receive } = display();
    receive({ conversationId: "chat-1", state: "started", expiresAtMs: "99" });
    expect(result.current.visible).toBe(false);
    receive({ conversationId: "chat-1", state: "started", expiresAtMs: "invalid" });
    act(() => vi.advanceTimersByTime(15000));
    expect(result.current.visible).toBe(false);
  });

  it("clears on conversation changes, current loss, and unmount", () => {
    const { result, receive, rerender, unmount, unsubscribe } = display();
    receive({ conversationId: "chat-1", state: "started" });
    rerender({ conversationId: "chat-2", current: true, local: false });
    expect(result.current.visible).toBe(false);
    unmount();
    expect(unsubscribe).toHaveBeenCalled();
  });

  it("does nothing without a subscription or while the local user is typing", () => {
    const absent = display({ subscribe: false });
    expect(absent.result.current.visible).toBe(false);
    const { result, receive } = display({ local: true });
    receive({ conversationId: "chat-1", state: "started" });
    expect(result.current.announcement).toBe(false);
  });

  it("clears an announcement after five seconds and enforces the per-chat cooldown", () => {
    vi.useFakeTimers(); vi.setSystemTime(0);
    const { result, receive } = display();
    receive({ conversationId: "chat-1", state: "started" });
    expect(result.current.announcement).toBe(true);
    act(() => vi.advanceTimersByTime(5000));
    expect(result.current.announcement).toBe(false);
    receive({ conversationId: "chat-1", state: "stopped" });
    receive({ conversationId: "chat-1", state: "started" });
    expect(result.current.announcement).toBe(false);
  });
});
