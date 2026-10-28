import { useCallback, useEffect, useState } from "react";

export const REACTIONS_PREFERENCE_KEY = "peppy.reactions.v1";

export function readShowReactions(): boolean {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(REACTIONS_PREFERENCE_KEY) ?? "{}");
    const show = typeof stored === "object" && stored !== null ? (stored as { show?: unknown }).show : undefined;
    return typeof show === "boolean" ? show : true;
  } catch {
    return true;
  }
}

export function persistShowReactions(show: boolean): void {
  try {
    localStorage.setItem(REACTIONS_PREFERENCE_KEY, JSON.stringify({ show }));
  } catch {
    // Storage is optional in embedded previews.
  }
}

export function useShowReactions(): [boolean, (show: boolean) => void] {
  const [show, setShow] = useState(readShowReactions);
  useEffect(() => {
    const sync = (event: StorageEvent) => {
      if (event.key === null || event.key === REACTIONS_PREFERENCE_KEY) setShow(readShowReactions());
    };
    window.addEventListener("storage", sync);
    return () => window.removeEventListener("storage", sync);
  }, []);
  const update = useCallback((next: boolean) => {
    persistShowReactions(next);
    setShow(next);
  }, []);
  return [show, update];
}
