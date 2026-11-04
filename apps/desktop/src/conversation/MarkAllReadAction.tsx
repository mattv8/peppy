import { type FocusEvent, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { CheckCheck, X } from "@peppy/desktop-ui";

type Props = {
  unreadCount: number;
  enabled: boolean;
  markAllRead: () => Promise<string>;
  refresh: () => Promise<void>;
  onFocusFallback: () => void;
  toastHost?: HTMLElement | null;
};

type Notice = "success" | "up-to-date" | "mutation" | "mutation-refresh" | "refresh" | "outcome";

const CANONICAL_DECIMAL = /^(0|[1-9][0-9]*)$/;
export const TOAST_DISMISS_MS = 4000;

export function MarkAllReadAction({ unreadCount, enabled, markAllRead, refresh, onFocusFallback, toastHost }: Props) {
  const [confirming, setConfirming] = useState(false);
  const [pending, setPending] = useState(false);
  const [notice, setNotice] = useState<Notice>();
  const [count, setCount] = useState<string>();
  const [rawCount, setRawCount] = useState<string>();
  const [toastHovered, setToastHovered] = useState(false);
  const [toastFocused, setToastFocused] = useState(false);
  const actionRef = useRef<HTMLButtonElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const toastRef = useRef<HTMLDivElement>(null);
  const inFlightRef = useRef(false);
  const mountedRef = useRef(true);
  const focusOwnedRef = useRef(false);
  const restoreFocusAfterPendingRef = useRef(false);
  const focusFallbackRef = useRef(onFocusFallback);
  focusFallbackRef.current = onFocusFallback;

  const ownsFocus = () => Boolean(
    actionRef.current?.contains(document.activeElement)
    || popoverRef.current?.contains(document.activeElement)
    || toastRef.current?.contains(document.activeElement),
  );
  const deferRestoredFocus = () => requestAnimationFrame(() => {
    if (!mountedRef.current) return;
    const action = actionRef.current;
    if (document.activeElement !== document.body && document.activeElement !== action) return;
    if (action && !action.disabled) action.focus();
    else focusFallbackRef.current();
  });
  const restoreFocus = (ownsCurrentFocus: boolean) => {
    if (ownsCurrentFocus) deferRestoredFocus();
  };
  const closeConfirmation = () => {
    setConfirming(false);
    deferRestoredFocus();
  };

  useEffect(() => {
    if (confirming) confirmRef.current?.focus();
  }, [confirming]);
  useEffect(() => {
    if (!enabled && !pending) {
      if (confirming) closeConfirmation();
    }
  }, [enabled, pending, confirming]);
  useEffect(() => {
    if (pending || !restoreFocusAfterPendingRef.current) return;
    restoreFocusAfterPendingRef.current = false;
    deferRestoredFocus();
  }, [pending]);
  useEffect(() => {
    if (unreadCount || pending) return;
    const ownedFocus = focusOwnedRef.current;
    focusOwnedRef.current = false;
    setConfirming(false);
    restoreFocus(ownedFocus);
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
    return () => {
      document.removeEventListener("keydown", cancel);
      document.removeEventListener("pointerdown", outside);
    };
  }, [confirming, pending]);
  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);
  useEffect(() => {
    if ((notice !== "success" && notice !== "up-to-date") || toastHovered || toastFocused) return;
    const timeout = window.setTimeout(() => setNotice(undefined), TOAST_DISMISS_MS);
    return () => window.clearTimeout(timeout);
  }, [notice, toastHovered, toastFocused]);

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
    const ownedFocus = focusOwnedRef.current || ownsFocus();
    focusOwnedRef.current = false;
    setPending(true);
    setNotice(undefined);
    try {
      const result = await markAllRead();
      if (!mountedRef.current) return;
      if (!CANONICAL_DECIMAL.test(result)) {
        await refreshAfterAcknowledgement("outcome");
        return;
      }
      setRawCount(result);
      setCount(BigInt(result).toLocaleString());
      await refreshAfterAcknowledgement(result === "0" ? "up-to-date" : "success");
    } catch {
      if (mountedRef.current) await refreshAfterAcknowledgement("mutation");
    } finally {
      if (mountedRef.current) {
        restoreFocusAfterPendingRef.current = ownedFocus;
        setPending(false);
        setConfirming(false);
      }
      inFlightRef.current = false;
    }
  };
  const retryRefresh = () => {
    const ownedFocus = focusOwnedRef.current || ownsFocus();
    void refresh().then(() => {
      if (!mountedRef.current) return;
      setNotice(undefined);
      restoreFocus(ownedFocus);
    }).catch(() => undefined);
  };
  const dismissNotice = () => {
    const ownedFocus = focusOwnedRef.current || ownsFocus();
    setNotice(undefined);
    restoreFocus(ownedFocus);
  };
  const trackFocus = () => {
    focusOwnedRef.current = true;
    setToastFocused(Boolean(toastRef.current?.contains(document.activeElement)));
  };
  const trackBlur = () => requestAnimationFrame(() => {
    if (!mountedRef.current) return;
    const focusedInToast = Boolean(toastRef.current?.contains(document.activeElement));
    focusOwnedRef.current = ownsFocus();
    setToastFocused(focusedInToast);
  });
  const noticeIsError = notice === "mutation" || notice === "mutation-refresh" || notice === "refresh" || notice === "outcome";
  const toast = <div ref={toastRef} id="thread-list-action-notice" className="mark-all-read-toast" data-placement={toastHost ? "list" : "window"} role={noticeIsError ? "alert" : "status"} aria-live="polite" onMouseEnter={() => setToastHovered(true)} onMouseLeave={() => setToastHovered(false)} onFocus={trackFocus} onBlur={trackBlur}>
    {pending && "Marking all messages as read…"}
    {notice === "success" && <>Marked {count} message{rawCount === "1" ? "" : "s"} as read.</>}
    {notice === "up-to-date" && "All messages are already up to date."}
    {notice === "mutation" && <>Some messages may have been marked as read. <button className="secondary-button" type="button" onClick={() => void run()}>Retry mark all as read</button></>}
    {notice === "mutation-refresh" && <>Some messages may have been marked as read, but the current state could not be refreshed. <button className="secondary-button" type="button" onClick={retryRefresh}>Retry refresh</button></>}
    {(notice === "refresh" || notice === "outcome") && <>{notice === "refresh" ? "Messages were marked as read, but the current state could not be refreshed." : "The acknowledgement was invalid; the current state needs refresh."} <button className="secondary-button" type="button" onClick={retryRefresh}>Retry refresh</button></>}
    {notice && <button className="mark-all-read-toast-dismiss" type="button" aria-label="Dismiss notification" onClick={dismissNotice}><X size={14} strokeWidth={1.75} aria-hidden /></button>}
  </div>;

  const actionTitle = !enabled && !pending
    ? "Mark all as read (unavailable while locked or loading)"
    : unreadCount === 0
      ? "No unread messages"
      : `Mark all as read (${unreadCount.toLocaleString()} unread)`;
  const closeConfirmationOnFocusOut = (event: FocusEvent<HTMLDivElement>) => {
    const nextFocus = event.relatedTarget;
    if (nextFocus instanceof Node && event.currentTarget.contains(nextFocus)) return;
    setConfirming(false);
  };

  return <div id="mark-all-read-boundary" className="titlebar-action-anchor" onBlur={confirming ? closeConfirmationOnFocusOut : undefined}>
    <button ref={actionRef} id="mark-all-read" type="button" aria-label="Mark all as read" {...(unreadCount > 0 ? { "aria-describedby": "mark-all-read-count" } : {})} title={actionTitle} aria-haspopup="dialog" aria-expanded={confirming} {...(confirming ? { "aria-controls": "mark-all-read-confirmation" } : {})} disabled={!enabled || pending || unreadCount === 0} aria-busy={pending || undefined} onFocus={trackFocus} onBlur={trackBlur} onClick={() => setConfirming(current => !current)}><CheckCheck size={16} strokeWidth={1.75} aria-hidden />{unreadCount > 0 && <span className="titlebar-action-dot" aria-hidden />}</button>
    {unreadCount > 0 && <span id="mark-all-read-count" className="visually-hidden">{unreadCount.toLocaleString()} unread message{unreadCount === 1 ? "" : "s"}</span>}
    {confirming && <div ref={popoverRef} id="mark-all-read-confirmation" role="dialog" aria-label="Confirm mark all as read" onFocus={trackFocus} onBlur={trackBlur}><p>Mark {unreadCount.toLocaleString()} unread message{unreadCount === 1 ? "" : "s"} as read in all synced conversations?</p><div><button ref={confirmRef} className="primary-button" type="button" disabled={pending} onClick={() => void run()}>Mark all as read</button><button className="secondary-button" type="button" disabled={pending} onClick={closeConfirmation}>Cancel</button></div></div>}
    {createPortal(toast, toastHost ?? document.body)}
  </div>;
}
