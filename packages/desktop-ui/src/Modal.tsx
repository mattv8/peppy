import type { ReactNode, RefObject } from "react";
import { useEffect, useRef } from "react";

export type ModalProps = {
  id: string;
  title: ReactNode;
  children?: ReactNode;
  actions?: ReactNode;
  onDismiss(): void;
  busy?: boolean;
  /** Element to refocus on close. Defaults to the focused element at open; WebKit does not focus buttons on click, so pass the trigger. */
  opener?: RefObject<HTMLElement | null>;
  focusFallback?: RefObject<HTMLElement | null>;
  className?: string;
};

const focusedElement = () => document.activeElement instanceof HTMLElement && document.activeElement !== document.body ? document.activeElement : null;

/** Open a modal by conditionally mounting it; no `open` prop is required. */
export function Modal({
  id,
  title,
  children,
  actions,
  onDismiss,
  busy = false,
  opener,
  focusFallback,
  className,
}: ModalProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const titleRef = useRef<HTMLHeadingElement>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const isNativeModalRef = useRef(false);
  const isMountedRef = useRef(false);
  const busyRef = useRef(busy);
  const dismissRef = useRef(onDismiss);
  busyRef.current = busy;
  dismissRef.current = onDismiss;

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    isMountedRef.current = true;
    openerRef.current = opener?.current ?? focusedElement();
    isNativeModalRef.current = typeof dialog.showModal === "function";
    if (isNativeModalRef.current) {
      if (!dialog.open) dialog.showModal();
    } else dialog.open = true;
    titleRef.current?.focus();

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !isNativeModalRef.current && !busyRef.current) {
        dismissRef.current();
        return;
      }
      if (event.key !== "Tab") return;
      const focusable = [
        titleRef.current,
        ...Array.from(dialog.querySelectorAll<HTMLElement>("button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex=\"-1\"])")),
      ].filter((element): element is HTMLElement => element !== null);
      const first = focusable[0];
      const last = focusable.at(-1);
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last?.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first?.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => {
      isMountedRef.current = false;
      document.removeEventListener("keydown", onKeyDown);
      if (dialog.open) {
        if (isNativeModalRef.current) dialog.close();
        else dialog.open = false;
      }
      if (openerRef.current?.isConnected) openerRef.current.focus();
      else focusFallback?.current?.focus();
    };
  }, []);

  useEffect(() => {
    const active = document.activeElement;
    const focusLost = !dialogRef.current?.contains(active) || (active instanceof HTMLElement && active.matches(":disabled"));
    if (busy && focusLost) titleRef.current?.focus();
  }, [busy]);

  return <dialog
    id={id}
    ref={dialogRef}
    className="peppy-modal"
    aria-labelledby={`${id}-title`}
    aria-busy={busy || undefined}
    onCancel={(event) => {
      event.preventDefault();
      if (!busyRef.current) dismissRef.current();
    }}
    onClose={() => {
      // Chromium force-closes on a repeated Escape without firing cancel; keep React state and the dialog in sync.
      const dialog = dialogRef.current;
      if (!isMountedRef.current || !dialog || dialog.open) return;
      if (busyRef.current) dialog.showModal();
      else dismissRef.current();
    }}
  >
    <div className={className ? `peppy-modal-panel ${className}` : "peppy-modal-panel"} data-modal-panel>
      <h2 id={`${id}-title`} ref={titleRef} className="peppy-modal-title" tabIndex={-1}>{title}</h2>
      {children}
      {actions && <div className="peppy-modal-actions" data-modal-actions>{actions}</div>}
    </div>
  </dialog>;
}
