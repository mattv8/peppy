import type { LocalSearchHit, LocalSearchPage, LocalSearchRequest, MessageView } from "@peppy/desktop-ui/bridge-contracts";
import { parseTimestampMs } from "../media/grouping";
import type { ReactionNoticeInfo } from "./link";
import { normalizeReactionText, quoteMatches } from "./parse";

export type MessageSearchHit = Extract<LocalSearchHit, { kind: "message" }>;
/** A candidate was matched against a clipped search snippet and must have its loaded body checked with `quoteMatches` before navigation. */
export type ReactionTargetResult = { state: "found"; hit: MessageSearchHit } | { state: "candidate"; hit: MessageSearchHit } | { state: "not-found"; exhausted?: boolean } | { state: "indexing" };
export const REACTION_SEARCH_LIMIT = 50;
export const REACTION_SEARCH_MAX_PAGES = 4;
const MAX_QUERY_CHARS = 120;
const SNIPPET_CHARS = 160;
const SEARCHABLE = /[\p{L}\p{N}]/u;

type LocateInput = { conversationId: string; reaction: MessageView; notice: ReactionNoticeInfo };
type SnippetMatch = { hit: MessageSearchHit; requiresVerification: boolean };

export function reactionSearchQuery(quotedText: string): string {
  let query = "";
  for (const word of normalizeReactionText(quotedText).split(" ")) {
    const next = query ? `${query} ${word}` : word;
    if (next.length > MAX_QUERY_CHARS) break;
    query = next;
  }
  return query;
}

function snippetMatch(snippet: string, notice: ReactionNoticeInfo): boolean {
  const normalizedSnippet = normalizeReactionText(snippet);
  return quoteMatches(normalizedSnippet, notice) || (Array.from(snippet).length >= SNIPPET_CHARS && notice.quotedText.startsWith(normalizedSnippet));
}

function newestMatch(items: LocalSearchHit[], { conversationId, reaction, notice }: LocateInput): SnippetMatch | undefined {
  const reactedAt = parseTimestampMs(reaction.timestamp);
  const matches = items
    .filter((item): item is MessageSearchHit => item.kind === "message")
    .filter((item) => item.conversationId === conversationId && item.messageId !== reaction.id)
    .filter((item) => {
      const timestamp = parseTimestampMs(item.timestamp);
      return reactedAt === undefined || timestamp === undefined || timestamp <= reactedAt;
    })
    .filter((item) => snippetMatch(item.snippet, notice))
    .sort((left, right) => (parseTimestampMs(right.timestamp) ?? 0) - (parseTimestampMs(left.timestamp) ?? 0));
  const hit = matches.find((item) => Array.from(item.snippet).length < SNIPPET_CHARS) ?? matches[0];
  return hit ? { hit, requiresVerification: Array.from(hit.snippet).length >= SNIPPET_CHARS } : undefined;
}

export async function locateReactionTarget(search: (request: LocalSearchRequest) => Promise<LocalSearchPage>, input: LocateInput): Promise<ReactionTargetResult> {
  const query = reactionSearchQuery(input.notice.quotedText);
  if (!SEARCHABLE.test(query)) return { state: "not-found" };

  let cursor: string | undefined;
  let indexing = false;
  let cappedWithCursor = false;
  for (let pageIndex = 0; pageIndex < REACTION_SEARCH_MAX_PAGES; pageIndex += 1) {
    const page = await search({ query, kind: "message", limit: REACTION_SEARCH_LIMIT, ...(cursor ? { cursor } : {}) });
    indexing ||= page.indexState === "indexing";
    const match = newestMatch(page.items, input);
    if (match) return match.requiresVerification ? { state: "candidate", hit: match.hit } : { state: "found", hit: match.hit };
    if (!page.nextCursor) break;
    if (pageIndex === REACTION_SEARCH_MAX_PAGES - 1) cappedWithCursor = true;
    cursor = page.nextCursor;
  }
  return indexing ? { state: "indexing" } : { state: "not-found", exhausted: !cappedWithCursor };
}
