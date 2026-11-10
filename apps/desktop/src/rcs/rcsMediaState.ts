import type { AttachmentView, MessageView, RcsAttachmentState, RcsSendStatus } from "@peppy/desktop-ui/bridge-contracts";
import { peppyCopy } from "../generated/peppyCopy";
import type { RcsDownloadRetryBlock } from "./rcsRoute";

/** Same length as MEDIA_SYNC_RETRY_MS: a requested download retry stays "requested" for this long. */
export const RCS_RETRY_COOLDOWN_MS = 300_000;
const ATTACHMENT_LIMIT_LABEL = "32 MiB";

export type RcsPlaceholderState = Exclude<RcsAttachmentState, "stored">;

export type RcsDirection = "incoming" | "outgoing";

/** Incoming state that replaces the attachment cell. `stored` falls through to the normal rendering. */
export function rcsPlaceholderState(attachment: AttachmentView): RcsPlaceholderState | undefined {
  return attachment.rcsState && attachment.rcsState !== "stored" ? attachment.rcsState : undefined;
}

export function rcsDirection(attachment: AttachmentView): RcsDirection | undefined {
  if (attachment.rcsState) return "incoming";
  return attachment.rcsSendStatus ? "outgoing" : undefined;
}

/** Only a full store or a failed fetch can succeed on retry; too-large and expired files cannot. */
export function canRetryRcsDownload(state: RcsAttachmentState | undefined): boolean {
  return state === "quota_full" || state === "failed";
}

export function rcsPlaceholderCopy(state: RcsPlaceholderState): { label: string; detail?: string } {
  switch (state) {
    case "downloading":
      return { label: peppyCopy.rcs_media_state_downloading };
    case "too_large":
      return { label: peppyCopy.rcs_media_state_too_large, detail: peppyCopy.rcs_media_detail_too_large.replace("{limit}", ATTACHMENT_LIMIT_LABEL) };
    case "quota_full":
      return { label: peppyCopy.rcs_media_state_quota_full, detail: peppyCopy.rcs_media_detail_quota_full };
    case "failed":
      return { label: peppyCopy.rcs_media_state_failed, detail: peppyCopy.rcs_media_detail_failed };
    case "expired":
      return { label: peppyCopy.rcs_media_state_expired };
  }
}

export function rcsSendStatusLabel(status: RcsSendStatus): string {
  switch (status) {
    case "sending":
      return peppyCopy.rcs_media_state_sending;
    case "sent":
      return peppyCopy.rcs_media_state_sent;
    case "outcome_unknown":
      return peppyCopy.rcs_media_state_unknown;
    case "not_sent":
    case "members_changed":
      return peppyCopy.rcs_media_state_not_sent;
  }
}

/** Outgoing attachments whose outcome is not a clean success. */
export function rcsSendNeedsAttention(status: RcsSendStatus | undefined): boolean {
  return status === "not_sent" || status === "outcome_unknown" || status === "members_changed";
}

/** Zero-based attachment index for the retry command: the DTO value, else the position in the message. */
export function rcsAttachmentIndex(message: Pick<MessageView, "attachments">, attachment: AttachmentView): number {
  return attachment.index ?? message.attachments.findIndex((candidate) => candidate.id === attachment.id);
}

/** Message status is the worst of its attachments (U2): an unconfirmed or refused attachment never reads as "Sent". */
export function rcsOutcomeStatus(message: MessageView): MessageView {
  const statuses = message.attachments.map((attachment) => attachment.rcsSendStatus);
  if (!statuses.some((status) => status !== undefined)) return message;
  const status = statuses.includes("outcome_unknown")
    ? "unknown"
    : statuses.includes("not_sent")
      ? "failed-confirmed"
      : statuses.includes("members_changed")
        ? "failed-before-submit"
        : statuses.includes("sending") || statuses.includes(undefined)
          ? "submitted"
          : undefined;
  if (status === "submitted" && message.status !== "sent" && message.status !== "delivery-confirmed" && message.status !== "read") return message;
  return status && message.status !== status ? { ...message, status } : message;
}

export function rcsRetryBlockCopy(block: RcsDownloadRetryBlock): string | undefined {
  if (block.kind === "offline") return peppyCopy.rcs_media_retry_offline;
  if (block.kind === "link") return peppyCopy.rcs_media_retry_link;
  return undefined;
}

export function rcsRetryKey(messageId: string, attachmentIndex: number): string {
  return `${messageId}:${attachmentIndex}`;
}
