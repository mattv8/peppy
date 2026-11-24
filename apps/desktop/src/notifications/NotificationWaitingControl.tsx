import type { ReactElement } from "react";
import { ChevronDown, Clock } from "@peppy/desktop-ui";
import { NotificationDismissHelp } from "./NotificationDismissHelp";
import { WAITING_GROUP_LABEL, WAITING_HELP_LABEL } from "./notificationDismissCopy";

export type NotificationWaitingControlProps = {
  count: number;
  expanded: boolean;
  onToggle(): void;
};

/** Toolbar control for dismissed notifications that wait for a phone. Renders nothing at zero. */
export function NotificationWaitingControl({ count, expanded, onToggle }: NotificationWaitingControlProps): ReactElement | null {
  if (count === 0) return null;
  return <div id="notifications-pending-count" role="group" aria-label={WAITING_GROUP_LABEL} data-pending-count={count} data-pending-expanded={expanded}>
    <button id="notifications-pending-toggle" type="button" className="notification-pending-toggle" aria-expanded={expanded} aria-controls="notifications-waiting-groups" onClick={onToggle}>
      <Clock size={14} aria-hidden />
      <span className="notification-pending-count-number">{count}</span>{" "}
      <span className="notification-pending-label">waiting for phone</span>
      <ChevronDown size={14} aria-hidden className="notification-pending-chevron" data-expanded={expanded || undefined} />
    </button>
    <NotificationDismissHelp triggerId="notifications-pending-help" triggerLabel={WAITING_HELP_LABEL} help={{ kind: "waiting", count }} />
  </div>;
}
