import { describe, expect, it } from "vitest";
import { groupMessages, type GalleryMessage } from "./grouping";

const message = (id: string, overrides: Partial<GalleryMessage> = {}): GalleryMessage => ({ id, revision: "1", sender: "other", senderAddress: "+15550001", body: "", timestamp: "2026-01-01T12:00:00.000Z", attachments: [{ id: `${id}-image`, name: "image", mediaType: "image/jpeg", byteSize: 1, state: "ready" }], ...overrides });

describe("groupMessages", () => {
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
  });

  it("allows the caption only on the final group member", () => {
    const groups = groupMessages([message("one", { sender: "self", body: "ignored" }), message("two", { sender: "self", body: "caption", timestamp: "2026-01-01T12:01:00.000Z" })]);
    expect(groups[0].messages.at(-1)?.body).toBe("caption");
  });
});
