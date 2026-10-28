import { describe, expect, it } from "vitest";
import type { MessageView } from "@peppy/desktop-ui/bridge-contracts";
import { linkReactions, NO_REACTIONS } from "./link";

const message = (id: string, body: string, overrides: Partial<MessageView> = {}): MessageView => ({ id, revision: "1", sender: "other", senderAddress: "+15550001", body, timestamp: String(1000 + Number(id.replace(/\D/g, "") || 0)), attachments: [], ...overrides });

describe("linkReactions", () => {
  it("links to the most recent earlier matching non-reaction", () => {
    const linked = linkReactions([message("m1", "hi"), message("m2", "hi", { sender: "self", senderAddress: undefined }), message("m3", "Reacted 😆 to “hi”")]);
    expect(linked.notices.get("m3")).toMatchObject({ reactionId: "m3", targetId: "m2", emoji: "😆" });
    expect(linked.badges.get("m2")).toEqual([{ emoji: "😆", actors: [{ key: "other:+15550001", sender: "other", senderAddress: "+15550001" }] }]);
  });

  it("prefix-matches truncated quotes without targeting reactions or later messages", () => {
    const linked = linkReactions([message("m1", "Did they discuss the NICU at all in the group"), message("m2", "Reacted 😆 to “Did they discuss the NICU at all…”"), message("m3", "Loved “x”"), message("m4", "x")]);
    expect(linked.notices.get("m2")?.targetId).toBe("m1");
    expect(linked.notices.get("m3")?.targetId).toBeUndefined();
  });

  it("replaces an actor's emoji and removes only that actor", () => {
    const linked = linkReactions([message("m1", "plan?"), message("m2", "Loved “plan?”"), message("m3", "Reacted 😆 to “plan?”"), message("m4", "Reacted 😆 to “plan?”", { senderAddress: "+15550002" }), message("m5", "Removed 😆 from “plan?”")]);
    expect(linked.badges.get("m1")).toEqual([{ emoji: "😆", actors: [{ key: "other:+15550002", sender: "other", senderAddress: "+15550002" }] }]);
  });

  it("keeps unknown senders separate and ignores attachments or subjects", () => {
    const linked = linkReactions([message("m1", "x"), message("m2", "Loved “x”", { senderAddress: undefined }), message("m3", "Loved “x”", { senderAddress: undefined })]);
    expect(linked.badges.get("m1")?.[0].actors).toHaveLength(2);
    expect(linkReactions([message("m4", "Loved “x”", { attachments: [{ id: "a", name: "a", mediaType: "image/png", byteSize: 1, state: "ready" }] })])).toBe(NO_REACTIONS);
    expect(linkReactions([message("m5", "Loved “x”", { subject: "subject" })])).toBe(NO_REACTIONS);
  });
});
