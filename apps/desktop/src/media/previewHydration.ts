import type { AttachmentView } from "@peppy/desktop-ui/bridge-contracts";
import type { GalleryAttachment } from "./grouping";
import { usablePreview } from "./mediaState";

/** Native preview decoding runs at most this many at a time; other visible tiles wait their turn. */
export const NATIVE_HYDRATION_CONCURRENCY = 2;
/** `media-busy` is normal contention: retry while visible, with growing delays, then expose Retry. */
export const BUSY_RETRY_BASE_MS = 250;
export const BUSY_RETRY_MAX_MS = 2000;
export const MAX_BUSY_ATTEMPTS = 6;

export type HydratePreview = (messageId: string, attachmentId: string) => Promise<AttachmentView>;
/**
 * What a hydration produced. `settled` carries the native answer, which may still lack a usable preview;
 * `busy` is the only retryable failure; `terminal` is any other rejection (locked, not found, invalid);
 * `cancelled` means every waiter left before native started the request.
 */
export type HydrationOutcome =
  | { status: "settled"; view: AttachmentView }
  | { status: "busy" }
  | { status: "terminal" }
  | { status: "cancelled" };
export type PreviewHydrator = (messageId: string, attachmentId: string, signal?: AbortSignal) => Promise<HydrationOutcome>;
/**
 * A preview delivered by native for one message's original attachment. The message is part of identity:
 * an original ID can be reused by several messages, each with its own thumbnail. `sourceKey` records the
 * thumbnail source the preview was built for, so refreshes that keep that source never trigger another call.
 */
export type HydratedPreview = { messageId: string; attachmentId: string; sourceId: string; sourceKey: string; previewUrl: string };
export type HydrationRequest = { messageId: string; attachmentId: string; sourceKey: string };

/** Delay before the next attempt after `priorBusyCount` busy answers: 250ms, 500ms, 1s, then capped at 2s. */
export function busyRetryDelayMs(priorBusyCount: number): number {
  return Math.min(BUSY_RETRY_BASE_MS * 2 ** priorBusyCount, BUSY_RETRY_MAX_MS);
}

export function isMediaBusyError(error: unknown): boolean {
  return typeof error === "object" && error !== null && "code" in error && error.code === "media-busy";
}

type HydrationJob = {
  key: string;
  messageId: string;
  attachmentId: string;
  waiters: number;
  started: boolean;
  promise: Promise<HydrationOutcome>;
  resolve(outcome: HydrationOutcome): void;
};

async function outcomeOf(hydrate: HydratePreview, messageId: string, attachmentId: string): Promise<HydrationOutcome> {
  try {
    return { status: "settled", view: await hydrate(messageId, attachmentId) };
  } catch (error) {
    return isMediaBusyError(error) ? { status: "busy" } : { status: "terminal" };
  }
}

/**
 * Deduplicates hydrations per message and attachment and runs at most `concurrency` at once.
 * Queued work whose every waiter has aborted is removed before it reaches native, so scrolled-past tiles
 * never decode ahead of visible ones. Running work cannot be cancelled natively; its result is simply unused.
 */
export function createPreviewHydrator(hydrate: HydratePreview, concurrency = NATIVE_HYDRATION_CONCURRENCY): PreviewHydrator {
  const jobs = new Map<string, HydrationJob>();
  const queue: HydrationJob[] = [];
  let running = 0;

  const forget = (job: HydrationJob) => {
    if (jobs.get(job.key) === job) jobs.delete(job.key);
  };

  const drain = () => {
    while (running < concurrency && queue.length > 0) {
      const job = queue.shift()!;
      job.started = true;
      running += 1;
      void outcomeOf(hydrate, job.messageId, job.attachmentId).then((outcome) => {
        // Release the slot and the dedupe entry before resolving, so a caller reacting to the result starts fresh work.
        running -= 1;
        forget(job);
        drain();
        job.resolve(outcome);
      });
    }
  };

  const release = (job: HydrationJob) => {
    job.waiters -= 1;
    if (job.waiters > 0 || job.started) return;
    forget(job);
    const index = queue.indexOf(job);
    if (index >= 0) queue.splice(index, 1);
    job.resolve({ status: "cancelled" });
  };

  const createJob = (key: string, messageId: string, attachmentId: string): HydrationJob => {
    let resolve!: (outcome: HydrationOutcome) => void;
    const promise = new Promise<HydrationOutcome>((done) => { resolve = done; });
    return { key, messageId, attachmentId, waiters: 0, started: false, promise, resolve };
  };

  return (messageId, attachmentId, signal) => {
    const key = `${messageId}\u0000${attachmentId}`;
    let job = jobs.get(key);
    if (!job) {
      job = createJob(key, messageId, attachmentId);
      jobs.set(key, job);
      queue.push(job);
    }
    const shared = job;
    shared.waiters += 1;
    if (signal?.aborted) release(shared);
    else signal?.addEventListener("abort", () => release(shared), { once: true });
    drain();
    return shared.promise;
  };
}

