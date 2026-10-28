import type { ContactSort, ContactView } from "./bridge";

export const MAX_TARGET_LOOKUP_PAGES = 500;
export const CONTACTS_PAGE_SIZE = 200;

export type ContactTargetLookup = {
  bookId: string;
  contactId: string;
};

export type ContactTargetResult =
  | { state: "found"; contact: ContactView; offset: number; page: ContactView[] }
  | { state: "unavailable" }
  | { state: "oversized" }
  | { state: "cancelled" };

export async function findContactTarget(
  target: ContactTargetLookup,
  listContacts: (bookId: string, query: string, offset: number, sort: ContactSort) => Promise<ContactView[]>,
  signal: AbortSignal,
  isBookAvailable: (bookId: string) => boolean = () => true,
  sort: ContactSort = { key: "family", direction: "asc" },
): Promise<ContactTargetResult> {
  for (let page = 0; page < MAX_TARGET_LOOKUP_PAGES; page += 1) {
    if (signal.aborted || !isBookAvailable(target.bookId)) return { state: "cancelled" };
    const offset = page * CONTACTS_PAGE_SIZE;
    const contacts = await listContacts(target.bookId, "", offset, sort);
    if (signal.aborted || !isBookAvailable(target.bookId)) return { state: "cancelled" };
    const contact = contacts.find((candidate) => candidate.id === target.contactId);
    if (contact) return { state: "found", contact, offset, page: contacts };
    if (contacts.length < CONTACTS_PAGE_SIZE) return { state: "unavailable" };
  }

  return signal.aborted ? { state: "cancelled" } : { state: "oversized" };
}
