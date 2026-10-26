import type { AttachmentView } from "@peppy/desktop-ui/bridge-contracts";
import type { GalleryAttachment } from "./grouping";

export type TileState = "ready" | "loading" | "failed" | "unavailable";

export function usablePreview(attachment: GalleryAttachment): AttachmentView | undefined {
  const thumbnail = attachment.mediaMeta?.thumbnail;
  if (thumbnail?.state === "ready" && thumbnail.previewUrl) return thumbnail;
  if (attachment.state === "ready" && attachment.previewUrl) return attachment;
  return undefined;
}

export function previewUrl(attachment: GalleryAttachment): string | undefined {
  return usablePreview(attachment)?.previewUrl;
}

export function tileState(attachment: GalleryAttachment): TileState {
  if (usablePreview(attachment)) return "ready";
  if (attachment.mediaMeta?.previewUnavailable === "thumbnail_unavailable") return "unavailable";
  const thumbnail = attachment.mediaMeta?.thumbnail;
  if (thumbnail?.state === "failed" || attachment.state === "failed") return "failed";
  return "loading";
}

function isRemotePending(view: AttachmentView): boolean {
  return view.transfer === "download" && view.state === "pending";
}

/** The ID whose download makes a preview appear: the pending thumbnail, else a pending original with no thumbnail. */
export function demandTarget(attachment: GalleryAttachment): string | undefined {
  if (tileState(attachment) !== "loading") return undefined;
  const thumbnail = attachment.mediaMeta?.thumbnail;
  if (thumbnail) return isRemotePending(thumbnail) ? thumbnail.id : undefined;
  return isRemotePending(attachment) ? attachment.id : undefined;
}

/** The ID to retry for a failed tile: the failed thumbnail, else the original. */
export function retryTarget(attachment: GalleryAttachment): string {
  const thumbnail = attachment.mediaMeta?.thumbnail;
  return thumbnail?.state === "failed" ? thumbnail.id : attachment.id;
}

export function tileLabel(attachment: GalleryAttachment, state: TileState): string {
  if (state === "loading") return "Loading image";
  if (state === "failed") return "Image failed to load";
  if (state === "unavailable") return "Preview unavailable";
  return attachment.name || "Image";
}
