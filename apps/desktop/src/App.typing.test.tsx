import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
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
    const setSharing = vi.spyOn(bridge, "set_typing_sharing").mockResolvedValue();
    render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    expect(setSharing).toHaveBeenCalledWith({ enabled: true });
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

  it("reverts a rejected host preference write and reports the catalog error", async () => {
    const setSharing = vi.spyOn(bridge, "set_typing_sharing").mockResolvedValue();
    render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    setSharing.mockClear();
    setSharing.mockRejectedValueOnce(new Error("rejected"));
    openSettings();
    const setting = await screen.findByRole<HTMLInputElement>("checkbox", { name: /Show others/ });
    fireEvent.click(setting);
    expect(await screen.findByRole("alert")).toHaveAttribute("id", "settings-share-typing-error");
    expect(setting).toBeChecked();
    expect(JSON.parse(localStorage.getItem("peppy.layout.v1")!).shareTyping).toBe(true);
    expect(setSharing).toHaveBeenCalledTimes(1);
  });

  it("submits each local sharing toggle to the host exactly once", async () => {
    const setSharing = vi.spyOn(bridge, "set_typing_sharing").mockResolvedValue();
    render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    openSettings();
    const setting = await screen.findByRole<HTMLInputElement>("checkbox", { name: /Show others/ });
    setSharing.mockClear();

    fireEvent.click(setting);
    await waitFor(() => expect(setting).not.toBeChecked());
    expect(setSharing).toHaveBeenCalledExactlyOnceWith({ enabled: false });
    setSharing.mockClear();
    fireEvent.click(setting);
    await waitFor(() => expect(setting).toBeChecked());
    expect(setSharing).toHaveBeenCalledExactlyOnceWith({ enabled: true });
  });

  it("renders a current chat typing indicator from the bridge", async () => {
    let receive: (signal: { conversationId: string; state: "started" | "stopped"; expiresAtMs?: string }) => void = () => undefined;
    vi.spyOn(bridge, "subscribe_typing").mockImplementation(listener => {
      receive = listener;
      return () => undefined;
    });
    render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    await waitFor(() => expect(document.getElementById("conversation-rcs-badge")).toBeInTheDocument());
    act(() => receive({ conversationId: snapshot.activeConversationId!, state: "started" }));
    expect(document.getElementById("typing-indicator")).toHaveAttribute("data-typing-visible", "true");
  });

  it.each([false, true])("uses actual local activity, not a restored draft, for announcements (active=%s)", async (active) => {
    snapshot.draft!.text = "restored draft";
    let receive: (signal: { conversationId: string; state: "started" | "stopped"; expiresAtMs?: string }) => void = () => undefined;
    vi.spyOn(bridge, "subscribe_typing").mockImplementation(listener => {
      receive = listener;
      return () => undefined;
    });
    vi.spyOn(bridge, "send_typing").mockResolvedValue();
    render(<App />);
    const textarea = await screen.findByRole("textbox", { name: "Message" });
    await waitFor(() => expect(textarea).toHaveValue("restored draft"));
    await waitFor(() => expect(document.getElementById("conversation-rcs-badge")).toBeInTheDocument());
    expect(document.getElementById("shared-composer")).toHaveAttribute("data-typing-send", "idle");
    if (active) fireEvent.change(textarea, { target: { value: "restored draft edited" } });

    act(() => receive({ conversationId: snapshot.activeConversationId!, state: "started" }));

    expect(document.getElementById("typing-indicator")).toHaveAttribute("data-typing-visible", "true");
    if (active) expect(document.getElementById("typing-announcer")).toBeEmptyDOMElement();
    else expect(document.getElementById("typing-announcer")).not.toBeEmptyDOMElement();
  });

  it("sends typing only for the current RCS conversation and stops after sharing is disabled", async () => {
    const sendTyping = vi.spyOn(bridge, "send_typing").mockResolvedValue();
    const setSharing = vi.spyOn(bridge, "set_typing_sharing").mockResolvedValue();
    render(<App />);
    const textarea = await screen.findByRole("textbox", { name: "Message" });
    await waitFor(() => expect(document.getElementById("conversation-rcs-badge")).toBeInTheDocument());
    expect(document.getElementById("shared-composer")).toHaveAttribute("data-typing-send");
    sendTyping.mockClear();
    fireEvent.change(textarea, { target: { value: "hello" } });
    expect(sendTyping).toHaveBeenCalledWith({ conversationId: snapshot.activeConversationId, state: "started" });

    openSettings();
    fireEvent.click(await screen.findByRole("checkbox", { name: /Show others/ }));
    await waitFor(() => expect(setSharing).toHaveBeenCalledWith({ enabled: false }));
    expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: snapshot.activeConversationId, state: "stopped" });
    sendTyping.mockClear();
    fireEvent.change(textarea, { target: { value: "again" } });
    expect(sendTyping).not.toHaveBeenCalled();
  });

  it.each(["settings", "contacts", "notifications"])("stops active typing when switching to %s without a blur", async (view) => {
    const sendTyping = vi.spyOn(bridge, "send_typing").mockResolvedValue();
    render(<App />);
    const textarea = await screen.findByRole("textbox", { name: "Message" });
    sendTyping.mockClear();
    fireEvent.change(textarea, { target: { value: "hello" } });

    fireEvent.click(document.querySelector<HTMLButtonElement>(`[data-rail-item="${view}"]`)!);

    await waitFor(() => expect(sendTyping).toHaveBeenLastCalledWith({ conversationId: snapshot.activeConversationId, state: "stopped" }));
    expect(sendTyping).toHaveBeenCalledTimes(2);
    fireEvent.click(document.querySelector<HTMLButtonElement>('[data-rail-item="conversations"]')!);
    await screen.findByRole("textbox", { name: "Message" });
    expect(sendTyping).toHaveBeenCalledTimes(2);
    cleanup();
    expect(sendTyping).toHaveBeenCalledTimes(2);
  });

  it("suppresses typing when another window disables sharing or the route is not current", async () => {
    const sendTyping = vi.spyOn(bridge, "send_typing").mockResolvedValue();
    const setSharing = vi.spyOn(bridge, "set_typing_sharing").mockResolvedValue();
    render(<App />);
    const textarea = await screen.findByRole("textbox", { name: "Message" });
    setSharing.mockClear();
    localStorage.setItem("peppy.layout.v1", JSON.stringify({ shareTyping: false }));
    act(() => window.dispatchEvent(new StorageEvent("storage", { key: "peppy.layout.v1" })));
    expect(setSharing).toHaveBeenCalledExactlyOnceWith({ enabled: false });
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
