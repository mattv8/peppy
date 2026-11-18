import type { MessageReactionView, OwnReactionRequestView } from "@peppy/desktop-ui/bridge-contracts";
import type { ReactionActor, ReactionBadgeInfo } from "./link";

export type OwnChipState = "sent" | "sending" | "removing" | "not_sent" | "unknown";
export type ReactionSource = "native" | "text";
export type ReactionIntent = "add" | "switch" | "remove";
export type OwnReactionChip = {
  state: OwnChipState;
  source: ReactionSource;
  requestId?: string;
  /** Which wire action the chip describes; drives the copy of the chip and its issue popover. */
  intent?: ReactionIntent;
  previousEmoji?: string | null;
};
export type MergedReactionBadge = ReactionBadgeInfo & { own?: OwnReactionChip; source: ReactionSource };
export type MergeInput = { parsed?: ReactionBadgeInfo[]; native?: MessageReactionView[]; ownRequest?: OwnReactionRequestView };

/** The actor key for this device's user. Every merge uses it; there is no other self identity. */
const SELF_KEY = "self";
const SELF_ACTOR: ReactionActor = { key: SELF_KEY, sender: "self", senderAddress: undefined };

type Entry = { emoji: string; actor: ReactionActor; source: ReactionSource; reactionIds: string[] };

function nativeActor(actor: MessageReactionView["actors"][number], emoji: string, index: number): ReactionActor {
  if (actor.self) return SELF_ACTOR;
  if (actor.address) return { key: `other:${actor.address}`, sender: "other", senderAddress: actor.address };
  return { key: `other-unknown:${emoji}:${index}`, sender: "other", senderAddress: undefined };
}

function nativeEntries(native: readonly MessageReactionView[]): Entry[] {
  return native.flatMap((reaction) => reaction.actors.map((actor, index): Entry => ({
    emoji: reaction.emoji, actor: nativeActor(actor, reaction.emoji, index), source: "native", reactionIds: [],
  })));
}

function textEntries(parsed: readonly ReactionBadgeInfo[]): Entry[] {
  return parsed.flatMap((badge) => badge.actors.map((actor): Entry => ({
    emoji: badge.emoji, actor, source: "text", reactionIds: badge.reactionIds,
  })));
}

/** One entry per actor. A native entry wins over a text-parsed entry for the same actor. */
function resolveActors(native: Entry[], text: Entry[]): Map<string, Entry> {
  const byActor = new Map<string, Entry>();
  for (const entry of native) byActor.set(entry.actor.key, entry);
  for (const entry of text) if (!byActor.has(entry.actor.key)) byActor.set(entry.actor.key, entry);
  return byActor;
}

function groupOthers(entries: readonly Entry[]): MergedReactionBadge[] {
  const groups = new Map<string, { actors: ReactionActor[]; reactionIds: string[]; native: boolean }>();
  for (const entry of entries) {
    const group = groups.get(entry.emoji) ?? { actors: [], reactionIds: [], native: false };
    group.actors.push(entry.actor);
    group.reactionIds.push(...entry.reactionIds);
    group.native ||= entry.source === "native";
    groups.set(entry.emoji, group);
  }
  return [...groups].map(([emoji, group]): MergedReactionBadge => ({
    emoji, actors: group.actors, reactionIds: group.reactionIds, source: group.native ? "native" : "text",
  }));
}

function confirmedChip(self: Entry): MergedReactionBadge {
  return { emoji: self.emoji, actors: [self.actor], reactionIds: self.reactionIds, source: self.source, own: { state: "sent", source: self.source } };
}

function intentOf(emoji: string | null, previous: string | null): ReactionIntent {
  if (!emoji) return "remove";
  return previous && previous !== emoji ? "switch" : "add";
}

function chipState(request: OwnReactionRequestView): OwnChipState {
  if (request.state === "sending") return request.emoji ? "sending" : "removing";
  return request.state;
}

function attemptChip(request: OwnReactionRequestView, previous: string | null, selfActor: ReactionActor): MergedReactionBadge | undefined {
  const emoji = request.emoji ?? previous;
  if (!emoji) return undefined;
  return {
    emoji,
    actors: [selfActor],
    reactionIds: [],
    source: "native",
    own: { state: chipState(request), source: "native", requestId: request.requestId, intent: intentOf(request.emoji, previous), previousEmoji: previous },
  };
}

/** A pending request replaces the confirmed own chip; a failed or unknown one sits beside it. */
function ownChips(self: Entry | undefined, request: OwnReactionRequestView | undefined, selfActor: ReactionActor): MergedReactionBadge[] {
  const confirmed = self ? [confirmedChip(self)] : [];
  if (!request) return confirmed;
  const previous = request.previousEmoji ?? self?.emoji ?? null;
  const attempt = attemptChip(request, previous, selfActor);
  if (request.state === "sending") return attempt ? [attempt] : [];
  return attempt ? [...confirmed, attempt] : confirmed;
}

/**
 * Merges the text-parsed badges, the native reaction set and this device's own reaction request into the
 * chips a message shows. One reaction per person: a native actor's entry replaces the text entry for that
 * actor, and an own request overlays the confirmed set. Other people's reactions group by emoji; own chips
 * stay separate so their state, label and interaction are never merged into a count.
 */
export function mergeReactions(input: MergeInput): MergedReactionBadge[] {
  const resolved = resolveActors(nativeEntries(input.native ?? []), textEntries(input.parsed ?? []));
  const self = resolved.get(SELF_KEY);
  const others = [...resolved.values()].filter((entry) => entry.actor.key !== SELF_KEY);
  const selfActor = SELF_ACTOR;
  return [...groupOthers(others), ...ownChips(self, input.ownRequest, selfActor)];
}
