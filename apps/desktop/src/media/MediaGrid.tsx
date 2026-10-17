import type { ReactNode, RefCallback } from "react";
import type { AttachmentView } from "@peppy/desktop-ui/bridge-contracts";
import { groupMessages, type GalleryAttachment, type GalleryMessage, type MessageGroup } from "./grouping";
import "./media.css";

export type MediaGridProps = {
  messages: GalleryMessage[];
  onOpen(item: { attachment: GalleryAttachment; message: GalleryMessage }): void;
  renderMessageMeta?(message: GalleryMessage): ReactNode;
  renderAttachmentActions?(attachment: GalleryAttachment): ReactNode;
  messageRef?(message: GalleryMessage): RefCallback<HTMLElement> | undefined;
};

function thumbnailState(attachment: GalleryAttachment): string {
  const preview = usablePreview(attachment);
  if (preview) return "ready";
  if (attachment.mediaMeta?.previewUnavailable === "thumbnail_unavailable") return "preview-unavailable";
  const thumbnail = attachment.mediaMeta?.thumbnail;
  if (thumbnail?.state === "failed" || attachment.state === "failed") return "failed";
  return (thumbnail ?? attachment).transfer === "download" ? "pending-download" : "loading";
}

function labelFor(attachment: GalleryAttachment, state: string): string {
  if (state === "loading") return "Loading image";
  if (state === "pending-download") return "Image not yet downloaded";
  if (state === "failed") return "Image failed to load";
  if (state === "preview-unavailable") return "Preview unavailable";
  return attachment.name || "Image";
}

function previewUrl(attachment: GalleryAttachment): string | undefined {
  return usablePreview(attachment)?.previewUrl;
}

function usablePreview(attachment: GalleryAttachment): AttachmentView | undefined {
  const thumbnail = attachment.mediaMeta?.thumbnail;
  if (thumbnail?.state === "ready" && thumbnail.previewUrl) return thumbnail;
  if (attachment.state === "ready" && attachment.previewUrl) return attachment;
  return undefined;
}

function Group({ group, onOpen, renderMessageMeta, renderAttachmentActions, messageRef }: { group: MessageGroup; onOpen: MediaGridProps["onOpen"]; renderMessageMeta?: MediaGridProps["renderMessageMeta"]; renderAttachmentActions?: MediaGridProps["renderAttachmentActions"]; messageRef?: MediaGridProps["messageRef"] }) {
  const visible = group.attachments.length > 4 ? group.attachments.slice(0, 3) : group.attachments;
  const finalMessage = group.messages.at(-1);
  const isGrouped = group.messages.length > 1 || group.attachments.length > 1;
  return <div className={`message-row ${group.messages[0].sender}`} data-message-group={group.id} data-sender={group.messages[0].sender}>
    <article data-grouped={isGrouped || undefined} data-sender={group.messages[0].sender} data-sender-address={group.messages[0].senderAddress} aria-label={`${group.attachments.length} images`}>
      <div data-group-grid data-cell-count={visible.length}>
        {visible.map(({ attachment, message }) => {
          const state = thumbnailState(attachment);
          return <div key={`${message.id}:${attachment.id}`} data-gallery-attachment-actions={attachment.id}>
            <button type="button" data-thumbnail data-thumbnail-state={state} data-attachment-id={attachment.id} data-message-id={message.id} aria-label={labelFor(attachment, state)} onClick={() => onOpen({ attachment, message })}>
              {previewUrl(attachment) && <img src={previewUrl(attachment)} alt="" aria-hidden="true" />}
              {state !== "ready" && <span>{labelFor(attachment, state)}</span>}
            </button>
            {renderAttachmentActions?.(attachment)}
          </div>;
        })}
        {group.attachments.length > 4 && <button type="button" data-overflow-count aria-label={`Open gallery, ${group.attachments.length - 3} more images`} onClick={() => onOpen(group.attachments[3])}>+{group.attachments.length - 3} more</button>}
      </div>
      {finalMessage?.body && <p data-group-caption>{finalMessage.body}</p>}
      {finalMessage?.subject && <p data-group-subject>Subject: {finalMessage.subject}</p>}
    </article>
    {group.messages.map((message) => <div key={message.id} ref={messageRef?.(message)} data-message-row={message.id}><div data-message-id={message.id} data-message-meta={message.id}>{renderMessageMeta?.(message)}</div></div>)}
  </div>;
}

export function MediaGrid(props: MediaGridProps) {
  return <section data-media-message-grid>{groupMessages(props.messages).map((group) => <Group key={group.id} group={group} {...props} />)}</section>;
}
