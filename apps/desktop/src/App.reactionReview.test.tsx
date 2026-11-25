import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, fixtureReactionConversations, type DesktopSnapshot } from "./bridge/bridge";

let snapshot: DesktopSnapshot;
/** The app's refresh listener: calling it delivers the current `snapshot` to the app, as the core's change event does. */
let publishSnapshot: (() => void) | undefined;
const publish = () => act(async () => { publishSnapshot?.(); await Promise.resolve(); });

const rectOf = (top: number, bottom: number, left: number, right: number) => ({ top, bottom, left, right, width: right - left, height: bottom - top, x: left, y: top, toJSON() {} });

function stubLayout() {
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
    if (this.id === "message-list") return rectOf(0, 800, 0, 400);
    if (this.id === "composer-area") return rectOf(700, 780, 0, 400);
    if (this.getAttribute("data-message-id") || this.getAttribute("data-reaction-message") || this.hasAttribute("data-media-message-content") || this.hasAttribute("data-media-frame")) return rectOf(200, 260, 16, 300);
    return rectOf(0, 0, 0, 0);
  });
}

async function openConversation(conversationId: string, otherConversationId?: string) {
  const conversation = fixtureReactionConversations.find((candidate) => candidate.id === conversationId)!;
  snapshot.conversations = [structuredClone(conversation)];
  if (otherConversationId) snapshot.conversations.push(structuredClone(fixtureReactionConversations.find((candidate) => candidate.id === otherConversationId)!));
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
  publishSnapshot = undefined;
  vi.spyOn(bridge, "subscribe").mockImplementation((listener) => { publishSnapshot = listener; return () => undefined; });
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

describe("finding 1: one submit at a time, on both routes", () => {
  it("returns focus to the message when a focused own chip is removed", async () => {
    await openConversation("conv-reaction-states");
    vi.mocked(bridge.react_to_message!).mockReturnValueOnce(new Promise(() => undefined));
    const chip = document.querySelector<HTMLElement>('[data-reaction-badges="reaction-own-sent"] [data-reaction-own="true"][data-reaction-state="sent"]')!;
    chip.focus();
    fireEvent.click(chip);
    expect(article("reaction-own-sent")).toHaveFocus();
    expect(document.querySelector('[data-reaction-badges="reaction-own-sent"] [data-reaction-state="removing"]')).not.toBeNull();
  });

  it("keeps exactly one pending own text chip when the outgoing text arrives during the call", async () => {
    await openConversation("conv-reaction-text");
    vi.mocked(bridge.react_to_message!).mockReturnValueOnce(new Promise(() => undefined));
    keyOpen("reaction-text-incoming");
    fireEvent.click(document.querySelector('[data-reaction-quick="thumbs-up"]')!);
    snapshot.conversations[0].messages.push({ id: "reaction-new-text", revision: "1", sender: "self", body: "Reacted 👍 to “Running late, start without me”", timestamp: "1788434680000", transport: "sms", status: "queued-local", attachments: [] });
    await publish();
    const chips = document.querySelectorAll('[data-reaction-badges="reaction-text-incoming"] [data-reaction-own="true"]');
    expect(chips).toHaveLength(1);
    expect(chips[0]).toHaveAttribute("data-reaction-state", "pending");
    expect(chips[0]).toHaveAttribute("data-reaction-source", "text");
  });

  it("does not announce when an accepted text overlay clears", async () => {
    await openConversation("conv-reaction-text");
    vi.mocked(bridge.react_to_message!).mockResolvedValueOnce({ accepted: true, route: "text" });
    keyOpen("reaction-text-incoming");
    fireEvent.click(document.querySelector('[data-reaction-quick="thumbs-up"]')!);
    await waitFor(() => expect(document.getElementById("reaction-announcer")).toHaveTextContent("Sending your 👍 as a text message."));
    const announcer = document.getElementById("reaction-announcer")!;
    const observer = vi.fn();
    const mutations = new MutationObserver(observer);
    mutations.observe(announcer, { childList: true, characterData: true, subtree: true });
    await publish();
    await act(async () => { await new Promise<void>((resolve) => requestAnimationFrame(() => resolve())); });
    expect(observer).not.toHaveBeenCalled();
    mutations.disconnect();
  });

  it.each([
    ["conv-reaction-states", "reaction-group-dinner", "conv-reaction-text", "reaction-text-incoming"],
    ["conv-reaction-text", "reaction-text-incoming", "conv-reaction-states", "reaction-group-dinner"],
  ])("retains the pending chip and busy bar after leaving and returning to %s", async (conversationId, messageId, otherId, otherMessageId) => {
    await openConversation(conversationId, otherId);
    vi.mocked(bridge.react_to_message!).mockReturnValueOnce(new Promise(() => undefined));
    keyOpen(messageId);
    fireEvent.click(document.querySelector('[data-reaction-quick="thumbs-up"]')!);
    expect(document.querySelector(`[data-reaction-badges="${messageId}"] [data-reaction-state="pending"]`)).not.toBeNull();
    fireEvent.click(document.querySelector(`[data-conversation-id="${otherId}"]`)!);
    await waitFor(() => expect(document.querySelector(`article[data-message-id="${otherMessageId}"]`)).not.toBeNull());
    fireEvent.click(document.querySelector(`[data-conversation-id="${conversationId}"]`)!);
    await waitFor(() => expect(document.querySelector(`article[data-message-id="${messageId}"]`)).not.toBeNull());
    expect(document.querySelector(`[data-reaction-badges="${messageId}"] [data-reaction-state="pending"]`)).not.toBeNull();
    keyOpen(messageId);
    expect(document.querySelector("[data-reaction-bar-notice='busy']")).not.toBeNull();
    expect(bridge.react_to_message).toHaveBeenCalledTimes(1);
  });

  it("shows the native pending chip before the bridge call settles", async () => {
    await openConversation("conv-reaction-states");
    vi.mocked(bridge.react_to_message!).mockReturnValueOnce(new Promise(() => undefined));
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector('[data-reaction-quick="heart"]')!);
    expect(document.querySelector('[data-reaction-badges="reaction-group-dinner"] [data-reaction-state="pending"]')).not.toBeNull();
    keyOpen("reaction-group-dinner");
    expect(document.querySelector("[data-reaction-bar-notice='busy']")).not.toBeNull();
  });

  it("shows a pending text chip before the bridge call settles", async () => {
    await openConversation("conv-reaction-text");
    vi.mocked(bridge.react_to_message!).mockReturnValueOnce(new Promise(() => undefined));
    keyOpen("reaction-text-incoming");
    fireEvent.click(document.querySelector('[data-reaction-quick="thumbs-up"]')!);
    expect(document.querySelector('[data-reaction-badges="reaction-text-incoming"] [data-reaction-source="text"][data-reaction-state="pending"]')).not.toBeNull();
  });

  it("does not send a second text reaction when the bar reopens during an unresolved text submit", async () => {
    await openConversation("conv-reaction-text");
    // The first text submit never settles, as a slow phone would leave it.
    vi.mocked(bridge.react_to_message!).mockReturnValueOnce(new Promise(() => undefined));
    keyOpen("reaction-text-incoming");
    fireEvent.click(document.querySelector('[data-reaction-quick="thumbs-up"]')!);
    expect(bridge.react_to_message).toHaveBeenCalledTimes(1);
    await act(async () => {});

    keyOpen("reaction-text-incoming");
    expect(document.querySelector("[data-reaction-bar-notice='busy']")).toHaveTextContent("Sending your reaction…");
    expect(document.querySelector("[data-reaction-quick]")).toBeNull();
    expect(bridge.react_to_message).toHaveBeenCalledTimes(1);
  });
});

