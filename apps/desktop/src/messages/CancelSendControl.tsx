import type { ReactElement } from "react";
import { X } from "@peppy/desktop-ui";

export type CancelSendControlProps = {
  messageId: string;
  onCancel: () => void;
  disabled?: boolean;
};

export function CancelSendControl({ messageId, onCancel, disabled }: CancelSendControlProps): ReactElement {
  return (
    <button
      type="button"
      data-message-cancel={messageId}
      aria-label="Cancel send and remove message"
      disabled={disabled}
      onClick={onCancel}
    >
      <X size={14} aria-hidden />
    </button>
  );
}

export function SendCancellationPending({ messageId, waitingForPhone }: {
  messageId: string;
  waitingForPhone?: boolean;
}): ReactElement {
  return (
    <div data-message-cancellation={messageId} role="status" aria-atomic="true" tabIndex={-1}>
      <span>Cancelling…</span>
      {waitingForPhone && <span>Waiting for phone</span>}
    </div>
  );
}
