export type SeenReactionScope = { mode: string; origin?: string };

export interface SeenReactionLedger {
  has(conversationId: string, reactionId: string): boolean;
  mark(conversationId: string, reactionIds: readonly string[]): void;
  subscribe(onChange: () => void): () => void;
  dispose(): void;
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function namespace(scope: SeenReactionScope): string | undefined {
  if (!scope.mode.trim() || !scope.origin) return undefined;
  try {
    const url = new URL(scope.origin);
    if (url.protocol !== "http:" && url.protocol !== "https:") return undefined;
    return `peppy.reactions.seen.v1:${JSON.stringify([scope.mode, url.origin])}:`;
  } catch {
    return undefined;
  }
}

function storage(): Storage | undefined {
  try {
    return globalThis.localStorage;
  } catch {
    return undefined;
  }
}

// One permanent key per seen event; quota failures retain only this ledger's memory.
export function createSeenReactionLedger(scope: SeenReactionScope): SeenReactionLedger {
  const prefix = namespace(scope);
  const seen = new Set<string>();
  const subscribers = new Set<() => void>();
  let disposed = false;

  const eventKey = (conversationId: string, reactionId: string): string | undefined => {
    if (!conversationId.trim() || !reactionId.trim()) return undefined;
    const durable = prefix && UUID.test(conversationId) && UUID.test(reactionId);
    return durable
      ? prefix + JSON.stringify([conversationId.toLowerCase(), reactionId.toLowerCase()])
      : JSON.stringify([conversationId, reactionId]);
  };
  const notify = () => {
    subscribers.forEach(onChange => onChange());
  };
  const hasKey = (key: string): boolean => {
    if (seen.has(key)) return true;
    try {
      if (prefix && key.startsWith(prefix) && storage()?.getItem(key) === "1") {
        seen.add(key);
        return true;
      }
    } catch {
      // Storage denial must not prevent this session from recording visibility.
    }
    return false;
  };
  const sync = (event: StorageEvent) => {
    if (!prefix || (event.storageArea && event.storageArea !== storage())) return;
    if (event.key !== null && !event.key.startsWith(prefix)) return;
    if (event.key !== null && event.newValue === "1") seen.add(event.key);
    // No negative cache: clear/removal needs only a fresh relevant-key read.
    notify();
  };

  return {
    has(conversationId, reactionId) {
      const key = eventKey(conversationId, reactionId);
      return key !== undefined && hasKey(key);
    },
    mark(conversationId, reactionIds) {
      let changed = false;
      for (const reactionId of reactionIds) {
        const key = eventKey(conversationId, reactionId);
        if (key === undefined || hasKey(key)) continue;
        seen.add(key);
        changed = true;
        try {
          if (prefix && key.startsWith(prefix)) storage()?.setItem(key, "1");
        } catch {
          // Keep the positive memory entry without retrying an idempotent mark.
        }
      }
      if (changed) notify();
    },
    subscribe(onChange) {
      if (disposed) return () => {};
      if (subscribers.size === 0 && typeof window !== "undefined") window.addEventListener("storage", sync);
      // Each subscription owns its cleanup, even when callbacks are identical.
      const listener = () => onChange();
      subscribers.add(listener);
      return () => {
        subscribers.delete(listener);
        if (subscribers.size === 0 && typeof window !== "undefined") window.removeEventListener("storage", sync);
      };
    },
    dispose() {
      disposed = true;
      subscribers.clear();
      if (typeof window !== "undefined") window.removeEventListener("storage", sync);
    },
  };
}