describe("finding 2: rejected calls are reported once and never retried", () => {
  it("announces an unreachable Peppy in the assertive region after a rejected submit, and frees the bar", async () => {
    await openConversation("conv-reaction-states");
    vi.mocked(bridge.react_to_message!).mockRejectedValueOnce(new Error("bridge down"));
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector('[data-reaction-quick="heart"]')!);
    await waitFor(() => expect(document.getElementById("reaction-alert")).toHaveTextContent("Couldn't reach Peppy. Try again."));
    expect(document.getElementById("reaction-alert")).toHaveAttribute("aria-live", "assertive");
    expect(bridge.react_to_message).toHaveBeenCalledTimes(1);
    await act(async () => {});
    keyOpen("reaction-group-dinner");
    expect(document.querySelector("[data-reaction-quick]")).not.toBeNull();
  });

  it("announces an unreachable Peppy when dismissing a request is rejected, and does not retry the dismiss", async () => {
    await openConversation("conv-reaction-states");
    const dismiss = vi.spyOn(bridge, "dismiss_reaction_request").mockRejectedValueOnce(new Error("bridge down"));
    fireEvent.click(document.querySelector('[data-reaction-state="unknown"]')!);
    fireEvent.click(await screen.findByRole("button", { name: "Dismiss" }));
    await waitFor(() => expect(document.getElementById("reaction-alert")).toHaveTextContent("Couldn't reach Peppy. Try again."));
    expect(dismiss).toHaveBeenCalledTimes(1);
  });
});

