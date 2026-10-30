import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { bridge } from "../bridge";
import { InvalidMediaViewerWindow, MediaViewerWindow, readMediaViewerWindow } from "./MediaViewerWindow";

vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ close: vi.fn() }) }));

const pageItem = (id: string, messageId = `message-${id}`) => ({
  attachment: { id, name: id, mediaType: "image/jpeg", byteSize: 1, state: "ready" as const },
  messageId,
  cursor: { localOrder: "1", sortAtMs: "2026-01-01T00:00:00.000Z", messageId, attachmentPosition: 0 },
  timestamp: "2026-01-01T00:00:00.000Z",
});

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("readMediaViewerWindow", () => {
  it("accepts a complete valid media route", () => {
    expect(readMediaViewerWindow("?window=media&conversationId=aurora&attachmentId=file_1&messageId=message-1")).toEqual({ conversationId: "aurora", attachmentId: "file_1", messageId: "message-1" });
  });

  it("rejects incomplete or invalid media routes", () => {
    expect(readMediaViewerWindow("?window=media&conversationId=aurora&attachmentId=file")).toBeNull();
    expect(readMediaViewerWindow("?window=media&conversationId=bad!&attachmentId=file&messageId=message")).toBeNull();
  });

  it("renders a closable error shell for an invalid media route", () => {
    const close = vi.fn();
    render(<InvalidMediaViewerWindow close={close} />);
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't open this media");
    expect(screen.getByRole("button", { name: "Close media viewer" })).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "Close media viewer" }));
    expect(close).toHaveBeenCalledOnce();
  });
});

