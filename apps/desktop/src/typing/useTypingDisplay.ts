import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { DesktopBridge, TypingSignal } from "@peppy/desktop-ui/bridge-contracts";

const MAX_DISPLAY_MS = 15_000;
const STOP_HIDE_DELAY_MS = 1_000;
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
  const groupDeadlines = useRef(new Map<string, number>());
  const refreshGroup = useRef<(clearImmediately?: boolean) => void>(() => undefined);
  membersRef.current = members;

  useLayoutEffect(() => {
    if (!connected || (direct && incomingMessageId)) setState(hidden);
  }, [connected, direct, incomingMessageId]);

  useLayoutEffect(() => {
    if (!connected || !current || !conversationId || !bridge.subscribe_typing) {
      setState(hidden);
      return;
    }
    const deadlines = groupDeadlines.current;
    deadlines.clear();
    let displayTimer: number | undefined;
    let hideTimer: number | undefined;
    let hideDeadline: number | undefined;
    let announcementTimer: number | undefined;
    const clear = () => {
      if (displayTimer !== undefined) window.clearTimeout(displayTimer);
      if (announcementTimer !== undefined) window.clearTimeout(announcementTimer);
      if (hideTimer !== undefined) window.clearTimeout(hideTimer);
      displayTimer = undefined;
      announcementTimer = undefined;
      hideTimer = undefined;
      hideDeadline = undefined;
      deadlines.clear();
      setState(hidden);
    };
    const hideAfterStop = (deadline?: number) => {
      const nextDeadline = Math.min(Date.now() + STOP_HIDE_DELAY_MS, deadline ?? Infinity);
      if (hideDeadline !== undefined && hideDeadline <= nextDeadline) return;
      if (hideTimer !== undefined) window.clearTimeout(hideTimer);
      hideDeadline = nextDeadline;
      hideTimer = window.setTimeout(() => setState(hidden), Math.max(0, nextDeadline - Date.now()));
    };
    const scheduleGroup = (clearImmediately = false, stopDeadline?: number) => {
      const now = Date.now();
      let expired = false;
      for (const [participant, deadline] of deadlines) {
        if (deadline <= now) {
          deadlines.delete(participant);
          expired = true;
        }
      }
      const participants = [...deadlines.keys()];
      if (displayTimer !== undefined) window.clearTimeout(displayTimer);
      if (!participants.length) {
        if (clearImmediately || expired) setState(hidden);
        else hideAfterStop(stopDeadline);
        return;
      }
      if (hideTimer !== undefined) window.clearTimeout(hideTimer);
      hideTimer = undefined;
      hideDeadline = undefined;
      setState(previous => ({ visible: true, announcement: previous.announcement, participants }));
      const nextDeadline = Math.min(...deadlines.values());
      displayTimer = Number.isFinite(nextDeadline) ? window.setTimeout(() => scheduleGroup(true), Math.max(0, nextDeadline - now)) : undefined;
    };
    refreshGroup.current = scheduleGroup;
    const receive = (signal: TypingSignal) => {
      if (signal.conversationId !== conversationId) return;
      if (!direct) {
        if (!signal.participant || !membersRef.current.includes(signal.participant)) return;
        if (signal.state === "stopped") {
          const deadline = deadlines.get(signal.participant);
          deadlines.delete(signal.participant);
          scheduleGroup(false, deadline);
          return;
        }
        else {
          const expiresAtMs = Number(signal.expiresAtMs);
          const deadline = Math.min(Number.isFinite(expiresAtMs) ? expiresAtMs : Date.now() + MAX_DISPLAY_MS, Date.now() + MAX_DISPLAY_MS);
          if (deadline > Date.now()) deadlines.set(signal.participant, deadline);
        }
        scheduleGroup();
        return;
      }
      if (signal.state === "stopped") return hideAfterStop();
      const expiresAtMs = Number(signal.expiresAtMs);
      const deadline = Math.min(Number.isFinite(expiresAtMs) ? expiresAtMs : Date.now() + MAX_DISPLAY_MS, Date.now() + MAX_DISPLAY_MS);
      if (deadline <= Date.now()) return clear();
      if (displayTimer !== undefined) window.clearTimeout(displayTimer);
      if (hideTimer !== undefined) window.clearTimeout(hideTimer);
      hideTimer = undefined;
      hideDeadline = undefined;
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
    return () => { refreshGroup.current = () => undefined; clear(); unsubscribe(); };
  }, [bridge, connected, conversationId, current, direct]);

  useLayoutEffect(() => {
    if (!direct && incomingSender) {
      groupDeadlines.current.delete(incomingSender);
      refreshGroup.current(true);
    }
  }, [direct, incomingMessageId, incomingSender]);
  return state;
}
