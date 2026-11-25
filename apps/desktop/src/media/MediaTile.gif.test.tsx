import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaActionsProvider, type MediaResult } from "./MediaActionsContext";
import { MediaTile } from "./MediaTile";
import type { GalleryAttachment, GalleryMessage } from "./grouping";
import { replaceWindowProperty, stubMatchMedia } from "../test-support/browserStubs";

const message: GalleryMessage = { id: "message", revision: "1", sender: "other", body: "", timestamp: "2026-01-01T12:00:00.000Z", attachments: [] };
const gif = (overrides: Partial<GalleryAttachment> = {}): GalleryAttachment => ({ id: "gif", name: "GIF", mediaType: "image/gif", byteSize: 1, state: "ready", previewUrl: "https://example.test/preview", ...overrides });
const props = () => ({
  publicCopies: {}, onSave: vi.fn().mockResolvedValue("saved" as const), onRetry: vi.fn().mockResolvedValue(undefined),
  requestDownload: vi.fn().mockResolvedValue(undefined), loadOriginal: vi.fn<(id: string) => Promise<MediaResult>>().mockResolvedValue({ state: "ready", url: "https://example.test/animated" }),
  releaseMedia: vi.fn(), canCopyImage: false,
});
function tiles(values: GalleryAttachment[], actions: ReturnType<typeof props>, variant: "single" | "slide" | "cell" = "single") {
  return <MediaActionsProvider {...actions}>{values.map((attachment) => <MediaTile key={attachment.id} attachment={attachment} message={message} variant={variant} onOpen={vi.fn()} />)}</MediaActionsProvider>;
}

