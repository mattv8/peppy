import { useEffect, useState } from "react";
import type { GalleryAttachment, GalleryMessage } from "./grouping";

export type MediaPageItem = {
  attachment: GalleryAttachment;
  message: GalleryMessage;
  cursor?: unknown;
};

export type MediaPage = { items: MediaPageItem[]; nextCursor?: unknown };

export type MediaPanelProps = {
  loadPage(cursor?: unknown): Promise<MediaPage>;
  onOpen(item: MediaPageItem, loadedItems: MediaPageItem[]): void;
};

function cellState(attachment: GalleryAttachment): string {
  const preview = usablePreview(attachment);
  if (preview) return "ready";
  if (attachment.mediaMeta?.previewUnavailable === "thumbnail_unavailable") return "preview-unavailable";
  const thumbnail = attachment.mediaMeta?.thumbnail;
  if (thumbnail?.state === "failed" || attachment.state === "failed") return "failed";
  return (thumbnail ?? attachment).transfer === "download" ? "pending-download" : "loading";
}

function previewUrl(attachment: GalleryAttachment): string | undefined {
  return usablePreview(attachment)?.previewUrl;
}

function cellLabel(attachment: GalleryAttachment, state: string): string {
  if (state === "ready") return attachment.name || "Image";
  if (state === "preview-unavailable") return "Preview unavailable";
  if (state === "failed") return "Image failed to load";
  if (state === "loading") return "Loading image";
  return "Image not yet downloaded";
}

function usablePreview(attachment: GalleryAttachment): GalleryAttachment | undefined {
  const thumbnail = attachment.mediaMeta?.thumbnail;
  if (thumbnail?.state === "ready" && thumbnail.previewUrl) return thumbnail;
  if (attachment.state === "ready" && attachment.previewUrl) return attachment;
  return undefined;
}

export function MediaPanel({ loadPage, onOpen }: MediaPanelProps) {
  const [items, setItems] = useState<MediaPageItem[]>();
  const [nextCursor, setNextCursor] = useState<unknown>();
  const [error, setError] = useState<string>();
  const [loading, setLoading] = useState(false);
  const load = async (cursor?: unknown) => {
    setLoading(true); setError(undefined);
    try {
      const page = await loadPage(cursor);
      setItems((current) => cursor ? [...(current ?? []), ...page.items] : page.items);
      setNextCursor(page.nextCursor);
    } catch { setError("Couldn't load media."); } finally { setLoading(false); }
  };
  useEffect(() => { void load(); }, []);
  if (!items && loading) return <div id="media-tab-loading" role="status" aria-live="polite" data-media-loading>Loading media…</div>;
  return <section id="media-tab-content" data-media-tab aria-busy={loading || undefined}>
    {items?.length === 0 && <div id="media-tab-empty" data-media-empty>No media in this conversation.</div>}
    {items && <div id="media-tab-grid" data-media-grid>{items.map((item) => {
      const state = cellState(item.attachment);
      return <button key={`${item.message.id}:${item.attachment.id}`} type="button" className="media-cell" data-media-cell data-cell-state={state} data-attachment-id={item.attachment.id} data-message-id={item.message.id} aria-label={cellLabel(item.attachment, state)} onClick={() => onOpen(item, items)}>
        {previewUrl(item.attachment) && <img src={previewUrl(item.attachment)} alt="" aria-hidden="true" />}
        {state !== "ready" && <span>{cellLabel(item.attachment, state)}</span>}
      </button>;
    })}</div>}
    <div id="media-tab-pagination" data-media-pagination>
      {error && <div id="media-load-error" role="alert">{error} <button type="button" className="secondary-button" onClick={() => void load(nextCursor)}>Retry</button></div>}
      {nextCursor !== undefined && <button id="media-load-more-btn" type="button" className="secondary-button" disabled={loading} aria-busy={loading || undefined} onClick={() => void load(nextCursor)}>Load more</button>}
    </div>
  </section>;
}
