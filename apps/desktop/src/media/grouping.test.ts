import { describe, expect, it } from "vitest";
import { groupMessages, type GalleryMessage } from "./grouping";

const message = (id: string, overrides: Partial<GalleryMessage> = {}): GalleryMessage => ({ id, revision: "1", sender: "other", senderAddress: "+15550001", body: "", timestamp: "2026-01-01T12:00:00.000Z", attachments: [{ id: `${id}-image`, name: "image", mediaType: "image/jpeg", byteSize: 1, state: "ready" }], ...overrides });

const replyTo = (messageId: string) => ({ messageId, sender: "other" as const, excerpt: "Dinner", available: true });

describe("groupMessages", () => {
  it("splits consecutive photos that reply to different originals", () => {
    const groups = groupMessages([message("one", { replyTo: replyTo("a") }), message("two", { replyTo: replyTo("b") })]);
    expect(groups.map((group) => group.messages.map((item) => item.id))).toEqual([["one"], ["two"]]);
  });

  it("splits a reply from a photo that is not a reply", () => {
    const groups = groupMessages([message("one", { replyTo: replyTo("a") }), message("two")]);
    expect(groups.map((group) => group.messages.map((item) => item.id))).toEqual([["one"], ["two"]]);
    const reversed = groupMessages([message("one"), message("two", { replyTo: replyTo("a") })]);
    expect(reversed.map((group) => group.messages.map((item) => item.id))).toEqual([["one"], ["two"]]);
  });

  it("keeps consecutive photos that reply to the same original in one group, under the usual rules", () => {
    const groups = groupMessages([message("one", { replyTo: replyTo("a") }), message("two", { replyTo: replyTo("a"), timestamp: "2026-01-01T12:01:00.000Z" })]);
    expect(groups.map((group) => group.messages.map((item) => item.id))).toEqual([["one", "two"]]);
  });

  it("keeps plain photos from one sender grouped when neither replies", () => {
    expect(groupMessages([message("one"), message("two", { timestamp: "2026-01-01T12:01:00.000Z" })])).toHaveLength(1);
  });

  it("groups adjacent actual senders and preserves message identities", () => {
    const source = [message("one"), message("two", { timestamp: "2026-01-01T12:02:00.000Z" })];
    const group = groupMessages(source)[0];
    expect(group.messages).toEqual(source);
    expect(group.attachments.map(({ message: owner }) => owner.id)).toEqual(["one", "two"]);
  });

  it("keeps unknown senders, invalid dates, and mixed attachments separate", () => {
    expect(groupMessages([message("one", { senderAddress: undefined }), message("two", { senderAddress: undefined })])).toHaveLength(2);
    expect(groupMessages([message("one"), message("two", { timestamp: "" })])).toHaveLength(2);
    expect(groupMessages([message("one"), message("two", { attachments: [{ id: "video", name: "v", mediaType: "video/mp4", byteSize: 1, state: "ready" }] })])).toHaveLength(2);
    expect(groupMessages([message("one", { timestamp: " " }), message("two", { timestamp: "  " })])).toHaveLength(2);
  });

  it("ends an image run after a caption or subject", () => {
    const captioned = groupMessages([message("one", { sender: "self", body: "first" }), message("two", { sender: "self", body: "second", timestamp: "2026-01-01T12:01:00.000Z" })]);
    const subject = groupMessages([message("three", { sender: "self", subject: "Subject" }), message("four", { sender: "self", timestamp: "2026-01-01T12:01:00.000Z" })]);
    expect(captioned).toHaveLength(2);
    expect(subject).toHaveLength(2);
  });
});
