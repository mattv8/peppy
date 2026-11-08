import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { formatDateTime } from "@peppy/desktop-ui";
import { App } from "./App";
import { bridge, type DesktopSnapshot, type MessageView } from "./bridge/bridge";
import { peppyCopy } from "./generated/peppyCopy";

const SENT_AT = "1788434460000";
const READ_AT = "1788434520000";

const message = (id: string, overrides: Partial<MessageView> = {}): MessageView => ({
  id,
  revision: id,
  sender: "self",
  body: "Hello",
  timestamp: SENT_AT,
  attachments: [],
  transport: "rcs",
  ...overrides,
});

function snapshot(messages: MessageView[]): DesktopSnapshot {
  return {
    version: "1",
    mode: "native",
    connection: { state: "connected", origin: "https://example.test" },
    encryption: { state: "unlocked" },
    gateways: [],
    conversations: [{ id: "aurora", name: "Aurora", preview: "", unread: 0, messages }],
    activeConversationId: "aurora",
    head: { enabled: false, capability: "unsupported" },
    pendingCount: 0,
    quarantineCount: 0,
    notifications: [],
    appFilters: [],
    notificationPreferences: { messageBanners: true, mirroredBanners: true, preview: "full" },
  };
}

function renderMessages(messages: MessageView[]) {
  vi.spyOn(bridge, "load_state").mockImplementation(async () => snapshot(messages));
  render(<App />);
  return async (id: string) => {
    await waitFor(() => expect(document.querySelector(`[data-message-row="${id}"]`)).toBeInTheDocument());
    return document.querySelector<HTMLElement>(`[data-message-meta="${id}"]`);
  };
}

