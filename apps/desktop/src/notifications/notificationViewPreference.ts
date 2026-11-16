import { useCallback, useEffect, useState } from "react";

export const NOTIFICATIONS_PREFERENCE_ITEM = "peppy.notifications.v1";

export type NotificationArrangement = "time" | "app";

export function readNotificationArrangement(): NotificationArrangement {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(NOTIFICATIONS_PREFERENCE_ITEM) ?? "{}");
    const arrangement = typeof stored === "object" && stored !== null
      ? (stored as { arrangement?: unknown }).arrangement
      : undefined;
    return arrangement === "app" ? "app" : "time";
  } catch {
    return "time";
  }
}

export function persistNotificationArrangement(next: NotificationArrangement): void {
  try {
    localStorage.setItem(NOTIFICATIONS_PREFERENCE_ITEM, JSON.stringify({ arrangement: next }));
  } catch {
    // Storage is optional in embedded previews.
  }
}

export function useNotificationArrangement(): [NotificationArrangement, (next: NotificationArrangement) => void] {
  const [arrangement, setArrangement] = useState(readNotificationArrangement);
  useEffect(() => {
    const sync = (event: StorageEvent) => {
      if (event.key === null || event.key === NOTIFICATIONS_PREFERENCE_ITEM) setArrangement(readNotificationArrangement());
    };
    window.addEventListener("storage", sync);
    return () => window.removeEventListener("storage", sync);
  }, []);
  const update = useCallback((next: NotificationArrangement) => {
    persistNotificationArrangement(next);
    setArrangement(next);
  }, []);
  return [arrangement, update];
}
