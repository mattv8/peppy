import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaLightbox } from "./MediaLightbox";

const items = ["one", "two"].map((id) => ({ attachment: { id, name: id, mediaType: "image/jpeg", byteSize: 1, state: "ready" as const }, message: { id: `message-${id}`, revision: "1", sender: "self" as const, body: "", timestamp: "2026-01-01T00:00:00.000Z", attachments: [] } }));

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

describe("MediaLightbox", () => {
  it("navigates with keys and releases a loaded original on close", async () => {
    const releaseMedia = vi.fn();
    const requestOriginal = vi.fn().mockResolvedValue({ state: "ready" as const, url: "blob:one" });
    const view = render(<MediaLightbox items={items} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={requestOriginal} saveMedia={vi.fn()} releaseMedia={releaseMedia} />);
    await act(async () => {});
    expect(requestOriginal).toHaveBeenCalledWith("one");
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
    fireEvent.click(screen.getByRole("button", { name: "Next image" }));
    await act(async () => resolve({ state: "ready" as const, url: "blob:late" }));
    expect(releaseMedia).toHaveBeenCalledWith("one", "blob:late");
  });

  it("lets pending and failed media retry through the injected request operation", async () => {
    const requestOriginal = vi.fn().mockResolvedValue({ state: "pending" as const });
    const pending = [{ ...items[0], attachment: { ...items[0].attachment, transfer: "download" as const } }];
    const view = render(<MediaLightbox items={pending} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={requestOriginal} saveMedia={vi.fn()} />);
    await act(async () => {});
    expect(requestOriginal).toHaveBeenCalledWith("one");
    view.unmount();
    render(<MediaLightbox items={[{ ...items[0], attachment: { ...items[0].attachment, state: "failed" } }]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={requestOriginal} saveMedia={vi.fn()} />);
    await act(async () => {});
    fireEvent.click(screen.getByRole("button", { name: "Downloading…" }));
    expect(requestOriginal).toHaveBeenCalledTimes(3);
  });

  it("advances through multiple adjacent pages and disables next at exhaustion", async () => {
    const third = { ...items[0], attachment: { ...items[0].attachment, id: "three", name: "three" }, message: { ...items[0].message, id: "message-three" } };
    const loadAdjacentPage = vi.fn()
      .mockResolvedValueOnce({ items: [items[1]], hasNextPage: true })
      .mockResolvedValueOnce({ items: [third], hasNextPage: false });
    const requestOriginal = vi.fn().mockResolvedValue({ state: "pending" as const });
    render(<MediaLightbox items={[items[0]]} initialAttachmentId="one" onDismiss={vi.fn()} requestOriginal={requestOriginal} checkOriginal={requestOriginal} saveMedia={vi.fn()} loadAdjacentPage={loadAdjacentPage} hasNextPage />);
    fireEvent.click(screen.getByRole("button", { name: "Next image" }));
    await act(async () => {});
    fireEvent.click(screen.getByRole("button", { name: "Next image" }));
    await act(async () => {});
    expect(screen.getByText("Image 3 of 3")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Next image" })).toBeDisabled();
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
});
