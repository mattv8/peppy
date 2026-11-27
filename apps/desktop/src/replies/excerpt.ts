import { graphemes } from "../reactions/emoji";

/** The longest quote kept whole. Longer bodies keep the first 79 graphemes and an ellipsis. */
const QUOTE_MAX_GRAPHEMES = 80;
const QUOTE_KEPT_GRAPHEMES = 79;
const ELLIPSIS = "…";
/** Rust `split_whitespace` and `trim_end` use Unicode White_Space, which differs from JS `\s` (adds U+FEFF, lacks U+0085). */
const WHITE_SPACE_RUN = /\p{White_Space}+/u;
const TRAILING_WHITE_SPACE = /\p{White_Space}+$/u;

/**
 * Mirrors `quote_excerpt` in `crates/client-core`: NFC, whitespace runs collapsed to single spaces, and a
 * quote past 80 graphemes cut to 79 with trailing whitespace trimmed and an ellipsis. Pinned by
 * `packages/contracts/fixtures/reply-quote.json`.
 */
export function quoteExcerpt(body: string): string {
  const collapsed = body.normalize("NFC").split(WHITE_SPACE_RUN).filter(Boolean).join(" ");
  const parts = graphemes(collapsed);
  if (parts.length <= QUOTE_MAX_GRAPHEMES) return collapsed;
  return `${parts.slice(0, QUOTE_KEPT_GRAPHEMES).join("").replace(TRAILING_WHITE_SPACE, "")}${ELLIPSIS}`;
}

/** The carrier text prefix for a quoted SMS/MMS reply; the phone builds the same text on send. */
export function replyCounterPrefix(excerpt: string): string {
  return `“${excerpt}”\n`;
}
