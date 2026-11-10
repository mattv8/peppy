import { useEffect, useState } from "react";
import type { McpDeviceStatus } from "@peppy/desktop-ui/bridge-contracts";
import { bridge } from "../bridge/bridge";

const statuses = new Map<string, McpDeviceStatus>();
const listeners = new Map<string, Set<(status: McpDeviceStatus) => void>>();
const revisions = new Map<string, number>();
const UNAVAILABLE_SESSION = "unavailable";

export function publishMcpStatus(sessionKey: string, status: McpDeviceStatus): void {
  revisions.set(sessionKey, (revisions.get(sessionKey) ?? 0) + 1);
  if (sessionKey !== UNAVAILABLE_SESSION) statuses.set(sessionKey, status);
  listeners.get(sessionKey)?.forEach(listener => listener(status));
}

export function useMcpStatus(sessionKey: string): McpDeviceStatus | null {
  const [state, setState] = useState(() => ({ sessionKey, status: statuses.get(sessionKey) ?? null }));

  useEffect(() => {
    let active = true;
    setState({ sessionKey, status: statuses.get(sessionKey) ?? null });
    const subscribers = listeners.get(sessionKey) ?? new Set<(next: McpDeviceStatus) => void>();
    listeners.set(sessionKey, subscribers);
    const receive = (next: McpDeviceStatus) => {
      if (active) setState({ sessionKey, status: next });
    };
    subscribers.add(receive);

    const revision = revisions.get(sessionKey) ?? 0;
    void bridge.mcp_status().then(next => {
      if (!active || (revisions.get(sessionKey) ?? 0) !== revision) return;
      publishMcpStatus(sessionKey, next);
    }).catch(() => undefined);

    return () => {
      active = false;
      subscribers.delete(receive);
      if (!subscribers.size) listeners.delete(sessionKey);
    };
  }, [sessionKey]);

  return state.sessionKey === sessionKey ? state.status : null;
}
