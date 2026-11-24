import { BellOff, UPDATE_PAUSED_DESCRIPTION, formatElapsedShort, isoDateTime } from "@peppy/desktop-ui";
import type { AppFilter, MirroredNotification, NotificationTarget } from "../bridge/bridge";
import type { OneTimeCodeTarget } from "@peppy/desktop-ui/bridge-contracts";
import { newestDismissTargets, NOTIFICATION_DISMISS_LIMIT } from "./dismissalOverlay";
import { notificationMuteKey } from "./notificationIdentity";
import { notificationKey, sectionNotifications, type NotificationStackData } from "./notificationSections";
import { NotificationAppIcon, NotificationRow, type NotificationLabels, type NotificationRowProps, type NotificationRowStatus } from "./NotificationRow";

type FeedProps = {
  labels: NotificationLabels;
  filtersByApp: Map<string, AppFilter>;
  newKeys: ReadonlySet<string>;
  showPhone: boolean;
  query: string;
  writesPaused?: boolean;
  onDismiss: NotificationRowProps["onDismiss"];
  onMute: NotificationRowProps["onMute"];
  onCopy: (target: OneTimeCodeTarget, code: string) => Promise<void>;
  onAction(keys: string[]): void;
  /** Status line for a row: a refusal or a failed dismissal. */
  statusFor?(notification: MirroredNotification): NotificationRowStatus | undefined;
};
const notificationCount = (count: number) => `${count} notification${count === 1 ? "" : "s"}`;

export const filterFor = (notification: MirroredNotification, filtersByApp: Map<string, AppFilter>): AppFilter =>
  filtersByApp.get(notificationMuteKey(notification.target.sourceDeviceId, notification.packageName)) ?? {
    sourceDeviceId: notification.target.sourceDeviceId,
    packageName: notification.packageName,
    appName: notification.appName,
    muted: false,
  };

export function NotificationSections({ notifications, ...props }: FeedProps & { notifications: MirroredNotification[] }) {
  const sections = sectionNotifications(notifications, Date.now());
  const showLabels = sections.length > 1;
  const showPhone = new Set(notifications.map(notification => notification.target.sourceDeviceId)).size > 1;
  return <>{sections.map(section => <section key={section.id} data-notification-section={section.id} aria-labelledby={`notification-section-${section.id}`} className="notif-section">
    <h2 id={`notification-section-${section.id}`} className={showLabels ? "notif-section-label" : "visually-hidden"}>
      {section.label}
    </h2>
    <ul data-notification-section-list={section.id}>{section.items.map(notification => <li key={notificationKey(notification.target)}><NotificationRow
      notification={notification}
      labels={props.labels}
      filter={filterFor(notification, props.filtersByApp)}
      unread={props.newKeys.has(notificationKey(notification.target))}
      showPhone={showPhone}
      query={props.query}
      writesPaused={props.writesPaused}
      onDismiss={props.onDismiss}
      onMute={props.onMute}
      onCopy={props.onCopy}
      onAction={props.onAction}
      status={props.statusFor?.(notification)}
    /></li>)}</ul>
  </section>)}</>;
}

export function NotificationStacks({
  stacks, expanded, focusStackKey, onExpanded, onDismissMany, ...props
}: FeedProps & {
  stacks: NotificationStackData[];
  expanded: ReadonlySet<string>;
  focusStackKey?: string;
  onExpanded(key: string): void;
  onDismissMany(targets: NotificationTarget[]): void;
}) {
  return <div data-notification-stacks>{stacks.map(stack => {
    const isExpanded = expanded.has(stack.key) || stack.key === focusStackKey;
    const items = stack.items.length === 1 || isExpanded ? stack.items : stack.items.slice(0, 2);
    const first = stack.items[0];
    const appName = props.labels.appName(first);
    const showPhone = props.showPhone;
    const filter = filterFor(first, props.filtersByApp);
    const eligible = stack.items.filter(item => item.dismissible && !item.dismissalPending);
    const dismissible = newestDismissTargets(eligible);
    const newCount = stack.items.filter(item => props.newKeys.has(notificationKey(item.target))).length;
    return <section key={stack.key} data-notification-stack={stack.key} className="notification-stack">
      {stack.items.length > 1 && <header className="notification-stack-header" data-notification-stack-header>
        <NotificationAppIcon appName={appName} base64={first.appIconPngBase64} />
        <div className="notification-stack-summary">
          <b>{appName}</b>
          {showPhone && <span className="notif-phone-badge">{props.labels.sourceName(first)}</span>}
          <span>{notificationCount(stack.items.length)}</span>
          {newCount > 0 && <span className="notification-count-badge">{newCount} new</span>}
        </div>
        <div className="notification-stack-actions">
          <time className="notif-row-time" dateTime={isoDateTime(first.postedAt)}>{formatElapsedShort(first.postedAt)}</time>
          <button type="button" className="notif-inline-action" aria-label={`Mute ${appName}${showPhone ? ` on ${props.labels.sourceName(first)}` : ""}`} disabled={props.writesPaused} title={props.writesPaused ? UPDATE_PAUSED_DESCRIPTION : undefined} aria-describedby={props.writesPaused ? "update-paused-description" : undefined} onClick={() => { props.onAction(stack.items.map(item => notificationKey(item.target))); props.onMute(filter); }}><BellOff size={14} aria-hidden /></button>
          <button type="button" className="notif-stack-dismiss-btn" disabled={!dismissible.length || props.writesPaused} title={props.writesPaused ? UPDATE_PAUSED_DESCRIPTION : undefined} aria-describedby={props.writesPaused ? "update-paused-description" : undefined} onClick={() => { props.onAction(dismissible.map(notificationKey)); onDismissMany(dismissible); }}>
            {eligible.length > NOTIFICATION_DISMISS_LIMIT ? `Dismiss up to ${NOTIFICATION_DISMISS_LIMIT}` : `Dismiss ${eligible.length}`}
          </button>
        </div>
      </header>}
      <ul data-notification-list data-notification-stack-list={stack.key}>{items.map(notification => <li key={notificationKey(notification.target)}><NotificationRow
        notification={notification}
        labels={props.labels}
        filter={filter}
        unread={props.newKeys.has(notificationKey(notification.target))}
        showPhone={showPhone && stack.items.length === 1}
        query={props.query}
        writesPaused={props.writesPaused}
        onDismiss={props.onDismiss}
        onMute={props.onMute}
        onCopy={props.onCopy}
        onAction={props.onAction}
        status={props.statusFor?.(notification)}
        hideAppName={stack.items.length > 1}
      /></li>)}</ul>
      {stack.items.length > 2 && <button type="button" className="notification-stack-toggle" aria-expanded={isExpanded} onClick={() => onExpanded(stack.key)}>
        {isExpanded ? "Show fewer" : `Show ${stack.items.length - 2} more`}
      </button>}
    </section>;
  })}</div>;
}
