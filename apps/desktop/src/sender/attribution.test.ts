import { describe, expect, it } from "vitest";
import { senderLabel, senderRunEdges, type SenderRunIdentity } from "./attribution";

type Item = {
  id: string;
  identity: SenderRunIdentity;
  endsRunAfter?: boolean;
};

const runEdges = (items: Item[]) =>
  senderRunEdges(items, {
    key: (item) => item.id,
    identity: (item) => item.identity,
    endsRunAfter: (item) => item.endsRunAfter ?? false,
  });

describe("senderLabel", () => {
  it("uses a resolved contact name and target", () => {
    expect(senderLabel("+18015551234", {
      "+18015551234": { bookId: "book-1", contactId: "contact-1", displayName: "Ada Lovelace" },
    })).toEqual({
      text: "Ada Lovelace",
      isName: true,
      contact: { bookId: "book-1", contactId: "contact-1" },
    });
  });

  it("uses a placeholder contact name without a target", () => {
    expect(senderLabel("+18015551234", {
      "+18015551234": { bookId: "", contactId: "", displayName: "Chosen recipient" },
    })).toEqual({ text: "Chosen recipient", isName: true });
  });

  it("formats an unresolved address", () => {
    expect(senderLabel("+18015551234", {})).toEqual({ text: "(801) 555-1234", isName: false });
  });

  it("uses an unknown label for missing addresses", () => {
    expect(senderLabel(undefined, {})).toEqual({ text: "Unknown sender", isName: false });
    expect(senderLabel("", {})).toEqual({ text: "Unknown sender", isName: false });
  });
});

describe("senderRunEdges", () => {
  it("marks the edges of a single sender run", () => {
    expect(runEdges([
      { id: "one", identity: { kind: "incoming", address: "+1" } },
      { id: "two", identity: { kind: "incoming", address: "+1" } },
      { id: "three", identity: { kind: "incoming", address: "+1" } },
    ])).toEqual(new Map([
      ["one", { start: true, end: false }],
      ["two", { start: false, end: false }],
      ["three", { start: false, end: true }],
    ]));
  });

  it("splits runs when the sender changes or an outgoing item appears", () => {
    expect(runEdges([
      { id: "one", identity: { kind: "incoming", address: "+1" } },
      { id: "two", identity: { kind: "incoming", address: "+2" } },
      { id: "three", identity: { kind: "outgoing" } },
      { id: "four", identity: { kind: "incoming", address: "+2" } },
    ])).toEqual(new Map([
      ["one", { start: true, end: true }],
      ["two", { start: true, end: true }],
      ["four", { start: true, end: true }],
    ]));
  });

  it("breaks runs at boundary notices", () => {
    expect(runEdges([
      { id: "one", identity: { kind: "incoming", address: "+1" } },
      { id: "notice", identity: { kind: "boundary" } },
      { id: "two", identity: { kind: "incoming", address: "+1" } },
    ])).toEqual(new Map([
      ["one", { start: true, end: true }],
      ["two", { start: true, end: true }],
    ]));
  });

  it("makes incoming items without an address their own runs", () => {
    expect(runEdges([
      { id: "one", identity: { kind: "incoming" } },
      { id: "two", identity: { kind: "incoming" } },
    ])).toEqual(new Map([
      ["one", { start: true, end: true }],
      ["two", { start: true, end: true }],
    ]));
  });

  it("splits a run after items that visibly show timestamps", () => {
    expect(runEdges([
      { id: "one", identity: { kind: "incoming", address: "+1" }, endsRunAfter: true },
      { id: "two", identity: { kind: "incoming", address: "+1" } },
    ])).toEqual(new Map([
      ["one", { start: true, end: true }],
      ["two", { start: true, end: true }],
    ]));
  });

  it("returns no edges for an empty list", () => {
    expect(runEdges([])).toEqual(new Map());
  });
});
