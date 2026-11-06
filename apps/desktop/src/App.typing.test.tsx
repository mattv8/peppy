import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge/bridge";

let snapshot: DesktopSnapshot;

async function currentSnapshot() {
  const base = structuredClone(await fixtureBridge.load_state());
  const conversationId = base.activeConversationId!;
  const gatewayId = base.gateways[0].id;
  const expiresAtMs = String(Date.now() + 10_000);
  return {
    ...base,
    mode: "native" as const,
    gateways: [{ ...base.gateways[0], simulated: false }],
    draft: { id: "draft-current", conversationId, text: "", recipientIds: [], attachmentIds: [], gatewayId, simId: base.gateways[0].simId, revision: "1" },
    rcsStatus: {
      gateways: [{ gatewayId, state: "linked" as const, current: true, available: true, healthy: true, phoneResponsive: true, observedAtMs: "0", capabilityRevision: "1", expiresAtMs }],
      eligibilities: [{ conversationId, gatewayId, eligibility: "eligible" as const, observedAtMs: "0", mappingRevision: "1", expiresAtMs }],
    },
  };
}

function openSettings() {
  fireEvent.click(document.querySelector<HTMLButtonElement>('[data-rail-item="settings"]')!);
}

beforeEach(async () => {
  vi.restoreAllMocks();
  const values = new Map<string, string>();
  Object.defineProperty(window, "localStorage", { configurable: true, value: {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
    clear: () => values.clear(),
  } });
  snapshot = await currentSnapshot();
  vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => ({ ...structuredClone(snapshot), activeConversationId: conversationId ?? snapshot.activeConversationId }));
  vi.spyOn(bridge, "subscribe").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("typing sharing", () => {
  it("shows the setting only when a link and typing bridge are available", async () => {
    render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    openSettings();
    expect(await screen.findByRole("checkbox", { name: /Show others/ })).toBeChecked();
    cleanup();

    snapshot.rcsStatus = { ...snapshot.rcsStatus!, gateways: [{ ...snapshot.rcsStatus!.gateways[0], state: "unlinked" }] };
    render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    openSettings();
    expect(screen.queryByRole("checkbox", { name: /Show others/ })).toBeNull();
    cleanup();

    snapshot = await currentSnapshot();
    const sendTyping = bridge.send_typing;
    bridge.send_typing = undefined;
    render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    openSettings();
    expect(screen.queryByRole("checkbox", { name: /Show others/ })).toBeNull();
    bridge.send_typing = sendTyping;
  });

  it("reverts a failed preference write and reports the catalog error", async () => {
    Object.defineProperty(window, "localStorage", { configurable: true, value: { ...window.localStorage, setItem: () => { throw new Error("storage"); } } });
    render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    openSettings();
    const setting = await screen.findByRole<HTMLInputElement>("checkbox", { name: /Show others/ });
    fireEvent.click(setting);
    expect(setting).toBeChecked();
    expect(await screen.findByRole("alert")).toHaveAttribute("id", "settings-share-typing-error");
  });

  it("sends typing only for the current RCS conversation and stops after sharing is disabled", async () => {
    const sendTyping = vi.spyOn(bridge, "send_typing").mockResolvedValue();
    const setSharing = vi.spyOn(bridge, "set_typing_sharing").mockResolvedValue();
    render(<App />);
    const textarea = await screen.findByRole("textbox", { name: "Message" });
    await waitFor(() => expect(document.getElementById("conversation-rcs-badge")).toBeInTheDocument());
    sendTyping.mockClear();
    fireEvent.change(textarea, { target: { value: "hello" } });
    expect(sendTyping).toHaveBeenCalledWith({ conversationId: snapshot.activeConversationId, state: "started" });

    openSettings();
    fireEvent.click(await screen.findByRole("checkbox", { name: /Show others/ }));
    await waitFor(() => expect(setSharing).toHaveBeenCalledWith({ enabled: false }));
    sendTyping.mockClear();
    fireEvent.change(textarea, { target: { value: "again" } });
    expect(sendTyping).not.toHaveBeenCalled();
  });

  it("suppresses typing when another window disables sharing or the route is not current", async () => {
    const sendTyping = vi.spyOn(bridge, "send_typing").mockResolvedValue();
    render(<App />);
    const textarea = await screen.findByRole("textbox", { name: "Message" });
    localStorage.setItem("peppy.layout.v1", JSON.stringify({ shareTyping: false }));
    window.dispatchEvent(new StorageEvent("storage", { key: "peppy.layout.v1" }));
    fireEvent.change(textarea, { target: { value: "hello" } });
    expect(sendTyping).not.toHaveBeenCalled();
    cleanup();

    snapshot.rcsStatus = { ...snapshot.rcsStatus!, eligibilities: [{ ...snapshot.rcsStatus!.eligibilities[0], eligibility: "not_eligible" }] };
    localStorage.setItem("peppy.layout.v1", JSON.stringify({ shareTyping: true }));
    render(<App />);
    fireEvent.change(await screen.findByRole("textbox", { name: "Message" }), { target: { value: "hello" } });
    expect(sendTyping).not.toHaveBeenCalled();
  });
});
