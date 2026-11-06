import "@testing-library/jest-dom/vitest";
import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../../desktop/src/App";
import * as desktopBridge from "../../desktop/src/bridge/bridge";
import { installBrowserBridge as installDesktopBrowserBridge } from "../../desktop/src/bridge/bridge";
import { BrowserBridge } from "./browser-bridge";
import * as uiBridge from "./ui-bridge";
import { renewHostedSession } from "./ui-bridge";
import type { DesktopBridge, DesktopRcsSnapshot } from "./ui-bridge";

afterEach(() => cleanup());

describe("browser UI bridge", () => {
  it("rejects native-only claimant discard without reporting a no-op cancellation as success", async () => {
    const sent: unknown[] = [];
    const port = {
      start() {},
      close() {},
      postMessage(message: unknown) { sent.push(message); },
    } as unknown as MessagePort;
    const browserBridge = new BrowserBridge(port);
    uiBridge.installBrowserBridge(browserBridge);

    try {
      expect(uiBridge).toHaveProperty("discardComputerClaimant");
      await expect(uiBridge.discardComputerClaimant()).rejects.toBeInstanceOf(Error);
      expect(sent).toEqual([]);
    } finally {
      browserBridge.dispose();
    }
  });

  it("rejects the native-only hosted-session renewal", async () => {
    await expect(renewHostedSession()).rejects.toThrow("unavailable in the browser");
  });

  it("passes reported RCS fields from a browser bridge snapshot to the shared renderer", async () => {
    const snapshot: DesktopRcsSnapshot = {
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
      messages: [{ id: "message-browser", revision: "1", sender: "self", body: "", timestamp: "0", status: "unknown", transport: "rcs", rcsReported: false, attachments: [] }],
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
        expect(document.getElementById("conversation-rcs-notice")).not.toBeEmptyDOMElement();
      });
    } finally {
      uiBridge.installBrowserBridge(previousBridge);
      desktopBridge.installBrowserBridge(previousDesktopBridge);
    }
  });
});
