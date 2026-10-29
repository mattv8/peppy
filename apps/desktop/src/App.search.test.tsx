import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot, type MessageView } from "./bridge";
import type { HistorySearchSnapshot, LocalSearchHit } from "@peppy/desktop-ui/bridge-contracts";

let snapshot: DesktopSnapshot;
let subscription: (() => void) | undefined;
const subscriptionListeners = new Set<() => void>();
const broadcastSubscription = () => {
  for (const listener of [...subscriptionListeners]) listener();
};

const message = (id: string, body = id): MessageView => ({
  id,
  revision: "1",
  sender: "other",
  body,
  timestamp: "1788435180000",
  attachments: [],
});


const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((accept, decline) => { resolve = accept; reject = decline; });
  return { promise, resolve, reject };
};

const pageFor = (query: string, items: LocalSearchHit[]) => ({
  query,
  items,
  indexState: "ready" as const,
  hasMore: { conversation: false, message: false, contact: false },
});

async function activate(hit: LocalSearchHit) {
  vi.mocked(bridge.search_local).mockImplementation(async ({ query }) => pageFor(query, [hit]));
  const input = screen.getByRole("combobox", { name: "Search Peppy" });
  fireEvent.change(input, { target: { value: hit.kind === "contact" ? "contact" : hit.kind === "message" ? hit.messageId : hit.conversationId } });
  await waitFor(() => expect(document.querySelector("[data-search-hit]")).not.toBeNull());
  fireEvent.click(document.querySelector<HTMLElement>("[data-search-hit]")!);
}

