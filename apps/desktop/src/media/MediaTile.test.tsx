import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaActionsProvider } from "./MediaActionsContext";
import { MediaTile } from "./MediaTile";
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
});
