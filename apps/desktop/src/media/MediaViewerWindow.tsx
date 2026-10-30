import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ImageOff, Loader2, RotateCw, X } from "@peppy/desktop-ui";
import { bridge } from "../bridge";
import { MediaActionsProvider } from "./MediaActionsContext";
import { MediaLightbox } from "./MediaLightbox";
import type { MediaPageItem } from "./MediaPanel";

const ID = /^[A-Za-z0-9_-]{1,128}$/;
const PAGE_SIZE = 40;
type Route = { conversationId: string; attachmentId: string; messageId: string };
type ViewerState = "loading" | "error" | "missing";

const defaultClose = () => { void getCurrentWindow().close().catch(() => undefined); };
const releaseMedia = (attachmentId: string, url: string) => { void bridge.close_media(attachmentId, url); };
const loadOriginal = (attachmentId: string) => bridge.load_media(attachmentId);
const checkOriginal = (attachmentId: string) => bridge.open_media(attachmentId);

export function isMediaViewerRoute(search: string): boolean {
  return new URLSearchParams(search).get("window") === "media";
}

export function readMediaViewerWindow(search: string): Route | null {
  const query = new URLSearchParams(search);
  const conversationId = query.get("conversationId");
  const attachmentId = query.get("attachmentId");
  const messageId = query.get("messageId");
  return isMediaViewerRoute(search) && conversationId && attachmentId && messageId && [conversationId, attachmentId, messageId].every(value => ID.test(value)) ? { conversationId, attachmentId, messageId } : null;
}

function viewerTheme(): "light" | "dark" | "system" {
  try { const layout = JSON.parse(localStorage.getItem("peppy.layout.v1") ?? "{}"); return layout && typeof layout === "object" && ["light", "dark", "system"].includes(layout.theme) ? layout.theme : "system"; } catch { return "system"; }
}
function toItem(item: Awaited<ReturnType<typeof bridge.media_page>>["items"][number]): MediaPageItem { return { attachment: { ...item.attachment, mediaMeta: item.attachment.mediaMeta ?? item.mediaMeta }, message: { id: item.messageId, revision: item.cursor.localOrder, sender: item.senderAddress ? "other" : "self", senderAddress: item.senderAddress, body: "", timestamp: item.timestamp, attachments: [] }, cursor: item.cursor }; }
function unique(items: MediaPageItem[]): MediaPageItem[] { return [...new Map(items.map(item => [`${item.message.id}:${item.attachment.id}`, item])).values()]; }

function useViewerTheme() {
  useLayoutEffect(() => {
    const sync = () => { const theme = viewerTheme(); document.body.classList.remove("theme-light", "theme-dark", "theme-system"); document.body.classList.add(`theme-${theme}`); };
    const onStorage = (event: StorageEvent) => { if (event.key === "peppy.layout.v1") sync(); };
    sync();
    window.addEventListener("storage", onStorage);
    return () => { window.removeEventListener("storage", onStorage); document.body.classList.remove("theme-light", "theme-dark", "theme-system"); };
  }, []);
}

function useViewerTitle() {
  useEffect(() => {
    const previousTitle = document.title;
    document.title = "Peppy media";
    return () => { document.title = previousTitle; };
  }, []);
}

function MediaViewerShell({ state, close, retry }: { state: ViewerState; close: () => void; retry?: () => void }) {
  const closeRef = useRef<HTMLButtonElement>(null);
  const retryRef = useRef<HTMLButtonElement>(null);
  useEffect(() => { (state === "error" ? retryRef.current ?? closeRef.current : closeRef.current)?.focus(); }, [state]);
  const content = state === "loading" ? <div data-lightbox-loading role="status"><Loader2 className="lightbox-spinner" size={28} aria-hidden="true" /><span className="visually-hidden">Loading media</span></div> : state === "error" ? <div data-lightbox-failure role="alert"><ImageOff size={28} aria-hidden="true" /><span>Couldn&apos;t open this media</span>{retry && <button ref={retryRef} id="lightbox-retry-btn" type="button" onClick={retry}><RotateCw size={14} aria-hidden="true" />Retry</button>}</div> : <p role="status">This media is no longer available.</p>;
  return <main id="media-viewer-window" data-lightbox data-chrome="window" data-viewer-state={state} aria-labelledby="media-viewer-window-title"><h1 id="media-viewer-window-title" className="visually-hidden">Media viewer</h1><div id="lightbox-toolbar" data-lightbox-toolbar data-tauri-drag-region><div data-lightbox-actions data-tauri-drag-region><button ref={closeRef} id="lightbox-close-btn" type="button" aria-label="Close media viewer" title="Close" onClick={close}><X size={18} aria-hidden="true" /></button></div></div><div data-lightbox-stage><div data-lightbox-media>{content}</div></div></main>;
}

