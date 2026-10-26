import { useEffect, useRef } from "react";
import type { RefObject } from "react";
import { useMediaActions } from "./MediaActionsContext";

export function useLazyDemand(ref: RefObject<Element | null>, ids: readonly (string | undefined)[]): void {
  const actions = useMediaActions();
  const idsKey = ids.join("\u0000");
  const requestedRef = useRef(new Set<string>());
  const requestDownloadRef = useRef(actions?.requestDownload);
  requestDownloadRef.current = actions?.requestDownload;
  const hasActions = actions !== undefined;

  useEffect(() => {
    const element = ref.current;
    const hasPendingId = ids.some((id) => id !== undefined && !requestedRef.current.has(id));
    if (!hasActions || !element || !hasPendingId) return;
    const request = () => {
      for (const id of ids) {
        if (id && !requestedRef.current.has(id)) {
          requestedRef.current.add(id);
          requestDownloadRef.current?.(id);
        }
      }
    };
    if (typeof IntersectionObserver === "undefined") {
      request();
      return;
    }
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        request();
        observer.disconnect();
      }
    }, { rootMargin: "300px" });
    observer.observe(element);
    return () => observer.disconnect();
  }, [hasActions, idsKey, ref]);
}
