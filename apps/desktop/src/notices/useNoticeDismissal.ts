import { useEffect, useRef, useState } from "react";

export const TOAST_DISMISS_MS = 4000;

export function useNoticeDismissal(notice: { kind: "success" | "info" | "error"; holdMs?: number } | undefined, onDismiss: () => void) {
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
    if (!notice || notice.kind === "error" || hovered || focused) return;
    const timeout = window.setTimeout(() => onDismissRef.current(), notice.holdMs ?? TOAST_DISMISS_MS);
    return () => window.clearTimeout(timeout);
  }, [focused, hovered, notice]);

  const trackBlur = () => requestAnimationFrame(() => setFocused(Boolean(toastRef.current?.contains(document.activeElement))));
  return { toastRef, setHovered, setFocused, trackBlur };
}
