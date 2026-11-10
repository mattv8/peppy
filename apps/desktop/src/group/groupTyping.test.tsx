import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DesktopBridge, TypingSignal } from "@peppy/desktop-ui/bridge-contracts";
import { useTypingDisplay } from "../typing/useTypingDisplay";
import { groupTypingLabel } from "./groupCopy";

const alex = "+15551230001";
const sam = "+15551230002";
const jo = "+15551230003";
const roster = [alex, sam];

type GroupDisplayProps = { members: readonly string[]; incomingSender?: string };

function groupDisplay(initialMembers: readonly string[] = roster) {
  let receive: (signal: TypingSignal) => void = () => undefined;
  const subscribe = vi.fn((listener: (signal: TypingSignal) => void) => { receive = listener; return vi.fn(); });
  const bridge = { subscribe_typing: subscribe } as unknown as DesktopBridge;
  const hook = renderHook(
    ({ members, incomingSender }: GroupDisplayProps) => useTypingDisplay({ bridge, conversationId: "group-1", current: true, direct: false, members, incomingSender }),
    { initialProps: { members: initialMembers } as GroupDisplayProps },
  );
  return {
    ...hook,
    subscribeCount: () => subscribe.mock.calls.length,
    signal: (signal: Omit<TypingSignal, "conversationId">) => act(() => receive({ conversationId: "group-1", ...signal })),
  };
}

describe("group typing display", () => {
  afterEach(() => vi.useRealTimers());

  it("shows the participant who started typing, and only them", () => {
    vi.useFakeTimers(); vi.setSystemTime(0);
    const { result, signal } = groupDisplay();
    signal({ state: "started", participant: alex, expiresAtMs: "5000" });
    expect(result.current.visible).toBe(true);
    expect(result.current.participants).toEqual([alex]);
  });

  it("expires each participant on its own deadline", () => {
    vi.useFakeTimers(); vi.setSystemTime(0);
    const { result, signal } = groupDisplay();
    signal({ state: "started", participant: alex, expiresAtMs: "4000" });
    signal({ state: "started", participant: sam, expiresAtMs: "9000" });
    expect(result.current.participants).toEqual([alex, sam]);
    act(() => vi.advanceTimersByTime(5000));
    expect(result.current.participants).toEqual([sam]);
    act(() => vi.advanceTimersByTime(4000));
    expect(result.current.visible).toBe(false);
  });

  it("drops a signal whose participant is not on the roster", () => {
    vi.useFakeTimers(); vi.setSystemTime(0);
    const { result, signal } = groupDisplay();
    signal({ state: "started", participant: jo, expiresAtMs: "5000" });
    signal({ state: "started", expiresAtMs: "5000" });
    expect(result.current.visible).toBe(false);
    expect(result.current.participants).toEqual([]);
  });

  it("stops only the participant named by a stopped signal", () => {
    vi.useFakeTimers(); vi.setSystemTime(0);
    const { result, signal } = groupDisplay();
    signal({ state: "started", participant: alex, expiresAtMs: "5000" });
    signal({ state: "started", participant: sam, expiresAtMs: "5000" });
    signal({ state: "stopped", participant: alex });
    expect(result.current.participants).toEqual([sam]);
  });

  it("removes only the incoming sender when they send a message", () => {
    vi.useFakeTimers(); vi.setSystemTime(0);
    const { result, signal, rerender } = groupDisplay();
    signal({ state: "started", participant: alex, expiresAtMs: "5000" });
    signal({ state: "started", participant: sam, expiresAtMs: "5000" });
    rerender({ members: roster, incomingSender: alex });
    expect(result.current.participants).toEqual([sam]);
  });

  it("keeps typing state when the caller passes a new members array with the same people", () => {
    vi.useFakeTimers(); vi.setSystemTime(0);
    const { result, signal, rerender, subscribeCount } = groupDisplay();
    signal({ state: "started", participant: alex, expiresAtMs: "5000" });
    rerender({ members: [...roster] });
    rerender({ members: [...roster] });
    expect(result.current.participants).toEqual([alex]);
    expect(subscribeCount()).toBe(1);
  });

  it("builds the label from the participants the hook reports", () => {
    vi.useFakeTimers(); vi.setSystemTime(0);
    const names: Record<string, string> = { [alex]: "Alex", [sam]: "Sam", [jo]: "Jo" };
    const { result, signal, rerender } = groupDisplay([alex, sam, jo]);
    signal({ state: "started", participant: alex, expiresAtMs: "5000" });
    signal({ state: "started", participant: sam, expiresAtMs: "5000" });
    expect(groupTypingLabel(result.current.participants, (address) => names[address])).toBe("Alex and Sam are typing");
    signal({ state: "started", participant: jo, expiresAtMs: "5000" });
    expect(groupTypingLabel(result.current.participants, (address) => names[address])).toBe("3 people are typing");
    rerender({ members: [alex, sam, jo] });
  });
});