describe("finding 3: a message's badges render once, however many photos it has", () => {
  it("declares the R shortcut on a reacted photo's slide and on a single reacted photo", async () => {
    await openConversation("conv-reaction-states");
    expect(document.querySelector('[data-reaction-message="reaction-photo"]')).toHaveAttribute("aria-keyshortcuts", "R");
    expect(document.querySelector('[data-reaction-message="reaction-photo-pair"][data-slide-index="0"]')).toHaveAttribute("aria-keyshortcuts", "R");
  });

  it("draws one badge group and one id for a reacted message with two photos", async () => {
    await openConversation("conv-reaction-states");
    expect(document.querySelectorAll('[data-reaction-badges="reaction-photo-pair"]')).toHaveLength(1);
    expect(document.querySelectorAll("#reactions-reaction-photo-pair")).toHaveLength(1);
  });
});

describe("photo labels", () => {
  it("names a single photo without a count", async () => {
    await openConversation("conv-reaction-states");
    fireEvent.keyDown(document.querySelector('[data-reaction-message="reaction-photo"]')!, { key: "r" });
    expect(bar()).toHaveAttribute("aria-label", "React to photo from +15555550180");
  });

  it("numbers a slide within its run, never zero", async () => {
    await openConversation("conv-reaction-states");
    fireEvent.keyDown(document.querySelector('[data-reaction-message="reaction-photo-pair"][data-slide-index="1"]')!, { key: "r" });
    expect(bar()).toHaveAttribute("aria-label", "React to photo 2 of 2 from +15555550180");
  });
});

describe("roving toolbar", () => {
  it("is one tab stop: the selected button, with the arrow keys moving it and Tab leaving", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-own-sent");
    const buttons = () => [...document.querySelectorAll<HTMLElement>("#message-reaction-bar button[data-reaction-quick], #message-reaction-bar button[data-reaction-more]")];
    expect(buttons().filter((button) => button.tabIndex === 0)).toHaveLength(1);
    expect(buttons().find((button) => button.tabIndex === 0)).toHaveAttribute("data-reaction-quick", "laughing");

    fireEvent.keyDown(bar()!, { key: "ArrowRight" });
    expect(document.activeElement).toHaveAttribute("data-reaction-quick", "surprised");
    expect(buttons().filter((button) => button.tabIndex === 0)).toEqual([document.activeElement]);
  });
});

