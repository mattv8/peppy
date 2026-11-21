import type { ReactNode } from "react";
import { Clock, Loader2, TriangleAlert } from "@peppy/desktop-ui";

export type ContactsStatusKind = "unsaved" | "error" | "removed" | "sync" | "readiness" | "scan" | "unavailable";
export type ContactsRepairState = "rebuilding" | "repairWaiting" | "failed" | "needsRepair";

export type ContactsStatusStrip = { kind: ContactsStatusKind; node: ReactNode };

/**
 * The single persistent strip above the contacts list. Strips arrive in priority order and only the first one present
 * is shown; lower-priority strips wait until the higher one clears.
 */
/** The strip the status slot shows: the first present one in priority order. */
export function activeContactsStatus(strips: Array<ContactsStatusStrip | false | null | undefined>): ContactsStatusStrip | undefined {
  return strips.find((strip): strip is ContactsStatusStrip => Boolean(strip));
}

export function ContactsStatus({ strips }: { strips: Array<ContactsStatusStrip | false | null | undefined> }) {
  const active = activeContactsStatus(strips);
  if (!active) return null;
  return <div id="contacts-status" data-status-kind={active.kind}>{active.node}</div>;
}

const SYNC_ICONS = {
  rebuilding: Loader2,
  repairWaiting: Clock,
  needsRepair: Clock,
  failed: TriangleAlert,
} satisfies Record<ContactsRepairState, typeof Clock>;

export function SyncStatusStrip({ state, text }: { state: ContactsRepairState; text: string }) {
  const Icon = SYNC_ICONS[state];
  return <p id="contacts-sync-status" role="status" data-sync-state={state}>
    <Icon className={state === "rebuilding" ? "contacts-sync-icon" : undefined} size={14} aria-hidden />
    <span>{text}</span>
  </p>;
}
