import "@testing-library/jest-dom/vitest";
import { cleanup, render, waitFor } from "@testing-library/react";
import { createElement } from "react";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../../desktop/src/App";
import { installBrowserBridge as installDesktopBrowserBridge } from "../../desktop/src/bridge/bridge";
import { BrowserBridge } from "./browser-bridge";
import * as uiBridge from "./ui-bridge";
import { renewHostedSession } from "./ui-bridge";
import type { DesktopBridge, DesktopSnapshot } from "./ui-bridge";

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

  it("renders the current RCS badge from a browser bridge snapshot", async () => {
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
    const browserBridge = {
      load_state: async () => snapshot,
      subscribe: () => () => undefined,
      subscribe_lifecycle: () => () => undefined,
      subscribe_lifecycle_finished: () => () => undefined,
      set_notification_context: async () => undefined,
      mark_seen: async () => undefined,
    } as DesktopBridge;
    uiBridge.installBrowserBridge(browserBridge);
    installDesktopBrowserBridge(browserBridge);

    render(createElement(App, { hostKind: "browser" }));

    await waitFor(() => expect(document.getElementById("conversation-rcs-badge")).toBeInTheDocument());
  });
});