beforeEach(() => {
  vi.restoreAllMocks();
  const storage = new Map<string, string>();
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
      removeItem: (key: string) => storage.delete(key),
      clear: () => storage.clear(),
    },
  });
  vi.spyOn(bridge, "get_app_lock_status").mockResolvedValue({ enabled: false, locked: false, storageAvailable: true, pending: false });
  vi.spyOn(bridge, "subscribe").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  vi.spyOn(bridge, "media_page").mockResolvedValue({ items: [] });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("RCS outgoing message states", () => {
  it.each([
    ["submitted", "Submitted"],
    ["sent", "Sent"],
    ["delivery-confirmed", "Delivery confirmed"],
  ] as const)("renders %s as its compact status without read or failure wording", async (status, label) => {
    const meta = renderMessages([message("row", { status, statusAt: READ_AT })]);
    const element = await meta("row");
    expect(element).toHaveAttribute("data-status", status);
    expect(element!.querySelector(`[aria-label="${label}"]`)).toBeInTheDocument();
    expect(element).not.toHaveTextContent("Read");
    expect(element!.querySelector("[data-message-read]")).toBeNull();
    expect(element!.querySelector("[data-message-route-hint]")).toBeNull();
  });

  it("does not infer delivery or read from a submitted or sent RCS message", async () => {
    const meta = renderMessages([
      message("submitted", { status: "submitted", statusAt: READ_AT }),
      message("sent", { status: "sent", statusAt: READ_AT }),
    ]);
    for (const id of ["submitted", "sent"]) {
      const element = await meta(id);
      expect(within(element!).queryByText("Read")).toBeNull();
      expect(element!.querySelector("[data-message-read]")).toBeNull();
      expect(element!.querySelector('[aria-label="Delivery confirmed"]')).toBeNull();
    }
  });

  it("renders read as visible text with a separator and the formatted statusAt time", async () => {
    const meta = renderMessages([message("read", { status: "read", statusAt: READ_AT })]);
    const element = await meta("read");
    expect(element).toHaveAttribute("data-status", "read");
    expect(element!.querySelector('[data-message-read="read"]')).toHaveTextContent("Read");
    expect(element!.querySelector(".message-meta-separator")).toBeInTheDocument();
    expect(element!.querySelector("time")).toHaveTextContent(formatDateTime(READ_AT)!);
    expect(element!.querySelector("time")).not.toHaveTextContent(formatDateTime(SENT_AT)!);
  });

  it("never falls back to the sent time for read without a valid statusAt", async () => {
    const meta = renderMessages([message("read", { status: "read" })]);
    const element = await meta("read");
    expect(element).toHaveTextContent("Read");
    expect(element!.querySelector("time")).toBeNull();
    expect(element).not.toHaveTextContent(formatDateTime(SENT_AT)!);
  });

  it("shows a Google-attributed hint for failed-confirmed only when Google reported it", async () => {
    const meta = renderMessages([message("reported", { status: "failed-confirmed", rcsReported: true })]);
    const element = await meta("reported");
    const hint = element!.querySelector("[data-message-route-hint='reported']");
    expect(hint).toHaveAttribute("data-hint", "failed-reported");
    expect(hint).toHaveTextContent(peppyCopy.rcs_message_failed_reported);
  });

  it("gives a send-state failure the neutral hint instead of the Google-attributed one", async () => {
    const meta = renderMessages([message("fallback", { status: "failed-confirmed", rcsReported: false })]);
    const element = await meta("fallback");
    const hint = element!.querySelector("[data-message-route-hint='fallback']");
    expect(hint).toHaveAttribute("data-hint", "failed");
    expect(hint).toHaveTextContent(peppyCopy.rcs_message_failed_unreported);
    expect(element).not.toHaveTextContent(peppyCopy.rcs_message_failed_reported);
  });

  it("shows the unknown hint inside the meta row with its status", async () => {
    const meta = renderMessages([message("unknown", { status: "unknown" })]);
    const element = await meta("unknown");
    expect(element).toHaveAttribute("data-status", "unknown");
    expect(element!.querySelector("[data-message-route-hint='unknown']")).toHaveAttribute("data-hint", "unknown");
    expect(element!.querySelector("[data-message-route-hint='unknown']")).toHaveTextContent(peppyCopy.rcs_message_unknown_hint);
  });

  it("sets aria-live off on RCS status lines only", async () => {
    const meta = renderMessages([
      message("rcs", { status: "sent" }),
      message("sms", { status: "sent", transport: "sms" }),
    ]);
    expect(await meta("rcs")).toHaveAttribute("aria-live", "off");
    expect(await meta("sms")).not.toHaveAttribute("aria-live");
  });

  it("renders no route hint for carrier unknown or failed-confirmed messages", async () => {
    const meta = renderMessages([
      message("sms-unknown", { status: "unknown", transport: "sms" }),
      message("sms-failed", { status: "failed-confirmed", transport: "sms" }),
    ]);
    for (const id of ["sms-unknown", "sms-failed"]) {
      const element = await meta(id);
      expect(element!.querySelector("[data-message-route-hint]")).toBeNull();
      expect(element!.querySelector("[aria-live]")).toBeNull();
    }
  });

  it("uses the read label without a clock icon for a non-RCS read status", async () => {
    const meta = renderMessages([message("sms-read", { status: "read", transport: "sms", statusAt: READ_AT })]);
    const element = await meta("sms-read");
    expect(element).toHaveTextContent("Read");
    expect(element!.querySelector("svg.lucide-clock")).toBeNull();
    expect(element!.querySelector(".message-meta-separator")).toBeInTheDocument();
  });

  it("suppresses the phone-history icon for an RCS self message without status", async () => {
    const meta = renderMessages([
      message("sms-history", { transport: "sms" }),
      message("rcs-history"),
    ]);
    const rcs = await meta("rcs-history");
    expect(rcs).toBeInTheDocument();
    expect(rcs!.querySelector("[data-message-phone-icon]")).toBeNull();
    expect((await meta("sms-history"))!.querySelector("[data-message-phone-icon]")).toBeInTheDocument();
  });

  it("renders an incoming RCS message as an ordinary bubble with no status line", async () => {
    const meta = renderMessages([message("incoming", { sender: "other", senderAddress: "+15550001" })]);
    const element = await meta("incoming");
    expect(element).not.toHaveAttribute("data-status");
    expect(element!.querySelector(".message-meta-status")).toBeNull();
  });

  it("renders nothing for an unknown status string instead of an empty span", async () => {
    const meta = renderMessages([message("mystery", { status: "not-a-status" as MessageView["status"], transport: "sms" })]);
    await waitFor(() => expect(document.querySelector('[data-message-row="mystery"]')).toBeInTheDocument());
    expect(document.querySelector('[data-message-meta="mystery"]')).toBeNull();
    expect(document.querySelectorAll(".message-meta-status:empty")).toHaveLength(0);
    expect(await meta("mystery")).toBeNull();
  });

  it("keeps the RCS word out of every message row", async () => {
    renderMessages([
      message("a", { status: "read", statusAt: READ_AT }),
      message("b", { status: "failed-confirmed", rcsReported: true }),
      message("c", { status: "unknown" }),
      message("d", { sender: "other" }),
    ]);
    await waitFor(() => expect(document.querySelector('[data-message-row="d"]')).toBeInTheDocument());
    const rows = Array.from(document.querySelectorAll("[data-message-row]"));
    expect(rows.length).toBeGreaterThan(0);
    for (const row of rows) expect(row.textContent).not.toMatch(/\bRCS\b/);
  });
});
