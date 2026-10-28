import "@testing-library/jest-dom/vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AttachmentView } from "@peppy/desktop-ui/bridge-contracts";
import { MediaActionsProvider } from "./MediaActionsContext";
import { MediaTile } from "./MediaTile";
import type { GalleryAttachment, GalleryMessage } from "./grouping";
import { MAX_BUSY_ATTEMPTS, type HydratePreview } from "./previewHydration";

const messageOf = (id: string): GalleryMessage => ({ id, revision: "1", sender: "other", body: "", timestamp: "2026-01-01T12:00:00.000Z", attachments: [] });
const message = messageOf("message-13");
const localThumbnail = (overrides: Partial<GalleryAttachment> = {}): GalleryAttachment => ({
  id: "image-13",
  name: "Photo 13",
  mediaType: "image/jpeg",
  byteSize: 1,
  state: "ready",
  mediaMeta: { thumbnail: { id: "thumb-13", name: "thumb", mediaType: "image/jpeg", byteSize: 1, state: "ready" } },
  ...overrides,
});
/** The native contract: the ORIGINAL attachment view, with its local thumbnail nested under mediaMeta. */
const nativeOriginal = (thumbnailId: string, thumbnailUrl?: string, attachmentId = "image-13"): AttachmentView => ({
  id: attachmentId,
  name: "Photo 13",
  mediaType: "image/jpeg",
  byteSize: 1,
  state: "ready",
  mediaMeta: { thumbnail: { id: thumbnailId, name: "thumb", mediaType: "image/jpeg", byteSize: 1, state: "ready", previewUrl: thumbnailUrl } },
});
type ProviderOverrides = { hydrateMediaPreview?: HydratePreview; copyNativeImage?: (id: string) => Promise<void>; requestMediaSync?: () => Promise<void> };
const providerProps = (overrides: ProviderOverrides = {}) => ({
  publicCopies: {},
  onSave: vi.fn().mockResolvedValue("saved"),
  onPublish: vi.fn().mockResolvedValue(null),
  onRetry: vi.fn().mockResolvedValue(undefined),
  requestDownload: vi.fn().mockResolvedValue(undefined),
  loadOriginal: vi.fn().mockResolvedValue({ state: "pending" }),
  releaseMedia: vi.fn(),
  canCopyImage: false,
  hydrateMediaPreview: vi.fn<HydratePreview>().mockResolvedValue(nativeOriginal("thumb-13", "blob:thumb-13")),
  ...overrides,
});
type Props = ReturnType<typeof providerProps>;

/** Each tile registers one observer; tests drive visibility per tile element. */
const observers: Array<{ next: IntersectionObserverCallback; target: Element | null }> = [];
class Observer {
  private readonly entry: { next: IntersectionObserverCallback; target: Element | null };
  constructor(next: IntersectionObserverCallback) {
    this.entry = { next, target: null };
    observers.push(this.entry);
  }
  observe(target: Element) { this.entry.target = target; }
  disconnect() { this.entry.target = null; }
  unobserve() {}
  takeRecords() { return []; }
  readonly root = null;
  readonly rootMargin = "";
  readonly thresholds = [];
}
const setVisibility = (tile: Element, isIntersecting: boolean) => act(() => {
  for (const entry of observers) if (entry.target === tile) entry.next([{ isIntersecting, target: tile }] as unknown as IntersectionObserverEntry[], {} as IntersectionObserver);
});
const setAllVisible = (isIntersecting: boolean) => act(() => {
  for (const entry of observers) if (entry.target) entry.next([{ isIntersecting, target: entry.target }] as unknown as IntersectionObserverEntry[], {} as IntersectionObserver);
});
const tile = (value: GalleryAttachment, props: Props, owner: GalleryMessage = message) => <MediaActionsProvider {...props}><MediaTile attachment={value} message={{ ...owner, attachments: [value] }} variant="cell" onOpen={vi.fn()} /></MediaActionsProvider>;
const previewSrc = () => document.querySelector("[data-media-tile] img")?.getAttribute("src");
const previewSrcFor = (messageId: string) => document.querySelector(`[data-media-tile] [data-message-id="${messageId}"]`)?.closest("[data-media-tile]")?.querySelector("img")?.getAttribute("src");
const tileRoot = () => document.querySelector("[data-media-tile]")!;
const flushMicrotasks = () => act(async () => { await Promise.resolve(); });
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

