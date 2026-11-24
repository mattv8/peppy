import "@testing-library/jest-dom/vitest";
import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../../desktop/src/App";
import * as desktopBridge from "../../desktop/src/bridge/bridge";
import { installBrowserBridge as installDesktopBrowserBridge } from "../../desktop/src/bridge/bridge";
import * as uiBridge from "./ui-bridge";
import type { DesktopBridge, DesktopSnapshot } from "./ui-bridge";

afterEach(() => cleanup());

describe("browser UI bridge", () => {
  it("does not expose typing controls without browser typing capabilities", async () => {
    const browserBridge: DesktopBridge = {
      ...desktopBridge.fixtureBridge,
      load_state: async () => await desktopBridge.fixtureBridge.load_state(),
      subscribe: () => () => undefined,
      subscribe_lifecycle: () => () => undefined,
      subscribe_lifecycle_finished: () => () => undefined,
      set_notification_context: async () => undefined,
      mark_seen: async () => undefined,
    };
    delete browserBridge.send_typing;
    delete browserBridge.set_typing_sharing;
    delete browserBridge.subscribe_typing;
    const previousBridge = uiBridge.bridge;
    const previousDesktopBridge = desktopBridge.bridge;
    uiBridge.installBrowserBridge(browserBridge);
    installDesktopBrowserBridge(browserBridge);
    try {
      render(<App hostKind="browser" />);
      await waitFor(() => expect(document.getElementById("shared-composer")).toBeInTheDocument());
      document.querySelector<HTMLButtonElement>('[data-rail-item="settings"]')?.click();
      await waitFor(() => expect(document.getElementById("settings-view")).toBeInTheDocument());
      expect(document.getElementById("settings-share-typing")).not.toBeInTheDocument();
      expect(document.getElementById("typing-indicator")).toBeNull();
    } finally {
      uiBridge.installBrowserBridge(previousBridge);
      desktopBridge.installBrowserBridge(previousDesktopBridge);
    }
  });

  it("shows the typing sharing setting when browser typing is present and a gateway is linked", async () => {
    const snapshot: DesktopSnapshot = {
      version: "1",
      mode: "browser",
      connection: { state: "connected" },
      encryption: { state: "unlocked" },
      gateways: [{ id: "gateway-browser", name: "Phone", simId: "sim-browser", online: true, simulated: false, supportsSms: true, supportsMms: false }],
      conversations: [{ id: "conversation-browser", name: "Browser chat", preview: "", unread: 0, messages: [] }],
      activeConversationId: "conversation-browser",
      draft: { id: "draft-browser", conversationId: "conversation-browser", text: "", recipientIds: [], attachmentIds: [], gatewayId: "gateway-browser", simId: "sim-browser", revision: "1" },
      head: { enabled: false, capability: "unsupported" },
      pendingCount: 0,
      quarantineCount: 0,
      notifications: [],
      appFilters: [],
      notificationPreferences: { messageBanners: true, mirroredBanners: true, preview: "full" },
      rcsStatus: {
        gateways: [{ gatewayId: "gateway-browser", state: "linked", current: true, available: true, healthy: true, phoneResponsive: true, observedAtMs: "0", capabilityRevision: "1", expiresAtMs: String(Date.now() + 10_000) }],
        eligibilities: [],
      },
    };
    const browserBridge: DesktopBridge = {
      ...desktopBridge.fixtureBridge,
      load_state: async () => snapshot,
      subscribe: () => () => undefined,
      subscribe_lifecycle: () => () => undefined,
      subscribe_lifecycle_finished: () => () => undefined,
      set_notification_context: async () => undefined,
      mark_seen: async () => undefined,
      send_typing: async () => undefined,
      set_typing_sharing: async () => undefined,
      subscribe_typing: () => Object.assign(() => undefined, { ready: Promise.resolve() }),
    };
    const previousBridge = uiBridge.bridge;
    const previousDesktopBridge = desktopBridge.bridge;
    uiBridge.installBrowserBridge(browserBridge);
    installDesktopBrowserBridge(browserBridge);
    try {
      render(<App hostKind="browser" />);
      await waitFor(() => expect(document.getElementById("shared-composer")).toBeInTheDocument());
      document.querySelector<HTMLButtonElement>('[data-rail-item="settings"]')?.click();
      await waitFor(() => expect(document.getElementById("settings-share-typing")).toBeInTheDocument());
    } finally {
      uiBridge.installBrowserBridge(previousBridge);
      desktopBridge.installBrowserBridge(previousDesktopBridge);
    }
  });

  it("passes reported RCS fields from a browser bridge snapshot to the shared renderer", async () => {
    const snapshot: DesktopSnapshot = {
      version: "1",
      mode: "browser",
      connection: { state: "connected" },
      encryption: { state: "unlocked" },
      gateways: [{ id: "gateway-browser", name: "Phone", simId: "sim-browser", online: true, simulated: false, supportsSms: true, supportsMms: false }],
      conversations: [{ id: "conversation-browser", name: "Browser chat", preview: "", unread: 0, messages: [] }],
      activeConversationId: "conversation-browser",
      draft: { id: "draft-browser", conversationId: "conversation-browser", text: "", recipientIds: [], attachmentIds: [], gatewayId: "gateway-browser", simId: "sim-browser", revision: "1" },
      head: { enabled: false, capability: "unsupported" },
      pendingCount: 0,
      quarantineCount: 0,
      notifications: [],
      appFilters: [],
      notificationPreferences: { messageBanners: true, mirroredBanners: true, preview: "full" },
      rcsStatus: {
        gateways: [{ gatewayId: "gateway-browser", state: "linked", current: true, available: true, healthy: true, phoneResponsive: true, observedAtMs: "0", capabilityRevision: "1", expiresAtMs: String(Date.now() + 10_000) }],
        eligibilities: [{ conversationId: "conversation-browser", gatewayId: "gateway-browser", eligibility: "eligible", observedAtMs: "0", mappingRevision: "1", expiresAtMs: String(Date.now() + 10_000) }],
      },
    };
    Object.assign(snapshot.conversations[0], {
      rcsEncryption: true,
      rcsHistoryGap: true,
      messages: [
        { id: "message-browser", revision: "1", sender: "self", body: "", timestamp: "0", status: "unknown", transport: "rcs", rcsReported: false, attachments: [] },
        { id: "message-read", revision: "1", sender: "self", body: "", timestamp: "0", status: "read", statusAt: "1788434520000", transport: "rcs", attachments: [] },
        { id: "message-failed", revision: "1", sender: "self", body: "", timestamp: "0", status: "failed-confirmed", transport: "rcs", rcsReported: true, attachments: [] },
      ],
    });
    const browserBridge = {
      load_state: async () => snapshot,
      subscribe: () => () => undefined,
      subscribe_lifecycle: () => () => undefined,
      subscribe_lifecycle_finished: () => () => undefined,
      set_notification_context: async () => undefined,
      mark_seen: async () => undefined,
    } satisfies Partial<DesktopBridge>;
    const partialBrowserBridge = browserBridge as unknown as DesktopBridge;
    const previousBridge = uiBridge.bridge;
    const previousDesktopBridge = desktopBridge.bridge;
    uiBridge.installBrowserBridge(partialBrowserBridge);
    installDesktopBrowserBridge(partialBrowserBridge);

    try {
      render(<App hostKind="browser" />);

      await waitFor(() => {
        expect(document.getElementById("conversation-rcs-badge")).toBeInTheDocument();
        expect(document.getElementById("conversation-rcs-e2ee-badge")).toBeInTheDocument();
        expect(document.querySelector('[data-message-route-hint="message-browser"]')).toHaveAttribute("data-hint", "unknown");
        expect(document.querySelector('[data-message-read="message-read"]')).toHaveTextContent("Read");
        expect(document.querySelector('[data-message-route-hint="message-failed"]')).toHaveAttribute("data-hint", "failed-reported");
        expect(document.getElementById("conversation-rcs-notice")).not.toBeEmptyDOMElement();
      });
    } finally {
      uiBridge.installBrowserBridge(previousBridge);
      desktopBridge.installBrowserBridge(previousDesktopBridge);
    }
  });
});
