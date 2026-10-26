import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaActionsProvider, useMediaActions } from "./MediaActionsContext";
import type { GalleryAttachment } from "./grouping";

const attachment: GalleryAttachment = { id: "image-1", name: "Photo", mediaType: "image/jpeg", byteSize: 1, state: "ready" };
const defaults = () => ({
  publicCopies: {}, onSave: vi.fn().mockResolvedValue("saved"), onPublish: vi.fn().mockResolvedValue(null), onRetry: vi.fn().mockResolvedValue(undefined), requestDownload: vi.fn().mockResolvedValue(undefined), loadOriginal: vi.fn().mockResolvedValue({ state: "ready", url: "https://example.test/image" }), releaseMedia: vi.fn(), canCopyImage: false,
});

function Controls() {
  const actions = useMediaActions()!;
  return <><button onClick={() => actions.save(attachment)}>save</button><button onClick={() => actions.shareLink(attachment)}>link</button><button onClick={() => actions.copyImage(attachment)}>copy image</button><button onClick={() => actions.requestDownload(attachment.id)}>download</button></>;
}

function renderActions(props = defaults()) {
  return { props, ...render(<MediaActionsProvider {...props}><Controls /></MediaActionsProvider>) };
}

afterEach(() => { cleanup(); vi.useRealTimers(); vi.unstubAllGlobals(); });

describe("MediaActionsProvider", () => {
  it("deduplicates in-flight download requests", async () => {
    let resolve!: () => void;
    const props = defaults();
    props.requestDownload.mockImplementation(() => new Promise<void>((done) => { resolve = done; }));
    renderActions(props);
    fireEvent.click(screen.getByText("download"));
    fireEvent.click(screen.getByText("download"));
    expect(props.requestDownload).toHaveBeenCalledTimes(1);
    resolve();
    await waitFor(() => expect(props.requestDownload).toHaveBeenCalledTimes(1));
  });

  it("keeps cancelled publication silent", async () => {
    const props = defaults();
    renderActions(props);
    fireEvent.click(screen.getByText("link"));
    await waitFor(() => expect(props.onPublish).toHaveBeenCalled());
    expect(screen.getByRole("status")).toBeEmptyDOMElement();
  });

  it("copies an existing link without publishing", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    const props = defaults();
    props.publicCopies = { [attachment.id]: { url: "https://example.test/public", expiresInSeconds: 60 } };
    renderActions(props);
    fireEvent.click(screen.getByText("link"));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith("https://example.test/public"));
    expect(props.onPublish).not.toHaveBeenCalled();
    expect(screen.getByRole("status")).toHaveTextContent("Link copied");
  });

  it("deduplicates native publish confirmation and warns after a successful publication", async () => {
    vi.useFakeTimers();
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    let publish!: (copy: { url: string; expiresInSeconds: number }) => void;
    const props = defaults();
    props.onPublish.mockImplementation(() => new Promise((resolve) => { publish = resolve; }));
    renderActions(props);
    fireEvent.click(screen.getByText("link"));
    fireEvent.click(screen.getByText("link"));
    expect(props.onPublish).toHaveBeenCalledTimes(1);
    publish({ url: "https://example.test/public", expiresInSeconds: 60 });
    await act(async () => {});
    expect(writeText).toHaveBeenCalledWith("https://example.test/public");
    expect(screen.getByRole("status")).toHaveAttribute("data-tone", "warning");
    act(() => vi.advanceTimersByTime(6000));
    expect(screen.getByRole("status")).toBeEmptyDOMElement();
  });

  it("announces pending saves, clears info after four seconds, and keeps saved and cancelled outcomes silent", async () => {
    vi.useFakeTimers();
    const props = defaults();
    props.onSave.mockResolvedValue("pending");
    renderActions(props);
    fireEvent.click(screen.getByText("save"));
    await act(async () => {});
    expect(screen.getByRole("status")).toHaveTextContent("Downloading Photo from your phone before saving");
    act(() => vi.advanceTimersByTime(4000));
    expect(screen.getByRole("status")).toBeEmptyDOMElement();
    cleanup();
    const silentProps = defaults();
    silentProps.onSave.mockResolvedValueOnce("saved").mockResolvedValueOnce("cancelled");
    renderActions(silentProps);
    fireEvent.click(screen.getByText("save"));
    await act(async () => {});
    fireEvent.click(screen.getByText("save"));
    await act(async () => {});
    expect(screen.getByRole("status")).toBeEmptyDOMElement();
  });

  it("moves feedback into an open dialog at announce time", async () => {
    const props = defaults();
    props.onSave.mockResolvedValue("pending");
    renderActions(props);
    const dialog = document.createElement("dialog");
    dialog.setAttribute("open", "");
    document.body.append(dialog);
    fireEvent.click(screen.getByText("save"));
    await waitFor(() => expect(dialog.querySelector("#media-action-status")).toHaveTextContent("Downloading Photo"));
  });

  it("releases media when copy image loading fails", async () => {
    class Item { constructor(_: Record<string, Promise<Blob>>) {} }
    vi.stubGlobal("ClipboardItem", Item);
    vi.stubGlobal("navigator", { clipboard: { write: vi.fn().mockResolvedValue(undefined) } });
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new Error("network")));
    const props = defaults();
    props.canCopyImage = true;
    renderActions(props);
    fireEvent.click(screen.getByText("copy image"));
    await waitFor(() => expect(props.releaseMedia).toHaveBeenCalledWith(attachment.id, "https://example.test/image"));
    expect(screen.getByRole("status")).toHaveTextContent("Couldn't copy image");
  });

  it("passes ClipboardItem a promise before the original load resolves", () => {
    let resolve!: (result: { state: "ready"; url: string }) => void;
    const loadOriginal = vi.fn(() => new Promise<{ state: "ready"; url: string }>((done) => { resolve = done; }));
    const received: Promise<Blob>[] = [];
    class Item { constructor(items: Record<string, Promise<Blob>>) { received.push(items["image/png"]); } }
    vi.stubGlobal("ClipboardItem", Item);
    vi.stubGlobal("navigator", { clipboard: { write: vi.fn().mockResolvedValue(undefined) } });
    const props = defaults();
    props.canCopyImage = true;
    props.loadOriginal = loadOriginal;
    renderActions(props);
    fireEvent.click(screen.getByText("copy image"));
    expect(loadOriginal).toHaveBeenCalledWith(attachment.id);
    expect(received).toHaveLength(1);
    resolve({ state: "ready", url: "https://example.test/image" });
  });
});
