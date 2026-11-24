import { useState, type ReactElement } from "react";
import { Smartphone } from "@peppy/desktop-ui";
import type { MirroredNotification } from "../bridge/bridge";
import type { AppFilter } from "../bridge/bridge";
import { BLOCKER_GROUP_NOTE, WAITING_GROUP_NOTE, waitingGroupHeader } from "./notificationDismissCopy";
import { notificationKey } from "./notificationSections";
import { filterFor } from "./NotificationStack";
import { NotificationRow, type NotificationRowProps } from "./NotificationRow";

export type WaitingGroup = {
  phoneKey: string;
  phoneName: string;
  notifications: MirroredNotification[];
};

/**
 * Row props shared by every group. Each row takes its own app filter from `filtersByApp`. Dismissed
 * rows always read as read: the user has already acted on them.
 */
export type WaitingGroupRowProps = Omit<NotificationRowProps, "notification" | "unread" | "status" | "locked" | "filter"> & {
  filtersByApp: Map<string, AppFilter>;
};

export type NotificationWaitingGroupsProps = {
  source: "blocker" | "count";
  groups: WaitingGroup[];
  rowProps: WaitingGroupRowProps;
  /** Phones whose reveal is closed: an empty placeholder keeps each toggle's `aria-controls` target in the page. */
  closedPhoneKeys?: string[];
};

const PREVIEW_COUNT = 3;

function WaitingGroupSection({ source, group, rowProps }: { source: "blocker" | "count"; group: WaitingGroup; rowProps: WaitingGroupRowProps }): ReactElement {
  const [showAll, setShowAll] = useState(false);
  const { filtersByApp, ...row } = rowProps;
  const { phoneKey, phoneName, notifications } = group;
  const hiddenCount = notifications.length - PREVIEW_COUNT;
  const visible = showAll ? notifications : notifications.slice(0, PREVIEW_COUNT);
  const note = source === "blocker" ? BLOCKER_GROUP_NOTE(phoneName) : WAITING_GROUP_NOTE(phoneName);
  return <section id={`notifications-hidden-${phoneKey}`} className="notification-hidden-group" data-hidden-group data-hidden-source={source}>
    <header className="notification-hidden-group-header">
      <Smartphone size={14} aria-hidden />
      <div>
        <strong>{waitingGroupHeader(phoneName)}</strong>
        <span className="notification-hidden-group-note">{note}</span>
      </div>
    </header>
    <ul id={`notifications-hidden-${phoneKey}-list`} className="notification-hidden-list">
      {visible.map(notification => <li key={notificationKey(notification.target)}>
        <NotificationRow {...row} notification={notification} filter={filterFor(notification, filtersByApp)} unread={false} locked status={{ kind: "waiting", phoneName }} />
      </li>)}
    </ul>
    {hiddenCount > 0 && <button id={`notifications-hidden-${phoneKey}-more`} type="button" className="notification-stack-toggle" aria-expanded={showAll} aria-controls={`notifications-hidden-${phoneKey}-list`} onClick={() => setShowAll(!showAll)}>
      {showAll ? "Show fewer" : `Show ${hiddenCount} more`}
    </button>}
  </section>;
}

/** Locked groups of dismissed notifications waiting for a phone. Each row offers Mute only. The wrapper is always present, so toggles can point at it. */
export function NotificationWaitingGroups({ source, groups, rowProps, closedPhoneKeys = [] }: NotificationWaitingGroupsProps): ReactElement {
  const id = source === "blocker" ? "notifications-blocker-groups" : "notifications-waiting-groups";
  return <div id={id} className="notification-waiting-groups" data-waiting-groups={source}>
    {groups.map(group => <WaitingGroupSection key={group.phoneKey} source={source} group={group} rowProps={rowProps} />)}
    {closedPhoneKeys.map(key => <div key={key} id={`notifications-hidden-${key}`} hidden />)}
  </div>;
}
