import { describe, expect, it, vi } from "vitest";
import { searchRecipientSuggestions, type RecipientSource } from "./recipientSearch";
import { bridge } from "../bridge/bridge";

vi.mock("../bridge/bridge", { spy: true });

const ada: RecipientSource = {
  address: "+12025550123",
  bookId: "book-1",
  displayName: "Ada Lovelace",
  number: "(202) 555-0123",
  label: "mobile",
  normalized: true,
  contactId: "contact-ada",
  phoneId: "phone-ada",
};

async function search(rows: RecipientSource[]) {
  vi.mocked(bridge.search_contact_recipients).mockResolvedValueOnce(rows);
  return searchRecipientSuggestions("ada");
}

describe("searchRecipientSuggestions", () => {
  it("merges identical rows from two books and drops the source", async () => {
    const suggestions = await search([ada, { ...ada, bookId: "book-2", contactId: "contact-ada-2", phoneId: "phone-ada-2" }]);

    expect(suggestions).toHaveLength(1);
    const [suggestion] = suggestions;
    expect(suggestion).toMatchObject({ id: "+12025550123", label: "Ada Lovelace", source: null });
    expect(suggestion.rowId).toBe(JSON.stringify(["+12025550123", "Ada Lovelace", "mobile"]));
  });

  it("keeps the source when the same contact appears twice", async () => {
    const [suggestion] = await search([ada, { ...ada, phoneId: "phone-ada-second" }]);

    expect(suggestion.source).toEqual(ada);
    expect(suggestion.rowId).toBe(JSON.stringify(["+12025550123", "Ada Lovelace", "mobile"]));
  });

  it("keeps a single row's own row id and source", async () => {
    const [suggestion] = await search([ada]);

    expect(suggestion.source).toEqual(ada);
    expect(suggestion.rowId).toBe(JSON.stringify(["book-1", "contact-ada", "phone-ada"]));
  });

  it("keeps rows with different labels separate", async () => {
    const work = { ...ada, label: "work", phoneId: "phone-ada-work" };
    const suggestions = await search([ada, { ...ada, bookId: "book-2", contactId: "contact-ada-2" }, work]);

    expect(suggestions).toHaveLength(2);
    expect(suggestions.map((suggestion) => suggestion.detail)).toEqual([
      "mobile · (202) 555-0123",
      "work · (202) 555-0123",
    ]);
  });

  it("keeps rows with different names separate", async () => {
    const suggestions = await search([ada, { ...ada, bookId: "book-2", contactId: "contact-ada-2", displayName: "Ada L." }]);

    expect(suggestions.map((suggestion) => suggestion.label)).toEqual(["Ada Lovelace", "Ada L."]);
    expect(suggestions.map((suggestion) => suggestion.source?.contactId)).toEqual(["contact-ada", "contact-ada-2"]);
  });

  it("preserves first-seen order across merged and single rows", async () => {
    const grace = { ...ada, address: "+12025550199", displayName: "Grace Hopper", number: "(202) 555-0199", contactId: "contact-grace", phoneId: "phone-grace" };
    const suggestions = await search([
      ada,
      grace,
      { ...ada, bookId: "book-2", contactId: "contact-ada-2", phoneId: "phone-ada-2" },
    ]);

    expect(suggestions.map((suggestion) => suggestion.id)).toEqual(["+12025550123", "+12025550199"]);
    expect(suggestions.map((suggestion) => suggestion.source === null)).toEqual([true, false]);
  });
});
