import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, fixtureReactionConversations, type DesktopSnapshot } from "./bridge/bridge";

let snapshot: DesktopSnapshot;

const rectOf = (top: number, bottom: number, left: number, right: number) => ({ top, bottom, left, right, width: right - left, height: bottom - top, x: left, y: top, toJSON() {} });

/** jsdom has no layout: give the list and every message article a fixed rectangle. */
function stubLayout() {
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
    if (this.id === "message-list") return rectOf(0, 800, 0, 400);
    if (this.id === "composer-area") return rectOf(700, 780, 0, 400);
    if (this.getAttribute("data-message-id")) return rectOf(200, 260, 16, 300);
    return rectOf(0, 0, 0, 0);
  });
}

async function openConversation(conversationId: string) {
  const conversation = fixtureReactionConversations.find((candidate) => candidate.id === conversationId)!;
  snapshot.conversations = [structuredClone(conversation)];
  snapshot.activeConversationId = conversationId;
  vi.mocked(bridge.load_message_window).mockImplementation(async (input) => ({
    conversationId: input.conversationId,
    messages: snapshot.conversations.find((candidate) => candidate.id === input.conversationId)?.messages ?? [],
    hasOlder: false,
    hasNewer: false,
  }));
  render(<App hostKind="browser" />);
  await waitFor(() => expect(document.querySelector("article[data-message-id]")).not.toBeNull());
}

const article = (messageId: string) => document.querySelector<HTMLElement>(`article[data-message-id="${messageId}"]`)!;
const bar = () => document.getElementById("message-reaction-bar");
const keyOpen = (messageId: string) => {
  act(() => article(messageId).focus());
  fireEvent.keyDown(article(messageId), { key: "Enter" });
};