describe("persistent live regions", () => {
  it("renders both announcers before any reaction, the status one polite and the alert one assertive", async () => {
    await openConversation("conv-reaction-states");
    const status = document.getElementById("reaction-announcer")!;
    const alert = document.getElementById("reaction-alert")!;
    expect(status).toHaveAttribute("role", "status");
    expect(status).toHaveAttribute("aria-live", "polite");
    expect(alert).toHaveAttribute("aria-live", "assertive");
    expect(alert).not.toHaveAttribute("role", "alert");
    expect(document.getElementById("message-list")!.contains(status)).toBe(false);
    expect(document.getElementById("message-list")!.contains(alert)).toBe(false);
  });

  it("keeps the hint outside the log, with keyboard shortcuts declared on the roving article", async () => {
    await openConversation("conv-reaction-states");
    expect(document.getElementById("message-list")!.contains(document.getElementById("message-reaction-hint"))).toBe(false);
    expect(article("reaction-unknown")).toHaveAttribute("aria-keyshortcuts", "Enter");
  });

});

describe("optimistic state", () => {
  it("shows a pending chip and the busy pill as soon as the call is accepted, before any snapshot", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector('[data-reaction-quick="surprised"]')!);
    await waitFor(() => expect(article("reaction-group-dinner").closest(".message-row")!.querySelector('[data-reaction-state="pending"]')).not.toBeNull());
    await act(async () => {});
    keyOpen("reaction-group-dinner");
    expect(document.querySelector("[data-reaction-bar-notice='busy']")).toHaveTextContent("Sending your reaction…");
  });

  it("drops the optimistic request when the call is refused, and says why", async () => {
    await openConversation("conv-reaction-states");
    vi.mocked(bridge.react_to_message!).mockResolvedValueOnce({ accepted: false, reason: "offline" });
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector('[data-reaction-quick="surprised"]')!);
    await waitFor(() => expect(document.getElementById("reaction-alert")).toHaveTextContent("Phone is offline. Check it and try again."));
    // Scoped to the message: the fixture's own static pending chip lives on another message.
    expect(article("reaction-group-dinner").closest(".message-row")!.querySelector('[data-reaction-state="pending"]')).toBeNull();
  });
});

describe("capture feedback", () => {
  it("says the chosen emoji and the first-emoji note in one polite line", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector("[data-reaction-more]")!);
    fireEvent.input(screen.getByRole("textbox", { name: "Emoji reaction" }), { target: { value: "👍🏽👍" } });
    await waitFor(() => expect(document.getElementById("reaction-announcer")).toHaveTextContent("Sending your 👍🏽 reaction. Using the first emoji only."));
    expect(document.getElementById("reaction-alert")).toHaveTextContent("");
  });
});

describe("re-check: leaving the toolbar with Tab and Shift+Tab", () => {
  it("Tab closes the bar and moves focus to the composer, not to BODY", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-own-sent");
    const stop = document.activeElement as HTMLElement;
    expect(stop).toHaveAttribute("data-reaction-quick", "laughing");
    // preventDefault: fireEvent returns false when the handler took the key.
    expect(fireEvent.keyDown(stop, { key: "Tab" })).toBe(false);
    expect(bar()).toBeNull();
    expect(document.activeElement).toBe(document.getElementById("composer-textarea"));
  });

  it("Shift+Tab closes the bar and returns focus to the target message, not a sender label", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-own-sent");
    const stop = document.activeElement as HTMLElement;
    expect(fireEvent.keyDown(stop, { key: "Tab", shiftKey: true })).toBe(false);
    expect(bar()).toBeNull();
    expect(document.activeElement).toBe(article("reaction-own-sent"));
  });
});

