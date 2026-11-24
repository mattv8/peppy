import { useEffect, useMemo, useRef, useState } from "react";
import type { AppFilter, MirroredNotification, NotificationTarget } from "../bridge/bridge";
import type { NotificationDismissalBlock, NotificationDismissalBlockReason, OneTimeCodeTarget } from "@peppy/desktop-ui/bridge-contracts";
import { notificationMuteKey, notificationTuple } from "./notificationIdentity";
import { notificationKey, stackNotifications } from "./notificationSections";
import { searchNotifications } from "./notificationSearch";
import { useNotificationArrangement } from "./notificationViewPreference";
import { SearchNoMatch } from "../search/SearchNoMatch";
import { NotificationEmptyState, NotificationLockedState } from "./NotificationEmptyState";
import { NotificationFilterBar, notificationFilterChips } from "./NotificationFilterBar";
import { filterFor, NotificationSections, NotificationStacks } from "./NotificationStack";
import type { NotificationLabels, NotificationRowStatus } from "./NotificationRow";
import { NotificationActionNotice } from "./NotificationActionNotice";
import { NotificationWaitingControl } from "./NotificationWaitingControl";
import { NotificationWaitingGroups, type WaitingGroup, type WaitingGroupRowProps } from "./NotificationWaitingGroups";
import { NotificationPhoneBlockers, type PhoneBlock } from "./NotificationPhoneBlockers";
import {
  BLOCKED_HIDDEN_ANNOUNCEMENT,
  BLOCKED_SHOWN_ANNOUNCEMENT,
  blockerCopy,
  NO_WAITING_ANNOUNCEMENT,
  WAITING_HIDDEN_ANNOUNCEMENT,
  WAITING_SHOWN_ANNOUNCEMENT,
} from "./notificationDismissCopy";
import { newestDismissTargets } from "./dismissalOverlay";

export { notificationMuteKey, notificationTuple } from "./notificationIdentity";

type Source = { id: string; name: string };
type FocusRequest = { requestId: string; target: NotificationTarget };
type DeferredFocus = { removed: string; neighbor?: string; activeInFeed: boolean; fromToolbar: boolean };
type Failure = { message: string; canRetry: boolean };
/** A message for the action announcer. A new `seq` re-announces even when the text repeats. */
export type NotificationAnnouncement = { seq: number; text: string };
const ACTIVATION_HIGHLIGHT_MS = 1600;
const PHONE_SIM_SUFFIX = / · SIM \d+$/;
const NO_KEYS: ReadonlySet<string> = new Set();
const NO_FAILURES: ReadonlyMap<string, Failure> = new Map();
const NO_BLOCKS: NotificationDismissalBlock[] = [];
const NO_ANNOUNCEMENT: NotificationAnnouncement = { seq: 0, text: "" };

/**
 * Polite live region for dismissal, waiting and phone-block messages. A repeated message is cleared first, then set on
 * the next tick, so screen readers announce it again.
 */
function ActionAnnouncer({ announcement }: { announcement: NotificationAnnouncement }) {
  const [text, setText] = useState("");
  useEffect(() => {
    setText("");
    const timer = window.setTimeout(() => setText(announcement.text), 0);
    return () => window.clearTimeout(timer);
  }, [announcement.seq]);
  return <div id="notifications-action-announcer" className="visually-hidden" aria-live="polite">{text}</div>;
}

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

/** Id-safe form of a device id for DOM ids. Non-alphanumerics are hex-escaped, so distinct devices stay distinct. */
export function phoneKey(sourceDeviceId: string): string {
  return [...sourceDeviceId].map(char => /[A-Za-z0-9]/.test(char) ? char : `_${(char.codePointAt(0) ?? 0).toString(16)}_`).join("");
}

/** A phone's display name, without the " · SIM n" suffix that repeats per SIM. */
export function phoneNameOf(sourceDeviceId: string, sources: Source[], knownIds: string[]): string {
  return notificationSourceName(sourceDeviceId, sources, knownIds).replace(PHONE_SIM_SUFFIX, "");
}

/** Blocks the list can name: a block for a phone that is not a known gateway is ignored. */
export function shownDismissalBlocks(blocks: NotificationDismissalBlock[], sources: Source[]): NotificationDismissalBlock[] {
  const ids = new Set(sources.map(source => source.id));
  return blocks.filter(block => ids.has(block.sourceDeviceId));
}

