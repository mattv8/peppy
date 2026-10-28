import { describe, expect, it } from "vitest";
import { hiddenTimeIds } from "./timestamp-bursts";

type TestMessage = {
  id: string;
  timestamp: string;
  shown?: boolean;
  sender?: "self" | "other";
};

function message(id: string, timestamp: string, shown = true, sender?: TestMessage["sender"]): TestMessage {
  return { id, timestamp, shown, sender };
}

function hidden(messages: readonly TestMessage[]): Set<string> {
  return hiddenTimeIds(messages, (item) => item.shown === true);
}

describe("hiddenTimeIds", () => {
  it("keeps timestamps for empty and singleton message lists", () => {
    expect(hidden([])).toEqual(new Set());
    expect(hidden([message("only", "2026-01-01T12:00:00.000Z")])).toEqual(new Set());
  });

  it("hides the earlier eligible time at the inclusive one-hour boundary", () => {
    const start = "2026-01-01T12:00:00.000Z";
    expect(hidden([message("first", start), message("last", "2026-01-01T13:00:00.000Z")])).toEqual(new Set(["first"]));
    expect(hidden([message("first", start), message("after", "2026-01-01T13:00:00.001Z")])).toEqual(new Set());
    expect(hidden([message("before", "2026-01-01T10:59:59.999Z"), message("last", start)])).toEqual(new Set());
    expect(hidden([message("", start), message("last", "2026-01-01T12:01:00.000Z")])).toEqual(new Set([""]));
  });

  it("chains adjacent links beyond one hour and ignores senders", () => {
    const messages = [
      message("one", "2026-01-01T12:00:00.000Z", true, "self"),
      message("two", "2026-01-01T12:59:00.000Z", true, "other"),
      message("three", "2026-01-01T13:58:00.000Z", true, "self"),
    ];

    expect(hidden(messages)).toEqual(new Set(["one", "two"]));
  });

  it("treats invalid anchors as singleton bursts", () => {
    expect(hidden([
      message("before", "2026-01-01T12:00:00.000Z"),
      message("invalid", "not-a-date"),
      message("after", "2026-01-01T12:01:00.000Z"),
    ])).toEqual(new Set());
  });

  it("uses the shared parser for integer epoch and ISO anchors while rejecting decimal epochs", () => {
    expect(hidden([
      message("epoch", "1767268800000"),
      message("iso", "2026-01-01T12:30:00.000Z"),
    ])).toEqual(new Set(["epoch"]));
    expect(hidden([
      message("decimal", "1767268800000.5"),
      message("iso", "2026-01-01T12:30:00.000Z"),
    ])).toEqual(new Set());
  });

  it("links backwards adjacent gaps by absolute difference", () => {
    expect(hidden([
      message("later", "2026-01-01T13:00:00.000Z"),
      message("earlier", "2026-01-01T12:00:00.000Z"),
    ])).toEqual(new Set(["later"]));
    expect(hidden([
      message("later", "2026-01-01T13:00:00.001Z"),
      message("earlier", "2026-01-01T12:00:00.000Z"),
    ])).toEqual(new Set());
  });

  it("keeps the last time that can render when queued messages chain a burst", () => {
    expect(hidden([
      message("eligible", "2026-01-01T12:00:00.000Z"),
      message("queued", "2026-01-01T12:50:00.000Z", false),
      message("eligible-last", "2026-01-01T13:40:00.000Z"),
    ])).toEqual(new Set(["eligible"]));
  });

  it("retains the prior keeper when a queued message ends the loaded window", () => {
    expect(hidden([
      message("eligible", "2026-01-01T12:00:00.000Z"),
      message("queued", "2026-01-01T12:50:00.000Z", false),
    ])).toEqual(new Set());
  });

  it("does not hide timestamps when no message in a burst can render one", () => {
    expect(hidden([
      message("queued-first", "2026-01-01T12:00:00.000Z", false),
      message("queued-last", "2026-01-01T12:50:00.000Z", false),
    ])).toEqual(new Set());
  });

  it("demotes the previous keeper when an eligible message is appended", () => {
    const first = message("first", "2026-01-01T12:00:00.000Z");
    expect(hidden([first])).toEqual(new Set());
    expect(hidden([first, message("appended", "2026-01-01T12:50:00.000Z")])).toEqual(new Set(["first"]));
  });

  it("does not change an existing keeper when history is prepended", () => {
    const existing = [message("existing", "2026-01-01T12:30:00.000Z")];
    const withHistory = [message("history", "2026-01-01T12:00:00.000Z"), ...existing];

    expect(hidden(existing)).toEqual(new Set());
    expect(hidden(withHistory)).toEqual(new Set(["history"]));
  });

  it("preserves an existing keeper when far-gap history is prepended", () => {
    const existing = [message("existing", "2026-01-01T14:00:00.000Z")];
    const withHistory = [message("history", "2026-01-01T12:00:00.000Z"), ...existing];

    expect(hidden(existing)).toEqual(new Set());
    expect(hidden(withHistory)).toEqual(new Set());
  });
});