/** Local image data exists but no preview URL is attached yet. */
export function needsPreviewHydration(attachment: GalleryAttachment): boolean {
  if (usablePreview(attachment)) return false;
  if (attachment.mediaMeta?.thumbnail?.state === "ready") return true;
  return attachment.state === "ready" && attachment.mediaType.startsWith("image/");
}

/**
 * Identity of the local thumbnail source. Original hold or state changes do not change it, so a state refresh
 * keeps a successful preview; replacing or invalidating the thumbnail does.
 */
export function previewSourceKey(attachment: GalleryAttachment): string {
  const thumbnail = attachment.mediaMeta?.thumbnail;
  return [thumbnail?.id ?? "", thumbnail?.state ?? ""].join("\u0000");
}

/**
 * Native answers with the original view. Candidates are its nested ready thumbnail URL first, then the
 * original's own ready URL as top-level fallback. Candidates are unvalidated until `applicableHydration`.
 */
export function hydratedPreviewCandidates(request: HydrationRequest, view: AttachmentView): HydratedPreview[] {
  const candidates: HydratedPreview[] = [];
  const thumbnail = view.mediaMeta?.thumbnail;
  if (thumbnail?.state === "ready" && thumbnail.previewUrl) {
    candidates.push({ ...request, sourceId: thumbnail.id, previewUrl: thumbnail.previewUrl });
  }
  if (view.state === "ready" && view.previewUrl) {
    candidates.push({ ...request, sourceId: request.attachmentId, previewUrl: view.previewUrl });
  }
  return candidates;
}

/**
 * The first candidate that the CURRENT snapshot accepts. A native source the snapshot does not
 * carry (for example a thumbnail it never listed) is never applied; undefined means nothing usable.
 */
export function applicableHydration(attachment: GalleryAttachment, messageId: string, candidates: readonly HydratedPreview[]): HydratedPreview | undefined {
  return candidates.find((candidate) => withHydratedPreview(attachment, messageId, candidate) !== attachment);
}

/** True when a stored preview was built for this message, this original, and the current thumbnail source. */
export function isCurrentHydration(hydrated: HydratedPreview | undefined, request: HydrationRequest): boolean {
  return hydrated !== undefined && hydrated.messageId === request.messageId && hydrated.attachmentId === request.attachmentId && hydrated.sourceKey === request.sourceKey;
}

/** Fills only a missing URL, and only on the message and source that produced it. Snapshot URLs always win. */
export function withHydratedPreview(attachment: GalleryAttachment, messageId: string, hydrated: HydratedPreview | undefined): GalleryAttachment {
  if (!hydrated || hydrated.messageId !== messageId || hydrated.attachmentId !== attachment.id) return attachment;
  const thumbnail = attachment.mediaMeta?.thumbnail;
  if (thumbnail?.id === hydrated.sourceId) return withThumbnailPreview(attachment, thumbnail, hydrated.previewUrl);
  if (attachment.id === hydrated.sourceId && attachment.state === "ready" && !attachment.previewUrl) {
    return { ...attachment, previewUrl: hydrated.previewUrl };
  }
  return attachment;
}

function withThumbnailPreview(attachment: GalleryAttachment, thumbnail: AttachmentView, previewUrl: string): GalleryAttachment {
  if (thumbnail.state !== "ready" || thumbnail.previewUrl) return attachment;
  return { ...attachment, mediaMeta: { ...attachment.mediaMeta, thumbnail: { ...thumbnail, previewUrl } } };
}
