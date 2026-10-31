import { useEffect, useRef, useState } from "react";
import { attachmentStorageRemovalText, ChevronLeft, ChevronRight, Copy, Download, formatDateTime, ImageOff, Link, Loader2, Modal, RotateCw, X } from "@peppy/desktop-ui";
import type { MediaPageItem } from "./MediaPanel";
import { type MediaResult, useMediaActions } from "./MediaActionsContext";
import { originalPending, previewUrl, usablePreview } from "./mediaState";
import { peppyCopy } from "../generated/peppyCopy";
import "./media-lightbox.css";

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
  chrome?: "modal" | "window";
};

function mediaState(item: MediaPageItem, url?: string, pending?: boolean): string {
  if (url) return item.attachment.mediaType.startsWith("video/") ? "ready-video" : item.attachment.mediaType.startsWith("audio/") ? "ready-audio" : "ready-image";
  if (originalPending(item.attachment)) return "pending";
  if (item.attachment.state === "failed") return "failed";
  if (item.attachment.mediaMeta?.previewUnavailable === "thumbnail_unavailable" && !usablePreview(item.attachment)) return "preview-unavailable";
  if (!item.attachment.mediaType.startsWith("image/") && !item.attachment.mediaType.startsWith("video/") && !item.attachment.mediaType.startsWith("audio/")) return "unsupported";
  return pending || item.attachment.transfer === "download" ? "pending" : "loading";
}

function isMediaPending(error: unknown): boolean {
  return typeof error === "object" && error !== null && "code" in error && (error as { code?: unknown }).code === "media-pending";
}

