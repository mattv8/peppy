import type { ClipboardEvent, ReactElement } from "react";
import { useEffect, useRef, useState } from "react";
import { Check, CircleAlert, Clock, Image, ImageOff, Loader, RotateCcw, X } from "lucide-react";
import type { DraftAttachmentState } from "./bridge-contracts";

export type AttachmentLayout = "tiles" | "rows";

export type Attachment = {
  id: string;
  name: string;
  state: "pending" | "uploading" | "failed" | "ready";
  error?: string;
  previewUrl?: string;
  byteSize?: number;
  draftState?: DraftAttachmentState;
  retryable?: boolean;
};

type AttachmentStatus = {
  text: string;
  icon: ReactElement;
  retryable: boolean;
};

const BYTE_UNITS = ["B", "KB", "MB", "GB"];

function formatBytes(bytes: number): string {
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 10 || unit === 0 ? Math.round(value) : value.toFixed(1)} ${BYTE_UNITS[unit]}`;
}

function attachmentStatus(attachment: Attachment): AttachmentStatus {
  const state = attachment.draftState ?? attachment.state;
  switch (state) {
    case "saving": return { text: "Saving image…", icon: <Loader aria-hidden size={12} className="busy-indicator-icon" />, retryable: false };
    case "local": return { text: "Image not saved", icon: <CircleAlert aria-hidden size={12} />, retryable: true };
    case "waiting":
    case "pending": return { text: "Waiting for connection", icon: <Clock aria-hidden size={12} />, retryable: false };
    case "uploading": return { text: "Uploading…", icon: <Loader aria-hidden size={12} className="busy-indicator-icon" />, retryable: false };
    case "uploaded":
    case "ready": return { text: "Uploaded", icon: <Check aria-hidden size={12} />, retryable: false };
    case "download-failed": return { text: "Download failed", icon: <CircleAlert aria-hidden size={12} />, retryable: true };
    case "unavailable": return { text: "Image unavailable", icon: <CircleAlert aria-hidden size={12} />, retryable: false };
    case "downloading": return { text: "Loading image…", icon: <Loader aria-hidden size={12} className="busy-indicator-icon" />, retryable: false };
    case "upload-failed":
    case "failed": return { text: "Upload failed", icon: <CircleAlert aria-hidden size={12} />, retryable: true };
  }
}

function AttachmentPreview({ attachment }: { attachment: Attachment }) {
  const [previewFailed, setPreviewFailed] = useState(false);
  useEffect(() => setPreviewFailed(false), [attachment.previewUrl]);
  const state = attachment.draftState ?? attachment.state;
  const unavailable = ["saving", "downloading", "unavailable", "failed", "upload-failed", "download-failed"].includes(state);
  return (
    <div className="attachment-preview">
      {attachment.previewUrl && !previewFailed
        ? <img src={attachment.previewUrl} alt="" onError={() => setPreviewFailed(true)} />
        : unavailable ? <ImageOff aria-hidden size={20} /> : <Image aria-hidden size={20} />}
    </div>
  );
}

export function ComposerAttachmentTray({
  attachments,
  layout = "tiles",
  onRemoveAttachment,
  onRetryAttachment,
  onPaste,
  onFocusTextarea,
}: {
  attachments: Attachment[];
  layout?: AttachmentLayout;
  onRemoveAttachment?(id: string): void;
  onRetryAttachment?(id: string): void;
  onPaste?(event: ClipboardEvent<HTMLUListElement>): void;
  onFocusTextarea(): void;
}) {
  const removeButtons = useRef(new Map<string, HTMLButtonElement>());
  const previousStates = useRef<Map<string, { name: string; status: string }> | null>(null);
  const [announcement, setAnnouncement] = useState("");
  const totalSize = attachments.reduce<number | null>((total, attachment) => (
    total === null || attachment.byteSize === undefined ? null : total + attachment.byteSize
  ), 0);
  const hasTrayAction = Boolean(onRemoveAttachment) || attachments.some((attachment) => {
    const status = attachmentStatus(attachment);
    return Boolean(onRetryAttachment) && (attachment.retryable || status.retryable);
  });
  useEffect(() => {
    const currentStates = new Map(attachments.map((attachment) => [attachment.id, {
      name: attachment.name,
      status: attachmentStatus(attachment).text,
    }]));
    const previous = previousStates.current;
    previousStates.current = currentStates;
    if (!previous) return;
    const changed = attachments.flatMap((attachment) => {
      const previousAttachment = previous.get(attachment.id);
      const currentAttachment = currentStates.get(attachment.id)!;
      return previousAttachment && previousAttachment.status !== currentAttachment.status
        ? [`${attachment.name}: ${currentAttachment.status}`]
        : [];
    });
    previous.forEach((attachment, id) => {
      if (!currentStates.has(id)) changed.push(`${attachment.name} removed`);
    });
    if (changed.length) setAnnouncement(changed.join(", "));
  }, [attachments]);
  const focusAfterRemoval = (id: string) => {
    const index = attachments.findIndex((attachment) => attachment.id === id);
    const next = attachments[index + 1] ?? attachments[index - 1];
    requestAnimationFrame(() => {
      if (next) removeButtons.current.get(next.id)?.focus();
      else onFocusTextarea();
    });
  };
  if (!attachments.length) {
    return <section id="attachment-tray-region" data-attachment-layout={layout} data-empty aria-label="Attached images"><p className="sr-only" aria-live="polite">{announcement}</p></section>;
  }
  return (
    <section id="attachment-tray-region" data-attachment-layout={layout} aria-label="Attached images">
      <header className="attachment-tray-header">
        <span>{attachments.length} {attachments.length === 1 ? "image" : "images"}</span>
        {totalSize !== null && <span>{formatBytes(totalSize)}</span>}
      </header>
      <ul id="attachment-tray" data-attachment-layout={layout} aria-label="Image list" onPaste={onPaste} tabIndex={hasTrayAction ? undefined : 0}>
        {attachments.map((attachment) => {
          const status = attachmentStatus(attachment);
          const canRetry = Boolean(onRetryAttachment) && (attachment.retryable || status.retryable);
          return (
            <li key={attachment.id} data-attachment-id={attachment.id} className="attachment-chip" data-attachment-state={attachment.draftState ?? attachment.state}>
              <AttachmentPreview attachment={attachment} />
              <div className="attachment-copy">
                <span className="attachment-name" title={attachment.name}>{attachment.name}</span>
                <span className="attachment-state">{status.icon}{status.text}</span>
                {canRetry && <button type="button" className="attachment-retry" aria-label={`Retry ${attachment.name}`} onClick={() => onRetryAttachment?.(attachment.id)}><RotateCcw size={14} aria-hidden />Retry</button>}
                {attachment.error && <span className="attachment-error" role="alert" title={attachment.error}>{attachment.error}</span>}
              </div>
              {onRemoveAttachment && <button ref={(button) => { if (button) removeButtons.current.set(attachment.id, button); else removeButtons.current.delete(attachment.id); }} type="button" className="attachment-remove" aria-label={`Remove ${attachment.name}`} title={`Remove ${attachment.name}`} onClick={() => { onRemoveAttachment(attachment.id); focusAfterRemoval(attachment.id); }}><X size={14} aria-hidden /></button>}
            </li>
          );
        })}
      </ul>
      <p className="sr-only" aria-live="polite">{announcement}</p>
    </section>
  );
}
