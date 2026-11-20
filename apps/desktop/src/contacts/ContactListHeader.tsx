import { useEffect, useState, type ReactNode } from "react";
import type { ContactBookView } from "../bridge/bridge";
import { bookLabels, bookStateText } from "./contact-labels";

const countFormat = new Intl.NumberFormat();
/** Same delay as the sync progress bar (SHOW_DELAY_MS in @peppy/desktop-ui): quick loads never flash a bar. */
const COUNT_PROGRESS_DELAY_MS = 500;

/** True once `active` has stayed on for `delayMs`; turns off as soon as `active` does. */
function useDelayedFlag(active: boolean, delayMs: number): boolean {
  const [elapsed, setElapsed] = useState(false);
  useEffect(() => {
    if (!active) return;
    const timer = window.setTimeout(() => setElapsed(true), delayMs);
    return () => {
      window.clearTimeout(timer);
      setElapsed(false);
    };
  }, [active, delayMs]);
  return active && elapsed;
}

/** Count text for the selected book; the count is only known after the book list has loaded. */
function contactCountText(booksLoaded: boolean, book: ContactBookView | undefined): string {
  if (!booksLoaded) return "Loading contacts…";
  const count = book?.contactCount ?? 0;
  if (count === 0) return "No contacts";
  if (count === 1) return "1 contact";
  return `${countFormat.format(count)} contacts`;
}

/** Count text while a search is active; `matchCount` is undefined until its first page lands. */
function searchCountText(query: string, matchCount: number | undefined): string {
  if (matchCount === undefined) return "Searching contacts…";
  if (matchCount === 1) return `1 contact matches “${query}”`;
  return `${countFormat.format(matchCount)} contacts match “${query}”`;
}

/**
 * Header of the contact list column: the selected book (a select when the rail is hidden), its contact count,
 * and the list actions passed as children. A search replaces the count with its match count.
 */
export function ContactListHeader({ books, bookId, booksLoaded, railVisible, loaded, loadingMore, query, matchCount, onSelectBook, children }: {
  books: ContactBookView[];
  bookId: string;
  booksLoaded: boolean;
  railVisible: boolean;
  /** Rows loaded so far in the list. */
  loaded: number;
  loadingMore: boolean;
  /** The active search text; empty when the list is unfiltered. */
  query: string;
  /** Contacts the active search matches. */
  matchCount?: number;
  onSelectBook(bookId: string): void;
  children: ReactNode;
}) {
  const book = books.find(item => item.id === bookId);
  const { label } = bookLabels(books);
  const showSelector = !railVisible && books.length >= 2;
  const total = book?.contactCount;
  const delayedLoadingMore = useDelayedFlag(loadingMore, COUNT_PROGRESS_DELAY_MS);
  const progressTotal = !query && delayedLoadingMore && total !== undefined && loaded > 0 && loaded <= total ? total : undefined;
  return (
    <header id="contacts-list-header">
      <div id="contacts-list-heading">
        {showSelector ? (
          <select id="contacts-book-selector" aria-label="Contact book" value={bookId} onChange={event => onSelectBook(event.target.value)}>
            {books.map(item => (
              <option key={item.id} value={item.id}>{`${label(item)}, ${bookStateText(item)}${item.lookupExcluded ? ", hidden from names" : ""}`}</option>
            ))}
          </select>
        ) : (
          <h2 id="contacts-list-title" tabIndex={-1}>{book ? label(book) : "Contact book"}</h2>
        )}
        <span id="contacts-count" data-count-state={query ? "search" : progressTotal !== undefined ? "progress" : booksLoaded ? "known" : "loading"}>
          {query ? searchCountText(query, matchCount) : progressTotal !== undefined ? (
            <>
              {`${countFormat.format(loaded)} of ${countFormat.format(progressTotal)} contacts`}
              <progress id="contacts-count-bar" aria-label="Loading contacts" value={loaded} max={progressTotal} />
            </>
          ) : contactCountText(booksLoaded, book)}
        </span>
      </div>
      {children}
    </header>
  );
}
