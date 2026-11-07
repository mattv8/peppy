import { useEffect, useRef } from "react";

export const SETUP_POLL_INTERVAL_MS = 5_000;
export const MAX_SETUP_CHECKS = 60;
/** After the fast budget, only every Nth tick checks (every 30 s at the 5 s interval). */
export const SLOW_CHECK_EVERY_TICKS = 6;

/**
 * While the server client is not set up, re-checks it on an interval. Each visible tick
 * calls `check`; once it resolves true, `onReady` runs and polling stops. After the fast
 * budget is spent it keeps checking at a slower rate, so finishing setup is always
 * noticed. The fast budget resets when the window regains focus or becomes visible again.
 */
export function useServerClientWatch(active: boolean, check: () => Promise<boolean>, onReady: () => void) {
  const checkRef = useRef(check);
  const readyRef = useRef(onReady);

  useEffect(() => {
    checkRef.current = check;
    readyRef.current = onReady;
  });

  useEffect(() => {
    if (!active) return;
    let checks = 0;
    let slowTicks = 0;
    let inFlight = false;
    let stopped = false;

    const stop = () => {
      stopped = true;
      clearInterval(interval);
    };

    const skipSlowTick = () => {
      if (checks < MAX_SETUP_CHECKS) return false;
      slowTicks += 1;
      return slowTicks % SLOW_CHECK_EVERY_TICKS !== 0;
    };

    const tick = async () => {
      if (stopped || inFlight) return;
      if (document.visibilityState !== "visible") return;
      if (skipSlowTick()) return;
      checks += 1;
      inFlight = true;
      try {
        const ready = await checkRef.current();
        if (ready && !stopped) {
          stop();
          readyRef.current();
        }
      } catch {
        // A failed check keeps polling; the next due tick retries.
      } finally {
        inFlight = false;
      }
    };

    const restart = () => {
      checks = 0;
      slowTicks = 0;
      void tick();
    };
    const onVisibilityChange = () => {
      if (document.visibilityState === "visible") restart();
    };

    const interval = setInterval(() => void tick(), SETUP_POLL_INTERVAL_MS);
    window.addEventListener("focus", restart);
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      stop();
      window.removeEventListener("focus", restart);
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [active]);
}
