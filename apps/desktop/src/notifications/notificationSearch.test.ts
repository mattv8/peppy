import { describe, expect, it } from "vitest";
import type { MirroredNotification } from "../bridge/bridge";
import { highlightSegments, notificationSearchTerms, searchNotifications, type NotificationLabels } from "./notificationSearch";

const notification = (overrides: Partial<MirroredNotification>): MirroredNotification => ({
  target: { sourceDeviceId: "phone-a", notificationKey: "key", lifetime: "one" },
  packageName: "com.chat",
  appName: "Chat",
  title: "Title",
  text: "Body",
  postedAt: 0,
  dismissible: true,
  seen: false,
  dismissalPending: false,
  ...overrides,
});

const labels: NotificationLabels = {
  appName: n => n.appName,
  sourceName: n => `Phone ${n.target.sourceDeviceId}`,
  title: n => n.title,
};

describe("notificationSearchTerms", () => {
  it("trims, lowercases, and splits on whitespace", () => {
    expect(notificationSearchTerms("  Hello   WORLD\tfoo ")).toEqual(["hello", "world", "foo"]);
    expect(notificationSearchTerms("   ")).toEqual([]);
  });

  it("caps input at 128 code points", () => {
    const terms = notificationSearchTerms(`${"a".repeat(127)}😀bcd`);
    expect(terms).toEqual([`${"a".repeat(127)}😀`]);
    expect(Array.from(terms[0])).toHaveLength(128);
  });
});

describe("searchNotifications", () => {
  const list = [
    notification({ appName: "Chat", title: "Alice", text: "lunch at noon", target: { sourceDeviceId: "phone-a", notificationKey: "1", lifetime: "one" } }),
    notification({ appName: "Mail", title: "Invoice", text: "Due Friday", target: { sourceDeviceId: "phone-b", notificationKey: "2", lifetime: "one" } }),
    notification({ appName: "Chat", title: "Bob", text: "see you", target: { sourceDeviceId: "phone-b", notificationKey: "3", lifetime: "one" } }),
  ];

  it("returns the same list for an empty query", () => {
    expect(searchNotifications(list, "   ", labels)).toBe(list);
  });

  it("requires every term to match some field, case-insensitively, preserving order", () => {
    expect(searchNotifications(list, "CHAT alice", labels)).toEqual([list[0]]);
    expect(searchNotifications(list, "due friday", labels)).toEqual([list[1]]);
    expect(searchNotifications(list, "chat invoice", labels)).toEqual([]);
    expect(searchNotifications(list, "chat", labels)).toEqual([list[0], list[2]]);
  });

  it("matches phone names through the labels callbacks", () => {
    expect(searchNotifications(list, "phone-b", labels)).toEqual([list[1], list[2]]);
  });

  it("uses the label callbacks rather than raw fields", () => {
    const custom: NotificationLabels = { appName: () => "Zeta", sourceName: () => "Home", title: () => "Q" };
    expect(searchNotifications(list, "zeta home q", custom)).toEqual(list);
    expect(searchNotifications(list, "chat", custom)).toEqual([]);
  });
});

describe("highlightSegments", () => {
  it("returns no segments for empty text and one plain segment without terms", () => {
    expect(highlightSegments("", ["a"])).toEqual([]);
    expect(highlightSegments("Hello", [])).toEqual([{ text: "Hello", match: false }]);
  });

  it("highlights a single match", () => {
    expect(highlightSegments("say hello there", ["hello"])).toEqual([
      { text: "say ", match: false },
      { text: "hello", match: true },
      { text: " there", match: false },
    ]);
  });

  it("highlights multiple terms and preserves original casing", () => {
    expect(highlightSegments("Hello WORLD", ["world", "hello"])).toEqual([
      { text: "Hello", match: true },
      { text: " ", match: false },
      { text: "WORLD", match: true },
    ]);
  });

  it("merges overlapping and adjacent matches", () => {
    expect(highlightSegments("abcdef", ["abc", "bcd"])).toEqual([
      { text: "abcd", match: true },
      { text: "ef", match: false },
    ]);
    expect(highlightSegments("abcdef", ["ab", "cd"])).toEqual([
      { text: "abcd", match: true },
      { text: "ef", match: false },
    ]);
  });

  it("escapes regular-expression characters in terms", () => {
    expect(highlightSegments("cost (usd) 5.00", ["(usd)", "5.00"])).toEqual([
      { text: "cost ", match: false },
      { text: "(usd)", match: true },
      { text: " ", match: false },
      { text: "5.00", match: true },
    ]);
  });
});
