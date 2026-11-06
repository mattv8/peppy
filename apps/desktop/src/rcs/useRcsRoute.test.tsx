import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DesktopSnapshot } from "../bridge/bridge";
import { useRcsRoute } from "./useRcsRoute";

function snapshot(): DesktopSnapshot {
  return {
    version: "1", mode: "native", connection: { state: "connected" }, encryption: { state: "unlocked" },
    gateways: [{ id: "gateway-a", name: "Gateway", simId: "sim-a", online: true, simulated: false, supportsSms: true, supportsMms: true }],
    conversations: [{ id: "conversation-a", name: "Conversation", preview: "", unread: 0, messages: [], participants: ["recipient"] }],
    head: { enabled: false, capability: "unsupported" }, pendingCount: 0, quarantineCount: 0, notifications: [], appFilters: [], notificationPreferences: { messageBanners: true, mirroredBanners: true, preview: "full", muteWithHeads: true },
    rcsStatus: { gateways: [{ gatewayId: "gateway-a", state: "linked", current: true, available: true, healthy: true, phoneResponsive: true, observedAtMs: "1", capabilityRevision: "1", expiresAtMs: "200" }], eligibilities: [{ conversationId: "conversation-a", gatewayId: "gateway-a", eligibility: "eligible", observedAtMs: "1", mappingRevision: "1", expiresAtMs: "200" }] },
  };
}

describe("useRcsRoute", () => {
  beforeEach(() => { vi.useFakeTimers(); vi.setSystemTime(100); });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it("expires at the projection deadline without another render", () => {
    const { result } = renderHook(() => useRcsRoute({ snapshot: snapshot(), activeConversationId: "conversation-a", gatewayId: "gateway-a", locked: false }));
    expect(result.current).toEqual({ current: true, expiresAtMs: 200 });
    act(() => vi.advanceTimersByTime(100));
    expect(result.current).toEqual({ current: false });
  });

  it("does not postpone expiry after an unrelated rerender", () => {
    const value = snapshot();
    const { result, rerender } = renderHook(({ unrelated }) => useRcsRoute({ snapshot: value, activeConversationId: "conversation-a", gatewayId: "gateway-a", locked: false }), { initialProps: { unrelated: 0 } });
    act(() => vi.advanceTimersByTime(50));
    rerender({ unrelated: 1 });
    act(() => vi.advanceTimersByTime(50));
    expect(result.current).toEqual({ current: false });
  });

  it("re-arms when the timer fires before Date.now reaches the deadline", () => {
    const value = snapshot();
    const { result } = renderHook(() => useRcsRoute({ snapshot: value, activeConversationId: "conversation-a", gatewayId: "gateway-a", locked: false }));
    let lagMs = 0;
    vi.spyOn(Date, "now").mockImplementation(() => new Date().getTime() - lagMs);
    lagMs = 1;
    act(() => vi.advanceTimersByTime(100));
    expect(result.current).toEqual({ current: true, expiresAtMs: 200 });
    act(() => vi.advanceTimersByTime(1));
    expect(result.current).toEqual({ current: false });
  });

  it("re-evaluates on focus and drops the route when its conversation changes", () => {
    const value = snapshot();
    const { result, rerender } = renderHook(({ conversationId }) => useRcsRoute({ snapshot: value, activeConversationId: conversationId, gatewayId: "gateway-a", locked: false }), { initialProps: { conversationId: "conversation-a" } });
    vi.setSystemTime(201);
    act(() => window.dispatchEvent(new Event("focus")));
    expect(result.current).toEqual({ current: false });
    vi.setSystemTime(100);
    rerender({ conversationId: "other" });
    expect(result.current).toEqual({ current: false });
  });
});
