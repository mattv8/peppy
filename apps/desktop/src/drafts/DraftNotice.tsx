import { useEffect, useState } from "react";
import type { DraftNoticeAction, DraftNoticeView } from "./draftNoticeModel";

export const DRAFT_NOTICE_DELAY_MS = 600;

const ACTION_LABELS: Record<DraftNoticeAction, { caption: string; accessibleName?: string }> = {
  retry: { caption: "Retry" },
  undo: { caption: "Undo" },
  copy: { caption: "Copy text", accessibleName: "Copy text of retained draft" },
};

export type DraftNoticeProps = {
  notice: DraftNoticeView | undefined;
  onAction: (action: DraftNoticeAction) => void;
  busy?: boolean;
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

export function DraftNotice({ notice, onAction, busy = false }: DraftNoticeProps) {
  const pending = notice?.kind === "saving" || notice?.kind === "loading";
  const pendingVisible = useDelayedFlag(pending, DRAFT_NOTICE_DELAY_MS, notice?.kind);
  if (!notice || (pending && !pendingVisible)) return null;

  const action = notice.action;
  const label = action && ACTION_LABELS[action];
  return (
    <div
      key={notice.kind}
      id="draft-notice"
      data-draft-notice={notice.kind}
      data-kind={notice.kind}
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
          onClick={() => {
            if (!busy) onAction(action);
          }}
        >
          {label.caption}
        </button>
      )}
    </div>
  );
}
