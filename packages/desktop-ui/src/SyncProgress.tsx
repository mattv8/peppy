import type { ReactElement } from "react";
import { useEffect, useRef, useState } from "react";
import { Check, Lock, LoaderCircle } from "lucide-react";
import type { SyncProgress as SyncProgressSnapshot } from "./bridge-contracts";

type SyncPhase = SyncProgressSnapshot["phase"];
type ActivePhase = Exclude<SyncPhase, "current">;

export const SHOW_DELAY_MS = 500;
export const HIDE_LINGER_MS = 3000;

export type SyncProgressProps = {
  /** Canonical `snapshot.syncProgress`; absent or `current` shows no banner. */
  progress?: SyncProgressSnapshot;
};

/** Compact, truthful phase labels. */
const TITLES: Record<ActivePhase, string> = {
  receiving: "Loading your history…",
  applying: "Organizing downloaded history…",
  waitingForKeys: "Waiting for vault keys to continue loading history…",
};

const PHASE_ANNOUNCEMENTS: Record<SyncPhase, string> = {
  receiving: "Loading your history.",
  applying: "Organizing downloaded history.",
  waitingForKeys: "Waiting for vault keys to continue loading history.",
  current: "Your history is up to date.",
};
const DOWNLOADING_COUNT = "%s of %s items";
const number = new Intl.NumberFormat();

function snapshotCounts(progress: SyncProgressSnapshot) {
  if (progress.snapshotReceived === undefined || progress.snapshotExpected === undefined) return;
  const received = Number(progress.snapshotReceived);
  const expected = Number(progress.snapshotExpected);
  // A zero total has no honest ratio; the banner then shows exactly what a live catch-up shows.
  if (!Number.isSafeInteger(received) || !Number.isSafeInteger(expected) || received < 0 || expected <= 0 || expected < received) return;
  return { received, expected };
}

function activePhase(phase: SyncPhase | undefined): ActivePhase | undefined {
  return phase === undefined || phase === "current" ? undefined : phase;
}

export function SyncProgress({ progress }: SyncProgressProps): ReactElement {
  const phase = progress?.phase;
  const active = activePhase(phase);
  const initialActive = active;
  const [displayedPhase, setDisplayedPhase] = useState<ActivePhase | undefined>(initialActive);
  const [announcement, setAnnouncement] = useState(initialActive ? PHASE_ANNOUNCEMENTS[initialActive] : "");
  const showTimerRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const hideTimerRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const latestPhaseRef = useRef(phase);
  const latestProgressRef = useRef(progress);
  const lastActiveProgressRef = useRef<SyncProgressSnapshot | undefined>(initialActive ? progress : undefined);
  const inactivePhaseRef = useRef<"current" | "absent" | undefined>(undefined);

  useEffect(() => {
    latestPhaseRef.current = phase;
    latestProgressRef.current = progress;
    if (active) lastActiveProgressRef.current = progress;

    if (displayedPhase) {
      if (active) {
        if (hideTimerRef.current) clearTimeout(hideTimerRef.current);
        hideTimerRef.current = undefined;
        inactivePhaseRef.current = undefined;
        if (displayedPhase !== active) {
          setDisplayedPhase(active);
          setAnnouncement(PHASE_ANNOUNCEMENTS[active]);
        }
        return;
      }

      const inactivePhase = phase === "current" ? "current" : "absent";
      inactivePhaseRef.current = inactivePhase;
      if (!hideTimerRef.current) {
        hideTimerRef.current = setTimeout(() => {
          setDisplayedPhase(undefined);
          if (inactivePhaseRef.current === "current") setAnnouncement(PHASE_ANNOUNCEMENTS.current);
          hideTimerRef.current = undefined;
          inactivePhaseRef.current = undefined;
        }, HIDE_LINGER_MS);
      }
      return;
    }

    if (!active) {
      if (showTimerRef.current) clearTimeout(showTimerRef.current);
      showTimerRef.current = undefined;
      return;
    }

    if (active === "waitingForKeys") {
      if (showTimerRef.current) clearTimeout(showTimerRef.current);
      showTimerRef.current = undefined;
      setDisplayedPhase(active);
      setAnnouncement(PHASE_ANNOUNCEMENTS[active]);
      return;
    }

    if (!showTimerRef.current) {
      showTimerRef.current = setTimeout(() => {
        const delayedPhase = activePhase(latestPhaseRef.current);
        if (delayedPhase) {
          setDisplayedPhase(delayedPhase);
          setAnnouncement(PHASE_ANNOUNCEMENTS[delayedPhase]);
        }
        showTimerRef.current = undefined;
      }, SHOW_DELAY_MS);
    }
  }, [active, displayedPhase, phase, progress]);

  useEffect(() => () => {
    if (showTimerRef.current) clearTimeout(showTimerRef.current);
    if (hideTimerRef.current) clearTimeout(hideTimerRef.current);
    showTimerRef.current = undefined;
    hideTimerRef.current = undefined;
  }, []);

  const cursorProgress = progress ?? lastActiveProgressRef.current;
  const complete = phase === "current";
  const counts = cursorProgress && snapshotCounts(cursorProgress);
  const countText = counts && displayedPhase === "receiving"
    ? DOWNLOADING_COUNT.replace("%s", number.format(counts.received)).replace("%s", number.format(counts.expected))
    : undefined;
  return <>
    {displayedPhase && cursorProgress && <section
      id="sync-progress"
      className="sync-progress"
      aria-label="History sync"
      data-sync-progress
      data-sync-phase={complete ? "current" : displayedPhase}
    >
      {complete
        ? <Check className="sync-progress-icon" size={14} strokeWidth={1.75} aria-hidden />
        : displayedPhase === "waitingForKeys"
        ? <Lock className="sync-progress-icon" size={14} strokeWidth={1.75} aria-hidden />
        : <LoaderCircle className="busy-indicator-icon sync-progress-icon" size={14} strokeWidth={1.75} aria-hidden />}
      <div className="sync-progress-main">
        <p className="sync-progress-title" data-sync-title>{complete ? "History is up to date" : TITLES[displayedPhase]}</p>
        {!complete && displayedPhase === "receiving" && counts && <progress id="sync-progress-bar" aria-label="Loading your history" value={counts.received} max={counts.expected} />}
        {!complete && displayedPhase === "applying" && <progress id="sync-progress-bar" aria-label="Organizing downloaded history" />}
        {!complete && countText && <p data-sync-count>{countText}</p>}
      </div>
    </section>}
    <span id="sync-progress-announcement" className="sr-only" role="status" aria-live="polite" data-sync-announcement>{announcement}</span>
  </>;
}
