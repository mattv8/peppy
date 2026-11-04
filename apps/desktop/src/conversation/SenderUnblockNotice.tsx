import { useEffect, useRef, useState, type JSX } from "react";
import { createPortal } from "react-dom";
import { X } from "@peppy/desktop-ui";
import { TOAST_DISMISS_MS } from "./MarkAllReadAction";

export type SenderUnblockNoticeState = { kind: "success" | "error"; id: string; name: string };
type Props = { notice?: SenderUnblockNoticeState; pending: boolean; toastHost?: HTMLElement | null; onRetry(): void; onDismiss(): void };

export function SenderUnblockNotice({ notice, pending, toastHost, onRetry, onDismiss }: Props): JSX.Element {
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const toastRef = useRef<HTMLDivElement>(null);
  const dismissRef = useRef(onDismiss);
  dismissRef.current = onDismiss;
  useEffect(() => {
    setHovered(Boolean(toastRef.current?.matches(":hover")));
    setFocused(Boolean(toastRef.current?.contains(document.activeElement)));
  }, [notice]);
  useEffect(() => {
    if (!notice || notice.kind !== "success" || hovered || focused) return;
    const timer = window.setTimeout(() => dismissRef.current(), TOAST_DISMISS_MS);
    return () => window.clearTimeout(timer);
  }, [focused, hovered, notice]);
  const trackBlur = () => requestAnimationFrame(() => setFocused(Boolean(toastRef.current?.contains(document.activeElement))));
  const error = notice?.kind === "error";
  const copy = notice && (error ? `Couldn't unblock ${notice.name}.` : `Unblocking ${notice.name} on your phone.`);
  return createPortal(<div ref={toastRef} id="sender-unblock-notice" className="mark-all-read-toast" data-placement={toastHost ? "list" : "window"} role={error ? "alert" : "status"} aria-live={error ? undefined : "polite"} onMouseEnter={() => setHovered(true)} onMouseLeave={() => setHovered(false)} onFocus={() => setFocused(true)} onBlur={trackBlur}>
    {notice && <>{copy}{error && <button className="secondary-button" type="button" disabled={pending} onClick={onRetry}>Retry</button>}<button className="mark-all-read-toast-dismiss" type="button" aria-label="Dismiss notification" disabled={pending} onClick={onDismiss}><X size={14} strokeWidth={1.75} aria-hidden /></button></>}
  </div>, toastHost ?? document.body);
}
