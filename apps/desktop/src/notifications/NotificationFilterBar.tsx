import { CheckCheck, Clock, Layers } from "@peppy/desktop-ui";
import type { MirroredNotification } from "../bridge/bridge";
import { NotificationAppIcon } from "./NotificationRow";

export type NotificationFilterChip = {
  packageName: string;
  name: string;
  icon?: string;
  count: number;
};

const notificationCount = (count: number) => `${count} notification${count === 1 ? "" : "s"}`;

export function notificationFilterChips(
  notifications: MirroredNotification[],
  appName: (notification: MirroredNotification) => string,
): NotificationFilterChip[] {
  const chips = new Map<string, NotificationFilterChip>();
  for (const notification of notifications) {
    const chip = chips.get(notification.packageName);
    if (chip) chip.count += 1;
    else chips.set(notification.packageName, {
      packageName: notification.packageName,
      name: appName(notification),
      icon: notification.appIconPngBase64,
      count: 1,
    });
  }
  return [...chips.values()].sort((left, right) => right.count - left.count || left.name.localeCompare(right.name));
}

export function NotificationFilterBar({
  chips, appFilter, onAppFilter, arrangement, onArrangement, dismissAll,
}: {
  chips: NotificationFilterChip[];
  appFilter: string | null;
  onAppFilter(packageName: string | null): void;
  arrangement: "time" | "app";
  onArrangement(arrangement: "time" | "app"): void;
  dismissAll?: { label: string; disabled: boolean; onDismissAll(): void };
}) {
  return <div id="notifications-toolbar">
    {(chips.length >= 2 || appFilter !== null) && <div id="notifications-filter-bar" role="group" aria-label="Filter by app" className="notification-filter-chips">
      <button type="button" aria-pressed={appFilter === null} onClick={() => onAppFilter(null)}>All</button>
      {chips.map(chip => <button
        type="button"
        key={chip.packageName}
        aria-pressed={appFilter === chip.packageName}
        aria-label={`${chip.name}, ${notificationCount(chip.count)}`}
        onClick={() => onAppFilter(chip.packageName)}
      >
        <NotificationAppIcon appName={chip.name} base64={chip.icon} />
        <span>{chip.name}</span>
        <span className="notification-chip-count">{chip.count}</span>
      </button>)}
    </div>}
    <div role="group" aria-label="Arrange by" className="notification-arrangement">
      <button type="button" title="Time" aria-pressed={arrangement === "time"} onClick={() => onArrangement("time")}><Clock size={14} aria-hidden /><span className="notification-toolbar-label">Time</span></button>
      <button type="button" title="App" aria-pressed={arrangement === "app"} onClick={() => onArrangement("app")}><Layers size={14} aria-hidden /><span className="notification-toolbar-label">App</span></button>
    </div>
    {dismissAll && <button id="notifications-dismiss-all" type="button" title={dismissAll.label} disabled={dismissAll.disabled} onClick={dismissAll.onDismissAll}><CheckCheck size={14} aria-hidden /><span className="notification-toolbar-label">{dismissAll.label}</span></button>}
  </div>;
}
