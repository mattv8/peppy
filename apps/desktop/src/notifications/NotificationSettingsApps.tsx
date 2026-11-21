import { UPDATE_PAUSED_DESCRIPTION } from "@peppy/desktop-ui";
import { useMemo, useState, type ReactElement } from "react";
import type { AppFilter, MirroredNotification } from "../bridge/bridge";
import { notificationAppName, notificationSourceName, notificationTuple } from "./Notifications";

type NotificationSettingsAppsProps = {
  apps: AppFilter[];
  notifications: MirroredNotification[];
  sources: { id: string; name: string }[];
  busy: boolean;
  writesPaused?: boolean;
  pauseNoticeVisible?: boolean;
  onMute(filter: AppFilter): void;
};

function AppIcon({ appName, icon }: { appName: string; icon?: string }): ReactElement {
  const [failedIcon, setFailedIcon] = useState<string>();
  if (icon && icon !== failedIcon) {
    return <img className="notif-app-icon" src={`data:image/png;base64,${icon}`} alt="" onError={() => setFailedIcon(icon)} />;
  }
  return <span className="notif-app-letter-avatar" aria-hidden>{appName.slice(0, 1).toUpperCase()}</span>;
}

function AppRow({ filter, phone, notification, busy, writesPaused, pauseNoticeVisible, onMute }: {
  filter: AppFilter;
  phone: string;
  notification?: MirroredNotification;
  busy: boolean;
  writesPaused?: boolean;
  pauseNoticeVisible?: boolean;
  onMute(filter: AppFilter): void;
}): ReactElement {
  const id = notificationTuple(filter.sourceDeviceId, filter.packageName);
  const appName = notificationAppName(filter.appName, filter.packageName);
  return <li data-device-id={filter.sourceDeviceId} data-package={filter.packageName} className="notif-app-filter-row">
    <AppIcon appName={appName} icon={notification?.appIconPngBase64} />
    <span className="notif-app-filter-label"><b>{appName}</b><small>{phone}</small></span>
    <span className="settings-switch">
      <input id={`filter-${id}`} type="checkbox" role="switch" disabled={busy || writesPaused} title={writesPaused ? UPDATE_PAUSED_DESCRIPTION : undefined} aria-describedby={writesPaused ? pauseNoticeVisible ? "notification-update-note" : "update-paused-description" : undefined}
        aria-label={`Mirror ${appName} from ${phone}`} checked={!filter.muted} aria-checked={!filter.muted}
        onChange={event => onMute({ ...filter, muted: !event.target.checked })} />
      <span aria-hidden />
    </span>
  </li>;
}

export function NotificationSettingsApps({ apps, notifications, sources, busy, writesPaused, pauseNoticeVisible, onMute }: NotificationSettingsAppsProps): ReactElement {
  const [query, setQuery] = useState("");
  const sourceIds = apps.map(app => app.sourceDeviceId);
  const mutedApps = apps.filter(app => app.muted);
  const unmutedApps = apps.filter(app => !app.muted);
  const phones = new Set(apps.map(app => app.sourceDeviceId));
  const newestNotifications = useMemo(() => new Map(
    notifications.reduce((newest, notification) => {
      const key = notificationTuple(notification.target.sourceDeviceId, notification.packageName);
      const current = newest.get(key);
      if (!current || notification.postedAt > current.postedAt) newest.set(key, notification);
      return newest;
    }, new Map<string, MirroredNotification>()).entries(),
  ), [notifications]);
  const appName = (app: AppFilter) => notificationAppName(app.appName, app.packageName);
  const phoneName = (app: AppFilter) => notificationSourceName(app.sourceDeviceId, sources, sourceIds);
  const matchesQuery = (app: AppFilter) => {
    const haystack = `${appName(app)} ${phoneName(app)}`.toLowerCase();
    return haystack.includes(query.trim().toLowerCase());
  };
  const visibleApps = unmutedApps.filter(app => apps.length <= 8 || matchesQuery(app));
  const renderRow = (app: AppFilter) => <AppRow key={notificationTuple(app.sourceDeviceId, app.packageName)} filter={app} phone={phoneName(app)}
    notification={newestNotifications.get(notificationTuple(app.sourceDeviceId, app.packageName))} busy={busy} writesPaused={writesPaused} pauseNoticeVisible={pauseNoticeVisible} onMute={onMute} />;

  return <>
    {mutedApps.length > 0 && <section id="notification-muted-app-list" aria-label="Muted">
      <h4>Muted</h4>
      <ul id="notification-muted-apps">{mutedApps.map(renderRow)}</ul>
    </section>}
    {unmutedApps.length > 0 && <details id="notification-app-rules-disclosure" className="settings-disclosure">
      <summary>All apps ({apps.length})</summary>
      {apps.length > 8 && <input id="notification-app-filter-input" aria-label="Find an app" value={query} onChange={event => setQuery(event.target.value)} />}
      <ul id="notification-app-filter-list" aria-label="App mirroring filters">
        {phones.size > 1
          ? [...phones].map(phoneId => {
            const phoneApps = visibleApps.filter(app => app.sourceDeviceId === phoneId);
            return phoneApps.length > 0 && <li key={phoneId} data-notification-app-phone={phoneId} className="notification-app-phone-group">
              <h4>{notificationSourceName(phoneId, sources, sourceIds)}</h4>
              <ul data-notification-app-phone-list={phoneId}>{phoneApps.map(renderRow)}</ul>
            </li>;
          })
          : visibleApps.map(renderRow)}
      </ul>
    </details>}
  </>;
}
