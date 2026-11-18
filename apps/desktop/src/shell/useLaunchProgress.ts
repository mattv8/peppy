import { useEffect, useState } from "react";
import type { LaunchProgress } from "@peppy/generated/launch-progress";
import type { DesktopBridge } from "../bridge/bridge";

export function useLaunchProgress(bridge: Pick<DesktopBridge, "launch_progress" | "subscribe_launch_progress">): LaunchProgress | undefined {
  const [progress, setProgress] = useState<LaunchProgress>();

  useEffect(() => {
    if (!bridge.launch_progress || !bridge.subscribe_launch_progress) return;
    let active = true;
    let newestElapsed = -1;
    let stopped = false;
    let readyReceived = false;
    let unsubscribe: (() => void) | undefined;
    const stop = () => {
      if (!stopped && unsubscribe) { stopped = true; unsubscribe(); }
    };
    const accept = (value: LaunchProgress) => {
      if (!active || value.elapsedMs < newestElapsed) return;
      newestElapsed = value.elapsedMs;
      setProgress(value);
      if (value.phase === "ready") { readyReceived = true; stop(); }
    };
    const subscription = bridge.subscribe_launch_progress(accept);
    unsubscribe = subscription;
    if (readyReceived) stop();
    void (subscription.ready ?? Promise.resolve())
      .then(() => active && !stopped ? bridge.launch_progress!() : undefined)
      .then(value => {
        if (value && value.elapsedMs > newestElapsed) accept(value);
      })
      .catch(() => {});
    return () => { active = false; stop(); };
  }, [bridge]);

  return progress;
}
