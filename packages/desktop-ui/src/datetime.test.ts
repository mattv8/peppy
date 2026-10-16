import { afterEach, describe, expect, it, vi } from "vitest";
import {
  formatDateTime,
  formatElapsedShort,
  formatRelativeTime,
  isoDateTime,
  toDate,
} from "./datetime";

const UTC_FORMAT = { locale: "en-US", timeZone: "UTC" };
const EXAMPLE_TIME = 1_788_435_180_000;

describe("datetime", () => {
  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it("formats a medium date and short time with an explicit locale and timezone", () => {
    expect(formatDateTime(EXAMPLE_TIME, UTC_FORMAT)).toBe("Sep 3, 2026, 11:33 AM");
  });

  it("parses positive epoch milliseconds and ISO timestamps", () => {
    expect(toDate(EXAMPLE_TIME)?.toISOString()).toBe("2026-09-03T11:33:00.000Z");
    expect(toDate(String(EXAMPLE_TIME))?.toISOString()).toBe("2026-09-03T11:33:00.000Z");
    expect(toDate("2026-09-03T11:33:00.000Z")?.toISOString()).toBe("2026-09-03T11:33:00.000Z");
    expect(isoDateTime(EXAMPLE_TIME)).toBe("2026-09-03T11:33:00.000Z");
  });

  it("rejects missing, nonpositive, nonfinite, numeric-like, time-only, and invalid inputs", () => {
    for (const value of [undefined, null, "", "  ", 0, -1, Infinity, NaN, "0", "-1", "1e3", "11:33 AM", "2026-02-30", "not a date"]) {
      expect(toDate(value)).toBeUndefined();
    }
  });

  it("rejects timestamps outside JavaScript's valid Date range", () => {
    for (const value of [1e20, 8.64e15 + 1, "9".repeat(17)]) {
      expect(toDate(value)).toBeUndefined();
      expect(formatDateTime(value, UTC_FORMAT)).toBeUndefined();
      expect(isoDateTime(value)).toBeUndefined();
    }
  });

  it("converts times through the requested timezone", () => {
    expect(formatDateTime("2026-09-03T00:30:00.000Z", { locale: "en-US", timeZone: "America/Los_Angeles" })).toBe("Sep 2, 2026, 5:30 PM");
  });

  it("uses the current default timezone instead of a cached formatter", () => {
    const value = "2026-07-01T00:30:00.000Z";
    vi.stubEnv("TZ", "UTC");
    const utc = formatDateTime(value);

    vi.stubEnv("TZ", "America/Los_Angeles");
    const expected = new Intl.DateTimeFormat(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(new Date(value));
    expect(formatDateTime(value)).toBe(expected);
    expect(expected).not.toBe(utc);
  });

  it("formats elapsed notification time with future clamping", () => {
    const now = 1_788_433_980_000;
    expect(formatElapsedShort(now, now)).toBe("now");
    expect(formatElapsedShort(now - 90_000, now)).toBe("2m");
    expect(formatElapsedShort(now - 90 * 60_000, now)).toBe("2h");
    expect(formatElapsedShort(now - 25 * 60 * 60_000, now)).toBe("1d");
    expect(formatElapsedShort(now + 60_000, now)).toBe("now");
    expect(formatElapsedShort(0, now)).toBeUndefined();
    expect(formatElapsedShort(now, Infinity)).toBeUndefined();
    expect(formatElapsedShort(now, NaN)).toBeUndefined();
  });

  it("formats relative time in runtime locale minute units", () => {
    const now = 1_788_433_980_000;
    const formatter = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
    expect(formatRelativeTime(now - 90_000, now)).toBe(formatter.format(-1, "minute"));
    expect(formatRelativeTime(now + 60_000, now)).toBe(formatter.format(1, "minute"));
    expect(formatRelativeTime("invalid", now)).toBeUndefined();
    expect(formatRelativeTime(now, Infinity)).toBeUndefined();
    expect(formatRelativeTime(now, NaN)).toBeUndefined();
  });
});
