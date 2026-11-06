import { useEffect, useState } from "react";
import type { DesktopBridge, TypingSignal } from "@peppy/desktop-ui/bridge-contracts";

const MAX_DISPLAY_MS = 15_000;

export function useTypingDisplay(bridge: DesktopBridge, conversationId: string | undefined, current: boolean) {
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    if (!current || !conversationId || !bridge.subscribe_typing) return;
    let timeout: number | undefined;
    const clear = () => { if (timeout) window.clearTimeout(timeout); timeout = undefined; setVisible(false); };
    const receive = (signal: TypingSignal) => {
      if (signal.conversationId !== conversationId || signal.state === "stopped") return clear();
      const expires = Number(signal.expiresAtMs);
      const deadline = Math.min(Number.isFinite(expires) ? expires : Date.now() + MAX_DISPLAY_MS, Date.now() + MAX_DISPLAY_MS);
      clear(); setVisible(true);
      timeout = window.setTimeout(clear, Math.max(0, deadline - Date.now()));
    };
    const unsubscribe = bridge.subscribe_typing(receive);
    return () => { clear(); unsubscribe(); };
  }, [bridge, conversationId, current]);
  return visible;
}
