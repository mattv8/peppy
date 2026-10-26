import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot, type MessageView } from "./bridge";
import type { LocalSearchHit } from "@peppy/desktop-ui/bridge-contracts";

let snapshot: DesktopSnapshot;
let subscription: (() => void) | undefined;

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
  vi.spyOn(bridge, "subscribe").mockImplementation((listener) => {
    subscription = listener;
    return () => { subscription = undefined; };
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
    snapshot = { ...snapshot, encryption: { ...snapshot.encryption, state: "locked" } };
    await act(async () => subscription?.());
    await waitFor(() => expect(screen.getByRole("combobox", { name: "Search Peppy" })).toBeDisabled());

    await act(async () => around.resolve({ conversationId: "conv-aurora", messages: [message("locked-target")], hasOlder: false, hasNewer: false }));

    expect(screen.queryByText("locked-target")).not.toBeInTheDocument();
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



});
