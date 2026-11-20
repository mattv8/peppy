import type { ContactSort, ContactView } from "../bridge/bridge";

export const MAX_TARGET_LOOKUP_PAGES = 500;
export const CONTACTS_PAGE_SIZE = 200;

export type ContactTargetLookup = {
  bookId: string;
  contactId: string;
};

/** `contacts` holds every page fetched up to and including the target's page, in list order. */
export type ContactTargetResult =
  | { state: "found"; contact: ContactView; contacts: ContactView[] }
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
  const fetched: ContactView[] = [];
  for (let page = 0; page < MAX_TARGET_LOOKUP_PAGES; page += 1) {
    if (signal.aborted || !isBookAvailable(target.bookId)) return { state: "cancelled" };
    const offset = page * CONTACTS_PAGE_SIZE;
    const contacts = await listContacts(target.bookId, "", offset, sort);
    if (signal.aborted || !isBookAvailable(target.bookId)) return { state: "cancelled" };
    fetched.push(...contacts);
    const contact = contacts.find((candidate) => candidate.id === target.contactId);
    if (contact) return { state: "found", contact, contacts: fetched };
    if (contacts.length < CONTACTS_PAGE_SIZE) return { state: "unavailable" };
  }

  return signal.aborted ? { state: "cancelled" } : { state: "oversized" };
}
