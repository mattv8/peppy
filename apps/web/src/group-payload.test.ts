import { describe, expect, it } from "vitest";
import type { ConversationView, DesktopSnapshot, GroupView } from "@peppy/desktop-ui/bridge-contracts";
import { groupDraftFields, normalizeGroupSnapshot, parseGroupView, parseSendResult, parseTypingSignal } from "./group-payload";

/** Wire fixtures, one per UX state in ux-spec §12 and contracts §6. */
const fixtures = {
  liveNamed: { name: "Weekend crew", members: ["+15551230001", "+15551230002"], state: "live", memberRevision: "4", canCreate: false, createNeedsConfirmation: false },
  liveUnnamed: { members: ["+15551230001", "+15551230002", "+15551230003"], state: "live", memberRevision: "1", canCreate: false, createNeedsConfirmation: false },
  paused: { name: "Weekend crew", members: ["+15551230001", "+15551230002"], state: "paused", memberRevision: "5", canCreate: false, createNeedsConfirmation: false },
  mms: { members: ["+18015551234", "+15555550199"], state: "unlinked", memberRevision: "2", canCreate: true, createNeedsConfirmation: false },
  proving: { members: ["+18015551234", "+15555550199"], state: "proving", memberRevision: "2", canCreate: false, createNeedsConfirmation: false },
  withdrawn: { members: ["+15551230001"], state: "withdrawn", memberRevision: "9", canCreate: false, createNeedsConfirmation: false },
  createUnconfirmed: { members: ["+15551230001", "+15551230002"], state: "unlinked", memberRevision: "1", canCreate: false, createNeedsConfirmation: true },
  longName: { name: "Ünïcode 🌊 ".repeat(9), members: ["+15551230001"], state: "live", memberRevision: "3", canCreate: false, createNeedsConfirmation: false },
};

const conversation = (group?: unknown): ConversationView => ({ id: "conv-group", name: "Weekend crew", preview: "", unread: 0, messages: [], participants: ["+15551230001", "+15551230002"], ...(group === undefined ? {} : { group }) }) as ConversationView;
const snapshot = (conversations: ConversationView[]): DesktopSnapshot => ({ version: "1", mode: "browser", conversations } as unknown as DesktopSnapshot);

describe("parseGroupView", () => {
  it.each(Object.entries(fixtures))("accepts the %s group fixture unchanged", (_, fixture) => {
    expect(parseGroupView(fixture)).toEqual(fixture);
  });

  it("drops fields the contract does not define", () => {
    expect(parseGroupView({ ...fixtures.liveNamed, secret: "x" })).toEqual(fixtures.liveNamed);
  });

  it("keeps the core's notices and drops only the notices it cannot describe", () => {
    const parsed = parseGroupView({
      ...fixtures.liveNamed,
      notices: [
        { kind: "added", member: "+15551230002", atMs: "1730000000000" },
        { kind: "renamed", name: "Sunday crew", atMs: "1730000001000" },
        { kind: "renamed", atMs: "1730000002000" },
        { kind: "added", atMs: "1730000003000" },
        { kind: "removed", member: "not-a-number", atMs: "1" },
        { kind: "moved", member: "+15551230002", atMs: "1" },
        { kind: "removed", member: "+15551230002", atMs: "-1" },
      ],
    });
    expect(parsed?.notices).toEqual([
      { kind: "added", member: "+15551230002", atMs: "1730000000000" },
      { kind: "renamed", name: "Sunday crew", atMs: "1730000001000" },
      { kind: "renamed", atMs: "1730000002000" },
    ]);
  });

  it("omits notices when the core sent none", () => {
    expect(parseGroupView(fixtures.liveNamed)).not.toHaveProperty("notices");
  });

  it("omits an absent name instead of inventing one", () => {
    expect(parseGroupView(fixtures.liveUnnamed)).not.toHaveProperty("name");
  });

  it.each([
    ["an unknown state", { ...fixtures.liveNamed, state: "left" }],
    ["a non-canonical member", { ...fixtures.liveNamed, members: ["(801) 555-0100"] }],
    ["a member that is not an array", { ...fixtures.liveNamed, members: "+15551230001" }],
    ["a revision that is not decimal", { ...fixtures.liveNamed, memberRevision: "4.0" }],
    ["a numeric revision", { ...fixtures.liveNamed, memberRevision: 4 }],
    ["a missing canCreate", { ...fixtures.mms, canCreate: undefined }],
    ["a non-string name", { ...fixtures.liveNamed, name: 7 }],
    ["null", null],
  ])("rejects %s", (_, value) => {
    expect(parseGroupView(value)).toBeUndefined();
  });
});

