import { describe, expect, it } from "vitest";
import type { ContactBookView, ContactResolutionMap, ConversationView } from "./bridge";
import { canCreateContactInBook, conversationContactAddress } from "./conversation-contact";

const writableBook: ContactBookView = {
  id: "book-1",
  deviceName: "Phone",
  state: "active",
  capabilities: { canWrite: true, canDelete: true, supportsNotes: true, supportsPhoto: true },
  contactCount: 0,
  pendingEditCount: 0,
};

const conversation = (overrides: Partial<ConversationView> = {}): ConversationView => ({
  id: "conversation-1",
  name: "+18015551234",
  preview: "Hello",
  unread: 0,
  messages: [],
  participants: ["+18015551234"],
  ...overrides,
});

describe("canCreateContactInBook", () => {
  it("accepts writable active and limited books", () => {
    expect(canCreateContactInBook(writableBook)).toBe(true);
    expect(canCreateContactInBook({ ...writableBook, state: "limited" })).toBe(true);
  });

  it("rejects unavailable, revoked, and read-only books", () => {
    expect(canCreateContactInBook({ ...writableBook, state: "unavailable" })).toBe(false);
    expect(canCreateContactInBook({ ...writableBook, sourceRevoked: true })).toBe(false);
    expect(canCreateContactInBook({ ...writableBook, capabilities: { ...writableBook.capabilities, canWrite: false } })).toBe(false);
  });
});

describe("conversationContactAddress", () => {
  const resolution: ContactResolutionMap = {};

  it("returns the raw address for an unsaved participant when a usable book exists", () => {
    expect(conversationContactAddress({ conversation: conversation(), resolution, books: [writableBook] })).toBe("+18015551234");
  });

  it("rejects saved contacts, groups, drafts, invalid numbers, and unusable books", () => {
    expect(conversationContactAddress({
      conversation: conversation(),
      resolution: { "+18015551234": { bookId: "book-1", contactId: "contact-1", displayName: "Ada" } },
      books: [writableBook],
    })).toBeUndefined();
    expect(conversationContactAddress({ conversation: conversation({ participants: ["+18015551234", "+18015551235"] }), resolution, books: [writableBook] })).toBeUndefined();
    expect(conversationContactAddress({ conversation: conversation({ draftOnly: true }), resolution, books: [writableBook] })).toBeUndefined();
    expect(conversationContactAddress({ conversation: conversation({ participants: ["911"] }), resolution, books: [writableBook] })).toBeUndefined();
    expect(conversationContactAddress({ conversation: conversation(), resolution, books: [{ ...writableBook, state: "retired" }] })).toBeUndefined();
  });

  it("allows missing conversation or books", () => {
    expect(conversationContactAddress({ conversation: undefined, resolution, books: [writableBook] })).toBeUndefined();
    expect(conversationContactAddress({ conversation: conversation(), resolution, books: undefined })).toBeUndefined();
  });
});
