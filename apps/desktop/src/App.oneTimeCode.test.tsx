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
  vi.spyOn(bridge, "search_local").mockImplementation(fixtureBridge.search_local);
  vi.spyOn(bridge, "copy_one_time_code").mockResolvedValue();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const codeMessageRow = () => document.querySelector<HTMLElement>('[data-message-id="message-aurora-3"]');

describe("App one-time code copy", () => {
  it("shows Copy code inside an incoming bubble that carries a code", async () => {
    render(<App hostKind="browser" />);

    const bubble = await waitFor(() => {
      const element = codeMessageRow();
      expect(element).not.toBeNull();
      return element!;
    });
    const button = within(bubble).getByRole("button", { name: "Copy code 482913" });
    expect(button).toHaveAttribute("data-one-time-code-button", "message-aurora-3");
    expect(bubble.lastElementChild?.getAttribute("data-one-time-code-action")).toBe("message-aurora-3");
  });

  it("does not show the button on outgoing bubbles or incoming bubbles without a code", async () => {
    render(<App hostKind="browser" />);
    await waitFor(() => expect(codeMessageRow()).not.toBeNull());

    expect(document.querySelectorAll("[data-one-time-code-button]")).toHaveLength(1);
    expect(document.querySelector('[data-message-id="message-aurora-2"] [data-one-time-code-button]')).toBeNull();
    expect(document.querySelector('[data-message-id="message-aurora-1"] [data-one-time-code-button]')).toBeNull();
  });

  it("hides the button when the conversation is in the spam folder", async () => {
    snapshot = {
      ...snapshot,
      conversations: snapshot.conversations.map(conversation => conversation.id === "conv-aurora" ? { ...conversation, folder: "spam" } : conversation),
    };
    render(<App hostKind="browser" />);
    await waitFor(() => expect(codeMessageRow()).not.toBeNull());

    expect(screen.queryByRole("button", { name: /^Copy code 482913$/ })).not.toBeInTheDocument();
  });

  it("copies through the bridge with the message target and shows Copied, without a React-side mark-read", async () => {
    render(<App hostKind="browser" />);
    const button = await screen.findByRole("button", { name: "Copy code 482913" });
    vi.mocked(bridge.mark_seen).mockClear();

    fireEvent.click(button);

    await waitFor(() => expect(bridge.copy_one_time_code).toHaveBeenCalledWith({ kind: "message", messageId: "message-aurora-3" }, "482913"));
    expect(await screen.findByRole("button", { name: "Copied code 482913" })).toBeInTheDocument();
    expect(bridge.mark_seen).not.toHaveBeenCalledWith(["message-aurora-3"]);
  });

  it("shows the generic error when the copy fails", async () => {
    vi.mocked(bridge.copy_one_time_code).mockRejectedValue(Object.assign(new Error("denied"), { code: "clipboard-unavailable" }));
    render(<App hostKind="browser" />);
    const button = await screen.findByRole("button", { name: "Copy code 482913" });

    fireEvent.click(button);

    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't copy the code. Try again.");
    expect(screen.queryByText("Copied")).not.toBeInTheDocument();
  });
});

describe("App one-time code copy from Notifications", () => {
  const openNotifications = async () => {
    render(<App hostKind="browser" />);
    await screen.findByRole("button", { name: "Copy code 482913" });
    fireEvent.click(screen.getByRole("button", { name: /^Notifications/ }));
    return screen.findByRole("button", { name: "Copy code 839201 from Chat" });
  };

  it("copies with the notification target, then refreshes the snapshot", async () => {
    const button = await openNotifications();
    const loadsBeforeCopy = vi.mocked(bridge.load_state).mock.calls.length;

    fireEvent.click(button);

    await waitFor(() => expect(bridge.copy_one_time_code).toHaveBeenCalledWith({ kind: "notification", target: snapshot.notifications.find(item => item.target.notificationKey === "chat-43")!.target }, "839201"));
    expect(await screen.findByRole("button", { name: "Copied code 839201 from Chat" })).toBeInTheDocument();
    await waitFor(() => expect(vi.mocked(bridge.load_state).mock.calls.length).toBeGreaterThan(loadsBeforeCopy));
  });

  it("keeps the copy successful when the follow-up refresh rejects", async () => {
    const button = await openNotifications();
    vi.mocked(bridge.load_state).mockImplementationOnce(() => Promise.reject(new Error("refresh unavailable")));

    fireEvent.click(button);

    expect(await screen.findByRole("button", { name: "Copied code 839201 from Chat" })).toBeInTheDocument();
    expect(screen.queryByText("Couldn't copy the code. Try again.")).not.toBeInTheDocument();
  });

  it("shows the generic error when the native copy rejects, without refreshing", async () => {
    const button = await openNotifications();
    vi.mocked(bridge.copy_one_time_code).mockRejectedValueOnce(Object.assign(new Error("denied"), { code: "clipboard-unavailable" }));
    const loadsBeforeCopy = vi.mocked(bridge.load_state).mock.calls.length;

    fireEvent.click(button);

    expect(await screen.findByRole("alert")).toHaveTextContent("Couldn't copy the code. Try again.");
    expect(vi.mocked(bridge.load_state).mock.calls.length).toBe(loadsBeforeCopy);
  });
});
