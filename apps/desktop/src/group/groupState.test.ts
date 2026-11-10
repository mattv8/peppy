import { describe, expect, it } from "vitest";
import type { GroupView } from "@peppy/desktop-ui/bridge-contracts";
import { groupAllowsTypingSend, groupComposerNotice, groupIsReadOnly, groupMembers, groupNoticeState } from "./groupState";
import { interleaveGroupNotices } from "./groupTimeline";
import { groupNoticeItems } from "./groupNotices";

const view = (state: GroupView["state"], members: string[] = []): GroupView => ({ members, state, memberRevision: "1", canCreate: false, createNeedsConfirmation: false });

describe("groupNoticeState", () => {
  it("maps withdrawn to left, paused to paused, and says nothing for other states", () => {
    expect(groupNoticeState({ state: "withdrawn" })).toBe("left");
    expect(groupNoticeState({ state: "paused" })).toBe("paused");
    expect(groupNoticeState({ state: "live" })).toBeUndefined();
    expect(groupNoticeState({ state: "unlinked" })).toBeUndefined();
    expect(groupNoticeState(undefined)).toBeUndefined();
  });
});

describe("group read-only and typing", () => {
  it("makes only a withdrawn group read-only", () => {
    expect(groupIsReadOnly({ state: "withdrawn" })).toBe(true);
    expect(groupIsReadOnly({ state: "paused" })).toBe(false);
    expect(groupIsReadOnly(undefined)).toBe(false);
  });

  it("sends typing for one-to-one chats and live groups only", () => {
    expect(groupAllowsTypingSend({ participants: ["+15551230001"], group: undefined })).toBe(true);
    expect(groupAllowsTypingSend({ participants: ["+15551230001", "+15551230002"], group: view("live") })).toBe(true);
    expect(groupAllowsTypingSend({ participants: ["+15551230001", "+15551230002"], group: view("paused") })).toBe(false);
    expect(groupAllowsTypingSend({ participants: ["+15551230001", "+15551230002"], group: view("unlinked") })).toBe(false);
    expect(groupAllowsTypingSend({ participants: ["+15551230001", "+15551230002"], group: undefined })).toBe(false);
  });
});

describe("groupMembers", () => {
  it("prefers the group projection over the native participants", () => {
    expect(groupMembers({ participants: ["+15551230009"], group: view("live", ["+15551230001"]) })).toEqual(["+15551230001"]);
    expect(groupMembers({ participants: ["+15551230009"], group: undefined })).toEqual(["+15551230009"]);
  });
});

describe("groupComposerNotice", () => {
  it("keeps the refused members and revision for a members-changed refusal", () => {
    expect(groupComposerNotice({ reason: "members_changed", group: { members: ["+15551230001"], memberRevision: "4" } }, "k")).toEqual({
      kind: "members-changed",
      key: "k",
      memberRevision: "4",
      members: ["+15551230001"],
    });
  });

  it("leaves a members-changed refusal without metadata generic", () => {
    expect(groupComposerNotice({ reason: "members_changed" }, "k")).toEqual({ kind: "members-changed", key: "k", memberRevision: undefined, members: undefined });
  });

  it.each([
    ["create_unconfirmed", "create-unconfirmed"],
    ["create_not_rcs", "create-not-rcs"],
    ["update_required", "update-required"],
    ["withdrawn", "withdrawn"],
  ] as const)("maps %s to %s", (reason, kind) => {
    expect(groupComposerNotice({ reason }, "k")).toEqual({ kind, key: "k" });
  });

  it("returns nothing for a reason with generic copy", () => {
    expect(groupComposerNotice({ reason: "something_else" }, "k")).toBeUndefined();
    expect(groupComposerNotice({}, "k")).toBeUndefined();
  });
});

describe("interleaveGroupNotices", () => {
  const group = (id: string, timestamp: string) => ({ id, messages: [{ timestamp }] });
  const notice = (id: string, atMs: string) => groupNoticeItems([{ kind: "added", member: "+15551230001", atMs }]).map((item) => ({ ...item, id }))[0];

  it("places each notice before the first group at or after its time", () => {
    const items = interleaveGroupNotices([group("a", "1000"), group("b", "3000")], [notice("n", "2000")]);
    expect(items.map((item) => item.id)).toEqual(["a", "n", "b"]);
  });

  it("keeps notices later than every message after the last group", () => {
    const items = interleaveGroupNotices([group("a", "1000")], [notice("late", "9000")]);
    expect(items.map((item) => item.id)).toEqual(["a", "late"]);
  });

  it("keeps groups in order and leaves the list alone when there are no notices", () => {
    expect(interleaveGroupNotices([group("a", "1"), group("b", "2")], []).map((item) => item.kind)).toEqual(["group", "group"]);
  });
});
