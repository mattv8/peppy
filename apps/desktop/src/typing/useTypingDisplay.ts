import { useEffect, useRef, useState } from "react";
import type { DesktopBridge, TypingSignal } from "@peppy/desktop-ui/bridge-contracts";

const MAX_DISPLAY_MS = 15_000;
const ANNOUNCEMENT_DURATION_MS = 5_000;
export const TYPING_ANNOUNCE_COOLDOWN_MS = 30_000;

export function useTypingDisplay(bridge: DesktopBridge, conversationId: string | undefined, current: boolean, localUserTyping = false, incomingMessageId?: string, connected = true, direct = true) {
  const [state, setState] = useState({ visible: false, announcement: false });
  const announcements = useRef(new Map<string, number>());
  const localTypingRef = useRef(localUserTyping);
  localTypingRef.current = localUserTyping;
  useEffect(() => {
    if (!connected || (direct && incomingMessageId)) setState({ visible: false, announcement: false });
  }, [connected, direct, incomingMessageId]);
  useEffect(() => {
    if (!connected || !current || !conversationId || !bridge.subscribe_typing) {
      setState({ visible: false, announcement: false });
      return;
    }
    let displayTimer: number | undefined;
    let announcementTimer: number | undefined;
    const clear = () => {
      if (displayTimer !== undefined) window.clearTimeout(displayTimer);
      if (announcementTimer !== undefined) window.clearTimeout(announcementTimer);
      displayTimer = undefined;
      announcementTimer = undefined;
      setState({ visible: false, announcement: false });
    };
    const receive = (signal: TypingSignal) => {
      if (signal.conversationId !== conversationId) return;
      if (signal.state === "stopped") return clear();
      const expiresAtMs = Number(signal.expiresAtMs);
      const deadline = Math.min(Number.isFinite(expiresAtMs) ? expiresAtMs : Date.now() + MAX_DISPLAY_MS, Date.now() + MAX_DISPLAY_MS);
      if (deadline <= Date.now()) return clear();
      if (displayTimer !== undefined) window.clearTimeout(displayTimer);
      const lastAnnouncement = announcements.current.get(conversationId);
      const announce = !localTypingRef.current && (lastAnnouncement === undefined || lastAnnouncement + TYPING_ANNOUNCE_COOLDOWN_MS <= Date.now());
      if (announce) announcements.current.set(conversationId, Date.now());
      setState(previous => ({ visible: true, announcement: previous.announcement || announce }));
      if (announce) {
        if (announcementTimer !== undefined) window.clearTimeout(announcementTimer);
        announcementTimer = window.setTimeout(() => setState(previous => ({ ...previous, announcement: false })), ANNOUNCEMENT_DURATION_MS);
      }
      displayTimer = window.setTimeout(clear, deadline - Date.now());
    };
    const unsubscribe = bridge.subscribe_typing(receive);
    return () => { clear(); unsubscribe(); };
  }, [bridge, connected, conversationId, current]);
  return state;
}
