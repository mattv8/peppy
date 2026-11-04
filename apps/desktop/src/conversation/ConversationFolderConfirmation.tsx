import { useEffect, useLayoutEffect, useRef, type FocusEvent, type JSX, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { createPortal } from "react-dom";

type Props = { kind: "archived" | "spam"; name: string; trigger: HTMLButtonElement; pending: boolean; onConfirm(): void; onCancel(): void };

const VIEWPORT_MARGIN = 8;
const POPOVER_GAP = 6;
const QUESTION_ID = "conversation-folder-confirmation-question";
const NOTE_ID = "conversation-folder-confirmation-note";

export function ConversationFolderConfirmation({ kind, name, trigger, pending, onConfirm, onCancel }: Props): JSX.Element {
  const dialogRef = useRef<HTMLDivElement>(null);
  const primaryRef = useRef<HTMLButtonElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);

  const cancel = (restoreFocus = false) => {
    if (pending) return;
    onCancel();
    if (restoreFocus) requestAnimationFrame(() => {
      if (trigger.isConnected && !trigger.disabled) trigger.focus();
    });
  };

  useLayoutEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const row = trigger.closest<HTMLElement>("[data-conversation-row]") ?? trigger;
    const triggerRect = trigger.getBoundingClientRect();
    const rowRect = row.getBoundingClientRect();
    const width = dialog.offsetWidth;
    const height = dialog.offsetHeight;
    const right = rowRect.right - VIEWPORT_MARGIN;
    const left = Math.max(VIEWPORT_MARGIN, Math.min(right - width, window.innerWidth - width - VIEWPORT_MARGIN));
    const below = triggerRect.bottom + POPOVER_GAP;
    const above = triggerRect.top - POPOVER_GAP - height;
    const top = below + height > window.innerHeight - VIEWPORT_MARGIN && above >= VIEWPORT_MARGIN
      ? above
      : Math.max(VIEWPORT_MARGIN, below);
    Object.assign(dialog.style, { left: `${left}px`, top: `${top}px` });
    primaryRef.current?.focus();
  }, [trigger]);

  useEffect(() => {
    if (pending) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      cancel(true);
    };
    const onPointerDown = (event: PointerEvent) => {
      if (dialogRef.current?.contains(event.target as Node) || trigger.contains(event.target as Node)) return;
      cancel();
    };
    const onExternalChange = () => cancel();
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("scroll", onExternalChange, true);
    window.addEventListener("resize", onExternalChange);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("scroll", onExternalChange, true);
      window.removeEventListener("resize", onExternalChange);
    };
  }, [pending, trigger]);

  const onBlur = (event: FocusEvent<HTMLDivElement>) => {
    if (pending) return;
    const nextFocus = event.relatedTarget;
    if (nextFocus instanceof Node && (event.currentTarget.contains(nextFocus) || trigger.contains(nextFocus))) return;
    cancel();
  };
  // Focus leaves the dialog only through its two edges; the trap cancels and returns focus to the trigger, as Escape does.
  const onDialogKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Tab") return;
    const leavingFirst = event.shiftKey && document.activeElement === primaryRef.current;
    const leavingLast = !event.shiftKey && document.activeElement === cancelRef.current;
    if (!leavingFirst && !leavingLast) return;
    event.preventDefault();
    cancel(true);
  };
  const archive = kind === "archived";
  const message = archive ? `Archive the conversation with ${name} on all synced devices?` : `Mark ${name} as spam on all synced devices?`;
  const note = archive ? "It returns to Inbox when a new message arrives." : "It moves to Spam and stops desktop notifications. The number isn't blocked on your phone or reported to your carrier.";
  const confirmLabel = archive ? "Archive" : "Mark as spam";

  return createPortal(
    <div ref={dialogRef} id="conversation-folder-confirmation" role="dialog" aria-labelledby={QUESTION_ID} aria-describedby={NOTE_ID} style={{ maxHeight: "calc(100vh - 16px)" }} onBlur={onBlur} onKeyDown={onDialogKeyDown}>
      <p id={QUESTION_ID}>{message}</p>
      <p id={NOTE_ID} className="conversation-folder-confirmation-note">{note}</p>
      <div>
        <button ref={primaryRef} className="primary-button" type="button" disabled={pending} onClick={onConfirm}>{confirmLabel}</button>
        <button ref={cancelRef} className="secondary-button" type="button" disabled={pending} onClick={() => cancel()}>Cancel</button>
      </div>
    </div>,
    document.body,
  );
}
