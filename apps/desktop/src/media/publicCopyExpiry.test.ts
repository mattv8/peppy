import { describe, expect, it } from "vitest";
import { expiryText } from "./publicCopyExpiry";

describe("public copy expiry text", () => {
  it.each([
    [-1, "No expiry was reported; revoke it on the server when it is no longer needed."],
    [0, "No expiry was reported; revoke it on the server when it is no longer needed."],
    [0.1, "Expires in 1 min."],
    [60, "Expires in 1 min."],
    [60.1, "Expires in 2 min."],
    [3599.9, "Expires in 60 min."],
    [3600, "Expires in 1 h."],
    [5399.9, "Expires in 1 h."],
    [5400, "Expires in 2 h."],
    [172799.9, "Expires in 48 h."],
    [172800, "Expires in 2 days."],
    [215999.9, "Expires in 2 days."],
    [216000, "Expires in 3 days."],
  ])("formats %s seconds", (seconds, expected) => {
    expect(expiryText(seconds)).toBe(expected);
  });
});
