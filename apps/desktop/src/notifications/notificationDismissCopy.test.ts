import { describe, expect, it } from "vitest";
import {
  blockerCopy, blockerReasonSeverity, blockerRevealLabel, formatReportAge, groupedBlockersBody, groupedBlockersTitle, joinNames,
  localFailureText, reportedText, waitingCountLabel, waitingHelpParagraphs,
} from "./notificationDismissCopy";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

describe("formatReportAge", () => {
  it("reads as just now under a minute and clamps future reports", () => {
    expect(formatReportAge(1_000, 1_000)).toBe("just now");
    expect(formatReportAge(1_000, 1_000 + MINUTE - 1)).toBe("just now");
    expect(formatReportAge(5_000, 1_000)).toBe("just now");
  });
  it("uses minutes, hours and days", () => {
    expect(formatReportAge(0, 5 * MINUTE)).toBe("5m");
    expect(formatReportAge(0, 3 * HOUR + 10 * MINUTE)).toBe("3h");
    expect(formatReportAge(0, 2 * DAY)).toBe("2d");
  });
  it("wraps the age in the report sentence", () => {
    expect(reportedText(0, 10)).toBe("Reported just now.");
    expect(reportedText(0, 5 * MINUTE)).toBe("Reported 5m ago.");
  });
});

describe("notification dismissal copy", () => {
  it("pluralises the waiting count", () => {
    expect(waitingCountLabel(1)).toBe("1 waiting for phone");
    expect(waitingCountLabel(4)).toBe("4 waiting for phone");
    expect(waitingHelpParagraphs(4)[3]).toContain("\u201C4 waiting for phone\u201D");
  });
  it("formats local failure reasons", () => {
    expect(localFailureText("Peppy is locked. Unlock it, then try again.")).toBe("Couldn't dismiss. Peppy is locked. Unlock it, then try again.");
  });
  it("uses the singular waiting sentence for one notification", () => {
    expect(blockerCopy("mirroringOff", "Pixel", 1).body).toBe(
      "1 dismissed notification is waiting. Turn on Notification mirroring in Peppy on the phone, and Pixel will try to clear them.",
    );
    expect(blockerCopy("mirroringOff", "Pixel", 2).body).toContain("2 dismissed notifications are waiting.");
  });
  it("gives each reason its title and fix sentence", () => {
    expect(blockerCopy("notificationAccessOff", "Pixel", 1).title).toBe("Notification access is off on Pixel");
    expect(blockerCopy("waitingForWifi", "Pixel", 1).title).toBe("Pixel is waiting for Wi-Fi");
    expect(blockerCopy("other", "Pixel", 1).fix).toBe("Something on the phone is stopping it. Open Peppy on the phone to check.");
  });
  it("ends every blocker help with the closing line", () => {
    for (const reason of ["mirroringOff", "notificationAccessOff", "waitingForWifi", "other"] as const) {
      const paragraphs = blockerCopy(reason, "Pixel", 1).help.paragraphs;
      expect(paragraphs.at(-1)).toBe("This notice goes away after Pixel reports back. A phone that is locked or offline can't report, so it can take a while.");
    }
  });
  it("makes wifi the only informational severity", () => {
    expect(blockerReasonSeverity("waitingForWifi")).toBe("info");
    expect(blockerReasonSeverity("notificationAccessOff")).toBe("warning");
    expect(blockerReasonSeverity("other")).toBe("warning");
  });
  it("labels the reveal toggle", () => {
    expect(blockerRevealLabel(2, false)).toBe("Show 2 dismissed");
    expect(blockerRevealLabel(2, true)).toBe("Hide 2 dismissed");
  });
  it("joins phone names and titles the grouped banner", () => {
    expect(joinNames(["A", "B", "C"])).toBe("A, B and C");
    expect(joinNames(["A", "B"])).toBe("A and B");
    expect(groupedBlockersBody(["A", "B", "C"], 5)).toBe("A, B and C have 5 dismissed notifications waiting. Show phones to see what each one needs.");
    expect(groupedBlockersTitle(3, false)).toBe("3 phones can't clear notifications");
    expect(groupedBlockersTitle(3, true)).toBe("3 phones are waiting for Wi-Fi");
  });
});
