import type { JSX } from "react";
import { UPDATE_PAUSED_DESCRIPTION, X } from "@peppy/desktop-ui";
import { useNoticeDismissal } from "../notices/useNoticeDismissal";

export type NotificationActionNotice = {
  kind: "info" | "error";
  message: string;
  /** Keys of the rows the notice is about; focus moves from them when the action runs. */
  keys?: string[];
  action?: { label: string; onAction(): void; /** True while writes are paused: the action stays visible, explained, and inert. */ disabled?: boolean };
};

export function NotificationActionNotice({ notice, onDismiss, onClosed }: { notice?: NotificationActionNotice; onDismiss(): void; /** Runs after the close control dismisses the notice. */ onClosed?(): void }): JSX.Element {
  const { toastRef, setHovered, setFocused, trackBlur } = useNoticeDismissal(notice, onDismiss);
  const isError = notice?.kind === "error";
  const action = notice?.action;
  return <div ref={toastRef} id="notifications-action-notice" className="mark-all-read-toast" data-placement="list"
    role={isError ? "alert" : "status"} aria-live={isError ? undefined : "polite"}
    onMouseEnter={() => setHovered(true)} onMouseLeave={() => setHovered(false)}
    onFocus={() => setFocused(true)} onBlur={trackBlur}>
    {notice && <>
      <span>{notice.message}</span>
      {action && <button id="notifications-action-notice-action" className="secondary-button notification-notice-action" type="button"
        disabled={action.disabled} title={action.disabled ? UPDATE_PAUSED_DESCRIPTION : undefined}
        aria-describedby={action.disabled ? "update-paused-description" : undefined} onClick={action.onAction}>{action.label}</button>}
      <button className="mark-all-read-toast-dismiss" type="button" aria-label="Dismiss notice" onClick={() => { onDismiss(); onClosed?.(); }}>
        <X size={14} strokeWidth={1.75} aria-hidden />
      </button>
    </>}
  </div>;
}
