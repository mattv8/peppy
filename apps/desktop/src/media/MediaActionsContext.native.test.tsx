import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaActionsProvider, useMediaActions } from "./MediaActionsContext";
import { MediaTile } from "./MediaTile";
import type { GalleryAttachment, GalleryMessage } from "./grouping";

const attachment: GalleryAttachment = { id: "image-1", name: "Photo", mediaType: "image/jpeg", byteSize: 1, state: "ready", previewUrl: "https://example.test/preview.jpg" };
const message: GalleryMessage = { id: "message-1", revision: "1", sender: "self", body: "", timestamp: "2026-01-01T12:00:00.000Z", attachments: [] };
const providerProps = (overrides: Record<string, unknown> = {}) => ({
  publicCopies: {},
  onSave: vi.fn().mockResolvedValue("saved"),
  onPublish: vi.fn().mockResolvedValue(null),
  onRetry: vi.fn().mockResolvedValue(undefined),
  requestDownload: vi.fn().mockResolvedValue(undefined),
  loadOriginal: vi.fn().mockResolvedValue({ state: "ready", url: "https://example.test/original" }),
  releaseMedia: vi.fn(),
  canCopyImage: false,
  ...overrides,
});

function Controls() {
  const actions = useMediaActions()!;
  return <><button onClick={() => actions.copyImage(attachment)}>copy image</button><span data-testid="capability">{String(actions.canCopyImage)}</span></>;
}

function stubBrowserClipboard() {
  const ClipboardItemSpy = vi.fn();
  const write = vi.fn().mockResolvedValue(undefined);
  const fetchSpy = vi.fn();
  vi.stubGlobal("ClipboardItem", ClipboardItemSpy);
  vi.stubGlobal("navigator", { clipboard: { write } });
  vi.stubGlobal("fetch", fetchSpy);
  return { ClipboardItemSpy, write, fetchSpy };
}

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe("native Copy image", () => {
  it("routes through the native command without the browser clipboard, fetch, or original URLs", async () => {
    const browser = stubBrowserClipboard();
    const copyNativeImage = vi.fn().mockResolvedValue(undefined);
    const props = providerProps({ copyNativeImage });
    render(<MediaActionsProvider {...props}><Controls /></MediaActionsProvider>);
    expect(screen.getByTestId("capability")).toHaveTextContent("true");
    fireEvent.click(screen.getByText("copy image"));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Image copied"));
    expect(copyNativeImage).toHaveBeenCalledWith("image-1");
    expect(props.loadOriginal).not.toHaveBeenCalled();
    expect(browser.fetchSpy).not.toHaveBeenCalled();
    expect(browser.ClipboardItemSpy).not.toHaveBeenCalled();
    expect(browser.write).not.toHaveBeenCalled();
  });

  it("says the original is downloading when native reports media-pending, with no background write", async () => {
    const browser = stubBrowserClipboard();
    const copyNativeImage = vi.fn().mockRejectedValue({ code: "media-pending" });
    const props = providerProps({ copyNativeImage });
    render(<MediaActionsProvider {...props}><Controls /></MediaActionsProvider>);
    fireEvent.click(screen.getByText("copy image"));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Downloading image from your phone. Copy again when it finishes."));
    expect(screen.getByRole("status")).toHaveAttribute("data-tone", "info");
    expect(browser.write).not.toHaveBeenCalled();
    expect(props.releaseMedia).not.toHaveBeenCalled();
  });

  it("reports other native failures as errors", async () => {
    const copyNativeImage = vi.fn().mockRejectedValue({ code: "internal-failure" });
    render(<MediaActionsProvider {...providerProps({ copyNativeImage })}><Controls /></MediaActionsProvider>);
    fireEvent.click(screen.getByText("copy image"));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Couldn't copy image"));
    expect(screen.getByRole("status")).toHaveAttribute("data-tone", "error");
  });

  it("offers Copy image from the tile context menu through the native command", async () => {
    const copyNativeImage = vi.fn().mockResolvedValue(undefined);
    const props = providerProps({ copyNativeImage });
    render(<MediaActionsProvider {...props}><MediaTile attachment={attachment} message={{ ...message, attachments: [attachment] }} variant="cell" onOpen={vi.fn()} /></MediaActionsProvider>);
    fireEvent.contextMenu(screen.getByRole("button", { name: "Photo" }), { clientX: 4, clientY: 4 });
    fireEvent.click(screen.getByRole("menuitem", { name: "Copy image" }));
    await waitFor(() => expect(copyNativeImage).toHaveBeenCalledWith("image-1"));
    expect(props.loadOriginal).not.toHaveBeenCalled();
  });

  it("leaves browser hosts without a native command on the existing capability check", () => {
    render(<MediaActionsProvider {...providerProps()}><Controls /></MediaActionsProvider>);
    expect(screen.getByTestId("capability")).toHaveTextContent("false");
  });

  it("ignores a second Copy while the first native copy is in flight, and says it is copying", async () => {
    const settle = deferred<void>();
    const copyNativeImage = vi.fn(() => settle.promise);
    render(<MediaActionsProvider {...providerProps({ copyNativeImage })}><Controls /></MediaActionsProvider>);
    fireEvent.click(screen.getByText("copy image"));
    fireEvent.click(screen.getByText("copy image"));
    expect(copyNativeImage).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Copying image…"));
    settle.resolve();
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Image copied"));
    fireEvent.click(screen.getByText("copy image"));
    await waitFor(() => expect(copyNativeImage).toHaveBeenCalledTimes(2));
  });

  it.each([
    ["media-busy", "Media is busy. Try Copy again shortly.", "info"],
    ["media-too-large", "This image is too large to copy safely.", "error"],
    ["unsupported", "Only PNG, JPEG, WebP, and GIF images can be copied.", "error"],
    ["attachment-invalid", "This image could not be decoded for copying.", "error"],
    ["clipboard-unavailable", "The system clipboard is unavailable. Try again.", "error"],
  ])("maps native %s to its own message", async (code, text, tone) => {
    const copyNativeImage = vi.fn().mockRejectedValue({ code });
    render(<MediaActionsProvider {...providerProps({ copyNativeImage })}><Controls /></MediaActionsProvider>);
    fireEvent.click(screen.getByText("copy image"));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent(text));
    expect(screen.getByRole("status")).toHaveAttribute("data-tone", tone);
  });
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
