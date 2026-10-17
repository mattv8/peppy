import { useEffect, useRef, useState } from "react";
import { formatDateTime, Modal } from "@peppy/desktop-ui";
import type { MediaPageItem } from "./MediaPanel";

type MediaResult = { state: "ready"; url: string } | { state: "pending" };
export type MediaLightboxProps = {
  items: MediaPageItem[];
  initialAttachmentId: string;
  initialMessageId?: string;
  onDismiss(): void;
  requestOriginal(id: string): Promise<MediaResult>;
  checkOriginal(id: string): Promise<MediaResult>;
  saveMedia(id: string): Promise<void>;
  releaseMedia?(attachmentId: string, url: string): void;
  loadAdjacentPage?(direction: "previous" | "next"): Promise<{ items: MediaPageItem[]; hasNextPage: boolean; cursor?: unknown } | undefined>;
  hasNextPage?: boolean;
  hasPreviousPage?: boolean;
  retryKey?: unknown;
};
function mediaState(item: MediaPageItem, url?: string, pending?: boolean): string {
  if (url) return item.attachment.mediaType.startsWith("video/") ? "ready-video" : item.attachment.mediaType.startsWith("audio/") ? "ready-audio" : "ready-image";
  if (item.attachment.state === "failed") return "failed";
  if (item.attachment.mediaMeta?.previewUnavailable === "thumbnail_unavailable" && !hasUsablePreview(item.attachment)) return "preview-unavailable";
  if (!item.attachment.mediaType.startsWith("image/") && !item.attachment.mediaType.startsWith("video/") && !item.attachment.mediaType.startsWith("audio/")) return "unsupported";
  return pending || item.attachment.transfer === "download" ? "pending" : "loading";
}
function hasUsablePreview(attachment: MediaPageItem["attachment"]): boolean {
  const thumbnail = attachment.mediaMeta?.thumbnail;
  return Boolean((thumbnail?.state === "ready" && thumbnail.previewUrl) || (attachment.state === "ready" && attachment.previewUrl));
}
function isMediaPending(error: unknown): boolean {
  return typeof error === "object" && error !== null && "code" in error && (error as { code?: unknown }).code === "media-pending";
}
export function MediaLightbox({ items: initialItems, initialAttachmentId, initialMessageId, onDismiss, requestOriginal, checkOriginal, saveMedia, releaseMedia, loadAdjacentPage, hasNextPage = false, hasPreviousPage = false, retryKey }: MediaLightboxProps) {
  const [items, setItems] = useState(initialItems);
  const [index, setIndex] = useState(() => Math.max(0, initialItems.findIndex(item => item.attachment.id === initialAttachmentId && (!initialMessageId || item.message.id === initialMessageId))));
  const [url, setUrl] = useState<string>(); const [pending, setPending] = useState(false); const [error, setError] = useState<string>(); const [saving, setSaving] = useState(false); const [savePending, setSavePending] = useState(false); const [paging, setPaging] = useState(false); const [previousExhausted, setPreviousExhausted] = useState(!hasPreviousPage); const [nextExhausted, setNextExhausted] = useState(!hasNextPage);
  const requestVersion = useRef(0); const requested = useRef(false); const itemsRef = useRef(items); itemsRef.current = items;
  const item = items[index];
  const request = async () => {
    if (!item) return;
    const target = item.attachment.id; const version = ++requestVersion.current; requested.current = true; setError(undefined);
    try { const result = await requestOriginal(target); if (version !== requestVersion.current) { if (result.state === "ready") releaseMedia?.(target, result.url); } else if (result.state === "ready") { setPending(false); setUrl(result.url); } else setPending(true); } catch { if (version === requestVersion.current) setError("Could not load media."); }
  };
  const check = async () => {
    if (!item) return;
    const target = item.attachment.id; const version = ++requestVersion.current;
    try { const result = await checkOriginal(target); if (version !== requestVersion.current) { if (result.state === "ready") releaseMedia?.(target, result.url); } else if (result.state === "ready") { setPending(false); setUrl(result.url); } } catch { if (version === requestVersion.current) setError("Could not load media."); }
  };
  useEffect(() => () => { requestVersion.current += 1; if (url && item) releaseMedia?.(item.attachment.id, url); }, [item?.attachment.id, releaseMedia, url]);
  useEffect(() => { requestVersion.current += 1; setUrl(undefined); setPending(false); setSavePending(false); setError(undefined); requested.current = false; }, [item?.attachment.id]);
  useEffect(() => { if (item && !mediaState(item).startsWith("unsupported")) void request(); }, [item?.attachment.id]);
  useEffect(() => { if (pending && requested.current) void check(); }, [retryKey]);
  const navigate = async (offset: -1 | 1) => {
    const nextIndex = index + offset;
    if (nextIndex >= 0 && nextIndex < itemsRef.current.length) return setIndex(nextIndex);
    if (!loadAdjacentPage || paging) return;
    setPaging(true); setError(undefined);
    try {
      const direction = offset < 0 ? "previous" : "next";
      const seenCursors = new Set<string>();
      for (let emptyPages = 0; emptyPages < 10; emptyPages += 1) {
        const page = await loadAdjacentPage(direction);
        if (!page) return;
        if (page.items.length) {
          if (offset < 0) { setItems(current => [...page.items, ...current]); setIndex(page.items.length - 1); setPreviousExhausted(!page.hasNextPage); } else { const oldLength = itemsRef.current.length; setItems(current => [...current, ...page.items]); setIndex(oldLength); setNextExhausted(!page.hasNextPage); }
          return;
        }
        if (!page.hasNextPage) { if (offset < 0) setPreviousExhausted(true); else setNextExhausted(true); return; }
        const cursor = JSON.stringify(page.cursor);
        if (!page.cursor || seenCursors.has(cursor)) { setError("Couldn't load more media."); return; }
        seenCursors.add(cursor);
      }
      setError("Couldn't load more media.");
    } catch { setError("Couldn't load more media."); } finally { setPaging(false); }
  };
  useEffect(() => { const onKeyDown = (event: KeyboardEvent) => { if (event.target instanceof HTMLMediaElement) return; if (event.key === "ArrowLeft") void navigate(-1); if (event.key === "ArrowRight") void navigate(1); }; document.addEventListener("keydown", onKeyDown); return () => document.removeEventListener("keydown", onKeyDown); });
  const save = async () => { if (!item) return; setSaving(true); setError(undefined); try { await saveMedia(item.attachment.id); setSavePending(false); } catch (saveError) { if (isMediaPending(saveError)) setSavePending(true); else setError("Could not save media."); } finally { setSaving(false); } };
  useEffect(() => { if (savePending && !saving) void save(); }, [retryKey]);
  if (!item) return null;
  const state = mediaState(item, url, pending);
  const positionLabel = !previousExhausted || !nextExhausted ? `Image ${index + 1} of at least ${items.length} loaded` : `Image ${index + 1} of ${items.length}`;
  return <Modal id="media-lightbox" title="Media viewer" onDismiss={onDismiss} className="media-lightbox" actions={null}><div data-lightbox><div data-lightbox-backdrop aria-hidden="true" onClick={onDismiss} /><div id="lightbox-toolbar" data-lightbox-toolbar><span id="lightbox-position-label" className="visually-hidden" aria-live="polite">{positionLabel}</span><button id="lightbox-save-btn" type="button" className="secondary-button" disabled={saving || savePending} aria-busy={saving || savePending || undefined} onClick={() => void save()}>{savePending ? "Downloading…" : saving ? "Saving…" : "Save"}</button><button id="lightbox-close-btn" type="button" className="secondary-button" aria-label="Close media viewer" onClick={onDismiss}>Close</button></div><div data-lightbox-stage><button id="lightbox-prev-btn" type="button" className="secondary-button" data-lightbox-nav="prev" aria-label="Newer image" title="Newer image" disabled={(index === 0 && (!loadAdjacentPage || previousExhausted)) || paging} aria-busy={paging || undefined} onClick={() => void navigate(-1)}>Newer</button><div data-lightbox-media data-lightbox-state={state}>{url && state === "ready-image" && <img src={url} alt={item.attachment.name || "Image"} />}{url && state === "ready-video" && <video controls preload="none" src={url} aria-label={item.attachment.name || "Video"} />}{url && state === "ready-audio" && <audio controls preload="none" src={url} aria-label={item.attachment.name || "Audio"} />}{!url && state !== "unsupported" && <button id="lightbox-demand-btn" type="button" className="primary-button" onClick={() => void request()}>{pending ? "Downloading…" : state === "failed" ? "Retry" : "Load media"}</button>}{state === "unsupported" && <p role="status">Preview unavailable. Save the original to view it in another app.</p>}</div><button id="lightbox-next-btn" type="button" className="secondary-button" data-lightbox-nav="next" aria-label="Older image" title="Older image" disabled={(index === items.length - 1 && (!loadAdjacentPage || nextExhausted)) || paging} aria-busy={paging || undefined} onClick={() => void navigate(1)}>Older</button></div>{error && <p role="alert">{error}</p>}<div id="lightbox-meta">{formatDateTime(item.message.timestamp)}</div></div></Modal>;
}