export function InvalidMediaViewerWindow({ close = defaultClose }: { close?: () => void }) {
  useViewerTheme();
  useViewerTitle();
  useEffect(() => { const key = (event: KeyboardEvent) => { if (event.key === "Escape" && !event.defaultPrevented && !event.repeat && !document.fullscreenElement) close(); }; document.addEventListener("keydown", key); return () => document.removeEventListener("keydown", key); }, [close]);
  return <MediaViewerShell state="error" close={close} />;
}

export function MediaViewerWindow({ conversationId, attachmentId, messageId, close = defaultClose }: Route & { close?: () => void }) {
  const [items, setItems] = useState<MediaPageItem[]>();
  const [state, setState] = useState<ViewerState>("loading");
  const [retryKey, setRetryKey] = useState(0);
  const cache = useRef<{ items: MediaPageItem[]; newerCursor?: unknown; olderCursor?: unknown }>({ items: [] });
  const loadVersion = useRef(0);
  useViewerTheme();
  useViewerTitle();
  const load = async () => {
    const version = ++loadVersion.current;
    setState("loading");
    try {
      const page = await bridge.media_page_around(conversationId, attachmentId, messageId, PAGE_SIZE);
      if (version !== loadVersion.current) return;
      const loaded = unique([...page.newer.map(toItem).reverse(), toItem(page.target), ...page.older.map(toItem)]);
      if (!loaded.some(item => item.message.id === messageId && item.attachment.id === attachmentId)) { setState("missing"); return; }
      cache.current = { items: loaded, newerCursor: page.newerCursor, olderCursor: page.olderCursor };
      setItems(loaded);
    } catch { if (version === loadVersion.current) setState("error"); }
  };
  useEffect(() => { void load(); return () => { loadVersion.current += 1; }; }, [attachmentId, conversationId, messageId]);
  useEffect(() => bridge.subscribe(() => setRetryKey(value => value + 1)), []);
  useEffect(() => { const key = (event: KeyboardEvent) => { if (event.key === "Escape" && !event.defaultPrevented && !event.repeat && !document.fullscreenElement) close(); }; document.addEventListener("keydown", key); return () => document.removeEventListener("keydown", key); }, [close]);
  if (!items) return <MediaViewerShell state={state} close={close} retry={() => void load()} />;
  return <MediaActionsProvider publicCopies={{}} onSave={async attachment => await bridge.save_attachment(attachment.id) ? "saved" : "cancelled"} onRetry={id => bridge.retry_attachment(id)} requestDownload={id => bridge.request_attachment_download(id)} loadOriginal={loadOriginal} releaseMedia={releaseMedia} canCopyImage={false} copyNativeImage={bridge.copy_attachment_image ? id => bridge.copy_attachment_image!(id) : undefined}><MediaLightbox chrome="window" items={items} initialAttachmentId={attachmentId} initialMessageId={messageId} onDismiss={close} requestOriginal={loadOriginal} checkOriginal={checkOriginal} saveMedia={async id => { await bridge.save_attachment(id); }} releaseMedia={releaseMedia} retryKey={retryKey} hasPreviousPage={Boolean(cache.current.newerCursor)} hasNextPage={Boolean(cache.current.olderCursor)} loadAdjacentPage={async direction => { const cursor = direction === "previous" ? cache.current.newerCursor : cache.current.olderCursor; if (!cursor) return undefined; const page = direction === "previous" ? await bridge.media_page_newer(conversationId, cursor as never, PAGE_SIZE) : await bridge.media_page(conversationId, cursor as never, PAGE_SIZE); const pageItems = unique(page.items.map(toItem)).filter(item => !cache.current.items.some(known => known.message.id === item.message.id && known.attachment.id === item.attachment.id)); const ordered = direction === "previous" ? pageItems.reverse() : pageItems; cache.current.items = direction === "previous" ? [...ordered, ...cache.current.items] : [...cache.current.items, ...ordered]; if (direction === "previous") cache.current.newerCursor = page.nextCursor; else cache.current.olderCursor = page.nextCursor; return { items: ordered, hasNextPage: Boolean(page.nextCursor), cursor: page.nextCursor }; }} /> </MediaActionsProvider>;
}
