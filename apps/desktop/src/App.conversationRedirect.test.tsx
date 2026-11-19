import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, fixtureBridge, mergeFixtureConversation, type DesktopSnapshot } from "./bridge/bridge";

/**
 * The browser fixture models a merge: `conv-aurora` (selected at start) is merged into `conv-river`.
 * Requests for `conv-aurora` then answer with `requestedRedirect` to the survivor, as the core does.
 */
let publishSnapshot: (() => void) | undefined;
let restoreMerge: (() => void) | undefined;
let auroraLast: string;
let auroraFirstId: string;
// The fixture is the bridge in tests, so the spy below replaces its property; keep the real function to call.
const realLoadState = fixtureBridge.load_state;
const realMediaPage = fixtureBridge.media_page;
const realMediaPageAround = fixtureBridge.media_page_around;

const publish = () => act(async () => { publishSnapshot?.(); await Promise.resolve(); });
const loadedIds = () => vi.mocked(bridge.load_state).mock.calls.map(([id]) => id);

const rectOf = (top: number, bottom: number) => ({ top, bottom, left: 0, right: 400, width: 400, height: bottom - top, x: 0, y: top, toJSON() {} });
function stubLayout() {
  const original = HTMLElement.prototype.getBoundingClientRect;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    if (this.id === "message-list") return rectOf(0, 800);
    if (this.dataset.messageId === auroraLast) return rectOf(200, 260);
    return original.call(this);
  });
}
const folderTrigger = () => document.getElementById("conversation-folder-trigger")!;
const merge = () => { restoreMerge = mergeFixtureConversation("conv-aurora", "conv-river"); };

beforeEach(async () => {
  vi.restoreAllMocks();
  const fixture: DesktopSnapshot = await fixtureBridge.load_state("conv-aurora");
  const aurora = fixture.conversations.find((conversation) => conversation.id === "conv-aurora")!;
  auroraLast = aurora.messages.at(-1)!.body;
  auroraFirstId = aurora.messages[0].id;
  await fixtureBridge.set_conversation_folder("conv-aurora", "archived");
  vi.spyOn(bridge, "load_state").mockImplementation((conversationId, historyFrom, options) => realLoadState(conversationId, historyFrom, options));
  vi.spyOn(bridge, "subscribe").mockImplementation((listener) => { publishSnapshot = listener; return () => undefined; });
  vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "subscribe_lifecycle_finished").mockReturnValue(() => undefined);
  vi.spyOn(bridge, "set_notification_context").mockResolvedValue();
  vi.spyOn(bridge, "mark_seen").mockResolvedValue();
  Object.defineProperty(Element.prototype, "scrollIntoView", { configurable: true, value: vi.fn() });
  stubLayout();
});

afterEach(async () => {
  cleanup();
  restoreMerge?.();
  restoreMerge = undefined;
  await fixtureBridge.set_conversation_folder("conv-aurora", "inbox");
  await fixtureBridge.set_conversation_folder("conv-river", "inbox");
  window.history.replaceState({}, "", "/");
  vi.restoreAllMocks();
});

