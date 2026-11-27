import { describe, expect, it } from "vitest";
import replyQuotes from "../../../../packages/contracts/fixtures/reply-quote.json";
import { quoteExcerpt, replyCounterPrefix } from "./excerpt";

describe("quoteExcerpt", () => {
  replyQuotes.forEach(({ body, excerpt, reply, text }, index) => {
    it(`matches the shared reply-quote fixture ${index}`, () => {
      expect(quoteExcerpt(body)).toBe(excerpt);
      expect(replyCounterPrefix(excerpt) + reply).toBe(text);
    });
  });

  it("keeps a quote of exactly 80 graphemes whole", () => {
    const body = "a".repeat(80);
    expect(quoteExcerpt(body)).toBe(body);
  });
});
