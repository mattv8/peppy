import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot } from "./bridge/bridge";

let snapshot: DesktopSnapshot;

beforeEach(async () => {
  vi.restoreAllMocks();
  snapshot = { ...structuredClone(await fixtureBridge.load_state()), mode: "native" };
  vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => ({
    ...structuredClone(snapshot),
    activeConversationId: conversationId ?? snapshot.activeConversationId,
  }));
  vi.spyOn(bridge, "subscribe").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  vi.spyOn(bridge, "mark_all_read").mockResolvedValue("2");
  vi.spyOn(bridge, "search_local").mockImplementation(fixtureBridge.search_local);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("App mark-all-read titlebar action", () => {
  it("derives availability from the full conversation snapshot rather than mirrored notifications", async () => {
    snapshot = {
      ...snapshot,
      conversations: snapshot.conversations.map(conversation => ({ ...conversation, unread: 0 })),
    };
    render(<App hostKind="browser" />);

    const action = await screen.findByRole("button", { name: "Mark all as read" });
    expect(action).toBeDisabled();
    expect(document.getElementById("titlebar-actions")).toContainElement(action);
  });

  it("marks the full snapshot count after filtering and keeps its toast in the visible thread list", async () => {
    snapshot = {
      ...snapshot,
      notifications: [],
      conversations: snapshot.conversations.map((conversation, index) => ({ ...conversation, unread: index === 0 ? 2 : 0 })),
    };
    vi.mocked(bridge.mark_all_read).mockImplementation(async () => {
      snapshot = {
        ...snapshot,
        conversations: snapshot.conversations.map(conversation => ({ ...conversation, unread: 0 })),
      };
      return "2";
    });
    render(<App hostKind="browser" />);

    await screen.findByRole("button", { name: "Mark all as read" });
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "River" } });
    const action = screen.getByRole("button", { name: "Mark all as read" });
    fireEvent.click(action);
    expect(screen.getByRole("dialog")).toHaveTextContent("2 unread messages");
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Mark all as read" }));

    await waitFor(() => expect(bridge.mark_all_read).toHaveBeenCalledOnce());
    await waitFor(() => expect(screen.getByRole("button", { name: "Mark all as read" })).toBeDisabled());
    const notice = document.getElementById("thread-list-action-notice")!;
    expect(notice).toHaveTextContent(/Marked/);
    expect(document.getElementById("thread-list")).toContainElement(notice);
  });
});
