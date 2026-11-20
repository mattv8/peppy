import type { DesktopBridge, ContactSort, ContactView } from "../bridge/bridge";

export const CONTACTS_PAGE_SIZE = 200;
const MAX_TARGET_ROWS = 2_000;

export type ContactTargetLookup = {
  bookId: string;
  contactId: string;
};

/** `contacts` holds the ordered rows through the target. */
export type ContactTargetResult =
  | { state: "found"; contact: ContactView; contacts: ContactView[]; total: number; nextCursor?: string }
  | { state: "unavailable" }
  | { state: "oversized" }
  | { state: "cancelled" };

export async function findContactTarget(
  target: ContactTargetLookup,
  listContacts: DesktopBridge["list_contacts_page"],
  signal: AbortSignal,
  isBookAvailable: (bookId: string) => boolean = () => true,
  sort: ContactSort = { key: "family", direction: "asc" },
): Promise<ContactTargetResult> {
  if (signal.aborted || !isBookAvailable(target.bookId)) return { state: "cancelled" };
  const page = await listContacts(target.bookId, { through: target.contactId, sort });
  if (signal.aborted || !isBookAvailable(target.bookId)) return { state: "cancelled" };
  const contact = page.contacts.find(candidate => candidate.id === target.contactId);
  if (page.throughFound && contact) return { state: "found", contact, contacts: page.contacts, total: page.total, nextCursor: page.nextCursor };
  // Core returns only the first page when the target lies beyond the cap, so the target may still exist past it.
  const mayLieBeyondCap = !page.throughFound && page.total > MAX_TARGET_ROWS;
  return { state: mayLieBeyondCap ? "oversized" : "unavailable" };
}
