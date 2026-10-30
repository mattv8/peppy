import { useEffect, useRef, useState } from "react";
import type { DraftNoticeAction, DraftNoticeView } from "./draftNoticeModel";

export const DRAFT_NOTICE_DELAY_MS = 600;

const ACTION_LABELS: Record<DraftNoticeAction, { caption: string; accessibleName?: string }> = {
  retry: { caption: "Retry" },
  "save-new": { caption: "Save as new draft" },
  "discard-local": { caption: "Discard local changes" },
  undo: { caption: "Undo" },
  copy: { caption: "Copy text", accessibleName: "Copy text of retained draft" },
};

export type DraftNoticeProps = {
  notice: DraftNoticeView | undefined;
  onAction: (action: DraftNoticeAction) => void;
  busy?: boolean;
  noticeKey?: string;
};

// `key` names the pending state; changing it restarts the delay.
export function useDelayedFlag(active: boolean, delayMs: number, key = ""): boolean {
  const [elapsedKey, setElapsedKey] = useState<string>();
  useEffect(() => {
    if (!active) return undefined;
    const timer = window.setTimeout(() => setElapsedKey(key), delayMs);
    return () => {
      window.clearTimeout(timer);
      setElapsedKey(undefined);
    };
  }, [active, delayMs, key]);
  return active && elapsedKey === key;
}

export function DraftNotice({ notice, onAction, busy = false, noticeKey = "" }: DraftNoticeProps) {
  const pending = notice?.kind === "saving" || notice?.kind === "loading";
  const pendingVisible = useDelayedFlag(pending, DRAFT_NOTICE_DELAY_MS, notice?.kind);
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);
  const cancelDiscardRef = useRef<HTMLButtonElement>(null);
  useEffect(() => setConfirmingDiscard(false), [noticeKey]);
  useEffect(() => {
    if (confirmingDiscard) cancelDiscardRef.current?.focus();
  }, [confirmingDiscard]);
  if (!notice || (pending && !pendingVisible)) return null;

  const action = notice.action;
  const label = action && ACTION_LABELS[action];
  return (
    <div
      key={notice.kind}
      id="draft-notice"
      data-draft-notice={notice.kind}
      data-kind={notice.kind}
      data-draft-error={notice.errorCode}
      data-tone={notice.tone}
      role={notice.role}
    >
      <span className="draft-notice-message">{notice.message}</span>
      {action && label && (
        <button
          id="draft-notice-action"
          type="button"
          aria-label={label.accessibleName}
          aria-disabled={busy || undefined}
          data-draft-action={action}
          onClick={() => {
            if (!busy) onAction(action);
          }}
        >
          {label.caption}
        </button>
      )}
      {notice.secondaryAction && !confirmingDiscard && (
        <button
          type="button"
          aria-disabled={busy || undefined}
          data-draft-action={notice.secondaryAction}
          onClick={() => {
            if (!busy) setConfirmingDiscard(true);
          }}
        >
          Discard local changes
        </button>
      )}
      {confirmingDiscard && (
        <div
          data-draft-discard-confirmation
          role="alertdialog"
          aria-label="Discard local changes?"
          onKeyDown={(event) => {
            if (event.key === "Escape") setConfirmingDiscard(false);
          }}
        >
          <strong>Discard local changes?</strong>
          <span>This permanently discards the unsaved text and attachment list in this window. A draft saved from another window is not affected.</span>
          <button ref={cancelDiscardRef} type="button" data-draft-discard-cancel onClick={() => setConfirmingDiscard(false)}>Cancel</button>
          <button type="button" data-draft-discard-confirm onClick={() => onAction("discard-local")}>Discard local changes</button>
        </div>
      )}
    </div>
  );
}
