import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App, REACTION_HIGHLIGHT_MS } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot, type MessageView } from "./bridge";

let snapshot: DesktopSnapshot;
let reactionStorage: Storage;

const message = (id: string, body: string, overrides: Partial<MessageView> = {}): MessageView => ({
  id,
  revision: "1",
  sender: "other",
  senderAddress: "+12025550123",
  body,
  timestamp: "1788435180000",
  attachments: [],
  ...overrides,
});

const searchPage = (items: Array<{ messageId: string; snippet: string; timestamp?: string }>, indexState: "ready" | "indexing" = "ready") => ({
  query: "",
  items: items.map((item) => ({ kind: "message" as const, conversationId: "conv-aurora", title: "Aurora", timestamp: item.timestamp ?? "1788435170000", ...item })),
  indexState,
  hasMore: { conversation: false, message: false, contact: false },
});

beforeEach(async () => {
  vi.restoreAllMocks();
  const values = new Map<string, string>();
  reactionStorage = {
    get length() { return values.size; },
    clear: () => values.clear(),
    getItem: (key) => values.get(key) ?? null,
    key: (index) => [...values.keys()][index] ?? null,
    removeItem: (key) => values.delete(key),
    setItem: (key, value) => values.set(key, value),
  };
  vi.stubGlobal("localStorage", reactionStorage);
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
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

function renderMessages(messages: MessageView[]) {
  snapshot.conversations[0].messages = messages;
  return render(<App hostKind="browser" />);
}

describe("App reactions", () => {
  it("renders default-on badges and notices with resolved actor names", async () => {
    renderMessages([message("plan", "plan?", { sender: "self", senderAddress: undefined }), message("reaction", "Reacted 😆 to “plan?”")]);

    const badges = await screen.findByRole("img", { name: /😆 from Aurora Chen/ });
    expect(badges).toHaveAttribute("data-reaction-badges", "plan");
    expect(document.querySelector('[data-reaction-notice="reaction"]')).toHaveTextContent("Aurora Chen reacted 😆 to “plan?”");
    expect([...document.querySelectorAll(".message-bubble")].some((bubble) => bubble.textContent?.includes("Reacted 😆"))).toBe(false);
    expect(document.querySelector('article [data-reaction-badges]')).toBeNull();
  });

  it("uses a one-to-one conversation name only when the sender has no contact", async () => {
    const senderAddress = "+15555550142";
    snapshot.conversations[0] = { ...snapshot.conversations[0], name: "Sam Rivera", participants: [senderAddress] };
    renderMessages([message("target", "plan?"), message("reaction", "Reacted 😆 to “plan?”", { senderAddress })]);
    await screen.findByRole("button", { name: "Sam Rivera reacted 😆 to “plan?”" });
  });

  it("keeps an unresolved group sender address distinct from the group name", async () => {
    const senderAddress = "+15555550142";
    snapshot.conversations[0] = { ...snapshot.conversations[0], name: "Weekend group", participants: [senderAddress, "+15555550143"] };
    renderMessages([message("target", "plan?"), message("reaction", "Reacted 😆 to “plan?”", { senderAddress })]);
    await screen.findByRole("button", { name: `${senderAddress} reacted 😆 to “plan?”` });
  });

  it("focuses and clears the highlight when a linked notice opens", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    renderMessages([message("plan", "plan?", { sender: "self", senderAddress: undefined }), message("reaction", "Reacted 😆 to “plan?”")]);
    const notice = await screen.findByRole("button", { name: "Aurora Chen reacted 😆 to “plan?”" });
    fireEvent.click(notice);
    const target = document.querySelector<HTMLElement>('[data-message-id="plan"]')!;
    expect(target).toHaveAttribute("data-reaction-highlight", "true");
    expect(target).toHaveFocus();
    expect(Element.prototype.scrollIntoView).toHaveBeenCalled();
    await act(async () => { await vi.advanceTimersByTimeAsync(REACTION_HIGHLIGHT_MS + 50); });
    expect(target).not.toHaveAttribute("data-reaction-highlight");
    expect(target).not.toHaveAttribute("tabindex");
    expect(bridge.search_local).not.toHaveBeenCalled();
  });

  it("shows status metadata for failed self notices and restores raw messages when disabled", async () => {
    renderMessages([message("target", "plan?"), message("reaction", "Reacted 😆 to “plan?”", { sender: "self", senderAddress: undefined, status: "failed-confirmed" })]);
    await screen.findByRole("button", { name: "You reacted 😆 to “plan?”" });
    expect(document.querySelector('[data-message-meta="reaction"]')).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    const toggle = await screen.findByRole("switch", { name: "Show reactions" });
    expect(toggle).toBeChecked();
    fireEvent.click(toggle);
    expect(reactionStorage.getItem("peppy.reactions.v1")).toBe('{"show":false}');
    fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
    await waitFor(() => expect(document.querySelector(".message-bubble")?.textContent).toContain("plan?"));
    expect(document.querySelector("[data-reaction-notice]")).toBeNull();
    expect(document.querySelectorAll(".message-bubble")[1]).toHaveTextContent("Reacted 😆 to “plan?”");
  });

  it("warns for missing targets and drops a stale lookup", async () => {
    let resolveSearch!: (page: ReturnType<typeof searchPage>) => void;
    vi.mocked(bridge.search_local).mockReturnValue(new Promise((resolve) => { resolveSearch = resolve; }));
    renderMessages([message("reaction", "Reacted 😆 to “deep text”")]);
    fireEvent.click(await screen.findByRole("button", { name: "Aurora Chen reacted 😆 to “deep text”" }));
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    await act(async () => resolveSearch(searchPage([])));
    expect(screen.queryByText("The original message couldn't be found.")).not.toBeInTheDocument();
  });

  it("explains when a capped search could not prove the target is absent", async () => {
    vi.mocked(bridge.search_local).mockResolvedValue({ ...searchPage([]), nextCursor: "more" });
    renderMessages([message("reaction", "Reacted 😆 to “deep text”")]);
    fireEvent.click(await screen.findByRole("button", { name: "Aurora Chen reacted 😆 to “deep text”" }));
    await screen.findByText("The original message wasn't found in the searched results. Load more history and try again.");
    expect(document.querySelector('[data-reaction-feedback="reaction"]')).toHaveTextContent("The original message wasn't found in the searched results. Load more history and try again.");
    expect(screen.queryByText("The original message couldn't be found.")).not.toBeInTheDocument();
    expect(bridge.search_local).toHaveBeenCalledTimes(4);
  });

  it("clears inline feedback for a new lookup and navigation", async () => {
    let resolveSearch!: (page: ReturnType<typeof searchPage>) => void;
    vi.mocked(bridge.search_local)
      .mockResolvedValueOnce(searchPage([]))
      .mockReturnValueOnce(new Promise((resolve) => { resolveSearch = resolve; }));
    renderMessages([message("reaction", "Reacted 😆 to “deep text”")]);
    const notice = await screen.findByRole("button", { name: "Aurora Chen reacted 😆 to “deep text”" });
    fireEvent.click(notice);
    await screen.findByText("The original message couldn't be found.");
    expect(document.querySelector('[data-reaction-feedback="reaction"]')).toBeInTheDocument();
    fireEvent.click(notice);
    expect(document.querySelector('[data-reaction-feedback="reaction"]')).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
    await act(async () => resolveSearch(searchPage([])));
    expect(document.querySelector('[data-reaction-feedback="reaction"]')).toBeNull();
  });

  it("lets a newer loaded target supersede an unresolved lookup", async () => {
    let resolveSearch!: (page: ReturnType<typeof searchPage>) => void;
    vi.mocked(bridge.search_local).mockReturnValue(new Promise((resolve) => { resolveSearch = resolve; }));
    renderMessages([
      message("plan", "plan?", { sender: "self", senderAddress: undefined }),
      message("reaction-loaded", "Reacted 😆 to “plan?”"),
      message("reaction-deep", "Reacted 😆 to “deep text”"),
    ]);
    const deepNotice = await screen.findByRole("button", { name: "Aurora Chen reacted 😆 to “deep text”" });
    deepNotice.focus();
    fireEvent.click(deepNotice);
    expect(deepNotice).toHaveAttribute("aria-disabled", "true");
    expect(deepNotice).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "Aurora Chen reacted 😆 to “plan?”" }));
    const target = document.querySelector<HTMLElement>('[data-message-id="plan"]')!;
    expect(target).toHaveAttribute("data-reaction-highlight", "true");
    await act(async () => {
      resolveSearch(searchPage([{ messageId: "deep", snippet: "deep text" }]));
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    expect(bridge.load_message_window).not.toHaveBeenCalled();
    expect(target).toHaveAttribute("data-reaction-highlight", "true");
    expect(screen.getByRole("button", { name: "Aurora Chen reacted 😆 to “deep text”" })).not.toHaveAttribute("aria-disabled");
  });

  it.each(["found", "candidate"])("clears locating after successful %s navigation", async (matchState) => {
    const quote = matchState === "candidate" ? `${"word ".repeat(40)}target` : "deep text";
    const target = message("deep", quote);
    const reaction = message("reaction", `Reacted 😆 to “${quote}”`);
    const snippet = matchState === "candidate" ? "word ".repeat(32) : quote;
    vi.mocked(bridge.search_local).mockResolvedValue(searchPage([{ messageId: target.id, snippet }]));
    vi.mocked(bridge.load_message_window).mockResolvedValue({ conversationId: "conv-aurora", messages: [target, reaction], hasOlder: false, hasNewer: false });
    renderMessages([reaction]);

    fireEvent.click(await screen.findByRole("button", { name: `Aurora Chen reacted 😆 to “${quote}”` }));
    await waitFor(() => expect(document.querySelector('[data-search-target="true"]')).toHaveAttribute("data-message-id", target.id));
    const retainedNotice = screen.getByRole("button", { name: `Aurora Chen reacted 😆 to “${quote}”` });
    expect(retainedNotice).not.toHaveAttribute("aria-busy");
    expect(retainedNotice).not.toHaveAttribute("aria-disabled");
  });

  it("drops a lookup that becomes stale after leaving and returning to conversations", async () => {
    let resolveSearch!: (page: ReturnType<typeof searchPage>) => void;
    vi.mocked(bridge.search_local).mockReturnValue(new Promise((resolve) => { resolveSearch = resolve; }));
    renderMessages([message("reaction", "Reacted 😆 to “deep text”")]);
    fireEvent.click(await screen.findByRole("button", { name: "Aurora Chen reacted 😆 to “deep text”" }));
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
    await act(async () => resolveSearch(searchPage([{ messageId: "deep", snippet: "deep text" }])));
    expect(bridge.load_message_window).not.toHaveBeenCalled();
    expect(screen.queryByText("The original message couldn't be found.")).not.toBeInTheDocument();
  });

  it("keeps focus on a notice when lookup fails", async () => {
    vi.mocked(bridge.search_local).mockRejectedValue(new Error("offline"));
    renderMessages([message("reaction", "Reacted 😆 to “deep text”")]);
    const notice = await screen.findByRole("button", { name: "Aurora Chen reacted 😆 to “deep text”" });
    notice.focus();
    fireEvent.click(notice);
    await screen.findByText("The original message couldn't be found.");
    expect(notice).toHaveFocus();
  });

  it("verifies a clipped candidate body before navigating", async () => {
    const prefix = "a ".repeat(80);
    const quotedText = `${prefix}remainder`;
    const candidate = message("deep", `${prefix}wrong body`);
    vi.mocked(bridge.search_local).mockResolvedValue(searchPage([{ messageId: "deep", snippet: prefix }]));
    vi.mocked(bridge.load_message_window).mockResolvedValue({ conversationId: "conv-aurora", messages: [candidate], hasOlder: false, hasNewer: false });
    renderMessages([message("reaction", `Reacted 😆 to “${quotedText}”`)]);
    fireEvent.click(await screen.findByRole("button", { name: `Aurora Chen reacted 😆 to “${quotedText}”` }));
    await screen.findByText("The original message couldn't be found.");
    expect(bridge.load_message_window).toHaveBeenCalledWith({ direction: "around", conversationId: "conv-aurora", messageId: "deep" });
  });
});
