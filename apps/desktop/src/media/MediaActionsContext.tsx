import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type JSX, type ReactNode } from "react";
import { createPortal } from "react-dom";
import type { PublicCopy } from "@peppy/desktop-ui/bridge-contracts";
import type { GalleryAttachment } from "./grouping";
import { retryTarget } from "./mediaState";
import { MediaPendingError, originalAsPng, writeImageToClipboard } from "./clipboard";
import { createPreviewHydrator, type HydratePreview, type HydrationOutcome } from "./previewHydration";
import "./media-actions.css";

export type SaveOutcome = "saved" | "cancelled" | "pending";
export type MediaResult = { state: "ready"; url: string } | { state: "pending" };
export type StatusTone = "info" | "warning" | "error";

export type MediaActionsProviderProps = {
  publicCopies: Record<string, PublicCopy>;
  onSave(attachment: GalleryAttachment): Promise<SaveOutcome>;
  onPublish(attachment: GalleryAttachment): Promise<PublicCopy | null>;
  onRetry(attachmentId: string): Promise<void>;
  requestDownload(attachmentId: string): Promise<void>;
  requestMediaSync?(attachmentIds: string[]): Promise<void>;
  loadOriginal(attachmentId: string): Promise<MediaResult>;
  releaseMedia(attachmentId: string, url: string): void;
  canCopyImage: boolean;
  /** Native clipboard export by original attachment ID; when present it replaces the browser clipboard path. */
  copyNativeImage?(attachmentId: string): Promise<void>;
  hydrateMediaPreview?: HydratePreview;
  children: ReactNode;
};

export type MediaActions = {
  save(attachment: GalleryAttachment): void;
  isSaving(attachmentId: string): boolean;
  publicCopy(attachmentId: string): PublicCopy | undefined;
  shareLink(attachment: GalleryAttachment): void;
  canCopyImage: boolean;
  copyImage(attachment: GalleryAttachment): void;
  retry(attachment: GalleryAttachment): void;
  requestDownload(attachmentId: string): void;
  canRequestMediaSync: boolean;
  requestMediaSync?(attachmentIds: string[]): Promise<void>;
  /** Classified outcome: `busy` is retryable; `terminal` and `settled` without a preview are not. */
  hydratePreview?(messageId: string, attachmentId: string, signal?: AbortSignal): Promise<HydrationOutcome>;
  /** Native Copy also runs for pending originals: it demands the download and reports that it is pending. */
  nativeCopy: boolean;
};

type Status = { text: string; tone: StatusTone; host: Element | null };
const MediaActionsContext = createContext<MediaActions | undefined>(undefined);

function expiryText(seconds: number): string {
  if (seconds <= 0) return "No expiry was reported; revoke it on the server when it is no longer needed.";
  if (seconds < 3600) return `Expires in ${Math.ceil(seconds / 60)} min.`;
  if (seconds < 172800) return `Expires in ${Math.round(seconds / 3600)} h.`;
  return `Expires in ${Math.round(seconds / 86400)} days.`;
}

type NativeCopyFailure = { text: string; tone: StatusTone };

/** Maps the native copy error codes (lib.rs clipboard_image_error and the copy command) to user messages. */
export function nativeCopyFailure(error: unknown): NativeCopyFailure {
  const code = typeof error === "object" && error !== null && "code" in error ? error.code : undefined;
  switch (code) {
    case "media-pending": return { text: "Downloading image from your phone. Copy again when it finishes.", tone: "info" };
    case "media-busy": return { text: "Media is busy. Try Copy again shortly.", tone: "info" };
    case "media-too-large": return { text: "This image is too large to copy safely.", tone: "error" };
    case "unsupported": return { text: "Only PNG, JPEG, WebP, and GIF images can be copied.", tone: "error" };
    case "attachment-invalid": return { text: "This image could not be decoded for copying.", tone: "error" };
    case "clipboard-unavailable": return { text: "The system clipboard is unavailable. Try again.", tone: "error" };
    default: return { text: "Couldn't copy image", tone: "error" };
  }
}

