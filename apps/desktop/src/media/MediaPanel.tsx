import { useEffect, useRef, useState } from "react";
import type { GalleryAttachment, GalleryMessage } from "./grouping";
import { MediaTile } from "./MediaTile";
import { tileState } from "./mediaState";

export type MediaPageItem = {
  attachment: GalleryAttachment;
  message: GalleryMessage;
  cursor?: unknown;
};

export type MediaPage = { items: MediaPageItem[]; nextCursor?: unknown };

export type MediaPanelProps = {
  loadPage(cursor?: unknown): Promise<MediaPage>;
  onOpen(item: MediaPageItem, loadedItems: MediaPageItem[]): void;
  refreshKey?: unknown;
};

function itemKey(item: MediaPageItem): string {
  return `${item.message.id}:${item.attachment.id}`;
}

export function MediaPanel({ loadPage, onOpen, refreshKey }: MediaPanelProps) {
  const [items, setItems] = useState<MediaPageItem[]>();
  const [nextCursor, setNextCursor] = useState<unknown>();
  const [error, setError] = useState<string>();
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const itemsRef = useRef<MediaPageItem[] | undefined>(undefined);
  const loadPageRef = useRef(loadPage);
  const refreshVersion = useRef(0);
  const previousRefreshKey = useRef(refreshKey);
  loadPageRef.current = loadPage;

  const load = async (cursor?: unknown) => {
    refreshVersion.current += 1;
    setRefreshing(false);
    setLoading(true); setError(undefined);
    try {
      const page = await loadPageRef.current(cursor);
      setItems((current) => {
        const nextItems = cursor ? [...(current ?? []), ...page.items] : page.items;
        itemsRef.current = nextItems;
        return nextItems;
      });
      setNextCursor(page.nextCursor);
    } catch { setError("Couldn't load media."); } finally { setLoading(false); }
  };

  useEffect(() => { void load(); }, []);

  useEffect(() => {
    if (Object.is(previousRefreshKey.current, refreshKey)) {
      setRefreshing(false);
      return;
    }
    previousRefreshKey.current = refreshKey;
    const version = ++refreshVersion.current;
    const timer = window.setTimeout(() => {
      const loadedItems = itemsRef.current;
      if (!loadedItems?.some((item) => tileState(item.attachment) !== "ready")) {
        setRefreshing(false);
        return;
      }
      setRefreshing(true);
      void (async () => {
        const refreshedItems: MediaPageItem[] = [];
        let cursor: unknown;
        for (let pageCount = 0; pageCount < 10 && refreshedItems.length < loadedItems.length; pageCount += 1) {
          const page = await loadPageRef.current(cursor);
          if (version !== refreshVersion.current) return;
          refreshedItems.push(...page.items);
          if (page.nextCursor === undefined) break;
          cursor = page.nextCursor;
        }
        if (version !== refreshVersion.current || itemsRef.current?.length !== loadedItems.length) return;
        const refreshedByKey = new Map(refreshedItems.map((item) => [itemKey(item), item]));
        setItems((current) => {
          if (!current || current.length !== loadedItems.length) return current;
          const mergedItems = current.map((item) => tileState(item.attachment) === "ready" ? item : refreshedByKey.get(itemKey(item)) ?? item);
          itemsRef.current = mergedItems;
          return mergedItems;
        });
      })().catch(() => undefined).finally(() => {
        if (version === refreshVersion.current) setRefreshing(false);
      });
    }, 750);
    return () => {
      window.clearTimeout(timer);
      refreshVersion.current += 1;
      setRefreshing(false);
    };
  }, [refreshKey]);

  if (!items && loading) return <div id="media-tab-loading" role="status" aria-live="polite" data-media-loading>Loading media…</div>;
  return <section id="media-tab-content" data-media-tab aria-busy={loading || refreshing || undefined}>
    {items?.length === 0 && <div id="media-tab-empty" data-media-empty>No media in this conversation.</div>}
    {items && <div id="media-tab-grid" data-media-grid>{items.map((item) => <MediaTile key={itemKey(item)} attachment={item.attachment} message={item.message} variant="cell" bodyAttributes={{ "data-media-cell": true }} onOpen={() => onOpen(item, items)} />)}</div>}
    <div id="media-tab-pagination" data-media-pagination>
      {error && <div id="media-load-error" role="alert">{error} <button type="button" className="secondary-button" onClick={() => void load(nextCursor)}>Retry</button></div>}
      {nextCursor !== undefined && <button id="media-load-more-btn" type="button" className="secondary-button" disabled={loading} aria-busy={loading || undefined} onClick={() => void load(nextCursor)}>Load more</button>}
    </div>
  </section>;
}
