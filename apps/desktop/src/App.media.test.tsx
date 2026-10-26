import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "./App";
import { bridge, type AttachmentView, type DesktopSnapshot, type MediaOpen, type MediaPageItem, type MessageView } from "./bridge";

const preview = "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==";

const deferred = <T,>() => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
};

const image = (id: string): AttachmentView => ({
  id,
  name: `${id}.png`,
  mediaType: "image/png",
  byteSize: 1,
  state: "ready",
  previewUrl: preview,
});

const message = (id: string, attachment: AttachmentView, senderAddress = "+15550001"): MessageView => ({
  id,
  revision: id,
  sender: "other",
  senderAddress,
  body: "",
  timestamp: "1788434460000",
  transport: "mms",
  attachments: [attachment],
});

const pageItem = (attachment: AttachmentView, messageId = `message-${attachment.id}`): MediaPageItem => ({
  attachment,
  messageId,
  timestamp: "1788434460000",
  cursor: { sortAtMs: "1788434460000", localOrder: messageId, messageId, attachmentPosition: 0 },
});

const ready = (url: string, attachment: AttachmentView): MediaOpen => ({
  state: "ready",
  url,
  mediaType: attachment.mediaType,
  byteSize: attachment.byteSize,
});

function snapshot(messages: MessageView[], activeConversationId = "aurora"): DesktopSnapshot {
  return {
    version: "1",
    mode: "native",
    connection: { state: "connected", origin: "https://example.test" },
    encryption: { state: "unlocked" },
    gateways: [],
    conversations: [
      { id: "aurora", name: "Aurora", preview: "", unread: 0, messages: activeConversationId === "aurora" ? messages : [] },
      { id: "river", name: "River", preview: "", unread: 0, messages: activeConversationId === "river" ? [message("river-message", image("river-image"))] : [] },
    ],
    activeConversationId,
    head: { enabled: false, capability: "unsupported" },
    pendingCount: 0,
    quarantineCount: 0,
    notifications: [],
    appFilters: [],
    notificationPreferences: { messageBanners: true, mirroredBanners: true, preview: "full" },
  };
}

let currentSnapshot: DesktopSnapshot;
let stateHint: (() => void) | undefined;

