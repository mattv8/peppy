import { describe, expect, it } from "vitest";
import { NOTICE_NAME_MAX_LENGTH, groupNoticeItems, groupNoticeCopy, groupNoticeText } from "./groupNotices";

const displayName = (address: string) => ({ "+15551230001": "Alex", "+15551230002": "Sam", "+15551230003": "Jo" } as Record<string, string>)[address] ?? address;

describe("groupNoticeItems", () => {
  it("maps each core notice to one item with a stable id and the change time", () => {
    expect(groupNoticeItems([
      { kind: "added", member: "+15551230003", atMs: "2000" },
      { kind: "removed", member: "+15551230002", atMs: "3000" },
      { kind: "renamed", name: "Sunday crew", atMs: "4000" },
    ])).toEqual([
      { id: "group-notice-added-2000-+15551230003", kind: "added", revision: "2000", atMs: 2000, addresses: ["+15551230003"] },
      { id: "group-notice-removed-3000-+15551230002", kind: "removed", revision: "3000", atMs: 3000, addresses: ["+15551230002"] },
      { id: "group-notice-name-4000", kind: "name", revision: "4000", atMs: 4000, name: "Sunday crew", addresses: [] },
    ]);
  });

  it("maps a rename without a name to a removed name, and trims a blank name", () => {
    expect(groupNoticeItems([{ kind: "renamed", atMs: "5", name: "   " }])[0]).toMatchObject({ kind: "name", name: undefined });
  });

  it("drops membership notices without a member and notices with a malformed time", () => {
    expect(groupNoticeItems([{ kind: "added", atMs: "1" }, { kind: "removed", member: "+15551230002", atMs: "x" }])).toEqual([]);
  });

  it("returns nothing when core sent no notices", () => {
    expect(groupNoticeItems(undefined)).toEqual([]);
    expect(groupNoticeItems([])).toEqual([]);
  });
});

describe("groupNoticeText", () => {
  it("names one member for an addition or removal", () => {
    const [added] = groupNoticeItems([{ kind: "added", member: "+15551230003", atMs: "2" }]);
    const [removed] = groupNoticeItems([{ kind: "removed", member: "+15551230002", atMs: "3" }]);
    expect(groupNoticeText(added, displayName)).toBe("Jo was added");
    expect(groupNoticeText(removed, displayName)).toBe("Sam is no longer in the group");
  });

  it("uses the neutral removal wording, since a roster change cannot tell leave from removal", () => {
    const [removed] = groupNoticeItems([{ kind: "removed", member: "+15551230001", atMs: "3" }]);
    expect(groupNoticeText(removed, displayName)).not.toMatch(/left|removed from/);
  });

  it("names the group when it is renamed and says so when the name is removed", () => {
    const [renamed] = groupNoticeItems([{ kind: "renamed", name: "Sunday crew", atMs: "4" }]);
    const [cleared] = groupNoticeItems([{ kind: "renamed", atMs: "5" }]);
    expect(groupNoticeText(renamed, displayName)).toBe("Name changed to Sunday crew");
    expect(groupNoticeText(cleared, displayName)).toBe("Name removed");
  });

  it("cuts a long name at the notice limit and keeps the full text in title", () => {
    const long = "L".repeat(80);
    const [renamed] = groupNoticeItems([{ kind: "renamed", name: long, atMs: "6" }]);
    const copy = groupNoticeCopy(renamed, displayName);
    expect(copy.values.name).toHaveLength(NOTICE_NAME_MAX_LENGTH);
    expect(copy.values.name.endsWith("…")).toBe(true);
    expect(copy.title).toBe(`Name changed to ${long}`);
  });
});