beforeEach(async () => {
  vi.restoreAllMocks();
  snapshot = structuredClone(await fixtureBridge.load_state());
  snapshot.encryption.profileFingerprint = "profile-a";
  vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => ({ ...structuredClone(snapshot), activeConversationId: conversationId ?? snapshot.activeConversationId }));
  vi.spyOn(bridge, "search_local").mockImplementation(fixtureBridge.search_local);
  vi.spyOn(bridge, "load_message_window").mockImplementation(fixtureBridge.load_message_window);
  vi.spyOn(bridge, "subscribe").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  vi.spyOn(bridge, "react_to_message").mockResolvedValue({ accepted: true, route: "native", requestId: "request-1" });
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
  stubLayout();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("reaction layer", () => {
  it("keeps one roving tab stop among reactable messages, defaulting to the newest", async () => {
    await openConversation("conv-reaction-states");
    const stops = [...document.querySelectorAll<HTMLElement>("article[data-reaction-ok]")].filter((node) => node.tabIndex === 0);
    expect(stops.map((node) => node.dataset.messageId)).toEqual(["reaction-unknown"]);
    expect(article("reaction-group-dinner").tabIndex).toBe(-1);
  });

  it("keeps the bar out of the message log and out of the row geometry", async () => {
    await openConversation("conv-reaction-states");
    const row = article("reaction-own-sent").closest<HTMLElement>(".message-row")!;
    const childrenBefore = [...row.children].map((child) => child.className);
    keyOpen("reaction-own-sent");
    expect(bar()).toBeInTheDocument();
    expect(document.getElementById("message-list")!.contains(bar())).toBe(false);
    expect(row.querySelector("#message-reaction-bar")).toBeNull();
    // The bar adds no sibling to the row, so the row's flow is untouched. Real rects need a browser run.
    expect([...row.children].map((child) => child.className)).toEqual(childrenBefore);
  });

  it("points the roving stop at the hint and its badge group", async () => {
    await openConversation("conv-reaction-states");
    expect(article("reaction-unknown")).toHaveAttribute("aria-describedby", "message-reaction-hint reactions-reaction-unknown");
    expect(article("reaction-group-dinner")).not.toHaveAttribute("aria-describedby");
  });

  it("draws a photo's reaction badges over the photo with overlay placement", async () => {
    await openConversation("conv-reaction-states");
    const overlay = document.querySelector<HTMLElement>('[data-reaction-message="reaction-photo"] .reaction-badges-overlay, [data-media-message-content] .reaction-badges-overlay');
    expect(overlay).not.toBeNull();
    expect(overlay).toHaveAttribute("data-reaction-placement", "overlay-bottom-start");
    expect(overlay!.querySelector('[data-reaction-emoji="😍"]')).not.toBeNull();
  });

  it("opens on a real pointer hover over a message, through the list content listeners", async () => {
    await openConversation("conv-reaction-states");
    const target = article("reaction-group-dinner").querySelector("p") ?? article("reaction-group-dinner");
    // A mouse hover, as the browser dispatches it: pointerover bubbles from the text into the article.
    fireEvent(target, Object.assign(new Event("pointerover", { bubbles: true }), { pointerType: "mouse" }));
    await waitFor(() => expect(bar()).toHaveAttribute("data-reaction-bar", "reaction-group-dinner"));
  });

  it("opens from the keyboard onto the user's current native reaction and returns focus on Escape", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-own-sent");
    const selected = document.querySelector<HTMLElement>('[data-reaction-quick="laughing"]')!;
    expect(selected).toHaveAttribute("aria-pressed", "true");
    expect(document.activeElement).toBe(selected);
    fireEvent.keyDown(selected, { key: "Escape" });
    expect(bar()).toBeNull();
    expect(document.activeElement).toBe(article("reaction-own-sent"));
  });

  it("switches through the quick row, then shows the pending request as busy", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-own-sent");
    fireEvent.click(document.querySelector('[data-reaction-quick="thumbs-up"]')!);
    expect(bridge.react_to_message).toHaveBeenLastCalledWith({ messageId: "reaction-own-sent", emoji: "👍" });
    await waitFor(() => expect(document.getElementById("reaction-announcer")).toHaveTextContent("Changing your reaction to 👍."));
    // The optimistic request holds the bar on busy until a snapshot carries it or the outcome.
    await act(async () => {});
    keyOpen("reaction-own-sent");
    expect(document.querySelector("[data-reaction-bar-notice='busy']")).toHaveTextContent("Sending your reaction…");
    expect(document.querySelector("[data-reaction-quick]")).toBeNull();
  });

  it("removes through the selected quick button with a null emoji", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-own-sent");
    fireEvent.click(document.querySelector('[data-reaction-quick="laughing"]')!);
    expect(bridge.react_to_message).toHaveBeenLastCalledWith({ messageId: "reaction-own-sent", emoji: null });
    await waitFor(() => expect(document.getElementById("reaction-announcer")).toHaveTextContent("Removing your 😂 reaction."));
  });

  it("adds a reaction the message did not have", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector('[data-reaction-quick="surprised"]')!);
    expect(bridge.react_to_message).toHaveBeenLastCalledWith({ messageId: "reaction-group-dinner", emoji: "😮" });
    await waitFor(() => expect(document.getElementById("reaction-announcer")).toHaveTextContent("Sending your 😮 reaction."));
  });

  it("takes the first emoji from the capture field and rejects text", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector("[data-reaction-more]")!);
    const field = screen.getByRole("textbox", { name: "Emoji reaction" });
    expect(field).toHaveFocus();
    fireEvent.input(field, { target: { value: "hello" } });
    await waitFor(() => expect(document.getElementById("reaction-announcer")).toHaveTextContent("Pick an emoji, not text."));
    expect(bridge.react_to_message).not.toHaveBeenCalled();
    fireEvent.input(field, { target: { value: "👍🏽x" } });
    expect(bridge.react_to_message).toHaveBeenCalledWith({ messageId: "reaction-group-dinner", emoji: "👍🏽" });
  });

  it("commits nothing while an IME composition is open", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector("[data-reaction-more]")!);
    const field = screen.getByRole("textbox", { name: "Emoji reaction" });
    fireEvent.input(field, { target: { value: "👍" }, isComposing: true });
    expect(bridge.react_to_message).not.toHaveBeenCalled();
    fireEvent.compositionEnd(field, { target: { value: "👍" } });
    expect(bridge.react_to_message).toHaveBeenCalledWith({ messageId: "reaction-group-dinner", emoji: "👍" });
  });

  it("does not call the OS picker in the browser, where the bridge has none", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector("[data-reaction-more]")!);
    expect(screen.getByText(/Press Control\+Command\+Space|Press Windows key \+ period|Right-click this field/)).toBeInTheDocument();
  });

  it("uses a text-route bar with no selected state for a text conversation", async () => {
    await openConversation("conv-reaction-text");
    keyOpen("reaction-text-incoming");
    expect(bar()).toHaveAttribute("data-reaction-route", "text");
    expect(document.querySelector('[data-reaction-route-label="text"]')).toHaveTextContent("Text");
    expect(document.querySelector('[data-reaction-quick="thumbs-up"]')).not.toHaveAttribute("aria-pressed");
    fireEvent.click(document.querySelector('[data-reaction-quick="thumbs-up"]')!);
    expect(bridge.react_to_message).toHaveBeenCalledWith({ messageId: "reaction-text-incoming", emoji: "👍" });
  });

  it("shows the offline pill in place of the buttons when the route's phone is offline", async () => {
    await openConversation("conv-phone-offline");
    keyOpen("reaction-offline");
    expect(bar()).toHaveAttribute("data-reaction-mode", "notice");
    expect(document.querySelector("[data-reaction-bar-notice='offline']")).toHaveTextContent("Phone is offline. Check it and try again.");
    expect(document.querySelector("[data-reaction-quick]")).toBeNull();
  });

  it("shows the update pill for an RCS chat whose phone has not advertised support", async () => {
    await openConversation("conv-needs-update");
    keyOpen("reaction-needs-update");
    expect(document.querySelector("[data-reaction-bar-notice='update-needed']")).toHaveTextContent("Update Peppy on your phone to react in this chat.");
    expect(document.activeElement).toBe(article("reaction-needs-update"));
    await waitFor(() => expect(document.getElementById("reaction-announcer")).toHaveTextContent("Update Peppy on your phone to react in this chat."));
  });

  it("blocks new reactions behind an unknown attempt until it is dismissed", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-unknown");
    expect(document.querySelector("[data-reaction-bar-notice='unconfirmed']")).toHaveTextContent("Dismiss your earlier reaction first.");
    expect(bridge.react_to_message).not.toHaveBeenCalled();
  });

  it("announces a refused submit in the alert region", async () => {
    vi.mocked(bridge.react_to_message!).mockResolvedValueOnce({ accepted: false, reason: "busy" });
    await openConversation("conv-reaction-states");
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector('[data-reaction-quick="heart"]')!);
    await waitFor(() => expect(document.getElementById("reaction-alert")).toHaveTextContent("Sending your reaction…"));
  });
});
