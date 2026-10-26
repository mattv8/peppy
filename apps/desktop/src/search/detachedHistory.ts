import type { MessageView } from "../bridge";

export const DETACHED_HISTORY_LIMIT = 150;

export type DetachedHistory = {
  conversationId: string;
  messageIds: string[];
  hasOlder: boolean;
  hasNewer: boolean;
  pendingTarget?: string;
  pendingLoad?: "around" | "older" | "newer" | "latest";
  generation: number;
  sessionKey: string;
};

export type DetachedMergeDirection = "older" | "newer" | "refresh";

export function uniqueMessages(messages: MessageView[]): MessageView[] {
  return [...new Map(messages.map(message => [message.id, message])).values()];
}

export function boundedAroundMessages(messages: MessageView[], targetId: string): MessageView[] {
  const unique = uniqueMessages(messages);
  if (unique.length <= DETACHED_HISTORY_LIMIT) return unique;
  const targetIndex = unique.findIndex(message => message.id === targetId);
  if (targetIndex < 0) return unique.slice(-DETACHED_HISTORY_LIMIT);
  const halfWindow = Math.floor(DETACHED_HISTORY_LIMIT / 2);
  const start = Math.max(0, Math.min(unique.length - DETACHED_HISTORY_LIMIT, targetIndex - halfWindow));
  return unique.slice(start, start + DETACHED_HISTORY_LIMIT);
}

export function mergeDetachedMessages(
  existing: MessageView[],
  incoming: MessageView[],
  direction: DetachedMergeDirection,
): { messages: MessageView[]; droppedOlder: boolean; droppedNewer: boolean } {
  if (direction === "refresh") {
    const incomingById = new Map(incoming.map(message => [message.id, message]));
    return {
      messages: existing.flatMap(message => {
        const refreshed = incomingById.get(message.id);
        return refreshed ? [refreshed] : [];
      }),
      droppedOlder: false,
      droppedNewer: false,
    };
  }

  const merged = uniqueMessages(direction === "older" ? [...incoming, ...existing] : [...existing, ...incoming]);
  if (merged.length <= DETACHED_HISTORY_LIMIT) {
    return { messages: merged, droppedOlder: false, droppedNewer: false };
  }
  return direction === "older"
    ? { messages: merged.slice(0, DETACHED_HISTORY_LIMIT), droppedOlder: false, droppedNewer: true }
    : { messages: merged.slice(-DETACHED_HISTORY_LIMIT), droppedOlder: true, droppedNewer: false };
}

export function searchSessionKey(snapshot: {
  connection: { origin?: string };
  mode: string;
  encryption: { state: string; profileFingerprint?: string };
} | null, appLocked: boolean): string {
  return [
    snapshot?.connection.origin ?? "",
    snapshot?.mode ?? "unavailable",
    snapshot?.encryption.state ?? "locked",
    snapshot?.encryption.profileFingerprint ?? "unknown-profile",
    appLocked ? "locked" : "open",
  ].join(":");
}
