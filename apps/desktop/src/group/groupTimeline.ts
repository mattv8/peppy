import type { GroupNoticeItem } from "./groupNotices";

export type TimelineGroup = { id: string; messages: readonly { timestamp: string }[] };

/** One row of the message timeline: a message group, or a group notice placed by its time. */
export type TimelineItem<G extends TimelineGroup> =
  | { kind: "group"; id: string; group: G }
  | { kind: "notice"; id: string; notice: GroupNoticeItem };

/**
 * Places each notice before the first group whose first message is at or after the notice's time.
 * Notices later than every message go last. Groups keep their order.
 */
export function interleaveGroupNotices<G extends TimelineGroup>(groups: readonly G[], notices: readonly GroupNoticeItem[]): TimelineItem<G>[] {
  const pending = [...notices].sort((a, b) => a.atMs - b.atMs);
  const items: TimelineItem<G>[] = [];
  for (const group of groups) {
    const groupAtMs = Number(group.messages[0]?.timestamp);
    while (pending.length && pending[0].atMs <= groupAtMs) items.push(noticeItem(pending.shift()!));
    items.push({ kind: "group", id: group.id, group });
  }
  for (const notice of pending) items.push(noticeItem(notice));
  return items;
}

const noticeItem = <G extends TimelineGroup>(notice: GroupNoticeItem): TimelineItem<G> => ({ kind: "notice", id: notice.id, notice });
