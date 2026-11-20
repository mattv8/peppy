import { useEffect, useRef, useState } from "react";
import type { AppFilter, MirroredNotification, NotificationTarget } from "../bridge/bridge";
import type { OneTimeCodeTarget } from "@peppy/desktop-ui/bridge-contracts";
import { notificationMuteKey, notificationTuple } from "./notificationIdentity";
import { notificationKey, stackNotifications } from "./notificationSections";
import { searchNotifications } from "./notificationSearch";
import { useNotificationArrangement } from "./notificationViewPreference";
import { NotificationEmptyState, NotificationLockedState, NotificationNoMatch } from "./NotificationEmptyState";
import { NotificationFilterBar, notificationFilterChips } from "./NotificationFilterBar";
import { NotificationSections, NotificationStacks } from "./NotificationStack";
import type { NotificationLabels } from "./NotificationRow";

export { notificationMuteKey, notificationTuple } from "./notificationIdentity";

type Source = { id: string; name: string };
type FocusRequest = { requestId: string; target: NotificationTarget };
type DeferredFocus = { removed: string; neighbor?: string; activeInFeed: boolean };
const ACTIVATION_HIGHLIGHT_MS = 1600;

export function notificationSourceName(id: string, sources: Source[], knownIds: string[]) {
  return sources.find(source => source.id === id)?.name ??
    `Phone ${[...new Set(knownIds)].sort().indexOf(id) + 1}`;
}

