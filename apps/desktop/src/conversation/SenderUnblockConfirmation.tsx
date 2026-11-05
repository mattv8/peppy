import { useEffect, useLayoutEffect, useRef, type FocusEvent, type JSX, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { createPortal } from "react-dom";

type Props = { name: string; canUnblock: boolean; trigger: HTMLButtonElement; pending: boolean; onConfirm(): void; onCancel(): void };

const VIEWPORT_MARGIN = 8;
const POPOVER_GAP = 6;
const QUESTION_ID = "sender-unblock-confirmation-question";
const NOTE_ID = "sender-unblock-confirmation-note";

export function SenderUnblockConfirmation({ name, canUnblock, trigger, pending, onConfirm, onCancel }: Props): JSX.Element {
  const dialogRef = useRef<HTMLDivElement>(null);
  const primaryRef = useRef<HTMLButtonElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);
  const cancel = (restoreFocus = false) => {
    if (pending) return;
    onCancel();
    if (restoreFocus) requestAnimationFrame(() => { if (trigger.isConnected && !trigger.disabled) trigger.focus(); });
  };
  useLayoutEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const row = trigger.closest<HTMLElement>("[data-conversation-row]") ?? trigger;
    const triggerRect = trigger.getBoundingClientRect();
    const rowRect = row.getBoundingClientRect();
    const width = dialog.offsetWidth;
    const height = dialog.offsetHeight;
    const left = Math.max(VIEWPORT_MARGIN, Math.min(rowRect.right - VIEWPORT_MARGIN - width, window.innerWidth - width - VIEWPORT_MARGIN));
    const below = triggerRect.bottom + POPOVER_GAP;
    const above = triggerRect.top - POPOVER_GAP - height;
    Object.assign(dialog.style, { left: `${left}px`, top: `${below + height > window.innerHeight - VIEWPORT_MARGIN && above >= VIEWPORT_MARGIN ? above : Math.max(VIEWPORT_MARGIN, below)}px` });
    primaryRef.current?.focus();
  }, [trigger]);
  useEffect(() => {
    if (pending) return;
    const onKeyDown = (event: KeyboardEvent) => { if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); cancel(true); } };
    const onPointerDown = (event: PointerEvent) => { if (!dialogRef.current?.contains(event.target as Node) && !trigger.contains(event.target as Node)) cancel(); };
    const onExternalChange = () => cancel();
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("scroll", onExternalChange, true);
    window.addEventListener("resize", onExternalChange);
    return () => { document.removeEventListener("keydown", onKeyDown); document.removeEventListener("pointerdown", onPointerDown); document.removeEventListener("scroll", onExternalChange, true); window.removeEventListener("resize", onExternalChange); };
  }, [pending, trigger]);
  const onBlur = (_event: FocusEvent<HTMLDivElement>) => {
    requestAnimationFrame(() => {
      const next = document.activeElement;
      if (!pending && !(next instanceof Node && (dialogRef.current?.contains(next) || trigger.contains(next)))) cancel();
    });
  };
  const onDialogKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (event.key !== "Tab") return;
    const leavingFirst = event.shiftKey && document.activeElement === primaryRef.current;
    const leavingLast = !event.shiftKey && document.activeElement === (cancelRef.current ?? primaryRef.current);
    if (!leavingFirst && !leavingLast) return;
    event.preventDefault();
    cancel(true);
  };
  const question = canUnblock ? `Unblock ${name} on your phone?` : `Your phone can't unblock ${name} yet.`;
  const note = canUnblock ? "Your phone removes this number from its blocked list. New texts from it arrive again, and this conversation moves to Inbox." : "Turn on Advanced features in Peppy on your phone, or unblock this number in your messaging app.";
  return createPortal(<div ref={dialogRef} id="sender-unblock-confirmation" role="dialog" aria-label="Confirm unblock" aria-labelledby={QUESTION_ID} aria-describedby={NOTE_ID} style={{ maxHeight: "calc(100vh - 16px)" }} onBlur={onBlur} onKeyDown={onDialogKeyDown}>
    <p id={QUESTION_ID}>{question}</p>
    <p id={NOTE_ID} className="conversation-folder-confirmation-note">{note}</p>
    <div>{canUnblock && <button ref={primaryRef} className="primary-button" type="button" disabled={pending} onClick={onConfirm}>Unblock</button>}<button ref={canUnblock ? cancelRef : primaryRef} className="secondary-button" type="button" disabled={pending} onClick={() => cancel(true)}>{canUnblock ? "Cancel" : "Close"}</button></div>
  </div>, document.body);
}
