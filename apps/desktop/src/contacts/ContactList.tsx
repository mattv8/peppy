import { useEffect, useRef, useState, type FocusEvent, type KeyboardEvent } from "react";
import { ContactAvatar, Loader2, contactSeed, formatPhoneNumber } from "@peppy/desktop-ui";
import type { ContactView } from "../bridge/bridge";
import { editStateLabel } from "./contact-labels";
import { formatContactName, type ContactNameFormat } from "./contact-name-format";

const SKELETON_ROWS = 10;
/** PageUp and PageDown move the roving focus this many rows. */
const PAGE_STEP = 10;
/** Starts loading the next page while the sentinel is still this far below the visible list. */
const LOAD_MORE_ROOT_MARGIN = "0px 0px 600px 0px";
const countFormat = new Intl.NumberFormat();

export type ContactListProps = {
  contacts: ContactView[];
  /** The first page of the current list, or the book list, is still loading. */
  loading: boolean;
  loadingMore: boolean;
  loadMoreError: string;
  hasMore: boolean;
  contactCount?: number;
  deviceName?: string;
  canWrite: boolean;
  /** The local projection is failed or waiting for repair; the empty list says so. */
  emptyFailed: boolean;
  nameFormat: ContactNameFormat;
  selectedId?: string;
  onSelect(contact: ContactView): void;
  onLoadMore(): void;
  onRetryLoadMore(): void;
};

function rowButtons(list: HTMLElement | null): HTMLButtonElement[] {
  return Array.from(list?.querySelectorAll<HTMLButtonElement>("button[data-contact-id]") ?? []);
}

function loadedAnnouncement(loaded: number, total: number): string {
  return `${countFormat.format(loaded)} of ${countFormat.format(Math.max(loaded, total))} contacts loaded.`;
}

function emptyCopy({ emptyFailed, deviceName, canWrite }: Pick<ContactListProps, "emptyFailed" | "deviceName" | "canWrite">): string {
  if (emptyFailed) return "Contacts couldn't be loaded yet.";
  if (!deviceName) return "No contacts in this book.";
  return canWrite ? `No contacts in ${deviceName}. Add one with New contact.` : `No contacts in ${deviceName}.`;
}

/** The one row that takes Tab: the focused row, else the selected row, else the first row. */
function rovingTabStop(contacts: ContactView[], focusedId: string | undefined, selectedId: string | undefined): string | undefined {
  const isListed = (id: string | undefined) => id !== undefined && contacts.some(contact => contact.id === id);
  if (isListed(focusedId)) return focusedId;
  if (isListed(selectedId)) return selectedId;
  return contacts[0]?.id;
}

/** Row index a navigation key moves to, or `undefined` when the key does not move focus. */
function nextRowIndex(key: string, index: number, last: number): number | undefined {
  switch (key) {
    case "ArrowDown": return Math.min(last, index + 1);
    case "ArrowUp": return Math.max(0, index - 1);
    case "Home": return 0;
    case "End": return last;
    case "PageDown": return Math.min(last, index + PAGE_STEP);
    case "PageUp": return Math.max(0, index - PAGE_STEP);
    default: return undefined;
  }
}

function ContactSkeleton() {
  return (
    <ul id="contacts-skeleton" aria-hidden="true">
      {Array.from({ length: SKELETON_ROWS }, (_, index) => (
        <li key={index} data-contact-skeleton>
          <span className="contacts-skeleton-avatar" />
          <span className="contacts-skeleton-copy">
            <span className="contacts-skeleton-line contacts-skeleton-line-long" />
            <span className="contacts-skeleton-line contacts-skeleton-line-short" />
          </span>
        </li>
      ))}
    </ul>
  );
}

/**
 * The contact rows of the list column: skeleton while the first page loads, infinite scroll through a sentinel,
 * a single tab stop with roving focus, and the polite announcer for loads the user triggers from the keyboard.
 */
