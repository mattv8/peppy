import { useMemo, useRef } from "react";
import { formatDateTime, formatElapsedShort, type OmniNotificationResults } from "@peppy/desktop-ui";
import type { AppFilter, MirroredNotification } from "../bridge/bridge";
import { useScopedSearch } from "../search/useScopedSearch";
import { notificationAppName, notificationSourceName, visibleNotifications } from "./Notifications";
import { highlightSegments, notificationSearchTerms, searchNotifications } from "./notificationSearch";
import { notificationKey } from "./notificationSections";

type Source = { id: string; name: string };

type UseNotificationSearchOptions = {
  active: boolean;
  locked: boolean;
  notifications: MirroredNotification[];
  filters: AppFilter[];
  sources: Source[];
  /** Must be stable (memoized) — a new identity rebuilds the results so resolved contact names show. */
  displayTitle(title: string): string;
  onOpenHit(target: MirroredNotification["target"]): void;
  onShowAll(query: string): void;
  onUnavailable(): void;
};

export function useNotificationSearch({
  active, locked, notifications, filters, sources, displayTitle, onOpenHit, onShowAll, onUnavailable,
}: UseNotificationSearchOptions) {
  const { query: notificationQuery, setQuery: setNotificationQuery, scope, resetSearch } = useScopedSearch({
    active,
    locked,
    label: "Notifications",
    placeholder: "Search notifications",
  });
  const targets = useRef(new Map<string, MirroredNotification["target"]>());
  const onOpenHitRef = useRef(onOpenHit);
  const onShowAllRef = useRef(onShowAll);
  const onUnavailableRef = useRef(onUnavailable);
  onOpenHitRef.current = onOpenHit;
  onShowAllRef.current = onShowAll;
  onUnavailableRef.current = onUnavailable;
  const notificationResults = useMemo<OmniNotificationResults | undefined>(() => {
    if (locked) return undefined;
    const visible = visibleNotifications(notifications, filters, false);
    const sourceIds = [...new Set(visible.map(notification => notification.target.sourceDeviceId))];
    const labels = {
      appName: (notification: MirroredNotification) => notificationAppName(notification.appName, notification.packageName),
      sourceName: (notification: MirroredNotification) => notificationSourceName(notification.target.sourceDeviceId, sources, sourceIds),
      title: (notification: MirroredNotification) => displayTitle(notification.title),
    };
    return {
      search(query) {
        const matches = searchNotifications(visible, query, labels).sort((left, right) => right.postedAt - left.postedAt);
        targets.current = new Map(matches.map(notification => [notificationKey(notification.target), notification.target]));
        const terms = notificationSearchTerms(query);
        return {
          total: matches.length,
          hits: matches.slice(0, 3).map(notification => ({
            key: notificationKey(notification.target),
            appName: labels.appName(notification),
            sourceName: sourceIds.length >= 2 ? labels.sourceName(notification) : undefined,
            iconPngBase64: notification.appIconPngBase64,
            title: highlightSegments(labels.title(notification), terms),
            snippet: highlightSegments(notification.text, terms),
            timeLabel: formatElapsedShort(notification.postedAt),
            timeTitle: formatDateTime(notification.postedAt),
          })),
        };
      },
      onActivate(key) {
        const current = visible.find(notification => notificationKey(notification.target) === key)?.target;
        if (!current) {
          onUnavailableRef.current();
          return;
        }
        onOpenHitRef.current(targets.current.get(key) ?? current);
      },
      onShowAll: query => onShowAllRef.current(query),
    };
  }, [displayTitle, filters, locked, notifications, sources]);
  return { notificationQuery, setNotificationQuery, notificationResults, resetSearch, scope };
}
