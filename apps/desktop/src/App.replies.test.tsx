import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, onTestFinished, vi } from "vitest";
import { App } from "./App";
import type { ConversationView } from "@peppy/desktop-ui/bridge-contracts";
import { bridge, fixtureBridge, fixtureReactionConversations, type DesktopSnapshot } from "./bridge/bridge";

let snapshot: DesktopSnapshot;
/** Fixture conversations with messages, read before `load_state` is spied: `bridge` and `fixtureBridge` share the method. */
let loadedFixtures: Map<string, ConversationView>;

beforeEach(async () => {
  vi.restoreAllMocks();
  snapshot = structuredClone(await fixtureBridge.load_state());
  const ids = ["conv-aurora", "conv-sam"];
  loadedFixtures = new Map(await Promise.all(ids.map(async (id) => {
    const loaded = await fixtureBridge.load_state(id);
    return [id, structuredClone(loaded.conversations.find((conversation) => conversation.id === id)!)] as const;
  })));
  snapshot.encryption.profileFingerprint = "profile-a";
  vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => ({ ...structuredClone(snapshot), activeConversationId: conversationId ?? snapshot.activeConversationId }));
  vi.spyOn(bridge, "search_local").mockImplementation(fixtureBridge.search_local);
  vi.spyOn(bridge, "load_message_window").mockImplementation(fixtureBridge.load_message_window);
  vi.spyOn(bridge, "subscribe").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  vi.spyOn(bridge, "save_draft").mockImplementation(async (input) => ({
    id: input.id || "draft-replies",
    conversationId: input.conversationId,
    text: input.text,
    recipientIds: input.recipientIds,
    attachmentIds: input.attachmentIds,
    groupName: input.groupName,
    gatewayId: input.gatewayId,
    simId: input.simId,
    revision: String(Number(input.expectedRevision) + 1),
  }));
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
  stubLayout();
});

const rectOf = (top: number, bottom: number, left: number, right: number) => ({ top, bottom, left, right, width: right - left, height: bottom - top, x: left, y: top, toJSON() {} });

/** jsdom has no layout: give the list, composer and every message article a fixed rectangle, as the reaction tests do. */
function stubLayout() {
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
    if (this.id === "message-list") return rectOf(0, 800, 0, 400);
    if (this.id === "composer-area") return rectOf(700, 780, 0, 400);
    if (this.getAttribute("data-message-id")) return rectOf(200, 260, 16, 300);
    return rectOf(0, 0, 0, 0);
  });
}

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

/** Drops a message from a conversation in both the host's snapshot and the windows it loads from the fixture store. */
function withoutMessage(conversationId: string, messageId: string) {
  const conversation = snapshot.conversations.find((item) => item.id === conversationId)!;
  conversation.messages = conversation.messages.filter((message) => message.id !== messageId);
  vi.spyOn(bridge, "load_message_window").mockImplementation(async (input) => {
    const window = await fixtureBridge.load_message_window(input);
    return { ...window, messages: window.messages.filter((message) => message.id !== messageId) };
  });
}

/** A fixture conversation with its messages. */
function fixtureConversation(conversationId: string): ConversationView {
  const reactionConversation = fixtureReactionConversations.find((conversation) => conversation.id === conversationId);
  if (reactionConversation) return structuredClone(reactionConversation);
  return structuredClone(loadedFixtures.get(conversationId)!);
}

/**
 * Renders the app on a fixture conversation (listed first, as the reaction tests do). `others` are listed after it.
 * The active draft is routed to the fixture gateway, as a sent SMS needs.
 */
async function openConversation(conversationId: string, others: string[] = [], edit?: (conversation: ConversationView) => void) {
  snapshot.conversations = [conversationId, ...others].map(fixtureConversation);
  if (edit) edit(snapshot.conversations[0]);
  snapshot.activeConversationId = conversationId;
  snapshot.draft = { id: `draft-${conversationId}`, conversationId, text: "", recipientIds: [], attachmentIds: [], gatewayId: "gateway-pixel8", simId: "sim-1", revision: "1" };
  render(<App hostKind="browser" />);
  await waitFor(() => expect(document.querySelector("[data-message-id]")).not.toBeNull());
}

const article = (messageId: string) => document.querySelector<HTMLElement>(`article[data-message-id="${messageId}"]`)!;
const chip = () => document.getElementById("composer-reply-chip");
const composerText = () => document.getElementById("composer-textarea") as HTMLTextAreaElement;

/** Opens the reaction bar on a message from the keyboard, then picks its Reply button. */
function chooseReply(messageId: string) {
  act(() => article(messageId).focus());
  fireEvent.keyDown(article(messageId), { key: "Enter" });
  fireEvent.click(document.querySelector("[data-reaction-reply]")!);
}

async function sendButton() {
  return waitFor(() => {
    const button = screen.getByRole("button", { name: "Send" });
    expect(button).toBeEnabled();
    return button;
  });
}

async function typeAndSend(text: string) {
  fireEvent.change(composerText(), { target: { value: text } });
  fireEvent.click(await sendButton());
}

