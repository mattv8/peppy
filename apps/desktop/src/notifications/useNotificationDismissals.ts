import { useEffect, useMemo, useRef, useState } from "react";
import type { DesktopBridge, MirroredNotification, NotificationDismissResult, NotificationTarget } from "@peppy/desktop-ui/bridge-contracts";
import {
  applyDismissResults,
  beginDismissals,
  classifyDismissalError,
  DISMISS_RETRY_DELAYS,
  type DismissalOverlay,
  markDismissalsFailed,
  markDismissalsRetrying,
  NOTIFICATION_DISMISS_LIMIT,
  reconcileDismissalOverlay,
  removeDismissals,
} from "./dismissalOverlay";
import {
  bulkFailureText,
  dismissedAnnouncement,
  dismissedManyAnnouncement,
  notDismissibleAnnouncement,
  notDismissibleManyAnnouncement,
  TRY_AGAIN_LABEL,
} from "./notificationDismissCopy";
import { notificationKey } from "./notificationSections";

export type BulkFailureNotice = {
  message: string;
  /** Keys of the rows the notice is about, so focus can move from them when the action runs. */
  keys: string[];
  action?: { label: string; onAction(): void };
};

/** The title and app name a row is announced with, as the user sees them. */
export type NotificationRowLabel = { title: string; appName: string };

export type NotificationDismissalOptions = {
  bridge: Pick<DesktopBridge, "dismiss_notifications">;
  notifications: MirroredNotification[];
  refresh(): Promise<void>;
  writesPaused: boolean;
  announce(message: string): void;
  onBulkFailure(notice: BulkFailureNotice | undefined): void;
  /** Display labels for a row. */
  label(row: MirroredNotification): NotificationRowLabel;
  /** Overrides the announcement for one row dismissed on its own, such as a row of a blocked phone. */
  singleAnnouncement?(row: MirroredNotification): string | undefined;
};

export type NotificationDismissals = {
  /**
   * Hides the targets at once, then records them. `single` means per-row failure reporting. Only the first
   * `NOTIFICATION_DISMISS_LIMIT` targets are sent. Returns false, sending nothing, when writes are paused or nothing was given.
   */
  dismiss(targets: NotificationTarget[], context: { single: boolean }): boolean;
  /** Re-dismisses a row whose last attempt failed. */
  retry(key: string): void;
  /** Rows to keep out of the list: pending dismissals, and in-flight or retrying ones. */
  hiddenKeys: ReadonlySet<string>;
  /** Per-row failures to show in place of the row. */
  failures: ReadonlyMap<string, { message: string; canRetry: boolean }>;
};

