import { useCallback, useEffect, useState } from "react";
import { readAccountAccess, type AccountNoticeDto } from "@peppy/desktop-ui/bridge-contracts";
import { bridge } from "./bridge";

const REFRESH_THROTTLE_MS = 60 * 1000;
const SNOOZE_MS = 24 * 60 * 60 * 1000;

const snoozeStorageKey = (snoozeKey: string) => `peppy.systemRibbon.snooze.${snoozeKey}`;

const readSnoozeUntil = (snoozeKey: string): number => {
  try {
    return Number(localStorage.getItem(snoozeStorageKey(snoozeKey)));
  } catch {
    return 0;
  }
};

/** A per-device UI preference, not a secret; a missing or unreadable store simply never snoozes. */
export function isAccountNoticeSnoozed(snoozeKey: string, now: number = Date.now()): boolean {
  return now < readSnoozeUntil(snoozeKey);
}

/** Returns the snooze expiry in epoch milliseconds. */
export function snoozeAccountNotice(snoozeKey: string, now: number = Date.now()): number {
  const until = now + SNOOZE_MS;
  try {
    localStorage.setItem(snoozeStorageKey(snoozeKey), String(until));
  } catch {
    // The in-session snooze in useAccountNotice still hides the notice until it expires.
  }
  return until;
}

type SessionSnooze = { snoozeKey: string; until: number };

/**
 * The last good account notice. A fetch runs on mount and on focus or visibility, throttled to once a minute.
 * Temporary failures keep the cached notice; only a definitive none or unsupported result clears it.
 * A change of `identityKey` (device or session) drops the cached notice and any snooze, and late replies
 * for the previous identity are ignored.
 */
export function useAccountNotice(identityKey: string | null): { notice: AccountNoticeDto | null; dismiss: () => void } {
  const [latest, setLatest] = useState<AccountNoticeDto | null>(null);
  const [sessionSnooze, setSessionSnooze] = useState<SessionSnooze | null>(null);
  const [cachedIdentity, setCachedIdentity] = useState(identityKey);

  if (cachedIdentity !== identityKey) {
    setCachedIdentity(identityKey);
    setLatest(null);
    setSessionSnooze(null);
  }

  useEffect(() => {
    let current = true;
    let lastStartedAt = Number.NEGATIVE_INFINITY;
    const refresh = () => {
      const now = Date.now();
      if (now - lastStartedAt < REFRESH_THROTTLE_MS) return;
      lastStartedAt = now;
      void readAccountAccess(bridge).then(result => {
        if (!current) return;
        if (result.kind === "notice") setLatest(result.notice);
        else if (result.kind === "none" || result.kind === "unsupported") setLatest(null);
      });
    };
    const refreshWhenVisible = () => {
      if (document.visibilityState === "visible") refresh();
    };
    refresh();
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refreshWhenVisible);
    return () => {
      current = false;
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refreshWhenVisible);
    };
  }, [identityKey]);

  const now = Date.now();
  const snoozed = latest !== null && latest.dismissible && (
    (sessionSnooze?.snoozeKey === latest.snoozeKey && now < sessionSnooze.until)
    || isAccountNoticeSnoozed(latest.snoozeKey, now)
  );
  const notice = snoozed ? null : latest;

  const dismiss = useCallback(() => {
    if (!latest?.dismissible) return;
    setSessionSnooze({ snoozeKey: latest.snoozeKey, until: snoozeAccountNotice(latest.snoozeKey) });
  }, [latest]);

  return { notice, dismiss };
}