describe("reply target in the composer", () => {
  it("puts a reply chip above the textarea and focuses the textarea", async () => {
    await openConversation("conv-reaction-text");
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    expect(chip()!.getAttribute("aria-label")).toContain("Running late, start without me");
    await waitFor(() => expect(document.activeElement).toBe(composerText()));
  });

  it("cancels the reply from the chip's close button", async () => {
    await openConversation("conv-reaction-text");
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    fireEvent.click(screen.getByRole("button", { name: /Cancel reply to/ }));
    expect(chip()).toBeNull();
  });

  it("cancels the reply on Escape in an empty textarea", async () => {
    await openConversation("conv-reaction-text");
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    fireEvent.keyDown(composerText(), { key: "Escape" });
    expect(chip()).toBeNull();
  });

  it("sends the reply target, then clears the chip once the send is accepted", async () => {
    await openConversation("conv-reaction-text");
    const sendDraft = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "queued-local", revision: "2" });
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await typeAndSend("Sure, 10 works");
    await waitFor(() => expect(sendDraft).toHaveBeenCalledTimes(1));
    expect(sendDraft.mock.calls[0][0]).toMatchObject({ replyTo: "reaction-text-incoming", text: "Sure, 10 works" });
    await waitFor(() => expect(chip()).toBeNull());
  });

  it("keeps the chip when the send is refused", async () => {
    await openConversation("conv-reaction-text");
    vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: false, status: "failed-before-submit", reason: "Carrier refused the message." });
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await typeAndSend("Sure, 10 works");
    await waitFor(() => expect(bridge.send_draft).toHaveBeenCalled());
    await waitFor(() => expect(composerText().value).toBe("Sure, 10 works"));
    expect(chip()).not.toBeNull();
  });

  it("drops the chip when the host refuses the reply target, and keeps the draft text", async () => {
    await openConversation("conv-reaction-text");
    vi.spyOn(bridge, "send_draft").mockRejectedValue({ code: "reply-unavailable", message: "The message you're replying to is no longer available." });
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await typeAndSend("Sure, 10 works");
    await waitFor(() => expect(chip()).toBeNull());
    expect(composerText().value).toBe("Sure, 10 works");
    expect(document.getElementById("composer-area")).toHaveTextContent("The message you're replying to is no longer available.");
  });

  it("keeps the chip for a refusal other than an unavailable target", async () => {
    await openConversation("conv-reaction-text");
    vi.spyOn(bridge, "send_draft").mockRejectedValue({ code: "invalid-request", message: "Request rejected." });
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await typeAndSend("Sure, 10 works");
    await waitFor(() => expect(bridge.send_draft).toHaveBeenCalled());
    await waitFor(() => expect(screen.getByText(/Send failed/)).toBeInTheDocument());
    expect(chip()).not.toBeNull();
  });

  it("keeps the cached name and quote on the chip after the target is no longer loaded", async () => {
    await openConversation("conv-reaction-text", ["conv-aurora"]);
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    // The target leaves the conversation's messages, as a deleted or unloaded original does.
    withoutMessage("conv-reaction-text", "reaction-text-incoming");
    fireEvent.click(document.querySelector("[data-conversation-row='conv-aurora'] .conversation-select")!);
    await waitFor(() => expect(chip()).toBeNull());
    fireEvent.click(document.querySelector("[data-conversation-row='conv-reaction-text'] .conversation-select")!);
    await waitFor(() => expect(chip()).not.toBeNull());
    expect(document.querySelector("[data-message-id='reaction-text-incoming']")).toBeNull();
    expect(chip()).toHaveAttribute("aria-label", expect.stringContaining("Running late, start without me"));
    expect(chip()).toHaveTextContent("Running late, start without me");
  });

  it("keeps each conversation's reply target when switching conversations", async () => {
    await openConversation("conv-reaction-text", ["conv-aurora"]);
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    fireEvent.click(document.querySelector("[data-conversation-row='conv-aurora'] .conversation-select")!);
    await waitFor(() => expect(chip()).toBeNull());
    fireEvent.click(document.querySelector("[data-conversation-row='conv-reaction-text'] .conversation-select")!);
    await waitFor(() => expect(chip()).not.toBeNull());
  });

  it("adds the quoted reply text to the SMS counter", async () => {
    await openConversation("conv-reaction-text");
    // 36 characters alone stay under the counter's threshold; the quote (“Running late, start without me”⏎, 33 units) adds UCS-2 units.
    fireEvent.change(composerText(), { target: { value: "Sure, I can do that after work today" } });
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await waitFor(() => expect(document.getElementById("sms-counter")).toHaveTextContent("69/70"));
  });
});

describe("reply context in the conversation", () => {
  it("shows an available original as a button that locates it", async () => {
    await openConversation("conv-aurora");
    const context = document.querySelector<HTMLButtonElement>("button[data-message-reply-context='message-aurora-2']");
    expect(context).not.toBeNull();
    expect(context).toHaveTextContent("Replying to you");
    expect(context).toHaveTextContent("I'll have it to you shortly.");
    fireEvent.click(context!);
    await waitFor(() => expect(article("message-aurora-2")).toHaveAttribute("data-reaction-highlight", "true"));
  });

  it("shows an unavailable original as plain text", async () => {
    await openConversation("conv-sam");
    const context = document.querySelector<HTMLElement>("[data-message-reply-context='message-sam-missing']");
    expect(context?.tagName).toBe("DIV");
    expect(context).toHaveAttribute("data-reply-source");
    expect(context).toHaveTextContent("Original message not available");
    expect(context?.querySelector("button")).toBeNull();
  });
});

