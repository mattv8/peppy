import "@testing-library/jest-dom/vitest";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge/bridge";

let snapshot: DesktopSnapshot;
let stateListener: (() => void) | undefined;

// These timings are defined in SyncProgress.tsx but not exported by the package entry.
const SHOW_DELAY_MS = 500;
const HIDE_LINGER_MS = 3000;

beforeEach(async () => {
  vi.restoreAllMocks();
  snapshot = { ...structuredClone(await fixtureBridge.load_state()), mode: "browser" };
  stateListener = undefined;
  vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => ({
    ...structuredClone(snapshot),
    activeConversationId: conversationId ?? snapshot.activeConversationId,
  }));
  vi.spyOn(bridge, "subscribe").mockImplementation((listener) => {
    stateListener = listener;
    return () => undefined;
  });
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  vi.spyOn(bridge, "search_local").mockImplementation(fixtureBridge.search_local);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.useRealTimers();
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
    expect(within(region).getByText("Loading your history…")).toBeInTheDocument();
    const body = document.getElementById("desktop-body");
    expect(body).not.toBeNull();
    expect(region.compareDocumentPosition(body!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(document.getElementById("desktop-shell")).toContainElement(region);
  });

  it("shows snapshot counts and a progress bar even when replay cursors are zero", async () => {
    snapshot = {
      ...snapshot,
      syncProgress: { phase: "receiving", receivedThrough: "0", processedThrough: "0", snapshotReceived: "1200", snapshotExpected: "108000", snapshotRemaining: "106800" },
    };
    render(<App hostKind="browser" />);

    const region = await screen.findByRole("region", { name: "History sync" });
    expect(region).toHaveAttribute("data-sync-phase", "receiving");
    expect(within(region).getByText("1,200 of 108,000 items")).toBeInTheDocument();
    expect(within(region).getByRole("progressbar")).toHaveAttribute("value", "1200");
  });

  it("shows receiving without a count when no snapshot totals are available", async () => {
    snapshot = { ...snapshot, syncProgress: { phase: "receiving", receivedThrough: "0", processedThrough: "0" } };
    render(<App hostKind="browser" />);

    const region = await screen.findByRole("region", { name: "History sync" });
    expect(within(region).getByText("Loading your history…")).toBeInTheDocument();
    expect(region.querySelector("[data-sync-count]")).not.toBeInTheDocument();
    expect(within(region).queryByRole("progressbar")).not.toBeInTheDocument();
  });

  it("updates on state signals and hides after the current-phase linger", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    snapshot = {
      ...snapshot,
      syncProgress: { phase: "receiving", receivedThrough: "0", processedThrough: "0", snapshotReceived: "1200", snapshotExpected: "108000" },
    };
    render(<App hostKind="browser" />);
    const region = await screen.findByRole("region", { name: "History sync" });
    expect(region).toHaveAttribute("data-sync-phase", "receiving");

    snapshot = { ...snapshot, syncProgress: { phase: "applying", receivedThrough: "0", processedThrough: "0" } };
    await act(async () => stateListener?.());
    await waitFor(() => expect(region).toHaveAttribute("data-sync-phase", "applying"));
    expect(within(region).getByText("Organizing downloaded history…")).toBeInTheDocument();

    snapshot = { ...snapshot, syncProgress: { phase: "current", receivedThrough: "0", processedThrough: "0" } };
    await act(async () => stateListener?.());
    expect(syncRegion()).toBeInTheDocument();
    act(() => vi.advanceTimersByTime(HIDE_LINGER_MS));
    expect(syncRegion()).not.toBeInTheDocument();
  });

  it("suppresses active progress while app lock is locked", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    snapshot = { ...snapshot, mode: "native", syncProgress: { phase: "receiving", receivedThrough: "0", processedThrough: "0" } };
    vi.spyOn(bridge, "get_app_lock_status").mockResolvedValue({ enabled: true, locked: true, storageAvailable: true, pending: false });
    render(<App />);

    await waitFor(() => expect(screen.getByRole("button", { name: "New conversation" })).toBeDisabled());
    act(() => vi.advanceTimersByTime(SHOW_DELAY_MS + 1));
    expect(syncRegion()).not.toBeInTheDocument();
  });

  it("suppresses active progress during setup", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const readySnapshot = snapshot;
    snapshot = {
      ...snapshot,
      conversations: [],
      activeConversationId: undefined,
      connection: { ...snapshot.connection, errorCode: "credentials-required" },
      syncProgress: { phase: "receiving", receivedThrough: "0", processedThrough: "0" },
    };
    const { unmount } = render(<App hostKind="browser" />);

    await waitFor(() => expect(document.querySelector('[data-browser-setup="true"]')).toBeInTheDocument());
    act(() => vi.advanceTimersByTime(SHOW_DELAY_MS + 1));
    expect(syncRegion()).not.toBeInTheDocument();
    unmount();

    snapshot = { ...readySnapshot, syncProgress: snapshot.syncProgress };
    render(<App hostKind="browser" />);
    const region = await screen.findByRole("region", { name: "History sync" });
    expect(region).toHaveAttribute("data-sync-phase", "receiving");
  });

  it("updates the thread list and progress while a recipientless draft is selected", async () => {
    const draftConversation = { id: "draft-new", name: "New message", preview: "Draft", unread: 0, messages: [], draftOnly: true, recipientlessDraft: true };
    snapshot = {
      ...snapshot,
      conversations: [draftConversation],
      activeConversationId: draftConversation.id,
      draft: { id: "draft-1", conversationId: draftConversation.id, text: "", recipientIds: [], attachmentIds: [], revision: "1" },
      syncProgress: { phase: "current", receivedThrough: "0", processedThrough: "0" },
    };
    render(<App hostKind="browser" />);
    await screen.findAllByText("New message");

    snapshot = {
      ...snapshot,
      conversations: [
        { id: "new-aurora", name: "Aurora Incoming", preview: "Hello", unread: 0, messages: [] },
        { id: "new-river", name: "River Incoming", preview: "Hello", unread: 0, messages: [] },
        draftConversation,
      ],
      syncProgress: { phase: "receiving", receivedThrough: "0", processedThrough: "0", snapshotReceived: "1200", snapshotExpected: "108000" },
    };
    await act(async () => stateListener?.());
    expect(await screen.findByText("Aurora Incoming")).toBeInTheDocument();
    expect(await screen.findByText("River Incoming")).toBeInTheDocument();
    expect(document.querySelector("[data-header-title]")).toHaveTextContent("New message");
    expect(await screen.findByRole("region", { name: "History sync" }, { timeout: 2000 })).toHaveAttribute("data-sync-phase", "receiving");
  });

  it("stays hidden when the snapshot reports current or omits sync progress", async () => {
    snapshot = { ...snapshot, syncProgress: { phase: "current", receivedThrough: "5", processedThrough: "5" } };
    const { unmount } = render(<App hostKind="browser" />);
    await screen.findByRole("toolbar", { name: "Conversation controls" });
    expect(syncRegion()).not.toBeInTheDocument();
    unmount();

    snapshot = { ...snapshot, syncProgress: undefined };
    render(<App hostKind="browser" />);
    await screen.findByRole("toolbar", { name: "Conversation controls" });
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