describe("GIF autoplay", () => {
  afterEach(cleanup);

  it("reveals the animation after load and releases it on unmount", async () => {
    stubMatchMedia([]);
    replaceWindowProperty("IntersectionObserver", undefined);
    const actions = props();
    const view = render(tiles([gif()], actions));
    await waitFor(() => expect(view.container.querySelector("[data-gif-animated]")).toBeInTheDocument());
    const image = view.container.querySelector("[data-gif-animated]")!;
    expect(image).not.toHaveAttribute("data-revealed");
    fireEvent.load(image);
    expect(image).toHaveAttribute("data-revealed", "true");
    expect(view.container.querySelectorAll(".tile-body img")).toHaveLength(2);
    view.unmount();
    expect(actions.releaseMedia).toHaveBeenCalledWith("gif", "https://example.test/animated");
  });

  it("does not load under reduced motion", () => {
    stubMatchMedia(["(prefers-reduced-motion: reduce)"]);
    replaceWindowProperty("IntersectionObserver", undefined);
    const actions = props();
    render(tiles([gif()], actions));
    expect(actions.loadOriginal).not.toHaveBeenCalled();
  });

  it.each([
    ["held", gif({ mediaHold: "held" }), "single"],
    ["large", gif({ byteSize: 8 * 1024 * 1024 + 1 }), "single"],
    ["cell", gif(), "cell"],
    ["non-GIF", gif({ mediaType: "image/jpeg" }), "single"],
  ] as const)("does not load %s", (_name, attachment, variant) => {
    stubMatchMedia([]);
    replaceWindowProperty("IntersectionObserver", undefined);
    const actions = props();
    render(tiles([attachment], actions, variant));
    expect(actions.loadOriginal).not.toHaveBeenCalled();
  });

  it("releases when reduced motion turns on", async () => {
    let listener: (() => void) | undefined;
    const media = { matches: false, addEventListener: (_name: string, callback: () => void) => { listener = callback; }, removeEventListener: vi.fn() };
    replaceWindowProperty("matchMedia", () => media);
    replaceWindowProperty("IntersectionObserver", undefined);
    const actions = props();
    const view = render(tiles([gif()], actions));
    await waitFor(() => expect(view.container.querySelector("[data-gif-animated]")).toBeInTheDocument());
    media.matches = true;
    act(() => listener?.());
    expect(view.container.querySelector("[data-gif-animated]")).not.toBeInTheDocument();
    expect(actions.releaseMedia).toHaveBeenCalledWith("gif", "https://example.test/animated");
  });

  it("holds at most two animations", async () => {
    stubMatchMedia([]);
    replaceWindowProperty("IntersectionObserver", undefined);
    const actions = props();
    const view = render(tiles(Array.from({ length: 5 }, (_, index) => gif({ id: `gif-${index}` })), actions));
    await waitFor(() => expect(view.container.querySelectorAll("[data-gif-animated]")).toHaveLength(2));
    await act(async () => { await Promise.resolve(); });
    expect(actions.loadOriginal).toHaveBeenCalledTimes(2);
  });

  it("starts a waiting tile when a slot is freed", async () => {
    stubMatchMedia([]);
    replaceWindowProperty("IntersectionObserver", undefined);
    const actions = props();
    const values = Array.from({ length: 3 }, (_, index) => gif({ id: `gif-${index}` }));
    const view = render(tiles(values, actions));
    await waitFor(() => expect(view.container.querySelectorAll("[data-gif-animated]")).toHaveLength(2));
    view.rerender(tiles(values.slice(1), actions));
    await waitFor(() => expect(actions.loadOriginal).toHaveBeenCalledTimes(3));
    expect(actions.loadOriginal).toHaveBeenLastCalledWith("gif-2");
  });

  it("releases an original that resolves after unmount", async () => {
    stubMatchMedia([]);
    replaceWindowProperty("IntersectionObserver", undefined);
    let resolve!: (value: { state: "ready"; url: string }) => void;
    const actions = props();
    actions.loadOriginal.mockReturnValue(new Promise((done) => { resolve = done; }));
    const view = render(tiles([gif()], actions));
    await waitFor(() => expect(actions.loadOriginal).toHaveBeenCalled());
    view.unmount();
    await act(async () => resolve({ state: "ready", url: "https://example.test/late" }));
    expect(actions.releaseMedia).toHaveBeenCalledWith("gif", "https://example.test/late");
    actions.loadOriginal.mockResolvedValue({ state: "ready", url: "https://example.test/animated" });
    const next = render(tiles([gif({ id: "a" }), gif({ id: "b" })], actions));
    await waitFor(() => expect(next.container.querySelectorAll("[data-gif-animated]")).toHaveLength(2));
  });

  it("loads only while visible, using the latest observation, and disconnects on unmount", async () => {
    stubMatchMedia([]);
    let observer!: { callback: IntersectionObserverCallback; disconnect: ReturnType<typeof vi.fn>; observe: ReturnType<typeof vi.fn> };
    class Observer {
      callback: IntersectionObserverCallback;
      disconnect = vi.fn();
      observe = vi.fn();
      constructor(callback: IntersectionObserverCallback, options?: IntersectionObserverInit) {
        expect(options?.rootMargin).toBe("200px");
        this.callback = callback;
        observer = this;
      }
    }
    replaceWindowProperty("IntersectionObserver", Observer);
    const actions = props();
    const view = render(tiles([gif()], actions));
    expect(observer.observe).toHaveBeenCalled();
    await act(async () => { await Promise.resolve(); });
    expect(actions.loadOriginal).not.toHaveBeenCalled();
    const emit = (...values: boolean[]) => act(() => observer.callback(values.map((isIntersecting) => ({ isIntersecting }) as IntersectionObserverEntry), observer as unknown as IntersectionObserver));
    emit(true, false);
    expect(actions.loadOriginal).not.toHaveBeenCalled();
    emit(true);
    await waitFor(() => expect(view.container.querySelector("[data-gif-animated]")).toBeInTheDocument());
    emit(false);
    expect(view.container.querySelector("[data-gif-animated]")).not.toBeInTheDocument();
    expect(actions.releaseMedia).toHaveBeenCalledWith("gif", "https://example.test/animated");
    view.unmount();
    expect(observer.disconnect).toHaveBeenCalled();
  });

  it.each(["pending", "reject"] as const)("%s result frees a slot without retrying", async (outcome) => {
    stubMatchMedia([]);
    replaceWindowProperty("IntersectionObserver", undefined);
    let settle!: () => void;
    const actions = props();
    actions.loadOriginal.mockImplementationOnce(() => new Promise((resolve, reject) => {
      settle = () => outcome === "pending" ? resolve({ state: "pending" }) : reject(new Error("unavailable"));
    }));
    const view = render(tiles([gif({ id: "a" }), gif({ id: "b" }), gif({ id: "c" })], actions));
    await waitFor(() => expect(actions.loadOriginal).toHaveBeenCalledTimes(2));
    await act(async () => settle());
    await waitFor(() => expect(actions.loadOriginal).toHaveBeenCalledTimes(3));
    expect(actions.loadOriginal).toHaveBeenLastCalledWith("c");
    expect(view.container.querySelector('[data-media-attachment="a"] [data-gif-animated]')).not.toBeInTheDocument();
    await act(async () => { await Promise.resolve(); });
    expect(actions.loadOriginal).toHaveBeenCalledTimes(3);
  });

  it("releases an image that fails to decode and retries only after the attachment state changes", async () => {
    stubMatchMedia([]);
    replaceWindowProperty("IntersectionObserver", undefined);
    const actions = props();
    const view = render(tiles([gif()], actions));
    await waitFor(() => expect(view.container.querySelector("[data-gif-animated]")).toBeInTheDocument());
    fireEvent.error(view.container.querySelector("[data-gif-animated]")!);
    expect(view.container.querySelector("[data-gif-animated]")).not.toBeInTheDocument();
    expect(actions.releaseMedia).toHaveBeenCalledWith("gif", "https://example.test/animated");
    view.rerender(tiles([gif()], actions));
    await act(async () => { await Promise.resolve(); });
    expect(actions.loadOriginal).toHaveBeenCalledTimes(1);
    view.rerender(tiles([gif({ state: "pending" })], actions));
    view.rerender(tiles([gif()], actions));
    await waitFor(() => expect(actions.loadOriginal).toHaveBeenCalledTimes(2));
  });
});