export function MediaLightbox({ items: initialItems, initialAttachmentId, initialMessageId, onDismiss, requestOriginal, checkOriginal, saveMedia, releaseMedia, loadAdjacentPage, hasNextPage = false, hasPreviousPage = false, retryKey, chrome = "modal" }: MediaLightboxProps) {
  const openableInitialItems = initialItems.filter((item) => !originalPending(item.attachment) || (item.attachment.id === initialAttachmentId && (!initialMessageId || item.message.id === initialMessageId)));
  const [items, setItems] = useState(openableInitialItems);
  const [index, setIndex] = useState(() => Math.max(0, openableInitialItems.findIndex(item => item.attachment.id === initialAttachmentId && (!initialMessageId || item.message.id === initialMessageId))));
  const [url, setUrl] = useState<string>(); const [pendingAttachmentId, setPendingAttachmentId] = useState<string>(); const [loadError, setLoadError] = useState<string>(); const [actionError, setActionError] = useState<string>(); const [saving, setSaving] = useState(false); const [savePending, setSavePending] = useState(false); const [paging, setPaging] = useState(false); const [previousExhausted, setPreviousExhausted] = useState(!hasPreviousPage); const [nextExhausted, setNextExhausted] = useState(!hasNextPage);
  const requestVersion = useRef(0); const requested = useRef(false); const itemsRef = useRef(items); itemsRef.current = items;
  const actions = useMediaActions();
  const closeButtonRef = useRef<HTMLButtonElement>(null);
  const retryButtonRef = useRef<HTMLButtonElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const [stillWaiting, setStillWaiting] = useState(false);
  const item = items[index];
  const removedRemoteOnly = item?.attachment.remoteState === "removed_by_storage_policy" && !item.attachment.localAvailable;
  const removedWithLocalCopy = item?.attachment.remoteState === "removed_by_storage_policy" && item.attachment.localAvailable;
  const pending = pendingAttachmentId === item?.attachment.id;
  const request = async () => {
  if (!item || originalPending(item.attachment)) return;
    const target = item.attachment.id; const version = ++requestVersion.current; requested.current = true; setLoadError(undefined);
    try { const result = await requestOriginal(target); if (version !== requestVersion.current) { if (result.state === "ready") releaseMedia?.(target, result.url); } else if (result.state === "ready") { setPendingAttachmentId(undefined); setUrl(result.url); } else setPendingAttachmentId(target); } catch { if (version === requestVersion.current) setLoadError("Could not load media."); }
  };
  const check = async () => {
  if (!item) return;
    const target = item.attachment.id; const version = ++requestVersion.current;
    try { const result = await checkOriginal(target); if (version !== requestVersion.current) { if (result.state === "ready") releaseMedia?.(target, result.url); } else if (result.state === "ready") { setPendingAttachmentId(undefined); setUrl(result.url); } } catch { if (version === requestVersion.current) setLoadError("Could not load media."); }
  };
  useEffect(() => () => { requestVersion.current += 1; if (url && item) releaseMedia?.(item.attachment.id, url); }, [item?.attachment.id, releaseMedia, url]);
  useEffect(() => { requestVersion.current += 1; setUrl(undefined); setPendingAttachmentId(undefined); setSavePending(false); setLoadError(undefined); setActionError(undefined); requested.current = false; }, [item?.attachment.id]);
  useEffect(() => { if (item && !removedRemoteOnly && !mediaState(item).startsWith("unsupported")) void request(); }, [item?.attachment.id, removedRemoteOnly]);
  useEffect(() => { if (item && !removedRemoteOnly && !url && (pending && requested.current || originalPending(item.attachment))) void check(); }, [removedRemoteOnly, retryKey]);
  useEffect(() => { if (!pending) { setStillWaiting(false); return; } const timer = window.setTimeout(() => setStillWaiting(true), 15000); return () => window.clearTimeout(timer); }, [item?.attachment.id, pending]);
  const navigate = async (offset: -1 | 1) => {
    const nextIndex = index + offset;
    if (nextIndex >= 0 && nextIndex < itemsRef.current.length) return setIndex(nextIndex);
    if (!loadAdjacentPage || paging) return;
    setPaging(true); setActionError(undefined);
    try {
      const direction = offset < 0 ? "previous" : "next"; const seenCursors = new Set<string>();
      for (let emptyPages = 0; emptyPages < 10; emptyPages += 1) {
        const page = await loadAdjacentPage(direction);
        if (!page) return;
        const openableItems = page.items.filter((candidate) => !originalPending(candidate.attachment));
        if (openableItems.length) {
          if (offset < 0) { setItems(current => [...openableItems, ...current]); setIndex(openableItems.length - 1); setPreviousExhausted(!page.hasNextPage); } else { const oldLength = itemsRef.current.length; setItems(current => [...current, ...openableItems]); setIndex(oldLength); setNextExhausted(!page.hasNextPage); }
          return;
        }
        if (!page.hasNextPage) { if (offset < 0) setPreviousExhausted(true); else setNextExhausted(true); return; }
        const cursor = JSON.stringify(page.cursor);
        if (!page.cursor || seenCursors.has(cursor)) { setActionError("Couldn't load more media."); return; }
        seenCursors.add(cursor);
      }
      setActionError("Couldn't load more media.");
    } catch { setActionError("Couldn't load more media."); } finally { setPaging(false); }
  };
  useEffect(() => { const onKeyDown = (event: KeyboardEvent) => { if (event.target instanceof HTMLMediaElement) return; if (event.key === "ArrowLeft") void navigate(-1); if (event.key === "ArrowRight") void navigate(1); }; document.addEventListener("keydown", onKeyDown); return () => document.removeEventListener("keydown", onKeyDown); });
  const save = async () => { if (!item) return; setSaving(true); setActionError(undefined); try { await saveMedia(item.attachment.id); setSavePending(false); } catch (saveError) { if (isMediaPending(saveError)) setSavePending(true); else setActionError("Could not save media."); } finally { setSaving(false); } };
  useEffect(() => { if (savePending && !saving) void save(); }, [retryKey]);
  const failed = !removedRemoteOnly && (item?.attachment.state === "failed" || (!url && Boolean(loadError)));
  useEffect(() => { if (!failed) return; const focused = document.activeElement; if (focused === closeButtonRef.current || focused === stageRef.current) retryButtonRef.current?.focus(); }, [failed]);
  useEffect(() => { closeButtonRef.current?.focus(); }, []);
  if (!item) return null;
  if (removedRemoteOnly) {
    const placeholder = <div data-attachment-storage-removed role="status"><ImageOff size={28} aria-hidden="true" /><span>{attachmentStorageRemovalText(false, peppyCopy).body}</span></div>;
    if (chrome === "window") return <main id="media-viewer-window" data-lightbox data-chrome="window" aria-labelledby="media-viewer-window-title"><h1 id="media-viewer-window-title" className="visually-hidden">Media viewer</h1>{placeholder}<button ref={closeButtonRef} id="lightbox-close-btn" type="button" aria-label="Close media viewer" title="Close" onClick={onDismiss}><X size={18} aria-hidden="true" /></button></main>;
    return <Modal id="media-lightbox" title="Media viewer" onDismiss={onDismiss} className="media-lightbox" actions={null} windowDragOutside><div data-lightbox>{placeholder}</div></Modal>;
  }
  const state = mediaState(item, url, pending); const imagePreviewUrl = previewUrl(item.attachment);
  const positionLabel = !previousExhausted || !nextExhausted ? `Image ${index + 1} of at least ${items.length} loaded` : `Image ${index + 1} of ${items.length}`;
  const saveLabel = savePending ? "Downloading image" : saving ? "Saving image" : "Save image";
  const linkLabel = actions?.publicCopy(item.attachment.id) ? "Copy public link" : "Create public link";
  const previousDisabled = (index === 0 && (!loadAdjacentPage || previousExhausted)) || paging;
  const nextDisabled = (index === items.length - 1 && (!loadAdjacentPage || nextExhausted)) || paging;
  const showPending = !url && !failed && state === "pending"; const showLoading = !url && !failed && state === "loading";
  const canCopyImage = actions?.canCopyImage && item.attachment.mediaType.startsWith("image/");
  const drag = chrome === "window" ? { "data-tauri-drag-region": true } : {};
  const content = <>
    {chrome === "modal" && <div data-lightbox-backdrop aria-hidden="true" onClick={onDismiss} />}
    <div id="lightbox-toolbar" data-lightbox-toolbar {...drag}><span id="lightbox-position-label" className="visually-hidden" aria-live="polite">{positionLabel}</span><span data-lightbox-counter aria-hidden="true">{index + 1} / {items.length}</span><div data-lightbox-actions {...drag}><button id="lightbox-save-btn" type="button" aria-label={saveLabel} title={saveLabel} disabled={saving || savePending} aria-disabled={saving || savePending || undefined} aria-busy={saving || savePending || undefined} onClick={() => void save()}>{saving || savePending ? <Loader2 className="lightbox-spinner" size={18} aria-hidden="true" /> : <Download size={18} aria-hidden="true" />}</button>{actions?.canShareLink && <button id="lightbox-link-btn" type="button" aria-label={linkLabel} title={linkLabel} onClick={() => actions.shareLink(item.attachment)}><Link size={18} aria-hidden="true" /></button>}{canCopyImage && <button id="lightbox-copy-btn" type="button" aria-label="Copy image" title="Copy image" onClick={() => actions.copyImage(item.attachment)}><Copy size={18} aria-hidden="true" /></button>}<button ref={closeButtonRef} id="lightbox-close-btn" type="button" aria-label="Close media viewer" title="Close" onClick={onDismiss}><X size={18} aria-hidden="true" /></button></div></div>
    <div ref={stageRef} data-lightbox-stage tabIndex={-1}><button id="lightbox-prev-btn" type="button" data-lightbox-nav="prev" aria-label="Newer image" title="Newer image" disabled={previousDisabled} aria-busy={paging || undefined} onClick={() => void navigate(-1)}><ChevronLeft size={22} aria-hidden="true" /></button><div data-lightbox-media data-lightbox-state={failed ? "failed" : state}>{url && state === "ready-image" && <img src={url} alt={item.attachment.name || "Image"} />}{url && state === "ready-video" && <video controls preload="none" src={url} aria-label={item.attachment.name || "Video"} />}{url && state === "ready-audio" && <audio controls preload="none" src={url} aria-label={item.attachment.name || "Audio"} />}{showPending && <>{imagePreviewUrl && <img className="lightbox-backdrop" src={imagePreviewUrl} alt="" aria-hidden="true" />}<div data-lightbox-pending data-has-backdrop={imagePreviewUrl ? true : undefined} role="status"><Loader2 className="lightbox-spinner" size={28} aria-hidden="true" /><span>Downloading from phone…</span>{stillWaiting && <small>Still waiting — make sure your phone is online.</small>}</div></>}{showLoading && <div data-lightbox-loading><Loader2 className="lightbox-spinner" size={28} aria-label="Loading image" /></div>}{failed && <div data-lightbox-failure><ImageOff size={28} aria-hidden="true" /><span>Couldn't load this image</span><button ref={retryButtonRef} id="lightbox-retry-btn" type="button" onClick={() => void request()}><RotateCw size={14} aria-hidden="true" />Retry</button></div>}{(state === "unsupported" || state === "preview-unavailable") && <p role="status">Preview unavailable. Save the original to view it in another app.</p>}</div><button id="lightbox-next-btn" type="button" data-lightbox-nav="next" aria-label="Older image" title="Older image" disabled={nextDisabled} aria-busy={paging || undefined} onClick={() => void navigate(1)}><ChevronRight size={22} aria-hidden="true" /></button></div>
    {actionError && <p role="alert">{actionError}</p>}{removedWithLocalCopy && <p data-attachment-storage-local-copy role="status">{attachmentStorageRemovalText(true, peppyCopy).body}</p>}<div id="lightbox-meta" {...drag}>{formatDateTime(item.message.timestamp)}</div>
  </>;
  if (chrome === "window") return <main id="media-viewer-window" data-lightbox data-chrome="window" aria-labelledby="media-viewer-window-title"><h1 id="media-viewer-window-title" className="visually-hidden">Media viewer</h1>{content}</main>;
  return <Modal id="media-lightbox" title="Media viewer" onDismiss={onDismiss} className="media-lightbox" actions={null} windowDragOutside><div data-lightbox>{content}</div></Modal>;
}
