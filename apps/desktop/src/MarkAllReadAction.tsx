import { useEffect, useRef, useState } from "react";
import { CheckCheck } from "@peppy/desktop-ui";

type Props = {
  unreadCount: number;
  enabled: boolean;
  markAllRead: () => Promise<string>;
  refresh: () => Promise<void>;
  onFocusFallback: () => void;
};

type Notice = "success" | "up-to-date" | "mutation" | "mutation-refresh" | "refresh" | "outcome";
const CANONICAL_DECIMAL = /^(0|[1-9][0-9]*)$/;

export function MarkAllReadAction({ unreadCount, enabled, markAllRead, refresh, onFocusFallback }: Props) {
  const [confirming, setConfirming] = useState(false);
  const [pending, setPending] = useState(false);
  const [notice, setNotice] = useState<Notice>();
  const [count, setCount] = useState<string>();
  const actionRef = useRef<HTMLButtonElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const inFlightRef = useRef(false);
  const mountedRef = useRef(true);
  const focusOwnedRef = useRef(false);
  const focusFallbackRef = useRef(onFocusFallback);
  const unreadCountRef = useRef(unreadCount);
  focusFallbackRef.current = onFocusFallback;
  unreadCountRef.current = unreadCount;

  const deferActionFocus = () => requestAnimationFrame(() => {
    const action = actionRef.current;
    if (!mountedRef.current || !action || document.activeElement !== document.body) return;
    action.focus();
  });
  const deferFallbackFocus = () => requestAnimationFrame(() => {
    if (!mountedRef.current || actionRef.current || document.activeElement !== document.body) return;
    focusFallbackRef.current();
  });
  const restoreFocus = (ownsFocus: boolean) => {
    if (!ownsFocus) return;
    if (unreadCountRef.current) deferActionFocus();
    else deferFallbackFocus();
  };
  const closeConfirmation = () => {
    setConfirming(false);
    deferActionFocus();
  };
  useEffect(() => {
    if (!confirming) return;
    confirmRef.current?.focus();
  }, [confirming]);
  useEffect(() => {
    if (enabled || pending) return;
    if (confirming) closeConfirmation();
  }, [enabled, pending, confirming]);
  useEffect(() => {
    if (unreadCount || pending) return;
    const ownsFocus = focusOwnedRef.current;
    focusOwnedRef.current = false;
    setConfirming(false);
    if (ownsFocus) deferFallbackFocus();
  }, [unreadCount, pending]);
  useEffect(() => {
    if (!confirming || pending) return;
    const cancel = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      closeConfirmation();
    };
    const outside = (event: PointerEvent) => {
      if (popoverRef.current?.contains(event.target as Node) || actionRef.current?.contains(event.target as Node)) return;
      setConfirming(false);
    };
    document.addEventListener("keydown", cancel);
    document.addEventListener("pointerdown", outside);
    return () => { document.removeEventListener("keydown", cancel); document.removeEventListener("pointerdown", outside); };
  }, [confirming, pending]);
  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);

  const refreshAfterAcknowledgement = async (nextNotice: Notice) => {
    try {
      await refresh();
      if (mountedRef.current) setNotice(nextNotice);
    } catch {
      if (mountedRef.current) setNotice(nextNotice === "outcome" ? "outcome" : nextNotice === "mutation" ? "mutation-refresh" : "refresh");
    }
  };
  const run = async () => {
    if (inFlightRef.current || !enabled) return;
    inFlightRef.current = true;
    const ownsFocus = focusOwnedRef.current
      || Boolean(actionRef.current?.contains(document.activeElement))
      || Boolean(popoverRef.current?.contains(document.activeElement));
    setPending(true);
    setNotice(undefined);
    try {
      const result = await markAllRead();
      if (!mountedRef.current) return;
      if (!CANONICAL_DECIMAL.test(result)) {
        await refreshAfterAcknowledgement("outcome");
        return;
      }
      setCount(BigInt(result).toLocaleString());
      await refreshAfterAcknowledgement(result === "0" ? "up-to-date" : "success");
    } catch {
      if (!mountedRef.current) return;
      await refreshAfterAcknowledgement("mutation");
    } finally {
      if (mountedRef.current) {
        restoreFocus(ownsFocus);
        setPending(false);
        setConfirming(false);
      }
      inFlightRef.current = false;
    }
  };
  const retryRefresh = () => void refresh().then(() => mountedRef.current && setNotice(undefined)).catch(() => undefined);
  const trackFocus = () => { focusOwnedRef.current = true; };
  const trackBlur = () => requestAnimationFrame(() => {
    if (!mountedRef.current) return;
    focusOwnedRef.current = Boolean(actionRef.current?.contains(document.activeElement) || popoverRef.current?.contains(document.activeElement));
  });
  const button = unreadCount || pending ? <button ref={actionRef} id="mark-all-read" type="button" aria-label="Mark all as read" title="Mark all as read" aria-haspopup="dialog" aria-expanded={confirming} {...(confirming ? { "aria-controls": "mark-all-read-confirmation" } : {})} disabled={!enabled || pending} aria-busy={pending || undefined} onFocus={trackFocus} onBlur={trackBlur} onClick={() => setConfirming(current => !current)}><CheckCheck size={16} strokeWidth={1.75} aria-hidden /></button> : null;

  return <div id="mark-all-read-boundary" role="toolbar" aria-label="Thread list toolbar" data-empty={!button && !confirming && !notice || undefined}>
    {button}
    {confirming && <div ref={popoverRef} id="mark-all-read-confirmation" role="dialog" aria-label="Confirm mark all as read" onFocus={trackFocus} onBlur={trackBlur}><p>{unreadCount.toLocaleString()} unread messages. All synced conversations in Peppy.</p><div><button ref={confirmRef} className="primary-button" type="button" disabled={pending} onClick={() => void run()}>Mark all as read</button><button className="secondary-button" type="button" disabled={pending} onClick={closeConfirmation}>Cancel</button></div></div>}
    <div id="thread-list-action-notice" role={notice === "mutation" || notice === "mutation-refresh" || notice === "refresh" || notice === "outcome" ? "alert" : "status"} aria-live="polite">
      {pending && "Marking all messages as read…"}
      {notice === "success" && <>Marked {count} messages as read.</>}
      {notice === "up-to-date" && "All messages are already up to date."}
      {notice === "mutation" && <>Some messages may have been marked as read. <button className="secondary-button" type="button" onClick={() => void run()}>Retry mark all as read</button></>}
      {notice === "mutation-refresh" && <>Some messages may have been marked as read, but the current state could not be refreshed. <button className="secondary-button" type="button" onClick={retryRefresh}>Retry refresh</button></>}
      {(notice === "refresh" || notice === "outcome") && <>{notice === "refresh" ? "Messages were marked as read, but the current state could not be refreshed." : "The acknowledgement was invalid; the current state needs refresh."} <button className="secondary-button" type="button" onClick={retryRefresh}>Retry refresh</button></>}
    </div>
  </div>;
}
