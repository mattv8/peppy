import type { MirroredNotification, NotificationDismissResult, NotificationTarget } from "@peppy/desktop-ui/bridge-contracts";
import { errorCode } from "../bridge/bridgeErrors";
import { GENERIC_FAILURE_REASON, LOCKED_REASON, UPDATE_BUSY_REASON, UPDATE_IN_PROGRESS_REASON } from "./notificationDismissCopy";
import { notificationKey } from "./notificationSections";

/** Waits before each automatic retry of `update-in-progress`; the third failure is final. */
export const DISMISS_RETRY_DELAYS = [2000, 8000, 20000] as const;

/** Most targets one `dismiss_notifications` call accepts (the core batch limit). */
export const NOTIFICATION_DISMISS_LIMIT = 100;

/** The newest rows (by post time, ties keep their given order), as targets, up to `limit`. Callers pass only rows they may dismiss. */
export function newestDismissTargets(rows: MirroredNotification[], limit = NOTIFICATION_DISMISS_LIMIT): NotificationTarget[] {
  return [...rows]
    .sort((left, right) => right.postedAt - left.postedAt)
    .slice(0, limit)
    .map(row => row.target);
}

export type DismissalEntry = {
  target: NotificationTarget;
  phase: "committing" | "recorded" | "retrying" | "failed";
  reason?: string;
  canRetry?: boolean;
};

/** Keyed by `notificationKey(target)`. Only `failed` entries are visible; all others hide their row. */
export type DismissalOverlay = Record<string, DismissalEntry>;

export function beginDismissals(overlay: DismissalOverlay, targets: NotificationTarget[]): DismissalOverlay {
  const next = { ...overlay };
  for (const target of targets) next[notificationKey(target)] = { target, phase: "committing" };
  return next;
}

export function markDismissalsRetrying(overlay: DismissalOverlay, targets: NotificationTarget[]): DismissalOverlay {
  return mapPresent(overlay, targets, entry => ({ ...entry, phase: "retrying" }));
}

export function markDismissalsFailed(overlay: DismissalOverlay, targets: NotificationTarget[], failure: { reason: string; canRetry: boolean }): DismissalOverlay {
  return mapPresent(overlay, targets, entry => ({ ...entry, phase: "failed", reason: failure.reason, canRetry: failure.canRetry }));
}

export function removeDismissals(overlay: DismissalOverlay, targets: NotificationTarget[]): DismissalOverlay {
  const next = { ...overlay };
  for (const target of targets) delete next[notificationKey(target)];
  return next;
}

function mapPresent(overlay: DismissalOverlay, targets: NotificationTarget[], update: (entry: DismissalEntry) => DismissalEntry): DismissalOverlay {
  const next = { ...overlay };
  for (const target of targets) {
    const key = notificationKey(target);
    const entry = next[key];
    if (entry) next[key] = update(entry);
  }
  return next;
}

/**
 * Applies one call's results to the targets it sent. A sent target with no result failed; `gone`, `muted` and
 * `notDismissible` drop their entry (the row returns or is already absent).
 */
export function applyDismissResults(overlay: DismissalOverlay, sent: NotificationTarget[], results: NotificationDismissResult[]): DismissalOverlay {
  const outcomes = new Map(results.map(result => [notificationKey(result.target), result.outcome]));
  const next = { ...overlay };
  for (const target of sent) {
    const key = notificationKey(target);
    const entry = next[key];
    if (!entry) continue;
    const outcome = outcomes.get(key);
    if (outcome === undefined) next[key] = { ...entry, phase: "failed", reason: GENERIC_FAILURE_REASON, canRetry: true };
    else if (outcome === "recorded" || outcome === "alreadyRecorded") next[key] = { ...entry, phase: "recorded" };
    else delete next[key];
  }
  return next;
}

/**
 * Drops recorded and failed entries once their row is pending or gone. A recorded row returns only as a refusal;
 * a failed row with nothing left to retry no longer needs its failure shown.
 */
export function reconcileDismissalOverlay(overlay: DismissalOverlay, notifications: MirroredNotification[]): DismissalOverlay {
  const rows = new Map(notifications.map(row => [notificationKey(row.target), row]));
  return Object.fromEntries(Object.entries(overlay).filter(([key, entry]) => {
    if (entry.phase !== "recorded" && entry.phase !== "failed") return true;
    const row = rows.get(key);
    return row !== undefined && !row.dismissalPending;
  }));
}

export function classifyDismissalError(error: unknown): { reason: string; autoRetry: boolean; canRetry: boolean } {
  switch (errorCode(error)) {
    case "update-in-progress":
      return { reason: UPDATE_IN_PROGRESS_REASON, autoRetry: true, canRetry: true };
    case "update-busy":
      return { reason: UPDATE_BUSY_REASON, autoRetry: false, canRetry: false };
    case "locked":
    case "credentials-required":
      return { reason: LOCKED_REASON, autoRetry: false, canRetry: true };
    default:
      return { reason: GENERIC_FAILURE_REASON, autoRetry: false, canRetry: true };
  }
}