describe("reply context on photo rows and the fixture host", () => {
  it("shows the reply line above a photo-only message, in the sender label's place", async () => {
    const replyTo = { messageId: "reaction-group-dinner", sender: "other" as const, senderAddress: "+15555550142", excerpt: "Dinner at 7", available: true };
    await openConversation("conv-reaction-states", [], (conversation) => {
      conversation.messages = conversation.messages.map((message) => message.id === "reaction-photo" ? { ...message, replyTo } : message);
    });
    const photo = document.querySelector<HTMLElement>("[data-media-message-content='reaction-photo']")!;
    const context = photo.closest<HTMLElement>(".message-row")?.querySelector<HTMLElement>("[data-message-reply-context]");
    expect(context).toHaveTextContent("Dinner at 7");
    expect(context).toHaveAttribute("data-reply-source", "reaction-photo");
    expect(context!.compareDocumentPosition(photo) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("refuses a reply whose target is not in the conversation, as the host does", async () => {
    const send = (replyTo: string) => fixtureBridge.send_draft({ id: "draft-aurora", conversationId: "conv-aurora", text: "Hi", recipientIds: [], attachmentIds: [], expectedRevision: "1", gatewayId: "gateway-pixel8", simId: "sim-1", replyTo });
    await expect(send("message-missing")).rejects.toMatchObject({ code: "reply-unavailable", message: "The message you're replying to is no longer available." });
    await expect(send("message-aurora-2")).resolves.toMatchObject({ accepted: true });
  });
});

describe("reply chip names, announcements and focus", () => {
  const announcer = () => document.getElementById("reaction-announcer");

  it("names your own message by subject, announces the reply, and keeps the visible name", async () => {
    await openConversation("conv-reaction-text");
    chooseReply("reaction-text-self");
    await waitFor(() => expect(chip()).not.toBeNull());
    expect(chip()!.getAttribute("aria-label")).toBe("Replying to your message: Sent it");
    expect(chip()!.querySelector("[data-reply-chip-quote] strong")).toHaveTextContent("You");
    expect(screen.getByRole("button", { name: "Cancel reply to your message" })).toBeInTheDocument();
    await waitFor(() => expect(announcer()).toHaveTextContent("Replying to your message."));
  });

  it("announces the cancel, returns focus to the composer, and describes the textarea by the chip", async () => {
    await openConversation("conv-reaction-text");
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    expect(composerText()).toHaveAttribute("aria-describedby", "composer-reply-chip");
    await waitFor(() => expect(announcer()).toHaveTextContent(/^Replying to /));
    fireEvent.click(screen.getByRole("button", { name: /Cancel reply to/ }));
    expect(chip()).toBeNull();
    await waitFor(() => expect(announcer()).toHaveTextContent("Reply cancelled."));
    expect(composerText()).toHaveFocus();
    expect(composerText()).not.toHaveAttribute("aria-describedby");
  });

  it("announces the cancel on Escape in an empty textarea", async () => {
    await openConversation("conv-reaction-text");
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    fireEvent.keyDown(composerText(), { key: "Escape" });
    await waitFor(() => expect(announcer()).toHaveTextContent("Reply cancelled."));
  });

  it("states the refusal in full when the host no longer has the reply target", async () => {
    await openConversation("conv-reaction-text");
    vi.spyOn(bridge, "send_draft").mockRejectedValue({ code: "reply-unavailable", message: "The message you're replying to is no longer available." });
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await typeAndSend("Sure, 10 works");
    await waitFor(() => expect(chip()).toBeNull());
    expect(document.getElementById("composer-area")).toHaveTextContent("Not sent. The message you're replying to is no longer available.");
  });

  it("does not collapse the composer window when Escape cancels a reply", async () => {
    window.history.replaceState({}, "", "/?window=composer&conversationId=conv-reaction-text");
    const close = vi.spyOn(bridge, "close_composer").mockResolvedValue(undefined);
    try {
      await openConversation("conv-reaction-text");
      chooseReply("reaction-text-incoming");
      await waitFor(() => expect(chip()).not.toBeNull());
      fireEvent.keyDown(composerText(), { key: "Escape" });
      await waitFor(() => expect(chip()).toBeNull());
      await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
      expect(close).not.toHaveBeenCalled();
      fireEvent.keyDown(composerText(), { key: "Escape" });
      await waitFor(() => expect(close).toHaveBeenCalled());
    } finally {
      window.history.replaceState({}, "", "/");
    }
  });
});

describe("reply context wording", () => {
  it("names an unavailable original with a colon and gives the full text as a tooltip", async () => {
    await openConversation("conv-sam");
    const context = document.querySelector<HTMLElement>("[data-message-reply-context='message-sam-missing']")!;
    expect(context).toHaveTextContent(/^Replying to .+: Original message not available$/);
    expect(context).toHaveAttribute("title", expect.stringMatching(/^Replying to .+: Original message not available$/));
  });

  it("names an available original's button with its visible text first, then the action", async () => {
    await openConversation("conv-aurora");
    const context = document.querySelector<HTMLButtonElement>("button[data-message-reply-context='message-aurora-2']")!;
    expect(context.getAttribute("aria-label")).toMatch(/^Replying to you “I'll have it to you shortly\..*”\. Go to original message$/);
    expect(context.getAttribute("title")).toMatch(/^Replying to you “I'll have it to you shortly\..*”$/);
  });
});

describe("reply target across session replacement", () => {
  it("keeps the chip and sends replyTo when only the session scope changes on a direct install", async () => {
    await openConversation("conv-reaction-text");
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    const sent = vi.spyOn(bridge, "send_draft").mockImplementation(fixtureBridge.send_draft);
    // A scope-only change (for example a retention epoch) keeps the search key, so the captured target still names the same message.
    // Activating a search hit installs its snapshot directly, not through performRefresh.
    snapshot.sessionScopeId = "session-b";
    const hit = { kind: "message" as const, conversationId: "conv-reaction-text", messageId: "reaction-text-incoming", title: "", snippet: "", timestamp: "1788435180000" };
    vi.spyOn(bridge, "search_local").mockImplementation(async ({ query }) => ({ query, items: [hit], indexState: "ready", hasMore: { conversation: false, message: false, contact: false } }));
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "reaction-text-incoming" } });
    await waitFor(() => expect(document.querySelector("[data-search-hit]")).not.toBeNull());
    fireEvent.click(document.querySelector<HTMLElement>("[data-search-hit]")!);
    await waitFor(() => expect(article("reaction-text-incoming")).toBeInTheDocument());
    expect(chip()).not.toBeNull();
    await typeAndSend("Sure, 10 works");
    await waitFor(() => expect(sent).toHaveBeenCalled());
    expect(sent.mock.calls[0][0]).toMatchObject({ replyTo: "reaction-text-incoming", text: "Sure, 10 works" });
  });

  it("drops the chip when the profile changes on a direct install, and aborts a send whose flush is still pending", async () => {
    let releaseSaves!: () => void;
    const saves = new Promise<void>((resolve) => { releaseSaves = resolve; });
    const saveNow = vi.mocked(bridge.save_draft).getMockImplementation()!;
    const save = vi.spyOn(bridge, "save_draft").mockImplementation(async (input) => { await saves; return saveNow(input); });
    let refreshSession!: () => void;
    vi.spyOn(bridge, "subscribe").mockImplementation((onChange) => { refreshSession = onChange as () => void; return () => undefined; });
    const sent = vi.spyOn(bridge, "send_draft").mockImplementation(fixtureBridge.send_draft);
    await openConversation("conv-reaction-text");
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    fireEvent.change(composerText(), { target: { value: "Sure, 10 works" } });
    fireEvent.click(await sendButton());
    await waitFor(() => expect(save).toHaveBeenCalled());
    // A different profile is a different search key. Its refresh installs the snapshot while the flush waits: the target is gone, and HEAD's abort applies.
    snapshot.encryption.profileFingerprint = "profile-b";
    await act(async () => { refreshSession(); });
    await waitFor(() => expect(chip()).toBeNull());
    releaseSaves();
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
    expect(sent).not.toHaveBeenCalled();
  });

  it("still sends a plain draft whose flush is pending when only the session scope changes", async () => {
    let releaseSaves!: () => void;
    const saves = new Promise<void>((resolve) => { releaseSaves = resolve; });
    const saveNow = vi.mocked(bridge.save_draft).getMockImplementation()!;
    const save = vi.spyOn(bridge, "save_draft").mockImplementation(async (input) => { await saves; return saveNow(input); });
    let refreshSession!: () => void;
    vi.spyOn(bridge, "subscribe").mockImplementation((onChange) => { refreshSession = onChange as () => void; return () => undefined; });
    const sent = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "queued-local", revision: "2" });
    await openConversation("conv-reaction-text");
    fireEvent.change(composerText(), { target: { value: "Plain text" } });
    fireEvent.click(await sendButton());
    await waitFor(() => expect(save).toHaveBeenCalled());
    // A scope-only change (for example a retention epoch) keeps the search key. A plain send has no reply to protect.
    snapshot.sessionScopeId = "session-b";
    await act(async () => { refreshSession(); });
    releaseSaves();
    await waitFor(() => expect(sent).toHaveBeenCalledTimes(1));
    expect(sent.mock.calls[0][0]).toMatchObject({ text: "Plain text" });
    expect(sent.mock.calls[0][0].replyTo).toBeUndefined();
  });

  it("keeps the chip and sends replyTo when a subscription refresh changes only the session scope", async () => {
    let refreshSession!: () => void;
    vi.spyOn(bridge, "subscribe").mockImplementation((onChange) => { refreshSession = onChange as () => void; return () => undefined; });
    const sent = vi.spyOn(bridge, "send_draft").mockImplementation(fixtureBridge.send_draft);
    await openConversation("conv-reaction-text");
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    // A scope-only change (for example a retention epoch) keeps the search key, so the refresh must not drop the reply target.
    snapshot.sessionScopeId = "session-b";
    await act(async () => { refreshSession(); });
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
    expect(chip()).not.toBeNull();
    await typeAndSend("Sure, 10 works");
    await waitFor(() => expect(sent).toHaveBeenCalled());
    expect(sent.mock.calls[0][0]).toMatchObject({ replyTo: "reaction-text-incoming" });
  });

  it("restores a refused reply and releases its slot when only the session scope changes after submission", async () => {
    let refuse!: () => void;
    const pending = new Promise<void>((resolve) => { refuse = resolve; });
    let refreshSession!: () => void;
    vi.spyOn(bridge, "subscribe").mockImplementation((onChange) => { refreshSession = onChange as () => void; return () => undefined; });
    const sent = vi.spyOn(bridge, "send_draft")
      .mockImplementationOnce(async () => { await pending; return { accepted: false, status: "failed-before-submit", reason: "Carrier refused the message." }; })
      .mockResolvedValue({ accepted: true, status: "queued-local", revision: "2" });
    await openConversation("conv-reaction-text");
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await typeAndSend("Sure, 10 works");
    await waitFor(() => expect(sent).toHaveBeenCalledTimes(1));
    // Submitted: a scope-only change (for example a retention epoch) keeps the search key, so settlement still runs.
    snapshot.sessionScopeId = "session-b";
    await act(async () => { refreshSession(); });
    await act(async () => { refuse(); });
    await waitFor(() => expect(composerText().value).toBe("Sure, 10 works"));
    expect(composerText()).toBeEnabled();
    // The slot is released, so the restored draft can be sent again.
    fireEvent.click(await sendButton());
    await waitFor(() => expect(sent).toHaveBeenCalledTimes(2));
    expect(sent.mock.calls[1][0]).toMatchObject({ text: "Sure, 10 works" });
  });

  it("marks an accepted reply sent and keeps the composer editable when only the session scope changes after submission", async () => {
    let accept!: () => void;
    const pending = new Promise<void>((resolve) => { accept = resolve; });
    let refreshSession!: () => void;
    vi.spyOn(bridge, "subscribe").mockImplementation((onChange) => { refreshSession = onChange as () => void; return () => undefined; });
    const sent = vi.spyOn(bridge, "send_draft")
      .mockImplementationOnce(async () => { await pending; return { accepted: true, status: "queued-local", revision: "2" }; })
      .mockImplementation(fixtureBridge.send_draft);
    await openConversation("conv-reaction-text");
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await typeAndSend("Sure, 10 works");
    await waitFor(() => expect(sent).toHaveBeenCalledTimes(1));
    snapshot.sessionScopeId = "session-b";
    await act(async () => { refreshSession(); });
    await act(async () => { accept(); });
    await waitFor(() => expect(composerText().value).toBe(""));
    expect(composerText()).toBeEnabled();
    fireEvent.change(composerText(), { target: { value: "Next message" } });
    expect(composerText().value).toBe("Next message");
    await waitFor(() => expect(bridge.save_draft).toHaveBeenCalledWith(expect.objectContaining({ text: "Next message" })));
    expect(chip()).toBeNull();
  });
});

