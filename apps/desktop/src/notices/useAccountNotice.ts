import { useCallback, useEffect, useState } from "react";
import { readAccountAccess, type AccountAccessResult, type AccountNoticeDto } from "@peppy/desktop-ui/bridge-contracts";
import { bridge } from "../bridge/bridge";

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

/** What the last definitive account-access answer allows: no notice or a warning is read-write, an error is read-only. */
export type AccountAccessLevel = "unknown" | "read_write" | "read_only";

/** A temporary failure keeps the previous level; a server without account access reports none. */
function nextAccessLevel(result: AccountAccessResult, previous: AccountAccessLevel): AccountAccessLevel {
  if (result.kind === "none") return "read_write";
  if (result.kind === "notice") return result.notice.severity === "error" ? "read_only" : "read_write";
  if (result.kind === "unsupported") return "unknown";
  return previous;
}

/**
 * The last good account notice and access level. A fetch runs on mount and on focus or visibility, throttled to once a minute.
 * Temporary failures keep the cached notice and access level; only a definitive none or unsupported result clears the notice.
 * A change of `identityKey` (device or session) drops the cached notice, access level and any snooze, and late replies
 * for the previous identity are ignored.
 */
export function useAccountNotice(identityKey: string | null): { notice: AccountNoticeDto | null; accessLevel: AccountAccessLevel; dismiss: () => void } {
  const [latest, setLatest] = useState<AccountNoticeDto | null>(null);
  const [accessLevel, setAccessLevel] = useState<AccountAccessLevel>("unknown");
  const [sessionSnooze, setSessionSnooze] = useState<SessionSnooze | null>(null);
  const [cachedIdentity, setCachedIdentity] = useState(identityKey);

  if (cachedIdentity !== identityKey) {
    setCachedIdentity(identityKey);
    setLatest(null);
    setAccessLevel("unknown");
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
        setAccessLevel(previous => nextAccessLevel(result, previous));
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

  return { notice, accessLevel, dismiss };
}
