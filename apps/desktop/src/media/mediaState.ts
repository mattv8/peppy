import type { AttachmentView } from "@peppy/desktop-ui/bridge-contracts";
import type { GalleryAttachment } from "./grouping";

export type TileState = "ready" | "loading" | "failed" | "unavailable" | "held";
export type OriginalView = { phase: "held" | "requested" | "available"; canRequest: boolean };

export function holdNoteText(state: "idle" | "requesting" | "queued" | "error", canRequest: boolean, error?: string): string {
  if (state === "requesting") return "Requesting sync from your phone…";
  if (state === "queued") return "Waiting for your phone.";
  if (state === "error") return "Original didn't sync.";
  return error ?? (canRequest ? "Original syncs on Wi-Fi. Sync uses mobile data if needed." : "Original is waiting on your phone.");
}

export function originalView(attachment: GalleryAttachment): OriginalView {
  if (attachment.mediaHold === "held") return { phase: "held", canRequest: true };
  if (attachment.mediaHold === "requested") return { phase: "requested", canRequest: false };
  return { phase: "available", canRequest: false };
}

export function originalPending(attachment: GalleryAttachment): boolean {
  return originalView(attachment).phase !== "available";
}

export function usablePreview(attachment: GalleryAttachment): AttachmentView | undefined {
  const thumbnail = attachment.mediaMeta?.thumbnail;
  if (thumbnail?.state === "ready" && thumbnail.previewUrl) return thumbnail;
  if (attachment.state === "ready" && attachment.previewUrl) return attachment;
  return undefined;
}

export function previewUrl(attachment: GalleryAttachment): string | undefined {
  return usablePreview(attachment)?.previewUrl;
}

/** `hydrationExhausted`: native was asked for a preview and gave none, so waiting longer will not help. */
export function tileState(attachment: GalleryAttachment, hydrationExhausted = false): TileState {
  if (usablePreview(attachment)) return "ready";
  if (attachment.mediaMeta?.previewUnavailable === "thumbnail_unavailable") return "unavailable";
  const thumbnail = attachment.mediaMeta?.thumbnail;
  if (thumbnail?.state === "failed" || attachment.state === "failed") return "failed";
  if (hydrationExhausted) return "unavailable";
  if (originalPending(attachment)) return "held";
  return "loading";
}

function isRemotePending(view: AttachmentView): boolean {
  return view.transfer === "download" && view.state === "pending";
}

/** The ID whose download makes a preview appear: the pending thumbnail, else a pending original with no thumbnail. */
export function demandTarget(attachment: GalleryAttachment): string | undefined {
  const state = tileState(attachment);
  const thumbnail = attachment.mediaMeta?.thumbnail;
  if (originalPending(attachment)) return thumbnail && isRemotePending(thumbnail) ? thumbnail.id : undefined;
  if (state !== "loading") return undefined;
  if (thumbnail) return isRemotePending(thumbnail) ? thumbnail.id : undefined;
  return isRemotePending(attachment) ? attachment.id : undefined;
}

/** The ID to retry for a failed tile: the failed thumbnail, else the original. */
export function retryTarget(attachment: GalleryAttachment): string {
  const thumbnail = attachment.mediaMeta?.thumbnail;
  return thumbnail?.state === "failed" ? thumbnail.id : attachment.id;
}

export function tileLabel(attachment: GalleryAttachment, state: TileState): string {
  if (originalPending(attachment)) return attachment.name ? `${attachment.name}, preview only` : "Image preview only";
  if (state === "loading") return "Loading image";
  if (state === "failed") return "Image failed to load";
  if (state === "unavailable") return "Preview unavailable";
  return attachment.name || "Image";
}

export function canOpen(attachment: GalleryAttachment, hydrationExhausted = false): boolean {
  const state = tileState(attachment, hydrationExhausted);
  return !originalPending(attachment) && (state === "ready" || state === "unavailable");
}