describe("merged-away selected conversation", () => {
  it("keeps the survivor selected with the folder view, without a warning", async () => {
    render(<App hostKind="browser" />);
    await screen.findByText(auroraLast);
    expect(folderTrigger()).toHaveAttribute("data-folder", "archived");
    await fixtureBridge.set_conversation_folder("conv-river", "inbox");

    merge();
    await publish();
    await publish();

    expect(loadedIds().at(-1)).toBe("conv-river");
    expect(screen.getByText(auroraLast)).toBeInTheDocument();
    expect(screen.queryByText(/no longer available|is unavailable/)).not.toBeInTheDocument();
    expect(folderTrigger()).toHaveAttribute("data-folder", "archived");
  });

  it("keeps the message list node and scroll position, without a stick-to-bottom jump", async () => {
    render(<App hostKind="browser" />);
    await screen.findByText(auroraLast);
    const list = document.getElementById("message-list")!;
    Object.defineProperties(list, { clientHeight: { configurable: true, value: 400 }, scrollHeight: { configurable: true, value: 1200 } });
    list.scrollTop = 500;
    fireEvent.scroll(list);

    merge();
    await publish();

    expect(document.getElementById("message-list")).toBe(list);
    expect(list.scrollTop).toBe(500);
  });

  it("keeps the open Media tab on the survivor", async () => {
    render(<App hostKind="browser" />);
    await screen.findByText(auroraLast);
    fireEvent.click(document.getElementById("tab-media")!);
    const mediaPanel = await waitFor(() => {
      const panel = document.getElementById("tabpanel-media");
      expect(panel).toBeInTheDocument();
      return panel!;
    });

    merge();
    await publish();

    expect(document.getElementById("tabpanel-media")).toBe(mediaPanel);
  });

  it("keeps unsaved composer text, its caret, and the composer element", async () => {
    vi.spyOn(bridge, "save_draft").mockReturnValue(new Promise(() => {}));
    render(<App hostKind="browser" />);
    await screen.findByText(auroraLast);
    const composer = document.getElementById("composer-textarea") as HTMLTextAreaElement;
    fireEvent.change(composer, { target: { value: "half written reply" } });
    composer.setSelectionRange(4, 4);

    merge();
    await publish();
    await publish();

    expect(loadedIds().at(-1)).toBe("conv-river");
    expect(document.getElementById("composer-textarea")).toBe(composer);
    expect(composer).toHaveValue("half written reply");
    expect(composer.selectionStart).toBe(4);
  });

  it("does not mix the requested conversation's older messages into an unrelated fallback", async () => {
    let fallback = false;
    vi.spyOn(bridge, "load_state").mockImplementation(async (conversationId, historyFrom, options) => {
      const state = await realLoadState(conversationId, historyFrom, options);
      return fallback && conversationId === "conv-aurora" ? { ...state, activeConversationId: "conv-river" } : state;
    });
    render(<App hostKind="browser" />);
    await screen.findByText(auroraLast);

    fallback = true;
    await publish();

    expect(document.querySelector(`[data-message-id="${auroraFirstId}"]`)).not.toBeInTheDocument();
    expect(screen.queryByText(/no longer available|is unavailable/)).not.toBeInTheDocument();
  });
});

describe("open gallery across a merge", () => {
  it("keeps the lightbox's items and paginates with the survivor ID", async () => {
    const [first] = (await realMediaPage("conv-river", undefined, 40)).items;
    const cellA = first;
    const cellC = { ...first, attachment: { ...first.attachment, id: "attachment-older" } };
    const olderCursor = { ...first.cursor, messageId: "older-message" };
    const around = await realMediaPageAround("conv-river", first.attachment.id, first.messageId, 40);
    vi.spyOn(bridge, "media_page").mockImplementation(async (_conversationId, cursor) =>
      cursor === olderCursor ? { items: [cellC], nextCursor: undefined } : { items: [cellA], nextCursor: undefined });
    vi.spyOn(bridge, "media_page_around").mockResolvedValue({ ...around, target: cellA, newer: [], older: [], olderCursor });

    render(<App hostKind="browser" />);
    await screen.findByText(auroraLast);
    fireEvent.click(document.getElementById("tab-media")!);
    await waitFor(() => expect(document.querySelector("[data-media-cell]")).not.toBeNull());
    fireEvent.click(document.querySelector("[data-media-cell]")!);
    await screen.findByRole("button", { name: "Older image" });

    merge();
    await publish();
    await publish();
    expect(screen.getByRole("button", { name: "Newer image" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Older image" }));

    await waitFor(() => expect(vi.mocked(bridge.media_page).mock.calls.some(([id, cursor]) => id === "conv-river" && cursor === olderCursor)).toBe(true));
    expect(vi.mocked(bridge.media_page).mock.calls.some(([id, cursor]) => id === "conv-aurora" && cursor === olderCursor)).toBe(false);
    await waitFor(() => expect(screen.getByRole("button", { name: "Newer image" })).toBeEnabled());
  });
});

describe("merged-away composer window", () => {
  it("keeps its window following the survivor across refreshes", async () => {
    window.history.replaceState({}, "", "/?window=composer&conversationId=conv-aurora");
    render(<App hostKind="browser" />);
    await waitFor(() => expect(loadedIds()).toContain("conv-aurora"));

    merge();
    await publish();
    await publish();

    expect(loadedIds().at(-1)).toBe("conv-river");
    expect(screen.queryByText(/no longer available|is unavailable/)).not.toBeInTheDocument();
  });
});
