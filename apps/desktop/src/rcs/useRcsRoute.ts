import { useEffect, useState } from "react";
import { currentRcsRoute, type RcsRoute, type RcsRouteInput } from "./rcsRoute";

const MAX_TIMEOUT_MS = 2_147_483_647;

export function useRcsRoute(input: Omit<RcsRouteInput, "nowMs">): RcsRoute {
  const [nowMs, setNowMs] = useState(() => Date.now());
  const route = currentRcsRoute({ ...input, nowMs });

  useEffect(() => {
    const refresh = () => setNowMs(Date.now());
    refresh();
    const onVisibilityChange = () => {
      if (document.visibilityState === "visible") refresh();
    };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [input.activeConversationId, input.gatewayId, input.locked, input.snapshot]);

  useEffect(() => {
    if (!route.current) return;
    const timeout = window.setTimeout(() => setNowMs(Date.now()), Math.min(Math.max(route.expiresAtMs - nowMs, 0), MAX_TIMEOUT_MS));
    return () => window.clearTimeout(timeout);
  }, [nowMs, route]);

  return route;
}
