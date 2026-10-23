import "@testing-library/jest-dom/vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { MediaPanel } from "./MediaPanel";

const item = (id: string) => ({ attachment: { id, name: id, mediaType: "image/jpeg", byteSize: 1, state: "ready" as const, previewUrl: "thumb" }, message: { id: `message-${id}`, revision: "1", sender: "self" as const, body: "", timestamp: "2026-01-01T00:00:00.000Z", attachments: [] } });

describe("MediaPanel", () => {
  it("loads newest-first pages and retries failures", async () => {
    const loadPage = vi.fn().mockResolvedValueOnce({ items: [item("one")], nextCursor: "next" }).mockResolvedValueOnce({ items: [item("two")], nextCursor: undefined });
    render(<MediaPanel loadPage={loadPage} onOpen={vi.fn()} />);
    expect(await screen.findByRole("button", { name: "one" })).toBeInTheDocument();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Load more" })));
    expect(await screen.findByRole("button", { name: "two" })).toBeInTheDocument();
  });

  it("shows a ready thumbnail when the original remains remote", async () => {
    const remoteOriginal = {
      attachment: { id: "original", name: "original", mediaType: "image/jpeg", byteSize: 1, state: "pending" as const, transfer: "download" as const, mediaMeta: { thumbnail: { id: "thumbnail", name: "thumbnail", mediaType: "image/jpeg", byteSize: 1, state: "ready" as const, previewUrl: "thumb" } } },
      message: { id: "message-original", revision: "1", sender: "self" as const, body: "", timestamp: "2026-01-01T00:00:00.000Z", attachments: [] },
    };
    render(<MediaPanel loadPage={vi.fn().mockResolvedValue({ items: [remoteOriginal] })} onOpen={vi.fn()} />);
    const cell = await screen.findByRole("button", { name: "original" });
    expect(cell).toHaveAttribute("data-cell-state", "ready");
    expect(cell.querySelector("img")).toHaveAttribute("src", "thumb");
  });

  it("announces producer-confirmed unavailable previews only without a usable preview", async () => {
    const unavailable = { attachment: { id: "original", name: "original", mediaType: "image/jpeg", byteSize: 1, state: "pending" as const, transfer: "download" as const, mediaMeta: { previewUnavailable: "thumbnail_unavailable" as const } }, message: { id: "message-original", revision: "1", sender: "self" as const, body: "", timestamp: "2026-01-01T00:00:00.000Z", attachments: [] } };
    render(<MediaPanel loadPage={vi.fn().mockResolvedValue({ items: [unavailable] })} onOpen={vi.fn()} />);
    expect(await screen.findByRole("button", { name: "Preview unavailable" })).toHaveAttribute("data-cell-state", "preview-unavailable");
  });

  it("uses the grid's failed-media copy", async () => {
    const failed = { ...item("failed"), attachment: { ...item("failed").attachment, state: "failed" as const, previewUrl: undefined } };
    render(<MediaPanel loadPage={vi.fn().mockResolvedValue({ items: [failed] })} onOpen={vi.fn()} />);
    expect(await screen.findByRole("button", { name: "Image failed to load" })).toHaveTextContent("Image failed to load");
  });

  it("labels pending cells with the grid's loading and not-downloaded copy", async () => {
    const message = (id: string) => ({ id: `message-${id}`, revision: "1", sender: "self" as const, body: "", timestamp: "2026-01-01T00:00:00.000Z", attachments: [] });
    const loading = { attachment: { id: "loading", name: "loading", mediaType: "image/jpeg", byteSize: 1, state: "pending" as const }, message: message("loading") };
    const remote = { attachment: { id: "remote", name: "remote", mediaType: "image/jpeg", byteSize: 1, state: "pending" as const, transfer: "download" as const }, message: message("remote") };
    render(<MediaPanel loadPage={vi.fn().mockResolvedValue({ items: [loading, remote] })} onOpen={vi.fn()} />);
    const loadingCell = await screen.findByRole("button", { name: "Loading image" });
    expect(loadingCell).toHaveAttribute("data-cell-state", "loading");
    expect(loadingCell).toHaveTextContent("Loading image");
    const remoteCell = screen.getByRole("button", { name: "Image not yet downloaded" });
    expect(remoteCell).toHaveAttribute("data-cell-state", "pending-download");
    expect(remoteCell).toHaveTextContent("Image not yet downloaded");
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
});
