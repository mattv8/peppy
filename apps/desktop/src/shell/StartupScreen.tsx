import { useEffect, useRef, useState } from "react";
import { Check, CircleAlert, Loader2 } from "@peppy/desktop-ui";
import type { LaunchPhase, LaunchProgress } from "@peppy/generated/launch-progress";
import "./startup-screen.css";

const SLOW_MS = 10_000;
const titles: Record<LaunchPhase, string> = {
  starting: "Starting Peppy…",
  unlocking: "Unlocking your messages…",
  opening: "Opening your messages…",
  loading: "Loading conversations…",
  updating: "Updating your messages for this version…",
  ready: "Loading conversations…",
};
const labels: Record<LaunchPhase, string> = {
  starting: "Starting", unlocking: "Unlocking", opening: "Opening your messages",
  loading: "Loading conversations", updating: "Updating your messages", ready: "Loading conversations",
};
const numberFormat = new Intl.NumberFormat();
const formatUpdateCount = (completed: number, total: number) =>
  `${numberFormat.format(completed)} of ${numberFormat.format(total)}`;

export function formatDuration(ms: number): string {
  if (ms < 100) return "<0.1 s";
  if (ms < 10_000) return `${(ms / 1000).toFixed(1)} s`;
  const seconds = Math.floor(ms / 1000);
  return seconds < 60 ? `${seconds} s` : `${Math.floor(seconds / 60)} min ${seconds % 60} s`;
}

type StartupScreenProps = {
  variant: "main" | "composer";
  progress?: LaunchProgress;
  error?: { title: string; message: string; onRetry: () => void };
};

export function StartupScreen({ variant, progress, error }: StartupScreenProps) {
  const [mountedAt] = useState(() => Date.now());
  const [now, setNow] = useState(() => Date.now());
  const [slow, setSlow] = useState(false);
  const [announcement, setAnnouncement] = useState("");
  const previousAnnouncement = useRef("");
  const screenRef = useRef<HTMLDivElement>(null);
  const [progressClock, setProgressClock] = useState(() => ({ progress, receivedAt: Date.now() }));
  useEffect(() => { setProgressClock({ progress, receivedAt: Date.now() }); }, [progress]);
  useEffect(() => {
    if (error) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [error]);
  const elapsed = progress && progress.phase !== "ready"
    ? progress.elapsedMs + Math.max(0, now - (progressClock.progress === progress ? progressClock.receivedAt : now))
    : now - mountedAt;
  const isSlow = slow || elapsed >= SLOW_MS;
  useEffect(() => {
    if (isSlow && !slow) setSlow(true);
  }, [isSlow, slow]);
  const phase = progress?.phase ?? "opening";
  const updating = phase === "updating";
  const update = progress?.update;
  const title = titles[phase];
  const announcementKey = `${phase}:${isSlow && !updating}`;
  useEffect(() => {
    if (announcementKey === previousAnnouncement.current) return;
    previousAnnouncement.current = announcementKey;
    setAnnouncement(`${title}${isSlow && !updating ? " This is taking longer than usual. Peppy is still working." : ""}`);
  }, [announcementKey, title, isSlow, updating]);
  const completedDuration = progress?.steps.reduce((sum, step) => sum + step.durationMs, 0) ?? 0;

  return <div ref={screenRef} id="startup-screen" tabIndex={-1} data-startup-phase={phase} data-startup-variant={variant} data-startup-slow={isSlow}>
    <span id="startup-announcer" className="sr-only" role="status" aria-live="polite">{announcement}</span>
    {error ? <div id="startup-error" className="startup-error" role="alert" aria-labelledby="startup-title">
      <CircleAlert size={22} aria-hidden />
      <p id="startup-title">{error.title}</p>
      <p id="startup-detail">{error.message}</p>
      <button id="startup-retry" className="primary-button" type="button" onClick={() => { screenRef.current?.focus(); error.onRetry(); }}>Try again</button>
    </div> : <div id="startup-content" className="startup-stack">
      <Loader2 className="busy-indicator-icon startup-spinner" size={24} aria-hidden />
      <p id="startup-title">{title}</p>
      {updating && update && update.total > 0 && <>
        <progress id="startup-progress" aria-labelledby="startup-title" value={update.completed} max={update.total} />
        <p id="startup-count">{formatUpdateCount(update.completed, update.total)}</p>
      </>}
      {(updating || isSlow) && <p id="startup-detail">{updating ? "This happens once after an update." : "This is taking longer than usual. Peppy is still working."}</p>}
      {variant === "main" && isSlow && !updating && phase !== "ready" && Boolean(progress?.steps.length) && <ul id="startup-steps">
        {progress!.steps.map((step, index) => <li id={`startup-step-${step.phase}`} key={index} className="startup-step-completed">
          <Check size={14} strokeWidth={1.75} aria-hidden />{labels[step.phase]}<span>{formatDuration(step.durationMs)}</span>
        </li>)}
        {!progress!.steps.some(step => step.phase === phase) && <li id={`startup-step-${phase}`} aria-current="step" className="startup-step-current">
          <Loader2 size={14} strokeWidth={1.75} className="busy-indicator-icon" aria-hidden />{labels[phase]}<span>{formatDuration(Math.max(0, elapsed - completedDuration))}</span>
        </li>}
      </ul>}
    </div>}
  </div>;
}
