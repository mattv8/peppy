import { createContext, useContext, useEffect, useLayoutEffect, useRef, type ReactElement } from "react";
import type { MessageView, SendRecoveryKind } from "@peppy/desktop-ui/bridge-contracts";
import { peppyCopy } from "../generated/peppyCopy";

export type SendAgainOutcome = { kind: "sent"; messageId: string } | { kind: "already" } | { kind: "failed"; code: string };

/**
 * Host-level state for Send again. It outlives one popover so that an in-flight call, the last error and
 * a sent-again result survive closing, reopening and another message's popover.
 */
export type SendAgainStore = {
  open(message: MessageView, trigger: HTMLElement): void;
  close(messageId: string): void;
  /** Moves to the new message row, or scrolls to the live tail when the row is not rendered. */
  showSentAgain(messageId: string): void;
  sentAgainFor(messageId: string): string | undefined;
  isPending(messageId: string): boolean;
  errorFor(messageId: string): string | undefined;
  /** Returns false when a call for this message is already in flight. */
  startCall(messageId: string): boolean;
  settleCall(messageId: string, outcome: SendAgainOutcome): void;
  requestDoneFocus(messageId: string): void;
  consumeDoneFocus(messageId: string): boolean;
};

export const SendAgainStoreContext = createContext<SendAgainStore | null>(null);

export const SEND_RECOVERY_STATUS: Record<SendRecoveryKind, { label: string; tone: "error" | "warning" }> = {
  "not-sent": { label: peppyCopy.message_status_not_sent, tone: "error" },
  "unconfirmed-removed": { label: peppyCopy.message_status_unconfirmed, tone: "warning" },
  "unconfirmed-removed-cancel": { label: peppyCopy.message_status_unconfirmed, tone: "warning" },
};

export const SEND_RECOVERY_REASON: Record<SendRecoveryKind, string> = {
  "not-sent": peppyCopy.message_not_sent_hint,
  "unconfirmed-removed": peppyCopy.message_unconfirmed_removed_hint,
  "unconfirmed-removed-cancel": peppyCopy.message_unconfirmed_cancel_hint,
};

/** Actions for a recovery state or an ungrouped sent-again message. */
export function SendRecoveryNotice({ message }: { message: MessageView }): ReactElement | null {
  const store = useContext(SendAgainStoreContext);
  const doneRef = useRef<HTMLDivElement>(null);
  const storeRef = useRef(store);
  storeRef.current = store;
  useLayoutEffect(() => () => {
    if (doneRef.current?.contains(document.activeElement)) storeRef.current?.requestDoneFocus(message.id);
  }, [message.id]);
  const recovery = message.sendRecovery;
  const sentAgainId = message.sentAgain?.messageId ?? recovery?.sentAgain?.messageId ?? store?.sentAgainFor(message.id);
  useEffect(() => {
    if (sentAgainId !== undefined && store?.consumeDoneFocus(message.id)) doneRef.current?.focus();
  }, [message.id, sentAgainId, store]);
  if (!recovery && sentAgainId === undefined) return null;
  const canSendAgain = recovery?.canSendAgain && sentAgainId === undefined;
  return <div data-message-recovery={message.id} data-recovery-kind={recovery?.kind}>
    {sentAgainId !== undefined ? <div ref={doneRef} data-message-send-again-done={message.id} id={`message-send-again-done-${message.id}`} tabIndex={-1}>
      <span className="message-sent-again-fact">{peppyCopy.message_send_again_sent}</span>
      <button type="button" id={`message-send-again-show-${message.id}`} className="message-meta-action message-meta-text-action" aria-label={peppyCopy.message_send_again_show_label} onClick={() => store?.showSentAgain(sentAgainId)}>{peppyCopy.message_send_again_show}</button>
    </div> : canSendAgain && <button type="button" id={`message-send-again-${message.id}`} className="message-meta-action message-meta-text-action" onClick={(event) => store?.open(message, event.currentTarget)}>{peppyCopy.message_send_again_action}</button>}
  </div>;
}
