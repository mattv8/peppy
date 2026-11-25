import { useSyncExternalStore } from "react";
import type { MessageView, OwnReactionRequestView, ReactionSubmitView } from "@peppy/desktop-ui/bridge-contracts";
import type { SendSettlement } from "../drafts/sendSettlement";
import type { ReactionSource } from "./merge";

export type PendingReaction = {
  conversationId: string;
  messageId: string;
  route: ReactionSource;
  request: OwnReactionRequestView;
  installedAt?: number;
};

type SubmitInput = {
  conversationId: string;
  messageId: string;
  route: ReactionSource;
  emoji: string | null;
  previousEmoji?: string;
};

export class PendingReactionStore {
  private current: ReadonlyMap<string, PendingReaction> = new Map();
  private listeners = new Set<() => void>();
  private snapshots = new Map<string, { messages: readonly MessageView[]; seq: number }>();
  private flights = new Set<Promise<SendSettlement>>();
  private counter = 0;

  entries = (): ReadonlyMap<string, PendingReaction> => this.current;
  isInFlight = (messageId: string): boolean => {
    const entry = this.current.get(messageId);
    return entry !== undefined && entry.installedAt === undefined;
  };
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };

  private replace(next: Map<string, PendingReaction>): void {
    this.current = next;
    for (const listener of this.listeners) listener();
  }

  private without(messageId: string): void {
    const next = new Map(this.current);
    next.delete(messageId);
    this.replace(next);
  }

  observe(conversationId: string, messages: readonly MessageView[]): PendingReaction[] {
    const previous = this.snapshots.get(conversationId);
    if (previous?.messages === messages) return [];
    const seq = (previous?.seq ?? 0) + 1;
    this.snapshots.set(conversationId, { messages, seq });
    const cleared = [...this.current.values()].filter((entry) =>
      entry.conversationId === conversationId && entry.installedAt !== undefined && entry.installedAt < seq);
    for (const entry of cleared) this.without(entry.messageId);
    return cleared;
  }

  retainConversations(ids: ReadonlySet<string>): void {
    const next = new Map(this.current);
    for (const [id, entry] of next) if (!ids.has(entry.conversationId)) next.delete(id);
    for (const id of this.snapshots.keys()) if (!ids.has(id)) this.snapshots.delete(id);
    if (next.size !== this.current.size) this.replace(next);
  }

  submit(input: SubmitInput, call: () => Promise<ReactionSubmitView | undefined>): Promise<ReactionSubmitView | undefined> | undefined {
    if (this.isInFlight(input.messageId)) return undefined;
    const startedAt = this.snapshots.get(input.conversationId)?.seq ?? 0;
    const entry: PendingReaction = {
      conversationId: input.conversationId, messageId: input.messageId, route: input.route,
      request: { requestId: `local:${input.messageId}:${++this.counter}`, emoji: input.emoji, previousEmoji: input.previousEmoji, state: "sending" },
    };
    this.replace(new Map(this.current).set(input.messageId, entry));
    let called: Promise<ReactionSubmitView | undefined>;
    try {
      called = Promise.resolve(call());
    } catch (error) {
      called = Promise.reject(error);
    }
    const result = called.then((response) => {
      if (this.current.get(input.messageId) === entry) {
        const seq = this.snapshots.get(input.conversationId)?.seq ?? 0;
        if (!response?.accepted || seq !== startedAt) this.without(input.messageId);
        else this.replace(new Map(this.current).set(input.messageId, { ...entry, request: { ...entry.request, requestId: input.route === "native" ? response.requestId ?? entry.request.requestId : entry.request.requestId }, installedAt: seq }));
      }
      return response;
    }, (error: unknown) => {
      if (this.current.get(input.messageId) === entry) {
        this.without(input.messageId);
      }
      throw error;
    });
    const settlement = result.then((response) => response?.accepted ? "sent" as const : "unsent" as const, () => "unsent" as const);
    this.flights.add(settlement);
    void settlement.then(() => this.flights.delete(settlement));
    return result;
  }

  inFlight(): Promise<SendSettlement>[] { return [...this.flights]; }

  reset(): void {
    // Calls from the previous session remain in flight until they settle.
    if (this.current.size) this.replace(new Map());
  }
}

export function usePendingReactions(store: PendingReactionStore): ReadonlyMap<string, PendingReaction> {
  return useSyncExternalStore(store.subscribe, store.entries, store.entries);
}
