import { describe, expect, it } from "vitest";
import type { DesktopSnapshot } from "../bridge/bridge";
import { currentRcsRoute } from "./rcsRoute";

function snapshot(): DesktopSnapshot {
  return {
    version: "1",
    mode: "native",
    connection: { state: "connected" },
    encryption: { state: "unlocked" },
    gateways: [{ id: "gateway-a", name: "Gateway", simId: "sim-a", online: true, simulated: false, supportsSms: true, supportsMms: true }],
    conversations: [{ id: "conversation-a", name: "Conversation", preview: "", unread: 0, messages: [], participants: ["recipient"] }],
    head: { enabled: false, capability: "unsupported" },
    pendingCount: 0,
    quarantineCount: 0,
    notifications: [],
    appFilters: [],
    notificationPreferences: { messageBanners: true, mirroredBanners: true, preview: "full", muteWithHeads: true },
    rcsStatus: {
      gateways: [{ gatewayId: "gateway-a", state: "linked", current: true, available: true, healthy: true, phoneResponsive: true, observedAtMs: "1", capabilityRevision: "1", expiresAtMs: "200" }],
      eligibilities: [{ conversationId: "conversation-a", gatewayId: "gateway-a", eligibility: "eligible", observedAtMs: "1", mappingRevision: "1", expiresAtMs: "150" }],
    },
  };
}

describe("currentRcsRoute", () => {
  it("uses the earliest current gateway and conversation eligibility expiry", () => {
    expect(currentRcsRoute({ snapshot: snapshot(), activeConversationId: "conversation-a", gatewayId: "gateway-a", nowMs: 149, locked: false }))
      .toEqual({ current: true, expiresAtMs: 150 });
  });

  it.each([
    ["missing RCS status", (value: DesktopSnapshot) => { value.rcsStatus = undefined; }],
    ["locked application", (_value: DesktopSnapshot, input: { locked: boolean }) => { input.locked = true; }],
    ["draft conversation", (value: DesktopSnapshot) => { value.conversations[0].draftOnly = true; }],
    ["group conversation", (value: DesktopSnapshot) => { value.conversations[0].participants = ["one", "two"]; }],
    ["simulated gateway", (value: DesktopSnapshot) => { value.gateways[0].simulated = true; }],
    ["ineligible conversation", (value: DesktopSnapshot) => { value.rcsStatus!.eligibilities[0].eligibility = "not_eligible"; }],
    ["expired projection", (value: DesktopSnapshot) => { value.rcsStatus!.eligibilities[0].expiresAtMs = "149"; }],
    ["invalid expiry", (value: DesktopSnapshot) => { value.rcsStatus!.eligibilities[0].expiresAtMs = "not-a-number"; }],
  ])("returns no route for %s", (_label, change) => {
    const value = snapshot();
    const input = { snapshot: value, activeConversationId: "conversation-a", gatewayId: "gateway-a", nowMs: 149, locked: false };
    change(value, input);

    expect(currentRcsRoute(input)).toEqual({ current: false });
  });

  it("expires strictly at the projection deadline", () => {
    const value = snapshot();

    expect(currentRcsRoute({ snapshot: value, activeConversationId: "conversation-a", gatewayId: "gateway-a", nowMs: 150, locked: false }))
      .toEqual({ current: false });
  });
});