/** Holds the draft save until `release`, so a merge or a cancel can land while the send's flush is pending. */
function holdSave() {
  let release!: () => void;
  const saves = new Promise<void>((resolve) => { release = resolve; });
  const saveNow = vi.mocked(bridge.save_draft).getMockImplementation()!;
  const save = vi.spyOn(bridge, "save_draft").mockImplementation(async (input) => { await saves; return saveNow(input); });
  return { save, release: () => act(async () => { release(); }) };
}

const TEXT_CONVERSATION = "conv-reaction-text";
const STATES_CONVERSATION = "conv-reaction-states";

/** Each conversation's stored draft: the source keeps an empty one, the survivor holds "Dinner plans". */
const storedDrafts: Record<string, NonNullable<DesktopSnapshot["draft"]>> = {
  [TEXT_CONVERSATION]: { id: "draft-conv-reaction-text", conversationId: TEXT_CONVERSATION, text: "", recipientIds: [], attachmentIds: [], gatewayId: "gateway-pixel8", simId: "sim-1", revision: "1" },
  [STATES_CONVERSATION]: { id: "draft-conv-reaction-states", conversationId: STATES_CONVERSATION, text: "Dinner plans", recipientIds: [], attachmentIds: [], gatewayId: "gateway-pixel8", simId: "sim-1", revision: "1" },
};

