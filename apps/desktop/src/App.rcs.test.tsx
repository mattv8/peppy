import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge/bridge";

let snapshot: DesktopSnapshot;

const currentSnapshot = async () => {
  const base = structuredClone(await fixtureBridge.load_state());
  const conversationId = base.activeConversationId!;
  const gatewayId = base.gateways[0].id;
  const expiresAtMs = String(Date.now() + 10_000);
  return {
    ...base,
    mode: "native" as const,
    gateways: [{ ...base.gateways[0], simulated: false }],
    draft: {
      id: "draft-current",
      conversationId,
      text: "x".repeat(161),
      recipientIds: [],
      attachmentIds: [],
      gatewayId,
      simId: base.gateways[0].simId,
      revision: "1",
    },
    rcsStatus: {
      gateways: [{ gatewayId, state: "linked" as const, current: true, available: true, healthy: true, phoneResponsive: true, observedAtMs: "0", capabilityRevision: "1", expiresAtMs }],
      eligibilities: [{ conversationId, gatewayId, eligibility: "eligible" as const, observedAtMs: "0", mappingRevision: "1", expiresAtMs }],
    },
  };
};

beforeEach(async () => {
  vi.restoreAllMocks();
  const values = new Map<string, string>();
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      removeItem: (key: string) => values.delete(key),
      clear: () => values.clear(),
    },
  });
  snapshot = await currentSnapshot();
  vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => ({
    ...structuredClone(snapshot),
    activeConversationId: conversationId ?? snapshot.activeConversationId,
  }));
  vi.spyOn(bridge, "subscribe").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const expectBadge = async () => {
  await waitFor(() => expect(document.getElementById("conversation-rcs-badge")).toBeInTheDocument());
  return document.getElementById("conversation-rcs-badge")!;
};
const expectNoBadge = () => expect(document.getElementById("conversation-rcs-badge")).toBeNull();

describe("RCS conversation route", () => {
  it("renders a badge only for the current eligible 1:1 chat and hides the SMS counter", async () => {
    render(<App />);

    const badge = await expectBadge();
    expect(badge).toHaveTextContent("RCS");
    expect(badge).toHaveAttribute("title", expect.stringContaining("RCS"));
    expect(badge?.querySelector(".visually-hidden")).toHaveTextContent("RCS");
    expect(document.getElementById("sms-counter")).toBeNull();
  });

  it.each([
    ["locked encryption", (next: DesktopSnapshot) => ({ ...next, encryption: { state: "locked" as const } })],
    ["an ineligible conversation", (next: DesktopSnapshot) => ({ ...next, rcsStatus: { ...next.rcsStatus!, eligibilities: [{ ...next.rcsStatus!.eligibilities[0], eligibility: "not_eligible" as const }] } })],
    ["a stale route", (next: DesktopSnapshot) => ({ ...next, rcsStatus: { ...next.rcsStatus!, eligibilities: [{ ...next.rcsStatus!.eligibilities[0], expiresAtMs: "0" }] } })],
    ["a group", (next: DesktopSnapshot) => ({ ...next, conversations: next.conversations.map((conversation) => conversation.id === next.activeConversationId ? { ...conversation, participants: ["one", "two"] } : conversation) })],
    ["a draft", (next: DesktopSnapshot) => ({ ...next, conversations: next.conversations.map((conversation) => conversation.id === next.activeConversationId ? { ...conversation, draftOnly: true } : conversation) })],
  ])("omits the badge for %s", async (_label, change) => {
    snapshot = change(snapshot);
    render(<App />);

    await screen.findByRole("textbox", { name: "Message" });
    expectNoBadge();
    expect(document.getElementById("sms-counter")).toBeInTheDocument();
  });

  it("omits the badge while the app is locked", async () => {
    vi.spyOn(bridge, "get_app_lock_status").mockResolvedValue({ enabled: true, locked: true, storageAvailable: true, pending: false });
    render(<App />);

    await waitFor(() => expect(bridge.get_app_lock_status).toHaveBeenCalled());
    expectNoBadge();
  });

  it("removes the badge at expiry without another snapshot", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(1000);
    snapshot.rcsStatus = {
      gateways: [{ ...snapshot.rcsStatus!.gateways[0], expiresAtMs: "2000" }],
      eligibilities: [{ ...snapshot.rcsStatus!.eligibilities[0], expiresAtMs: "2000" }],
    };
    render(<App />);
    await act(async () => {});
    expect(document.getElementById("conversation-rcs-badge")).toBeInTheDocument();

    await act(async () => vi.advanceTimersByTimeAsync(1000));

    expectNoBadge();
  });

  it("removes the badge in the same render when switching conversations", async () => {
    render(<App />);
    await expectBadge();

    fireEvent.click(screen.getByRole("button", { name: /River Park/ }));

    await screen.findByText("Attachment received");
    expectNoBadge();
  });

  it.each(["settings", "contacts", "notifications"] as const)('omits the badge on the %s screen', async (view) => {
    render(<App />);
    await expectBadge();

    fireEvent.click(document.querySelector<HTMLButtonElement>(`[data-rail-item="${view}"]`)!);

    await waitFor(() => expect(document.getElementById("conversation-pane")).toHaveAttribute("data-view", view));
    expectNoBadge();
  });

  it("omits the badge in detached composer and head panel windows", async () => {
    window.history.replaceState({}, "", "?window=composer&conversationId=conv-aurora");
    const { unmount } = render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    expectNoBadge();
    unmount();

    window.history.replaceState({}, "", "?window=composer&conversationId=conv-aurora&head=1");
    render(<App />);
    await screen.findByRole("textbox", { name: "Message" });
    expectNoBadge();
  });
});
