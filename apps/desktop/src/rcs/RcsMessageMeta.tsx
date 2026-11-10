import { copySegments } from "../group/groupCopy";
import { CheckCheck } from "@peppy/desktop-ui";
import type { MessageStatus } from "../bridge/bridge";
import type { AttachmentView } from "@peppy/desktop-ui/bridge-contracts";
import { peppyCopy } from "../generated/peppyCopy";
import { canRetryRcsDownload, rcsAttachmentIndex } from "./rcsMediaState";
import { RcsRetryAll, type RcsRetryPlaceholder } from "./RcsMediaPlaceholder";

type RcsMessageMetaProps = {
  messageId: string;
  status?: MessageStatus;
  rcsReported?: boolean;
  statusLabel: string;
  /** Group outcome from core. Takes precedence over the generic status hints. */
  groupHint?: GroupMessageHint;
  /** Copies the failed body back into the composer. Nothing is sent automatically. */
  onEditResend?: () => void;
  /** Puts the unconfirmed create back in the composer behind its explicit confirmation. Nothing is sent. */
  onCreateAgain?: () => void;
  attachments?: AttachmentView[];
};

type GroupMessageHint = "members_changed" | "update_required" | "withdrawn" | "create_unknown" | "create_not_rcs";

function attachmentHint(attachment: AttachmentView): { hint: string; copy: string } {
  if (attachment.rcsSendStatus === "outcome_unknown") return { hint: "media-unknown", copy: peppyCopy.rcs_media_unknown_hint };
  if (attachment.error === "not_allowed_mime" || attachment.error === "rejected") return { hint: "media-refused", copy: peppyCopy.rcs_media_refused_hint };
  return { hint: "media-failed", copy: peppyCopy.rcs_media_failed_hint };
}

function membersChangedHint(messageId: string, onEditResend?: () => void) {
  return <div data-message-route-hint={messageId} data-hint="group-members-changed">
    <p>{peppyCopy.group_message_members_changed_hint}</p>
    {onEditResend && <button type="button" id={`group-message-edit-${messageId}`} className="secondary-button" onClick={onEditResend}>{peppyCopy.group_message_edit_resend}</button>}
  </div>;
}

export function RcsMessageMeta({ messageId, status, rcsReported = false, statusLabel, groupHint, onEditResend, onCreateAgain, attachments = [] }: RcsMessageMetaProps) {
  if (groupHint === "create_unknown") return <div data-message-route-hint={messageId} data-hint="group-create-unknown">
    <p>{peppyCopy.group_create_unconfirmed}</p>
    {onCreateAgain && <button type="button" id={`group-message-create-again-${messageId}`} className="secondary-button" onClick={onCreateAgain}>{peppyCopy.group_create_again_action}</button>}
  </div>;
  if (groupHint === "members_changed") return membersChangedHint(messageId, onEditResend);
  if (groupHint === "update_required") return <p data-message-route-hint={messageId} data-hint="group-update-required">{peppyCopy.group_update_required}</p>;
  if (groupHint === "withdrawn") return <p data-message-route-hint={messageId} data-hint="group-withdrawn">{peppyCopy.group_withdrawn_notice}</p>;
  if (groupHint === "create_not_rcs") return <p data-message-route-hint={messageId} data-hint="group-create-not-rcs">{peppyCopy.group_create_not_rcs_info}</p>;

  const rcsSent = attachments.filter((attachment) => attachment.rcsSendStatus);
  const sent = rcsSent.filter((attachment) => attachment.rcsSendStatus === "sent").length;
  const problems = rcsSent.filter((attachment) => attachment.rcsSendStatus === "not_sent" || attachment.rcsSendStatus === "outcome_unknown");
  const membersChanged = rcsSent.some((attachment) => attachment.rcsSendStatus === "members_changed");
  const unsent = problems.some((attachment) => attachment.rcsSendStatus === "not_sent");
  const retryPlaceholders = attachments.flatMap((attachment): RcsRetryPlaceholder[] => {
    const state = attachment.rcsState;
    return state && canRetryRcsDownload(state) ? [{ index: rcsAttachmentIndex({ attachments }, attachment), state }] : [];
  });
  if (problems.length || membersChanged || retryPlaceholders.length >= 2) return <div id={`rcs-media-issues-${messageId}`} data-rcs-media-issues={messageId}>
    {sent > 0 && problems.length > 0 && <p data-rcs-media-summary={messageId}>{peppyCopy.rcs_media_summary_partial.replace("{sent}", String(sent)).replace("{total}", String(rcsSent.length))}</p>}
    {problems.map((attachment) => {
      const { hint, copy } = attachmentHint(attachment);
      return <p key={attachment.id} data-message-route-hint={messageId} data-hint={hint}>{copySegments(copy, { name: attachment.name }).map((segment) => segment.value
        ? <bdi key={segment.offset} title={segment.text} data-rcs-media-filename>{segment.text.length > 48 ? `${segment.text.slice(0, 35)}…${segment.text.slice(-12)}` : segment.text}</bdi>
        : <span key={segment.offset}>{segment.text}</span>)}</p>;
    })}
    {membersChanged && membersChangedHint(messageId, onEditResend)}
    {unsent && !membersChanged && onEditResend && <button type="button" id={`rcs-media-resend-${messageId}`} className="secondary-button" onClick={onEditResend}>{sent > 0 ? peppyCopy.rcs_media_resend_rest : peppyCopy.group_message_edit_resend}</button>}
    <RcsRetryAll messageId={messageId} placeholders={retryPlaceholders} />
  </div>;
  if (status === "read") return <span className="message-meta-status" data-message-read={messageId}><CheckCheck size={10} aria-hidden />{statusLabel}</span>;
  if (status === "failed-confirmed") return rcsReported
    ? <p data-message-route-hint={messageId} data-hint="failed-reported">{peppyCopy.rcs_message_failed_reported}</p>
    : <p data-message-route-hint={messageId} data-hint="failed">{peppyCopy.rcs_message_failed_unreported}</p>;
  if (status === "unknown") return <p data-message-route-hint={messageId} data-hint="unknown">{peppyCopy.rcs_message_unknown_hint}</p>;
  return null;
}
