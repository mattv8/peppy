import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App, REACTION_HIGHLIGHT_MS } from "./App";
import { bridge, fixtureBridge, type DesktopSnapshot, type MessageView } from "./bridge";

let snapshot: DesktopSnapshot;
let reactionStorage: Storage;

class ReactionObserver {
  static instances: ReactionObserver[] = [];
  nodes = new Set<Element>();
  constructor(private callback: IntersectionObserverCallback, readonly options?: IntersectionObserverInit) { ReactionObserver.instances.push(this); }
  observe = (node: Element) => { this.nodes.add(node); };
  unobserve = (node: Element) => { this.nodes.delete(node); };
  disconnect = () => { this.nodes.clear(); };
  takeRecords = () => [];
  deliver(node: Element, ratio = 0.75, area = 20) {
    const bounds = { width: area, height: area, top: 0, bottom: area, left: 0, right: area, x: 0, y: 0, toJSON() {} };
    this.callback([{ target: node, isIntersecting: true, intersectionRatio: ratio, boundingClientRect: bounds, intersectionRect: bounds } as IntersectionObserverEntry], this as unknown as IntersectionObserver);
  }
  static intersect(node: Element, ratio = 0.75, area = 20) {
    for (const observer of [...this.instances]) {
      if (observer.nodes.has(node)) observer.deliver(node, ratio, area);
    }
  }
}

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
  vi.unstubAllGlobals();
  ReactionObserver.instances = [];
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
  const conversationId = "10000000-0000-4000-8000-000000000001";
  const reactionId = "20000000-0000-4000-8000-000000000001";
  const nextReactionId = "20000000-0000-4000-8000-000000000002";
  const seenKey = (id = reactionId) => `peppy.reactions.seen.v1:["fixture","https://push.example.com"]:["${conversationId}","${id}"]`;
  const enableObservation = () => {
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    vi.stubGlobal("IntersectionObserver", ReactionObserver);
  };
  const durableConversation = () => {
    snapshot.conversations[0].id = conversationId;
    snapshot.activeConversationId = conversationId;
  };
  const refreshMessages = async (messages: MessageView[]) => {
    snapshot.conversations[0].messages = messages;
    await act(async () => { vi.mocked(bridge.subscribe).mock.calls.at(-1)![0](); });
  };
  const reactionChip = () => document.querySelector('[data-reaction-emoji="😆"]')!;

  it("preserves an exact visible bubble offset when a visible notice above it collapses", async () => {
    enableObservation();
    renderMessages([message("plan", "plan?"), message("reaction", "Reacted 😆 to “plan?”"), message("anchor", "anchor")]);
    await screen.findByText("anchor");
    const list = document.getElementById("message-list")!;
    Object.defineProperties(list, { clientHeight: { configurable: true, value: 400 }, scrollHeight: { configurable: true, get: () => document.querySelector('[data-reaction-marker]') ? 1160 : 1200 } });
    list.scrollTop = 500;
    fireEvent.scroll(list);
    const bounds = (top: number, height = 20) => ({ width: 200, height, top, bottom: top + height, left: 0, right: 200, x: 0, y: top, toJSON() {} });
    vi.spyOn(list, "getBoundingClientRect").mockImplementation(() => bounds(0, 400));
    const originalBounds = HTMLElement.prototype.getBoundingClientRect;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      if (this.dataset.messageId === "reaction") return bounds(10);
      if (this.dataset.messageId === "anchor") return bounds(80 - (document.querySelector('[data-reaction-marker]') ? 40 : 0) - (list.scrollTop - 500));
      if (this.dataset.messageId === "plan") return bounds(-100);
      return originalBounds.call(this);
    });
    const textarea = document.getElementById("composer-textarea")!;
    act(() => textarea.focus());
    act(() => ReactionObserver.intersect(reactionChip()));
    await waitFor(() => expect(list.scrollTop).toBe(460));
    expect(document.querySelector('[data-message-id="anchor"]')!.getBoundingClientRect().top).toBe(80);
    expect(textarea).toHaveFocus();
    expect(bridge.load_message_window).not.toHaveBeenCalled();
  });

  it("keeps latest bottom pinned through a notice collapse", async () => {
    enableObservation();
    renderMessages([message("plan", "plan?"), message("reaction", "Reacted 😆 to “plan?”")]);
    await screen.findByRole("img", { name: /😆 from/ });
    const list = document.getElementById("message-list")!;
    Object.defineProperties(list, { clientHeight: { configurable: true, value: 400 }, scrollHeight: { configurable: true, get: () => document.querySelector('[data-reaction-marker]') ? 1160 : 1200 } });
    list.scrollTop = 800;
    fireEvent.scroll(list);
    act(() => ReactionObserver.intersect(reactionChip()));
    await waitFor(() => expect(list.scrollTop).toBe(1160));
    expect(bridge.load_message_window).not.toHaveBeenCalled();
  });

  it("rebinds the visible scroll anchor when suppressing a hint merges adjacent media groups", async () => {
    durableConversation();
    const imageMessage = (id: string) => message(id, "", { transport: "mms", attachments: [{ id: `${id}-file`, name: `${id}.jpg`, mediaType: "image/jpeg", byteSize: 100, state: "ready", previewUrl: "data:image/jpeg;base64,AA==" }] });
    renderMessages([imageMessage("first-image"), message(reactionId, "Reacted 😆 to “unloaded”"), imageMessage("second-image")]);
    await screen.findByRole("button", { name: /reacted 😆/ });
    const list = document.getElementById("message-list")!;
    Object.defineProperties(list, { clientHeight: { configurable: true, value: 400 }, scrollHeight: { configurable: true, value: 1200 } });
    list.scrollTop = 500;
    fireEvent.scroll(list);
    const bounds = (top: number, height = 20) => ({ width: 200, height, top, bottom: top + height, left: 0, right: 200, x: 0, y: top, toJSON() {} });
    vi.spyOn(list, "getBoundingClientRect").mockImplementation(() => bounds(0, 400));
    const originalBounds = HTMLElement.prototype.getBoundingClientRect;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      if (!this.isConnected) return bounds(0, 0);
      if (this.dataset.messageRow === "first-image") return bounds(-100);
      if (this.dataset.messageRow === "second-image") return bounds(80 - (document.querySelector('[data-reaction-marker]') ? 40 : 0) - (list.scrollTop - 500));
      return originalBounds.call(this);
    });
    reactionStorage.setItem(seenKey(), "1");
    act(() => window.dispatchEvent(Object.assign(new Event("storage"), { key: seenKey(), newValue: "1" })));
    await waitFor(() => expect(list.scrollTop).toBe(460));
    expect(document.querySelector('[data-message-row="second-image"]')!.getBoundingClientRect().top).toBe(80);
  });

  it("reveals an already-seen explicit search-target notice before focusing its registered row", async () => {
    enableObservation();
    durableConversation();
    reactionStorage.setItem(seenKey(), "1");
    const target = message("plan", "plan?");
    const reaction = message(reactionId, "Reacted 😆 to “plan?”");
    vi.mocked(bridge.search_local).mockResolvedValue({ ...searchPage([{ messageId: reactionId, snippet: reaction.body }]), query: "Reacted", items: [{ kind: "message", conversationId, messageId: reactionId, title: "Aurora", snippet: reaction.body, timestamp: reaction.timestamp }] });
    vi.mocked(bridge.load_message_window).mockResolvedValue({ conversationId, messages: [target, reaction], hasOlder: false, hasNewer: false });
    renderMessages([target, reaction]);
    await screen.findByRole("img", { name: /😆 from/ });
    expect(document.querySelector('[data-reaction-notice]')).toBeNull();
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "Reacted" } });
    fireEvent.click(await screen.findByRole("option", { name: /Reacted 😆/ }));
    const notice = await screen.findByRole("button", { name: /Aurora Chen reacted 😆/ });
    await waitFor(() => expect(notice).toHaveFocus());
    expect(notice).toHaveAttribute("data-search-target", "true");
    expect(notice.closest('[data-reaction-notice]')).toBeVisible();
    act(() => screen.getByRole("button", { name: "Go to latest" }).focus());
    fireEvent.click(screen.getByRole("button", { name: "Go to latest" }));
    await waitFor(() => expect(document.querySelector('[data-reaction-marker]')).toBeInTheDocument());
  });

  it("batches the current actors sharing an emoji while leaving superseded and removed events visible", async () => {
    enableObservation();
    renderMessages([message("plan", "plan?"), message("superseded", "Reacted 😆 to “plan?”"), message("current", "Reacted 😆 to “plan?”"), message("second", "Reacted 😆 to “plan?”", { senderAddress: "+15555550142" }), message("removed-add", "Reacted 👍 to “plan?”", { senderAddress: "+15555550143" }), message("removed", "Removed 👍 from “plan?”", { senderAddress: "+15555550143" })]);
    await screen.findByRole("img", { name: /😆 from/ });
    act(() => ReactionObserver.intersect(reactionChip()));
    await waitFor(() => expect(document.querySelectorAll('[data-reaction-marker]')).toHaveLength(2));
    expect(bridge.mark_seen).toHaveBeenCalledWith(["current", "second"]);
    expect(document.querySelector('[data-reaction-notice="superseded"]')).toBeVisible();
    expect(document.querySelector('[data-reaction-notice="removed"]')).toBeVisible();
    expect(document.querySelector('[data-reaction-notice="removed-add"]')).toBeVisible();
  });

  it("keeps an incoming sender run continuous across a suppressed notice", async () => {
    enableObservation();
    snapshot.conversations[0].participants = ["+12025550123", "+15555550142"];
    renderMessages([message("plan", "plan?"), message("reaction", "Reacted 😆 to “plan?”", { senderAddress: "+15555550142" }), message("after", "after")]);
    await screen.findByRole("img", { name: /😆 from/ });
    act(() => ReactionObserver.intersect(reactionChip()));
    await waitFor(() => expect(document.querySelector('[data-reaction-marker]')).toBeInTheDocument());
    expect(document.querySelector('[data-message-row="plan"]')).toHaveAttribute("data-sender-run-start");
    expect(document.querySelector('[data-message-row="plan"]')).not.toHaveAttribute("data-sender-run-end");
    expect(document.querySelector('[data-message-row="after"]')).not.toHaveAttribute("data-sender-run-start");
    expect(document.querySelector('[data-message-row="after"]')).toHaveAttribute("data-sender-run-end");
  });

  it("waits for a fresh chip observation after the search navigation gate releases", async () => {
    enableObservation();
    const frames: FrameRequestCallback[] = [];
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => { frames.push(callback); return frames.length; });
    const target = message("plan", "plan?");
    const reaction = message("reaction", "Reacted 😆 to “plan?”");
    vi.mocked(bridge.search_local).mockResolvedValue({ ...searchPage([{ messageId: "plan", snippet: "plan?" }]), query: "plan" });
    vi.mocked(bridge.load_message_window).mockResolvedValue({ conversationId: "conv-aurora", messages: [target, reaction], hasOlder: false, hasNewer: false });
    renderMessages([target, reaction]);
    await screen.findByRole("img", { name: /😆 from/ });
    act(() => frames.splice(0).forEach(callback => callback(0)));
    fireEvent.change(screen.getByRole("combobox", { name: "Search Peppy" }), { target: { value: "plan" } });
    fireEvent.click(await screen.findByRole("option", { name: /plan\?/ }));
    await waitFor(() => expect(document.querySelector('[data-search-target="true"]')).toHaveAttribute("data-message-id", "plan"));
    const gatedObserver = [...ReactionObserver.instances].reverse().find(observer => observer.nodes.has(reactionChip()))!;
    act(() => gatedObserver.deliver(reactionChip()));
    expect(document.querySelector('[data-reaction-notice]')).toBeVisible();
    act(() => frames.splice(0).forEach(callback => callback(0)));
    expect(gatedObserver.nodes.size).toBe(0);
    expect(document.querySelector('[data-reaction-notice]')).toBeVisible();
    act(() => ReactionObserver.intersect(reactionChip()));
    await waitFor(() => expect(document.querySelector('[data-reaction-marker]')).toBeInTheDocument());
    expect(bridge.mark_seen).toHaveBeenCalledWith(["reaction"]);
  });

  it("uses composer occlusion and the half-chip threshold instead of whole-message visibility", async () => {
    enableObservation();
    vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (this: HTMLElement) { return this.id === "composer-area" ? 201 : 0; });
    renderMessages([message("plan", "plan?"), message("reaction", "Reacted 😆 to “plan?”")]);
    await waitFor(() => expect(reactionChip()).not.toBeNull());
    const options = [...ReactionObserver.instances].reverse().find(observer => observer.nodes.has(reactionChip()))?.options;
    expect(options?.root).toBe(document.getElementById("message-list"));
    expect(options?.rootMargin).toBe("0px 0px -208px 0px");
    act(() => ReactionObserver.intersect(reactionChip(), 0.49));
    expect(document.querySelector('[data-reaction-notice]')).toBeInTheDocument();
    expect(bridge.mark_seen).not.toHaveBeenCalled();
    act(() => ReactionObserver.intersect(reactionChip(), 0.5));
    await waitFor(() => expect(document.querySelector('[data-reaction-marker]')).toBeInTheDocument());
    expect(document.querySelector('[data-message-id="plan"]')).not.toHaveAttribute("data-visible", "true");
  });

  it("fences late chip callbacks after settings, disabling reactions, lock and conversation changes", async () => {
    enableObservation();
    renderMessages([message("plan", "plan?"), message("reaction", "Reacted 😆 to “plan?”")]);
    await screen.findByRole("img", { name: /😆 from/ });
    const chip = reactionChip();
    const oldObserver = [...ReactionObserver.instances].reverse().find(observer => observer.nodes.has(chip))!;
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    act(() => oldObserver.deliver(chip));
    fireEvent.click(await screen.findByRole("switch", { name: "Show reactions" }));
    fireEvent.click(screen.getByRole("button", { name: "Conversations" }));
    await screen.findByText("Reacted 😆 to “plan?”");
    act(() => oldObserver.deliver(chip));
    fireEvent.click(screen.getByRole("button", { name: /River Park/ }));
    await screen.findByText("Attachment received");
    act(() => oldObserver.deliver(chip));
    snapshot.encryption.state = "locked";
    await refreshMessages([message("plan", "plan?"), message("reaction", "Reacted 😆 to “plan?”")]);
    act(() => oldObserver.deliver(chip));
    expect(bridge.mark_seen).not.toHaveBeenCalled();
  });

  it("hides persisted hint-only history on its first committed render and survives restart", async () => {
    durableConversation();
    reactionStorage.setItem(seenKey(), "1");
    const commits: boolean[] = [];
    const observer = new MutationObserver(() => commits.push(Boolean(document.querySelector('[data-reaction-notice]'))));
    observer.observe(document.body, { subtree: true, childList: true });
    const first = renderMessages([message(reactionId, "Reacted 😆 to “unloaded text”")]);
    await waitFor(() => expect(document.querySelector('[data-reaction-marker]')).toHaveAttribute("data-message-id", reactionId));
    expect(commits).not.toContain(true);
    observer.disconnect();
    first.unmount();
    render(<App hostKind="browser" />);
    await waitFor(() => expect(document.querySelector('[data-reaction-marker]')).toHaveAttribute("data-message-id", reactionId));
    expect(document.querySelector('[data-reaction-notice]')).toBeNull();
  });

  it("persists seen UUIDs across remount and conversation navigation without suppressing a new same-emoji event", async () => {
    enableObservation();
    durableConversation();
    const first = renderMessages([message("plan", "plan?"), message(reactionId, "Reacted 😆 to “plan?”")]);
    await screen.findByRole("button", { name: /reacted 😆/ });
    act(() => ReactionObserver.intersect(reactionChip()));
    await waitFor(() => expect(reactionStorage.getItem(seenKey())).toBe("1"));
    first.unmount();
    render(<App hostKind="browser" />);
    await screen.findByRole("img", { name: /😆 from/ });
    expect(document.querySelector('[data-reaction-notice]')).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /River Park/ }));
    await screen.findByText("Attachment received");
    fireEvent.click(screen.getByRole("button", { name: /Aurora Chen/ }));
    await screen.findByRole("img", { name: /😆 from/ });
    expect(document.querySelector('[data-reaction-notice]')).toBeNull();
    await refreshMessages([message("plan", "plan?"), message(reactionId, "Reacted 😆 to “plan?”"), message(nextReactionId, "Reacted 😆 to “plan?”")]);
    await screen.findByRole("button", { name: /reacted 😆/ });
    expect(reactionStorage.getItem(seenKey(nextReactionId))).toBeNull();
    act(() => ReactionObserver.intersect(reactionChip()));
    await waitFor(() => expect(document.querySelector('[data-reaction-notice]')).toBeNull());
    expect(reactionStorage.getItem(seenKey(nextReactionId))).toBe("1");
  });

  it("retains focused notices until blur and failed or unknown metadata after seeing the chip", async () => {
    enableObservation();
    renderMessages([message("plan", "plan?"), message("reaction", "Reacted 😆 to “plan?”"), message("failed", "Reacted 👍 to “plan?”", { sender: "self", status: "failed-confirmed" }), message("unknown", "Reacted ❤️ to “plan?”", { sender: "self", status: "unknown", timestamp: "1788435190000" })]);
    const notice = await screen.findByRole("button", { name: /Aurora Chen reacted 😆/ });
    act(() => notice.focus());
    act(() => ReactionObserver.intersect(reactionChip()));
    expect(notice).toHaveFocus();
    expect(notice).toBeVisible();
    act(() => (document.getElementById("composer-textarea") as HTMLElement).focus());
    await waitFor(() => expect(document.querySelector('[data-reaction-marker="reaction"]')).toBeInTheDocument());
    for (const chip of document.querySelectorAll('[data-reaction-emoji]')) act(() => ReactionObserver.intersect(chip));
    expect(document.querySelector('[data-reaction-notice="failed"]')).toBeVisible();
    expect(document.querySelector('[data-reaction-notice="unknown"]')).toBeVisible();
    expect(document.getElementById("composer-textarea")).toHaveFocus();
  });

  it.each(["failed-before-submit", "failed-confirmed", "unknown"] as const)("retains persisted %s status until the status exception ends", async status => {
    durableConversation();
    reactionStorage.setItem(seenKey(), "1");
    snapshot.conversations[0].hasOlderMessages = true;
    let resolveHistory!: (page: Awaited<ReturnType<typeof bridge.load_history>>) => void;
    vi.spyOn(bridge, "load_history").mockReturnValue(new Promise(resolve => { resolveHistory = resolve; }));
    const reaction = message(reactionId, "Reacted 😆 to “plan?”", { sender: "self", senderAddress: undefined, status });
    renderMessages([message("plan", "plan?"), reaction, message("anchor", "anchor")]);
    await screen.findByRole("button", { name: /You reacted 😆/ });
    expect(document.querySelector(`[data-message-meta="${reactionId}"]`)).toBeVisible();
    const list = document.getElementById("message-list")!;
    Object.defineProperties(list, { clientHeight: { configurable: true, value: 400 }, scrollHeight: { configurable: true, value: 1200 } });
    list.scrollTop = 500;
    fireEvent.scroll(list);
    const bounds = (top: number, height = 20) => ({ width: 200, height, top, bottom: top + height, left: 0, right: 200, x: 0, y: top, toJSON() {} });
    vi.spyOn(list, "getBoundingClientRect").mockImplementation(() => bounds(0, 400));
    const originalBounds = HTMLElement.prototype.getBoundingClientRect;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      if (this.dataset.messageId === "anchor") return bounds(80 - (document.querySelector('[data-reaction-marker]') ? 40 : 0) - (list.scrollTop - 500));
      if (this.dataset.messageId === "plan") return bounds(-100);
      return originalBounds.call(this);
    });
    fireEvent.click(screen.getByRole("button", { name: "Load older messages" }));
    expect(bridge.load_history).toHaveBeenCalledTimes(1);
    act(() => document.getElementById("composer-textarea")!.focus());
    await refreshMessages([message("plan", "plan?"), { ...reaction, status: undefined }, message("anchor", "anchor")]);
    await waitFor(() => expect(document.querySelector('[data-reaction-marker]')).toBeInTheDocument());
    expect(list.scrollTop).toBe(460);
    expect(document.querySelector('[data-message-id="anchor"]')!.getBoundingClientRect().top).toBe(80);
    expect(document.getElementById("composer-textarea")).toHaveFocus();
    await refreshMessages([message("plan", "plan?"), reaction, message("anchor", "anchor")]);
    expect(document.querySelector('[data-reaction-notice]')).toBeInTheDocument();
    expect(list.scrollTop).toBe(500);
    expect(document.querySelector('[data-message-id="anchor"]')!.getBoundingClientRect().top).toBe(80);
    await refreshMessages([message("plan", "plan?"), { ...reaction, status: undefined }, message("anchor", "anchor")]);
    expect(list.scrollTop).toBe(460);
    expect(document.querySelector('[data-message-id="anchor"]')!.getBoundingClientRect().top).toBe(80);
    await act(async () => resolveHistory({ conversationId, messages: [], hasMore: false }));
    expect(list.scrollTop).toBe(460);
    expect(bridge.load_history).toHaveBeenCalledTimes(1);
  });

  it("retries rejected badge receipts on focus without marking unrelated or collapsed rows", async () => {
    enableObservation();
    vi.mocked(bridge.mark_seen).mockRejectedValueOnce(new Error("offline"));
    renderMessages([message("plan", "plan?"), message("reaction", "Reacted 😆 to “plan?”"), message("offscreen", "other")]);
    await screen.findByRole("img", { name: /😆 from/ });
    await act(async () => ReactionObserver.intersect(reactionChip()));
    expect(bridge.mark_seen).toHaveBeenCalledWith(["reaction"]);
    const marker = document.querySelector('[data-reaction-marker]')!;
    act(() => ReactionObserver.intersect(marker, 0, 0));
    expect(marker).not.toHaveAttribute("data-visible", "true");
    await act(async () => window.dispatchEvent(new Event("focus")));
    expect(bridge.mark_seen).toHaveBeenCalledTimes(2);
    expect(vi.mocked(bridge.mark_seen).mock.calls[1][0]).toEqual(["reaction"]);
    await act(async () => window.dispatchEvent(new Event("focus")));
    expect(bridge.mark_seen).toHaveBeenCalledTimes(2);
  });

  it("handles cross-window positives during StrictMode subscription replay", async () => {
    enableObservation();
    durableConversation();
    snapshot.conversations[0].messages = [message("plan", "plan?"), message(reactionId, "Reacted 😆 to “plan?”"), message(nextReactionId, "Reacted 👍 to “plan?”", { senderAddress: "+15555550142" })];
    render(<StrictMode><App hostKind="browser" /></StrictMode>);
    await screen.findByRole("button", { name: /reacted 😆/ });
    reactionStorage.setItem(seenKey(), "1");
    act(() => window.dispatchEvent(Object.assign(new Event("storage"), { key: seenKey(), newValue: "1" })));
    await waitFor(() => expect(document.querySelector(`[data-reaction-marker="${reactionId}"]`)).toBeInTheDocument());
    expect(screen.getByRole("button", { name: /reacted 👍/ })).toBeVisible();
    expect(bridge.mark_seen).not.toHaveBeenCalled();
  });

  it("requires fresh focused geometry and disables chip recording in the media tab", async () => {
    enableObservation();
    vi.mocked(document.hasFocus).mockReturnValue(false);
    renderMessages([message("plan", "plan?"), message("reaction", "Reacted 😆 to “plan?”")]);
    await screen.findByRole("img", { name: /😆 from/ });
    act(() => ReactionObserver.intersect(reactionChip()));
    expect(document.querySelector('[data-reaction-notice]')).toBeVisible();
    vi.mocked(document.hasFocus).mockReturnValue(true);
    act(() => window.dispatchEvent(new Event("focus")));
    expect(document.querySelector('[data-reaction-notice]')).toBeVisible();
    fireEvent.click(screen.getByRole("tab", { name: "Media" }));
    act(() => ReactionObserver.intersect(reactionChip()));
    expect(bridge.mark_seen).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("tab", { name: "Messages" }));
    act(() => ReactionObserver.intersect(reactionChip()));
    await waitFor(() => expect(document.querySelector('[data-reaction-marker]')).toBeInTheDocument());
  });

  it("does not record background or locked chip intersections", async () => {
    enableObservation();
    const reaction = message("reaction", "Reacted 😆 to “plan?”");
    renderMessages([message("plan", "plan?"), reaction]);
    await screen.findByRole("img", { name: /😆 from/ });
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    act(() => ReactionObserver.intersect(reactionChip()));
    expect(document.querySelector('[data-reaction-notice]')).toBeInTheDocument();
    expect(bridge.mark_seen).not.toHaveBeenCalled();
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
    act(() => document.dispatchEvent(new Event("visibilitychange")));
    const chip = reactionChip();
    const oldObserver = [...ReactionObserver.instances].reverse().find(observer => observer.nodes.has(chip))!;
    snapshot.encryption.state = "locked";
    await refreshMessages([message("plan", "plan?"), reaction]);
    act(() => oldObserver.deliver(chip));
    expect(bridge.mark_seen).not.toHaveBeenCalled();
    snapshot.encryption.state = "unlocked";
    await refreshMessages([message("plan", "plan?"), reaction]);
    await screen.findByRole("img", { name: /😆 from/ });
    expect(document.querySelector('[data-reaction-notice]')).toBeInTheDocument();
    act(() => ReactionObserver.intersect(reactionChip()));
    await waitFor(() => expect(document.querySelector('[data-reaction-marker]')).toBeInTheDocument());
  });

  it("preserves active lookup feedback until opening the loaded original ends the exception", async () => {
    enableObservation();
    let resolveSearch!: (page: ReturnType<typeof searchPage>) => void;
    vi.mocked(bridge.search_local).mockReturnValue(new Promise(resolve => { resolveSearch = resolve; }));
    const reaction = message("reaction", "Reacted 😆 to “plan?”");
    renderMessages([reaction]);
    fireEvent.click(await screen.findByRole("button", { name: /reacted 😆/ }));
    await refreshMessages([message("plan", "plan?"), reaction, message("anchor", "anchor")]);
    await screen.findByRole("img", { name: /😆 from/ });
    act(() => ReactionObserver.intersect(reactionChip()));
    expect(screen.getByRole("button", { name: /reacted 😆/ })).toHaveAttribute("aria-busy", "true");
    expect(document.querySelector('[data-reaction-marker]')).not.toBeInTheDocument();
    await act(async () => resolveSearch(searchPage([])));
    expect(document.querySelector('[data-reaction-feedback]')).toBeVisible();
    expect(screen.getByRole("button", { name: /reacted 😆/ })).toBeVisible();
    const list = document.getElementById("message-list")!;
    Object.defineProperties(list, { clientHeight: { configurable: true, value: 400 }, scrollHeight: { configurable: true, value: 1200 } });
    list.scrollTop = 500;
    fireEvent.scroll(list);
    const bounds = (top: number, height = 20) => ({ width: 200, height, top, bottom: top + height, left: 0, right: 200, x: 0, y: top, toJSON() {} });
    vi.spyOn(list, "getBoundingClientRect").mockImplementation(() => bounds(0, 400));
    const originalBounds = HTMLElement.prototype.getBoundingClientRect;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      if (this.dataset.messageId === "anchor") return bounds(80 - (document.querySelector('[data-reaction-marker]') ? 40 : 0) - (list.scrollTop - 500));
      if (this.dataset.messageId === "plan") return bounds(-100);
      return originalBounds.call(this);
    });
    fireEvent.click(screen.getByRole("button", { name: /reacted 😆/ }));
    await waitFor(() => expect(document.querySelector('[data-reaction-marker]')).toBeInTheDocument());
    expect(document.querySelector('[data-message-id="plan"]')).toHaveFocus();
    expect(list.scrollTop).toBe(460);
    expect(document.querySelector('[data-message-id="anchor"]')!.getBoundingClientRect().top).toBe(80);
  });

  it("permanently suppresses only represented hints and accounts for badge reads once", async () => {
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    vi.stubGlobal("IntersectionObserver", ReactionObserver);
    renderMessages([message("plan", "plan?"), message("reaction", "Reacted 😆 to “plan?”"), message("other", "Reacted 👍 to “plan?”", { senderAddress: "+15555550142" })]);
    const chip = (await screen.findByRole("img", { name: /😆 from/ })).querySelector('[data-reaction-emoji="😆"]')!;
    act(() => ReactionObserver.intersect(chip));
    await waitFor(() => expect(document.querySelector('[data-reaction-marker="reaction"]')).toHaveAttribute("hidden"));
    expect(screen.getByRole("button", { name: /reacted 👍/ })).toBeVisible();
    expect(screen.getByText("plan?")).toBeVisible();
    await waitFor(() => expect(bridge.mark_seen).toHaveBeenCalledWith(["reaction"]));
    act(() => ReactionObserver.intersect(chip, 0));
    expect(document.querySelector('[data-reaction-marker="reaction"]')).toHaveAttribute("hidden");
    expect(bridge.mark_seen).toHaveBeenCalledTimes(1);
  });

  it("does not read zero-area boundary rows", async () => {
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    vi.stubGlobal("IntersectionObserver", ReactionObserver);
    renderMessages([message("plan", "plan?")]);
    await screen.findByText("plan?");
    const row = document.querySelector('[data-message-id="plan"]')!;
    act(() => ReactionObserver.intersect(row, 0, 0));
    expect(bridge.mark_seen).not.toHaveBeenCalled();
    act(() => ReactionObserver.intersect(row));
    await waitFor(() => expect(bridge.mark_seen).toHaveBeenCalledWith(["plan"]));
  });

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

  it("uses resolved contact names and formatted unresolved numbers for group reaction actors", async () => {
    const senderAddress = "+15555550142";
    snapshot.conversations[0] = { ...snapshot.conversations[0], name: "Weekend group", participants: [senderAddress, "+15555550143"] };
    snapshot.contactResolution = { [senderAddress]: { bookId: "book-1", contactId: "contact-1", displayName: "Ada" } };
    const unresolvedAddress = "+18015551234";
    renderMessages([
      message("target", "plan?"),
      message("resolved-reaction", "Reacted 😆 to “plan?”", { senderAddress }),
      message("unresolved-reaction", "Reacted 👍 to “plan?”", { senderAddress: unresolvedAddress }),
    ]);
    await screen.findByRole("button", { name: "Ada reacted 😆 to “plan?”" });
    await screen.findByRole("button", { name: "(801) 555-1234 reacted 👍 to “plan?”" });
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
