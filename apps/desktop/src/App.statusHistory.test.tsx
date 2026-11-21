import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge/bridge";

let snapshot: DesktopSnapshot;

beforeEach(async () => {
  vi.restoreAllMocks();
  snapshot = { ...structuredClone(await fixtureBridge.load_state()), mode: "browser", encryption: { state: "unlocked" } };
  vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => ({
    ...structuredClone(snapshot),
    activeConversationId: conversationId ?? snapshot.activeConversationId,
  }));
  vi.spyOn(bridge, "subscribe").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  vi.spyOn(bridge, "search_local").mockImplementation(fixtureBridge.search_local);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

async function openStatus() {
  render(<App hostKind="browser" />);
  fireEvent.click(await screen.findByRole("button", { name: /^Status: Connection:/ }));
  return screen.getByRole("region", { name: "Status details" });
}

describe("status history disclosure", () => {
  const progress = { phase: "receiving" as const, receivedThrough: "0", processedThrough: "0", snapshotReceived: "38400", snapshotExpected: "108708" };

  it("labels the updating row as Messages update", async () => {
    snapshot = { ...snapshot, syncProgress: { ...progress, phase: "updating" } };
    const row = (await openStatus()).querySelector('[data-disclosure="history"]');
    expect(row?.querySelector("dt")).toHaveTextContent("Messages update");
  });

  it.each([
    [progress, "receiving", `Loading ${new Intl.NumberFormat().format(38400)} of ${new Intl.NumberFormat().format(108708)} items`],
    [{ ...progress, phase: "applying" as const }, "applying", "Organizing"],
    [{ ...progress, snapshotReceived: undefined, snapshotExpected: undefined }, "receiving", "Loading"],
  ])("shows history for $phase", async (syncProgress, phase, value) => {
    snapshot = { ...snapshot, syncProgress };
    const row = (await openStatus()).querySelector('[data-disclosure="history"]');
    expect(row).toHaveAttribute("data-history-phase", phase);
    expect(row?.querySelector("dt")).toHaveTextContent("History");
    expect(row?.querySelector("dd")).toHaveTextContent(value);
  });

  it("hides history when current", async () => {
    snapshot = { ...snapshot, syncProgress: { ...progress, phase: "current" } };
    expect((await openStatus()).querySelector('[data-disclosure="history"]')).toBeNull();
  });

  it("hides history while waiting for keys", async () => {
    snapshot = { ...snapshot, syncProgress: { ...progress, phase: "waitingForKeys" } };
    expect((await openStatus()).querySelector('[data-disclosure="history"]')).toBeNull();
  });

  it("shows receiving history during an encryption mismatch", async () => {
    snapshot = { ...snapshot, encryption: { state: "mismatch" }, syncProgress: progress };
    expect((await openStatus()).querySelector('[data-disclosure="history"]')).toHaveAttribute("data-history-phase", "receiving");
  });

  it("hides history when locked", async () => {
    snapshot = { ...snapshot, encryption: { state: "locked" }, syncProgress: progress };
    expect((await openStatus()).querySelector('[data-disclosure="history"]')).toBeNull();
  });
});
