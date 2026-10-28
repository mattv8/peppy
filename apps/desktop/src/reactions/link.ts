import type { MessageView } from "@peppy/desktop-ui/bridge-contracts";
import { normalizeReactionText, parseReaction, quoteMatches, type ParsedReaction } from "./parse";

export type ReactionActor = { key: string; sender: MessageView["sender"]; senderAddress?: string };
export type ReactionBadgeInfo = { emoji: string; actors: ReactionActor[] };
export type ReactionNoticeInfo = ParsedReaction & { reactionId: string; targetId?: string };
export type LinkedReactions = { badges: ReadonlyMap<string, ReactionBadgeInfo[]>; notices: ReadonlyMap<string, ReactionNoticeInfo> };
export const NO_REACTIONS: LinkedReactions = { badges: new Map(), notices: new Map() };

type EmojiActors = Map<string, Map<string, ReactionActor>>;

export function isReactionCandidate(message: MessageView): boolean {
  return message.attachments.length === 0 && !message.subject;
}

function actorOf(message: MessageView): ReactionActor {
  const key = message.sender === "self" ? "self" : message.senderAddress ? `other:${message.senderAddress}` : `other-msg:${message.id}`;
  return { key, sender: message.sender, senderAddress: message.senderAddress };
}

function findTarget(messages: readonly MessageView[], normalizedBodies: readonly string[], before: number, reaction: ParsedReaction, notices: ReadonlyMap<string, ReactionNoticeInfo>): string | undefined {
  for (let index = before - 1; index >= 0; index -= 1) {
    const candidate = messages[index];
    if (normalizedBodies[index] && !notices.has(candidate.id) && quoteMatches(normalizedBodies[index], reaction)) return candidate.id;
  }
  return undefined;
}

function applyReaction(state: EmojiActors, reaction: ParsedReaction, actor: ReactionActor): void {
  if (reaction.action === "add") for (const actors of state.values()) actors.delete(actor.key);
  const actors = state.get(reaction.emoji) ?? new Map<string, ReactionActor>();
  if (reaction.action === "add") actors.set(actor.key, actor);
  else actors.delete(actor.key);
  state.set(reaction.emoji, actors);
}

function toBadges(targets: ReadonlyMap<string, EmojiActors>): Map<string, ReactionBadgeInfo[]> {
  const badges = new Map<string, ReactionBadgeInfo[]>();
  for (const [targetId, emojiActors] of targets) {
    const targetBadges = [...emojiActors]
      .filter(([, actors]) => actors.size > 0)
      .map(([emoji, actors]) => ({ emoji, actors: [...actors.values()] }));
    if (targetBadges.length > 0) badges.set(targetId, targetBadges);
  }
  return badges;
}

/** Expects messages oldest-first, as the thread renders them. */
export function linkReactions(messages: readonly MessageView[]): LinkedReactions {
  const parsed = messages.map((message) => (isReactionCandidate(message) ? parseReaction(message.body) : null));
  if (!parsed.some(Boolean)) return NO_REACTIONS;

  const normalizedBodies = messages.map((message, index) => (parsed[index] ? "" : normalizeReactionText(message.body)));
  const notices = new Map<string, ReactionNoticeInfo>();
  const targets = new Map<string, EmojiActors>();
  messages.forEach((message, index) => {
    const reaction = parsed[index];
    if (!reaction) return;
    const targetId = findTarget(messages, normalizedBodies, index, reaction, notices);
    notices.set(message.id, { ...reaction, reactionId: message.id, targetId });
    if (!targetId) return;
    const state = targets.get(targetId) ?? new Map<string, Map<string, ReactionActor>>();
    applyReaction(state, reaction, actorOf(message));
    targets.set(targetId, state);
  });
  return { badges: toBadges(targets), notices };
}
