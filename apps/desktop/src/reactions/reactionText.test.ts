import { describe, expect, it } from "vitest";
import fixtures from "../../../../packages/contracts/fixtures/reaction-text.json";
import { canonicalEmoji, normalizeReactionText, parseReaction, quoteMatches } from "./parse";

describe("shared reaction text fixture", () => {
  it.each(fixtures)("parses the composed text for $emoji back to its original message", ({ body, emoji, text }) => {
    const parsed = parseReaction(text);
    expect(parsed).not.toBeNull();
    expect(parsed).toMatchObject({ action: "add", emoji: canonicalEmoji(emoji) });
    expect(quoteMatches(normalizeReactionText(body), parsed!)).toBe(true);
  });
});
