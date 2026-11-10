import { useState } from "react";
import { FileText, ImageOff, Loader2, RotateCw } from "@peppy/desktop-ui";
import type { AttachmentView, RcsAttachmentState } from "@peppy/desktop-ui/bridge-contracts";
import { peppyCopy } from "../generated/peppyCopy";
import { canRetryRcsDownload, rcsPlaceholderCopy, rcsPlaceholderState, rcsRetryBlockCopy } from "./rcsMediaState";
import { useRcsMediaActions } from "./RcsMediaActions";

type RcsRetryControlsProps = {
  messageId: string;
  attachment: AttachmentView;
  attachmentIndex: number;
};

/** Retry download (storage full and failed only) and Free up space (storage full only). Nothing renders without the actions context. */
export function RcsRetryControls({ messageId, attachment, attachmentIndex }: RcsRetryControlsProps) {
  const actions = useRcsMediaActions();
  const [failed, setFailed] = useState(false);
  const state = attachment.rcsState;
  if (!actions || !state || state === "stored") return null;
  const retryable = canRetryRcsDownload(state);
  const requestState = actions.requestState(messageId, attachmentIndex, state);
  const requested = requestState === "requested";
  const blockNote = rcsRetryBlockCopy(actions.retryBlock);
  const retryDisabled = requested || (retryable && blockNote !== undefined);
  const noteId = `rcs-media-retry-note-${attachment.id}`;
  const retry = () => {
    if (retryDisabled) return;
    setFailed(false);
    actions.requestRetry(messageId, attachmentIndex, state).catch(() => setFailed(true));
  };
  return <>
    {retryable && <button
      type="button"
      id={`rcs-media-retry-${attachment.id}`}
      className="secondary-button"
      aria-disabled={retryDisabled || undefined}
      aria-busy={requested || undefined}
      aria-describedby={blockNote || requested ? noteId : undefined}
      aria-label={requested ? undefined : peppyCopy.rcs_media_retry_label.replace("{name}", attachment.name)}
      onClick={retry}
    >
      {requested ? <Loader2 size={14} aria-hidden="true" className="tile-action-spinner" /> : <RotateCw size={14} aria-hidden="true" />}
      {requested ? peppyCopy.rcs_media_retry_requested : requestState === "retry-again" ? peppyCopy.rcs_media_retry_again : peppyCopy.rcs_media_retry}
    </button>}
    {requested && <span className="sr-only" role="status">{peppyCopy.rcs_media_retry_requested}</span>}
    {state === "downloading" && blockNote && <small data-rcs-media-waiting>{peppyCopy.rcs_media_retry_waiting}</small>}
    {retryable && (requested || blockNote) && <small id={noteId}>{requested ? peppyCopy.rcs_media_retry_waiting : blockNote}</small>}
    {state === "quota_full" && <button type="button" id={`rcs-media-free-space-${attachment.id}`} className="secondary-button" onClick={() => actions.openStorageSettings()}>{peppyCopy.rcs_media_free_space}</button>}
    {failed && <p role="alert" data-rcs-media-retry-error>{peppyCopy.rcs_media_retry_error}</p>}
  </>;
}

type RcsMediaFileRowProps = {
  messageId: string;
  attachment: AttachmentView;
  attachmentIndex: number;
  /** Formatted size, omitted when the byte size is unknown. */
  size?: string;
};

/** Incoming attachment that has no bytes yet, shown as a file row. Callers only render it for a placeholder state. */
export function RcsMediaFileRow({ messageId, attachment, attachmentIndex, size }: RcsMediaFileRowProps) {
  const state = rcsPlaceholderState(attachment);
  if (!state) return null;
  const copy = rcsPlaceholderCopy(state);
  const visual = attachment.mediaType.startsWith("image/") || attachment.mediaType.startsWith("video/");
  const Icon = state === "downloading" ? Loader2 : visual ? ImageOff : FileText;
  return <div id={`rcs-media-placeholder-${attachment.id}`} className="file-card rcs-media-placeholder" data-attachment-id={attachment.id} data-rcs-media-state={state} data-rcs-media-direction="incoming" aria-busy={state === "downloading" || undefined}>
    <Icon size={16} aria-hidden="true" className={state === "downloading" ? "tile-loading-icon" : undefined} />
    <span><bdi>{attachment.name}</bdi></span>
    {size && <small>{size}</small>}
    <p id={`rcs-media-status-${attachment.id}`} data-rcs-media-status>
      {copy.label}
      {copy.detail && <>{" "}<span id={`rcs-media-detail-${attachment.id}`}>{copy.detail}</span></>}
    </p>
    <RcsRetryControls messageId={messageId} attachment={attachment} attachmentIndex={attachmentIndex} />
  </div>;
}

export type RcsRetryPlaceholder = { index: number; state: RcsAttachmentState };

/** One "Retry all downloads" for a message with two or more retryable placeholders. Calls run one after another. */
export function RcsRetryAll({ messageId, placeholders }: { messageId: string; placeholders: RcsRetryPlaceholder[] }) {
  const actions = useRcsMediaActions();
  const [running, setRunning] = useState(false);
  const [failed, setFailed] = useState(false);
  if (!actions || placeholders.length < 2) return null;
  const blockNote = rcsRetryBlockCopy(actions.retryBlock);
  const disabled = running || blockNote !== undefined;
  const noteId = `rcs-media-retry-all-note-${messageId}`;
  const retryAll = async () => {
    if (disabled) return;
    setRunning(true);
    setFailed(false);
    const pending = placeholders.filter((placeholder) => actions.requestState(messageId, placeholder.index, placeholder.state) !== "requested");
    for (const placeholder of pending) {
      try {
        await actions.requestRetry(messageId, placeholder.index, placeholder.state);
      } catch {
        setFailed(true);
      }
    }
    setRunning(false);
  };
  return <div data-rcs-media-retry-all={messageId}>
    <button type="button" id={`rcs-media-retry-all-${messageId}`} className="secondary-button" aria-disabled={disabled || undefined} aria-busy={running || undefined} aria-describedby={blockNote ? noteId : undefined} onClick={() => void retryAll()}>
      {peppyCopy.rcs_media_retry_all}
    </button>
    {blockNote && <small id={noteId}>{blockNote}</small>}
    {failed && <p role="alert" data-rcs-media-retry-error>{peppyCopy.rcs_media_retry_error}</p>}
  </div>;
}
