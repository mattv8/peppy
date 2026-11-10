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
  incomingSender?: string;
  members?: readonly string[];
  connected?: boolean;
  direct?: boolean;
};

type TypingDisplay = { visible: boolean; announcement: boolean; participants: string[] };

const hidden: TypingDisplay = { visible: false, announcement: false, participants: [] };
const noMembers: readonly string[] = [];

export function useTypingDisplay({ bridge, conversationId, current, localUserTyping = false, incomingMessageId, incomingSender, members = noMembers, connected = true, direct = true }: TypingDisplayOptions) {
  const [state, setState] = useState<TypingDisplay>(hidden);
  const announcements = useRef(new Map<string, number>());
  const localTypingRef = useRef(localUserTyping);
  localTypingRef.current = localUserTyping;
  const membersRef = useRef(members);
  membersRef.current = members;

  useEffect(() => {
    if (!connected || (direct && incomingMessageId)) setState(hidden);
  }, [connected, direct, incomingMessageId]);

  useEffect(() => {
    if (!connected || !current || !conversationId || !bridge.subscribe_typing) {
      setState(hidden);
      return;
    }
    const deadlines = new Map<string, number>();
    let displayTimer: number | undefined;
    let announcementTimer: number | undefined;
    const clear = () => {
      if (displayTimer !== undefined) window.clearTimeout(displayTimer);
      if (announcementTimer !== undefined) window.clearTimeout(announcementTimer);
      displayTimer = undefined;
      announcementTimer = undefined;
      deadlines.clear();
      setState(hidden);
    };
    const scheduleGroup = () => {
      const now = Date.now();
      for (const [participant, deadline] of deadlines) if (deadline <= now) deadlines.delete(participant);
      const participants = [...deadlines.keys()];
      setState(previous => ({ visible: participants.length > 0, announcement: previous.announcement && participants.length > 0, participants }));
      if (displayTimer !== undefined) window.clearTimeout(displayTimer);
      const nextDeadline = Math.min(...deadlines.values());
      displayTimer = Number.isFinite(nextDeadline) ? window.setTimeout(scheduleGroup, Math.max(0, nextDeadline - now)) : undefined;
    };
    const receive = (signal: TypingSignal) => {
      if (signal.conversationId !== conversationId) return;
      if (!direct) {
        if (!signal.participant || !membersRef.current.includes(signal.participant)) return;
        if (signal.state === "stopped") deadlines.delete(signal.participant);
        else {
          const expiresAtMs = Number(signal.expiresAtMs);
          const deadline = Math.min(Number.isFinite(expiresAtMs) ? expiresAtMs : Date.now() + MAX_DISPLAY_MS, Date.now() + MAX_DISPLAY_MS);
          if (deadline > Date.now()) deadlines.set(signal.participant, deadline);
        }
        scheduleGroup();
        return;
      }
      if (signal.state === "stopped") return clear();
      const expiresAtMs = Number(signal.expiresAtMs);
      const deadline = Math.min(Number.isFinite(expiresAtMs) ? expiresAtMs : Date.now() + MAX_DISPLAY_MS, Date.now() + MAX_DISPLAY_MS);
      if (deadline <= Date.now()) return clear();
      if (displayTimer !== undefined) window.clearTimeout(displayTimer);
      const lastAnnouncement = announcements.current.get(conversationId);
      const announce = !localTypingRef.current && (lastAnnouncement === undefined || lastAnnouncement + TYPING_ANNOUNCE_COOLDOWN_MS <= Date.now());
      if (announce) announcements.current.set(conversationId, Date.now());
      setState(previous => ({ visible: true, announcement: previous.announcement || announce, participants: [] }));
      if (announce) {
        if (announcementTimer !== undefined) window.clearTimeout(announcementTimer);
        announcementTimer = window.setTimeout(() => setState(previous => ({ ...previous, announcement: false })), ANNOUNCEMENT_DURATION_MS);
      }
      displayTimer = window.setTimeout(clear, deadline - Date.now());
    };
    const unsubscribe = bridge.subscribe_typing(receive);
    return () => { clear(); unsubscribe(); };
  }, [bridge, connected, conversationId, current, direct]);

  useEffect(() => {
    if (!direct && incomingSender) setState(previous => {
      if (!previous.participants.includes(incomingSender)) return previous;
      const participants = previous.participants.filter(participant => participant !== incomingSender);
      return { ...previous, visible: participants.length > 0, participants };
    });
  }, [direct, incomingSender]);
  return state;
}