afterEach(() => { cleanup(); observers.length = 0; vi.unstubAllGlobals(); });

describe("MediaTile preview hydration", () => {
  it("recovers an image 13 thumbnail with no URL once visible, and keeps it across later snapshots", async () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const props = providerProps();
    const { rerender } = render(tile(localThumbnail(), props));
    expect(props.hydrateMediaPreview).not.toHaveBeenCalled();
    setAllVisible(true);
    await waitFor(() => expect(previewSrc()).toBe("blob:thumb-13"));
    expect(props.hydrateMediaPreview).toHaveBeenCalledWith("message-13", "image-13");
    expect(props.requestDownload).not.toHaveBeenCalled();
    rerender(tile(localThumbnail(), props));
    expect(previewSrc()).toBe("blob:thumb-13");
    expect(props.hydrateMediaPreview).toHaveBeenCalledTimes(1);
  });

  it("makes no native calls for tiles that never become visible", () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const props = providerProps();
    render(tile(localThumbnail(), props));
    setAllVisible(false);
    expect(props.hydrateMediaPreview).not.toHaveBeenCalled();
    expect(previewSrc()).toBeUndefined();
  });

  it("shows Preview unavailable after a terminal miss, makes no further calls on unrelated refreshes, and retries only on explicit request", async () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const hydrateMediaPreview = vi.fn<HydratePreview>()
      .mockResolvedValueOnce(nativeOriginal("thumb-13"))
      .mockResolvedValue(nativeOriginal("thumb-13", "blob:recovered"));
    const props = providerProps({ hydrateMediaPreview });
    const { rerender } = render(tile(localThumbnail(), props));
    setAllVisible(true);
    await waitFor(() => expect(screen.getByText("Preview unavailable")).toBeInTheDocument());
    expect(hydrateMediaPreview).toHaveBeenCalledTimes(1);
    rerender(tile(localThumbnail({ name: "Renamed" }), { ...props }));
    await flushMicrotasks();
    expect(hydrateMediaPreview).toHaveBeenCalledTimes(1);
    fireEvent.contextMenu(document.querySelector(".tile-body")!, { clientX: 4, clientY: 4 });
    fireEvent.click(screen.getByRole("menuitem", { name: "Retry preview" }));
    await waitFor(() => expect(previewSrc()).toBe("blob:recovered"));
    expect(hydrateMediaPreview).toHaveBeenCalledTimes(2);
  });

  it("does not refetch a successful preview on a state refresh, but refetches when the thumbnail is replaced", async () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const replaced = localThumbnail({ mediaMeta: { thumbnail: { id: "thumb-13b", name: "thumb", mediaType: "image/jpeg", byteSize: 1, state: "ready" } } });
    const hydrateMediaPreview = vi.fn<HydratePreview>()
      .mockResolvedValueOnce(nativeOriginal("thumb-13", "blob:thumb-13"))
      .mockResolvedValue(nativeOriginal("thumb-13b", "blob:thumb-13b"));
    const props = providerProps({ hydrateMediaPreview });
    const { rerender } = render(tile(localThumbnail(), props));
    setAllVisible(true);
    await waitFor(() => expect(previewSrc()).toBe("blob:thumb-13"));
    rerender(tile(localThumbnail({ state: "pending", mediaHold: "requested" }), { ...props }));
    await flushMicrotasks();
    expect(hydrateMediaPreview).toHaveBeenCalledTimes(1);
    expect(previewSrc()).toBe("blob:thumb-13");
    rerender(tile(replaced, { ...props }));
    await waitFor(() => expect(previewSrc()).toBe("blob:thumb-13b"));
    expect(hydrateMediaPreview).toHaveBeenCalledTimes(2);
    expect(hydrateMediaPreview).toHaveBeenLastCalledWith("message-13", "image-13");
  });

  it("shows a hydrated thumbnail while the original is still held, and offers native Copy from it", async () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const copyNativeImage = vi.fn().mockResolvedValue(undefined);
    const props = providerProps({ copyNativeImage });
    const held = localThumbnail({ state: "pending", mediaHold: "held" });
    render(tile(held, props));
    setAllVisible(true);
    await waitFor(() => expect(document.querySelector(".tile-held-preview")).toHaveAttribute("src", "blob:thumb-13"));
    expect(props.requestDownload).not.toHaveBeenCalled();
    fireEvent.contextMenu(document.querySelector(".tile-held")!, { clientX: 4, clientY: 4 });
    expect(screen.getByRole("menuitem", { name: "Copy image" })).not.toHaveAttribute("aria-disabled");
    expect(screen.queryByRole("menuitem", { name: "Save image" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("menuitem", { name: "Copy image" }));
    await waitFor(() => expect(copyNativeImage).toHaveBeenCalledWith("image-13"));
    expect(props.loadOriginal).not.toHaveBeenCalled();
  });

  it("offers a retry of a failed original whose thumbnail is still good", async () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const props = providerProps();
    const failedOriginal = localThumbnail({ state: "failed", mediaMeta: { thumbnail: { id: "thumb-13", name: "thumb", mediaType: "image/jpeg", byteSize: 1, state: "ready", previewUrl: "blob:good" } } });
    render(tile(failedOriginal, props));
    fireEvent.contextMenu(screen.getByRole("button", { name: "Photo 13" }), { clientX: 4, clientY: 4 });
    fireEvent.click(screen.getByRole("menuitem", { name: "Retry download" }));
    expect(props.onRetry).toHaveBeenCalledWith("image-13");
  });

  it("cancels a queued hydration when its tile leaves the viewport, before native starts it", async () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const hanging = new Map<string, ReturnType<typeof deferred<AttachmentView>>>();
    const hydrateMediaPreview = vi.fn<HydratePreview>((_messageId, attachmentId) => {
      const gate = deferred<AttachmentView>();
      hanging.set(attachmentId, gate);
      return gate.promise;
    });
    const props = providerProps({ hydrateMediaPreview });
    render(<MediaActionsProvider {...props}>
      {["image-a", "image-b", "image-c"].map((id) => <MediaTile key={id} attachment={localThumbnail({ id, mediaMeta: { thumbnail: { id: `thumb-${id}`, name: "thumb", mediaType: "image/jpeg", byteSize: 1, state: "ready" } } })} message={{ ...message, attachments: [] }} variant="cell" onOpen={vi.fn()} />)}
    </MediaActionsProvider>);
    const [first, second, third] = Array.from(document.querySelectorAll("[data-media-tile]"));
    setVisibility(first, true);
    setVisibility(second, true);
    setVisibility(third, true);
    expect(hydrateMediaPreview.mock.calls.map(([, id]) => id)).toEqual(["image-a", "image-b"]);
    setVisibility(third, false);
    hanging.get("image-a")!.resolve(nativeOriginal("thumb-image-a", "blob:a", "image-a"));
    await flushMicrotasks();
    expect(hydrateMediaPreview.mock.calls.map(([, id]) => id)).toEqual(["image-a", "image-b"]);
  });

  it("keeps a reused original's previews separate per message, even with different thumbnails", async () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const hydrateMediaPreview = vi.fn<HydratePreview>(async (messageId) => messageId === "message-a" ? nativeOriginal("thumb-a", "blob:thumb-a", "shared-original") : nativeOriginal("thumb-b", "blob:thumb-b", "shared-original"));
    const props = providerProps({ hydrateMediaPreview });
    const reused = (thumbnailId: string) => localThumbnail({ id: "shared-original", mediaMeta: { thumbnail: { id: thumbnailId, name: "thumb", mediaType: "image/jpeg", byteSize: 1, state: "ready" } } });
    render(<MediaActionsProvider {...props}>
      <MediaTile attachment={reused("thumb-a")} message={{ ...messageOf("message-a"), attachments: [reused("thumb-a")] }} variant="cell" onOpen={vi.fn()} />
      <MediaTile attachment={reused("thumb-b")} message={{ ...messageOf("message-b"), attachments: [reused("thumb-b")] }} variant="cell" onOpen={vi.fn()} />
    </MediaActionsProvider>);
    setAllVisible(true);
    await waitFor(() => expect(previewSrcFor("message-a")).toBe("blob:thumb-a"));
    await waitFor(() => expect(previewSrcFor("message-b")).toBe("blob:thumb-b"));
    expect(hydrateMediaPreview.mock.calls).toEqual(expect.arrayContaining([["message-a", "shared-original"], ["message-b", "shared-original"]]));
    expect(previewSrcFor("message-a")).not.toBe("blob:thumb-b");
  });

  it("leaves browser hosts without native hydration on their existing loading state", () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const props = providerProps({ hydrateMediaPreview: undefined });
    render(tile(localThumbnail(), props));
    setAllVisible(true);
    expect(previewSrc()).toBeUndefined();
    expect(tileRoot()).toHaveAttribute("data-state", "loading");
    expect(screen.queryByText("Preview unavailable")).not.toBeInTheDocument();
  });

  it("settles a native thumbnail the snapshot never listed as Preview unavailable, with no spinner", async () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const hydrateMediaPreview = vi.fn<HydratePreview>().mockResolvedValue(nativeOriginal("thumb-foreign", "blob:foreign"));
    const props = providerProps({ hydrateMediaPreview });
    const noThumbnail = localThumbnail({ mediaMeta: undefined });
    render(tile(noThumbnail, props));
    setAllVisible(true);
    await waitFor(() => expect(screen.getByText("Preview unavailable")).toBeInTheDocument());
    expect(hydrateMediaPreview).toHaveBeenCalledTimes(1);
    expect(previewSrc()).toBeUndefined();
    expect(document.querySelector(".tile-loading-icon")).toBeNull();
    expect(tileRoot()).toHaveAttribute("data-state", "unavailable");
  });

  it("opens the held copy menu from the keyboard, names it, restores focus on Escape, and never opens the viewer", async () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const copyNativeImage = vi.fn().mockResolvedValue(undefined);
    const onOpen = vi.fn();
    const props = providerProps({ copyNativeImage });
    const held = localThumbnail({ state: "pending", mediaHold: "held" });
    render(<MediaActionsProvider {...props}><MediaTile attachment={held} message={{ ...message, attachments: [held] }} variant="cell" onOpen={onOpen} /></MediaActionsProvider>);
    setAllVisible(true);
    await waitFor(() => expect(document.querySelector(".tile-held-preview")).toHaveAttribute("src", "blob:thumb-13"));
    const heldBody = document.querySelector<HTMLElement>(".tile-held")!;
    expect(heldBody).toHaveAttribute("tabindex", "0");
    expect(heldBody).toHaveAttribute("aria-haspopup", "menu");
    expect(heldBody).toHaveAccessibleName("Photo 13, preview only");
    heldBody.focus();
    fireEvent.keyDown(heldBody, { key: "F10", shiftKey: true });
    const menu = screen.getByRole("menu", { name: "Image actions" });
    expect(screen.getByRole("menuitem", { name: "Copy image" })).toHaveFocus();
    expect(screen.queryByRole("menuitem", { name: "Save image" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("menuitem", { name: "Copy image" }));
    await waitFor(() => expect(copyNativeImage).toHaveBeenCalledWith("image-13"));
    expect(menu).not.toBeInTheDocument();
    await waitFor(() => expect(heldBody).toHaveFocus());
    expect(onOpen).not.toHaveBeenCalled();
  });

  it("opens the same held copy menu with the ContextMenu key and returns focus on Escape", async () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const props = providerProps({ copyNativeImage: vi.fn().mockResolvedValue(undefined) });
    const held = localThumbnail({ state: "pending", mediaHold: "held" });
    render(tile(held, props));
    setAllVisible(true);
    await waitFor(() => expect(document.querySelector(".tile-held-preview")).toHaveAttribute("src", "blob:thumb-13"));
    const heldBody = document.querySelector<HTMLElement>(".tile-held")!;
    heldBody.focus();
    fireEvent.keyDown(heldBody, { key: "ContextMenu" });
    expect(screen.getByRole("menu", { name: "Image actions" })).toBeInTheDocument();
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    await waitFor(() => expect(heldBody).toHaveFocus());
  });

  it("leaves a held tile without native Copy unfocusable and without a menu", async () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const props = providerProps();
    render(tile(localThumbnail({ state: "pending", mediaHold: "held" }), props));
    setAllVisible(true);
    await waitFor(() => expect(document.querySelector(".tile-held-preview")).toHaveAttribute("src", "blob:thumb-13"));
    const heldBody = document.querySelector<HTMLElement>(".tile-held")!;
    expect(heldBody).not.toHaveAttribute("tabindex");
    heldBody.focus();
    fireEvent.keyDown(heldBody, { key: "F10", shiftKey: true });
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("reaches Retry preview from the keyboard after hydration gave up", async () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const hydrateMediaPreview = vi.fn<HydratePreview>()
      .mockResolvedValueOnce(nativeOriginal("thumb-13"))
      .mockResolvedValue(nativeOriginal("thumb-13", "blob:recovered"));
    const props = providerProps({ hydrateMediaPreview });
    render(tile(localThumbnail(), props));
    setAllVisible(true);
    await waitFor(() => expect(screen.getByText("Preview unavailable")).toBeInTheDocument());
    const body = document.querySelector<HTMLElement>(".tile-body")!;
    body.focus();
    fireEvent.keyDown(body, { key: "F10", shiftKey: true });
    fireEvent.click(screen.getByRole("menuitem", { name: "Retry preview" }));
    await waitFor(() => expect(previewSrc()).toBe("blob:recovered"));
    expect(hydrateMediaPreview).toHaveBeenCalledTimes(2);
  });

  it("recovers automatically after a media-busy answer, with no user action", async () => {
    vi.stubGlobal("IntersectionObserver", Observer);
    const hydrateMediaPreview = vi.fn<HydratePreview>()
      .mockRejectedValueOnce({ code: "media-busy", message: "Media is busy. Try Copy again shortly." })
      .mockResolvedValue(nativeOriginal("thumb-13", "blob:thumb-13"));
    const props = providerProps({ hydrateMediaPreview });
    render(tile(localThumbnail(), props));
    setAllVisible(true);
    await waitFor(() => expect(previewSrc()).toBe("blob:thumb-13"), { timeout: 2000 });
    expect(hydrateMediaPreview).toHaveBeenCalledTimes(2);
    expect(screen.queryByText("Preview unavailable")).not.toBeInTheDocument();
  });

  it("cancels a pending busy retry timer when the tile unmounts", async () => {
    vi.useFakeTimers();
    try {
      vi.stubGlobal("IntersectionObserver", Observer);
      const hydrateMediaPreview = vi.fn<HydratePreview>().mockRejectedValue({ code: "media-busy" });
      const view = render(tile(localThumbnail(), providerProps({ hydrateMediaPreview })));
      setAllVisible(true);
      await flushAsync();
      expect(hydrateMediaPreview).toHaveBeenCalledTimes(1);
      expect(vi.getTimerCount()).toBeGreaterThan(0);
      view.unmount();
      act(() => { vi.advanceTimersByTime(10_000); });
      await flushAsync();
      expect(hydrateMediaPreview).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("bounds repeated busy answers to the attempt budget, then offers an explicit retry that resets it", async () => {
    vi.useFakeTimers();
    try {
      vi.stubGlobal("IntersectionObserver", Observer);
      const hydrateMediaPreview = vi.fn<HydratePreview>().mockRejectedValue({ code: "media-busy" });
      render(tile(localThumbnail(), providerProps({ hydrateMediaPreview })));
      setAllVisible(true);
      for (let elapsed = 0; elapsed < 30_000; elapsed += 250) {
        await flushAsync();
        act(() => { vi.advanceTimersByTime(250); });
      }
      await flushAsync();
      expect(hydrateMediaPreview).toHaveBeenCalledTimes(MAX_BUSY_ATTEMPTS);
      expect(screen.getByText("Preview unavailable")).toBeInTheDocument();
      act(() => { vi.advanceTimersByTime(60_000); });
      await flushAsync();
      expect(hydrateMediaPreview).toHaveBeenCalledTimes(MAX_BUSY_ATTEMPTS);
      fireEvent.contextMenu(document.querySelector(".tile-body")!, { clientX: 4, clientY: 4 });
      fireEvent.click(screen.getByRole("menuitem", { name: "Retry preview" }));
      await flushAsync();
      expect(hydrateMediaPreview).toHaveBeenCalledTimes(MAX_BUSY_ATTEMPTS + 1);
    } finally {
      vi.useRealTimers();
    }
  });
});

/** Lets the scheduler and hook promise chains settle under fake timers. */
const flushAsync = () => act(async () => {
  for (let turn = 0; turn < 20; turn += 1) await Promise.resolve();
});
