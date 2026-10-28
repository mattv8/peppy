import type { AttachmentView } from "@peppy/desktop-ui/bridge-contracts";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { GalleryAttachment } from "./grouping";
import { applicableHydration, busyRetryDelayMs, createPreviewHydrator, hydratedPreviewCandidates, isCurrentHydration, MAX_BUSY_ATTEMPTS, needsPreviewHydration, NATIVE_HYDRATION_CONCURRENCY, previewSourceKey, withHydratedPreview, type HydratedPreview } from "./previewHydration";

/** The native contract: the ORIGINAL attachment view, with its best local thumbnail nested under mediaMeta. */
const nativeOriginal = (options: { thumbnailId?: string; thumbnailUrl?: string; attachmentId?: string; originalUrl?: string; state?: AttachmentView["state"] } = {}): AttachmentView => ({
  id: options.attachmentId ?? "image-1",
  name: "Photo",
  mediaType: "image/jpeg",
  byteSize: 1,
  state: options.state ?? "pending",
  previewUrl: options.originalUrl,
  mediaMeta: options.thumbnailId ? { thumbnail: { id: options.thumbnailId, name: "thumb", mediaType: "image/jpeg", byteSize: 1, state: "ready", previewUrl: options.thumbnailUrl } } : undefined,
});
const original = (overrides: Partial<GalleryAttachment> = {}): GalleryAttachment => ({ id: "image-1", name: "Photo", mediaType: "image/jpeg", byteSize: 1, state: "ready", ...overrides });
const thumbnailOf = (id: string, state: AttachmentView["state"] = "ready") => ({ id, name: "thumb", mediaType: "image/jpeg", byteSize: 1, state });
const withThumbnail = (id: string, state: AttachmentView["state"] = "ready", overrides: Partial<GalleryAttachment> = {}) => original({ mediaMeta: { thumbnail: thumbnailOf(id, state) }, ...overrides });
const request = { messageId: "message-1", attachmentId: "image-1", sourceKey: "thumb-1\u0000ready" };
const busyError = { code: "media-busy", message: "Media is busy. Try Copy again shortly." };

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

const flush = () => new Promise((done) => setTimeout(done, 0));

afterEach(() => vi.restoreAllMocks());

