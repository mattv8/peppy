import { describe, expect, it } from "vitest";
import type { OwnReactionRequestView } from "@peppy/desktop-ui/bridge-contracts";
import type { ReactionBadgeInfo } from "./link";
import { mergeReactions } from "./merge";

const alex = { key: "other:+15550001", sender: "other" as const, senderAddress: "+15550001" };
const sam = { key: "other:+15550002", sender: "other" as const, senderAddress: "+15550002" };
const self = { key: "self", sender: "self" as const, senderAddress: undefined };
const request = (overrides: Partial<OwnReactionRequestView> & Pick<OwnReactionRequestView, "state">): OwnReactionRequestView => ({ requestId: "req-1", emoji: "❤️", ...overrides });
const nativeSelf = (emoji: string) => ({ emoji, actors: [{ self: true }] });

describe("mergeReactions", () => {
  it("returns no chips without any reaction facts", () => {
    expect(mergeReactions({})).toEqual([]);
  });

  it("shows native reactions from other people as native chips", () => {
    expect(mergeReactions({ native: [{ emoji: "❤️", actors: [{ self: false, address: "+15550001" }] }] })).toEqual([
      { emoji: "❤️", actors: [alex], reactionIds: [], source: "native" },
    ]);
  });

  it("lets a native reaction win over a text reaction from the same actor", () => {
    const parsed: ReactionBadgeInfo[] = [{ emoji: "👍", actors: [alex], reactionIds: ["m2"] }];
    const merged = mergeReactions({ parsed, native: [{ emoji: "❤️", actors: [{ self: false, address: "+15550001" }] }] });
    expect(merged).toEqual([{ emoji: "❤️", actors: [alex], reactionIds: [], source: "native" }]);
  });

  it("keeps text reactions from actors the native set does not mention", () => {
    const parsed: ReactionBadgeInfo[] = [{ emoji: "😂", actors: [sam], reactionIds: ["m3"] }];
    const merged = mergeReactions({ parsed, native: [{ emoji: "❤️", actors: [{ self: false, address: "+15550001" }] }] });
    expect(merged).toEqual([
      { emoji: "❤️", actors: [alex], reactionIds: [], source: "native" },
      { emoji: "😂", actors: [sam], reactionIds: ["m3"], source: "text" },
    ]);
  });

  it("groups actors who share an emoji and marks the group native when any actor is native", () => {
    const parsed: ReactionBadgeInfo[] = [{ emoji: "👍", actors: [sam], reactionIds: ["m4"] }];
    const merged = mergeReactions({ parsed, native: [{ emoji: "👍", actors: [{ self: false, address: "+15550001" }] }] });
    expect(merged).toEqual([{ emoji: "👍", actors: [alex, sam], reactionIds: ["m4"], source: "native" }]);
  });

  it("keeps unresolved native actors apart", () => {
    const merged = mergeReactions({ native: [{ emoji: "😮", actors: [{ self: false }, { self: false }] }] });
    expect(merged[0].actors.map((actor) => actor.key)).toEqual(["other-unknown:😮:0", "other-unknown:😮:1"]);
  });

  it("shows the user's confirmed native reaction as a sent own chip", () => {
    expect(mergeReactions({ native: [nativeSelf("😂")] })).toEqual([
      { emoji: "😂", actors: [self], reactionIds: [], source: "native", own: { state: "sent", source: "native" } },
    ]);
  });

  it("shows a text-sourced own reaction as a read-only sent chip", () => {
    const parsed: ReactionBadgeInfo[] = [{ emoji: "👍", actors: [self], reactionIds: ["m5"] }];
    expect(mergeReactions({ parsed })).toEqual([
      { emoji: "👍", actors: [self], reactionIds: ["m5"], source: "text", own: { state: "sent", source: "text" } },
    ]);
  });

  it("replaces the confirmed own emoji with a sending request", () => {
    const merged = mergeReactions({ native: [nativeSelf("👍")], ownRequest: request({ state: "sending", previousEmoji: "👍" }) });
    expect(merged).toEqual([{ emoji: "❤️", actors: [self], reactionIds: [], source: "native", own: { state: "sending", source: "native", requestId: "req-1", intent: "switch", previousEmoji: "👍" } }]);
  });

  it("lets a sending request replace a text-sourced own reaction too", () => {
    const parsed: ReactionBadgeInfo[] = [{ emoji: "👍", actors: [self], reactionIds: ["m5"] }];
    const merged = mergeReactions({ parsed, ownRequest: request({ state: "sending", previousEmoji: "👍" }) });
    expect(merged.map((badge) => [badge.emoji, badge.own?.state])).toEqual([["❤️", "sending"]]);
  });

  it("keeps the old own chip dimmed while a removal is sending", () => {
    const merged = mergeReactions({ native: [nativeSelf("😂")], ownRequest: request({ state: "sending", emoji: null, previousEmoji: "😂" }) });
    expect(merged).toEqual([{ emoji: "😂", actors: [self], reactionIds: [], source: "native", own: { state: "removing", source: "native", requestId: "req-1", intent: "remove", previousEmoji: "😂" } }]);
  });

  it("shows a failed switch beside the confirmed chip it did not replace", () => {
    const merged = mergeReactions({ native: [nativeSelf("👍")], ownRequest: request({ state: "not_sent", previousEmoji: "👍" }) });
    expect(merged).toEqual([
      { emoji: "👍", actors: [self], reactionIds: [], source: "native", own: { state: "sent", source: "native" } },
      { emoji: "❤️", actors: [self], reactionIds: [], source: "native", own: { state: "not_sent", source: "native", requestId: "req-1", intent: "switch", previousEmoji: "👍" } },
    ]);
  });

  it("shows a failed add alone when nothing was confirmed", () => {
    const merged = mergeReactions({ ownRequest: request({ state: "not_sent", emoji: "😮" }) });
    expect(merged).toEqual([{ emoji: "😮", actors: [self], reactionIds: [], source: "native", own: { state: "not_sent", source: "native", requestId: "req-1", intent: "add", previousEmoji: null } }]);
  });

  it("shows a failed removal as a second chip beside the original", () => {
    const merged = mergeReactions({ native: [nativeSelf("😂")], ownRequest: request({ state: "not_sent", emoji: null, previousEmoji: "😂" }) });
    expect(merged.map((badge) => [badge.emoji, badge.own?.state, badge.own?.intent])).toEqual([
      ["😂", "sent", undefined],
      ["😂", "not_sent", "remove"],
    ]);
  });

  it("shows an unknown removal with no known emoji as nothing", () => {
    expect(mergeReactions({ ownRequest: request({ state: "not_sent", emoji: null }) })).toEqual([]);
    expect(mergeReactions({ ownRequest: request({ state: "sending", emoji: null }) })).toEqual([]);
  });

  it("keeps an unknown attempt until the request is resolved", () => {
    const merged = mergeReactions({ native: [nativeSelf("👍")], ownRequest: request({ state: "not_sent", emoji: "😂", previousEmoji: "👍" }) });
    expect(merged.at(-1)?.own).toMatchObject({ state: "not_sent", intent: "switch" });
  });

  it("orders other people's groups first and own chips last", () => {
    const parsed: ReactionBadgeInfo[] = [{ emoji: "😂", actors: [sam], reactionIds: ["m6"] }];
    const merged = mergeReactions({ parsed, native: [nativeSelf("👍"), { emoji: "❤️", actors: [{ self: false, address: "+15550001" }] }] });
    expect(merged.map((badge) => badge.emoji)).toEqual(["❤️", "😂", "👍"]);
  });
});
