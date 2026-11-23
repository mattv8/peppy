import type { JSX } from "react";
import { X } from "@peppy/desktop-ui";
import { useNoticeDismissal } from "../notices/useNoticeDismissal";

export type NotificationActionNotice = { kind: "info" | "error"; message: string };

export function NotificationActionNotice({ notice, onDismiss }: { notice?: NotificationActionNotice; onDismiss(): void }): JSX.Element {
  const { toastRef, setHovered, setFocused, trackBlur } = useNoticeDismissal(notice, onDismiss);
  const isError = notice?.kind === "error";
  return <div ref={toastRef} id="notifications-action-notice" className="mark-all-read-toast" data-placement="list"
    role={isError ? "alert" : "status"} aria-live={isError ? undefined : "polite"}
    onMouseEnter={() => setHovered(true)} onMouseLeave={() => setHovered(false)}
    onFocus={() => setFocused(true)} onBlur={trackBlur}>
    {notice && <>
      <span>{notice.message}</span>
      <button className="mark-all-read-toast-dismiss" type="button" aria-label="Dismiss notice" onClick={onDismiss}>
        <X size={14} strokeWidth={1.75} aria-hidden />
      </button>
    </>}
  </div>;
}