export function useNotificationDismissals(options: NotificationDismissalOptions): NotificationDismissals {
  const { notifications } = options;
  const [overlay, setOverlay] = useState<DismissalOverlay>({});
  // Refs hold the source of truth so async callbacks never read a stale render.
  const overlayRef = useRef<DismissalOverlay>({});
  const optionsRef = useRef(options);
  const mountedRef = useRef(true);
  const timersRef = useRef(new Set<ReturnType<typeof setTimeout>>());

  useEffect(() => {
    optionsRef.current = options;
  });

  useEffect(() => {
    mountedRef.current = true;
    const timers = timersRef.current;
    return () => {
      mountedRef.current = false;
      timers.forEach(timer => clearTimeout(timer));
      timers.clear();
    };
  }, []);

  const commit = (next: DismissalOverlay) => {
    overlayRef.current = next;
    setOverlay(next);
  };

  useEffect(() => {
    const next = reconcileDismissalOverlay(overlayRef.current, notifications);
    if (Object.keys(next).length !== Object.keys(overlayRef.current).length) commit(next);
  }, [notifications]);

  const schedule = (delay: number, run: () => void) => {
    const timer = setTimeout(() => {
      timersRef.current.delete(timer);
      run();
    }, delay);
    timersRef.current.add(timer);
  };

  const rowFor = (target: NotificationTarget) => optionsRef.current.notifications.find(row => notificationKey(row.target) === notificationKey(target));

  const handleFailure = (targets: NotificationTarget[], bulk: boolean, attempt: number, error: unknown) => {
    const { reason, autoRetry, canRetry } = classifyDismissalError(error);
    if (autoRetry && attempt < DISMISS_RETRY_DELAYS.length) {
      commit(markDismissalsRetrying(overlayRef.current, targets));
      schedule(DISMISS_RETRY_DELAYS[attempt], () => void send(targets, bulk, attempt + 1));
      return;
    }
    if (!bulk) {
      commit(markDismissalsFailed(overlayRef.current, targets, { reason, canRetry }));
      return;
    }
    commit(removeDismissals(overlayRef.current, targets));
    optionsRef.current.onBulkFailure({
      message: bulkFailureText(targets.length, reason),
      keys: targets.map(notificationKey),
      action: canRetry ? { label: TRY_AGAIN_LABEL, onAction: () => retryBulk(targets) } : undefined,
    });
  };

  const retryBulk = (targets: NotificationTarget[]) => {
    // Paused writes keep the notice, so its disabled action stays explained.
    if (optionsRef.current.writesPaused) return;
    optionsRef.current.onBulkFailure(undefined);
    dismiss(targets, { single: false });
  };

  const announceRefusals = (results: NotificationDismissResult[], bulk: boolean) => {
    const refused = results.filter(result => result.outcome === "notDismissible");
    if (refused.length === 0) return;
    const row = !bulk && refused.length === 1 ? rowFor(refused[0].target) : undefined;
    if (row) optionsRef.current.announce(notDismissibleAnnouncement(optionsRef.current.label(row).title));
    else optionsRef.current.announce(notDismissibleManyAnnouncement(refused.length));
  };

  const send = async (targets: NotificationTarget[], bulk: boolean, attempt: number) => {
    let results: NotificationDismissResult[];
    try {
      results = await optionsRef.current.bridge.dismiss_notifications(targets);
    } catch (error) {
      if (mountedRef.current) handleFailure(targets, bulk, attempt, error);
      return;
    }
    if (!mountedRef.current) return;
    // Reconcile against the newest snapshot now, so a row already pending or gone does not wait for the next one.
    const applied = applyDismissResults(overlayRef.current, targets, results);
    commit(reconcileDismissalOverlay(applied, optionsRef.current.notifications));
    announceRefusals(results, bulk);
    void optionsRef.current.refresh().catch(() => undefined);
  };

  const announceDismissal = (targets: NotificationTarget[], bulk: boolean) => {
    if (bulk) {
      optionsRef.current.announce(dismissedManyAnnouncement(targets.length));
      return;
    }
    const row = rowFor(targets[0]);
    if (!row) {
      optionsRef.current.announce("Dismissed notification.");
      return;
    }
    const custom = optionsRef.current.singleAnnouncement?.(row);
    const { title, appName } = optionsRef.current.label(row);
    optionsRef.current.announce(custom ?? dismissedAnnouncement(title, appName));
  };

  const dismiss = (targets: NotificationTarget[], context: { single: boolean }): boolean => {
    if (optionsRef.current.writesPaused || targets.length === 0) return false;
    const sent = targets.slice(0, NOTIFICATION_DISMISS_LIMIT);
    const bulk = !context.single && sent.length > 1;
    commit(beginDismissals(overlayRef.current, sent));
    announceDismissal(sent, bulk);
    void send(sent, bulk, 0);
    return true;
  };

  const retry = (key: string) => {
    const entry = overlayRef.current[key];
    if (entry?.phase === "failed") dismiss([entry.target], { single: true });
  };

  const hiddenKeys = useMemo(() => new Set([
    ...Object.entries(overlay).filter(([, entry]) => entry.phase !== "failed").map(([key]) => key),
    ...notifications.filter(row => row.dismissalPending && row.dismissible).map(row => notificationKey(row.target)),
  ]), [overlay, notifications]);

  const failures = useMemo(() => new Map(
    Object.entries(overlay)
      .filter(([, entry]) => entry.phase === "failed")
      .map(([key, entry]) => [key, { message: entry.reason ?? "", canRetry: entry.canRetry === true }]),
  ), [overlay]);

  return { dismiss, retry, hiddenKeys, failures };
}
