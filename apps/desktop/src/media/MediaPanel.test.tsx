import "@testing-library/jest-dom/vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { MediaPanel } from "./MediaPanel";

const item = (id: string) => ({ attachment: { id, name: id, mediaType: "image/jpeg", byteSize: 1, state: "ready" as const, thumbnailUrl: "thumb" }, message: { id: `message-${id}`, revision: "1", sender: "self" as const, body: "", timestamp: "2026-01-01T00:00:00.000Z", attachments: [] } });

describe("MediaPanel", () => {
  it("loads newest-first pages and retries failures", async () => {
    const loadPage = vi.fn().mockResolvedValueOnce({ items: [item("one")], nextCursor: "next" }).mockResolvedValueOnce({ items: [item("two")], nextCursor: undefined });
    render(<MediaPanel loadPage={loadPage} onOpen={vi.fn()} />);
    expect(await screen.findByRole("button", { name: "one" })).toBeInTheDocument();
    await act(async () => fireEvent.click(screen.getByRole("button", { name: "Load more" })));
    expect(await screen.findByRole("button", { name: "two" })).toBeInTheDocument();
  });
});