export function notificationAppName(appName: string, packageName: string) {
  if (appName.trim() && appName !== packageName) return appName;
  const parts = packageName.split(".").filter(Boolean);
  const segment = parts.at(-1) === "app" || parts.at(-1) === "android" ? parts.at(-2) : parts.at(-1);
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
  notifications, filters, sources, locked, writesPaused = false, onDismiss, onMute, onSeen, onCopyOneTimeCode, onSettings, onManageApps,
  onDismissMany, query, onClearQuery, appFilter, onAppFilter, focusRequest, onFocusResult, displayTitle, dismissAll,
}: {
  notifications: MirroredNotification[];
  filters: AppFilter[];
  sources: Source[];
  locked: boolean;
  onDismiss(target: NotificationTarget): void;
  onMute(filter: AppFilter): void;
  writesPaused?: boolean;
  onSeen(targets: NotificationTarget[]): Promise<void>;
  onCopyOneTimeCode(target: OneTimeCodeTarget, code: string): Promise<void>;
  onSettings(): void;
  onManageApps(): void;
  onDismissMany(targets: NotificationTarget[]): Promise<void>;
  query: string;
  onClearQuery(): void;
  appFilter: string | null;
  onAppFilter(packageName: string | null): void;
  focusRequest?: FocusRequest;
  onFocusResult?(requestId: string, found: boolean): void;
  displayTitle(title: string): string;
  dismissAll?: { label: string; disabled: boolean; onDismissAll(): void };
}) {
  const feedRef = useRef<HTMLDivElement>(null);
  const visible = useRef(new Map<string, NotificationTarget>());
  const seen = useRef(new Set<string>());
  const inFlight = useRef(new Set<string>());
  const newKeys = useRef(new Set<string>());
  const pendingFocus = useRef<DeferredFocus | undefined>(undefined);
  const onSeenRef = useRef(onSeen);
  const snapshotSeenRef = useRef(new Map<string, boolean>());
  onSeenRef.current = onSeen;
  const unfiltered = visibleNotifications(notifications, filters, locked);
  const knownSourceIds = [...new Set(unfiltered.map(notification => notification.target.sourceDeviceId))];
  const labels: NotificationLabels = {
    appName: notification => notificationAppName(notification.appName, notification.packageName),
    sourceName: notification => notificationSourceName(notification.target.sourceDeviceId, sources, knownSourceIds),
    title: notification => displayTitle(notification.title),
  };
  const queryMatches = searchNotifications(unfiltered, query, labels);
  const matching = appFilter ? queryMatches.filter(notification => notification.packageName === appFilter) : queryMatches;
  const shown = matching.map((notification, index) => ({ notification, index }))
    .sort((left, right) => right.notification.postedAt - left.notification.postedAt || left.index - right.index)
    .map(({ notification }) => notification);
  const [arrangement, setArrangement] = useNotificationArrangement();
  const [expandedStacks, setExpandedStacks] = useState<ReadonlySet<string>>(() => new Set());
  const stacks = stackNotifications(shown);
  const focusKey = focusRequest && notificationKey(focusRequest.target);
  const focusStackKey = focusKey && stacks.find(stack => stack.items.some(item => notificationKey(item.target) === focusKey))?.key;
  const fullVisibleKey = unfiltered.map(notification => notificationKey(notification.target)).join(",");
  snapshotSeenRef.current = new Map(unfiltered.map(notification => [notificationKey(notification.target), notification.seen]));
  const shownKey = shown.map(notification => notificationKey(notification.target)).join(",");
  const rendered = arrangement === "time"
    ? shown
    : stacks.flatMap(stack => {
      const expanded = expandedStacks.has(stack.key) || stack.key === focusStackKey;
      return stack.items.length === 1 || expanded ? stack.items : stack.items.slice(0, 2);
    });
  const renderedKey = rendered.map(notification => notificationKey(notification.target)).join(",");
  const showPhone = new Set(shown.map(notification => notification.target.sourceDeviceId)).size > 1;

  for (const notification of unfiltered) {
    if (!notification.seen) newKeys.current.add(notificationKey(notification.target));
  }

  useEffect(() => {
    if (focusStackKey && arrangement === "app") {
      setExpandedStacks(current => current.has(focusStackKey) ? current : new Set([...current, focusStackKey]));
    }
  }, [arrangement, focusStackKey]);

  useEffect(() => {
    if (locked || !focusRequest || !focusKey) return;
    const row = document.getElementById(`notif-${focusKey}`);
    if (!row || !feedRef.current?.contains(row)) {
      onFocusResult?.(focusRequest.requestId, false);
      return;
    }
    row.scrollIntoView({ block: "nearest", behavior: "instant" });
    row.focus({ preventScroll: true });
    row.dataset.activationHighlight = "true";
    const timer = window.setTimeout(() => delete row.dataset.activationHighlight, ACTIVATION_HIGHLIGHT_MS);
    onFocusResult?.(focusRequest.requestId, true);
    return () => {
      window.clearTimeout(timer);
      delete row.dataset.activationHighlight;
    };
  }, [locked, focusRequest?.requestId, focusKey, shownKey]);

  useEffect(() => {
    if (appFilter && !unfiltered.some(notification => notification.packageName === appFilter)) onAppFilter(null);
  }, [appFilter, onAppFilter, fullVisibleKey]);

  useEffect(() => {
    const cancelWhenFocusLeaves = (event: FocusEvent) => {
      const target = event.target;
      if (target instanceof Node && !feedRef.current?.contains(target)) pendingFocus.current = undefined;
    };
    document.addEventListener("focusin", cancelWhenFocusLeaves);
    return () => document.removeEventListener("focusin", cancelWhenFocusLeaves);
  }, []);

  useEffect(() => {
    const deferred = pendingFocus.current;
    if (!deferred || !deferred.activeInFeed) return;
    const active = document.activeElement;
    if (active && active !== document.body && !feedRef.current?.contains(active)) {
      pendingFocus.current = undefined;
      return;
    }
    const removed = document.getElementById(`notif-${deferred.removed}`);
    if (removed) return;
    const next = deferred.neighbor ? document.getElementById(`notif-${deferred.neighbor}`) : null;
    const fallback = feedRef.current?.querySelector<HTMLElement>("article");
    const target = next ?? fallback ?? document.querySelector<HTMLElement>("#notifications-empty-state h2, #notifications-no-match h2");
    target?.focus({ preventScroll: true });
    pendingFocus.current = undefined;
  }, [renderedKey, locked]);

  useEffect(() => {
    const visibleKeys = new Set(unfiltered.map(notification => notificationKey(notification.target)));
    visible.current.clear();
    for (const key of seen.current) if (!visibleKeys.has(key)) seen.current.delete(key);
    for (const key of inFlight.current) if (!visibleKeys.has(key)) inFlight.current.delete(key);
    const flushSeen = () => {
      if (!document.hasFocus() || document.visibilityState !== "visible") return;
      const targets = [...visible.current.entries()]
        .filter(([key]) => !seen.current.has(key) && !inFlight.current.has(key) && !snapshotSeenRef.current.get(key))
        .map(([, target]) => target);
      if (!targets.length) return;
      const keys = targets.map(notificationKey);
      keys.forEach(key => inFlight.current.add(key));
      void onSeenRef.current(targets).then(
        () => keys.forEach(key => seen.current.add(key)),
        () => undefined,
      ).finally(() => keys.forEach(key => inFlight.current.delete(key)));
    };
    const feed = feedRef.current;
    if (!feed || typeof IntersectionObserver === "undefined") return;
    let current = true;
    const observer = new IntersectionObserver(entries => {
      if (!current) return;
      for (const entry of entries) {
        const row = entry.target as HTMLElement;
        const target = JSON.parse(row.dataset.notificationTarget ?? "null") as NotificationTarget | null;
        if (!target) continue;
        if (entry.isIntersecting) visible.current.set(notificationKey(target), target);
        else visible.current.delete(notificationKey(target));
      }
      flushSeen();
    }, { root: feed });
    feed.querySelectorAll<HTMLElement>("[data-notification-target]").forEach(row => observer.observe(row));
    window.addEventListener("focus", flushSeen);
    document.addEventListener("visibilitychange", flushSeen);
    flushSeen();
    return () => {
      current = false;
      observer.disconnect();
      window.removeEventListener("focus", flushSeen);
      document.removeEventListener("visibilitychange", flushSeen);
    };
  }, [fullVisibleKey, renderedKey, arrangement]);

  const rememberNeighbor = (keys: string[]) => {
    const rows = [...(feedRef.current?.querySelectorAll<HTMLElement>("article") ?? [])];
    const indices = keys.map(key => rows.findIndex(row => row.id === `notif-${key}`)).filter(index => index >= 0);
    const first = Math.min(...indices);
    const last = Math.max(...indices);
    const neighbor = rows[last + 1] ?? rows[first - 1];
    const active = document.activeElement;
    pendingFocus.current = {
      removed: keys[0],
      neighbor: neighbor?.id.replace(/^notif-/, ""),
      activeInFeed: active === document.body || Boolean(active && feedRef.current?.contains(active)),
    };
  };

  if (locked) return <section id="notifications-view" role="region" aria-label="Notifications"><h1 className="visually-hidden">Notifications</h1><NotificationLockedState /></section>;

  const filtersByApp = new Map(filters.map(filter => [notificationMuteKey(filter.sourceDeviceId, filter.packageName), filter]));
  const chips = notificationFilterChips(queryMatches, labels.appName);
  const feedProps = { labels, filtersByApp, newKeys: newKeys.current, showPhone, query, writesPaused, onDismiss, onMute, onCopy: onCopyOneTimeCode, onAction: rememberNeighbor };
  return <section id="notifications-view" role="region" aria-label="Notifications">
    <h1 className="visually-hidden">Notifications</h1>
    <NotificationFilterBar chips={chips} appFilter={appFilter} onAppFilter={onAppFilter} arrangement={arrangement} onArrangement={setArrangement} dismissAll={dismissAll} />
    {query && <p id="notifications-search-status">{shown.length} notification{shown.length === 1 ? "" : "s"} match{shown.length === 1 ? "es" : ""} “{query}”</p>}
    <div id="notifications-announcer" className="visually-hidden" aria-live="polite">
      {query ? `${shown.length} notifications match “${query}”` : appFilter ? `Showing ${shown.length} ${chips.find(chip => chip.packageName === appFilter)?.name ?? "app"} notifications` : ""}
    </div>
    {!shown.length ? (query
      ? <NotificationNoMatch query={query} onClearQuery={onClearQuery} />
      : <NotificationEmptyState filters={filters} hasNotifications={notifications.length > 0} onSettings={onSettings} onManageApps={onManageApps} />
    ) : <div id="notifications-feed" ref={feedRef} aria-label="Notification feed">
      {arrangement === "time"
        ? <NotificationSections notifications={shown} {...feedProps} />
        : <NotificationStacks stacks={stacks} expanded={expandedStacks} focusStackKey={focusStackKey} onExpanded={key => setExpandedStacks(current => current.has(key) ? new Set([...current].filter(value => value !== key)) : new Set([...current, key]))} onDismissMany={onDismissMany} {...feedProps} />}
    </div>}
  </section>;
}
