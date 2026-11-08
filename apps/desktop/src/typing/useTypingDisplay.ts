import { useEffect, useRef, useState } from "react";
import type { DesktopBridge, TypingSignal } from "@peppy/desktop-ui/bridge-contracts";

const MAX_DISPLAY_MS = 15_000;
const ANNOUNCEMENT_DURATION_MS = 5_000;
export const TYPING_ANNOUNCE_COOLDOWN_MS = 30_000;

type TypingDisplayOptions = {
  bridge: DesktopBridge;
  conversationId?: string;
  current: boolean;
  localUserTyping?: boolean;
  incomingMessageId?: string;
  connected?: boolean;
  direct?: boolean;
};

export function useTypingDisplay({ bridge, conversationId, current, localUserTyping = false, incomingMessageId, connected = true, direct = true }: TypingDisplayOptions) {
  const [state, setState] = useState({ visible: false, announcement: false });
  const announcements = useRef(new Map<string, number>());
  const localTypingRef = useRef(localUserTyping);
  localTypingRef.current = localUserTyping;
  useEffect(() => {
    if (!connected || (direct && incomingMessageId)) setState((previous) => previous.visible || previous.announcement ? { visible: false, announcement: false } : previous);
  }, [connected, direct, incomingMessageId]);
  useEffect(() => {
    if (!connected || !current || !conversationId || !bridge.subscribe_typing) {
      setState((previous) => previous.visible || previous.announcement ? { visible: false, announcement: false } : previous);
      return;
    }
    let displayTimer: number | undefined;
    let announcementTimer: number | undefined;
    const clear = () => {
      if (displayTimer !== undefined) window.clearTimeout(displayTimer);
      if (announcementTimer !== undefined) window.clearTimeout(announcementTimer);
      displayTimer = undefined;
      announcementTimer = undefined;
      setState((previous) => previous.visible || previous.announcement ? { visible: false, announcement: false } : previous);
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
