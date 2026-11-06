import { useEffect, useState } from "react";
import { currentRcsRoute, type RcsRoute, type RcsRouteInput } from "./rcsRoute";

const MAX_TIMEOUT_MS = 2_147_483_647;

export function useRcsRoute(input: Omit<RcsRouteInput, "nowMs">): RcsRoute {
  const [, setRenderVersion] = useState(0);
  const route = currentRcsRoute({ ...input, nowMs: Date.now() });

  useEffect(() => {
    const refresh = () => setRenderVersion((version) => version + 1);
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
    const timeout = window.setTimeout(() => setRenderVersion((version) => version + 1), Math.min(Math.max(route.expiresAtMs - Date.now(), 0), MAX_TIMEOUT_MS));
    return () => window.clearTimeout(timeout);
  }, [route.current, route.current ? route.expiresAtMs : undefined]);

  return route;
}
