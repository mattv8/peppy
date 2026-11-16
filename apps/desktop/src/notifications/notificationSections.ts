import type { MirroredNotification, NotificationTarget } from "../bridge/bridge";
import { notificationMuteKey, notificationTuple } from "./notificationIdentity";

export const notificationKey = (target: NotificationTarget) =>
  notificationTuple(target.sourceDeviceId, target.notificationKey, target.lifetime);

export type NotificationSectionId = "new" | "today" | "yesterday" | "older";
export type NotificationSection = { id: NotificationSectionId; label: string; items: MirroredNotification[] };

export type NotificationStackData = {
  key: string;
  sourceDeviceId: string;
  packageName: string;
  items: MirroredNotification[];
};

type DayBounds = { todayStart: number; yesterdayStart: number };

const localDayStart = (year: number, month: number, day: number) => new Date(year, month, day).getTime();

function dayBounds(now: number): DayBounds {
  const today = new Date(now);
  const year = today.getFullYear();
  const month = today.getMonth();
  const day = today.getDate();
  return {
    todayStart: localDayStart(year, month, day),
    yesterdayStart: localDayStart(year, month, day - 1),
  };
}

function startOfLocalDay(timestamp: number) {
  const date = new Date(timestamp);
  return localDayStart(date.getFullYear(), date.getMonth(), date.getDate());
}

function bucketFor(
  notification: MirroredNotification,
  newKeys: ReadonlySet<string>,
  bounds: DayBounds,
): NotificationSectionId {
  if (newKeys.has(notificationKey(notification.target))) return "new";
  const day = startOfLocalDay(notification.postedAt);
  // A phone clock running ahead can post into the future; keep those with today.
  if (day >= bounds.todayStart) return "today";
  if (day === bounds.yesterdayStart) return "yesterday";
  return "older";
}

/** Splits notifications into New, Today/Earlier today, Yesterday, and Older by local calendar day. */
export function sectionNotifications(
  list: MirroredNotification[],
  newKeys: ReadonlySet<string>,
  now: number,
): NotificationSection[] {
  const bounds = dayBounds(now);
  const buckets: Record<NotificationSectionId, MirroredNotification[]> = { new: [], today: [], yesterday: [], older: [] };
  for (const notification of list) buckets[bucketFor(notification, newKeys, bounds)].push(notification);
  const todayLabel = buckets.new.length > 0 ? "Earlier today" : "Today";
  const sections: NotificationSection[] = [
    { id: "new", label: "New", items: buckets.new },
    { id: "today", label: todayLabel, items: buckets.today },
    { id: "yesterday", label: "Yesterday", items: buckets.yesterday },
    { id: "older", label: "Older", items: buckets.older },
  ];
  return sections.filter(section => section.items.length > 0);
}

/** Groups notifications per phone and app; each stack is newest-first and stacks are ordered by their newest item. */
export function stackNotifications(list: MirroredNotification[]): NotificationStackData[] {
  const stacks = new Map<string, NotificationStackData>();
  for (const notification of list) {
    const { sourceDeviceId } = notification.target;
    const key = notificationMuteKey(sourceDeviceId, notification.packageName);
    const stack = stacks.get(key) ?? { key, sourceDeviceId, packageName: notification.packageName, items: [] };
    stack.items.push(notification);
    stacks.set(key, stack);
  }
  return [...stacks.values()]
    .map(stack => ({ ...stack, items: [...stack.items].sort((a, b) => b.postedAt - a.postedAt) }))
    .sort((a, b) => b.items[0].postedAt - a.items[0].postedAt);
}
