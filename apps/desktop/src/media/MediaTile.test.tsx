import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { StrictMode } from "react";
import { MediaActionsProvider } from "./MediaActionsContext";
import { MEDIA_SYNC_RETRY_MS, MediaTile } from "./MediaTile";
import type { GalleryAttachment, GalleryMessage } from "./grouping";

const message: GalleryMessage = { id: "message-1", revision: "1", sender: "self", body: "", timestamp: "2026-01-01T12:00:00.000Z", attachments: [] };
const attachment = (overrides: Partial<GalleryAttachment> = {}): GalleryAttachment => ({ id: "image-1", name: "Photo", mediaType: "image/jpeg", byteSize: 1, state: "ready", previewUrl: "https://example.test/photo.jpg", ...overrides });
const providerProps = (overrides = {}) => ({
  publicCopies: {},
  onSave: vi.fn().mockResolvedValue("saved"),
  onPublish: vi.fn().mockResolvedValue(null),
  onRetry: vi.fn().mockResolvedValue(undefined),
  requestDownload: vi.fn().mockResolvedValue(undefined),
  loadOriginal: vi.fn().mockResolvedValue({ state: "ready", url: "https://example.test/original.jpg" }),
  releaseMedia: vi.fn(),
  canCopyImage: false,
  ...overrides,
});

function renderTile(value = attachment(), props = providerProps()) {
  return render(<MediaActionsProvider {...props}><MediaTile attachment={value} message={{ ...message, attachments: [value] }} variant="cell" onOpen={vi.fn()} /></MediaActionsProvider>);
}

