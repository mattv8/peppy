import { describe, expect, it, vi } from "vitest";
import { locateReactionTarget, reactionSearchQuery, REACTION_SEARCH_LIMIT, REACTION_SEARCH_MAX_PAGES } from "./locate";

type Hit = { kind: "message"; conversationId: string; messageId: string; title: string; snippet: string; timestamp: string };
const hit = (messageId: string, snippet: string, timestamp: string, conversationId = "c1"): Hit => ({ kind: "message", conversationId, messageId, title: "title", snippet, timestamp });
const page = (items: Hit[], nextCursor?: string, indexState: "ready" | "indexing" = "ready") => ({ query: "query", items, nextCursor, indexState, hasMore: { conversation: false, message: Boolean(nextCursor), contact: false } });
const reaction = { id: "r", revision: "1", sender: "other" as const, body: "Reacted 😆 to “hello there…”", timestamp: "5000", attachments: [] };
const notice = { action: "add" as const, emoji: "😆", quotedText: "hello there", truncated: true, reactionId: "r" };

describe("locateReactionTarget", () => {
  it("returns the newest eligible same-conversation full snippet match", async () => {
    const search = vi.fn().mockResolvedValue(page([hit("old", "hello there friend", "1000"), hit("new", "hello there pal", "4000"), hit("later", "hello there later", "9000"), hit("other", "hello there", "4500", "c2")]));
    await expect(locateReactionTarget(search, { conversationId: "c1", reaction, notice })).resolves.toEqual({ state: "found", hit: expect.objectContaining({ messageId: "new" }) });
    expect(search).toHaveBeenCalledWith({ query: "hello there", kind: "message", limit: REACTION_SEARCH_LIMIT });
  });

  it("marks clipped snippets as candidates and prefers an earlier verified hit in the same page", async () => {
    const clipped = "hello there ".repeat(14).slice(0, 160);
    const mixedSearch = vi.fn().mockResolvedValue(page([hit("verified", "hello there friend", "1000"), hit("clipped", clipped, "4000")]));
    await expect(locateReactionTarget(mixedSearch, { conversationId: "c1", reaction, notice })).resolves.toMatchObject({ state: "found", hit: { messageId: "verified" } });
    const candidateSearch = vi.fn().mockResolvedValue(page([hit("clipped", clipped, "2")]));
    await expect(locateReactionTarget(candidateSearch, { conversationId: "c1", reaction, notice })).resolves.toMatchObject({ state: "candidate", hit: { messageId: "clipped" } });
  });

  it("marks clipped exact quotes as candidates, pages to a bounded limit, and reports indexing", async () => {
    const search = vi.fn().mockResolvedValueOnce(page([hit("other", "hello there", "1", "c2")], "cursor")).mockResolvedValueOnce(page([hit("match", "hello there", "2")]));
    const clipped = "a ".repeat(80).slice(0, 160);
    const exactNotice = { ...notice, quotedText: clipped, truncated: false };
    const candidateSearch = vi.fn().mockResolvedValue(page([hit("clipped", clipped, "2")]));
    await expect(locateReactionTarget(candidateSearch, { conversationId: "c1", reaction, notice: exactNotice })).resolves.toMatchObject({ state: "candidate", hit: { messageId: "clipped" } });
    await expect(locateReactionTarget(search, { conversationId: "c1", reaction, notice })).resolves.toMatchObject({ state: "found", hit: { messageId: "match" } });
    const endless = vi.fn().mockResolvedValue(page([hit("other", "hello there", "1", "c2")], "more"));
    await expect(locateReactionTarget(endless, { conversationId: "c1", reaction, notice })).resolves.toEqual({ state: "not-found", exhausted: false });
    expect(endless).toHaveBeenCalledTimes(REACTION_SEARCH_MAX_PAGES);
    await expect(locateReactionTarget(vi.fn().mockResolvedValue(page([], undefined, "indexing")), { conversationId: "c1", reaction, notice })).resolves.toEqual({ state: "indexing" });
    await expect(locateReactionTarget(vi.fn().mockResolvedValue(page([])), { conversationId: "c1", reaction, notice })).resolves.toEqual({ state: "not-found", exhausted: true });
  });

  it("does not search text with no searchable terms and preserves word boundaries", async () => {
    const search = vi.fn();
    await expect(locateReactionTarget(search, { conversationId: "c1", reaction, notice: { ...notice, quotedText: "😆 !!" } })).resolves.toEqual({ state: "not-found" });
    expect(search).not.toHaveBeenCalled();
    expect(reactionSearchQuery("word ".repeat(60))).toMatch(/word$/);
    expect(reactionSearchQuery("word ".repeat(60)).length).toBeLessThanOrEqual(120);
  });

  it("accepts ISO timestamps and skips the time filter when a timestamp is unparseable", async () => {
    const search = vi.fn().mockResolvedValue(page([hit("target", "hello there", "2026-10-28T11:00:00Z")]));
    await expect(locateReactionTarget(search, { conversationId: "c1", reaction: { ...reaction, timestamp: "2026-10-28T12:00:00Z" }, notice })).resolves.toMatchObject({ state: "found", hit: { messageId: "target" } });
    await expect(locateReactionTarget(search, { conversationId: "c1", reaction: { ...reaction, timestamp: "" }, notice })).resolves.toMatchObject({ state: "found", hit: { messageId: "target" } });
  });
});
