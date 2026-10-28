import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import { MEDIA_SYNC_RETRY_MS } from "./MediaTile";
import { MediaGrid } from "./MediaGrid";
import { MediaActionsProvider } from "./MediaActionsContext";
import { MessageText } from "../message-links";

const message = (id: string, body = "") => ({ id, revision: "1", sender: "self" as const, body, timestamp: `2026-01-01T12:0${id}:00.000Z`, attachments: [{ id: `${id}-image`, name: id, mediaType: "image/jpeg", byteSize: 1, state: "ready" as const, mediaMeta: { thumbnail: { id: `${id}-thumbnail`, name: `${id}-thumbnail`, mediaType: "image/jpeg", byteSize: 1, state: "ready" as const, previewUrl: "thumb" } } }] });

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe("MediaGrid", () => {
  it("renders every caption and keeps a single message-row hook per message", () => {
    const view = render(<MediaGrid messages={[message("1", "first"), message("2", "final")]} onOpen={vi.fn()} renderMessageMeta={(value) => value.id} />);
    expect(view.container.querySelectorAll("[data-message-meta]")).toHaveLength(2);
    expect(screen.getByText("first")).toBeInTheDocument();
    expect(screen.getByText("final")).toBeInTheDocument();
    expect(view.container.querySelectorAll('[data-message-row="1"]')).toHaveLength(1);
    expect(view.container.querySelector('[data-message-group="1"]')).toBeInTheDocument();
  });

  it("uses the supplied renderer for media captions", () => {
    render(<MediaGrid messages={[message("1", "Read https://peppy.pro/docs")]} onOpen={vi.fn()} renderCaption={(value) => <MessageText text={value.body} />} />);
    expect(screen.getByRole("link", { name: "https://peppy.pro/docs" })).toHaveAttribute("href", "https://peppy.pro/docs");
  });

  it("renders a single tile without carousel controls", () => {
    const view = render(<MediaGrid messages={[message("1")]} onOpen={vi.fn()} />);
    expect(view.container.querySelector("[data-media-carousel]")).toBeNull();
    expect(view.container.querySelector("[data-media-tile][data-variant='single']")).toBeInTheDocument();
  });

  it("uses per-message titles on single images and carousel slides", () => {
    const title = (value: { id: string }) => `Sent at ${value.id}`;
    const single = render(<MediaGrid messages={[message("1")]} onOpen={vi.fn()} messageTitle={title} />);
    expect(single.container.querySelector("article")).toHaveAttribute("title", "Sent at 1");
    single.unmount();

    const carousel = render(<MediaGrid messages={[message("1"), message("2")]} onOpen={vi.fn()} messageTitle={title} />);
    expect(carousel.container.querySelectorAll("[data-media-slide]")[0]).toHaveAttribute("title", "Sent at 1");
    expect(carousel.container.querySelectorAll("[data-media-slide]")[1]).toHaveAttribute("title", "Sent at 2");
    expect(carousel.container.querySelector("[data-media-carousel]")).not.toHaveAttribute("title");
  });

  it("renders multiple attachments as a carousel and moves slide focus with arrow keys", () => {
    const first = message("1");
    const second = message("2");
    const view = render(<MediaGrid messages={[first, second]} onOpen={vi.fn()} />);
    expect(view.container.querySelector("[data-media-carousel]")).toHaveAttribute("aria-label", "Images, 2 total");
    expect(screen.getByText("1 / 2")).toBeInTheDocument();
    const firstBody = screen.getByRole("button", { name: "Photo, 1 of 2" });
    const secondBody = screen.getByRole("button", { name: "Photo, 2 of 2" });
    firstBody.focus();
    fireEvent.keyDown(firstBody, { key: "ArrowRight" });
    expect(secondBody).toHaveFocus();
    expect(view.container.querySelectorAll("[data-message-meta]")).toHaveLength(2);
  });

  it("renders chevrons for a scrollable two-slide carousel", async () => {
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => { callback(0); return 0; });
    const view = render(<MediaGrid messages={[message("1"), message("2")]} onOpen={vi.fn()} />);
    const track = view.container.querySelector<HTMLDivElement>(".carousel-track")!;
    Object.defineProperties(track, { clientWidth: { configurable: true, value: 200 }, scrollWidth: { configurable: true, value: 406 }, scrollLeft: { configurable: true, value: 0 } });
    fireEvent.scroll(track);
    await waitFor(() => expect(screen.getByRole("button", { name: "Next images" })).toBeInTheDocument());
  });

  it("uses the first half-visible slide for the counter", () => {
    let notify!: IntersectionObserverCallback;
    vi.stubGlobal("IntersectionObserver", class {
      constructor(callback: IntersectionObserverCallback) { notify = callback; }
      observe() {}
      disconnect() {}
    });
    const view = render(<MediaGrid messages={[message("1"), message("2"), message("3")]} onOpen={vi.fn()} />);
    const slides = view.container.querySelectorAll("[data-media-slide]");
    act(() => notify([{ target: slides[1], isIntersecting: true, intersectionRatio: .5 } as IntersectionObserverEntry, { target: slides[2], isIntersecting: true, intersectionRatio: .5 } as IntersectionObserverEntry], {} as IntersectionObserver));
    expect(screen.getByText("2 / 3")).toBeInTheDocument();
  });

  it("skips failed slides during arrow-key navigation", () => {
    const failedAttachment = message("2").attachments[0];
    const failed = { ...message("2"), attachments: [{ ...failedAttachment, state: "failed" as const, mediaMeta: { thumbnail: { ...failedAttachment.mediaMeta.thumbnail, state: "failed" as const } } }] };
    render(<MediaGrid messages={[message("1"), failed, message("3")]} onOpen={vi.fn()} />);
    const firstBody = screen.getByRole("button", { name: "Photo, 1 of 3" });
    const thirdBody = screen.getByRole("button", { name: "Photo, 3 of 3" });
    firstBody.focus();
    fireEvent.keyDown(firstBody, { key: "ArrowRight" });
    expect(thirdBody).toHaveFocus();
  });

  it("requests every held image once from the single all-held group control, but uses per-tile controls for mixed groups", () => {
    const requestMediaSync = vi.fn().mockResolvedValue(undefined);
    const provider = (ui: ReactNode) => <MediaActionsProvider publicCopies={{}} onSave={vi.fn().mockResolvedValue("saved")} onPublish={vi.fn().mockResolvedValue(null)} onRetry={vi.fn().mockResolvedValue(undefined)} requestDownload={vi.fn().mockResolvedValue(undefined)} requestMediaSync={requestMediaSync} loadOriginal={vi.fn().mockResolvedValue({ state: "pending" })} releaseMedia={vi.fn()} canCopyImage={false}>{ui}</MediaActionsProvider>;
    const held = (id: string) => ({ ...message(id), attachments: [{ ...message(id).attachments[0], state: "pending" as const, mediaHold: "held" as const }] });
    const { rerender } = render(provider(<MediaGrid messages={[held("1"), held("2")]} onOpen={vi.fn()} />));
    expect(screen.getAllByRole("button", { name: /Sync 2 images/ })).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Sync 2 images using mobile data if needed" }));
    expect(requestMediaSync).toHaveBeenCalledWith(["1-image", "2-image"]);
    rerender(provider(<MediaGrid messages={[held("1"), message("2")]} onOpen={vi.fn()} />));
    expect(screen.queryByRole("button", { name: "Sync 2 images" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Sync photo, 1 of 2 using mobile data/ })).toBeInTheDocument();
  });

  it("keeps the group's accepted request queued until one requested hold returns to held", async () => {
    const requestMediaSync = vi.fn().mockResolvedValue(undefined);
    const provider = (ui: ReactNode) => <MediaActionsProvider publicCopies={{}} onSave={vi.fn().mockResolvedValue("saved")} onPublish={vi.fn().mockResolvedValue(null)} onRetry={vi.fn().mockResolvedValue(undefined)} requestDownload={vi.fn().mockResolvedValue(undefined)} requestMediaSync={requestMediaSync} loadOriginal={vi.fn().mockResolvedValue({ state: "pending" })} releaseMedia={vi.fn()} canCopyImage={false}>{ui}</MediaActionsProvider>;
    const hold = (id: string, mediaHold: "held" | "requested") => ({ ...message(id), attachments: [{ ...message(id).attachments[0], state: "pending" as const, mediaHold }] });
    const { rerender } = render(provider(<MediaGrid messages={[hold("1", "held"), hold("2", "held")]} onOpen={vi.fn()} />));
    const group = () => document.querySelector<HTMLButtonElement>("[data-group-sync]")!;
    fireEvent.click(screen.getByRole("button", { name: "Sync 2 images using mobile data if needed" }));
    await waitFor(() => expect(group()).toHaveAttribute("data-sync-state", "queued"));
    expect(group()).toHaveAttribute("aria-disabled", "true");
    fireEvent.click(group());
    expect(requestMediaSync).toHaveBeenCalledTimes(1);
    rerender(provider(<MediaGrid messages={[hold("1", "requested"), hold("2", "held")]} onOpen={vi.fn()} />));
    expect(group()).toHaveAttribute("data-sync-state", "queued");
    rerender(provider(<MediaGrid messages={[hold("1", "held"), hold("2", "held")]} onOpen={vi.fn()} />));
    await waitFor(() => expect(group()).toHaveAttribute("data-sync-state", "idle"));
    expect(group()).not.toHaveAttribute("aria-disabled");
    expect(document.querySelector("[data-group-hold-note]")).toHaveTextContent("Original syncs on Wi-Fi");
  });

  it("retries an all-requested group with every requested image after the cooldown, and a successful retry starts a new cooldown", async () => {
    vi.useFakeTimers();
    try {
      const requestMediaSync = vi.fn().mockResolvedValue(undefined);
      const provider = (ui: ReactNode) => <MediaActionsProvider publicCopies={{}} onSave={vi.fn().mockResolvedValue("saved")} onPublish={vi.fn().mockResolvedValue(null)} onRetry={vi.fn().mockResolvedValue(undefined)} requestDownload={vi.fn().mockResolvedValue(undefined)} requestMediaSync={requestMediaSync} loadOriginal={vi.fn().mockResolvedValue({ state: "pending" })} releaseMedia={vi.fn()} canCopyImage={false}>{ui}</MediaActionsProvider>;
      const requested = (id: string) => ({ ...message(id), attachments: [{ ...message(id).attachments[0], state: "pending" as const, mediaHold: "requested" as const }] });
      render(provider(<MediaGrid messages={[requested("1"), requested("2")]} onOpen={vi.fn()} />));
      const group = document.querySelector<HTMLButtonElement>("[data-group-sync]")!;
      expect(group.textContent).toBe("Request queued");
      act(() => { vi.advanceTimersByTime(MEDIA_SYNC_RETRY_MS - 1); });
      expect(group.textContent).toBe("Request queued");
      act(() => { vi.advanceTimersByTime(1); });
      expect(group.textContent).toBe("Request again");
      expect(requestMediaSync).not.toHaveBeenCalled();
      await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Request again 2 images using mobile data if needed" })); });
      expect(requestMediaSync).toHaveBeenCalledTimes(1);
      expect(requestMediaSync).toHaveBeenCalledWith(["1-image", "2-image"]);
      expect(group.textContent).toBe("Request queued");
      act(() => { vi.advanceTimersByTime(MEDIA_SYNC_RETRY_MS - 1); });
      expect(group.textContent).toBe("Request queued");
      act(() => { vi.advanceTimersByTime(1); });
      expect(group.textContent).toBe("Request again");
    } finally {
      vi.useRealTimers();
    }
  });

  it("hides a partly visible held slide's control and note until the slide is fully visible, without moving focus", () => {
    const observers: Array<{ callback: IntersectionObserverCallback; targets: Element[] }> = [];
    vi.stubGlobal("IntersectionObserver", class {
      targets: Element[] = [];
      constructor(readonly callback: IntersectionObserverCallback) { observers.push(this); }
      observe(target: Element) { this.targets.push(target); }
      unobserve() {}
      disconnect() {}
      takeRecords() { return []; }
    });
    const held = { ...message("1"), attachments: [{ ...message("1").attachments[0], state: "pending" as const, mediaHold: "held" as const }] };
    const { container } = render(<MediaActionsProvider publicCopies={{}} onSave={vi.fn().mockResolvedValue("saved")} onPublish={vi.fn().mockResolvedValue(null)} onRetry={vi.fn().mockResolvedValue(undefined)} requestDownload={vi.fn().mockResolvedValue(undefined)} requestMediaSync={vi.fn().mockResolvedValue(undefined)} loadOriginal={vi.fn().mockResolvedValue({ state: "pending" })} releaseMedia={vi.fn()} canCopyImage={false}><MediaGrid messages={[held, message("2")]} onOpen={vi.fn()} /></MediaActionsProvider>);
    const tile = container.querySelector<HTMLElement>('[data-media-tile][data-original="held"]')!;
    const observer = observers.find((candidate) => candidate.targets.includes(tile))!;
    const sync = screen.getByRole("button", { name: /Sync photo, 1 of 2 using mobile data/ });
    sync.focus();
    act(() => observer.callback([{ target: tile, isIntersecting: true, intersectionRatio: 0.5 } as unknown as IntersectionObserverEntry], observer as unknown as IntersectionObserver));
    expect(tile).toHaveAttribute("data-partly-visible");
    expect(sync).toHaveFocus();
    act(() => observer.callback([{ target: tile, isIntersecting: true, intersectionRatio: 1 } as unknown as IntersectionObserverEntry], observer as unknown as IntersectionObserver));
    expect(tile).not.toHaveAttribute("data-partly-visible");
    expect(sync).toBeInTheDocument();
  });
});
