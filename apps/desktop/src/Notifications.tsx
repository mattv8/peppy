import { useEffect, useRef, useState } from "react";
import { Bell, formatDateTime, formatElapsedShort, isoDateTime, Lock, X } from "@peppy/desktop-ui";
import type {
  AppFilter,
  MirroredNotification,
  NotificationTarget,
} from "./bridge";

type Source = { id: string; name: string };
type SeenResult = Promise<void>;
const ACTIVATION_HIGHLIGHT_MS = 1600;

export const notificationTuple = (...parts: string[]) =>
  encodeURIComponent(JSON.stringify(parts));

export const notificationMuteKey = (sourceDeviceId: string, packageName: string) =>
  notificationTuple(sourceDeviceId, packageName);

export function notificationSourceName(id: string, sources: Source[], knownIds: string[]) {
  return sources.find(source => source.id === id)?.name ??
    `Phone ${[...new Set(knownIds)].sort().indexOf(id) + 1}`;
}

export function notificationAppName(appName: string, packageName: string) {
  if (appName.trim() && appName !== packageName) return appName;
  const parts = packageName.split(".").filter(Boolean);
  const segment = parts.at(-1) === "app" || parts.at(-1) === "android"
    ? parts.at(-2)
    : parts.at(-1);
  return segment ? segment.replace(/[-_]+/g, " ").replace(/\b\w/g, letter => letter.toUpperCase()) : "App";
}

export function visibleNotifications(notifications: MirroredNotification[], filters: AppFilter[], locked: boolean) {
  if (locked) return [];
  const muted = new Set(filters.filter(filter => filter.muted).map(filter => notificationMuteKey(filter.sourceDeviceId, filter.packageName)));
  return notifications.filter(notification => !muted.has(notificationMuteKey(notification.target.sourceDeviceId, notification.packageName)));
}

export function dismissibleNotifications(notifications: MirroredNotification[], filters: AppFilter[], locked: boolean) {
  return visibleNotifications(notifications, filters, locked).filter(notification => notification.dismissible && !notification.dismissalPending);
}

export function NotificationsView({
  notifications,
  filters,
  sources,
  locked,
  onDismiss,
  onMute,
  onSeen,
  onSettings,
  focusRequest,
  onFocusResult,
  displayTitle = (title) => title,
}: {
  /** Display-only contact name for a phone-number title; stored text is never changed. */
  displayTitle?(title: string): string;
  notifications: MirroredNotification[];
  filters: AppFilter[];
  sources: Source[];
  locked: boolean;
  onDismiss(target: NotificationTarget): void;
  onMute(filter: AppFilter): void;
  onSeen(targets: NotificationTarget[]): SeenResult;
  onSettings(): void;
  /** Supply only after the current unlocked snapshot is ready. */
  focusRequest?: { requestId: string; target: NotificationTarget };
  onFocusResult?: (requestId: string, found: boolean) => void;
}) {
  const feedRef = useRef<HTMLDivElement>(null);
  const visible = useRef(new Map<string, NotificationTarget>());
  const seen = useRef(new Set<string>());
  const inFlight = useRef(new Set<string>());
  const onSeenRef = useRef(onSeen);
  onSeenRef.current = onSeen;
  const onFocusResultRef = useRef(onFocusResult);
  onFocusResultRef.current = onFocusResult;
  const shown = visibleNotifications(notifications, filters, locked);
  const knownSourceIds = [...new Set(shown.map(notification => notification.target.sourceDeviceId))];
  const filtersByApp = new Map(filters.map(filter => [notificationMuteKey(filter.sourceDeviceId, filter.packageName), filter]));
  const shownKey = shown
    .map((notification) =>
      notificationTuple(
        notification.target.sourceDeviceId,
        notification.target.notificationKey,
        notification.target.lifetime,
      ),
    )
    .join(",");

  const focusRequestId = focusRequest?.requestId;
  const focusKey = focusRequest && notificationTuple(
    focusRequest.target.sourceDeviceId,
    focusRequest.target.notificationKey,
    focusRequest.target.lifetime,
  );
  const focusRowShown = shown.some(notification => notificationTuple(
    notification.target.sourceDeviceId,
    notification.target.notificationKey,
    notification.target.lifetime,
  ) === focusKey);

  useEffect(() => {
    if (locked || focusRequestId === undefined || focusKey === undefined) return;
    const row = focusRowShown ? document.getElementById(`notif-${focusKey}`) : null;
    if (!row || !feedRef.current?.contains(row)) {
      onFocusResultRef.current?.(focusRequestId, false);
      return;
    }
    // Instant scrolling avoids obsolete smooth-scroll work and respects reduced motion.
    row.scrollIntoView({ block: "nearest", behavior: "instant" });
    row.focus({ preventScroll: true });
    row.dataset.activationHighlight = "true";
    const timer = window.setTimeout(() => {
      delete row.dataset.activationHighlight;
    }, ACTIVATION_HIGHLIGHT_MS);
    onFocusResultRef.current?.(focusRequestId, true);
    return () => {
      window.clearTimeout(timer);
      delete row.dataset.activationHighlight;
    };
  }, [locked, focusRequestId, focusKey, focusRowShown]);

  useEffect(() => {
    const flushSeen = () => {
      if (!document.hasFocus() || document.visibilityState !== "visible") return;
      const targets = [...visible.current.entries()]
        .filter(([key]) => !seen.current.has(key) && !inFlight.current.has(key))
        .map(([, target]) => target);
      if (!targets.length) return;
      const keys = targets.map((target) =>
        notificationTuple(
          target.sourceDeviceId,
          target.notificationKey,
          target.lifetime,
        ),
      );
      keys.forEach((key) => inFlight.current.add(key));
      void onSeenRef.current(targets).then(
        () => keys.forEach((key) => seen.current.add(key)),
        () => undefined,
      ).finally(() => keys.forEach((key) => inFlight.current.delete(key)));
    };
    const feed = feedRef.current;
    if (!feed || typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          const row = entry.target as HTMLElement;
          const target = JSON.parse(
            row.dataset.notificationTarget ?? "null",
          ) as NotificationTarget | null;
          if (!target) return;
          const key = notificationTuple(
            target.sourceDeviceId,
            target.notificationKey,
            target.lifetime,
          );
          if (entry.isIntersecting) visible.current.set(key, target);
          else visible.current.delete(key);
        });
        flushSeen();
      },
      { root: feed },
    );
    feed
      .querySelectorAll<HTMLElement>("[data-notification-target]")
      .forEach((row) => observer.observe(row));
    window.addEventListener("focus", flushSeen);
    document.addEventListener("visibilitychange", flushSeen);
    flushSeen();
    return () => {
      observer.disconnect();
      window.removeEventListener("focus", flushSeen);
      document.removeEventListener("visibilitychange", flushSeen);
    };
  }, [shownKey]);

  if (locked) {
    return <section id="notifications-view" role="region" aria-label="Notifications">
      <div id="notifications-locked-state" role="status">
        <Lock size={32} aria-hidden />
        <p>Sync is locked</p>
        <p className="setup-hint">Unlock sync to receive notifications.</p>
      </div>
    </section>;
  }

  if (!shown.length) {
    return <section id="notifications-view" role="region" aria-label="Notifications">
      <div id="notifications-empty-state" role="status">
        <Bell size={32} aria-hidden />
        <p>No notifications</p>
        <p className="setup-hint">
          Enable notification mirroring and notification access in the Android
          companion.
        </p>
        <button
          className="secondary-button"
          onClick={onSettings}
        >
          Open settings
        </button>
      </div>
    </section>;
  }

  return <section id="notifications-view" role="region" aria-label="Notifications">
    <div id="notifications-feed" ref={feedRef} role="feed" aria-label="Notification feed">
      {shown.map(notification => <NotificationRow
        key={notificationTuple(notification.target.sourceDeviceId, notification.target.notificationKey, notification.target.lifetime)}
        notification={notification}
        phoneName={notificationSourceName(notification.target.sourceDeviceId, sources, knownSourceIds)}
        appName={notificationAppName(notification.appName, notification.packageName)}
        filter={filtersByApp.get(notificationMuteKey(notification.target.sourceDeviceId, notification.packageName)) ?? { sourceDeviceId: notification.target.sourceDeviceId, packageName: notification.packageName, appName: notification.appName, muted: false }}
        title={displayTitle(notification.title)}
        onDismiss={onDismiss}
        onMute={onMute}
      />)}
    </div>
  </section>;
}