describe("re-check: the tile body is described by the R hint", () => {
  it("points a reactable photo's body at the sr-only R hint, outside the log", async () => {
    await openConversation("conv-reaction-states");
    const hint = document.getElementById("message-reaction-hint-r")!;
    expect(hint).toHaveTextContent("Press R to react.");
    expect(document.getElementById("message-list")!.contains(hint)).toBe(false);
    expect(document.querySelector('.tile-body[data-message-id="reaction-photo"]')).toHaveAttribute("aria-describedby", "message-reaction-hint-r");
  });
});

describe("review round 2 (R2-C): the optimistic overlay reconciles against authoritative snapshots", () => {
  /** The group message as the core would project it after the phone settled the request with no reaction change. */
  const settledNoChange = () => { snapshot.conversations[0].messages = snapshot.conversations[0].messages.map((m) => (m.id === "reaction-group-dinner" ? { ...m, ownReactionRequest: undefined } : m)); };
  const groupChipPending = () => article("reaction-group-dinner").closest(".message-row")!.querySelector('[data-reaction-state="pending"]');

  it("keeps the provisional chip during a snapshot and drops it when the accepted call catches up", async () => {
    await openConversation("conv-reaction-states");
    let resolve!: (value: { accepted: boolean; requestId: string }) => void;
    vi.mocked(bridge.react_to_message!).mockReturnValueOnce(new Promise((done) => { resolve = done; }));
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector('[data-reaction-quick="surprised"]')!);
    settledNoChange();
    await publish();
    expect(groupChipPending()).not.toBeNull();
    await act(async () => { resolve({ accepted: true, requestId: "real" }); });
    expect(groupChipPending()).toBeNull();
  });

  it.each(["refused", "rejected"] as const)("removes the provisional chip after a deferred %s call", async (outcome) => {
    await openConversation("conv-reaction-states");
    let resolve!: (value: { accepted: boolean; reason: "busy" }) => void;
    let reject!: (error: Error) => void;
    vi.mocked(bridge.react_to_message!).mockReturnValueOnce(new Promise((yes, no) => { resolve = yes; reject = no; }));
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector('[data-reaction-quick="surprised"]')!);
    expect(groupChipPending()).not.toBeNull();
    const alert = document.getElementById("reaction-alert")!;
    const expected = outcome === "refused" ? "Sending your reaction…" : "Couldn't reach Peppy. Try again.";
    let announcements = 0;
    const observer = new MutationObserver(() => { if (alert.textContent === expected) announcements += 1; });
    observer.observe(alert, { childList: true, characterData: true, subtree: true });
    await act(async () => { if (outcome === "refused") resolve({ accepted: false, reason: "busy" }); else reject(new Error("offline")); });
    expect(groupChipPending()).toBeNull();
    await waitFor(() => expect(alert).toHaveTextContent(expected));
    await act(async () => { await new Promise<void>((done) => requestAnimationFrame(() => done())); });
    expect(announcements).toBe(1);
    observer.disconnect();
  });

  it("ignores a second activation of the pending own chip", async () => {
    await openConversation("conv-reaction-states");
    vi.mocked(bridge.react_to_message!).mockReturnValueOnce(new Promise(() => undefined));
    keyOpen("reaction-group-dinner");
    const button = document.querySelector('[data-reaction-quick="surprised"]')!;
    fireEvent.click(button);
    fireEvent.click(button);
    expect(groupChipPending()).not.toBeNull();
    fireEvent.click(groupChipPending()!);
    expect(bridge.react_to_message).toHaveBeenCalledTimes(1);
  });

  it("clears on a snapshot delivered before the response, with no reaction change and no open request", async () => {
    await openConversation("conv-reaction-states");
    vi.mocked(bridge.react_to_message!).mockImplementationOnce(async () => {
      // The core settled the request before the response reached the UI: the terminal snapshot arrives first.
      settledNoChange();
      await publish();
      return { accepted: true, route: "native", requestId: "request-1" };
    });
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector('[data-reaction-quick="surprised"]')!);
    await act(async () => {});
    expect(groupChipPending()).toBeNull();
    keyOpen("reaction-group-dinner");
    expect(document.querySelector("[data-reaction-bar-notice='busy']")).toBeNull();
    expect(document.querySelector("[data-reaction-quick]")).not.toBeNull();
  });

  it("clears on the first snapshot after the response, even with no reaction change, and never leaves a busy pill", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector('[data-reaction-quick="surprised"]')!);
    await waitFor(() => expect(groupChipPending()).not.toBeNull());
    await act(async () => {});
    // The next snapshot shows the request settled with the reactions unchanged: the overlay must go.
    settledNoChange();
    await publish();
    expect(groupChipPending()).toBeNull();
    keyOpen("reaction-group-dinner");
    expect(document.querySelector("[data-reaction-bar-notice='busy']")).toBeNull();
  });

  it("announces the sent outcome when the first snapshot after the response shows the reaction", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector('[data-reaction-quick="surprised"]')!);
    await waitFor(() => expect(groupChipPending()).not.toBeNull());
    await act(async () => {});
    const announcer = document.getElementById("reaction-announcer")!;
    let sentAnnouncements = 0;
    const observer = new MutationObserver(() => { if (announcer.textContent === "Your 😮 reaction was sent.") sentAnnouncements += 1; });
    observer.observe(announcer, { childList: true, characterData: true, subtree: true });
    snapshot.conversations[0].messages = snapshot.conversations[0].messages.map((m) => (m.id === "reaction-group-dinner" ? { ...m, ownReactionRequest: undefined, reactions: [...(m.reactions ?? []), { emoji: "😮", actors: [{ self: true }] }] } : m));
    await publish();
    await waitFor(() => expect(document.getElementById("reaction-announcer")).toHaveTextContent("Your 😮 reaction was sent."));
    await act(async () => { await new Promise<void>((resolve) => requestAnimationFrame(() => resolve())); });
    expect(sentAnnouncements).toBe(1);
    observer.disconnect();
    expect(groupChipPending()).toBeNull();
  });

  it("announces a removal that settles with no reaction left", async () => {
    await openConversation("conv-reaction-states");
    keyOpen("reaction-own-sent");
    fireEvent.click(document.querySelector('[data-reaction-quick="laughing"]')!);
    await act(async () => {});
    snapshot.conversations[0].messages = snapshot.conversations[0].messages.map((m) => (m.id === "reaction-own-sent" ? { ...m, ownReactionRequest: undefined, reactions: [] } : m));
    await publish();
    await waitFor(() => expect(document.getElementById("reaction-announcer")).toHaveTextContent("Your reaction was removed."));
  });

  it("holds no overlay after a refusal, and a later unchanged snapshot does not create one", async () => {
    await openConversation("conv-reaction-states");
    vi.mocked(bridge.react_to_message!).mockResolvedValueOnce({ accepted: false, reason: "busy" });
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector('[data-reaction-quick="surprised"]')!);
    await waitFor(() => expect(document.getElementById("reaction-alert")).toHaveTextContent("Sending your reaction…"));
    await act(async () => {});
    await publish();
    expect(groupChipPending()).toBeNull();
    keyOpen("reaction-group-dinner");
    expect(document.querySelector("[data-reaction-quick]")).not.toBeNull();
  });

  it("holds no overlay after a rejected call, and the next snapshot does not revive it", async () => {
    await openConversation("conv-reaction-states");
    vi.mocked(bridge.react_to_message!).mockRejectedValueOnce(new Error("bridge down"));
    keyOpen("reaction-group-dinner");
    fireEvent.click(document.querySelector('[data-reaction-quick="surprised"]')!);
    await waitFor(() => expect(document.getElementById("reaction-alert")).toHaveTextContent("Couldn't reach Peppy. Try again."));
    await act(async () => {});
    await publish();
    expect(groupChipPending()).toBeNull();
  });
});
