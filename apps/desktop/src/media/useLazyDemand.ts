import { useCallback, useEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import { useMediaActions } from "./MediaActionsContext";
import type { GalleryAttachment } from "./grouping";
import { applicableHydration, busyRetryDelayMs, hydratedPreviewCandidates, isCurrentHydration, MAX_BUSY_ATTEMPTS, needsPreviewHydration, previewSourceKey, type HydratedPreview } from "./previewHydration";

const VIEWPORT_ROOT_MARGIN = "300px";

export function useLazyDemand(ref: RefObject<Element | null>, ids: readonly (string | undefined)[]): void {
  const actions = useMediaActions();
  const idsKey = ids.join("\u0000");
  const requestedRef = useRef(new Set<string>());
  const requestDownloadRef = useRef(actions?.requestDownload);
  requestDownloadRef.current = actions?.requestDownload;
  const hasActions = actions !== undefined;

  useEffect(() => {
    const element = ref.current;
    const hasPendingId = ids.some((id) => id !== undefined && !requestedRef.current.has(id));
    if (!hasActions || !element || !hasPendingId) return;
    const request = () => {
      for (const id of ids) {
        if (id && !requestedRef.current.has(id)) {
          requestedRef.current.add(id);
          requestDownloadRef.current?.(id);
        }
      }
    };
    if (typeof IntersectionObserver === "undefined") {
      request();
      return;
    }
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        request();
        observer.disconnect();
      }
    }, { rootMargin: "300px" });
    observer.observe(element);
    return () => observer.disconnect();
  }, [hasActions, idsKey, ref]);
}

export type PreviewHydrationState = {
  /** A preview built for the current message, original, and thumbnail source. */
  preview?: HydratedPreview;
  /** Native gave up for this source (terminal miss, or busy beyond the budget); retry is explicit only. */
  exhausted: boolean;
  retry(): void;
};

/**
 * Asks native for a preview of a locally held image while its tile is near the viewport.
 * - Requests run for the original attachment ID only; no remote original is demanded here.
 * - A matching stored preview is never requested again, whatever global state refreshes happen.
 * - `media-busy` retries while visible with growing delays (250ms doubling, capped at 2s), at most
 *   MAX_BUSY_ATTEMPTS times; then the tile is exhausted and offers an explicit retry.
 * - Any other rejection, or a settled answer without an applicable preview, is terminal for this source.
 * - Leaving the viewport, changing the source, or unmounting cancels the request and any pending retry timer.
 */
export function usePreviewHydration(ref: RefObject<Element | null>, messageId: string, attachment: GalleryAttachment): PreviewHydrationState {
  const actions = useMediaActions();
  const hydratePreview = actions?.hydratePreview;
  const needed = hydratePreview !== undefined && needsPreviewHydration(attachment);
  const attachmentId = attachment.id;
  const sourceKey = previewSourceKey(attachment);
  const [visible, setVisible] = useState(false);
  const [hydrated, setHydrated] = useState<HydratedPreview>();
  const [missed, setMissed] = useState<{ messageId: string; attachmentId: string; sourceKey: string; attempt: number }>();
  const [attempt, setAttempt] = useState(0);
  const [retryTick, setRetryTick] = useState(0);
  // Busy answers are counted in a ref: counting must not re-run the effect that schedules the next attempt.
  const busy = useRef({ key: "", count: 0 });
  // Read at settle time: a native answer is validated against the snapshot the tile shows now.
  const latestAttachment = useRef(attachment);
  latestAttachment.current = attachment;
  const requestKey = `${messageId}\u0000${attachmentId}\u0000${sourceKey}\u0000${attempt}`;
  const hasPreview = isCurrentHydration(hydrated, { messageId, attachmentId, sourceKey });
  const exhausted = !hasPreview && missed !== undefined && missed.messageId === messageId && missed.attachmentId === attachmentId && missed.sourceKey === sourceKey && missed.attempt === attempt;

  useEffect(() => {
    const element = ref.current;
    if (!needed || !element) return;
    if (typeof IntersectionObserver === "undefined") {
      setVisible(true);
      return;
    }
    const observer = new IntersectionObserver((entries) => {
      const latest = entries.at(-1);
      if (latest) setVisible(latest.isIntersecting);
    }, { rootMargin: VIEWPORT_ROOT_MARGIN });
    observer.observe(element);
    return () => observer.disconnect();
  }, [needed, ref]);

  useEffect(() => {
    if (!needed || !visible || !hydratePreview || hasPreview || exhausted) return;
    const request = { messageId, attachmentId, sourceKey };
    const controller = new AbortController();
    let live = true;
    let retryTimer: number | undefined;
    void hydratePreview(messageId, attachmentId, controller.signal).then((outcome) => {
      if (!live || outcome.status === "cancelled") return;
      if (outcome.status === "busy") {
        const priorBusy = busy.current.key === requestKey ? busy.current.count : 0;
        busy.current = { key: requestKey, count: priorBusy + 1 };
        if (priorBusy + 1 >= MAX_BUSY_ATTEMPTS) setMissed({ ...request, attempt });
        else retryTimer = window.setTimeout(() => setRetryTick((tick) => tick + 1), busyRetryDelayMs(priorBusy));
        return;
      }
      const candidates = outcome.status === "settled" ? hydratedPreviewCandidates(request, outcome.view) : [];
      const preview = applicableHydration(latestAttachment.current, messageId, candidates);
      if (preview) setHydrated(preview);
      else setMissed({ ...request, attempt });
    });
    return () => {
      live = false;
      controller.abort();
      window.clearTimeout(retryTimer);
    };
  }, [attachmentId, attempt, exhausted, hasPreview, hydratePreview, messageId, needed, requestKey, retryTick, sourceKey, visible]);

  const retry = useCallback(() => setAttempt((count) => count + 1), []);
  return { preview: hasPreview ? hydrated : undefined, exhausted, retry };
}