describe("MediaViewerWindow", () => {
  it("loads the target media without joining the draft lifecycle", async () => {
    vi.spyOn(bridge, "media_page_around").mockResolvedValue({ target: pageItem("file"), newer: [], older: [] });
    vi.spyOn(bridge, "load_media").mockResolvedValue({ state: "ready", url: "blob:file", mediaType: "image/jpeg", byteSize: 1 });
    vi.spyOn(bridge, "open_media").mockResolvedValue({ state: "ready", url: "blob:file", mediaType: "image/jpeg", byteSize: 1 });
    const subscribeLifecycle = vi.spyOn(bridge, "subscribe_lifecycle");
    const acknowledge = vi.spyOn(bridge, "acknowledge_lifecycle").mockResolvedValue();
    render(<MediaViewerWindow conversationId="aurora" attachmentId="file" messageId="message-file" />);
    await waitFor(() => expect(bridge.media_page_around).toHaveBeenCalledWith("aurora", "file", "message-file", 40));
    expect(await screen.findByRole("img", { name: "file" })).toHaveAttribute("src", "blob:file");
    expect(subscribeLifecycle).not.toHaveBeenCalled();
    expect(acknowledge).not.toHaveBeenCalled();
  });

  it("shows a retryable load error and closes on Escape", async () => {
    vi.spyOn(bridge, "media_page_around").mockRejectedValue(new Error("offline"));
    vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
    const close = vi.fn();
    render(<MediaViewerWindow conversationId="aurora" attachmentId="file" messageId="message-file" close={close} />);
    expect(await screen.findByRole("button", { name: "Retry" })).toBeInTheDocument();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(close).toHaveBeenCalledOnce();
  });

  it("loads the older media page when navigation reaches its initial boundary", async () => {
    const olderCursor = { localOrder: "2", sortAtMs: "2026-01-01T00:00:00.000Z", messageId: "message-file", attachmentPosition: 0 };
    vi.spyOn(bridge, "media_page_around").mockResolvedValue({ target: pageItem("file"), newer: [], older: [], olderCursor });
    vi.spyOn(bridge, "media_page").mockResolvedValue({ items: [pageItem("older")], nextCursor: undefined });
    vi.spyOn(bridge, "load_media").mockImplementation(async id => ({ state: "ready", url: `blob:${id}`, mediaType: "image/jpeg", byteSize: 1 }));
    vi.spyOn(bridge, "open_media").mockResolvedValue({ state: "pending" });
    vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);

    render(<MediaViewerWindow conversationId="aurora" attachmentId="file" messageId="message-file" />);
    await screen.findByRole("img", { name: "file" });
    fireEvent.click(screen.getByRole("button", { name: "Older image" }));

    await waitFor(() => expect(screen.getByRole("img", { name: "older" })).toHaveAttribute("src", "blob:older"));
    expect(bridge.media_page).toHaveBeenCalledWith("aurora", olderCursor, 40);
  });

  it("keeps the current lease through state updates and releases it on unmount", async () => {
    let stateListener!: () => void;
    vi.spyOn(bridge, "media_page_around").mockResolvedValue({ target: pageItem("file"), newer: [], older: [] });
    vi.spyOn(bridge, "load_media").mockResolvedValue({ state: "ready", url: "blob:file", mediaType: "image/jpeg", byteSize: 1 });
    vi.spyOn(bridge, "open_media").mockResolvedValue({ state: "ready", url: "blob:file", mediaType: "image/jpeg", byteSize: 1 });
    vi.spyOn(bridge, "subscribe").mockImplementation(listener => { stateListener = listener; return () => undefined; });
    vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
    const closeMedia = vi.spyOn(bridge, "close_media").mockResolvedValue();
    const view = render(<MediaViewerWindow conversationId="aurora" attachmentId="file" messageId="message-file" />);
    await screen.findByRole("img", { name: "file" });
    act(stateListener);
    expect(closeMedia).not.toHaveBeenCalled();
    expect(screen.getByRole("img", { name: "file" })).toHaveAttribute("src", "blob:file");
    view.unmount();
    expect(closeMedia).toHaveBeenCalledWith("file", "blob:file");
  });

  it("keeps an in-flight original request after a state update", async () => {
    let stateListener!: () => void;
    let resolveOriginal!: (result: { state: "ready"; url: string; mediaType: string; byteSize: number }) => void;
    vi.spyOn(bridge, "media_page_around").mockResolvedValue({ target: pageItem("file"), newer: [], older: [] });
    vi.spyOn(bridge, "load_media").mockReturnValue(new Promise(resolve => { resolveOriginal = resolve; }));
    vi.spyOn(bridge, "subscribe").mockImplementation(listener => { stateListener = listener; return () => undefined; });
    vi.spyOn(bridge, "subscribe_lifecycle").mockReturnValue(() => undefined);
    const closeMedia = vi.spyOn(bridge, "close_media").mockResolvedValue();
    render(<MediaViewerWindow conversationId="aurora" attachmentId="file" messageId="message-file" />);
    await waitFor(() => expect(bridge.load_media).toHaveBeenCalledWith("file"));
    act(stateListener);
    await act(async () => resolveOriginal({ state: "ready", url: "blob:file", mediaType: "image/jpeg", byteSize: 1 }));
    expect(await screen.findByRole("img", { name: "file" })).toHaveAttribute("src", "blob:file");
    expect(closeMedia).not.toHaveBeenCalled();
  });

  it("stops checking a held target once its original is displayed", async () => {
    let stateListener!: () => void;
    const target = pageItem("file");
    vi.spyOn(bridge, "media_page_around").mockResolvedValue({ target: { ...target, attachment: { ...target.attachment, mediaHold: "held" } }, newer: [], older: [] });
    const check = vi.spyOn(bridge, "open_media").mockResolvedValue({ state: "ready", url: "blob:file", mediaType: "image/jpeg", byteSize: 1 });
    vi.spyOn(bridge, "subscribe").mockImplementation(listener => { stateListener = listener; return () => undefined; });
    const release = vi.spyOn(bridge, "close_media").mockResolvedValue();
    render(<MediaViewerWindow conversationId="aurora" attachmentId="file" messageId="message-file" />);
    await screen.findByRole("img", { name: "file" });
    act(stateListener);
    act(stateListener);
    expect(check).toHaveBeenCalledTimes(1);
    expect(release).not.toHaveBeenCalled();
  });
});
