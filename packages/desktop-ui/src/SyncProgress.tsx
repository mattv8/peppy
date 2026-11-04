import type { ReactElement } from "react";
import { useEffect, useRef, useState } from "react";
import { Lock, LoaderCircle } from "lucide-react";
import type { SyncProgress as SyncProgressSnapshot } from "./bridge-contracts";

type SyncPhase = SyncProgressSnapshot["phase"];
type ActivePhase = Exclude<SyncPhase, "current">;

export const SHOW_DELAY_MS = 500;
export const HIDE_LINGER_MS = 3000;

export type SyncProgressProps = {
  /** Canonical `snapshot.syncProgress`; absent or `current` shows no banner. */
  progress?: SyncProgressSnapshot;
};

/** Compact, truthful phase labels; exact cursors live in the details disclosure. */
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
  return <>
    {displayedPhase && cursorProgress && <section
      id="sync-progress"
      className="sync-progress"
      aria-label="History sync"
      data-sync-progress
      data-sync-phase={displayedPhase}
    >
      {displayedPhase === "waitingForKeys"
        ? <Lock className="sync-progress-icon" size={14} strokeWidth={1.75} aria-hidden />
        : <LoaderCircle className="busy-indicator-icon sync-progress-icon" size={14} strokeWidth={1.75} aria-hidden />}
      <div className="sync-progress-main">
        <p className="sync-progress-title" data-sync-title>{TITLES[displayedPhase]}</p>
        <details className="sync-progress-details" data-sync-details>
          <summary>Sync details</summary>
          <dl className="sync-progress-cursors">
            <dt>Received through</dt>
            <dd data-sync-received>{cursorProgress.receivedThrough}</dd>
            <dt>Processed through</dt>
            <dd data-sync-processed>{cursorProgress.processedThrough}</dd>
            {cursorProgress.targetHighWater !== undefined && <>
              <dt>Target</dt>
              <dd data-sync-target>{cursorProgress.targetHighWater}</dd>
            </>}
          </dl>
          <p className="sync-progress-note">You can use conversations as they become available.</p>
        </details>
      </div>
    </section>}
    <span id="sync-progress-announcement" className="sr-only" role="status" aria-live="polite" data-sync-announcement>{announcement}</span>
  </>;
}
