import { useEffect, useRef } from "react";
import type { DesktopBridge } from "@peppy/desktop-ui/bridge-contracts";
import { readShareTyping } from "../shell/layout-persistence";

const REFRESH_MS = 3_200;
const IDLE_STOP_MS = 6_000;

export function useTypingSender({ bridge, conversationId, current, eligible, sendSupported }: { bridge: DesktopBridge; conversationId?: string; current: boolean; eligible: boolean; sendSupported: boolean }) {
  const active = useRef(false);
  const refresh = useRef<number | undefined>(undefined);
  const idle = useRef<number | undefined>(undefined);
  const stop = () => {
    if (!active.current || !conversationId) return;
    active.current = false;
    if (refresh.current) window.clearInterval(refresh.current);
    if (idle.current) window.clearTimeout(idle.current);
    void bridge.send_typing?.({ conversationId, state: "stopped" }).catch(() => undefined);
  };
  const start = () => {
    if (!bridge.send_typing || !conversationId || !current || !eligible || !sendSupported || !readShareTyping()) return;
    if (!active.current) { active.current = true; void bridge.send_typing({ conversationId, state: "started" }).catch(() => undefined); }
    if (!refresh.current) refresh.current = window.setInterval(() => { if (readShareTyping()) void bridge.send_typing?.({ conversationId, state: "started" }).catch(() => undefined); else stop(); }, REFRESH_MS);
    if (idle.current) window.clearTimeout(idle.current);
    idle.current = window.setTimeout(stop, IDLE_STOP_MS);
  };
  useEffect(() => {
    const storage = (event: StorageEvent) => { if (event.key === "peppy.layout.v1" && !readShareTyping()) stop(); };
    window.addEventListener("storage", storage);
    return () => { window.removeEventListener("storage", storage); stop(); };
  }, [conversationId, current, eligible, sendSupported]);
  return { noteInput: start, noteSend: stop, setFocused: (focused: boolean) => { if (!focused) stop(); } };
}
