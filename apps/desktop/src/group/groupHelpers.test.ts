import { describe, expect, it } from "vitest";
import { peppyCopy } from "../generated/peppyCopy";
import { copySegments, fillCopy, groupTypingLabel, summarizeGroupNames } from "./groupCopy";
import { GROUP_NAME_MAX_LENGTH, displayGroupName, normalizeGroupName } from "./groupName";
import { groupSubtitle, groupTitle } from "./groupTitle";

const names: Record<string, string> = { "+15551230001": "Alex", "+15551230002": "Sam", "+15551230003": "Jo" };
const displayName = (address: string) => names[address] ?? address;

describe("fillCopy", () => {
  it("fills every named token and leaves an unknown token visible", () => {
    expect(fillCopy("{first} and {second} are typing {extra}", { first: "Alex", second: "Sam" })).toBe("Alex and Sam are typing {extra}");
  });

  it("returns the template unchanged when there are no tokens", () => {
    expect(fillCopy("Members changed.", {})).toBe("Members changed.");
  });

  it("keeps text around values in order", () => {
    expect(copySegments("{count} people", { count: 3 })).toEqual([
      { text: "3", value: true, offset: 0 },
      { text: " people", value: false, offset: 7 },
    ]);
  });
});

describe("summarizeGroupNames", () => {
  it("lists up to five names with commas", () => {
    expect(summarizeGroupNames(["Alex", "Sam", "Jo"])).toBe("Alex, Sam, Jo");
  });

  it("adds the overflow count after five names", () => {
    expect(summarizeGroupNames(["A", "B", "C", "D", "E", "F", "G"])).toBe("A, B, C, D, E and 2 more");
  });

  it("returns an empty string for no names", () => {
    expect(summarizeGroupNames([])).toBe("");
  });
});

describe("groupTypingLabel", () => {
  it("names one typing participant", () => {
    expect(groupTypingLabel(["+15551230001"], displayName)).toBe("Alex is typing");
  });

  it("names two typing participants in order", () => {
    expect(groupTypingLabel(["+15551230001", "+15551230002"], displayName)).toBe("Alex and Sam are typing");
  });

  it("counts three or more participants", () => {
    expect(groupTypingLabel(["+15551230001", "+15551230002", "+15551230003"], displayName)).toBe("3 people are typing");
  });

  it("is empty when nobody is typing", () => {
    expect(groupTypingLabel([], displayName)).toBe("");
  });

  it("uses the display name callback, so contact names and formatted numbers both work", () => {
    expect(groupTypingLabel(["+18015551234"], (address) => `Contact for ${address}`)).toBe("Contact for +18015551234 is typing");
  });
});

describe("normalizeGroupName", () => {
  it("trims and collapses internal whitespace", () => {
    expect(normalizeGroupName("  Weekend \n  crew  ")).toBe("Weekend crew");
  });

  it("treats blank or missing names as unnamed", () => {
    expect(normalizeGroupName("   ")).toBeUndefined();
    expect(normalizeGroupName("")).toBeUndefined();
    expect(normalizeGroupName(undefined)).toBeUndefined();
  });

  it("never shortens a name for display", () => {
    expect(displayGroupName("x".repeat(150))).toHaveLength(150);
    expect(displayGroupName("  Crew \n  ")).toBe("Crew");
  });

  it("caps the length at 100 characters, counting code points, for the composer", () => {
    expect(normalizeGroupName("a".repeat(150))).toHaveLength(GROUP_NAME_MAX_LENGTH);
    expect(Array.from(normalizeGroupName("🌊".repeat(150)) ?? "")).toHaveLength(GROUP_NAME_MAX_LENGTH);
  });
});

describe("groupTitle and groupSubtitle", () => {
  it("uses the group name when one is set", () => {
    expect(groupTitle({ name: "Weekend crew", memberNames: ["Alex", "Sam"] })).toBe("Weekend crew");
  });

  it("falls back to the member names when the name is blank", () => {
    expect(groupTitle({ name: "  ", memberNames: ["Alex", "Sam"] })).toBe("Alex, Sam");
  });

  it("falls back to 'Group' when there are no names at all", () => {
    expect(groupTitle({ memberNames: [] })).toBe(peppyCopy.group_tag_group);
  });

  it("shows the member names under a named group", () => {
    expect(groupSubtitle({ name: "Weekend crew", memberNames: ["Alex", "Sam"] })).toBe("Alex, Sam");
  });

  it("shows the count, including the user, under an unnamed group", () => {
    expect(groupSubtitle({ memberNames: ["Alex", "Sam", "Jo"] })).toBe("4 people");
  });
});
