import "@testing-library/jest-dom/vitest";
import { useState, type ReactElement } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaLightbox } from "./MediaLightbox";
import { MediaActionsProvider } from "./MediaActionsContext";

const items = ["one", "two"].map((id) => ({ attachment: { id, name: id, mediaType: "image/jpeg", byteSize: 1, state: "ready" as const }, message: { id: `message-${id}`, revision: "1", sender: "self" as const, body: "", timestamp: "2026-01-01T00:00:00.000Z", attachments: [] } }));

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.useRealTimers(); });

function renderWithActions(ui: ReactElement) {
  return render(<MediaActionsProvider publicCopies={{}} onSave={vi.fn().mockResolvedValue("saved")} onPublish={vi.fn().mockResolvedValue(null)} onRetry={vi.fn().mockResolvedValue(undefined)} requestDownload={vi.fn().mockResolvedValue(undefined)} loadOriginal={vi.fn().mockResolvedValue({ state: "pending" })} releaseMedia={vi.fn()} canCopyImage>{ui}</MediaActionsProvider>);
}

describe("MediaLightbox", () => {
  it("keeps a remote-only policy removal permanent without requesting or offering recovery actions", () => {
    const requestOriginal = vi.fn();
    renderWithActions(<MediaLightbox items={[{ ...items[0], attachment: { ...items[0].attachment, remoteState: "removed_by_storage_policy", localAvailable: false } }]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={vi.fn()} saveMedia={vi.fn()} />);

    expect(screen.getByText("This attachment was removed from hosted storage to make room. The message remains.")).toBeInTheDocument();
    expect(requestOriginal).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: /save|public|retry/i })).not.toBeInTheDocument();
  });

  it("keeps a verified local policy-removed attachment usable and labels it", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "ready" as const, url: "blob:local" });
    renderWithActions(<MediaLightbox items={[{ ...items[0], attachment: { ...items[0].attachment, remoteState: "removed_by_storage_policy", localAvailable: true } }]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={requestOriginal} saveMedia={vi.fn()} />);

    expect(await screen.findByText("Hosted copy removed. Available on this device.")).toBeInTheDocument();
    expect(requestOriginal).toHaveBeenCalledWith("one");
    expect(await screen.findByRole("button", { name: "Save image" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Create public link" })).toBeInTheDocument();
  });

  it("navigates with keys and releases a loaded original on close", async () => {
    const releaseMedia = vi.fn();
    const requestOriginal = vi.fn().mockResolvedValue({ state: "ready" as const, url: "blob:one" });
    const view = render(<MediaLightbox items={items} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={requestOriginal} saveMedia={vi.fn()} releaseMedia={releaseMedia} />);
    await act(async () => {});
    expect(requestOriginal).toHaveBeenCalledWith("one");
    expect(screen.getByRole("button", { name: "Newer image" })).toHaveAttribute("title", "Newer image");
    expect(screen.getByRole("button", { name: "Older image" })).toHaveAttribute("title", "Older image");
    fireEvent.keyDown(document, { key: "ArrowRight" });
    expect(screen.getByText("Image 2 of 2")).toBeInTheDocument();
    view.unmount();
    expect(releaseMedia).toHaveBeenCalledWith("one", "blob:one");
  });

  it("releases a late original resolution after navigation", async () => {
    let resolve!: (result: { state: "ready"; url: string } | { state: "pending" }) => void;
    const releaseMedia = vi.fn();
    const requestOriginal = vi.fn()
      .mockImplementationOnce(() => new Promise<{ state: "ready"; url: string } | { state: "pending" }>((done) => { resolve = done; }))
      .mockResolvedValue({ state: "pending" as const });
    render(<MediaLightbox items={items} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={requestOriginal} saveMedia={vi.fn()} releaseMedia={releaseMedia} />);
    await act(async () => {});
    fireEvent.click(screen.getByRole("button", { name: "Older image" }));
    await act(async () => resolve({ state: "ready" as const, url: "blob:late" }));
    expect(releaseMedia).toHaveBeenCalledWith("one", "blob:late");
  });

  it("shows pending feedback and lets failed media retry through the injected request operation", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "pending" as const });
    const pending = [{ ...items[0], attachment: { ...items[0].attachment, transfer: "download" as const } }];
    const view = render(<MediaLightbox items={pending} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={requestOriginal} saveMedia={vi.fn()} />);
    await act(async () => {});
    expect(requestOriginal).toHaveBeenCalledWith("one");
    expect(screen.getByRole("status")).toHaveTextContent("Downloading from phone…");
    view.unmount();
    render(<MediaLightbox items={[{ ...items[0], attachment: { ...items[0].attachment, state: "failed" } }]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={requestOriginal} saveMedia={vi.fn()} />);
    await act(async () => {});
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(requestOriginal).toHaveBeenCalledTimes(3);
  });

  it("adds the waiting guidance after fifteen seconds", async () => {
    vi.useFakeTimers();
    const requestOriginal = vi.fn().mockResolvedValue({ state: "pending" as const });
    render(<MediaLightbox items={[{ ...items[0], attachment: { ...items[0].attachment, transfer: "download" as const } }]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={requestOriginal} saveMedia={vi.fn()} />);
    await act(async () => {});
    expect(screen.queryByText("Still waiting — make sure your phone is online.")).not.toBeInTheDocument();
    await act(async () => { vi.advanceTimersByTime(15000); });
    expect(screen.getByText("Still waiting — make sure your phone is online.")).toBeInTheDocument();
  });

  it("shows only a spinner while an original is loading", () => {
    render(<MediaLightbox items={[items[0]]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={vi.fn(() => new Promise<never>(() => {}))} checkOriginal={vi.fn()} saveMedia={vi.fn()} />);
    expect(screen.getByLabelText("Loading image")).toBeInTheDocument();
    expect(screen.queryByText("Downloading from phone…")).not.toBeInTheDocument();
  });

  it("keeps a loaded image visible when saving fails", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "ready" as const, url: "blob:one" });
    render(<MediaLightbox items={[items[0]]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={requestOriginal} saveMedia={vi.fn().mockRejectedValue(new Error("save failed"))} />);
    await act(async () => {});
    fireEvent.click(screen.getByRole("button", { name: "Save image" }));
    await act(async () => {});
    expect(screen.getByRole("img", { name: "one" })).toHaveAttribute("src", "blob:one");
    expect(screen.getByRole("alert")).toHaveTextContent("Could not save media.");
    expect(document.querySelector("#lightbox-retry-btn")).not.toBeInTheDocument();
  });

  it("shows only Save without media actions and adds Link and Copy with a provider", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "ready" as const, url: "blob:one" });
    const properties = { items: [items[0]], initialAttachmentId: "one", onDismiss: vi.fn(), requestOriginal, checkOriginal: requestOriginal, saveMedia: vi.fn() };
    const view = render(<MediaLightbox {...properties} />);
    await act(async () => {});
    expect(screen.getByRole("button", { name: "Save image" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Create public link" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Copy image" })).not.toBeInTheDocument();
    view.unmount();
    renderWithActions(<MediaLightbox {...properties} />);
    await act(async () => {});
    expect(screen.getByRole("button", { name: "Create public link" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy image" })).toBeInTheDocument();
  });

  it("lets the app window be dragged from the area around the in-app viewer", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "ready" as const, url: "blob:one" });
    render(<MediaLightbox items={[items[0]]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={requestOriginal} saveMedia={vi.fn()} />);
    await act(async () => {});
    expect(document.querySelector("#media-lightbox")).toHaveAttribute("data-tauri-drag-region", "true");
  });

  it("renders the native window chrome without a modal or sharing action", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "ready" as const, url: "blob:one" });
    render(<MediaLightbox chrome="window" items={[items[0]]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={requestOriginal} saveMedia={vi.fn()} />);

    await act(async () => {});

    expect(screen.getByRole("main", { name: "Media viewer" })).toHaveAttribute("id", "media-viewer-window");
    expect(document.querySelector("#media-lightbox")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Close media viewer" }).parentElement).toHaveAttribute("data-tauri-drag-region", "true");
    expect(screen.queryByRole("button", { name: "Create public link" })).not.toBeInTheDocument();
  });

  it("advances through multiple adjacent pages and disables next at exhaustion", async () => {
    const third = { ...items[0], attachment: { ...items[0].attachment, id: "three", name: "three" }, message: { ...items[0].message, id: "message-three" } };
    const loadAdjacentPage = vi.fn()
      .mockResolvedValueOnce({ items: [items[1]], hasNextPage: true })
      .mockResolvedValueOnce({ items: [third], hasNextPage: false });
    const requestOriginal = vi.fn().mockResolvedValue({ state: "pending" as const });
    render(<MediaLightbox items={[items[0]]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={requestOriginal} saveMedia={vi.fn()} loadAdjacentPage={loadAdjacentPage} hasNextPage />);
    expect(screen.getByText("Image 1 of at least 1 loaded")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Older image" }));
    await act(async () => {});
    fireEvent.click(screen.getByRole("button", { name: "Older image" }));
    await act(async () => {});
    expect(screen.getByText("Image 3 of 3")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Older image" })).toBeDisabled();
    expect(loadAdjacentPage).toHaveBeenCalledTimes(2);
  });

  it("skips duplicate-only pages without falsely exhausting a direction", async () => {
    const third = { ...items[0], attachment: { ...items[0].attachment, id: "three", name: "three" }, message: { ...items[0].message, id: "message-three" } };
    const loadAdjacentPage = vi.fn()
      .mockResolvedValueOnce({ items: [], hasNextPage: true, cursor: "duplicate-page" })
      .mockResolvedValueOnce({ items: [third], hasNextPage: false });
    render(<MediaLightbox items={[items[0]]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={vi.fn().mockResolvedValue({ state: "pending" })} checkOriginal={vi.fn().mockResolvedValue({ state: "pending" })} saveMedia={vi.fn()} loadAdjacentPage={loadAdjacentPage} hasNextPage />);
    fireEvent.click(screen.getByRole("button", { name: "Older image" }));
    await act(async () => {});
    expect(screen.getByText("Image 2 of 2")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Older image" })).toBeDisabled();
    expect(loadAdjacentPage).toHaveBeenCalledTimes(2);
  });

  it("uses a passive check for each pending state hint", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "pending" as const });
    const checkOriginal = vi.fn().mockResolvedValue({ state: "pending" as const });
    const view = render(<MediaLightbox items={[items[0]]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={checkOriginal} saveMedia={vi.fn()} retryKey={0} />);
    await act(async () => {});
    view.rerender(<MediaLightbox items={[items[0]]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={checkOriginal} saveMedia={vi.fn()} retryKey={1} />);
    await act(async () => {});
    expect(requestOriginal).toHaveBeenCalledTimes(1);
    expect(checkOriginal).toHaveBeenCalledTimes(1);
  });

  it.each([
    { initialAttachmentId: "one", navigationButton: "Older image", selectedAttachmentId: "two" },
    { initialAttachmentId: "two", navigationButton: "Newer image", selectedAttachmentId: "one" },
  ])("keeps the newly selected original when navigation and a retry hint are batched", async ({ initialAttachmentId, navigationButton, selectedAttachmentId }) => {
    let resolveOriginal!: (result: { state: "ready"; url: string }) => void;
    const requestOriginal = vi.fn()
      .mockResolvedValueOnce({ state: "pending" as const })
      .mockImplementationOnce(() => new Promise<{ state: "ready"; url: string }>((resolve) => { resolveOriginal = resolve; }));
    const releaseMedia = vi.fn();

    function LightboxWithRetryHint() {
      const [retryKey, setRetryKey] = useState(0);
      return <div onClickCapture={() => setRetryKey(current => current + 1)}><MediaLightbox items={items} initialAttachmentId={initialAttachmentId} onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={vi.fn().mockResolvedValue({ state: "pending" })} saveMedia={vi.fn()} releaseMedia={releaseMedia} retryKey={retryKey} /></div>;
    }

    render(<LightboxWithRetryHint />);
    await act(async () => {});
    fireEvent.click(screen.getByRole("button", { name: navigationButton }));
    await act(async () => {});
    await act(async () => resolveOriginal({ state: "ready", url: `blob:${selectedAttachmentId}` }));

    expect(screen.getByRole("img", { name: selectedAttachmentId })).toHaveAttribute("src", `blob:${selectedAttachmentId}`);
    expect(releaseMedia).not.toHaveBeenCalledWith(selectedAttachmentId, `blob:${selectedAttachmentId}`);
  });

  it.each([
    { initialAttachmentId: "one", navigationButton: "Older image", selectedAttachmentId: "two" },
    { initialAttachmentId: "two", navigationButton: "Newer image", selectedAttachmentId: "one" },
  ])("shows the next original after leaving a loaded image", async ({ initialAttachmentId, navigationButton, selectedAttachmentId }) => {
    let resolveSelected!: (result: { state: "ready"; url: string }) => void;
    const requestOriginal = vi.fn((id: string) => id === initialAttachmentId
      ? Promise.resolve({ state: "ready" as const, url: `blob:${id}` })
      : new Promise<{ state: "ready"; url: string }>((resolve) => { resolveSelected = resolve; }));
    const releaseMedia = vi.fn();
    render(<MediaLightbox items={items} initialAttachmentId={initialAttachmentId} onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={vi.fn().mockResolvedValue({ state: "pending" })} saveMedia={vi.fn()} releaseMedia={releaseMedia} />);
    await act(async () => {});
    expect(screen.getByRole("img", { name: initialAttachmentId })).toHaveAttribute("src", `blob:${initialAttachmentId}`);

    fireEvent.click(screen.getByRole("button", { name: navigationButton }));
    await act(async () => resolveSelected({ state: "ready", url: `blob:${selectedAttachmentId}` }));

    expect(screen.getByRole("img", { name: selectedAttachmentId })).toHaveAttribute("src", `blob:${selectedAttachmentId}`);
    expect(releaseMedia).toHaveBeenCalledTimes(1);
    expect(releaseMedia).toHaveBeenCalledWith(initialAttachmentId, `blob:${initialAttachmentId}`);
  });

  it("offers Retry for a failed original even when the producer marked the preview unavailable", async () => {
    const failed = [{ ...items[0], attachment: { ...items[0].attachment, state: "failed" as const, mediaMeta: { previewUnavailable: "thumbnail_unavailable" as const } } }];
    render(<MediaLightbox items={failed} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={vi.fn().mockRejectedValue(new Error("offline"))} checkOriginal={vi.fn().mockResolvedValue({ state: "pending" as const })} saveMedia={vi.fn()} />);
    await act(async () => {});
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
    expect(document.querySelector("[data-lightbox-state]")).toHaveAttribute("data-lightbox-state", "failed");
  });

  it("stops after a repeated cursor with a retryable error and without exhausting the direction", async () => {
    const loadAdjacentPage = vi.fn()
      .mockResolvedValueOnce({ items: [], hasNextPage: true, cursor: "same" })
      .mockResolvedValueOnce({ items: [], hasNextPage: true, cursor: "same" })
      .mockResolvedValueOnce({ items: [items[1]], hasNextPage: false });
    render(<MediaLightbox items={[items[0]]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={vi.fn().mockResolvedValue({ state: "pending" as const })} checkOriginal={vi.fn().mockResolvedValue({ state: "pending" as const })} saveMedia={vi.fn()} loadAdjacentPage={loadAdjacentPage} hasNextPage />);
    fireEvent.click(screen.getByRole("button", { name: "Older image" }));
    await act(async () => {});
    expect(loadAdjacentPage).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't load more media.");
    expect(screen.getByText("Image 1 of at least 1 loaded")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Older image" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Older image" }));
    await act(async () => {});
    expect(loadAdjacentPage).toHaveBeenCalledTimes(3);
    expect(screen.getByText("Image 2 of 2")).toBeInTheDocument();
  });

  it("stops on an empty page without a cursor with a retryable error and without exhausting the direction", async () => {
    const loadAdjacentPage = vi.fn().mockResolvedValueOnce({ items: [], hasNextPage: true });
    render(<MediaLightbox items={[items[0]]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={vi.fn().mockResolvedValue({ state: "pending" as const })} checkOriginal={vi.fn().mockResolvedValue({ state: "pending" as const })} saveMedia={vi.fn()} loadAdjacentPage={loadAdjacentPage} hasNextPage />);
    fireEvent.click(screen.getByRole("button", { name: "Older image" }));
    await act(async () => {});
    expect(loadAdjacentPage).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't load more media.");
    expect(screen.getByRole("button", { name: "Older image" })).toBeEnabled();
  });

  it("caps duplicate-only paging at ten pages with a retryable error and without exhausting the direction", async () => {
    let calls = 0;
    const loadAdjacentPage = vi.fn(async () => ({ items: [], hasNextPage: true, cursor: `page-${++calls}` }));
    render(<MediaLightbox items={[items[0]]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={vi.fn().mockResolvedValue({ state: "pending" as const })} checkOriginal={vi.fn().mockResolvedValue({ state: "pending" as const })} saveMedia={vi.fn()} loadAdjacentPage={loadAdjacentPage} hasNextPage />);
    fireEvent.click(screen.getByRole("button", { name: "Older image" }));
    await act(async () => {});
    expect(loadAdjacentPage).toHaveBeenCalledTimes(10);
    expect(screen.getByRole("alert")).toHaveTextContent("Couldn't load more media.");
    expect(screen.getByText("Image 1 of at least 1 loaded")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Older image" })).toBeEnabled();
  });
});