export function visibleNotifications(notifications: MirroredNotification[], filters: AppFilter[], locked: boolean) {
  if (locked) return [];
  const muted = new Set(filters.filter(filter => filter.muted).map(filter => notificationMuteKey(filter.sourceDeviceId, filter.packageName)));
  return notifications.filter(notification => !muted.has(notificationMuteKey(notification.target.sourceDeviceId, notification.packageName)));
}

/** Rows that can still be dismissed now: visible, dismissible, not waiting, and not hidden by an in-flight dismissal. */
export function dismissibleNotifications(
  notifications: MirroredNotification[],
  filters: AppFilter[],
  locked: boolean,
  hiddenKeys: ReadonlySet<string> = NO_KEYS,
) {
  return visibleNotifications(notifications, filters, locked).filter(notification =>
    notification.dismissible && !notification.dismissalPending && !hiddenKeys.has(notificationKey(notification.target)));
}

/** Waiting rows grouped per phone, in the order given (newest first). */
function waitingGroups(rows: MirroredNotification[], nameOf: (sourceDeviceId: string) => string): WaitingGroup[] {
  const groups = new Map<string, WaitingGroup>();
  for (const row of rows) {
    const id = row.target.sourceDeviceId;
    const key = phoneKey(id);
    const group = groups.get(key) ?? { phoneKey: key, phoneName: nameOf(id), notifications: [] };
    group.notifications.push(row);
    groups.set(key, group);
  }
  return [...groups.values()];
}

