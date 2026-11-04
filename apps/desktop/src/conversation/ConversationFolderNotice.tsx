import { useEffect, useRef, useState, type JSX } from "react";
import { createPortal } from "react-dom";
import { X } from "@peppy/desktop-ui";
import type { ConversationFolder } from "@peppy/desktop-ui/bridge-contracts";
import { TOAST_DISMISS_MS } from "./MarkAllReadAction";

export type FolderNotice = { kind: "success" | "error"; conversationId: string; name: string; from: ConversationFolder; to: ConversationFolder };

type Props = { notice?: FolderNotice; pending: boolean; toastHost?: HTMLElement | null; onUndo(): void; onRetry(): void; onDismiss(): void };

function actionCopy(notice: FolderNotice): string {
  if (notice.to === "archived") return notice.kind === "success" ? `Archived ${notice.name}.` : `Couldn't archive ${notice.name}.`;
  if (notice.to === "spam") return notice.kind === "success" ? `Marked ${notice.name} as spam.` : `Couldn't mark ${notice.name} as spam.`;
  const movedFromSpam = notice.from === "spam";
  if (notice.kind === "success") return movedFromSpam ? `Moved ${notice.name} out of Spam.` : `Moved ${notice.name} to Inbox.`;
  return movedFromSpam ? `Couldn't move ${notice.name} out of Spam.` : `Couldn't move ${notice.name} to Inbox.`;
}

export function ConversationFolderNotice({ notice, pending, toastHost, onUndo, onRetry, onDismiss }: Props): JSX.Element {
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
  return createPortal(<div ref={toastRef} id="conversation-folder-notice" className="mark-all-read-toast" data-placement={toastHost ? "list" : "window"} role={isError ? "alert" : "status"} aria-live={isError ? undefined : "polite"} onMouseEnter={() => setHovered(true)} onMouseLeave={() => setHovered(false)} onFocus={() => setFocused(true)} onBlur={trackBlur}>
    {notice && <>{actionCopy(notice)} <button className="secondary-button" type="button" disabled={pending} onClick={notice.kind === "success" ? onUndo : onRetry}>{notice.kind === "success" ? "Undo" : "Retry"}</button><button className="mark-all-read-toast-dismiss" type="button" aria-label="Dismiss notification" disabled={pending} onClick={onDismiss}><X size={14} strokeWidth={1.75} aria-hidden /></button></>}
  </div>, toastHost ?? document.body);
}
