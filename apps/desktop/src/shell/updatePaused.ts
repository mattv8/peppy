import type { DesktopSnapshot } from "@peppy/desktop-ui/bridge-contracts";
import { SHOW_DELAY_MS } from "@peppy/desktop-ui";
import { useEffect, useState } from "react";

export function isWritesPaused(snapshot?: Pick<DesktopSnapshot, "writesPaused"> | null): boolean {
  return snapshot?.writesPaused === true;
}

export function useDelayedWritesPaused(paused: boolean) {
  const [elapsed, setElapsed] = useState(false);
  useEffect(() => {
    if (!paused) {
      setElapsed(false);
      return;
    }
    const timer = setTimeout(() => setElapsed(true), SHOW_DELAY_MS);
    return () => clearTimeout(timer);
  }, [paused]);
  return { paused, visibleNotice: paused && elapsed };
}
