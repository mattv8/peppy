import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type RefObject } from "react";
import { createPortal } from "react-dom";
import type { DraftNoticeAction, DraftNoticeView } from "./draftNoticeModel";
import "./draftInlineStatus.css";

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

const VIEWPORT_MARGIN = 8;
const POPOVER_GAP = 6;

function DraftDiscardConfirmation({ opener, onCancel, onConfirm, toolbar }: {
  opener: RefObject<HTMLButtonElement | null>;
  onCancel(): void;
  onConfirm(): void;
  toolbar: HTMLElement | null;
}) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  const [position, setPosition] = useState<CSSProperties>();
  const host = toolbar?.closest<HTMLElement>(".theme-light, .theme-dark, .theme-system") ?? document.body;

  useLayoutEffect(() => {
    const updatePosition = () => {
      const rect = toolbar?.getBoundingClientRect();
      if (!rect) return;
      setPosition({
        bottom: `${window.innerHeight - rect.top + POPOVER_GAP}px`,
        left: `${Math.max(VIEWPORT_MARGIN, rect.left + VIEWPORT_MARGIN)}px`,
        width: `${Math.min(320, window.innerWidth - VIEWPORT_MARGIN * 2)}px`,
      });
    };
    updatePosition();
    window.addEventListener("resize", updatePosition);
    cancelRef.current?.focus();
    return () => window.removeEventListener("resize", updatePosition);
  }, [toolbar]);

  const cancel = () => {
    onCancel();
    requestAnimationFrame(() => opener.current?.focus());
  };

  return createPortal(
    <div data-draft-discard-confirmation role="alertdialog" aria-label="Discard local changes?" style={position} onKeyDown={(event) => {
      if (event.key === "Escape") cancel();
    }}>
      <strong>Discard local changes?</strong>
      <span>This permanently discards the unsaved text and attachment list in this window. A draft saved from another window is not affected.</span>
      <div data-draft-discard-actions>
        <button ref={cancelRef} type="button" data-draft-discard-cancel onClick={cancel}>Cancel</button>
        <button type="button" data-draft-discard-confirm onClick={onConfirm}>Discard local changes</button>
      </div>
    </div>,
    host,
  );
}

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
  const noticeRef = useRef<HTMLDivElement>(null);
  const discardOpenerRef = useRef<HTMLButtonElement>(null);
  useEffect(() => setConfirmingDiscard(false), [noticeKey]);
  if (!notice || (pending && !pendingVisible)) return null;

  const action = notice.action;
  const label = action && ACTION_LABELS[action];
  return (
    <div
      ref={noticeRef}
      key={notice.kind}
      id="draft-notice"
      data-draft-notice={notice.kind}
      data-kind={notice.kind}
      data-draft-error={notice.errorCode}
      data-tone={notice.tone}
      role={notice.role}
    >
      <span className="draft-notice-message" title={notice.message}>{notice.message}</span>
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
          ref={discardOpenerRef}
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
        <DraftDiscardConfirmation
          opener={discardOpenerRef}
          toolbar={noticeRef.current?.closest<HTMLElement>("#composer-toolbar") ?? null}
          onCancel={() => setConfirmingDiscard(false)}
          onConfirm={() => onAction("discard-local")}
        />
      )}
    </div>
  );
}