describe("createPreviewHydrator", () => {
  it("deduplicates in-flight requests for one message's attachment", async () => {
    const gate = deferred<AttachmentView>();
    const hydrate = vi.fn(() => gate.promise);
    const hydrator = createPreviewHydrator(hydrate);
    const first = hydrator("message-1", "image-1");
    const second = hydrator("message-1", "image-1");
    expect(second).toBe(first);
    expect(hydrate).toHaveBeenCalledTimes(1);
    gate.resolve(nativeOriginal({ thumbnailId: "thumb-1", thumbnailUrl: "blob:one" }));
    await expect(first).resolves.toMatchObject({ status: "settled", view: { id: "image-1" } });
  });

  it("does not dedupe a reused original ID across different messages", () => {
    const hydrate = vi.fn(() => new Promise<AttachmentView>(() => undefined));
    const hydrator = createPreviewHydrator(hydrate);
    hydrator("message-1", "shared-original");
    hydrator("message-2", "shared-original");
    expect(hydrate.mock.calls).toEqual([["message-1", "shared-original"], ["message-2", "shared-original"]]);
  });

  it("never runs more than the native concurrency limit and starts queued work as slots free", async () => {
    expect(NATIVE_HYDRATION_CONCURRENCY).toBe(2);
    const gates = new Map<string, ReturnType<typeof deferred<AttachmentView>>>();
    const hydrate = vi.fn((_messageId: string, id: string) => {
      const gate = deferred<AttachmentView>();
      gates.set(id, gate);
      return gate.promise;
    });
    const hydrator = createPreviewHydrator(hydrate);
    ["a", "b", "c", "d", "e"].forEach((id) => hydrator("message-1", id));
    expect(hydrate.mock.calls.map(([, id]) => id)).toEqual(["a", "b"]);
    gates.get("a")!.resolve(nativeOriginal({ attachmentId: "a" }));
    await flush();
    expect(hydrate.mock.calls.map(([, id]) => id)).toEqual(["a", "b", "c"]);
    gates.get("b")!.resolve(nativeOriginal({ attachmentId: "b" }));
    gates.get("c")!.resolve(nativeOriginal({ attachmentId: "c" }));
    await flush();
    expect(hydrate.mock.calls.map(([, id]) => id)).toEqual(["a", "b", "c", "d", "e"]);
  });

  it("classifies a media-busy rejection as busy, so the caller can retry it", async () => {
    const hydrator = createPreviewHydrator(vi.fn().mockRejectedValue(busyError), 1);
    await expect(hydrator("message-1", "image-1")).resolves.toEqual({ status: "busy" });
  });

  it("classifies any other rejection as terminal: locked, not-found and invalid answers are never retried", async () => {
    const hydrator = createPreviewHydrator(vi.fn().mockRejectedValue({ code: "not-found" }), 1);
    await expect(hydrator("message-1", "image-1")).resolves.toEqual({ status: "terminal" });
    const locked = createPreviewHydrator(vi.fn().mockRejectedValue(new Error("locked")), 1);
    await expect(locked("message-1", "image-1")).resolves.toEqual({ status: "terminal" });
  });

  it("frees its slot after a rejection so queued work still runs", async () => {
    const hydrate = vi.fn()
      .mockRejectedValueOnce(busyError)
      .mockResolvedValue(nativeOriginal({ thumbnailId: "thumb-1", thumbnailUrl: "blob:later" }));
    const hydrator = createPreviewHydrator(hydrate, 1);
    await expect(hydrator("message-1", "image-1")).resolves.toEqual({ status: "busy" });
    await expect(hydrator("message-1", "image-1")).resolves.toMatchObject({ status: "settled" });
    expect(hydrate).toHaveBeenCalledTimes(2);
  });

  it("drops a queued request whose only waiter aborts, so native never sees it", async () => {
    const gate = deferred<AttachmentView>();
    const hydrate = vi.fn((_messageId: string, id: string) => (id === "running" ? gate.promise : new Promise<AttachmentView>(() => undefined)));
    const hydrator = createPreviewHydrator(hydrate, 1);
    hydrator("message-1", "running");
    const controller = new AbortController();
    const queued = hydrator("message-1", "scrolled-past", controller.signal);
    controller.abort();
    await expect(queued).resolves.toEqual({ status: "cancelled" });
    gate.resolve(nativeOriginal());
    await flush();
    expect(hydrate.mock.calls.map(([, id]) => id)).toEqual(["running"]);
  });

  it("keeps a queued request alive while another waiter still wants it", async () => {
    const gate = deferred<AttachmentView>();
    const hydrate = vi.fn((_messageId: string, id: string) => (id === "running" ? gate.promise : Promise.resolve(nativeOriginal({ attachmentId: id, thumbnailId: "thumb", thumbnailUrl: "blob:kept" }))));
    const hydrator = createPreviewHydrator(hydrate, 1);
    hydrator("message-1", "running");
    const aborting = new AbortController();
    hydrator("message-1", "shared", aborting.signal);
    const staying = hydrator("message-1", "shared");
    aborting.abort();
    gate.resolve(nativeOriginal());
    await expect(staying).resolves.toMatchObject({ status: "settled", view: { id: "shared" } });
    expect(hydrate.mock.calls.map(([, id]) => id)).toEqual(["running", "shared"]);
  });

  it("never cancels a request native already started; its result is still returned to a live caller", async () => {
    const gate = deferred<AttachmentView>();
    const hydrate = vi.fn(() => gate.promise);
    const hydrator = createPreviewHydrator(hydrate);
    const controller = new AbortController();
    const started = hydrator("message-1", "image-1", controller.signal);
    controller.abort();
    gate.resolve(nativeOriginal({ thumbnailId: "thumb-1", thumbnailUrl: "blob:late" }));
    expect(hydrate).toHaveBeenCalledTimes(1);
    await expect(started).resolves.toMatchObject({ status: "settled", view: { id: "image-1" } });
  });
});