/** load_state serves each conversation its own stored draft. A redirected conversation gets its survivor's, as the host answers a merge. */
function serveStoredDrafts(drafts: Record<string, NonNullable<DesktopSnapshot["draft"]>>) {
  vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => {
    const requested = conversationId ?? snapshot.activeConversationId ?? "";
    const redirect = snapshot.requestedRedirect;
    const survivor = redirect && redirect.from === requested ? redirect.to : requested;
    return { ...structuredClone(snapshot), activeConversationId: survivor, draft: drafts[survivor] };
  });
}

describe("reply across a chat merge", () => {
  /** The host reports a merge of `from` into `to` on the next refresh, as a subscription update does. */
  function mergeOnRefresh(from: string, to: string) {
    let refreshSession!: () => void;
    vi.spyOn(bridge, "subscribe").mockImplementation((onChange) => { refreshSession = onChange as () => void; return () => undefined; });
    return async () => {
      snapshot.requestedRedirect = { from, to };
      await act(async () => { refreshSession(); });
    };
  }

  it("sends the reply shown at Send when the chat merges during the flush, and the chip follows the draft to the survivor", async () => {
    const { save, release } = holdSave();
    const merge = mergeOnRefresh("conv-reaction-text", "conv-reaction-states");
    const sent = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "queued-local", revision: "2" });
    await openConversation("conv-reaction-text", ["conv-reaction-states"]);
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    fireEvent.change(composerText(), { target: { value: "Sure, 10 works" } });
    fireEvent.click(await sendButton());
    await waitFor(() => expect(save).toHaveBeenCalled());
    await merge();
    await waitFor(() => expect(chip()).toHaveTextContent("Running late, start without me"));
    await release();
    await waitFor(() => expect(sent).toHaveBeenCalled());
    expect(sent.mock.calls[0][0]).toMatchObject({ replyTo: "reaction-text-incoming" });
    await waitFor(() => expect(chip()).toBeNull());
  });

  it("keeps the survivor's own reply and says so when a merge brings a second reply", async () => {
    const { save, release } = holdSave();
    const merge = mergeOnRefresh("conv-reaction-text", "conv-reaction-states");
    const sent = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "queued-local", revision: "2" });
    await openConversation("conv-reaction-text", ["conv-reaction-states"]);
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    fireEvent.change(composerText(), { target: { value: "Sure, 10 works" } });
    fireEvent.click(document.querySelector("[data-conversation-row='conv-reaction-states'] .conversation-select")!);
    await waitFor(() => expect(article("reaction-group-dinner")).toBeInTheDocument());
    chooseReply("reaction-group-dinner");
    await waitFor(() => expect(chip()).toHaveTextContent("Dinner at 7"));
    fireEvent.click(document.querySelector("[data-conversation-row='conv-reaction-text'] .conversation-select")!);
    await waitFor(() => expect(chip()).toHaveTextContent("Running late, start without me"));
    expect(composerText().value).toBe("Sure, 10 works");
    fireEvent.click(await sendButton());
    await waitFor(() => expect(save).toHaveBeenCalled());
    await merge();
    await waitFor(() => expect(document.getElementById("composer-area")).toHaveTextContent("Reply removed because this chat was merged."));
    expect(chip()).toHaveTextContent("Dinner at 7");
    await release();
    await waitFor(() => expect(sent).toHaveBeenCalled());
    expect(sent.mock.calls[0][0]).toMatchObject({ replyTo: "reaction-text-incoming" });
    // The accepted send clears its own (captured) selection; the survivor's conflicting reply stays.
    await waitFor(() => expect(composerText().value).toBe(""));
    expect(chip()).toHaveTextContent("Dinner at 7");
  });

  it("drops a source's reply, and says so, when the survivor keeps its own text and has no reply", async () => {
    const merge = mergeOnRefresh("conv-reaction-text", "conv-reaction-states");
    serveStoredDrafts(storedDrafts);
    const sent = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "queued-local", revision: "2" });
    await openConversation("conv-reaction-states", ["conv-reaction-text"]);
    fireEvent.change(composerText(), { target: { value: "Dinner plans" } });
    fireEvent.click(document.querySelector("[data-conversation-row='conv-reaction-text'] .conversation-select")!);
    await waitFor(() => expect(article("reaction-text-incoming")).toBeInTheDocument());
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await merge();
    await waitFor(() => expect(document.getElementById("composer-area")).toHaveTextContent("Reply removed because this chat was merged."));
    expect(chip()).toBeNull();
    expect(composerText().value).toBe("Dinner plans");
    fireEvent.click(await sendButton());
    await waitFor(() => expect(sent).toHaveBeenCalled());
    expect(sent.mock.calls[0][0]).not.toHaveProperty("replyTo");
    expect(sent.mock.calls[0][0]).toMatchObject({ text: "Dinner plans" });
    await waitFor(() => expect(composerText().value).toBe(""));
  });

  it("sends the survivor's own draft without the source's reply when the merge lands during the flush and the survivor keeps its text", async () => {
    const merge = mergeOnRefresh(TEXT_CONVERSATION, STATES_CONVERSATION);
    serveStoredDrafts(storedDrafts);
    await openConversation(STATES_CONVERSATION, [TEXT_CONVERSATION]);
    fireEvent.click(document.querySelector("[data-conversation-row='conv-reaction-text'] .conversation-select")!);
    await waitFor(() => expect(article("reaction-text-incoming")).toBeInTheDocument());
    const { save, release } = holdSave();
    const sent = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "queued-local", revision: "2" });
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    fireEvent.change(composerText(), { target: { value: "Sure, 10 works" } });
    fireEvent.click(await sendButton());
    await waitFor(() => expect(save).toHaveBeenCalled());
    await merge();
    await waitFor(() => expect(document.getElementById("composer-area")).toHaveTextContent("Reply removed because this chat was merged."));
    expect(chip()).toBeNull();
    await release();
    await waitFor(() => expect(sent).toHaveBeenCalled());
    expect(sent.mock.calls[0][0]).not.toHaveProperty("replyTo");
    expect(sent.mock.calls[0][0]).toMatchObject({ id: "draft-conv-reaction-states", text: "Dinner plans" });
  });

  it("removes the reply when a refresh installs the survivor's own stored draft into the moved slot", async () => {
    const merge = mergeOnRefresh(TEXT_CONVERSATION, STATES_CONVERSATION);
    serveStoredDrafts(storedDrafts);
    await openConversation(TEXT_CONVERSATION, [STATES_CONVERSATION]);
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    const sent = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "queued-local", revision: "2" });
    await merge();
    await waitFor(() => expect(document.getElementById("composer-area")).toHaveTextContent("Reply removed because this chat was merged."));
    expect(chip()).toBeNull();
    await waitFor(() => expect(composerText().value).toBe("Dinner plans"));
    fireEvent.click(await sendButton());
    await waitFor(() => expect(sent).toHaveBeenCalled());
    expect(sent.mock.calls[0][0]).not.toHaveProperty("replyTo");
    expect(sent.mock.calls[0][0]).toMatchObject({ id: "draft-conv-reaction-states", text: "Dinner plans" });
  });

  it("keeps the reply when the merge moves the source's stored draft onto a survivor opened with no stored draft", async () => {
    const merge = mergeOnRefresh(TEXT_CONVERSATION, STATES_CONVERSATION);
    serveStoredDrafts({ [TEXT_CONVERSATION]: storedDrafts[TEXT_CONVERSATION] });
    await openConversation(STATES_CONVERSATION, [TEXT_CONVERSATION]);
    fireEvent.click(document.querySelector("[data-conversation-row='conv-reaction-text'] .conversation-select")!);
    await waitFor(() => expect(article("reaction-text-incoming")).toBeInTheDocument());
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    // The host moves the source's draft, under its own id, onto the survivor.
    serveStoredDrafts({ [STATES_CONVERSATION]: { ...storedDrafts[TEXT_CONVERSATION], conversationId: STATES_CONVERSATION } });
    const sent = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "queued-local", revision: "2" });
    await merge();
    await waitFor(() => expect(chip()).toHaveTextContent("Running late, start without me"));
    expect(document.getElementById("composer-area")).not.toHaveTextContent("Reply removed because this chat was merged.");
    fireEvent.change(composerText(), { target: { value: "Sure, 10 works" } });
    fireEvent.click(await sendButton());
    await waitFor(() => expect(sent).toHaveBeenCalled());
    expect(sent.mock.calls[0][0]).toMatchObject({ id: "draft-conv-reaction-text", replyTo: "reaction-text-incoming", text: "Sure, 10 works" });
  });
});

