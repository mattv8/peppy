import { useEffect, useState } from "react";
import { currentRcsRoute, rcsBlockExpiry, rcsSendBlock, type RcsRoute, type RcsRouteInput, type RcsSendBlock } from "./rcsRoute";

const MAX_TIMEOUT_MS = 2_147_483_647;

export function useRcsRoute(input: Omit<RcsRouteInput, "nowMs">): RcsRoute & { block: RcsSendBlock } {
  const [renderVersion, setRenderVersion] = useState(0);
  const route = currentRcsRoute({ ...input, nowMs: Date.now() });
  const blockExpiry = rcsBlockExpiry({ ...input, nowMs: Date.now() });

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
  }, []);

  useEffect(() => {
    const expiresAtMs = route.current ? route.expiresAtMs : blockExpiry;
    if (expiresAtMs === undefined || expiresAtMs <= Date.now()) return;
    const timeout = window.setTimeout(() => setRenderVersion((version) => version + 1), Math.min(Math.max(expiresAtMs - Date.now(), 0), MAX_TIMEOUT_MS));
    return () => window.clearTimeout(timeout);
  }, [route.current, route.current ? route.expiresAtMs : blockExpiry, renderVersion]);

  return { ...route, block: rcsSendBlock({ ...input, nowMs: Date.now() }) };
}
