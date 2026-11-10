import { CheckCheck } from "@peppy/desktop-ui";
import type { MessageStatus } from "../bridge/bridge";
import { peppyCopy } from "../generated/peppyCopy";

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
};

type GroupMessageHint = "members_changed" | "update_required" | "withdrawn" | "create_unknown" | "create_not_rcs";

export function RcsMessageMeta({ messageId, status, rcsReported = false, statusLabel, groupHint, onEditResend, onCreateAgain }: RcsMessageMetaProps) {
  if (groupHint === "create_unknown") return <div data-message-route-hint={messageId} data-hint="group-create-unknown">
    <p>{peppyCopy.group_create_unconfirmed}</p>
    {onCreateAgain && <button type="button" id={`group-message-create-again-${messageId}`} className="secondary-button" onClick={onCreateAgain}>{peppyCopy.group_create_again_action}</button>}
  </div>;
  if (groupHint === "members_changed") return <div data-message-route-hint={messageId} data-hint="group-members-changed">
    <p>{peppyCopy.group_message_members_changed_hint}</p>
    {onEditResend && <button type="button" id={`group-message-edit-${messageId}`} className="secondary-button" onClick={onEditResend}>{peppyCopy.group_message_edit_resend}</button>}
  </div>;
  if (groupHint === "update_required") return <p data-message-route-hint={messageId} data-hint="group-update-required">{peppyCopy.group_update_required}</p>;
  if (groupHint === "withdrawn") return <p data-message-route-hint={messageId} data-hint="group-withdrawn">{peppyCopy.group_withdrawn_notice}</p>;
  if (groupHint === "create_not_rcs") return <p data-message-route-hint={messageId} data-hint="group-create-not-rcs">{peppyCopy.group_create_not_rcs_info}</p>;
  if (status === "read") return <span className="message-meta-status" data-message-read={messageId}><CheckCheck size={10} aria-hidden />{statusLabel}</span>;
  if (status === "failed-confirmed") return rcsReported
    ? <p data-message-route-hint={messageId} data-hint="failed-reported">{peppyCopy.rcs_message_failed_reported}</p>
    : <p data-message-route-hint={messageId} data-hint="failed">{peppyCopy.rcs_message_failed_unreported}</p>;
  if (status === "unknown") return <p data-message-route-hint={messageId} data-hint="unknown">{peppyCopy.rcs_message_unknown_hint}</p>;
  return null;
}
