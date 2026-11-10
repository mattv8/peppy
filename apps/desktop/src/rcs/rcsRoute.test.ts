import { describe, expect, it } from "vitest";
import type { DesktopSnapshot } from "../bridge/bridge";
import { currentRcsRoute, knownRcsChat, rcsSendBlock } from "./rcsRoute";

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

  it.each([
    ["live", true],
    ["paused", true],
    ["unlinked", false],
    ["proving", false],
    ["withdrawn", false],
  ] as const)("routes a group in state %s: %s", (state, routed) => {
    const value = snapshot();
    value.conversations[0].participants = ["one", "two"];
    value.conversations[0].group = { members: ["one", "two"], state, memberRevision: "1", canCreate: false, createNeedsConfirmation: false };
    const route = currentRcsRoute({ snapshot: value, activeConversationId: "conversation-a", gatewayId: "gateway-a", nowMs: 149, locked: false });

    expect(route.current).toBe(routed);
  });

  it("expires strictly at the projection deadline", () => {
    const value = snapshot();

    expect(currentRcsRoute({ snapshot: value, activeConversationId: "conversation-a", gatewayId: "gateway-a", nowMs: 150, locked: false }))
      .toEqual({ current: false });
  });

  it.each([
    ["locked encryption", (value: DesktopSnapshot) => { value.encryption.state = "locked"; }],
    ["fixture mode", (value: DesktopSnapshot) => { value.mode = "fixture"; }],
    ["recipientless draft", (value: DesktopSnapshot) => { value.conversations[0].recipientlessDraft = true; }],
    ["unlinked gateway", (value: DesktopSnapshot) => { value.rcsStatus!.gateways[0].state = "unlinked"; }],
    ["unknown eligibility", (value: DesktopSnapshot) => { value.rcsStatus!.eligibilities[0].eligibility = "unknown"; }],
    ["negative gateway expiry", (value: DesktopSnapshot) => { value.rcsStatus!.gateways[0].expiresAtMs = "-1"; }],
  ])("rejects %s", (_label, change) => {
    const value = snapshot();
    change(value);
    expect(currentRcsRoute({ snapshot: value, activeConversationId: "conversation-a", gatewayId: "gateway-a", nowMs: 149, locked: false })).toEqual({ current: false });
  });

  it("uses the gateway expiry and ignores observed timestamps", () => {
    const value = snapshot();
    value.rcsStatus!.gateways[0].expiresAtMs = "140";
    value.rcsStatus!.gateways[0].observedAtMs = "999999";
    expect(currentRcsRoute({ snapshot: value, activeConversationId: "conversation-a", gatewayId: "gateway-a", nowMs: 139, locked: false })).toEqual({ current: true, expiresAtMs: 140 });
  });
});

describe("rcsSendBlock", () => {
  const input = (value: DesktopSnapshot) => ({ snapshot: value, activeConversationId: "conversation-a", gatewayId: "gateway-a", nowMs: 149, locked: false });

  it("recognizes only existing route evidence", () => {
    const conversation = snapshot().conversations[0];
    expect(knownRcsChat(conversation)).toBe(false);
    conversation.rcsEncryption = true;
    expect(knownRcsChat(conversation)).toBe(true);
    conversation.rcsEncryption = undefined;
    conversation.rcsHistoryGap = true;
    expect(knownRcsChat(conversation)).toBe(true);
    conversation.rcsHistoryGap = undefined;
    conversation.messages.push({ id: "rcs", revision: "1", sender: "other", body: "", timestamp: "1", attachments: [], transport: "rcs" });
    expect(knownRcsChat(conversation)).toBe(true);
  });

  it.each([
    ["unproven eligibility", (value: DesktopSnapshot) => { value.conversations[0].rcsEncryption = true; value.rcsStatus!.eligibilities[0].eligibility = "unknown"; }, { kind: "unproven" }],
    ["relink", (value: DesktopSnapshot) => { value.conversations[0].rcsEncryption = true; value.rcsStatus!.gateways[0].state = "needs_relink"; }, { kind: "link", help: "relink" }],
    ["phone", (value: DesktopSnapshot) => { value.conversations[0].rcsEncryption = true; value.rcsStatus!.gateways[0].phoneResponsive = false; }, { kind: "link", help: "phone" }],
    ["plain SMS", (_value: DesktopSnapshot) => {}, { kind: "none" }],
    ["unlinked", (value: DesktopSnapshot) => { value.conversations[0].rcsEncryption = true; value.rcsStatus!.gateways[0].state = "unlinked"; }, { kind: "none" }],
    ["group", (value: DesktopSnapshot) => { value.conversations[0].rcsEncryption = true; value.conversations[0].participants = ["one", "two"]; }, { kind: "none" }],
  ])("returns %o for %s", (_label, change, expected) => {
    const value = snapshot();
    change(value);
    expect(rcsSendBlock(input(value))).toEqual(expected);
  });
});