describe("MediaTile", () => {
  afterEach(cleanup);
  it("requests a held video without trying to open an unbound original", async () => {
    const requestMediaSync = vi.fn().mockResolvedValue(undefined);
    const props = providerProps({ requestMediaSync });
    const video = attachment({ id: "video-held", name: "clip.mp4", mediaType: "video/mp4", state: "pending", mediaHold: "held", previewUrl: undefined });
    const onOpen = vi.fn();
    render(<MediaActionsProvider {...props}><MediaTile attachment={video} message={{ ...message, attachments: [video] }} variant="single" onOpen={onOpen} /></MediaActionsProvider>);
    fireEvent.click(screen.getByRole("button", { name: /Sync clip.mp4/ }));
    await waitFor(() => expect(requestMediaSync).toHaveBeenCalledWith(["video-held"]));
    expect(onOpen).not.toHaveBeenCalled();
    expect(props.loadOriginal).not.toHaveBeenCalled();
  });
  it("renders state labels and a loading busy state", () => {
    const props = providerProps();
    const { rerender } = renderTile(attachment({ state: "pending", transfer: "download", previewUrl: undefined }), props);
    expect(screen.getByRole("button", { name: "Loading image" })).toBeInTheDocument();
    expect(document.querySelector("[data-media-tile]")).toHaveAttribute("aria-busy", "true");
    const failed = attachment({ state: "failed", previewUrl: undefined, mediaMeta: { thumbnail: { id: "thumbnail-1", name: "thumbnail", mediaType: "image/jpeg", byteSize: 1, state: "failed" } } });
    rerender(<MediaActionsProvider {...props}><MediaTile attachment={failed} message={{ ...message, attachments: [failed] }} variant="cell" onOpen={vi.fn()} bodyAttributes={{ "data-thumbnail": true }} /></MediaActionsProvider>);
    const failedBody = screen.getByRole("img", { name: "Image failed to load" });
    expect(failedBody).toHaveAttribute("data-attachment-id", "image-1");
    expect(failedBody).toHaveAttribute("data-message-id", "message-1");
    expect(failedBody).toHaveAttribute("data-thumbnail", "true");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(props.onRetry).toHaveBeenCalledWith("thumbnail-1");
  });

  it("uses provider actions and keeps action buttons beside the tile body", () => {
    const props = providerProps();
    renderTile(attachment(), props);
    const tile = screen.getByRole("button", { name: "Photo" }).parentElement!;
    const body = screen.getByRole("button", { name: "Photo" });
    const save = screen.getByRole("button", { name: "Save image" });
    fireEvent.click(save);
    expect(props.onSave).toHaveBeenCalledWith(expect.objectContaining({ id: "image-1" }));
    expect(tile.querySelector("[data-tile-actions]")?.parentElement).toBe(tile);
    expect(tile).toHaveAttribute("data-has-actions", "true");
    expect(body.querySelector("button")).toBeNull();
  });

  it("does not render share actions without a publish provider", () => {
    renderTile(attachment(), providerProps({ onPublish: undefined }));
    expect(screen.queryByRole("button", { name: /public link/i })).not.toBeInTheDocument();
    fireEvent.contextMenu(screen.getByRole("button", { name: "Photo" }));
    expect(screen.queryByRole("menuitem", { name: /public link/i })).not.toBeInTheDocument();
  });

  it("keeps the saving button focusable while a save is in flight", () => {
    const props = providerProps({ onSave: vi.fn(() => new Promise<"saved">(() => undefined)) });
    renderTile(attachment(), props);
    const save = screen.getByRole("button", { name: "Save image" });
    save.focus();
    fireEvent.click(save);
    expect(save).toHaveAttribute("aria-busy", "true");
    expect(save).toHaveAttribute("aria-disabled", "true");
    expect(save).not.toBeDisabled();
    expect(save).toHaveFocus();
    fireEvent.click(save);
    expect(props.onSave).toHaveBeenCalledTimes(1);
  });

  it("opens the context menu with Shift+F10 and restores focus after Escape", async () => {
    renderTile();
    const body = screen.getByRole("button", { name: "Photo" });
    body.focus();
    fireEvent.keyDown(body, { key: "F10", shiftKey: true });
    const menu = screen.getByRole("menu", { name: "Image actions" });
    expect(menu).toBeInTheDocument();
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(screen.getByRole("menuitem", { name: "Create public link" })).toHaveFocus();
    fireEvent.keyDown(menu, { key: "Escape" });
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    await waitFor(() => expect(body).toHaveFocus());
  });

  it("opens the context menu on right click and lets Tab continue after closing", () => {
    renderTile();
    const body = screen.getByRole("button", { name: "Photo" });
    body.focus();
    fireEvent.contextMenu(body, { clientX: 32, clientY: 48 });
    const menu = screen.getByRole("menu", { name: "Image actions" });
    expect(fireEvent.keyDown(menu, { key: "Tab" })).toBe(true);
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("renders no action controls without a provider", () => {
    const value = attachment();
    render(<MediaTile attachment={value} message={{ ...message, attachments: [value] }} variant="cell" onOpen={vi.fn()} />);
    expect(screen.queryByRole("button", { name: "Save image" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Photo" }).parentElement).not.toHaveAttribute("data-has-actions");
    expect(screen.getByRole("button", { name: "Photo" })).toBeInTheDocument();
  });

  it("keeps held originals non-interactive, blurs an available preview, and requests only after a click", async () => {
    const requestMediaSync = vi.fn().mockResolvedValue(undefined);
    const props = providerProps({ requestMediaSync });
    const held = attachment({ state: "pending", transfer: "download", mediaHold: "held", mediaMeta: { thumbnail: { id: "thumbnail-1", name: "thumbnail", mediaType: "image/jpeg", byteSize: 1, state: "ready", previewUrl: "thumb" } } });
    renderTile(held, props);
    expect(screen.queryByRole("button", { name: "Image waiting for Wi-Fi on your phone" })).not.toBeInTheDocument();
    expect(document.querySelector(".tile-held-preview")).toBeInTheDocument();
    expect(props.requestDownload).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: /Sync Photo using mobile data/ }));
    expect(requestMediaSync).toHaveBeenCalledWith(["image-1"]);
    await waitFor(() => expect(document.querySelector("#media-action-status")).toHaveTextContent("Requested original from your phone"));
  });

  it("moves focus from a held sync control to the image when the original resolves", () => {
    const props = providerProps({ requestMediaSync: vi.fn().mockResolvedValue(undefined) });
    const held = attachment({ state: "pending", mediaHold: "held", previewUrl: undefined });
    const { rerender } = renderTile(held, props);
    screen.getByRole("button", { name: /Sync Photo using mobile data/ }).focus();
    const ready = attachment();
    rerender(<MediaActionsProvider {...props}><MediaTile attachment={ready} message={{ ...message, attachments: [ready] }} variant="cell" onOpen={vi.fn()} /></MediaActionsProvider>);
    expect(screen.getByRole("button", { name: "Photo" })).toHaveFocus();
  });

  it("shows a durable requested hold as queued rather than syncing or offline", () => {
    const props = providerProps({ requestMediaSync: vi.fn().mockResolvedValue(undefined) });
    renderTile(attachment({ state: "pending", mediaHold: "requested", previewUrl: undefined }), props);
    expect(screen.getByRole("button", { name: "Requested Photo" })).toHaveAttribute("aria-disabled", "true");
    expect(document.querySelector(".tile-held-note")).toHaveTextContent("Waiting for your phone.");
  });

  it("keeps an accepted sync queued until a requested hold returns to held, then resets", async () => {
    const requestMediaSync = vi.fn().mockResolvedValue(undefined);
    const props = providerProps({ requestMediaSync });
    const tile = (mediaHold: "held" | "requested") => {
      const value = attachment({ state: "pending", mediaHold, previewUrl: undefined });
      return <MediaActionsProvider {...props}><MediaTile attachment={value} message={{ ...message, attachments: [value] }} variant="slide" onOpen={vi.fn()} /></MediaActionsProvider>;
    };
    const { rerender } = render(tile("held"));
    const sync = screen.getByRole("button", { name: /Sync Photo using mobile data/ });
    fireEvent.click(sync);
    await waitFor(() => expect(sync).toHaveAttribute("data-sync-state", "queued"));
    expect(sync).toHaveAttribute("aria-disabled", "true");
    expect(sync).toHaveTextContent("Request queued");
    fireEvent.click(sync);
    expect(requestMediaSync).toHaveBeenCalledTimes(1);
    rerender(tile("held"));
    expect(sync).toHaveAttribute("data-sync-state", "queued");
    rerender(tile("requested"));
    expect(sync).toHaveAttribute("data-sync-state", "queued");
    rerender(tile("held"));
    await waitFor(() => expect(sync).toHaveAttribute("data-sync-state", "idle"));
    expect(sync).not.toHaveAttribute("aria-disabled");
    expect(sync).toHaveTextContent("Sync image");
    expect(document.querySelector(".tile-held-note")).toHaveTextContent("Original syncs on Wi-Fi");
  });

  it("keeps the optimistic queued sync under StrictMode", async () => {
    const props = providerProps({ requestMediaSync: vi.fn().mockResolvedValue(undefined) });
    const value = attachment({ state: "pending", mediaHold: "held", previewUrl: undefined });
    render(<StrictMode><MediaActionsProvider {...props}><MediaTile attachment={value} message={{ ...message, attachments: [value] }} variant="slide" onOpen={vi.fn()} /></MediaActionsProvider></StrictMode>);
    const sync = screen.getByRole("button", { name: /Sync Photo using mobile data/ });
    fireEvent.click(sync);
    await waitFor(() => expect(sync).toHaveAttribute("data-sync-state", "queued"));
    expect(document.querySelector(".tile-held-note")).toHaveTextContent("Waiting for your phone.");
  });

  it("keeps an accepted sync queued for five minutes, then offers a local retry that only a click sends", async () => {
    vi.useFakeTimers();
    try {
      const requestMediaSync = vi.fn().mockResolvedValue(undefined);
      const props = providerProps({ requestMediaSync });
      const held = attachment({ state: "pending", mediaHold: "held", previewUrl: undefined });
      render(<MediaActionsProvider {...props}><MediaTile attachment={held} message={{ ...message, attachments: [held] }} variant="slide" onOpen={vi.fn()} /></MediaActionsProvider>);
      const sync = screen.getByRole("button", { name: /Sync Photo using mobile data/ });
      await act(async () => { fireEvent.click(sync); });
      expect(sync).toHaveAttribute("data-sync-state", "queued");
      act(() => { vi.advanceTimersByTime(MEDIA_SYNC_RETRY_MS - 1); });
      expect(sync).toHaveTextContent("Request queued");
      expect(sync).toHaveAttribute("aria-disabled", "true");
      act(() => { vi.advanceTimersByTime(1); });
      expect(sync).toHaveTextContent("Request again");
      expect(sync).toHaveAttribute("data-retryable", "true");
      expect(sync).not.toHaveAttribute("aria-disabled");
      const retry = screen.getByRole("button", { name: "Request again Photo using mobile data if needed" });
      expect(retry).toBe(sync);
      expect(requestMediaSync).toHaveBeenCalledTimes(1);
      expect(document.querySelector(".tile-held-note")).toHaveTextContent("Waiting for your phone.");
      expect(document.querySelector(".tile-held-note")).not.toHaveTextContent(/offline|failed|couldn't/i);
      expect(sync).not.toHaveTextContent(/offline|failed|couldn't/i);
      await act(async () => { fireEvent.click(retry); });
      expect(requestMediaSync).toHaveBeenCalledTimes(2);
      expect(requestMediaSync).toHaveBeenLastCalledWith(["image-1"]);
      expect(sync).toHaveAttribute("data-sync-state", "queued");
      expect(sync).not.toHaveAttribute("data-retryable");
      act(() => { vi.advanceTimersByTime(MEDIA_SYNC_RETRY_MS - 1); });
      expect(sync).toHaveTextContent("Request queued");
      act(() => { vi.advanceTimersByTime(1); });
      expect(sync).toHaveTextContent("Request again");
    } finally {
      vi.useRealTimers();
    }
  });

  it("labels a compact cell's cooldown Retry and retries a failed retry at once", async () => {
    vi.useFakeTimers();
    try {
      const requestMediaSync = vi.fn().mockRejectedValueOnce(new Error("offline")).mockResolvedValue(undefined);
      const props = providerProps({ requestMediaSync });
      const requested = attachment({ state: "pending", mediaHold: "requested", previewUrl: undefined });
      render(<MediaActionsProvider {...props}><MediaTile attachment={requested} message={{ ...message, attachments: [requested] }} variant="cell" onOpen={vi.fn()} /></MediaActionsProvider>);
      const sync = document.querySelector<HTMLButtonElement>("[data-original-sync-button]")!;
      expect(sync.textContent).toBe("Queued");
      act(() => { vi.advanceTimersByTime(MEDIA_SYNC_RETRY_MS); });
      expect(sync.textContent).toBe("Retry");
      expect(screen.getByRole("button", { name: "Retry Photo using mobile data if needed" })).toBe(sync);
      await act(async () => { fireEvent.click(sync); });
      expect(requestMediaSync).toHaveBeenCalledTimes(1);
      expect(sync.textContent).toBe("Try again");
      expect(sync).not.toHaveAttribute("aria-disabled");
      await act(async () => { fireEvent.click(sync); });
      expect(requestMediaSync).toHaveBeenCalledTimes(2);
      expect(sync.textContent).toBe("Queued");
      expect(sync).toHaveAttribute("aria-disabled", "true");
    } finally {
      vi.useRealTimers();
    }
  });

  it("uses the short queued and sending labels in compact media-tab cells", () => {
    const props = providerProps({ requestMediaSync: vi.fn(() => new Promise<void>(() => undefined)) });
    const held = attachment({ state: "pending", mediaHold: "held", previewUrl: undefined });
    const { rerender } = renderTile(held, props);
    const sync = document.querySelector<HTMLButtonElement>("[data-original-sync-button]")!;
    expect(sync.textContent).toBe("Sync");
    fireEvent.click(sync);
    expect(sync.textContent).toBe("Sending…");
    expect(sync).toHaveAttribute("aria-disabled", "true");
    // An in-flight request overrides the durable queued snapshot until it settles.
    rerender(<MediaActionsProvider {...props}><MediaTile attachment={attachment({ state: "pending", mediaHold: "requested", previewUrl: undefined })} message={message} variant="cell" onOpen={vi.fn()} /></MediaActionsProvider>);
    expect(sync.textContent).toBe("Sending…");
  });
});