describe("reply before the draft has a host id", () => {
  it("keeps the chip when the first save gives the unstored draft its host id, and sends the reply", async () => {
    serveStoredDrafts({});
    const sent = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "queued-local", revision: "2" });
    await openConversation(TEXT_CONVERSATION);
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    fireEvent.change(composerText(), { target: { value: "Sure, 10 works" } });
    await waitFor(() => expect(bridge.save_draft).toHaveBeenCalled());
    await act(async () => { await Promise.resolve(); });
    expect(chip()).toHaveTextContent("Running late, start without me");
    fireEvent.click(await sendButton());
    await waitFor(() => expect(sent).toHaveBeenCalled());
    expect(sent.mock.calls[0][0]).toMatchObject({ replyTo: "reaction-text-incoming", text: "Sure, 10 works" });
  });
});

describe("reply when the stored draft is replaced", () => {
  it("hides the reply, without a merge notice, when another stored draft takes the composer over, and sends without replyTo", async () => {
    let refreshSession!: () => void;
    vi.spyOn(bridge, "subscribe").mockImplementation((onChange) => { refreshSession = onChange as () => void; return () => undefined; });
    serveStoredDrafts(storedDrafts);
    await openConversation(TEXT_CONVERSATION, [STATES_CONVERSATION]);
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    const replacement = { ...storedDrafts[TEXT_CONVERSATION], id: "draft-conv-reaction-text-2", text: "Another draft" };
    serveStoredDrafts({ ...storedDrafts, [TEXT_CONVERSATION]: replacement });
    const sent = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "queued-local", revision: "2" });
    await act(async () => { refreshSession(); });
    await waitFor(() => expect(composerText().value).toBe("Another draft"));
    expect(chip()).toBeNull();
    expect(document.getElementById("composer-area")).not.toHaveTextContent("Reply removed because this chat was merged.");
    fireEvent.click(await sendButton());
    await waitFor(() => expect(sent).toHaveBeenCalled());
    expect(sent.mock.calls[0][0]).not.toHaveProperty("replyTo");
    expect(sent.mock.calls[0][0]).toMatchObject({ id: "draft-conv-reaction-text-2", text: "Another draft" });
  });
});

