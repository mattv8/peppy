import { describe, expect, it } from "vitest";
import { intentOf, pickAnnouncement, requestAnnouncement, withFirstEmojiNote, type RequestSnapshot } from "./announcements";

const request = (overrides: Partial<RequestSnapshot>): RequestSnapshot => ({ requestId: "r1", state: "sending", emoji: "😂", previousEmoji: null, ...overrides });

describe("pickAnnouncement", () => {
  it("names each pick: send, switch, remove and the text route", () => {
    expect(pickAnnouncement({ route: "native", emoji: "😂" })).toBe("Sending your 😂 reaction.");
    expect(pickAnnouncement({ route: "native", emoji: "❤️", selected: "👍" })).toBe("Changing your reaction to ❤️.");
    expect(pickAnnouncement({ route: "native", emoji: "😂", selected: "😂", removing: true })).toBe("Removing your 😂 reaction.");
    expect(pickAnnouncement({ route: "text", emoji: "😂" })).toBe("Sending your 😂 as a text message.");
  });

  it("appends the first-emoji note only when capture dropped extra emoji", () => {
    expect(withFirstEmojiNote("Sending your 👍🏽 reaction.", true)).toBe("Sending your 👍🏽 reaction. Using the first emoji only.");
    expect(withFirstEmojiNote("Sending your 👍🏽 reaction.", false)).toBe("Sending your 👍🏽 reaction.");
  });
});

describe("intentOf", () => {
  it("reads add, switch and remove from the request", () => {
    expect(intentOf({ emoji: "😂", previousEmoji: null })).toBe("add");
    expect(intentOf({ emoji: "❤️", previousEmoji: "👍" })).toBe("switch");
    expect(intentOf({ emoji: null, previousEmoji: "😂" })).toBe("remove");
  });
});

describe("requestAnnouncement, outcomes", () => {
  it("announces a failed add, a failed removal and a failed switch with the exact copy", () => {
    expect(requestAnnouncement(undefined, request({ state: "not_sent", emoji: "😂" }), undefined)).toEqual({ alert: "Couldn't send your 😂 reaction. Press Enter on the reaction to try again." });
    expect(requestAnnouncement(undefined, request({ state: "not_sent", emoji: null, previousEmoji: "😂" }), "😂")).toEqual({ alert: "Couldn't remove your 😂 reaction. Your reaction is still there." });
    expect(requestAnnouncement(undefined, request({ state: "not_sent", emoji: "❤️", previousEmoji: "👍" }), "👍")).toEqual({ alert: "Couldn't change your reaction to ❤️. Your earlier 👍 is still in place." });
  });

  it("announces an unknown add, an unknown removal and an unknown switch with the exact copy", () => {
    expect(requestAnnouncement(undefined, request({ state: "unknown", emoji: "😂" }), undefined)).toEqual({ alert: "Not sure your 😂 reaction was sent. It won't be sent again on its own." });
    expect(requestAnnouncement(undefined, request({ state: "unknown", emoji: null, previousEmoji: "😂" }), "😂")).toEqual({ alert: "Not sure your 😂 reaction was removed. It won't be sent again on its own." });
    expect(requestAnnouncement(undefined, request({ state: "unknown", emoji: "❤️", previousEmoji: "👍" }), "👍")).toEqual({ alert: "Not sure your reaction changed to ❤️. It won't be sent again on its own." });
  });

  it("says a sent reaction resolved when the snapshot now shows it", () => {
    expect(requestAnnouncement(request({ state: "sending" }), undefined, "😂")).toEqual({ polite: "Your 😂 reaction was sent." });
  });

  it("says a removal resolved when the user no longer has the reaction", () => {
    expect(requestAnnouncement(request({ state: "sending", emoji: null, previousEmoji: "😂" }), undefined, undefined)).toEqual({ polite: "Your reaction was removed." });
  });

  it("stays silent for a dismissed request, a sending update, and a request that did not change", () => {
    expect(requestAnnouncement(request({ state: "not_sent" }), undefined, undefined)).toEqual({});
    expect(requestAnnouncement(request({ state: "sending" }), request({ state: "sending" }), undefined)).toEqual({});
    expect(requestAnnouncement(undefined, request({ state: "sending" }), undefined)).toEqual({});
  });
});
