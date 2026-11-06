import { useEffect, useState } from "react";
import type { DesktopBridge, TypingSignal } from "@peppy/desktop-ui/bridge-contracts";

const MAX_DISPLAY_MS = 15_000;

export function useTypingDisplay(bridge: DesktopBridge, conversationId: string | undefined, current: boolean) {
  const [state, setState] = useState({ visible: false, announcement: "" });
  useEffect(() => {
    if (!current || !conversationId || !bridge.subscribe_typing) return;
    let timeout: number | undefined;
    const clear = () => { if (timeout) window.clearTimeout(timeout); timeout = undefined; setState({ visible: false, announcement: "" }); };
    const receive = (signal: TypingSignal) => {
      if (signal.conversationId !== conversationId || signal.state === "stopped") return clear();
      const expires = Number(signal.expiresAtMs);
      const deadline = Math.min(Number.isFinite(expires) ? expires : Date.now() + MAX_DISPLAY_MS, Date.now() + MAX_DISPLAY_MS);
      setState((current) => ({ visible: true, announcement: current.visible ? "" : "start" }));
      timeout = window.setTimeout(clear, Math.max(0, deadline - Date.now()));
    };
    const unsubscribe = bridge.subscribe_typing(receive);
    return () => { clear(); unsubscribe(); };
  }, [bridge, conversationId, current]);
  return state;
}
