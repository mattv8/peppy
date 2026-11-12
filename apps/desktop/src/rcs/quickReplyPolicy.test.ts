import { describe, expect, it } from "vitest";

import type { DesktopSnapshot } from "../bridge/bridge";
import { rcsSendBlock } from "./rcsRoute";
import fixtures from "../../../../crates/desktop-api/tests/fixtures/notification-rcs-policy.json";

type PolicyFixture = {
  name: string;
  gateway: NonNullable<DesktopSnapshot["rcsStatus"]>["gateways"][number]["state"];
  current?: boolean;
  available?: boolean;
  healthy?: boolean;
  responsive?: boolean;
  eligible?: NonNullable<DesktopSnapshot["rcsStatus"]>["eligibilities"][number]["eligibility"];
  expired?: boolean;
  knownRcs?: boolean;
  expected: "allow" | "phone" | "relink" | "unproven";
};

describe("notification quick-reply RCS policy corpus", () => {
  it.each(fixtures as PolicyFixture[])("applies $name", (fixture) => {
    const nowMs = 100;
    const expiresAtMs = fixture.expired ? "100" : "101";
    const snapshot: DesktopSnapshot = {
      version: "1",
      mode: "native",
      connection: { state: "connected" },
      encryption: { state: "unlocked" },
      gateways: [{ id: "gateway", name: "Gateway", simId: "sim", online: true, simulated: false, supportsSms: true, supportsMms: true }],
      conversations: [{ id: "conversation", name: "Conversation", preview: "", unread: 0, messages: [], participants: ["recipient"], rcsEncryption: fixture.knownRcs === false ? undefined : true }],
      head: { enabled: false, capability: "unsupported" },
      pendingCount: 0,
      quarantineCount: 0,
      notifications: [],
      appFilters: [],
      notificationPreferences: { messageBanners: true, mirroredBanners: true, preview: "full", muteWithHeads: true },
      rcsStatus: {
        gateways: [{ gatewayId: "gateway", state: fixture.gateway, current: fixture.current ?? false, available: fixture.available ?? false, healthy: fixture.healthy ?? false, phoneResponsive: fixture.responsive ?? false, observedAtMs: "1", capabilityRevision: "1", expiresAtMs }],
        eligibilities: [{ conversationId: "conversation", gatewayId: "gateway", eligibility: fixture.eligible ?? "unknown", observedAtMs: "1", mappingRevision: "1", expiresAtMs }],
      },
    };

    const expected = fixture.expected === "allow"
      ? { kind: "none" }
      : fixture.expected === "phone"
        ? { kind: "link", help: "phone" }
        : fixture.expected === "relink"
          ? { kind: "link", help: "relink" }
          : { kind: "unproven" };
    expect(rcsSendBlock({ snapshot, activeConversationId: "conversation", gatewayId: "gateway", nowMs, locked: false })).toEqual(expected);
  });
});