beforeEach(async () => {
  vi.restoreAllMocks();
  snapshot = structuredClone(await fixtureBridge.load_state());
  snapshot.encryption.profileFingerprint = "profile-a";
  vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId) => ({
    ...structuredClone(snapshot),
    activeConversationId: conversationId ?? snapshot.activeConversationId,
  }));
  vi.spyOn(bridge, "search_local").mockImplementation(fixtureBridge.search_local);
  vi.spyOn(bridge, "load_message_window").mockImplementation(fixtureBridge.load_message_window);
  subscriptionListeners.clear();
  subscription = undefined;
  vi.spyOn(bridge, "subscribe").mockImplementation((listener) => {
    subscriptionListeners.add(listener);
    subscription = broadcastSubscription;
    return () => { subscriptionListeners.delete(listener); };
  });
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("App search navigation", () => {
  it("projects message search titles with the resolved conversation context", async () => {
    vi.mocked(bridge.search_local).mockResolvedValue(pageFor("message", [
      { kind: "message", conversationId: "conv-aurora", messageId: "generic", title: "Message", snippet: "Body", timestamp: "1788435180000" },
      { kind: "message", conversationId: "conv-aurora", messageId: "subject", title: "MMS subject", snippet: "Picture", timestamp: "1788435180000" },
    ]));
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "message" } });

    await waitFor(() => expect(document.querySelector('[data-search-hit="message-conv-aurora-generic"]')).toHaveTextContent("Aurora Chen"));
    expect(screen.getByRole("option", { name: /MMS subject/ })).toHaveTextContent("Aurora Chen · MMS subject");
  });

  it("loads a bounded around window, adopts metadata, and marks the exact target", async () => {
    const target = message("deep-target", "Deep target");
    vi.mocked(bridge.load_message_window).mockResolvedValue({
      conversationId: "conv-aurora",
      messages: [message("before"), target, message("after")],
      hasOlder: true,
      hasNewer: true,
    });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");

    await activate({ kind: "message", conversationId: "conv-aurora", messageId: target.id, title: "Aurora", snippet: target.body, timestamp: target.timestamp });

    await waitFor(() => expect(document.querySelector('[data-search-target="true"]')).toHaveAttribute("data-message-id", target.id));
    expect(bridge.load_state).toHaveBeenCalledWith("conv-aurora", undefined, { metadataOnly: true });
    expect(bridge.load_message_window).toHaveBeenCalledWith({ direction: "around", conversationId: "conv-aurora", messageId: target.id });
    expect(document.querySelector('[data-search-target="true"]')).toHaveAttribute("tabindex", "-1");
  });

  it("refreshes only the detached visible ids with metadata-only state", async () => {
    const rows = [message("a"), message("target"), message("z")];
    vi.mocked(bridge.load_message_window).mockResolvedValueOnce({ conversationId: "conv-aurora", messages: rows, hasOlder: true, hasNewer: true });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    await screen.findByText("target");
    vi.mocked(bridge.load_message_window).mockResolvedValueOnce({ conversationId: "conv-aurora", messages: rows.slice(1), hasOlder: false, hasNewer: true });

    await act(async () => subscription?.());

    await waitFor(() => expect(bridge.load_message_window).toHaveBeenLastCalledWith({ direction: "refresh", conversationId: "conv-aurora", messageIds: ["a", "target", "z"] }));
    expect(bridge.load_state).toHaveBeenLastCalledWith("conv-aurora", undefined, { metadataOnly: true });
    expect(bridge.load_state).not.toHaveBeenCalledWith("conv-aurora", expect.any(String));
  });

  it("keeps detached metadata-only refreshes when a trailing state hint is queued", async () => {
    const rows = [message("a"), message("target"), message("z")];
    const firstState = deferred<DesktopSnapshot>();
    vi.mocked(bridge.load_message_window).mockResolvedValue({ conversationId: "conv-aurora", messages: rows, hasOlder: true, hasNewer: true });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    vi.mocked(bridge.load_state).mockReturnValueOnce(firstState.promise);

    await act(async () => subscription?.());
    await waitFor(() => expect(bridge.load_state).toHaveBeenLastCalledWith("conv-aurora", undefined, { metadataOnly: true }));
    await act(async () => subscription?.());
    await act(async () => firstState.resolve({ ...snapshot, activeConversationId: "conv-aurora" }));

    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledTimes(4));
    await waitFor(() => expect(bridge.load_message_window).toHaveBeenCalledWith({ direction: "refresh", conversationId: "conv-aurora", messageIds: ["a", "target", "z"] }));
    expect(bridge.load_state).toHaveBeenLastCalledWith("conv-aurora", undefined, { metadataOnly: true });
  });

  it("does not refresh an empty window while the around request is pending", async () => {
    let resolveAround!: (value: Awaited<ReturnType<typeof bridge.load_message_window>>) => void;
    vi.mocked(bridge.load_message_window).mockReturnValueOnce(new Promise(resolve => { resolveAround = resolve; }));
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "pending-target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    await waitFor(() => expect(bridge.load_message_window).toHaveBeenCalledTimes(1));

    await act(async () => subscription?.());

    expect(bridge.load_message_window).toHaveBeenCalledTimes(1);
    expect(bridge.load_message_window).not.toHaveBeenCalledWith(expect.objectContaining({ direction: "refresh" }));
    await act(async () => resolveAround({ conversationId: "conv-aurora", messages: [message("pending-target")], hasOlder: false, hasNewer: false }));
    await screen.findByText("pending-target");
  });

  it("ignores an older same-conversation target after a newer activation wins", async () => {
    let resolveFirst!: (value: Awaited<ReturnType<typeof bridge.load_message_window>>) => void;
    const first = new Promise<Awaited<ReturnType<typeof bridge.load_message_window>>>(resolve => { resolveFirst = resolve; });
    vi.mocked(bridge.load_message_window)
      .mockReturnValueOnce(first)
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [message("second", "Second wins")], hasOlder: false, hasNewer: false });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");

    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "first", title: "Aurora", snippet: "First", timestamp: "1788435180000" });
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "second", title: "Aurora", snippet: "Second", timestamp: "1788435180000" });
    await screen.findByText("Second wins");
    await act(async () => resolveFirst({ conversationId: "conv-aurora", messages: [message("first", "First loses")], hasOlder: false, hasNewer: false }));

    expect(screen.queryByText("First loses")).not.toBeInTheDocument();
    expect(document.querySelector('[data-search-target="true"]')).toHaveAttribute("data-message-id", "second");
  });

  it("caps older paging, exposes the newer edge, and returns to latest explicitly", async () => {
    const around = Array.from({ length: 150 }, (_, index) => message(`around-${index}`));
    const older = Array.from({ length: 50 }, (_, index) => message(`older-${index}`));
    vi.mocked(bridge.load_message_window)
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: around, hasOlder: true, hasNewer: false })
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: older, hasOlder: false, hasNewer: false })
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [...around.slice(100), message("newest")], hasOlder: true, hasNewer: false });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "around-75", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    await screen.findByText("around-75");

    fireEvent.click(screen.getByRole("button", { name: "Load older messages" }));

    await waitFor(() => expect(bridge.load_message_window).toHaveBeenLastCalledWith({ direction: "older", conversationId: "conv-aurora", messageId: "around-0" }));
    await screen.findByRole("button", { name: "Load newer messages" });
    expect(document.querySelectorAll("[data-message-id]")).toHaveLength(150);
    fireEvent.click(screen.getByRole("button", { name: "Load newer messages" }));
    await waitFor(() => expect(bridge.load_message_window).toHaveBeenLastCalledWith({ direction: "newer", conversationId: "conv-aurora", messageId: "around-99" }));
    await screen.findByText("newest");
    expect(document.querySelectorAll("[data-message-id]")).toHaveLength(150);
    fireEvent.click(screen.getByRole("button", { name: "Go to latest" }));
    await waitFor(() => expect(document.getElementById("detached-history-status")).not.toBeInTheDocument());
    expect(bridge.load_state).toHaveBeenCalledWith("conv-aurora");
  });

  it("exits detached history when the current conversation is selected again", async () => {
    vi.mocked(bridge.load_message_window).mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [message("same-thread-target")], hasOlder: false, hasNewer: true });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "same-thread-target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    await screen.findByRole("button", { name: "Go to latest" });
    vi.mocked(bridge.load_state).mockClear();

    await activate({ kind: "conversation", conversationId: "conv-aurora", title: "Aurora", subtitle: "Current" });

    await waitFor(() => expect(document.getElementById("detached-history-status")).not.toBeInTheDocument());
    expect(bridge.load_state).toHaveBeenCalledWith("conv-aurora");
  });

  it("returns to latest after a successful send from detached history", async () => {
    vi.spyOn(bridge, "save_draft").mockImplementation(async input => ({ ...input, id: "draft-search", conversationId: "conv-aurora", revision: "2" }));
    vi.spyOn(bridge, "send_draft").mockResolvedValue({ accepted: true, status: "queued-local" });
    vi.mocked(bridge.load_message_window).mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [message("send-target")], hasOlder: false, hasNewer: true });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "send-target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    await screen.findByRole("button", { name: "Go to latest" });
    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), { target: { value: "Sent from search history" } });
    await waitFor(() => expect(bridge.save_draft).toHaveBeenCalled());
    vi.mocked(bridge.load_state).mockClear();

    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    await waitFor(() => expect(bridge.send_draft).toHaveBeenCalled());
    await waitFor(() => expect(document.getElementById("detached-history-status")).not.toBeInTheDocument());
    expect(bridge.load_state).toHaveBeenCalledWith("conv-aurora");
    expect(bridge.load_state).not.toHaveBeenCalledWith("conv-aurora", "send-target");
  });

  it("clears detached history when the encrypted profile session changes", async () => {
    const rows = [message("profile-target")];
    vi.mocked(bridge.load_message_window)
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: rows, hasOlder: false, hasNewer: true })
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: rows, hasOlder: false, hasNewer: true });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "profile-target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    await screen.findByRole("button", { name: "Go to latest" });

    vi.mocked(bridge.load_state).mockClear();
    snapshot = { ...snapshot, encryption: { ...snapshot.encryption, profileFingerprint: "profile-b" } };
    await act(async () => subscription?.());

    await waitFor(() => expect(document.getElementById("detached-history-status")).not.toBeInTheDocument());
    expect(document.querySelector('[data-search-target="true"]')).not.toBeInTheDocument();
    expect(bridge.load_state).toHaveBeenCalledWith("conv-aurora");
  });

  it("keeps an around commit when an older metadata tick resolves late", async () => {
    const around = deferred<Awaited<ReturnType<typeof bridge.load_message_window>>>();
    const lateMetadata = deferred<DesktopSnapshot>();
    vi.mocked(bridge.load_message_window).mockReturnValueOnce(around.promise);
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "race-target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    vi.mocked(bridge.load_state).mockReturnValueOnce(lateMetadata.promise);
    await act(async () => subscription?.());

    await act(async () => around.resolve({ conversationId: "conv-aurora", messages: [message("race-target")], hasOlder: true, hasNewer: true }));
    await screen.findByText("race-target");
    await act(async () => lateMetadata.resolve(structuredClone(snapshot)));

    expect(screen.getByText("race-target")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Go to latest" })).toBeInTheDocument();
  });

  it("does not let a late detached refresh roll back a committed older page", async () => {
    const rows = Array.from({ length: 100 }, (_, index) => message(`window-${index}`));
    const lateMetadata = deferred<DesktopSnapshot>();
    const lateRefresh = deferred<Awaited<ReturnType<typeof bridge.load_message_window>>>();
    vi.mocked(bridge.load_message_window).mockResolvedValueOnce({ conversationId: "conv-aurora", messages: rows, hasOlder: true, hasNewer: true });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "window-50", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    await screen.findByText("window-50");
    vi.mocked(bridge.load_state).mockReturnValueOnce(lateMetadata.promise);
    vi.mocked(bridge.load_message_window).mockReturnValueOnce(lateRefresh.promise).mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [message("older-row")], hasOlder: false, hasNewer: true });
    act(() => subscription?.());
    await act(async () => lateMetadata.resolve(structuredClone(snapshot)));
    await waitFor(() => expect(bridge.load_message_window).toHaveBeenCalledWith(expect.objectContaining({ direction: "refresh" })));

    fireEvent.click(screen.getByRole("button", { name: "Load older messages" }));
    await screen.findByText("older-row");
    await act(async () => lateRefresh.resolve({ conversationId: "conv-aurora", messages: rows, hasOlder: true, hasNewer: true }));

    expect(screen.getByText("older-row")).toBeInTheDocument();
  });

  it("does not refocus the old target when newer paging completes", async () => {
    const newer = deferred<Awaited<ReturnType<typeof bridge.load_message_window>>>();
    vi.mocked(bridge.load_message_window)
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [message("focus-target")], hasOlder: false, hasNewer: true })
      .mockReturnValueOnce(newer.promise);
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "focus-target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    await screen.findByText("focus-target");
    const composer = screen.getByRole("textbox", { name: "Message" });
    fireEvent.click(screen.getByRole("button", { name: "Load newer messages" }));
    composer.focus();
    await act(async () => newer.resolve({ conversationId: "conv-aurora", messages: [message("newer-row")], hasOlder: true, hasNewer: false }));

    expect(document.activeElement).toBe(composer);
  });

  it("clears a vanished conversation instead of leaving an endless loading pane", async () => {
    vi.mocked(bridge.load_message_window).mockRejectedValueOnce({ code: "not-found", message: "gone" });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    vi.mocked(bridge.load_state)
      .mockResolvedValueOnce(structuredClone(snapshot))
      .mockResolvedValueOnce({ ...structuredClone(snapshot), activeConversationId: "conv-river" });
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "vanished", title: "Aurora", snippet: "gone", timestamp: "1788435180000" });

    await waitFor(() => expect(document.getElementById("message-list")).not.toHaveAttribute("aria-busy", "true"));
    expect(document.querySelector('[data-conversation-id="conv-aurora"]')).not.toHaveAttribute("aria-current", "location");
  });

  it("falls back to latest when the around target disappeared", async () => {
    vi.mocked(bridge.load_message_window).mockRejectedValueOnce({ code: "not-found", message: "gone" });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");

    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "gone", title: "Aurora", snippet: "Gone", timestamp: "1788435180000" });

    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledWith("conv-aurora"));
    expect(await screen.findByText(/no longer available/i)).toBeInTheDocument();
  });

  it.each([
    { kind: "conversation", conversationId: "conv-river", title: "River", subtitle: "Recent" } as LocalSearchHit,
    { kind: "message", conversationId: "conv-river", messageId: "guard-target", title: "River", snippet: "Guarded", timestamp: "1788435180000" } as LocalSearchHit,
    { kind: "contact", bookId: "book-pixel8", contactId: "contact-river", title: "River Park", subtitle: "+12025550124" } as LocalSearchHit,
  ])("cancels $kind search activation before any App navigation side effect", async (hit) => {
    const saveDraft = vi.spyOn(bridge, "save_draft");
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    fireEvent.click(screen.getByRole("button", { name: /^Contacts/ }));
    fireEvent.click(await screen.findByRole("button", { name: /Aurora Chen/ }));
    fireEvent.click(screen.getByLabelText("Edit contact"));
    fireEvent.change(screen.getByLabelText("Given name"), { target: { value: "Unsaved" } });
    await waitFor(() => expect(screen.getByLabelText("Given name")).toHaveValue("Unsaved"));
    vi.mocked(bridge.load_state).mockClear();
    vi.mocked(bridge.load_message_window).mockClear();
    saveDraft.mockClear();

    await activate(hit);
    fireEvent.click(await screen.findByRole("button", { name: "Keep editing" }));

    expect(screen.getByLabelText("Given name")).toHaveValue("Unsaved");
    expect(saveDraft).not.toHaveBeenCalled();
    expect(bridge.load_state).not.toHaveBeenCalled();
    expect(bridge.load_message_window).not.toHaveBeenCalled();
    expect(document.querySelector('[data-conversation-id="conv-aurora"]')).toHaveAttribute("aria-current", "location");
  });

  it("invalidates an around request when the encrypted session locks", async () => {
    const around = deferred<Awaited<ReturnType<typeof bridge.load_message_window>>>();
    vi.mocked(bridge.load_message_window).mockReturnValueOnce(around.promise);
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "locked-target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    vi.mocked(bridge.load_state).mockResolvedValueOnce({
      ...structuredClone(snapshot),
      connection: { state: "offline", origin: snapshot.connection.origin, errorCode: "app-locked" },
      encryption: { state: "locked" },
      conversations: [],
      notifications: [],
      activeConversationId: undefined,
      draft: undefined,
    });
    await act(async () => subscription?.());
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Search Peppy" })).toBeDisabled());
    expect(screen.queryAllByText("Can you send over the estimate?")).toHaveLength(0);

    await act(async () => around.resolve({ conversationId: "conv-aurora", messages: [message("locked-target")], hasOlder: false, hasNewer: false }));

    expect(screen.queryByText("locked-target")).not.toBeInTheDocument();
    expect(screen.queryAllByText("Can you send over the estimate?")).toHaveLength(0);
    expect(document.getElementById("detached-history-status")).not.toBeInTheDocument();
  });

  it("does not mark the search target read before target positioning settles", async () => {
    const animationFrames: FrameRequestCallback[] = [];
    const observerCallbacks: ((entries: { target: Element; isIntersecting: boolean }[]) => void)[] = [];
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      animationFrames.push(callback);
      return animationFrames.length;
    });
    vi.stubGlobal("IntersectionObserver", class {
      constructor(private callback: (entries: { target: Element; isIntersecting: boolean }[]) => void) {
        observerCallbacks.push(callback);
      }
      observe() {}
      disconnect() {}
      unobserve() {}
      takeRecords() { return []; }
      root = null;
      rootMargin = "";
      thresholds = [];
    });
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    vi.mocked(bridge.load_message_window).mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [message("read-gated-target")], hasOlder: false, hasNewer: false });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    vi.mocked(bridge.mark_seen).mockClear();

    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "read-gated-target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    const row = await screen.findByText("read-gated-target").then(node => node.closest("[data-message-id]")!);
    observerCallbacks.at(-1)?.([{ target: row, isIntersecting: true }]);

    expect(bridge.mark_seen).not.toHaveBeenCalled();
  });

  it("consumes a contact target once so returning to Contacts does not reapply it", async () => {
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "contact", bookId: "book-pixel8", contactId: "contact-river", title: "River Park", subtitle: "+12025550124" });
    await waitFor(() => expect(screen.getByLabelText("Contact detail")).toHaveTextContent("River Park"));

    fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
    fireEvent.click(screen.getByRole("button", { name: /^Contacts/ }));

    await screen.findByRole("region", { name: "Contacts browser" });
    await waitFor(() => expect(document.getElementById("contacts-detail-panel")?.textContent ?? "").not.toContain("River Park"));
  });



  it("keeps the post-lock refresh unanchored after leaving detached history", async () => {
    vi.mocked(bridge.load_message_window)
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [message("lock-target")], hasOlder: false, hasNewer: true })
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [message("lock-target")], hasOlder: false, hasNewer: true });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "lock-target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    await screen.findByText("lock-target");
    vi.mocked(bridge.load_state).mockClear();

    snapshot = { ...snapshot, encryption: { ...snapshot.encryption, state: "locked" } };
    await act(async () => subscription?.());
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Search Peppy" })).toBeDisabled());
    snapshot = { ...snapshot, encryption: { ...snapshot.encryption, state: "unlocked" } };
    await act(async () => subscription?.());

    await waitFor(() => expect(bridge.load_state).toHaveBeenCalledWith("conv-aurora"));
    expect(bridge.load_state).not.toHaveBeenCalledWith("conv-aurora", "lock-target");
  });

  describe("search further back", () => {
    const historyStatus = (overrides: Partial<HistorySearchSnapshot> = {}): HistorySearchSnapshot => ({
      state: "idle",
      canSearchFurther: true,
      progressVersion: "0",
      indexVersion: "0",
      ...overrides,
    });
    const typeQuery = (value: string) => fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value } });
    const noHits = () => vi.mocked(bridge.search_local).mockImplementation(async ({ query }) => pageFor(query, []));

    it("reads history for a no-result query without starting work", async () => {
      const read = vi.spyOn(bridge, "get_history_search").mockResolvedValue(historyStatus());
      const start = vi.spyOn(bridge, "start_history_search");
      noHits();
      render(<App hostKind="browser" />);
      await screen.findAllByText("Can you send over the estimate?");

      typeQuery("nothing-here");

      await screen.findByRole("option", { name: "Search further back" });
      expect(read).toHaveBeenCalled();
      expect(start).not.toHaveBeenCalled();
    });

    it("starts history only from the explicit option, with an operation id alone", async () => {
      vi.spyOn(bridge, "get_history_search").mockResolvedValue(historyStatus());
      const start = vi.spyOn(bridge, "start_history_search").mockResolvedValue(historyStatus({ state: "catchingUp", canSearchFurther: false, attemptId: "attempt-1" }));
      noHits();
      render(<App hostKind="browser" />);
      await screen.findAllByText("Can you send over the estimate?");
      typeQuery("nothing-here");

      fireEvent.click(await screen.findByRole("option", { name: "Search further back" }));

      await waitFor(() => expect(start).toHaveBeenCalledTimes(1));
      const [input] = start.mock.calls[0];
      expect(Object.keys(input)).toEqual(["operationId"]);
      expect(input.operationId).toEqual(expect.any(String));
    });

    it("shows host index updates for the current query without retyping", async () => {
      let indexed = false;
      let status = historyStatus();
      const hit: LocalSearchHit = { kind: "message", conversationId: "conv-aurora", messageId: "generic", title: "Message", snippet: "Body", timestamp: "1788435180000" };
      vi.mocked(bridge.search_local).mockImplementation(async ({ query }) => pageFor(query, indexed ? [hit] : []));
      vi.spyOn(bridge, "get_history_search").mockImplementation(async () => structuredClone(status));
      render(<App hostKind="browser" />);
      await screen.findAllByText("Can you send over the estimate?");
      typeQuery("aurora");
      await screen.findByRole("option", { name: "Search further back" });

      indexed = true;
      status = historyStatus({ state: "completed", canSearchFurther: false, progressVersion: "1", indexVersion: "1" });
      await act(async () => subscription?.());

      await waitFor(() => expect(document.querySelector('[data-search-hit="message-conv-aurora-generic"]')).not.toBeNull(), { timeout: 4000 });
      expect(bridge.search_local).toHaveBeenLastCalledWith(expect.objectContaining({ query: "aurora" }));
    });

    it("cancels only the attempt this view started", async () => {
      let status = historyStatus();
      vi.spyOn(bridge, "get_history_search").mockImplementation(async () => structuredClone(status));
      vi.spyOn(bridge, "start_history_search").mockImplementation(async () => {
        status = historyStatus({ state: "catchingUp", canSearchFurther: false, attemptId: "attempt-1" });
        return structuredClone(status);
      });
      const cancel = vi.spyOn(bridge, "cancel_history_search").mockImplementation(async () => {
        status = historyStatus({ state: "cancelled", canSearchFurther: true, progressVersion: "2" });
        return structuredClone(status);
      });
      noHits();
      render(<App hostKind="browser" />);
      await screen.findAllByText("Can you send over the estimate?");
      typeQuery("nothing-here");
      fireEvent.click(await screen.findByRole("option", { name: "Search further back" }));

      fireEvent.click(await screen.findByRole("option", { name: "Cancel searching further back" }));

      await waitFor(() => expect(cancel).toHaveBeenCalledTimes(1));
      expect(cancel).toHaveBeenCalledWith({ attemptId: "attempt-1" });
    });

    it("cancels an attempt the host reports after the view remounts, without starting", async () => {
      vi.spyOn(bridge, "get_history_search").mockResolvedValue(historyStatus({ state: "catchingUp", canSearchFurther: false, attemptId: "attempt-remount" }));
      const start = vi.spyOn(bridge, "start_history_search");
      const cancel = vi.spyOn(bridge, "cancel_history_search").mockResolvedValue(historyStatus({ state: "cancelled", canSearchFurther: true, progressVersion: "2" }));
      noHits();
      render(<App hostKind="browser" />);
      await screen.findAllByText("Can you send over the estimate?");
      typeQuery("nothing-here");

      fireEvent.click(await screen.findByRole("option", { name: "Cancel searching further back" }));

      await waitFor(() => expect(cancel).toHaveBeenCalledWith({ attemptId: "attempt-remount" }));
      expect(start).not.toHaveBeenCalled();
    });

    it("shows no Retry for a permission block that core reports as not offering further search", async () => {
      vi.spyOn(bridge, "get_history_search").mockResolvedValue(historyStatus({ state: "blocked", reason: "permission", canSearchFurther: false }));
      const start = vi.spyOn(bridge, "start_history_search");
      noHits();
      render(<App hostKind="browser" />);
      await screen.findAllByText("Can you send over the estimate?");
      typeQuery("nothing-here");

      await waitFor(() => expect(document.getElementById("omni-search-history-status")).toHaveTextContent("Phone needs permission"));
      expect(screen.queryByRole("option", { name: "Retry loading history" })).toBeNull();
      expect(start).not.toHaveBeenCalled();
    });

    it("offers no history action for an unsupported source", async () => {
      vi.spyOn(bridge, "get_history_search").mockResolvedValue(historyStatus({ state: "blocked", reason: "unsupported", canSearchFurther: false }));
      const start = vi.spyOn(bridge, "start_history_search");
      noHits();
      render(<App hostKind="browser" />);
      await screen.findAllByText("Can you send over the estimate?");
      typeQuery("nothing-here");

      await waitFor(() => expect(document.getElementById("omni-search-history-status")).toHaveTextContent("No matches"));
      expect(document.querySelector('[data-search-action="history"]')).toBeNull();
      expect(screen.queryByRole("option", { name: "Search further back" })).toBeNull();
      expect(start).not.toHaveBeenCalled();
    });
  });

  it("retries a failed newer page in the newer direction, never as older history (M27)", async () => {
    const loadHistory = vi.spyOn(bridge, "load_history");
    const newerRetry = deferred<{ conversationId: string; messages: MessageView[]; hasOlder: boolean; hasNewer: boolean }>();
    vi.mocked(bridge.load_message_window)
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [message("newer-target")], hasOlder: true, hasNewer: true })
      .mockRejectedValueOnce({ code: "network", message: "offline" })
      .mockReturnValueOnce(newerRetry.promise);
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "newer-target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    await screen.findByText("newer-target");

    fireEvent.click(screen.getByRole("button", { name: "Load newer messages" }));
    const retry = await screen.findByRole("button", { name: "Retry loading newer messages" });
    retry.focus();
    fireEvent.click(retry);

    const loadNewer = screen.getByRole("button", { name: "Load newer messages" });
    expect(loadNewer).toBeDisabled();
    expect(document.getElementById("history-load-newer-indicator")).toHaveTextContent("Loading newer messages…");
    expect(document.getElementById("history-load-band")).toHaveFocus();
    fireEvent.click(loadNewer);
    expect(bridge.load_message_window).toHaveBeenCalledTimes(3);
    await act(async () => newerRetry.resolve({ conversationId: "conv-aurora", messages: [message("newer-row")], hasOlder: true, hasNewer: false }));

    await screen.findByText("newer-row");
    const directions = vi.mocked(bridge.load_message_window).mock.calls.map(([input]) => input.direction);
    expect(directions).toEqual(["around", "newer", "newer"]);
    expect(bridge.load_message_window).toHaveBeenLastCalledWith({ direction: "newer", conversationId: "conv-aurora", messageId: "newer-target" });
    expect(loadHistory).not.toHaveBeenCalled();
    expect(document.getElementById("history-load-error")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry loading newer messages" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Load older messages" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Retry loading older messages" })).not.toBeInTheDocument();
  });

  it("clears a newer-page error when returning to the latest messages", async () => {
    snapshot = {
      ...snapshot,
      conversations: snapshot.conversations.map((conversation) =>
        conversation.id === "conv-aurora" ? { ...conversation, hasOlderMessages: true } : conversation,
      ),
    };
    vi.mocked(bridge.load_message_window)
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [message("newer-target")], hasOlder: true, hasNewer: true })
      .mockRejectedValueOnce({ code: "network", message: "offline" });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "newer-target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    await screen.findByText("newer-target");

    fireEvent.click(screen.getByRole("button", { name: "Load newer messages" }));
    await screen.findByRole("button", { name: "Retry loading newer messages" });
    fireEvent.click(screen.getByRole("button", { name: "Go to latest" }));

    await waitFor(() => expect(document.getElementById("detached-history-status")).not.toBeInTheDocument());
    expect(document.getElementById("history-load-error")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry loading newer messages" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Load older messages" })).toBeEnabled();
  });

  it("uses the older retry after a conversation-changed older page follows a newer error", async () => {
    vi.mocked(bridge.load_message_window)
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [message("newer-target")], hasOlder: true, hasNewer: true })
      .mockRejectedValueOnce({ code: "network", message: "offline" })
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [message("newer-row")], hasOlder: true, hasNewer: false });
    vi.spyOn(bridge, "load_history").mockResolvedValueOnce({ conversationId: "other-conversation", messages: [], hasMore: true });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "newer-target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    await screen.findByText("newer-target");

    fireEvent.click(screen.getByRole("button", { name: "Load newer messages" }));
    fireEvent.click(await screen.findByRole("button", { name: "Retry loading newer messages" }));
    await screen.findByText("newer-row");
    fireEvent.click(screen.getByRole("button", { name: "Load older messages" }));

    expect(await screen.findByRole("button", { name: "Retry loading older messages" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Retry loading newer messages" })).not.toBeInTheDocument();
  });

  it("retries a failed older detached page in the older direction (M27)", async () => {
    vi.mocked(bridge.load_message_window)
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [message("older-target")], hasOlder: true, hasNewer: true })
      .mockRejectedValueOnce({ code: "network", message: "offline" })
      .mockResolvedValueOnce({ conversationId: "conv-aurora", messages: [message("older-row")], hasOlder: false, hasNewer: true });
    render(<App hostKind="browser" />);
    await screen.findAllByText("Can you send over the estimate?");
    await activate({ kind: "message", conversationId: "conv-aurora", messageId: "older-target", title: "Aurora", snippet: "target", timestamp: "1788435180000" });
    await screen.findByText("older-target");

    fireEvent.click(screen.getByRole("button", { name: "Load older messages" }));
    fireEvent.click(await screen.findByRole("button", { name: "Retry loading older messages" }));

    await screen.findByText("older-row");
    const directions = vi.mocked(bridge.load_message_window).mock.calls.map(([input]) => input.direction);
    expect(directions).toEqual(["around", "older", "older"]);
    expect(screen.queryByRole("button", { name: "Retry loading newer messages" })).not.toBeInTheDocument();
  });
});