export function NotificationsView({
  notifications, filters, sources, locked, writesPaused = false, onDismiss, onMute, onSeen, onCopyOneTimeCode, onSettings, onManageApps,
  onDismissMany, query, onClearQuery, appFilter, onAppFilter, focusRequest, onFocusResult, displayTitle, dismissAll, actionNotice, onDismissActionNotice,
  hiddenKeys = NO_KEYS, failures = NO_FAILURES, onRetryDismiss, dismissalBlocks = NO_BLOCKS, announcement, onAnnounce,
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
  onDismissMany(targets: NotificationTarget[]): void;
  query: string;
  onClearQuery(): void;
  appFilter: string | null;
  onAppFilter(packageName: string | null): void;
  focusRequest?: FocusRequest;
  onFocusResult?(requestId: string, found: boolean): void;
  displayTitle(title: string): string;
  dismissAll?: { label: string; disabled: boolean; disabledReason?: string; onDismissAll(): void };
  actionNotice?: NotificationActionNotice;
  onDismissActionNotice(): void;
  /** Keys whose rows are hidden while a dismissal is in flight, or waiting for a phone. */
  hiddenKeys?: ReadonlySet<string>;
  /** Dismissals that failed; their rows stay visible with the reason. */
  failures?: ReadonlyMap<string, Failure>;
  onRetryDismiss?(key: string): void;
  /** Phones that report they cannot clear dismissed notifications. Only blocks for known phones are shown. */
  dismissalBlocks?: NotificationDismissalBlock[];
  /** Latest message for the action announcer, owned by the parent so dismissal hooks can share it. */
  announcement?: NotificationAnnouncement;
  onAnnounce?(message: string): void;
}) {
  const feedRef = useRef<HTMLDivElement>(null);
  const visible = useRef(new Map<string, NotificationTarget>());
  const seen = useRef(new Set<string>());
  const inFlight = useRef(new Set<string>());
  const newKeys = useRef(new Set<string>());
  const pendingFocus = useRef<DeferredFocus | undefined>(undefined);
  const waitingFocused = useRef(false);
  const noticeClosed = useRef(false);
  const countShownPhones = useRef<ReadonlySet<string>>(new Set());
  const blockedBefore = useRef(new Map<string, NotificationDismissalBlockReason>());
  const onSeenRef = useRef(onSeen);
  const snapshotSeenRef = useRef(new Map<string, boolean>());
  const [localAnnouncement, setLocalAnnouncement] = useState(NO_ANNOUNCEMENT);
  const [waitingExpanded, setWaitingExpanded] = useState(false);
  const [expandedPhones, setExpandedPhones] = useState<ReadonlySet<string>>(() => new Set());
  onSeenRef.current = onSeen;
  const announce = (message: string) => {
    setLocalAnnouncement(current => ({ seq: current.seq + 1, text: message }));
    onAnnounce?.(message);
  };
  const unfiltered = visibleNotifications(notifications, filters, locked);
  const knownSourceIds = [...new Set(unfiltered.map(notification => notification.target.sourceDeviceId))];
  const labels: NotificationLabels = {
    appName: notification => notificationAppName(notification.appName, notification.packageName),
    sourceName: notification => notificationSourceName(notification.target.sourceDeviceId, sources, knownSourceIds),
    title: notification => displayTitle(notification.title),
  };

  // Waiting rows and blocked phones. A row is in exactly one place: the list, a blocker group, or a count group.
  const waitingRows = unfiltered
    .filter(notification => notification.dismissalPending && notification.dismissible)
    .sort((left, right) => right.postedAt - left.postedAt);
  const nameOf = (sourceDeviceId: string) => phoneNameOf(sourceDeviceId, sources, knownSourceIds);
  const waitingByPhone = new Map<string, number>();
  for (const row of waitingRows) {
    const id = row.target.sourceDeviceId;
    waitingByPhone.set(id, (waitingByPhone.get(id) ?? 0) + 1);
  }
  // A phone with nothing waiting has nothing to explain: no banner, and it does not count toward the grouped banner.
  const bannered = shownDismissalBlocks(dismissalBlocks, sources).filter(block => (waitingByPhone.get(block.sourceDeviceId) ?? 0) > 0);
  const blockedIds = new Set(bannered.map(block => block.sourceDeviceId));
  const phoneBlocks: PhoneBlock[] = bannered.map(block => ({
    phoneKey: phoneKey(block.sourceDeviceId),
    phoneName: nameOf(block.sourceDeviceId),
    reason: block.reason,
    observedAtMs: block.observedAtMs,
    waitingCount: waitingByPhone.get(block.sourceDeviceId) ?? 0,
  }));
  const countRows = waitingRows.filter(notification => !blockedIds.has(notification.target.sourceDeviceId));
  const waitingCount = countRows.length;
  const waitingOpen = waitingExpanded && waitingCount > 0;
  const countGroups = waitingOpen ? waitingGroups(countRows, nameOf) : [];
  const blockerGroups = waitingGroups(
    waitingRows.filter(notification => blockedIds.has(notification.target.sourceDeviceId) && expandedPhones.has(phoneKey(notification.target.sourceDeviceId))),
    nameOf,
  );
  const groupsPresent = blockerGroups.length > 0 || countGroups.length > 0;
  const groupedKey = [...blockerGroups, ...countGroups].flatMap(group => group.notifications).map(notification => notificationKey(notification.target)).join(",");

  // Waiting rows leave the list too: they appear only in a group.
  const listRows = unfiltered.filter(notification =>
    !hiddenKeys.has(notificationKey(notification.target)) && !(notification.dismissalPending && notification.dismissible));
  const queryMatches = searchNotifications(listRows, query, labels);
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
  const blockSignature = phoneBlocks.map(block => `${block.phoneKey}:${block.reason}`).join("|");

  for (const notification of unfiltered) {
    if (!notification.seen) newKeys.current.add(notificationKey(notification.target));
  }

  /** List rows only; group rows are locked and never receive focus from a dismissal. */
  const listRowElements = () => [...(feedRef.current?.querySelectorAll<HTMLElement>("article") ?? [])]
    .filter(row => !row.closest("[data-hidden-group]"));

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
    // Remembers whether focus is on the waiting control, which is gone by the time its count reaches zero. Focus that
    // leaves for the body fires no focusin, so focusout clears the record too.
    const track = (event: FocusEvent) => {
      waitingFocused.current = event.target instanceof Element && Boolean(event.target.closest("#notifications-pending-count"));
    };
    const leave = () => { waitingFocused.current = false; };
    document.addEventListener("focusin", track);
    document.addEventListener("focusout", leave);
    return () => {
      document.removeEventListener("focusin", track);
      document.removeEventListener("focusout", leave);
    };
  }, []);

  useEffect(() => {
    const deferred = pendingFocus.current;
    if (!deferred || !deferred.activeInFeed) return;
    const active = document.activeElement;
    if (!deferred.fromToolbar && active && active !== document.body && !feedRef.current?.contains(active)) {
      pendingFocus.current = undefined;
      return;
    }
    if (document.getElementById(`notif-${deferred.removed}`)) return;
    const next = deferred.neighbor ? document.getElementById(`notif-${deferred.neighbor}`) : null;
    const fallback = listRowElements()[0];
    const target = next ?? fallback ?? document.querySelector<HTMLElement>("#notifications-empty-state h2, #notifications-no-match h2");
    target?.focus({ preventScroll: true });
    pendingFocus.current = undefined;
  }, [renderedKey, locked]);

  useEffect(() => {
    // A phone that stops being blocked loses its reveal. A count group open for a phone that becomes blocked moves under its banner, still open.
    const previous = blockedBefore.current;
    const current = new Map(phoneBlocks.map(block => [block.phoneKey, block.reason]));
    setExpandedPhones(expanded => {
      const next = new Set([...expanded].filter(key => current.has(key)));
      for (const block of phoneBlocks) {
        if (!previous.has(block.phoneKey) && countShownPhones.current.has(block.phoneKey)) next.add(block.phoneKey);
      }
      const unchanged = next.size === expanded.size && [...next].every(key => expanded.has(key));
      return unchanged ? expanded : next;
    });
    for (const block of phoneBlocks) {
      if (previous.get(block.phoneKey) === block.reason) continue;
      const copy = blockerCopy(block.reason, block.phoneName, block.waitingCount);
      announce(`${copy.title}. ${copy.body}`);
    }
    blockedBefore.current = current;
  }, [blockSignature]);

  useEffect(() => {
    countShownPhones.current = new Set(countGroups.map(group => group.phoneKey));
  });

  useEffect(() => {
    if (waitingCount === 0) setWaitingExpanded(false);
  }, [waitingCount]);

  useEffect(() => {
    if (waitingCount > 0 || !waitingFocused.current) return;
    waitingFocused.current = false;
    announce(NO_WAITING_ANNOUNCEMENT);
    const target = listRowElements()[0] ?? document.querySelector<HTMLElement>("#notifications-empty-state h2, #notifications-no-match h2");
    target?.focus({ preventScroll: true });
  }, [waitingCount]);

  useEffect(() => {
    // Closing the notice with its control moves focus once the notice has gone: Dismiss all, else the first row.
    if (actionNotice) {
      noticeClosed.current = false;
      return;
    }
    if (!noticeClosed.current) return;
    noticeClosed.current = false;
    const target = document.querySelector<HTMLElement>("#notifications-dismiss-all:not(:disabled)") ?? listRowElements()[0];
    target?.focus({ preventScroll: true });
  }, [actionNotice]);

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
  }, [fullVisibleKey, renderedKey, groupedKey, arrangement]);

  const rememberNeighbor = (keys: string[], fromToolbar = false) => {
    const rows = listRowElements();
    const indices = keys.map(key => rows.findIndex(row => row.id === `notif-${key}`)).filter(index => index >= 0);
    if (indices.length === 0) {
      pendingFocus.current = undefined;
      return;
    }
    const first = Math.min(...indices);
    const last = Math.max(...indices);
    const neighbor = rows[last + 1] ?? rows[first - 1];
    const active = document.activeElement;
    pendingFocus.current = {
      removed: keys[0],
      neighbor: neighbor?.id.replace(/^notif-/, ""),
      activeInFeed: fromToolbar || active === document.body || Boolean(active && feedRef.current?.contains(active)),
      fromToolbar,
    };
  };

  const rememberNeighborRef = useRef(rememberNeighbor);
  rememberNeighborRef.current = rememberNeighbor;
  // Stable per notice, so the notice's auto-dismiss timer is not restarted by unrelated renders. The action runs with
  // focus moved from its rows; the close control runs `noticeClosed` (see the effect above).
  const notice = useMemo(() => actionNotice?.action ? {
    ...actionNotice,
    action: {
      ...actionNotice.action,
      disabled: writesPaused,
      onAction: () => {
        rememberNeighborRef.current(actionNotice.keys ?? [], true);
        actionNotice.action?.onAction();
      },
    },
  } : actionNotice, [actionNotice, writesPaused]);

  const toggleWaiting = () => {
    const next = !waitingOpen;
    setWaitingExpanded(next);
    announce(next ? WAITING_SHOWN_ANNOUNCEMENT(waitingCount) : WAITING_HIDDEN_ANNOUNCEMENT);
  };

  const togglePhone = (key: string) => {
    const opening = !expandedPhones.has(key);
    const block = phoneBlocks.find(item => item.phoneKey === key);
    setExpandedPhones(current => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
    announce(opening ? BLOCKED_SHOWN_ANNOUNCEMENT(block?.waitingCount ?? 0) : BLOCKED_HIDDEN_ANNOUNCEMENT);
  };

  if (locked) return <section id="notifications-view" role="region" aria-label="Notifications"><h1 className="visually-hidden">Notifications</h1><NotificationLockedState /></section>;

  const filtersByApp = new Map(filters.map(filter => [notificationMuteKey(filter.sourceDeviceId, filter.packageName), filter]));
  const chips = notificationFilterChips(queryMatches, labels.appName);
  const statusFor = (notification: MirroredNotification): NotificationRowStatus | undefined => {
    const key = notificationKey(notification.target);
    const failure = failures.get(key);
    if (failure) return { kind: "failed", message: failure.message, onRetry: failure.canRetry && onRetryDismiss ? () => { rememberNeighbor([key]); onRetryDismiss(key); } : undefined };
    if (notification.dismissalPending && !notification.dismissible) return { kind: "refused" };
    return undefined;
  };
  const feedProps = { labels, filtersByApp, newKeys: newKeys.current, showPhone, query, writesPaused, onDismiss, onMute, onCopy: onCopyOneTimeCode, onAction: rememberNeighbor, statusFor };
  const groupRowProps: WaitingGroupRowProps = { labels, filtersByApp, showPhone: false, query, writesPaused, onDismiss, onMute, onCopy: onCopyOneTimeCode, onAction: rememberNeighbor };
  const toolbarDismissAll = dismissAll && {
    ...dismissAll,
    onDismissAll: () => {
      rememberNeighbor(shown.filter(notification => notification.dismissible && !notification.dismissalPending).map(notification => notificationKey(notification.target)), true);
      dismissAll.onDismissAll();
    },
  };
  const searchStatus = query
    ? `${shown.length} notifications match “${query}”`
    : appFilter ? `Showing ${shown.length} ${chips.find(chip => chip.packageName === appFilter)?.name ?? "app"} notifications` : "";
  // The feed also holds the waiting and blocker groups, which their toggles control, so it exists whenever either can show.
  const feedPresent = shown.length > 0 || groupsPresent || waitingCount > 0 || phoneBlocks.length > 0;
  const closedPhoneKeys = phoneBlocks.map(block => block.phoneKey).filter(key => !expandedPhones.has(key));

  return <section id="notifications-view" role="region" aria-label="Notifications">
    <h1 className="visually-hidden">Notifications</h1>
    <NotificationFilterBar
      chips={chips}
      appFilter={appFilter}
      onAppFilter={onAppFilter}
      arrangement={arrangement}
      onArrangement={setArrangement}
      dismissAll={toolbarDismissAll}
      waiting={<NotificationWaitingControl count={waitingCount} expanded={waitingOpen} onToggle={toggleWaiting} />}
    />
    <NotificationPhoneBlockers blocks={phoneBlocks} expandedPhones={expandedPhones} onTogglePhone={togglePhone} />
    {query && <p id="notifications-search-status">{shown.length} notification{shown.length === 1 ? "" : "s"} match{shown.length === 1 ? "es" : ""} “{query}”</p>}
    <div id="notifications-announcer" className="visually-hidden" aria-live="polite">{searchStatus}</div>
    <ActionAnnouncer announcement={announcement ?? localAnnouncement} />
    {feedPresent && <div id="notifications-feed" ref={feedRef} aria-label="Notification feed">
      <NotificationWaitingGroups source="blocker" groups={blockerGroups} rowProps={groupRowProps} closedPhoneKeys={closedPhoneKeys} />
      <NotificationWaitingGroups source="count" groups={countGroups} rowProps={groupRowProps} />
      {shown.length > 0 && (arrangement === "time"
        ? <NotificationSections notifications={shown} {...feedProps} />
        : <NotificationStacks stacks={stacks} expanded={expandedStacks} focusStackKey={focusStackKey} onExpanded={key => setExpandedStacks(current => current.has(key) ? new Set([...current].filter(value => value !== key)) : new Set([...current, key]))} onDismissMany={onDismissMany} {...feedProps} />)}
    </div>}
    {!shown.length && query && <SearchNoMatch id="notifications-no-match" noun="notifications" query={query} hint="Search covers the app name, phone, title and text." onClearQuery={onClearQuery} />}
    {!shown.length && !query && !groupsPresent && <NotificationEmptyState filters={filters} hasNotifications={notifications.length > 0} onSettings={onSettings} onManageApps={onManageApps} />}
    <NotificationActionNotice notice={notice} onDismiss={onDismissActionNotice} onClosed={() => { noticeClosed.current = true; }} />
  </section>;
}
