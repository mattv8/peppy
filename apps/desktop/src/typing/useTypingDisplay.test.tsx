import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DesktopBridge, TypingSignal } from "@peppy/desktop-ui/bridge-contracts";
import { useTypingDisplay } from "./useTypingDisplay";

function display(overrides: { conversationId?: string; current?: boolean; local?: boolean; subscribe?: boolean; incomingMessageId?: string; incomingSender?: string; connected?: boolean; direct?: boolean } = {}) {
  let receive: (signal: TypingSignal) => void = () => undefined;
  const unsubscribe = vi.fn();
  const bridge = { subscribe_typing: overrides.subscribe === false ? undefined : vi.fn(listener => { receive = listener; return unsubscribe; }) } as unknown as DesktopBridge;
  const hook = renderHook(({ conversationId = "chat-1", current = true, local = false, incomingMessageId, incomingSender, connected = true, direct = true }) => useTypingDisplay({ bridge, conversationId, current, localUserTyping: local, incomingMessageId, incomingSender, members: ["member-1"], connected, direct }), { initialProps: overrides });
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

  it("keeps a stopped indicator briefly before hiding it", () => {
    vi.useFakeTimers(); vi.setSystemTime(0);
    const { result, receive } = display();
    receive({ conversationId: "chat-1", state: "started" });
    receive({ conversationId: "chat-2", state: "stopped" });
    expect(result.current.visible).toBe(true);
    receive({ conversationId: "chat-1", state: "stopped" });
    expect(result.current.visible).toBe(true);
    act(() => vi.advanceTimersByTime(1000));
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

  it("clears when an incoming direct message arrives or the connection is lost", () => {
    const { result, receive, rerender } = display();
    receive({ conversationId: "chat-1", state: "started" });
    rerender({ conversationId: "chat-1", current: true, local: false, incomingMessageId: "incoming-2", connected: true, direct: true });
    expect(result.current.visible).toBe(false);

    receive({ conversationId: "chat-1", state: "started" });
    rerender({ conversationId: "chat-1", current: true, local: false, incomingMessageId: "incoming-2", connected: false, direct: true });
    expect(result.current.visible).toBe(false);
  });

  it("removes a group member when their message arrives without restoring their deadline", () => {
    const { result, receive, rerender } = display({ direct: false });
    rerender({ conversationId: "chat-1", current: true, local: false, direct: false, incomingSender: undefined });
    receive({ conversationId: "chat-1", state: "started", participant: "member-1", expiresAtMs: String(Date.now() + 10_000) });
    expect(result.current.participants).toEqual(["member-1"]);
    rerender({ conversationId: "chat-1", current: true, local: false, direct: false, incomingSender: "member-1" });
    expect(result.current.visible).toBe(false);
  });

  it("clears a restarted group typer for each new message from the same sender", () => {
    const { result, receive, rerender } = display({ direct: false });
    receive({ conversationId: "chat-1", state: "started", participant: "member-1", expiresAtMs: String(Date.now() + 10_000) });
    rerender({ conversationId: "chat-1", current: true, local: false, direct: false, incomingMessageId: "message-1", incomingSender: "member-1" });
    expect(result.current.visible).toBe(false);

    receive({ conversationId: "chat-1", state: "started", participant: "member-1", expiresAtMs: String(Date.now() + 10_000) });
    expect(result.current.visible).toBe(true);
    rerender({ conversationId: "chat-1", current: true, local: false, direct: false, incomingMessageId: "message-2", incomingSender: "member-1" });
    expect(result.current.visible).toBe(false);
  });

  it("does not let a group stopped signal outlive its authenticated deadline", () => {
    vi.useFakeTimers(); vi.setSystemTime(0);
    const { result, receive } = display({ direct: false });
    receive({ conversationId: "chat-1", state: "started", participant: "member-1", expiresAtMs: "500" });
    receive({ conversationId: "chat-1", state: "stopped", participant: "member-1" });
    act(() => vi.advanceTimersByTime(500));
    expect(result.current.visible).toBe(false);
  });

  it("keeps a visible remote indicator when local typing starts", () => {
    const { result, receive, rerender } = display();
    receive({ conversationId: "chat-1", state: "started" });
    rerender({ conversationId: "chat-1", current: true, local: true });
    expect(result.current.visible).toBe(true);
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