function statusHost(): Element {
  const dialogs = document.querySelectorAll("dialog[open]");
  return dialogs.item(dialogs.length - 1) ?? document.body;
}

export function MediaActionsProvider(props: MediaActionsProviderProps): JSX.Element {
  const [savingIds, setSavingIds] = useState<ReadonlySet<string>>(new Set());
  const savingIdsRef = useRef<ReadonlySet<string>>(new Set());
  const [status, setStatus] = useState<Status>({ text: "", tone: "info", host: null });
  const mounted = useRef(true);
  const clearTimer = useRef<number | undefined>(undefined);
  const animationFrame = useRef<number | undefined>(undefined);
  const lastStatusText = useRef("");
  const pendingDownloads = useRef(new Map<string, Promise<void>>());
  const pendingMediaSyncs = useRef(new Map<string, Promise<void>>());
  const publishingIds = useRef(new Set<string>());
  const requestDownloadHandler = useRef(props.requestDownload);
  requestDownloadHandler.current = props.requestDownload;
  const mediaSyncHandler = useRef(props.requestMediaSync);
  mediaSyncHandler.current = props.requestMediaSync;
  const copyingIds = useRef(new Set<string>());
  const hydratePreview = useMemo(() => props.hydrateMediaPreview && createPreviewHydrator(props.hydrateMediaPreview), [props.hydrateMediaPreview]);
  const canCopyImage = Boolean(props.copyNativeImage) || props.canCopyImage;

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      window.clearTimeout(clearTimer.current);
      if (animationFrame.current !== undefined) cancelAnimationFrame(animationFrame.current);
    };
  }, []);

  const announce = useCallback((text: string, tone: StatusTone = "info") => {
    if (!mounted.current) return;
    window.clearTimeout(clearTimer.current);
    if (animationFrame.current !== undefined) cancelAnimationFrame(animationFrame.current);
    const host = statusHost();
    const show = () => {
      animationFrame.current = undefined;
      lastStatusText.current = text;
      setStatus({ text, tone, host });
      clearTimer.current = window.setTimeout(() => {
        lastStatusText.current = "";
        setStatus({ text: "", tone: "info", host });
      }, tone === "warning" ? 6000 : 4000);
    };
    if (lastStatusText.current === text) {
      lastStatusText.current = "";
      setStatus({ text: "", tone: "info", host });
      animationFrame.current = requestAnimationFrame(show);
      return;
    }
    show();
  }, []);

  const save = useCallback((attachment: GalleryAttachment) => {
    if (savingIdsRef.current.has(attachment.id)) return;
    const next = new Set(savingIdsRef.current).add(attachment.id);
    savingIdsRef.current = next;
    setSavingIds(next);
    void props.onSave(attachment).then((outcome) => {
      if (outcome === "pending") announce(`Downloading ${attachment.name || "image"} from your phone before saving`);
    }).catch(() => announce("Couldn't save image", "error")).finally(() => {
      const settled = new Set(savingIdsRef.current);
      settled.delete(attachment.id);
      savingIdsRef.current = settled;
      setSavingIds(settled);
    });
  }, [announce, props.onSave]);

  const shareLink = useCallback((attachment: GalleryAttachment) => {
    const existingCopy = props.publicCopies[attachment.id];
    const copyText = async (copy: PublicCopy): Promise<boolean> => {
      if (!navigator.clipboard?.writeText) return false;
      try {
        await navigator.clipboard.writeText(copy.url);
        return true;
      } catch {
        return false;
      }
    };
    if (existingCopy) {
      void copyText(existingCopy).then((copied) => announce(copied ? "Link copied" : "Couldn't copy link", copied ? "info" : "error"));
      return;
    }
    if (publishingIds.current.has(attachment.id)) return;
    publishingIds.current.add(attachment.id);
    void props.onPublish(attachment).then(async (created) => {
      if (!created) return;
      const copied = await copyText(created);
      announce(copied ? `Link copied. Anyone with this link can view this image · expires ${expiryText(created.expiresInSeconds)}` : "Link created — use Copy public link to copy it", "warning");
    }).catch(() => announce("Couldn't create public link", "error")).finally(() => publishingIds.current.delete(attachment.id));
  }, [announce, props.onPublish, props.publicCopies]);

  const copyImage = useCallback((attachment: GalleryAttachment) => {
    const nativeCopy = props.copyNativeImage;
    if (nativeCopy) {
      // Native copy is a user-initiated OS export; a pending original is never copied in the background.
      if (copyingIds.current.has(attachment.id)) {
        announce("Copying image…");
        return;
      }
      copyingIds.current.add(attachment.id);
      announce("Copying image…");
      void nativeCopy(attachment.id).then(() => announce("Image copied"), (error: unknown) => {
        const failure = nativeCopyFailure(error);
        announce(failure.text, failure.tone);
      }).finally(() => copyingIds.current.delete(attachment.id));
      return;
    }
    if (!props.canCopyImage) {
      announce("Couldn't copy image", "error");
      return;
    }
    const png = originalAsPng(() => props.loadOriginal(attachment.id), (url) => props.releaseMedia(attachment.id, url));
    void writeImageToClipboard(png).then(() => announce("Image copied")).catch((error: unknown) => {
      announce(error instanceof MediaPendingError ? "Image is still downloading from your phone" : "Couldn't copy image", error instanceof MediaPendingError ? "info" : "error");
    });
  }, [announce, props.canCopyImage, props.copyNativeImage, props.loadOriginal, props.releaseMedia]);

  const retry = useCallback((attachment: GalleryAttachment) => {
    void props.onRetry(retryTarget(attachment)).catch(() => announce("Couldn't retry download", "error"));
  }, [announce, props.onRetry]);

  const requestDownload = useCallback((attachmentId: string) => {
    if (pendingDownloads.current.has(attachmentId)) return;
    const pending = requestDownloadHandler.current(attachmentId).catch(() => undefined).finally(() => pendingDownloads.current.delete(attachmentId));
    pendingDownloads.current.set(attachmentId, pending);
  }, []);

  const requestMediaSync = useCallback((attachmentIds: string[]) => {
    const handler = mediaSyncHandler.current;
    if (!handler) return Promise.reject(new Error("Media sync is unavailable"));
    const ids = [...new Set(attachmentIds)].sort();
    const key = ids.join("\u0000");
    const existing = pendingMediaSyncs.current.get(key);
    if (existing) return existing;
    const pending = (async () => {
      for (let start = 0; start < ids.length; start += 32) await handler(ids.slice(start, start + 32));
      announce("Requested original from your phone");
    })().catch((error: unknown) => {
      announce("Couldn't send sync request", "error");
      throw error;
    }).finally(() => pendingMediaSyncs.current.delete(key));
    pendingMediaSyncs.current.set(key, pending);
    return pending;
  }, [announce]);

  const actions = useMemo<MediaActions>(() => ({
    save,
    isSaving: (attachmentId) => savingIds.has(attachmentId),
    publicCopy: (attachmentId) => props.publicCopies[attachmentId],
    shareLink,
    canCopyImage,
    copyImage,
    retry,
    requestDownload,
    canRequestMediaSync: Boolean(props.requestMediaSync),
    requestMediaSync: props.requestMediaSync ? requestMediaSync : undefined,
    hydratePreview,
    nativeCopy: Boolean(props.copyNativeImage),
  }), [canCopyImage, copyImage, hydratePreview, props.copyNativeImage, props.publicCopies, props.requestMediaSync, requestDownload, requestMediaSync, retry, save, savingIds, shareLink]);

  const statusNode = <div id="media-action-status" role="status" aria-live="polite" aria-atomic="true" data-tone={status.tone}>{status.text}</div>;
  return <MediaActionsContext.Provider value={actions}>{props.children}{createPortal(statusNode, status.host ?? document.body)}</MediaActionsContext.Provider>;
}

export function useMediaActions(): MediaActions | undefined {
  return useContext(MediaActionsContext);
}
