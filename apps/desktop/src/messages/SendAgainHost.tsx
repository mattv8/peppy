import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { ConversationView, GatewayView, MessageView } from "@peppy/desktop-ui/bridge-contracts";
import { SendAgainPopover } from "./SendAgainPopover";
import { SendAgainStoreContext, type SendAgainOutcome, type SendAgainStore } from "./SendAgain";
import type { SendAgainRouteLabeler } from "./SendAgainRouteList";

type OpenState = { messageId: string; trigger: HTMLElement };
const DONE_FOCUS_WINDOW_MS = 3000;

export type SendAgainHostProps = {
  conversation?: ConversationView;
  gateways: GatewayView[];
  routeLabel: SendAgainRouteLabeler;
  serverOffline: boolean;
  writesPaused?: boolean;
  onUpdateInProgress?(error: unknown): void;
  /** The conversation's "jump to latest" action, used when a sent-again row is not rendered. */
  onGoToLatest(): void;
  children: ReactNode;
};

/**
 * Owns the Send again popover and the store behind it. The store is React state and refs in this component
 * (no module state), so it lives exactly as long as the conversation view that renders it.
 */
export function SendAgainHost({ conversation, gateways, routeLabel, serverOffline, writesPaused = false, onUpdateInProgress, onGoToLatest, children }: SendAgainHostProps) {
  const [open, setOpen] = useState<OpenState>();
  const [pending, setPending] = useState<ReadonlySet<string>>(() => new Set());
  const inFlight = useRef(new Set<string>());
  const [errors, setErrors] = useState<ReadonlyMap<string, string>>(() => new Map());
  const [sentAgain, setSentAgain] = useState<ReadonlyMap<string, string>>(() => new Map());
  const doneFocus = useRef<{ messageId: string; timer: number } | undefined>(undefined);
  const messages = conversation?.messages;
  const found = open ? messages?.find((message) => message.id === open.messageId) : undefined;
  // The last message seen for the open popover, so a pending call keeps its popover when the list changes under it.
  const [remembered, setRemembered] = useState<MessageView>();
  useEffect(() => { if (found) setRemembered(found); }, [found]);
  const shown = found ?? (remembered?.id === open?.messageId ? remembered : undefined);
  useEffect(() => {
    if (open && !found && !inFlight.current.has(open.messageId)) setOpen(undefined);
  }, [open, found]);
  // A sent-again result is kept until the snapshot carries the original's own sentAgain.
  useEffect(() => {
    setSentAgain((current) => {
      const confirmed = [...current.keys()].filter((messageId) => {
        const message = messages?.find((item) => item.id === messageId);
        return message?.sentAgain ?? message?.sendRecovery?.sentAgain;
      });
      if (confirmed.length === 0) return current;
      const next = new Map(current);
      confirmed.forEach((messageId) => next.delete(messageId));
      return next;
    });
  }, [messages]);
  useEffect(() => () => {
    if (doneFocus.current) window.clearTimeout(doneFocus.current.timer);
  }, []);

  const store = useMemo<SendAgainStore>(() => ({
    writesPaused,
    onUpdateInProgress,
    open: (message, trigger) => {
      // A call in flight holds the host: no other popover opens, and the pending popover is not toggled shut.
      if (inFlight.current.size > 0) return;
      setOpen((current) => current?.messageId === message.id ? undefined : { messageId: message.id, trigger });
    },
    close: (messageId) => setOpen((current) => current?.messageId === messageId ? undefined : current),
    showSentAgain: (messageId) => {
      const row = Array.from(document.querySelectorAll<HTMLElement>("[data-message-id]")).find((element) => element.dataset.messageId === messageId)
        ?? Array.from(document.querySelectorAll<HTMLElement>("[data-resend-attempts]")).find((element) => element.dataset.resendAttempts?.split(" ").includes(messageId));
      if (!row) {
        onGoToLatest();
        return;
      }
      row.scrollIntoView?.({ block: "center" });
      row.tabIndex = -1;
      row.focus();
    },
    sentAgainFor: (messageId) => sentAgain.get(messageId),
    isPending: (messageId) => pending.has(messageId),
    errorFor: (messageId) => errors.get(messageId),
    startCall: (messageId) => {
      if (inFlight.current.has(messageId)) return false;
      inFlight.current.add(messageId);
      setPending(new Set(inFlight.current));
      setErrors((current) => withoutKey(current, messageId));
      return true;
    },
    settleCall: (messageId, outcome: SendAgainOutcome) => {
      inFlight.current.delete(messageId);
      setPending(new Set(inFlight.current));
      if (outcome.kind === "sent") setSentAgain((current) => new Map(current).set(messageId, outcome.messageId));
      if (outcome.kind === "failed") setErrors((current) => new Map(current).set(messageId, outcome.code));
    },
    requestDoneFocus: (messageId) => {
      if (doneFocus.current) window.clearTimeout(doneFocus.current.timer);
      const timer = window.setTimeout(() => { if (doneFocus.current?.messageId === messageId) doneFocus.current = undefined; }, DONE_FOCUS_WINDOW_MS);
      doneFocus.current = { messageId, timer };
    },
    consumeDoneFocus: (messageId) => {
      if (doneFocus.current?.messageId !== messageId) return false;
      window.clearTimeout(doneFocus.current.timer);
      doneFocus.current = undefined;
      return true;
    },
  }), [pending, errors, sentAgain, onGoToLatest, writesPaused, onUpdateInProgress]);

  return <SendAgainStoreContext.Provider value={store}>
    {children}
    {open && shown && <SendAgainPopover key={open.messageId} message={shown} gateways={gateways} routeLabel={routeLabel} serverOffline={serverOffline} anchor={open.trigger} store={store} onClose={() => store.close(open.messageId)} />}
  </SendAgainStoreContext.Provider>;
}

function withoutKey<K, V>(map: ReadonlyMap<K, V>, key: K): ReadonlyMap<K, V> {
  if (!map.has(key)) return map;
  const next = new Map(map);
  next.delete(key);
  return next;
}