beforeEach(() => {
  vi.restoreAllMocks();
  const storage = new Map<string, string>();
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
      removeItem: (key: string) => storage.delete(key),
      clear: () => storage.clear(),
    },
  });
  currentSnapshot = snapshot([]);
  stateHint = undefined;
  vi.spyOn(bridge, "load_state").mockImplementation(async () => currentSnapshot);
  vi.spyOn(bridge, "get_app_lock_status").mockResolvedValue({ enabled: false, locked: false, storageAvailable: true });
  vi.spyOn(bridge, "subscribe").mockImplementation((listener) => {
    stateHint = listener;
    return () => undefined;
  });
  vi.spyOn(bridge, "media_page").mockResolvedValue({ items: [] });
  vi.spyOn(bridge, "media_page_around").mockImplementation(async (conversationId, attachmentId, messageId) => {
    const page = await bridge.media_page(conversationId);
    const target = page.items.find(item => item.attachment.id === attachmentId && item.messageId === messageId);
    if (!target) throw new Error("missing media anchor");
    return { target, newer: [], older: [] };
  });
  vi.spyOn(bridge, "media_page_newer").mockResolvedValue({ items: [] });
    vi.spyOn(bridge, "load_media").mockResolvedValue({ state: "pending" });
    vi.spyOn(bridge, "open_media").mockResolvedValue({ state: "pending" });
  vi.spyOn(bridge, "close_media").mockResolvedValue();
  vi.spyOn(bridge, "save_attachment").mockResolvedValue(true);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("App media wiring", () => {
  it("renders consecutive MMS image runs with per-message metadata and opens the clicked image once", async () => {
    const first = message("first", image("first-image"));
    const second = message("second", image("second-image"));
    const third = message("third", image("third-image"), "+15550002");
    currentSnapshot = snapshot([first, second, third]);
    vi.mocked(bridge.media_page).mockResolvedValue({ items: [pageItem(first.attachments[0], first.id), pageItem(second.attachments[0], second.id), pageItem(third.attachments[0], third.id)] });
    vi.mocked(bridge.load_media).mockResolvedValue(ready("blob:first", first.attachments[0]));

    render(<App />);

    await screen.findByRole("button", { name: "Photo, 1 of 2" });
    expect(document.querySelectorAll("[data-media-message-grid]")).toHaveLength(2);
    expect(document.querySelector('[data-message-meta="first"]')).toBeInTheDocument();
    expect(document.querySelector('[data-message-meta="second"]')).toBeInTheDocument();
    expect(document.querySelector('[data-message-meta="third"]')).toBeInTheDocument();
    expect(document.querySelector('[data-message-row="first"]')?.closest("[data-sender]"))
      .not.toBe(document.querySelector('[data-message-row="third"]')?.closest("[data-sender]"));

    fireEvent.click(screen.getByRole("button", { name: "Photo, 1 of 2" }));
    expect(await screen.findByRole("dialog", { name: "Media viewer" })).toBeInTheDocument();
    expect(bridge.load_media).toHaveBeenCalledTimes(1);
    expect(bridge.load_media).toHaveBeenCalledWith("first-image");
  });

  it("preserves captions and renders a v3 ready thumbnail while its original is remote", async () => {
    const thumbnail = image("remote-thumbnail");
    const original = { ...image("remote-original"), state: "pending" as const, previewUrl: undefined, transfer: "download" as const, mediaMeta: { thumbnail } };
    const captioned = { ...message("captioned", original), body: "A retained caption" };
    currentSnapshot = snapshot([captioned, message("following", image("following-image"))]);
    vi.mocked(bridge.media_page).mockResolvedValue({
      items: [{ ...pageItem(original, captioned.id), mediaMeta: { thumbnail } }],
    });

    render(<App />);

    expect(await screen.findByText("A retained caption")).toBeInTheDocument();
    expect(document.querySelectorAll('[data-message-row="captioned"]')).toHaveLength(1);
    expect(screen.getByRole("button", { name: "remote-original.png" })).toHaveAttribute("data-attachment-id", "remote-original");
    fireEvent.click(screen.getByRole("tab", { name: "Media" }));
    const cell = await screen.findByRole("button", { name: "remote-original.png" });
    expect(cell.closest("[data-media-tile]")).toHaveAttribute("data-state", "ready");
    expect(cell.querySelector("img")).toHaveAttribute("src", preview);
  });

  it("uses gallery hover actions to create a public link", async () => {
    const attachment = image("shared-image");
    currentSnapshot = snapshot([message("shared-message", attachment)]);
    vi.mocked(bridge.media_page).mockResolvedValue({ items: [pageItem(attachment, "shared-message")] });
    vi.spyOn(bridge, "publish_attachment").mockResolvedValue({ url: "https://example.test/public", expiresInSeconds: 3600 });
    render(<App />);
    await screen.findByRole("button", { name: "shared-image.png" });
    expect(bridge.publish_attachment).not.toHaveBeenCalled();
    expect(screen.queryByText("Attachment actions")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Create public link" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Copy public link" })).toBeInTheDocument());
    expect(bridge.publish_attachment).toHaveBeenCalledWith("shared-image");
  });

  it("opens an exact old target from one anchor query and pages both directions", async () => {
    const newest = image("newest");
    const middle = image("reused");
    const reusedElsewhere = image("reused");
    const oldest = image("oldest");
    const oldCursor = { sortAtMs: "2", localOrder: "2", messageId: "middle-message", attachmentPosition: 0 };
    const newCursor = { sortAtMs: "3", localOrder: "3", messageId: "middle-message", attachmentPosition: 0 };
    currentSnapshot = snapshot([
      message("newest-message", newest),
      message("middle-message", middle),
      message("reused-elsewhere-message", reusedElsewhere, "+15550002"),
      message("oldest-message", oldest),
    ]);
    vi.mocked(bridge.media_page_around).mockResolvedValue({ target: pageItem(middle, "middle-message"), newer: [], older: [], newerCursor: newCursor, olderCursor: oldCursor });
    vi.mocked(bridge.media_page_newer).mockResolvedValue({ items: [pageItem(newest, "newest-message")] });
    vi.mocked(bridge.media_page).mockResolvedValue({ items: [pageItem(oldest, "oldest-message")] });
    vi.mocked(bridge.load_media).mockResolvedValue(ready("blob:image", oldest));

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "Photo, 2 of 2" }));
    await screen.findByRole("dialog", { name: "Media viewer" });
    expect(bridge.media_page_around).toHaveBeenCalledWith("aurora", "reused", "middle-message", 40);
    expect(bridge.media_page).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Newer image" }));
    await screen.findByRole("img", { name: "newest.png" });
    fireEvent.click(screen.getByRole("button", { name: "Older image" }));
    await waitFor(() => expect(document.getElementById("lightbox-position-label")).toHaveTextContent("Image 2 of at least 2 loaded"));
    fireEvent.click(screen.getByRole("button", { name: "Older image" }));
    await screen.findByRole("img", { name: "oldest.png" });
    expect(bridge.media_page_newer).toHaveBeenCalledWith("aurora", newCursor, 40);
    expect(bridge.media_page).toHaveBeenCalledWith("aurora", oldCursor, 40);
    expect(document.getElementById("lightbox-position-label")).toHaveTextContent("Image 3 of 3");
  });

  it("fences delayed gallery and original responses after a selection change or close", async () => {
    const page = deferred<Awaited<ReturnType<typeof bridge.media_page_around>>>();
    const original = deferred<MediaOpen>();
    const attachment = image("fenced-image");
    currentSnapshot = snapshot([message("fenced-message", attachment)]);
    vi.mocked(bridge.media_page_around).mockReturnValue(page.promise);

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "fenced-image.png" }));
    fireEvent.click(screen.getByText("River"));
    currentSnapshot = snapshot([], "river");
    await act(async () => stateHint?.());
    await act(async () => page.resolve({ target: pageItem(attachment, "fenced-message"), newer: [], older: [] }));
    expect(screen.queryByRole("dialog", { name: "Media viewer" })).not.toBeInTheDocument();

    currentSnapshot = snapshot([message("fenced-message", attachment)]);
    await act(async () => stateHint?.());
    vi.mocked(bridge.media_page_around).mockResolvedValue({ target: pageItem(attachment, "fenced-message"), newer: [], older: [] });
    vi.mocked(bridge.load_media).mockReturnValue(original.promise);
    fireEvent.click(await screen.findByText("Aurora"));
    fireEvent.click(await screen.findByRole("button", { name: "fenced-image.png" }));
    await screen.findByRole("dialog", { name: "Media viewer" });
    fireEvent.click(screen.getByRole("button", { name: "Close media viewer" }));
    await act(async () => original.resolve(ready("blob:old", attachment)));
    await waitFor(() => expect(bridge.close_media).toHaveBeenCalledWith("fenced-image", "blob:old"));
    expect(bridge.close_media).toHaveBeenCalledTimes(1);
  });

  it("retries a pending original after one bridge state hint without a further click", async () => {
    const attachment = image("pending-image");
    currentSnapshot = snapshot([message("pending-message", attachment)]);
    vi.mocked(bridge.media_page).mockResolvedValue({ items: [pageItem(attachment, "pending-message")] });
    vi.mocked(bridge.load_media)
      .mockResolvedValueOnce({ state: "pending" });
    vi.mocked(bridge.open_media).mockResolvedValueOnce(ready("blob:ready", attachment));

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "pending-image.png" }));
    expect(await screen.findByText("Downloading from phone…")).toBeInTheDocument();
    await act(async () => stateHint?.());
    expect(await screen.findByRole("img", { name: "pending-image.png" })).toHaveAttribute("src", "blob:ready");
    expect(bridge.load_media).toHaveBeenCalledTimes(1);
    expect(bridge.open_media).toHaveBeenCalledTimes(1);
  });

  it.each(["native", "browser"] as const)("renders body and caption links for the %s host while keeping subjects plain", async (hostKind) => {
    const bodyUrl = "https://example.test/body";
    const captionUrl = "https://example.test/caption";
    const subjectUrl = "https://example.test/subject";
    const bodyMessage = { ...message("linked-body", image("unused-body-image")), body: `Open ${bodyUrl}`, transport: "sms" as const, attachments: [] };
    const captionMessage = { ...message("linked-caption", image("linked-image")), body: `Caption ${captionUrl}`, subject: `Reference ${subjectUrl}` };
    currentSnapshot = snapshot([bodyMessage, captionMessage]);
    const openExternal = vi.spyOn(bridge as { open_external_url(url: string): Promise<void> }, "open_external_url").mockResolvedValue();

    render(<App hostKind={hostKind} />);

    const bodyLink = await screen.findByRole("link", { name: bodyUrl });
    const captionLink = screen.getByRole("link", { name: captionUrl });
    expect(bodyLink).toHaveAttribute("href", bodyUrl);
    expect(captionLink).toHaveAttribute("href", captionUrl);
    expect(screen.getByText(`Subject: Reference ${subjectUrl}`).querySelector("a")).toBeNull();
    if (hostKind === "native") {
      fireEvent.click(bodyLink);
      fireEvent.click(captionLink);
      expect(openExternal).toHaveBeenNthCalledWith(1, bodyUrl);
      expect(openExternal).toHaveBeenNthCalledWith(2, captionUrl);
    } else {
      expect(bodyLink).toHaveAttribute("target", "_blank");
      expect(captionLink).toHaveAttribute("target", "_blank");
      expect(openExternal).not.toHaveBeenCalled();
    }
  });

});
