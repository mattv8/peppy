export type ReactionAction = "add" | "remove";
export type ParsedReaction = { action: ReactionAction; emoji: string; quotedText: string; truncated: boolean };

/**
 * Whitespace and invisible format characters that carry no content at a token boundary. Google Messages
 * frames its SMS reactions with U+200A and U+200B, and JavaScript's `\s` does not cover U+200B.
 * U+200D, variation selectors and tag characters are left out because emoji sequences are built from them.
 */
const IGNORABLE = "\\s\\u00AD\\u061C\\u180E\\u200B\\u200C\\u200E\\u200F\\u202A-\\u202E\\u2060\\u2066-\\u2069\\uFEFF";
const IGNORABLE_CHARACTER = new RegExp(`[${IGNORABLE}]`, "u");
const GAP = `[${IGNORABLE}]+`;
const EMOJI_TOKEN = `([^${IGNORABLE}]+)`;
const QUOTED_TAIL = "([\\s\\S]+)";
const reactionPattern = (...tokens: string[]): RegExp => new RegExp(`^${tokens.join(GAP)}$`, "u");

const GOOGLE = reactionPattern("(Reacted|Removed)", EMOJI_TOKEN, "(to|from)", QUOTED_TAIL);
const EMOJI_FIRST = reactionPattern(EMOJI_TOKEN, "to", QUOTED_TAIL);
const TAPBACK_ADD = reactionPattern("(Loved|Liked|Disliked|Laughed at|Emphasized|Questioned)", QUOTED_TAIL);
const TAPBACK_REMOVE = reactionPattern("Removed", "(a heart|a like|a dislike|a laugh|an exclamation|a question mark)", "from", QUOTED_TAIL);
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

/** A scan instead of an anchored pattern, which backtracks quadratically over a long run of whitespace. */
function trimIgnorable(text: string): string {
  let start = 0;
  let end = text.length;
  while (start < end && IGNORABLE_CHARACTER.test(text[start])) start += 1;
  while (end > start && IGNORABLE_CHARACTER.test(text[end - 1])) end -= 1;
  return text.slice(start, end);
}

export function normalizeReactionText(text: string): string {
  return trimIgnorable(text.normalize("NFC")).replace(/\s+/gu, " ");
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
  const text = trimIgnorable(body);
  const removal = TAPBACK_REMOVE.exec(text);
  if (removal) return parsedReaction("remove", TAPBACK_EMOJI[removal[1]], removal[2]);

  const google = GOOGLE.exec(text);
  if (google) {
    const [, verb, emoji, preposition, tail] = google;
    const isPaired = (verb === "Reacted" && preposition === "to") || (verb === "Removed" && preposition === "from");
    return isPaired && isSingleEmoji(emoji) ? parsedReaction(verb === "Reacted" ? "add" : "remove", emoji, tail) : null;
  }

  const emojiFirst = EMOJI_FIRST.exec(text);
  if (emojiFirst && isSingleEmoji(emojiFirst[1])) return parsedReaction("add", emojiFirst[1], emojiFirst[2]);

  const tapback = TAPBACK_ADD.exec(text);
  return tapback ? parsedReaction("add", TAPBACK_EMOJI[tapback[1]], tapback[2]) : null;
}
