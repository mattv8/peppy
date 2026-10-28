import { afterEach, describe, expect, it, vi } from "vitest";
import { canonicalEmoji, normalizeReactionText, parseReaction, quoteMatches } from "./parse";

describe("parseReaction", () => {
  afterEach(() => vi.unstubAllGlobals());

  it.each([
    ["Reacted 😆 to “Did they discuss the NICU?”", { action: "add", emoji: "😆", quotedText: "Did they discuss the NICU?", truncated: false }],
    ["Removed 😆 from “hello”", { action: "remove", emoji: "😆", quotedText: "hello", truncated: false }],
    ['Reacted 👍🏽 to "ok"', { action: "add", emoji: "👍🏽", quotedText: "ok", truncated: false }],
    ["Reacted 👨‍👩‍👧 to “family”", { action: "add", emoji: "👨‍👩‍👧", quotedText: "family", truncated: false }],
    ["Loved “dinner?”", { action: "add", emoji: "❤️", quotedText: "dinner?", truncated: false }],
    ["Liked “yes”", { action: "add", emoji: "👍", quotedText: "yes", truncated: false }],
    ["Disliked “no”", { action: "add", emoji: "👎", quotedText: "no", truncated: false }],
    ["Laughed at “lol”", { action: "add", emoji: "😂", quotedText: "lol", truncated: false }],
    ["Emphasized “now”", { action: "add", emoji: "‼️", quotedText: "now", truncated: false }],
    ["Questioned “why”", { action: "add", emoji: "❓", quotedText: "why", truncated: false }],
    ["Removed a heart from “dinner?”", { action: "remove", emoji: "❤️", quotedText: "dinner?", truncated: false }],
    ["Removed a like from “yes”", { action: "remove", emoji: "👍", quotedText: "yes", truncated: false }],
    ["Removed a dislike from “no”", { action: "remove", emoji: "👎", quotedText: "no", truncated: false }],
    ["Removed a laugh from “lol”", { action: "remove", emoji: "😂", quotedText: "lol", truncated: false }],
    ["Removed an exclamation from “now”", { action: "remove", emoji: "‼️", quotedText: "now", truncated: false }],
    ["Reacted ❤ to “hi”", { action: "add", emoji: "❤️", quotedText: "hi", truncated: false }],
    ["Reacted 😆 to “Did they discuss the NICU at all…”", { action: "add", emoji: "😆", quotedText: "Did they discuss the NICU at all", truncated: true }],
    ["  Reacted 😆 to “two\n  lines”  ", { action: "add", emoji: "😆", quotedText: "two lines", truncated: false }],
  ])("parses %s", (body, expected) => expect(parseReaction(body)).toEqual(expected));

  it.each(["Reacted to “x”", "Reacted haha to “x”", "Reacted 😆😆 to “x”", "I Loved “x”", "Loved “x” so much", "Reacted 😆 to “”", "Loved “   ”", "Reacted 😆 to “mixed\"", 'Loved "mixed”'])("rejects %s", (body) => expect(parseReaction(body)).toBeNull());

  it("normalizes text, emoji presentation, and exact or truncated quotes", () => {
    expect(normalizeReactionText("  e\u0301  a \n b ")).toBe("é a b");
    expect(canonicalEmoji("❤")).toBe("❤️");
    expect(quoteMatches("hello there", { quotedText: "hello", truncated: true })).toBe(true);
    expect(quoteMatches("hello there", { quotedText: "hello", truncated: false })).toBe(false);
  });

  it("leaves Google reaction text unparsed when grapheme segmentation is unavailable", () => {
    vi.stubGlobal("Intl", { ...Intl, Segmenter: undefined });
    expect(parseReaction("Reacted 😆 to “hello”")).toBeNull();
    expect(parseReaction("Loved “hello”")).toMatchObject({ emoji: "❤️" });
  });
});
