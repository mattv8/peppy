import { Bell, Lock } from "@peppy/desktop-ui";
import type { AppFilter } from "../bridge/bridge";

export function NotificationEmptyState({ filters, hasNotifications, onSettings, onManageApps }: {
  filters: AppFilter[];
  hasNotifications: boolean;
  onSettings(): void;
  onManageApps(): void;
}) {
  const mutedCount = filters.filter(filter => filter.muted).length;
  const unset = !filters.length && !hasNotifications;
  return <div id="notifications-empty-state" data-empty-state={unset ? "unset" : "caught-up"}>
    <Bell size={32} aria-hidden />
    <h2 tabIndex={-1}>{unset ? "No notifications yet" : "You're all caught up"}</h2>
    <p className="setup-hint">{unset ? "Turn on notification access in Peppy on your phone." : "New notifications from your phones will appear here."}</p>
    {unset ? <button className="secondary-button" onClick={onSettings}>Notification settings</button> : mutedCount > 0 && <button className="link-button" onClick={onManageApps}>{mutedCount} app{mutedCount === 1 ? "" : "s"} muted</button>}
  </div>;
}

export function NotificationLockedState() {
  return <div id="notifications-locked-state">
    <Lock size={32} aria-hidden />
    <h2 tabIndex={-1}>Sync is locked</h2>
    <p className="setup-hint">Unlock sync to see notifications. Notifications that arrive while it&apos;s locked are not kept.</p>
  </div>;
}