describe("reply cancelled during a deferred send flush", () => {
  it("still sends the reply shown at Send when its chip is cancelled while the flush is pending", async () => {
    const { save, release } = holdSave();
    const sent = vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "queued-local", revision: "2" });
    await openConversation("conv-reaction-text");
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    fireEvent.change(composerText(), { target: { value: "Sure, 10 works" } });
    fireEvent.click(await sendButton());
    await waitFor(() => expect(save).toHaveBeenCalled());
    fireEvent.click(screen.getByRole("button", { name: /Cancel reply to/ }));
    expect(chip()).toBeNull();
    await release();
    await waitFor(() => expect(sent).toHaveBeenCalledTimes(1));
    expect(sent.mock.calls[0][0]).toMatchObject({ replyTo: "reaction-text-incoming", text: "Sure, 10 works" });
  });
});

describe("reply selection while a send is pending", () => {
  /** Holds the first send_draft until `settle` is called, then answers with `outcome`. The rest go to the fixture host. */
  function holdFirstSend(outcome: Awaited<ReturnType<typeof fixtureBridge.send_draft>> | "reply-unavailable") {
    let settle!: () => void;
    const pending = new Promise<void>((resolve) => { settle = resolve; });
    const sent = vi.spyOn(bridge, "send_draft")
      .mockImplementationOnce(async () => {
        await pending;
        if (outcome === "reply-unavailable") throw { code: "reply-unavailable", message: "The message you're replying to is no longer available." };
        return outcome;
      })
      .mockImplementation(fixtureBridge.send_draft);
    return { sent, settle: () => act(async () => { settle(); }) };
  }

  it("keeps a reply chosen again on the same message when an older send is accepted", async () => {
    await openConversation("conv-reaction-text");
    const { sent, settle } = holdFirstSend({ accepted: true, status: "queued-local", revision: "2" });
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await typeAndSend("Sure, 10 works");
    await waitFor(() => expect(sent).toHaveBeenCalledTimes(1));
    // The user chooses Reply on the same message again while the first send is in flight: a new selection.
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await settle();
    await waitFor(() => expect(composerText().value).toBe(""));
    expect(chip()).toHaveTextContent("Running late, start without me");
    expect(sent.mock.calls[0][0]).toMatchObject({ replyTo: "reaction-text-incoming" });
  });

  it("keeps a reply chosen again on the same message when an older send is refused as unavailable", async () => {
    await openConversation("conv-reaction-text");
    const { sent, settle } = holdFirstSend("reply-unavailable");
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await typeAndSend("Sure, 10 works");
    await waitFor(() => expect(sent).toHaveBeenCalledTimes(1));
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await settle();
    await waitFor(() => expect(document.getElementById("composer-area")).toHaveTextContent("The message you're replying to is no longer available."));
    expect(chip()).toHaveTextContent("Running late, start without me");
    expect(sent).toHaveBeenCalledTimes(1);
  });

  it("clears the chip when the send that chose it is accepted and no newer reply exists", async () => {
    await openConversation("conv-reaction-text");
    const { sent, settle } = holdFirstSend({ accepted: true, status: "queued-local", revision: "2" });
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await typeAndSend("Sure, 10 works");
    await waitFor(() => expect(sent).toHaveBeenCalledTimes(1));
    await settle();
    await waitFor(() => expect(chip()).toBeNull());
  });
});