export function ContactList({
  contacts, loading, loadingMore, loadMoreError, hasMore, contactCount, deviceName, canWrite, emptyFailed,
  nameFormat, selectedId, onSelect, onLoadMore, onRetryLoadMore,
}: ContactListProps) {
  const listRef = useRef<HTMLUListElement>(null);
  const sentinelRef = useRef<HTMLLIElement>(null);
  const pendingFocusIndex = useRef<number | undefined>(undefined);
  const wasLoading = useRef(loading);
  const [focusedId, setFocusedId] = useState<string>();
  const [announcement, setAnnouncement] = useState("");
  const loadedCount = contacts.length;
  const total = contactCount ?? 0;
  const tabStopId = rovingTabStop(contacts, focusedId, selectedId);

  useEffect(() => {
    if (loading) {
      pendingFocusIndex.current = undefined;
      setAnnouncement("Loading contacts.");
    } else if (wasLoading.current) {
      setAnnouncement(loadedAnnouncement(loadedCount, total));
    }
    wasLoading.current = loading;
  }, [loading, loadedCount, total]);

  // A load started from the keyboard moves focus to its first new row once that row exists.
  useEffect(() => {
    const index = pendingFocusIndex.current;
    if (index === undefined || loadedCount <= index) return;
    pendingFocusIndex.current = undefined;
    rowButtons(listRef.current)[index]?.focus();
    setAnnouncement(loadedAnnouncement(loadedCount, total));
  }, [loadedCount, total]);

  useEffect(() => {
    if (loadMoreError) pendingFocusIndex.current = undefined;
  }, [loadMoreError]);

  useEffect(() => {
    const sentinel = sentinelRef.current;
    if (!sentinel || typeof IntersectionObserver === "undefined" || !hasMore || loadingMore || loadMoreError) return;
    const observer = new IntersectionObserver(entries => {
      if (entries.some(entry => entry.isIntersecting)) onLoadMore();
    }, { root: listRef.current, rootMargin: LOAD_MORE_ROOT_MARGIN });
    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [hasMore, loadingMore, loadMoreError, onLoadMore]);

  const handleKeyDown = (event: KeyboardEvent<HTMLUListElement>) => {
    const rows = rowButtons(listRef.current);
    const index = rows.indexOf(event.target as HTMLButtonElement);
    if (index < 0) return;
    if (event.key === "ArrowDown" && index === rows.length - 1) {
      event.preventDefault();
      if (hasMore) {
        pendingFocusIndex.current = rows.length;
        onLoadMore();
      }
      return;
    }
    const next = nextRowIndex(event.key, index, rows.length - 1);
    if (next === undefined) return;
    event.preventDefault();
    rows[next].focus();
  };

  const handleFocus = (event: FocusEvent<HTMLUListElement>) => {
    const id = (event.target as HTMLElement).dataset.contactId;
    if (id) setFocusedId(id);
  };

  return (
    <>
      {loading && <ContactSkeleton />}
      <ul
        id="contacts-list"
        ref={listRef}
        aria-label={deviceName ? `Contacts in ${deviceName}` : "Contacts"}
        aria-busy={loading}
        onKeyDown={handleKeyDown}
        onFocus={handleFocus}
      >
        {contacts.map(contact => {
          const name = formatContactName(contact, nameFormat);
          return (
            <li key={contact.id}>
              <button
                data-contact-id={contact.id}
                className="contacts-row"
                tabIndex={contact.id === tabStopId ? 0 : -1}
                aria-label={[name, contact.phones[0]?.number && formatPhoneNumber(contact.phones[0].number), contact.pendingEditState && editStateLabel(contact.pendingEditState)].filter(Boolean).join(", ")}
                aria-current={selectedId === contact.id ? "location" : undefined}
                onClick={() => onSelect(contact)}
              >
                <ContactAvatar
                  seed={contactSeed(contact.phones, contact.id)}
                  name={contact.displayName}
                  photoDataUrl={contact.photoDataUrl}
                />
                <span className="contacts-row-copy">
                  <b>{name}</b>
                  <small>{contact.phones[0]?.number ? formatPhoneNumber(contact.phones[0].number) : ""}</small>
                </span>
                {contact.pendingEditState && (
                  <span className="contacts-row-state-badge" data-state={contact.pendingEditState}>
                    <span className="sr-only">{editStateLabel(contact.pendingEditState)}</span>
                  </span>
                )}
              </button>
            </li>
          );
        })}
        {!loading && contacts.length === 0 && (
          <li id="contacts-empty-state" data-empty-reason={emptyFailed ? "failed" : "none"}>
            {emptyCopy({ emptyFailed, deviceName, canWrite })}
          </li>
        )}
        {loadingMore && (
          <li id="contacts-list-more" data-more-state="loading">
            <Loader2 className="busy-indicator-icon" size={14} aria-hidden />
            <span>Loading more contacts…</span>
          </li>
        )}
        {loadMoreError && (
          <li id="contacts-list-more" data-more-state="error" role="alert">
            <span>Couldn't load more contacts.</span>
            <button type="button" data-action="retry-load-more" onClick={onRetryLoadMore}>Try again</button>
          </li>
        )}
        <li id="contacts-list-sentinel" ref={sentinelRef} aria-hidden="true" />
      </ul>
      <p id="contacts-announcer" className="visually-hidden" role="status" aria-live="polite">{announcement}</p>
    </>
  );
}