describe("busyRetryDelayMs", () => {
  it("doubles from 250ms and caps at 2s, within the attempt budget", () => {
    expect([0, 1, 2, 3, 4].map(busyRetryDelayMs)).toEqual([250, 500, 1000, 2000, 2000]);
    expect(MAX_BUSY_ATTEMPTS).toBe(6);
  });
});

describe("hydratedPreviewCandidates", () => {
  it("lists the nested ready thumbnail first, attributed to its ID, then the original's ready URL", () => {
    expect(hydratedPreviewCandidates(request, nativeOriginal({ thumbnailId: "thumb-1", thumbnailUrl: "blob:thumb", originalUrl: "blob:orig", state: "ready" }))).toEqual([
      { ...request, sourceId: "thumb-1", previewUrl: "blob:thumb" },
      { ...request, sourceId: "image-1", previewUrl: "blob:orig" },
    ]);
  });

  it("offers only the original's ready URL when the nested thumbnail is not ready", () => {
    const view = { ...nativeOriginal({ thumbnailId: "thumb-1", thumbnailUrl: "blob:thumb", state: "ready", originalUrl: "blob:orig" }), mediaMeta: { thumbnail: thumbnailOf("thumb-1", "pending") } };
    expect(hydratedPreviewCandidates(request, view)).toEqual([{ ...request, sourceId: "image-1", previewUrl: "blob:orig" }]);
  });

  it("offers nothing for a view with no usable preview", () => {
    expect(hydratedPreviewCandidates(request, nativeOriginal({ thumbnailId: "thumb-1" }))).toEqual([]);
  });
});

describe("applicableHydration", () => {
  it("accepts a nested thumbnail the snapshot carries", () => {
    const attachment = withThumbnail("thumb-1");
    const candidates = hydratedPreviewCandidates(request, nativeOriginal({ thumbnailId: "thumb-1", thumbnailUrl: "blob:thumb", state: "ready" }));
    expect(applicableHydration(attachment, "message-1", candidates)?.sourceId).toBe("thumb-1");
  });

  it("never applies a nested thumbnail the snapshot does not carry, and falls back to a ready original URL", () => {
    const attachment = original();
    const candidates = hydratedPreviewCandidates(request, nativeOriginal({ thumbnailId: "thumb-foreign", thumbnailUrl: "blob:foreign", originalUrl: "blob:orig", state: "ready" }));
    expect(applicableHydration(attachment, "message-1", candidates)).toEqual({ ...request, sourceId: "image-1", previewUrl: "blob:orig" });
  });

  it("returns nothing when no candidate is applicable, so the tile settles instead of waiting", () => {
    const attachment = original();
    const candidates = hydratedPreviewCandidates(request, nativeOriginal({ thumbnailId: "thumb-foreign", thumbnailUrl: "blob:foreign", state: "ready" }));
    expect(applicableHydration(attachment, "message-1", candidates)).toBeUndefined();
  });
});

describe("isCurrentHydration", () => {
  const stored: HydratedPreview = { ...request, sourceId: "thumb-1", previewUrl: "blob:thumb" };

  it("matches only the same message, original, and thumbnail source", () => {
    expect(isCurrentHydration(stored, request)).toBe(true);
    expect(isCurrentHydration(stored, { ...request, messageId: "message-2" })).toBe(false);
    expect(isCurrentHydration(stored, { ...request, sourceKey: "thumb-2\u0000ready" })).toBe(false);
    expect(isCurrentHydration(undefined, request)).toBe(false);
  });
});