describe("reply and reaction announcements across conversations", () => {
  const announcer = () => document.getElementById("reaction-announcer");
  const alertRegion = () => document.getElementById("reaction-alert");
  const settle = () => act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });

  it("does not replay a reply announcement after navigating to another conversation in the same session", async () => {
    await openConversation("conv-reaction-text", ["conv-sam"]);
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(announcer()).toHaveTextContent(/^Replying to /));
    fireEvent.click(document.querySelector("[data-conversation-row='conv-sam'] .conversation-select")!);
    await waitFor(() => expect(document.querySelector("[data-conversation-row='conv-sam'][aria-current], [data-conversation-row='conv-sam'].selected")).not.toBeNull());
    await settle();
    expect(announcer()!.textContent).toBe("");
    expect(alertRegion()!.textContent).toBe("");
  });

  it("drops a reaction completion that resolves after navigating away", async () => {
    let resolveReaction!: (value: { accepted: boolean; reason?: "offline" }) => void;
    vi.spyOn(bridge, "react_to_message").mockReturnValue(new Promise((done) => { resolveReaction = done; }));
    await openConversation("conv-reaction-text", ["conv-sam"]);
    const article = document.querySelector<HTMLElement>('article[data-message-id="reaction-text-incoming"]')!;
    act(() => article.focus());
    fireEvent.keyDown(article, { key: "Enter" });
    fireEvent.click(document.querySelector('[data-reaction-quick="heart"]')!);
    await waitFor(() => expect(bridge.react_to_message).toHaveBeenCalled());
    fireEvent.click(document.querySelector("[data-conversation-row='conv-sam'] .conversation-select")!);
    await settle();
    await act(async () => resolveReaction({ accepted: false, reason: "offline" }));
    await settle();
    expect(alertRegion()!.textContent).toBe("");
    expect(announcer()!.textContent).toBe("");
  });

  it("drops a reaction completion from an earlier visit when the conversation is revisited (A→B→A)", async () => {
    let resolveReaction!: (value: { accepted: boolean; reason?: "offline" }) => void;
    vi.spyOn(bridge, "react_to_message").mockReturnValue(new Promise((done) => { resolveReaction = done; }));
    await openConversation("conv-reaction-text", ["conv-sam"]);
    const reactionArticle = document.querySelector<HTMLElement>('article[data-message-id="reaction-text-incoming"]')!;
    act(() => reactionArticle.focus());
    fireEvent.keyDown(reactionArticle, { key: "Enter" });
    fireEvent.click(document.querySelector('[data-reaction-quick="heart"]')!);
    await waitFor(() => expect(bridge.react_to_message).toHaveBeenCalled());
    fireEvent.click(document.querySelector("[data-conversation-row='conv-sam'] .conversation-select")!);
    await waitFor(() => expect(document.querySelector("[data-conversation-row='conv-sam'][aria-current], [data-conversation-row='conv-sam'].selected")).not.toBeNull());
    fireEvent.click(document.querySelector("[data-conversation-row='conv-reaction-text'] .conversation-select")!);
    await waitFor(() => expect(article("reaction-text-incoming")).toBeInTheDocument());
    await settle();
    await act(async () => resolveReaction({ accepted: false, reason: "offline" }));
    await settle();
    expect(alertRegion()!.textContent).toBe("");
    expect(announcer()!.textContent).toBe("");
  });

  it("announces a reaction completion in the conversation it was made in when nothing navigated away", async () => {
    let resolveReaction!: (value: { accepted: boolean; reason?: "offline" }) => void;
    vi.spyOn(bridge, "react_to_message").mockReturnValue(new Promise((done) => { resolveReaction = done; }));
    await openConversation("conv-reaction-text", ["conv-sam"]);
    const reactionArticle = document.querySelector<HTMLElement>('article[data-message-id="reaction-text-incoming"]')!;
    act(() => reactionArticle.focus());
    fireEvent.keyDown(reactionArticle, { key: "Enter" });
    fireEvent.click(document.querySelector('[data-reaction-quick="heart"]')!);
    await waitFor(() => expect(bridge.react_to_message).toHaveBeenCalled());
    await act(async () => resolveReaction({ accepted: false, reason: "offline" }));
    await waitFor(() => expect(alertRegion()!.textContent).toMatch(/offline/i));
  });
});

describe("reply target across lock", () => {
  const announcer = () => document.getElementById("reaction-announcer");

  it("drops the reply chip when the sync session locks and does not restore it on unlock", async () => {
    onTestFinished(() => { delete bridge.lock_sync; });
    snapshot.mode = "browser";
    snapshot.desktop = undefined;
    bridge.lock_sync = async () => { snapshot.encryption.state = "locked"; };
    vi.spyOn(bridge, "unlock_sync").mockImplementation(async () => { snapshot.encryption.state = "unlocked"; });
    await openConversation("conv-reaction-text");
    chooseReply("reaction-text-incoming");
    await waitFor(() => expect(chip()).not.toBeNull());
    await waitFor(() => expect(announcer()).toHaveTextContent(/^Replying to /));
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.click(await screen.findByRole("button", { name: "Lock now" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Unlock sync" })).toBeInTheDocument());
    expect(chip()).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Unlock sync" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Unlock sync" })).not.toBeInTheDocument());
    await screen.findByRole("button", { name: "Lock now" });
    fireEvent.click(document.querySelector("[data-conversation-row='conv-reaction-text'] .conversation-select")!);
    fireEvent.click(within(screen.getByRole("navigation", { name: "Main navigation" })).getByRole("button", { name: "Conversations" }));
    await waitFor(() => expect(composerText()).not.toBeNull());
    expect(chip()).toBeNull();
    // The announcement carries the sender's name. The announcer remounts with the conversation, so an announcement that survived
    // the lock would replay here on its next frame. Wait a frame, then assert both live regions are empty.
    await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
    expect(announcer()!.textContent).toBe("");
    expect(document.getElementById("reaction-alert")!.textContent).toBe("");
  });
});

describe("reaction hints when a conversation cannot take a reply", () => {
  it("says react or reply where Reply is offered", async () => {
    await openConversation("conv-reaction-text");
    expect(document.getElementById("message-reaction-hint")).toHaveTextContent(/^Press Enter to react or reply\.$/);
    expect(document.getElementById("message-reaction-hint-r")).toHaveTextContent(/^Press R to react or reply\.$/);
  });

  it("says react only in a conversation the user has left", async () => {
    await openConversation("conv-reaction-text", [], (conversation) => {
      conversation.group = { name: "Old crew", members: [], canCreate: false, createNeedsConfirmation: false, notices: [], state: "withdrawn", memberRevision: "4" };
    });
    expect(document.querySelector("[data-reaction-reply]")).toBeNull();
    expect(document.getElementById("message-reaction-hint-react")).toHaveTextContent(/^Press Enter to react\.$/);
    expect(document.getElementById("message-reaction-hint-r-react")).toHaveTextContent(/^Press R to react\.$/);
  });

  it("says react only on a message whose bar shows a notice instead of Reply", async () => {
    await openConversation("conv-phone-offline");
    act(() => article("reaction-offline").focus());
    expect(article("reaction-offline")).toHaveAttribute("aria-describedby", "message-reaction-hint-react");
    expect(document.getElementById("message-reaction-hint-react")).toHaveTextContent(/^Press Enter to react\.$/);
  });
});