function NotificationRow({ notification, phoneName, appName, filter, title, onDismiss, onMute }: { notification: MirroredNotification; phoneName: string; appName: string; filter: AppFilter; title: string; onDismiss(target: NotificationTarget): void; onMute(filter: AppFilter): void }) {
  const pending = notification.dismissalPending;
  const id = notificationTuple(notification.target.sourceDeviceId, notification.target.notificationKey, notification.target.lifetime);
  return <article id={`notif-${id}`} tabIndex={-1} data-device-id={notification.target.sourceDeviceId} data-notification-key={notification.target.notificationKey} data-dismiss-state={pending ? "pending" : "idle"} data-notification-target={JSON.stringify(notification.target)} className="notif-row" aria-label={`${appName} on ${phoneName}: ${title}: ${notification.text}`}>
    <NotificationAppIcon key={notification.appIconPngBase64} appName={appName} base64={notification.appIconPngBase64} />
    <div className="notif-row-body">
      <span className="notif-row-app-source"><b>{appName}</b><span>{phoneName}</span></span>
      <b className="notif-row-title">{title}</b>
      <span className="notif-row-text">{notification.text}</span>
      {pending && <span className="notif-pending" role="status">Dismissal pending — will apply on your phone&apos;s next sync.</span>}
    </div>
    <time className="notif-row-time" dateTime={isoDateTime(notification.postedAt)} title={formatDateTime(notification.postedAt) ?? "Unknown time"}>{formatElapsedShort(notification.postedAt) ?? "Unknown time"}</time>
    <button className="notif-inline-action" aria-label={`Mute ${appName} on ${phoneName}`} onClick={() => onMute(filter)}>Mute</button>
    <button className="notif-dismiss-btn" disabled={pending || !notification.dismissible} title={pending ? "Dismissal pending — will apply on your phone's next sync." : "Dismiss"} aria-label={`Dismiss ${title || appName || "notification"} from ${appName} on ${phoneName}`} onClick={() => onDismiss(notification.target)}>
      <X size={14} aria-hidden />
    </button>
  </article>;
}

function NotificationAppIcon({ appName, base64 }: { appName: string; base64?: string }) {
  const [failed, setFailed] = useState(false);
  if (!base64 || failed) return <span className="notif-app-icon-fallback" data-notification-app-icon="fallback" aria-hidden>{Array.from(appName)[0]?.toUpperCase() ?? "A"}</span>;
  return <img className="notif-app-icon" data-notification-app-icon="image" src={`data:image/png;base64,${base64}`} alt="" onError={() => setFailed(true)} />;
}
