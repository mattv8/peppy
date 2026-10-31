// @vitest-environment node
import { describe, expect, it, vi } from "vitest";
import type { ContactView } from "./bridge";
import { CONTACTS_PAGE_SIZE, MAX_TARGET_LOOKUP_PAGES, findContactTarget } from "./contact-target";

const contact = { id: "target", bookId: "book", revision: "1", displayName: "Target", phones: [], emails: [], addresses: [] } satisfies ContactView;

describe("findContactTarget", () => {
  it("finds the exact contact in a later page of its requested book", async () => {
    const firstPage = Array.from({ length: CONTACTS_PAGE_SIZE }, (_, index) => ({ ...contact, id: `contact-${index}` }));
    const listContacts = vi.fn().mockResolvedValueOnce(firstPage).mockResolvedValueOnce([contact]);

    await expect(findContactTarget({ bookId: "book", contactId: "target" }, listContacts, new AbortController().signal)).resolves.toEqual({
      state: "found",
      contact,
      offset: CONTACTS_PAGE_SIZE,
      page: [contact],
    });
    expect(listContacts).toHaveBeenNthCalledWith(2, "book", "", CONTACTS_PAGE_SIZE, { key: "family", direction: "asc" });
  });

  it("does not apply a contact page after cancellation", async () => {
    const controller = new AbortController();
    const listContacts = vi.fn(async () => {
      controller.abort();
      return [contact];
    });

    await expect(findContactTarget({ bookId: "book", contactId: "target" }, listContacts, controller.signal)).resolves.toEqual({ state: "cancelled" });
  });

  it("reports an unavailable exact target after the final short page", async () => {
    const listContacts = vi.fn().mockResolvedValue([]);

    await expect(findContactTarget({ bookId: "book", contactId: "missing" }, listContacts, new AbortController().signal)).resolves.toEqual({ state: "unavailable" });
  });

  it("reports an oversized book after the bounded exact lookup", async () => {
    const fullPage = Array.from({ length: CONTACTS_PAGE_SIZE }, (_, index) => ({ ...contact, id: `contact-${index}` }));
    const listContacts = vi.fn().mockResolvedValue(fullPage);

    await expect(findContactTarget({ bookId: "book", contactId: "missing" }, listContacts, new AbortController().signal)).resolves.toEqual({ state: "oversized" });
    expect(listContacts).toHaveBeenCalledTimes(MAX_TARGET_LOOKUP_PAGES);
  });

  it("stops a paged lookup when its book disappears between pages", async () => {
    const firstPage = Array.from({ length: CONTACTS_PAGE_SIZE }, (_, index) => ({ ...contact, id: `contact-${index}` }));
    let available = true;
    const listContacts = vi.fn(async () => {
      available = false;
      return firstPage;
    });

    await expect(findContactTarget({ bookId: "book", contactId: "target" }, listContacts, new AbortController().signal, () => available)).resolves.toEqual({ state: "cancelled" });
    expect(listContacts).toHaveBeenCalledOnce();
  });

  it("keeps the displayed sort while walking pages", async () => {
    const listContacts = vi.fn().mockResolvedValue([contact]);
    const sort = { key: "given" as const, direction: "desc" as const };
    await findContactTarget({ bookId: "book", contactId: "target" }, listContacts, new AbortController().signal, undefined, sort);
    expect(listContacts).toHaveBeenCalledWith("book", "", 0, sort);
  });

  it("uses the same sort for every page of a multi-page target lookup", async () => {
    const firstPage = Array.from({ length: CONTACTS_PAGE_SIZE }, (_, index) => ({ ...contact, id: `contact-${index}` }));
    const sort = { key: "given" as const, direction: "desc" as const };
    const listContacts = vi.fn().mockResolvedValueOnce(firstPage).mockResolvedValueOnce([contact]);

    await findContactTarget({ bookId: "book", contactId: "target" }, listContacts, new AbortController().signal, undefined, sort);

    expect(listContacts).toHaveBeenNthCalledWith(1, "book", "", 0, sort);
    expect(listContacts).toHaveBeenNthCalledWith(2, "book", "", CONTACTS_PAGE_SIZE, sort);
  });
});
