import { useEffect, useRef, useState, type JSX } from "react";
import { createPortal } from "react-dom";
import { X } from "@peppy/desktop-ui";
import type { AppFilter } from "../bridge/bridge";
import { TOAST_DISMISS_MS } from "../conversation/MarkAllReadAction";

export type NotificationMuteNotice = { kind: "success" | "error"; filter: AppFilter; label: string };

type Props = {
  notice?: NotificationMuteNotice;
  pending: boolean;
  toastHost?: HTMLElement | null;
  errorAction?: "mute" | "unmute";
  onUndo(): void;
  onRetry(): void;
  onDismiss(): void;
};

function actionCopy(notice: NotificationMuteNotice, errorAction: Props["errorAction"]): string {
  if (notice.kind === "success") return `Muted ${notice.label}.`;
  return errorAction === "unmute" ? `Couldn't unmute ${notice.label}.` : `Couldn't mute ${notice.label}.`;
}

export function NotificationMuteNotice({ notice, pending, toastHost, errorAction, onUndo, onRetry, onDismiss }: Props): JSX.Element {
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const toastRef = useRef<HTMLDivElement>(null);
  const onDismissRef = useRef(onDismiss);
  onDismissRef.current = onDismiss;

  useEffect(() => {
    setHovered(Boolean(toastRef.current?.matches(":hover")));
    setFocused(Boolean(toastRef.current?.contains(document.activeElement)));
  }, [notice]);

  useEffect(() => {
    if (!notice || notice.kind !== "success" || hovered || focused) return;
    const timeout = window.setTimeout(() => onDismissRef.current(), TOAST_DISMISS_MS);
    return () => window.clearTimeout(timeout);
  }, [focused, hovered, notice]);

  const trackBlur = () => requestAnimationFrame(() => setFocused(Boolean(toastRef.current?.contains(document.activeElement))));
  const isError = notice?.kind === "error";
  return createPortal(<div ref={toastRef} id="notifications-mute-notice" className="mark-all-read-toast" data-placement={toastHost ? "list" : "window"} role={isError ? "alert" : "status"} aria-live={isError ? undefined : "polite"} onMouseEnter={() => setHovered(true)} onMouseLeave={() => setHovered(false)} onFocus={() => setFocused(true)} onBlur={trackBlur}>
    {notice && <>{actionCopy(notice, errorAction)} <button className="secondary-button" type="button" disabled={pending} onClick={notice.kind === "success" ? onUndo : onRetry}>{notice.kind === "success" ? "Undo" : "Retry"}</button><button className="mark-all-read-toast-dismiss" type="button" aria-label="Dismiss notification" disabled={pending} onClick={onDismiss}><X size={14} strokeWidth={1.75} aria-hidden /></button></>}
  </div>, toastHost ?? document.body);
}