describe("normalizeGroupSnapshot", () => {
  it("returns the snapshot unchanged when no conversation carries a group", () => {
    const input = snapshot([conversation()]);
    expect(normalizeGroupSnapshot(input)).toBe(input);
  });

  it("maps every valid group and keeps the conversation when its group is malformed", () => {
    const result = normalizeGroupSnapshot(snapshot([
      conversation(fixtures.liveNamed),
      { ...conversation(), id: "conv-bad", group: { state: "live" } } as ConversationView,
      { ...conversation(), id: "conv-plain" },
    ]));
    expect(result.conversations.map((item) => item.id)).toEqual(["conv-group", "conv-bad", "conv-plain"]);
    expect(result.conversations[0].group).toEqual(fixtures.liveNamed);
    expect(result.conversations[1]).not.toHaveProperty("group");
    expect(result.conversations[2]).not.toHaveProperty("group");
  });
});

describe("parseSendResult", () => {
  it("keeps the members-changed refusal with the list the user must review", () => {
    expect(parseSendResult({ accepted: false, status: "failed-before-submit", reason: "members_changed", group: { members: ["+15551230001", "+15551230003"], memberRevision: "6" } })).toEqual({
      accepted: false,
      status: "failed-before-submit",
      reason: "members_changed",
      group: { members: ["+15551230001", "+15551230003"], memberRevision: "6" },
    });
  });

  it.each(["update_required", "withdrawn", "create_unconfirmed", "create_not_rcs"] as const)("keeps the %s reason", (reason) => {
    expect(parseSendResult({ accepted: false, status: "failed-before-submit", reason })).toMatchObject({ reason });
  });

  it("drops an unknown reason and a malformed group rather than passing them to the UI", () => {
    const result = parseSendResult({ accepted: false, status: "failed-before-submit", reason: "something_new", group: { members: ["bad"], memberRevision: "1" } });
    expect(result).not.toHaveProperty("reason");
    expect(result).not.toHaveProperty("group");
  });

  it("throws on a malformed envelope so the composer never guesses the outcome", () => {
    expect(() => parseSendResult({ accepted: "no", status: "sent" })).toThrow("Invalid send result.");
    expect(() => parseSendResult({ accepted: true, status: "bogus" })).toThrow("Invalid send result.");
  });

  it("keeps the revision and parts that the existing send result carries", () => {
    expect(parseSendResult({ accepted: true, status: "sent", revision: "3", parts: 2 })).toEqual({ accepted: true, status: "sent", revision: "3", parts: 2 });
  });
});

describe("parseTypingSignal", () => {
  it("accepts a group signal with its canonical participant", () => {
    expect(parseTypingSignal({ conversationId: "conv-group", state: "started", expiresAtMs: "1730000005000", participant: "+15551230001" })).toEqual({
      conversationId: "conv-group",
      state: "started",
      expiresAtMs: "1730000005000",
      participant: "+15551230001",
    });
  });

  it("accepts a one-to-one signal without a participant", () => {
    expect(parseTypingSignal({ conversationId: "conv-1", state: "stopped" })).toEqual({ conversationId: "conv-1", state: "stopped" });
  });

  it.each([
    ["a non-canonical participant", { conversationId: "c", state: "started", participant: "Alex" }],
    ["a numeric expiry", { conversationId: "c", state: "started", expiresAtMs: 5 }],
    ["an unknown state", { conversationId: "c", state: "typing" }],
  ])("rejects %s", (_, value) => {
    expect(parseTypingSignal(value)).toBeUndefined();
  });
});

describe("groupDraftFields", () => {
  it("passes a name up to 100 characters and a decimal revision", () => {
    expect(groupDraftFields({ groupName: "a".repeat(100), expectedMemberRevision: "12" })).toEqual({ groupName: "a".repeat(100), expectedMemberRevision: "12" });
  });

  it("counts characters, not UTF-16 units, when limiting the name", () => {
    expect(groupDraftFields({ groupName: "🌊".repeat(100) })).toEqual({ groupName: "🌊".repeat(100) });
    expect(() => groupDraftFields({ groupName: "🌊".repeat(101) })).toThrow("Invalid group name.");
  });

  it("omits fields that are absent", () => {
    expect(groupDraftFields({})).toEqual({});
  });

  it("passes a duplicate-create confirmation only as a boolean", () => {
    expect(groupDraftFields({ groupCreateConfirmDuplicate: true })).toEqual({ groupCreateConfirmDuplicate: true });
    expect(() => groupDraftFields({ groupCreateConfirmDuplicate: "yes" as unknown as boolean })).toThrow("Invalid group create confirmation.");
  });

  it("rejects a non-decimal revision", () => {
    expect(() => groupDraftFields({ expectedMemberRevision: "-1" })).toThrow("Invalid member revision.");
  });
});

describe("fixture coverage", () => {
  it("covers every group state the contract names", () => {
    const states = new Set(Object.values(fixtures).map((fixture) => (fixture as GroupView).state));
    expect([...states].sort()).toEqual(["live", "paused", "proving", "unlinked", "withdrawn"]);
  });
});
