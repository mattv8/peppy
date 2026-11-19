import { describe, expect, it } from "vitest";
import type { MirroredNotification } from "../bridge/bridge";
import { notificationTuple } from "./notificationIdentity";
import { notificationKey, sectionNotifications, stackNotifications } from "./notificationSections";

const notification = (id: string, postedAt: number, overrides: Partial<MirroredNotification> = {}): MirroredNotification => ({
  target: { sourceDeviceId: "phone-a", notificationKey: id, lifetime: "one" },
  packageName: "com.chat",
  appName: "Chat",
  title: id,
  text: "",
  postedAt,
  dismissible: true,
  seen: false,
  dismissalPending: false,
  ...overrides,
});

// Local-time timestamps keep these tests independent of the machine timezone.
const now = new Date(2026, 10, 16, 15).getTime();
const today = (hour: number) => new Date(2026, 10, 16, hour).getTime();
const yesterday = (hour: number) => new Date(2026, 10, 15, hour).getTime();
const older = new Date(2026, 10, 14, 9).getTime();

const titles = (section: { items: MirroredNotification[] }) => section.items.map(item => item.title);

describe("notificationKey", () => {
  it("builds the same tuple used for row identity", () => {
    expect(notificationKey({ sourceDeviceId: "phone-a", notificationKey: "k", lifetime: "one" })).toBe(
      notificationTuple("phone-a", "k", "one"),
    );
    expect(notificationKey({ sourceDeviceId: "phone-a", notificationKey: "k", lifetime: "one" })).not.toBe(
      notificationKey({ sourceDeviceId: "phone-a", notificationKey: "k", lifetime: "two" }),
    );
  });
});

describe("sectionNotifications", () => {
  it("places unseen older notifications in Older", () => {
    const sections = sectionNotifications([notification("unseen", older)], now);
    expect(sections.map(section => [section.id, titles(section)])).toEqual([["older", ["unseen"]]]);
  });

  it("labels the current day as Today", () => {
    const sections = sectionNotifications([notification("a", today(9))], now);
    expect(sections.map(section => [section.id, section.label])).toEqual([["today", "Today"]]);
  });

  it("splits by local calendar day into yesterday and older", () => {
    const list = [
      notification("today", today(1)),
      notification("yesterday-late", yesterday(23)),
      notification("yesterday-early", yesterday(0)),
      notification("older", older),
    ];
    const sections = sectionNotifications(list, now);
    expect(sections.map(section => [section.id, section.label])).toEqual([
      ["today", "Today"],
      ["yesterday", "Yesterday"],
      ["older", "Older"],
    ]);
    expect(titles(sections[1])).toEqual(["yesterday-late", "yesterday-early"]);
    expect(titles(sections[2])).toEqual(["older"]);
  });

  it("splits yesterday's last minute from today's first minute", () => {
    const midnight = new Date(2026, 10, 16, 0, 0, 0, 0).getTime();
    const lastMinute = new Date(2026, 10, 15, 23, 59, 0, 0).getTime();
    const sections = sectionNotifications([notification("midnight", midnight), notification("late", lastMinute)], now);
    expect(sections.map(section => [section.id, titles(section)])).toEqual([["today", ["midnight"]], ["yesterday", ["late"]]]);
  });

  it("finds yesterday across a month boundary", () => {
    const firstOfMonth = new Date(2026, 11, 1, 10).getTime();
    const lastOfPrevious = new Date(2026, 10, 30, 22).getTime();
    const sections = sectionNotifications([notification("prev", lastOfPrevious)], firstOfMonth);
    expect(sections.map(section => section.id)).toEqual(["yesterday"]);
  });

  it("keeps notifications from a phone clock running ahead with today", () => {
    const tomorrow = new Date(2026, 10, 17, 9).getTime();
    expect(sectionNotifications([notification("ahead", tomorrow)], now).map(section => section.id)).toEqual(["today"]);
  });

  it("omits empty sections and preserves item order", () => {
    const list = [notification("second", today(12)), notification("first", today(8))];
    const sections = sectionNotifications(list, now);
    expect(sections).toHaveLength(1);
    expect(titles(sections[0])).toEqual(["second", "first"]);
    expect(sectionNotifications([], now)).toEqual([]);
  });
});

describe("stackNotifications", () => {
  it("groups per phone and package so the same app on two phones makes two stacks", () => {
    const list = [
      notification("a1", 100, { target: { sourceDeviceId: "phone-a", notificationKey: "a1", lifetime: "one" } }),
      notification("b1", 200, { target: { sourceDeviceId: "phone-b", notificationKey: "b1", lifetime: "one" } }),
      notification("a2", 300, { target: { sourceDeviceId: "phone-a", notificationKey: "a2", lifetime: "one" } }),
    ];
    const stacks = stackNotifications(list);
    expect(stacks).toHaveLength(2);
    expect(stacks.map(stack => stack.sourceDeviceId)).toEqual(["phone-a", "phone-b"]);
    expect(titles(stacks[0])).toEqual(["a2", "a1"]);
    expect(stacks[0].packageName).toBe("com.chat");
  });

  it("orders items newest-first and stacks by their newest item", () => {
    const list = [
      notification("old-chat", 100, { packageName: "chat" }),
      notification("new-mail", 500, { packageName: "mail" }),
      notification("mid-chat", 300, { packageName: "chat" }),
    ];
    const stacks = stackNotifications(list);
    expect(stacks.map(stack => stack.packageName)).toEqual(["mail", "chat"]);
    expect(stacks[1].items.map(item => item.title)).toEqual(["mid-chat", "old-chat"]);
  });

  it("returns no stacks for an empty list", () => {
    expect(stackNotifications([])).toEqual([]);
  });
});