describe("needsPreviewHydration", () => {
  it("is true for a local ready thumbnail or original image without a URL", () => {
    expect(needsPreviewHydration(withThumbnail("thumb-1", "ready"))).toBe(true);
    expect(needsPreviewHydration(original())).toBe(true);
  });

  it("is false when a preview already exists, the image is not local, or the media is not an image", () => {
    expect(needsPreviewHydration(withThumbnail("thumb-1", "ready", { mediaMeta: { thumbnail: { ...thumbnailOf("thumb-1"), previewUrl: "blob:thumb" } } }))).toBe(false);
    expect(needsPreviewHydration(original({ previewUrl: "blob:original" }))).toBe(false);
    expect(needsPreviewHydration(original({ state: "pending" }))).toBe(false);
    expect(needsPreviewHydration(original({ mediaType: "video/mp4" }))).toBe(false);
  });
});

describe("withHydratedPreview", () => {
  it("applies a thumbnail preview only to the source that produced it", () => {
    const attachment = withThumbnail("thumb-1");
    const merged = withHydratedPreview(attachment, "message-1", { ...request, sourceId: "thumb-1", previewUrl: "blob:thumb" });
    expect(merged.mediaMeta?.thumbnail?.previewUrl).toBe("blob:thumb");
    expect(merged.previewUrl).toBeUndefined();
  });

  it("ignores a hydrated preview whose thumbnail ID was replaced", () => {
    const attachment = withThumbnail("thumb-2");
    expect(withHydratedPreview(attachment, "message-1", { ...request, sourceId: "thumb-1", previewUrl: "blob:stale" })).toBe(attachment);
  });

  it("ignores a hydrated preview for another attachment", () => {
    const attachment = withThumbnail("thumb-1", "ready", { id: "image-2" });
    expect(withHydratedPreview(attachment, "message-1", { ...request, sourceId: "thumb-1", previewUrl: "blob:other" })).toBe(attachment);
  });

  it("never applies a reused original's preview to another message's tile", () => {
    const hydrated = { messageId: "message-1", attachmentId: "shared-original", sourceKey: "thumb-2\u0000ready", sourceId: "thumb-2", previewUrl: "blob:message-one" };
    const shared = withThumbnail("thumb-2", "ready", { id: "shared-original" });
    expect(withHydratedPreview(shared, "message-2", hydrated)).toBe(shared);
    expect(withHydratedPreview(shared, "message-1", hydrated).mediaMeta?.thumbnail?.previewUrl).toBe("blob:message-one");
  });

  it("fills the original as a top-level fallback only while it is ready", () => {
    const hydrated = { ...request, sourceId: "image-1", previewUrl: "blob:original" };
    expect(withHydratedPreview(original(), "message-1", hydrated).previewUrl).toBe("blob:original");
    const pending = original({ state: "pending" });
    expect(withHydratedPreview(pending, "message-1", hydrated)).toBe(pending);
  });

  it("never overrides a URL the snapshot already provides", () => {
    const attachment = withThumbnail("thumb-1", "ready", { mediaMeta: { thumbnail: { ...thumbnailOf("thumb-1"), previewUrl: "blob:snapshot" } } });
    expect(withHydratedPreview(attachment, "message-1", { ...request, sourceId: "thumb-1", previewUrl: "blob:hydrated" })).toBe(attachment);
  });
});

describe("previewSourceKey", () => {
  it("changes when the thumbnail identity or ready state changes", () => {
    const base = previewSourceKey(withThumbnail("thumb-1"));
    expect(previewSourceKey(withThumbnail("thumb-2"))).not.toBe(base);
    expect(previewSourceKey(withThumbnail("thumb-1", "failed"))).not.toBe(base);
  });

  it("ignores original hold and state changes, so refreshes keep a successful preview", () => {
    const base = previewSourceKey(withThumbnail("thumb-1"));
    expect(previewSourceKey(withThumbnail("thumb-1", "ready", { mediaHold: "requested", state: "pending" }))).toBe(base);
  });
});
