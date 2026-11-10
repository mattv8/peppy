import type { GroupNoticeView } from "@peppy/desktop-ui/bridge-contracts";
import { peppyCopy } from "../generated/peppyCopy";
import { fillCopy } from "./groupCopy";
import { displayGroupName } from "./groupName";

export type GroupNoticeKind = "name" | "added" | "removed";

export type GroupNoticeItem = {
  id: string;
  kind: GroupNoticeKind;
  /** Stable per-notice revision; the timestamp of the change. */
  revision: string;
  atMs: number;
  /** New group name for a `name` notice; undefined means the name was removed. */
  name?: string;
  addresses: readonly string[];
};

/** Visible names are cut at this length in the timeline; the full name stays in `title`. */
export const NOTICE_NAME_MAX_LENGTH = 60;

/**
 * Timeline items from the core's notices, in the order core sent them. Core owns the diff; this
 * only maps each notice to one item. A membership notice without a member is dropped.
 */
export function groupNoticeItems(notices: readonly GroupNoticeView[] = []): GroupNoticeItem[] {
  return notices.flatMap((notice): GroupNoticeItem[] => {
    const atMs = Number(notice.atMs);
    if (!Number.isSafeInteger(atMs)) return [];
    if (notice.kind === "renamed") {
      return [{ id: `group-notice-name-${notice.atMs}`, kind: "name", revision: notice.atMs, atMs, name: displayGroupName(notice.name), addresses: [] }];
    }
    if (!notice.member) return [];
    return [{ id: `group-notice-${notice.kind}-${notice.atMs}-${notice.member}`, kind: notice.kind, revision: notice.atMs, atMs, addresses: [notice.member] }];
  });
}

export type GroupNoticeCopy = {
  template: string;
  values: Record<string, string>;
  /** Full text when the visible text was shortened. */
  title?: string;
};

/**
 * Catalog template and values for one notice. Names are returned as values so the component can
 * wrap them in `<bdi>`.
 */
export function groupNoticeCopy(item: GroupNoticeItem, displayName: (address: string) => string): GroupNoticeCopy {
  if (item.kind === "name") return nameCopy(item.name);
  const names = item.addresses.map(displayName);
  const template = item.kind === "removed" ? peppyCopy.group_notice_removed_one : peppyCopy.group_notice_added_one;
  return { template, values: { name: names[0] ?? "" } };
}

export function groupNoticeText(item: GroupNoticeItem, displayName: (address: string) => string): string {
  const copy = groupNoticeCopy(item, displayName);
  return fillCopy(copy.template, copy.values);
}

function nameCopy(name: string | undefined): GroupNoticeCopy {
  if (!name) return { template: peppyCopy.group_notice_name_removed, values: {} };
  const visible = truncate(name, NOTICE_NAME_MAX_LENGTH);
  return {
    template: peppyCopy.group_notice_name_changed,
    values: { name: visible },
    ...(visible === name ? {} : { title: fillCopy(peppyCopy.group_notice_name_changed, { name }) }),
  };
}

function truncate(value: string, max: number): string {
  const characters = Array.from(value);
  return characters.length <= max ? value : `${characters.slice(0, max - 1).join("")}…`;
}
