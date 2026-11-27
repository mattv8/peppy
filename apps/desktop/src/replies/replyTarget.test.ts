import { describe, expect, it } from "vitest";
import type { MessageView } from "@peppy/desktop-ui/bridge-contracts";
import { replyLineExcerpt, replySubject, replyTargetFrom } from "./replyTarget";

const attachment = (mediaType: string) => ({ id: `a-${mediaType}`, name: "file", mediaType, byteSize: 1, state: "ready" }) as MessageView["attachments"][number];

function message(overrides: Partial<MessageView>): MessageView {
  return { id: "m1", sender: "other", body: "", timestamp: "1", status: "sent", statusAt: "1", attachments: [], ...overrides } as MessageView;
}

describe("replyTargetFrom media excerpt", () => {
  it("names an all-image target Photo, a mixed target Attachment, and a target with no attachments no excerpt", () => {
    expect(replyTargetFrom(message({ attachments: [attachment("image/jpeg"), attachment("image/gif")] }), "Sam").excerpt).toBe("Photo");
    expect(replyTargetFrom(message({ attachments: [attachment("image/png"), attachment("application/pdf")] }), "Sam").excerpt).toBe("Attachment");
    expect(replyTargetFrom(message({ attachments: [] }), "Sam").excerpt).toBeUndefined();
  });
});

describe("replyTargetFrom", () => {
  it("quotes the body for the chip and the carrier prefix", () => {
    const target = replyTargetFrom(message({ id: "m2", body: "  See   you\nat noon " }), "Sam");
    expect(target).toMatchObject({ messageId: "m2", name: "Sam", quote: "See you at noon", excerpt: "See you at noon" });
  });

  it("shows a media word for a media-only target, but carries no quote for the SMS prefix", () => {
    const target = replyTargetFrom(message({ body: "", attachments: [attachment("image/jpeg")] }), "Sam");
    expect(target.excerpt).toBe("Photo");
    expect(target.quote).toBeUndefined();
  });
});

describe("reply subject and reply line excerpt", () => {
  it("names your own message by subject, and anyone else by name", () => {
    expect(replySubject({ sender: "self", name: "You" })).toBe("your message");
    expect(replySubject({ sender: "other", name: "Sam" })).toBe("Sam");
  });

  it("uses the core's excerpt, or the media word of a loaded media-only original", () => {
    const loaded = new Map([
      ["photo-1", message({ id: "photo-1", attachments: [attachment("image/png")] })],
      ["file-1", message({ id: "file-1", attachments: [attachment("application/pdf")] })],
    ]);
    expect(replyLineExcerpt({ messageId: "photo-1" }, loaded)).toBe("Photo");
    expect(replyLineExcerpt({ messageId: "file-1" }, loaded)).toBe("Attachment");
    expect(replyLineExcerpt({ messageId: "photo-1", excerpt: "Hi" }, loaded)).toBe("Hi");
    expect(replyLineExcerpt({ messageId: "missing" }, loaded)).toBeUndefined();
  });
});
