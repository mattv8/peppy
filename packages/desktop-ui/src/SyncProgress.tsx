import type { ReactElement } from "react";
import { useEffect, useRef, useState } from "react";
import { Lock, LoaderCircle } from "lucide-react";
import type { SyncProgress as SyncProgressSnapshot } from "./bridge-contracts";

type SyncPhase = SyncProgressSnapshot["phase"];
type ActivePhase = Exclude<SyncPhase, "current">;

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

/** Polite announcements fire only when the phase changes, never on cursor updates. */
const PHASE_ANNOUNCEMENTS: Record<SyncPhase, string> = {
  receiving: "Loading your history.",
  applying: "Organizing downloaded history.",
  waitingForKeys: "Waiting for vault keys to continue loading history.",
  current: "Your history is up to date.",
};

function usePhaseAnnouncement(phase: SyncPhase | undefined): string {
  const lastPhaseRef = useRef(phase);
  const [announcement, setAnnouncement] = useState("");
  useEffect(() => {
    if (lastPhaseRef.current === phase) return;
    lastPhaseRef.current = phase;
    if (phase) setAnnouncement(PHASE_ANNOUNCEMENTS[phase]);
  }, [phase]);
  return announcement;
}

export function SyncProgress({ progress }: SyncProgressProps): ReactElement {
  const phase = progress?.phase;
  const announcement = usePhaseAnnouncement(phase);
  const active = phase !== undefined && phase !== "current" ? phase : undefined;
  return <>
    {progress && active && <section
      id="sync-progress"
      className="sync-progress"
      aria-label="History sync"
      data-sync-progress
      data-sync-phase={active}
    >
      {active === "waitingForKeys"
        ? <Lock className="sync-progress-icon" size={14} strokeWidth={1.75} aria-hidden />
        : <LoaderCircle className="busy-indicator-icon sync-progress-icon" size={14} strokeWidth={1.75} aria-hidden />}
      <div className="sync-progress-main">
        <p className="sync-progress-title" data-sync-title>{TITLES[active]}</p>
        <details className="sync-progress-details" data-sync-details>
          <summary>Sync details</summary>
          <dl className="sync-progress-cursors">
            <dt>Received through</dt>
            <dd data-sync-received>{progress.receivedThrough}</dd>
            <dt>Processed through</dt>
            <dd data-sync-processed>{progress.processedThrough}</dd>
            {progress.targetHighWater !== undefined && <>
              <dt>Target</dt>
              <dd data-sync-target>{progress.targetHighWater}</dd>
            </>}
          </dl>
          <p className="sync-progress-note">You can use conversations as they become available.</p>
        </details>
      </div>
    </section>}
    <span id="sync-progress-announcement" className="sr-only" role="status" aria-live="polite" data-sync-announcement>{announcement}</span>
  </>;
}
