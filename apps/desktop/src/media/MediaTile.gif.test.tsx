import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MediaActionsProvider } from "./MediaActionsContext";
import { MediaTile } from "./MediaTile";
import type { GalleryAttachment, GalleryMessage } from "./grouping";
import { replaceWindowProperty, stubMatchMedia } from "../test-support/browserStubs";

const message: GalleryMessage = { id: "message", revision: "1", sender: "other", body: "", timestamp: "2026-01-01T12:00:00.000Z", attachments: [] };
const gif = (overrides: Partial<GalleryAttachment> = {}): GalleryAttachment => ({ id: "gif", name: "GIF", mediaType: "image/gif", byteSize: 1, state: "ready", previewUrl: "https://example.test/preview", ...overrides });
const props = () => ({
  publicCopies: {}, onSave: vi.fn().mockResolvedValue("saved" as const), onRetry: vi.fn().mockResolvedValue(undefined),
  requestDownload: vi.fn().mockResolvedValue(undefined), loadOriginal: vi.fn().mockResolvedValue({ state: "ready" as const, url: "https://example.test/animated" }),
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

  it("holds at most four animations", async () => {
    stubMatchMedia([]);
    replaceWindowProperty("IntersectionObserver", undefined);
    const actions = props();
    render(tiles(Array.from({ length: 5 }, (_, index) => gif({ id: `gif-${index}` })), actions));
    await waitFor(() => expect(actions.loadOriginal).toHaveBeenCalledTimes(4));
  });

  it("starts a waiting tile when a slot is freed", async () => {
    stubMatchMedia([]);
    replaceWindowProperty("IntersectionObserver", undefined);
    const actions = props();
    const values = Array.from({ length: 5 }, (_, index) => gif({ id: `gif-${index}` }));
    const view = render(tiles(values, actions));
    await waitFor(() => expect(view.container.querySelectorAll("[data-gif-animated]")).toHaveLength(4));
    view.rerender(tiles(values.slice(1), actions));
    await waitFor(() => expect(actions.loadOriginal).toHaveBeenCalledTimes(5));
    expect(actions.loadOriginal).toHaveBeenLastCalledWith("gif-4");
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
  });
});
