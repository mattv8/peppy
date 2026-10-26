import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type JSX, type ReactNode } from "react";
import { createPortal } from "react-dom";
import type { PublicCopy } from "@peppy/desktop-ui/bridge-contracts";
import type { GalleryAttachment } from "./grouping";
import { retryTarget } from "./mediaState";
import { MediaPendingError, originalAsPng, writeImageToClipboard } from "./clipboard";
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
  loadOriginal(attachmentId: string): Promise<MediaResult>;
  releaseMedia(attachmentId: string, url: string): void;
  canCopyImage: boolean;
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
};

type Status = { text: string; tone: StatusTone; host: Element | null };
const MediaActionsContext = createContext<MediaActions | undefined>(undefined);

function expiryText(seconds: number): string {
  if (seconds <= 0) return "No expiry was reported; revoke it on the server when it is no longer needed.";
  if (seconds < 3600) return `Expires in ${Math.ceil(seconds / 60)} min.`;
  if (seconds < 172800) return `Expires in ${Math.round(seconds / 3600)} h.`;
  return `Expires in ${Math.round(seconds / 86400)} days.`;
}

function statusHost(): Element {
  const dialogs = document.querySelectorAll("dialog[open]");
  return dialogs.item(dialogs.length - 1) ?? document.body;
}

export function MediaActionsProvider(props: MediaActionsProviderProps): JSX.Element {
  const [savingIds, setSavingIds] = useState<ReadonlySet<string>>(new Set());
  const savingIdsRef = useRef<ReadonlySet<string>>(new Set());
  const [status, setStatus] = useState<Status>({ text: "", tone: "info", host: null });
  const clearTimer = useRef<number | undefined>(undefined);
  const animationFrame = useRef<number | undefined>(undefined);
  const lastStatusText = useRef("");
  const pendingDownloads = useRef(new Map<string, Promise<void>>());
  const publishingIds = useRef(new Set<string>());
  const requestDownloadHandler = useRef(props.requestDownload);
  requestDownloadHandler.current = props.requestDownload;

  useEffect(() => () => {
    window.clearTimeout(clearTimer.current);
    if (animationFrame.current !== undefined) cancelAnimationFrame(animationFrame.current);
  }, []);

  const announce = useCallback((text: string, tone: StatusTone = "info") => {
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
    if (!props.canCopyImage) {
      announce("Couldn't copy image", "error");
      return;
    }
    const png = originalAsPng(() => props.loadOriginal(attachment.id), (url) => props.releaseMedia(attachment.id, url));
    void writeImageToClipboard(png).then(() => announce("Image copied")).catch((error: unknown) => {
      announce(error instanceof MediaPendingError ? "Image is still downloading from your phone" : "Couldn't copy image", error instanceof MediaPendingError ? "info" : "error");
    });
  }, [announce, props.canCopyImage, props.loadOriginal, props.releaseMedia]);

  const retry = useCallback((attachment: GalleryAttachment) => {
    void props.onRetry(retryTarget(attachment)).catch(() => announce("Couldn't retry download", "error"));
  }, [announce, props.onRetry]);

  const requestDownload = useCallback((attachmentId: string) => {
    if (pendingDownloads.current.has(attachmentId)) return;
    const pending = requestDownloadHandler.current(attachmentId).catch(() => undefined).finally(() => pendingDownloads.current.delete(attachmentId));
    pendingDownloads.current.set(attachmentId, pending);
  }, []);

  const actions = useMemo<MediaActions>(() => ({
    save,
    isSaving: (attachmentId) => savingIds.has(attachmentId),
    publicCopy: (attachmentId) => props.publicCopies[attachmentId],
    shareLink,
    canCopyImage: props.canCopyImage,
    copyImage,
    retry,
    requestDownload,
  }), [copyImage, props.canCopyImage, props.publicCopies, requestDownload, retry, save, savingIds, shareLink]);

  const statusNode = <div id="media-action-status" role="status" aria-live="polite" aria-atomic="true" data-tone={status.tone}>{status.text}</div>;
  return <MediaActionsContext.Provider value={actions}>{props.children}{createPortal(statusNode, status.host ?? document.body)}</MediaActionsContext.Provider>;
}

export function useMediaActions(): MediaActions | undefined {
  return useContext(MediaActionsContext);
}
