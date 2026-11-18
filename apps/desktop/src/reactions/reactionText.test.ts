import { describe, expect, it } from "vitest";
import fixtures from "../../../../packages/contracts/fixtures/reaction-text.json";
import { canonicalEmoji, normalizeReactionText, parseReaction, quoteMatches } from "./parse";

/** The Canadian flag case core adds to the shared fixture; kept here too, so the parser is checked before core lands it. */
const flagCase = { body: "Good to go, eh", emoji: "🇨🇦", text: "Reacted 🇨🇦 to “Good to go, eh”" };
const entries = [...fixtures, ...(fixtures.some((entry) => entry.emoji === flagCase.emoji) ? [] : [flagCase])];

describe("shared reaction text fixture", () => {
  it.each(entries)("parses the composed text for $emoji back to its original message", ({ body, emoji, text }) => {
    const parsed = parseReaction(text);
    expect(parsed).not.toBeNull();
    expect(parsed).toMatchObject({ action: "add", emoji: canonicalEmoji(emoji) });
    expect(quoteMatches(normalizeReactionText(body), parsed!)).toBe(true);
  });

  it("keeps the entries the spec requires", () => {
    const emojis = entries.map((entry) => entry.emoji);
    expect(emojis).toEqual(expect.arrayContaining(["👍", "❤️", "😂", "👍🏽", "👨‍👩‍👧", "😮", "🇨🇦"]));
  });
});
