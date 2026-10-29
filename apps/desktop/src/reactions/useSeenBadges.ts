import { useCallback, useLayoutEffect, useRef } from "react";

export type VisibleReactionBadge = { key: string; reactionIds: readonly string[] };

export type SeenBadgeObservation = {
  root: HTMLElement | null;
  obscuredBottom: number | null;
  enabled: boolean;
  canRecord(): boolean;
  badges: readonly VisibleReactionBadge[];
  onSeen(reactionIds: readonly string[]): void;
};

const MIN_VISIBLE_RATIO = 0.5;

export function useSeenBadges(input: SeenBadgeObservation): (key: string, node: HTMLElement | null) => void {
  const nodes = useRef(new Map<string, HTMLElement>());
  const delivered = useRef(new Set<string>());
  const refresh = useRef<() => void>(() => {});
  const register = useCallback((key: string, node: HTMLElement | null) => {
    if (nodes.current.get(key) === node || (!node && !nodes.current.has(key))) return;
    if (node) nodes.current.set(key, node);
    else nodes.current.delete(key);
    refresh.current();
  }, []);

  // Each committed render is an observation generation, including a caller's
  // transaction-gate release. Never reuse pre-update chip geometry.
  useLayoutEffect(() => {
    let dispose = () => {};
    const reobserve = () => {
      dispose();
      const { root, obscuredBottom, enabled, badges } = input;
      if (!root || obscuredBottom === null || !Number.isFinite(obscuredBottom)
        || obscuredBottom < 0 || !enabled || typeof IntersectionObserver === "undefined") return;

      const represented = new Map<Element, readonly string[]>();
      for (const badge of badges) {
        const node = nodes.current.get(badge.key);
        if (node) represented.set(node, [...badge.reactionIds]);
      }
      let current = true;
      const observer = new IntersectionObserver((entries) => {
        if (!current || !document.hasFocus() || document.visibilityState !== "visible" || !input.canRecord()) return;
        const seen = new Set<string>();
        for (const entry of entries) {
          if (!entry.isIntersecting || entry.intersectionRatio < MIN_VISIBLE_RATIO
            || entry.boundingClientRect.width <= 0 || entry.boundingClientRect.height <= 0) continue;
          for (const id of represented.get(entry.target) ?? []) {
            if (!delivered.current.has(id)) seen.add(id);
          }
        }
        if (!seen.size) return;
        seen.forEach((id) => delivered.current.add(id));
        input.onSeen([...seen]);
      }, { root, rootMargin: `0px 0px -${obscuredBottom}px 0px`, threshold: [0, MIN_VISIBLE_RATIO] });
      represented.forEach((_, node) => observer.observe(node));
      dispose = () => {
        current = false;
        represented.forEach((_, node) => observer.unobserve(node));
        observer.disconnect();
      };
    };
    refresh.current = reobserve;
    reobserve();
    window.addEventListener("focus", reobserve);
    window.addEventListener("blur", reobserve);
    document.addEventListener("visibilitychange", reobserve);
    return () => {
      refresh.current = () => {};
      dispose();
      window.removeEventListener("focus", reobserve);
      window.removeEventListener("blur", reobserve);
      document.removeEventListener("visibilitychange", reobserve);
    };
  });

  return register;
}
