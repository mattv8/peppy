import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { bridge, type ContactBookView } from "../bridge/bridge";
import { IsolatedText } from "../notices/IsolatedText";
import type { PostContactActionNotice } from "./contact-action-notice";
import { bookLabels } from "./contact-labels";

export const bookUseSwitchId = (bookId: string) => `contacts-book-use-switch-${bookId}`;

export type BookLookup = ReturnType<typeof useBookLookup>;

/** Toast text for a failed lookup save; `retryable` false leaves Retry off, since the same save would fail again. */
export type LookupFailure = (error: unknown) => { message: string; retryable: boolean };

export const defaultLookupFailure: LookupFailure = () => ({ message: "Couldn't save this change. Try again.", retryable: true });

/**
 * Lookup switch state for the contact books: one save at a time, the inline confirmation before turning off
 * the last included book, and focus restoration to the switch. Results are reported through `onNotice`.
 */
export function useBookLookup({ books, bookId, onBooksChanged, onNotice, failure = defaultLookupFailure }: {
  books: ContactBookView[];
  bookId: string;
  onBooksChanged(): void;
  onNotice: PostContactActionNotice;
  failure?: LookupFailure;
}) {
  const [busy, setBusy] = useState(false);
  const [confirmBookId, setConfirmBookId] = useState<string | null>(null);
  const focusSwitchAfterSave = useRef<string | null>(null);
  const mountedRef = useRef(true);
  const { label } = bookLabels(books);

  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);
  useEffect(() => {
    setConfirmBookId(null);
  }, [bookId]);
  useEffect(() => {
    if (confirmBookId) document.getElementById("contacts-book-use-cancel")?.focus();
  }, [confirmBookId]);
  useLayoutEffect(() => {
    const id = focusSwitchAfterSave.current;
    if (busy || !id) return;
    focusSwitchAfterSave.current = null;
    document.getElementById(bookUseSwitchId(id))?.focus();
  });

  const saveLookup = async (item: ContactBookView, enabled: boolean): Promise<void> => {
    setBusy(true);
    try {
      await bridge.set_contact_book_lookup(item.id, enabled);
      if (!mountedRef.current) return;
      if (!enabled && confirmBookId === item.id) focusSwitchAfterSave.current = item.id;
      setConfirmBookId(null);
      onBooksChanged();
      const name = <IsolatedText text={label(item)} />;
      onNotice({
        kind: "success",
        message: enabled
          ? <>{name} is used again for names and recipient search.</>
          : <>Names and recipient search no longer use {name}. Its contacts are unchanged.</>,
      });
    } catch (error) {
      if (mountedRef.current) {
        const { message, retryable } = failure(error);
        onNotice({ kind: "error", message, retry: retryable ? () => void saveLookup(item, enabled) : undefined });
      }
    } finally {
      if (mountedRef.current) setBusy(false);
    }
  };

  const toggle = (item: ContactBookView, enabled: boolean) => {
    const includedCount = books.filter(candidate => !candidate.lookupExcluded).length;
    if (!enabled && includedCount === 1) setConfirmBookId(item.id);
    else void saveLookup(item, enabled);
  };

  const cancelConfirm = () => {
    const id = confirmBookId;
    setConfirmBookId(null);
    if (id) document.getElementById(bookUseSwitchId(id))?.focus();
  };

  return { busy, confirmBookId, toggle, saveLookup, cancelConfirm };
}
