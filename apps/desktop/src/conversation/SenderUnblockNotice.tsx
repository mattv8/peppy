import type { JSX } from "react";
import { createPortal } from "react-dom";
import { UPDATE_PAUSED_DESCRIPTION, X } from "@peppy/desktop-ui";
import { useNoticeDismissal } from "../notices/useNoticeDismissal";

export type SenderUnblockNoticeState = { kind: "success" | "error"; id: string; name: string };
type Props = { notice?: SenderUnblockNoticeState; pending: boolean; writesPaused?: boolean; toastHost?: HTMLElement | null; onRetry(): void; onDismiss(): void };

export function SenderUnblockNotice({ notice, pending, writesPaused = false, toastHost, onRetry, onDismiss }: Props): JSX.Element {
  const { toastRef, setHovered, setFocused, trackBlur } = useNoticeDismissal(notice, onDismiss);
  const error = notice?.kind === "error";
  const copy = notice && (error ? `Couldn't unblock ${notice.name}.` : `Unblocking ${notice.name} on your phone.`);
  return createPortal(<div ref={toastRef} id="sender-unblock-notice" className="mark-all-read-toast" data-placement={toastHost ? "list" : "window"} role={error ? "alert" : "status"} aria-live={error ? undefined : "polite"} onMouseEnter={() => setHovered(true)} onMouseLeave={() => setHovered(false)} onFocus={() => setFocused(true)} onBlur={trackBlur}>
    {notice && <>{copy}{error && <button className="secondary-button" type="button" disabled={pending || writesPaused} title={writesPaused ? UPDATE_PAUSED_DESCRIPTION : undefined} aria-describedby={writesPaused ? "update-paused-description" : undefined} onClick={onRetry}>Retry</button>}<button className="mark-all-read-toast-dismiss" type="button" aria-label="Dismiss notification" disabled={pending} onClick={onDismiss}><X size={14} strokeWidth={1.75} aria-hidden /></button></>}
  </div>, toastHost ?? document.body);
}
