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

    await screen.findByRole("button", { name: "first-image.png" });
    expect(document.querySelectorAll("[data-media-message-grid]")).toHaveLength(2);
    expect(document.querySelector('[data-message-meta="first"]')).toBeInTheDocument();
    expect(document.querySelector('[data-message-meta="second"]')).toBeInTheDocument();
    expect(document.querySelector('[data-message-meta="third"]')).toBeInTheDocument();
    expect(document.querySelector('[data-message-row="first"]')?.closest("[data-sender]"))
      .not.toBe(document.querySelector('[data-message-row="third"]')?.closest("[data-sender]"));

    fireEvent.click(screen.getByRole("button", { name: "first-image.png" }));
    expect(await screen.findByRole("dialog", { name: "Media viewer" })).toBeInTheDocument();
    expect(bridge.load_media).toHaveBeenCalledTimes(1);
    expect(bridge.load_media).toHaveBeenCalledWith("first-image");
  });

  it("keeps MMS public-link creation explicit and shows the returned warning", async () => {
    const attachment = image("shared-image");
    currentSnapshot = snapshot([message("shared-message", attachment)]);
    vi.mocked(bridge.media_page).mockResolvedValue({ items: [pageItem(attachment, "shared-message")] });
    vi.spyOn(bridge, "publish_attachment").mockResolvedValue({ url: "https://example.test/public", expiresInSeconds: 3600 });
    render(<App />);
    await screen.findByRole("button", { name: "shared-image.png" });
    expect(bridge.publish_attachment).not.toHaveBeenCalled();
    fireEvent.click(screen.getByText("Attachment actions"));
    fireEvent.click(screen.getByRole("button", { name: "Create public link for shared-image.png" }));
    await screen.findByDisplayValue("https://example.test/public");
    expect(document.querySelector(".public-link-warning")).toHaveTextContent("Anyone with this link can view this copy");
    expect(bridge.publish_attachment).toHaveBeenCalledWith("shared-image");
  });

  it("walks cursor pages to an older timeline target and advances each lightbox boundary", async () => {
    const newest = image("newest");
    const middle = image("middle");
    const oldest = image("oldest");
    const cursorCalls: Array<string | undefined> = [];
    currentSnapshot = snapshot([
      message("newest-message", newest),
      message("middle-message", middle),
      message("oldest-message", oldest),
    ]);
    vi.mocked(bridge.media_page).mockImplementation(async (_conversationId, cursor) => {
      cursorCalls.push(cursor?.messageId);
      if (!cursor) return { items: [pageItem(newest)], nextCursor: { sortAtMs: "3", localOrder: "3", messageId: "newest", attachmentPosition: 0 } };
      if (cursor.messageId === "newest") return { items: [pageItem(middle)], nextCursor: { sortAtMs: "2", localOrder: "2", messageId: "middle", attachmentPosition: 0 } };
      return { items: [pageItem(oldest, "oldest-message")] };
    });
    vi.mocked(bridge.load_media).mockResolvedValue(ready("blob:image", oldest));

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "newest.png" }));
    await screen.findByRole("dialog", { name: "Media viewer" });
    fireEvent.click(screen.getByRole("button", { name: "Next image" }));
    await screen.findByRole("img", { name: "middle.png" });
    await waitFor(() => expect(document.getElementById("lightbox-position-label")).toHaveTextContent("Image 2 of 2"));
    fireEvent.click(screen.getByRole("button", { name: "Next image" }));
    await screen.findByRole("img", { name: "oldest.png" });
    await waitFor(() => expect(document.getElementById("lightbox-position-label")).toHaveTextContent("Image 3 of 3"));
    expect(screen.getByRole("button", { name: "Next image" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Close media viewer" }));

    fireEvent.click(await screen.findByRole("button", { name: "oldest.png" }));

    await screen.findByRole("dialog", { name: "Media viewer" });
    await waitFor(() => expect(cursorCalls).toEqual([undefined, "newest", "middle", undefined, "newest", "middle"]));
    expect(screen.getByRole("button", { name: "Next image" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Previous image" }));
    expect(screen.getByText("Image 2 of 3")).toBeInTheDocument();
  });

  it("fences delayed gallery and original responses after a selection change or close", async () => {
    const page = deferred<Awaited<ReturnType<typeof bridge.media_page>>>();
    const original = deferred<MediaOpen>();
    const attachment = image("fenced-image");
    currentSnapshot = snapshot([message("fenced-message", attachment)]);
    vi.mocked(bridge.media_page).mockReturnValue(page.promise);

    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "fenced-image.png" }));
    fireEvent.click(screen.getByText("River"));
    currentSnapshot = snapshot([], "river");
    await act(async () => stateHint?.());
    await act(async () => page.resolve({ items: [pageItem(attachment, "fenced-message")] }));
    expect(screen.queryByRole("dialog", { name: "Media viewer" })).not.toBeInTheDocument();

    currentSnapshot = snapshot([message("fenced-message", attachment)]);
    await act(async () => stateHint?.());
    vi.mocked(bridge.media_page).mockResolvedValue({ items: [pageItem(attachment, "fenced-message")] });
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
    expect(await screen.findByRole("button", { name: "Downloading…" })).toBeInTheDocument();
    await act(async () => stateHint?.());
    expect(await screen.findByRole("img", { name: "pending-image.png" })).toHaveAttribute("src", "blob:ready");
    expect(bridge.load_media).toHaveBeenCalledTimes(1);
    expect(bridge.open_media).toHaveBeenCalledTimes(1);
  });
});
