import type { OwnReactionRequestView, ReactionSubmitView } from "@peppy/desktop-ui/bridge-contracts";

/**
 * Announcer copy (ux-design C and E). Pure, so each string is tested once and every caller uses the same text.
 * Polite text goes to #reaction-announcer; failures and unknown outcomes go to #reaction-alert.
 */

export type RequestIntent = "add" | "switch" | "remove";
export type RequestSnapshot = Pick<OwnReactionRequestView, "requestId" | "state" | "emoji" | "previousEmoji">;

export function intentOf(request: Pick<OwnReactionRequestView, "emoji" | "previousEmoji">): RequestIntent {
  if (!request.emoji) return "remove";
  return request.previousEmoji && request.previousEmoji !== request.emoji ? "switch" : "add";
}

export type PickInput = { route: "native" | "text"; emoji: string; selected?: string; removing?: boolean };

/** The polite line announced when the user picks a reaction or removes one. */
export function pickAnnouncement({ route, emoji, selected, removing = false }: PickInput): string {
  if (route === "text") return `Sending your ${emoji} as a text message.`;
  if (removing) return `Removing your ${emoji} reaction.`;
  return selected ? `Changing your reaction to ${emoji}.` : `Sending your ${emoji} reaction.`;
}

export const ALREADY_REACTION_COPY = "That's already your reaction.";

/** Capture committed a pasted or typed run of emoji: the first one is used. */
export function withFirstEmojiNote(line: string, multiple: boolean): string {
  return multiple ? `${line} Using the first emoji only.` : line;
}

export const REFUSAL_COPY: Record<NonNullable<ReactionSubmitView["reason"]>, string> = {
  offline: "Phone is offline. Check it and try again.",
  unsupported: "Reactions aren't available in this chat.",
  needs_phone_update: "Update Peppy on your phone to react in this chat.",
  busy: "Sending your reaction…",
  not_reactable: "This message can't be reacted to.",
  invalid_emoji: "Pick an emoji, not text.",
};

/** The call itself failed (the bridge rejected). Nothing is retried automatically. */
export const UNREACHABLE_COPY = "Couldn't reach Peppy. Try again.";

export type RequestAnnouncement = { polite?: string; alert?: string };

/**
 * What a change in one own request says. `settled` is the user's native emoji in the newest snapshot
 * (undefined when the user has no native reaction on the message).
 */
export function requestAnnouncement(previous: RequestSnapshot | undefined, next: RequestSnapshot | undefined, settled: string | undefined): RequestAnnouncement {
  if (!next) return resolution(previous, settled);
  if (previous?.requestId === next.requestId && previous.state === next.state) return {};
  const intent = intentOf(next);
  const emoji = next.emoji ?? next.previousEmoji ?? previous?.emoji ?? "";
  const earlier = next.previousEmoji ?? previous?.previousEmoji ?? "";
  if (next.state === "not_sent") return { alert: refusedCopy(intent, emoji, earlier) };
  if (next.state === "unknown") return { alert: unknownCopy(intent, emoji, earlier) };
  return {};
}

function resolution(previous: RequestSnapshot | undefined, settled: string | undefined): RequestAnnouncement {
  if (!previous || (previous.state !== "sending" && previous.state !== "unknown")) return {};
  if (!previous.emoji) return settled ? {} : { polite: "Your reaction was removed." };
  return settled === previous.emoji ? { polite: `Your ${previous.emoji} reaction was sent.` } : {};
}

function refusedCopy(intent: RequestIntent, emoji: string, earlier: string): string {
  if (intent === "remove") return `Couldn't remove your ${earlier} reaction. Your reaction is still there.`;
  if (intent === "switch") return `Couldn't change your reaction to ${emoji}. Your earlier ${earlier} is still in place.`;
  return `Couldn't send your ${emoji} reaction. Press Enter on the reaction to try again.`;
}

function unknownCopy(intent: RequestIntent, emoji: string, earlier: string): string {
  if (intent === "remove") return `Not sure your ${earlier} reaction was removed. It won't be sent again on its own.`;
  if (intent === "switch") return `Not sure your reaction changed to ${emoji}. It won't be sent again on its own.`;
  return `Not sure your ${emoji} reaction was sent. It won't be sent again on its own.`;
}
