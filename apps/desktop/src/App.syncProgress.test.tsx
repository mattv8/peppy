import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge";

let snapshot: DesktopSnapshot;

beforeEach(async () => {
  vi.restoreAllMocks();
  snapshot = { ...structuredClone(await fixtureBridge.load_state()), mode: "browser" };
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

const syncRegion = () => screen.queryByRole("region", { name: "History sync" });

describe("App sync progress banner", () => {
  it("renders the banner above the desktop body for an active partial sync", async () => {
    snapshot = {
      ...snapshot,
      syncProgress: { phase: "receiving", receivedThrough: "1200", processedThrough: "800", targetHighWater: "2000" },
    };
    render(<App hostKind="browser" />);

    const region = await screen.findByRole("region", { name: "History sync" });
    expect(region).toHaveAttribute("data-sync-phase", "receiving");
    expect(region.querySelector("[data-sync-received]")).toHaveTextContent("1200");
    expect(region.querySelector("[data-sync-processed]")).toHaveTextContent("800");
    expect(region.querySelector("[data-sync-target]")).toHaveTextContent("2000");
    const body = document.getElementById("desktop-body");
    expect(body).not.toBeNull();
    expect(region.compareDocumentPosition(body!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(document.getElementById("desktop-shell")).toContainElement(region);
  });

  it("stays hidden when the snapshot reports current or omits sync progress", async () => {
    snapshot = { ...snapshot, syncProgress: { phase: "current", receivedThrough: "5", processedThrough: "5" } };
    const { unmount } = render(<App hostKind="browser" />);
    await screen.findByRole("toolbar", { name: "Thread list toolbar" });
    expect(syncRegion()).not.toBeInTheDocument();
    unmount();

    snapshot = { ...snapshot, syncProgress: undefined };
    render(<App hostKind="browser" />);
    await screen.findByRole("toolbar", { name: "Thread list toolbar" });
    expect(syncRegion()).not.toBeInTheDocument();
  });

  it("never mounts progress while the vault is locked", async () => {
    snapshot = {
      ...snapshot,
      encryption: { state: "locked" },
      syncProgress: { phase: "waitingForKeys", receivedThrough: "9", processedThrough: "3" },
    };
    render(<App hostKind="browser" />);
    await waitFor(() => expect(document.getElementById("desktop-shell")).not.toBeNull());
    expect(syncRegion()).not.toBeInTheDocument();
  });
});
