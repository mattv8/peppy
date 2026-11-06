import { CheckCheck } from "@peppy/desktop-ui";
import type { MessageStatus } from "../bridge/bridge";
import { peppyCopy } from "../generated/peppyCopy";

type RcsMessageMetaProps = { messageId: string; status?: MessageStatus };

export function RcsMessageMeta({ messageId, status }: RcsMessageMetaProps) {
  if (status === "read") return <span className="message-meta-status" data-message-read={messageId}><CheckCheck size={10} aria-hidden />Read</span>;
  if (status === "failed-confirmed") return <p data-message-route-hint={messageId} data-hint="failed-reported">{peppyCopy.rcs_message_failed_reported}</p>;
  if (status === "unknown") return <p data-message-route-hint={messageId} data-hint="unknown">{peppyCopy.rcs_message_unknown_hint}</p>;
  return null;
}
