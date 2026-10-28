export type ReactionAction = "add" | "remove";
export type ParsedReaction = { action: ReactionAction; emoji: string; quotedText: string; truncated: boolean };

const GOOGLE = /^(Reacted|Removed) (\S+) (to|from) ([\s\S]+)$/u;
const TAPBACK_ADD = /^(Loved|Liked|Disliked|Laughed at|Emphasized|Questioned) ([\s\S]+)$/u;
const TAPBACK_REMOVE = /^Removed (a heart|a like|a dislike|a laugh|an exclamation|a question mark) from ([\s\S]+)$/u;
const QUOTE_PAIRS: ReadonlyArray<readonly [string, string]> = [["“", "”"], ['"', '"']];
const TAPBACK_EMOJI: Record<string, string> = {
  Loved: "❤️", Liked: "👍", Disliked: "👎", "Laughed at": "😂", Emphasized: "‼️", Questioned: "❓",
  "a heart": "❤️", "a like": "👍", "a dislike": "👎", "a laugh": "😂", "an exclamation": "‼️", "a question mark": "❓",
};
const PICTOGRAPHIC = /\p{Extended_Pictographic}|\p{Regional_Indicator}/u;
const TEXT_DEFAULT_PICTOGRAPH = /^\p{Extended_Pictographic}$/u;
const EMOJI_PRESENTATION = /\p{Emoji_Presentation}/u;
const VARIATION_SELECTOR_16 = "\uFE0F";
const ELLIPSIS = /(?:…|\.\.\.)$/u;

export function normalizeReactionText(text: string): string {
  return text.normalize("NFC").replace(/\s+/gu, " ").trim();
}

export function canonicalEmoji(emoji: string): string {
  return TEXT_DEFAULT_PICTOGRAPH.test(emoji) && !EMOJI_PRESENTATION.test(emoji) ? `${emoji}${VARIATION_SELECTOR_16}` : emoji;
}

export function quoteMatches(normalizedText: string, reaction: Pick<ParsedReaction, "quotedText" | "truncated">): boolean {
  return reaction.truncated ? normalizedText.startsWith(reaction.quotedText) : normalizedText === reaction.quotedText;
}

function isSingleEmoji(token: string): boolean {
  const Segmenter = Intl.Segmenter as typeof Intl.Segmenter | undefined;
  if (!Segmenter) return false;
  const graphemes = new Segmenter(undefined, { granularity: "grapheme" });
  return [...graphemes.segment(token)].length === 1 && PICTOGRAPHIC.test(token);
}

function unquote(tail: string): string | null {
  const pair = QUOTE_PAIRS.find(([open, close]) => tail.length > open.length + close.length && tail.startsWith(open) && tail.endsWith(close));
  return pair ? tail.slice(pair[0].length, tail.length - pair[1].length) : null;
}

function parsedReaction(action: ReactionAction, emoji: string, tail: string): ParsedReaction | null {
  const inner = unquote(tail);
  if (inner === null) return null;
  const quote = normalizeReactionText(inner);
  const truncated = ELLIPSIS.test(quote);
  const quotedText = truncated ? quote.replace(ELLIPSIS, "").trimEnd() : quote;
  return quotedText ? { action, emoji: canonicalEmoji(emoji), quotedText, truncated } : null;
}

export function parseReaction(body: string): ParsedReaction | null {
  const text = body.trim();
  const removal = TAPBACK_REMOVE.exec(text);
  if (removal) return parsedReaction("remove", TAPBACK_EMOJI[removal[1]], removal[2]);

  const google = GOOGLE.exec(text);
  if (google) {
    const [, verb, emoji, preposition, tail] = google;
    const isPaired = (verb === "Reacted" && preposition === "to") || (verb === "Removed" && preposition === "from");
    return isPaired && isSingleEmoji(emoji) ? parsedReaction(verb === "Reacted" ? "add" : "remove", emoji, tail) : null;
  }

  const tapback = TAPBACK_ADD.exec(text);
  return tapback ? parsedReaction("add", TAPBACK_EMOJI[tapback[1]], tapback[2]) : null;
}
