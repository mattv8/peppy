import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaPanel } from "./MediaPanel";

const item = (id: string, state: "ready" | "pending" = "ready") => ({ attachment: { id, name: id, mediaType: "image/jpeg", byteSize: 1, state, previewUrl: state === "ready" ? "thumb" : undefined }, message: { id: `message-${id}`, revision: "1", sender: "self" as const, body: "", timestamp: "2026-01-01T00:00:00.000Z", attachments: [] } });

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => { resolve = next; });
  return { promise, resolve };
}

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.useRealTimers(); vi.unstubAllGlobals(); });

describe("MediaPanel", () => {
  it("loads newest-first pages and retries failures", async () => {
    const loadPage = vi.fn().mockResolvedValueOnce({ items: [item("one")], nextCursor: "next" }).mockResolvedValueOnce({ items: [item("two")], nextCursor: undefined });
    render(<MediaPanel loadPage={loadPage} onOpen={vi.fn()} />);
    expect(await screen.findByRole("button", { name: "one" })).toBeInTheDocument();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Load more" })));
    expect(await screen.findByRole("button", { name: "two" })).toBeInTheDocument();
  });

  it("uses MediaTile cells with tile hooks", async () => {
    render(<MediaPanel loadPage={vi.fn().mockResolvedValue({ items: [item("one")] })} onOpen={vi.fn()} />);
    const cell = await screen.findByRole("button", { name: "one" });
    expect(cell).toHaveAttribute("data-media-cell");
    expect(cell.closest("[data-media-tile]")).toHaveAttribute("data-variant", "cell");
    expect(cell.querySelector("img")).toHaveAttribute("src", "thumb");
  });

  it("announces producer-confirmed unavailable previews only without a usable preview", async () => {
    const unavailable = { attachment: { id: "original", name: "original", mediaType: "image/jpeg", byteSize: 1, state: "pending" as const, transfer: "download" as const, mediaMeta: { previewUnavailable: "thumbnail_unavailable" as const } }, message: { id: "message-original", revision: "1", sender: "self" as const, body: "", timestamp: "2026-01-01T00:00:00.000Z", attachments: [] } };
    render(<MediaPanel loadPage={vi.fn().mockResolvedValue({ items: [unavailable] })} onOpen={vi.fn()} />);
    const cell = await screen.findByRole("button", { name: "Preview unavailable" });
    expect(cell).toBeInTheDocument();
    expect(cell.closest("[data-media-tile]")).toHaveAttribute("data-state", "unavailable");
  });

  it("uses the shared loading and failure states", async () => {
    const failed = { ...item("failed"), attachment: { ...item("failed").attachment, state: "failed" as const, previewUrl: undefined } };
    const loading = item("loading", "pending");
    render(<MediaPanel loadPage={vi.fn().mockResolvedValue({ items: [failed, loading] })} onOpen={vi.fn()} />);
    expect(await screen.findByRole("img", { name: "Image failed to load" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Loading image" })).toBeInTheDocument();
  });

  it("renders a reused attachment id from two messages without duplicate-key warnings", async () => {
    const reused = (messageId: string) => ({ attachment: { id: "shared", name: "shared", mediaType: "image/jpeg", byteSize: 1, state: "ready" as const, previewUrl: "thumb" }, message: { id: messageId, revision: "1", sender: "self" as const, body: "", timestamp: "2026-01-01T00:00:00.000Z", attachments: [] } });
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => {});
    const { container } = render(<MediaPanel loadPage={vi.fn().mockResolvedValue({ items: [reused("message-a"), reused("message-b")] })} onOpen={vi.fn()} />);
    await screen.findAllByRole("button", { name: "shared" });
    expect(container.querySelectorAll("[data-media-cell]")).toHaveLength(2);
    expect(consoleError.mock.calls.some(([message]) => String(message).includes("same key"))).toBe(false);
    consoleError.mockRestore();
  });

  it("debounces refreshes and merges only non-ready items by message and attachment id", async () => {
    vi.useFakeTimers();
    const ready = item("ready");
    const pending = item("pending", "pending");
    const refreshedPending = { ...pending, attachment: { ...pending.attachment, state: "ready" as const, previewUrl: "updated" } };
    const loadPage = vi.fn().mockResolvedValueOnce({ items: [ready, pending], nextCursor: "next" }).mockResolvedValueOnce({ items: [refreshedPending, { ...ready, attachment: { ...ready.attachment, previewUrl: "ignored" } }], nextCursor: "refreshed-next" });
    const { rerender } = render(<MediaPanel loadPage={loadPage} onOpen={vi.fn()} refreshKey="one" />);
    await act(async () => {});
    rerender(<MediaPanel loadPage={loadPage} onOpen={vi.fn()} refreshKey="two" />);
    await act(async () => { await vi.advanceTimersByTimeAsync(750); });
    expect(loadPage).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("button", { name: "pending" }).querySelector("img")).toHaveAttribute("src", "updated");
    expect(screen.getByRole("button", { name: "ready" }).querySelector("img")).toHaveAttribute("src", "thumb");
    expect(screen.getByRole("button", { name: "Load more" })).toBeInTheDocument();
  });

  it("does not refetch when every loaded tile is ready", async () => {
    vi.useFakeTimers();
    const loadPage = vi.fn().mockResolvedValue({ items: [item("ready")] });
    const { rerender } = render(<MediaPanel loadPage={loadPage} onOpen={vi.fn()} refreshKey="one" />);
    await act(async () => {});
    rerender(<MediaPanel loadPage={loadPage} onOpen={vi.fn()} refreshKey="two" />);
    await act(async () => { await vi.advanceTimersByTimeAsync(750); });
    expect(loadPage).toHaveBeenCalledTimes(1);
  });

  it("does not re-arm refreshes when only loadPage changes", async () => {
    vi.useFakeTimers();
    const initialLoadPage = vi.fn().mockResolvedValue({ items: [item("pending", "pending")] });
    const replacementLoadPage = vi.fn().mockResolvedValue({ items: [item("pending", "pending")] });
    const { rerender } = render(<MediaPanel loadPage={initialLoadPage} onOpen={vi.fn()} refreshKey="one" />);
    await act(async () => {});
    rerender(<MediaPanel loadPage={replacementLoadPage} onOpen={vi.fn()} refreshKey="one" />);
    await act(async () => { await vi.advanceTimersByTimeAsync(750); });
    expect(initialLoadPage).toHaveBeenCalledTimes(1);
    expect(replacementLoadPage).not.toHaveBeenCalled();
  });

  it("discards a refresh after loading more and clears its busy state", async () => {
    vi.useFakeTimers();
    const refresh = deferred<{ items: ReturnType<typeof item>[]; nextCursor?: unknown }>();
    const pending = item("pending", "pending");
    const older = item("older");
    const loadPage = vi.fn()
      .mockResolvedValueOnce({ items: [pending], nextCursor: "next" })
      .mockImplementationOnce(() => refresh.promise)
      .mockResolvedValueOnce({ items: [older], nextCursor: undefined });
    const { container, rerender } = render(<MediaPanel loadPage={loadPage} onOpen={vi.fn()} refreshKey="one" />);
    await act(async () => {});
    rerender(<MediaPanel loadPage={loadPage} onOpen={vi.fn()} refreshKey="two" />);
    await act(async () => { await vi.advanceTimersByTimeAsync(750); });
    expect(container.querySelector("[data-media-tab]")).toHaveAttribute("aria-busy", "true");
    await act(async () => fireEvent.click(container.querySelector<HTMLButtonElement>("#media-load-more-btn")!));
    await act(async () => { refresh.resolve({ items: [{ ...pending, attachment: { ...pending.attachment, state: "ready" as const, previewUrl: "updated" } }], nextCursor: "refreshed-next" }); });
    expect(container.querySelectorAll("[data-media-cell]")).toHaveLength(2);
    expect(container.querySelector("[data-media-tab]")).not.toHaveAttribute("aria-busy");
    expect(container.querySelector("#media-load-more-btn")).not.toBeInTheDocument();
  });
});
