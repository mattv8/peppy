import { useCallback, useRef, useState, type CSSProperties, type RefObject } from "react";
import { Ellipsis, PhoneOff, Search, Smartphone } from "@peppy/desktop-ui";
import type { ContactBookView } from "../bridge/bridge";
import type { PostContactActionNotice } from "./contact-action-notice";
import { bookContactCountText, bookLabels, bookStateText, bookSummaryText } from "./contact-labels";
import { BookRowMenu, pointerMenuPoint, underButtonPoint, type BookMenuEntry, type BookMenuPoint } from "./BookRowMenu";
import { bookUseSwitchId, useBookLookup, type BookLookup, type LookupFailure } from "./useBookLookup";
import "./book-rail.css";

const BOOK_USE_HINT = "This computer only. Contacts stay here to browse.";
const BOOK_SKELETON_ROWS = 3;

/** The contact-book column shown above ≈700px. Each row carries its own lookup switch when the switch applies. */
export function BookRail({ books, loading, bookId, width, panelRef, onSelect, onBooksChanged, onRemoveRequest, onDeleteRequest, deleteDisabledReason, onNotice, lookupFailure }: {
  books: ContactBookView[];
  /** The book list has not loaded yet: placeholder rows stand in for it. */
  loading: boolean;
  bookId: string;
  width: number;
  panelRef: RefObject<HTMLElement | null>;
  onSelect(bookId: string): void;
  onBooksChanged(): void;
  onRemoveRequest(book: ContactBookView, opener: RefObject<HTMLElement | null>): void;
  onDeleteRequest(book: ContactBookView, opener: RefObject<HTMLElement | null>): void;
  deleteDisabledReason?: string;
  onNotice: PostContactActionNotice;
  lookupFailure?: LookupFailure;
}) {
  const lookup = useBookLookup({ books, bookId, onBooksChanged, onNotice, failure: lookupFailure });
  const { suffix } = bookLabels(books);
  const railStyle = { "--contacts-book-width": `${width}px` } as CSSProperties;
  return (
    <aside ref={panelRef} id="contacts-book-list" aria-label="Contact books" style={railStyle}>
      <ul id="contacts-books" aria-busy={loading}>
        {loading && Array.from({ length: BOOK_SKELETON_ROWS }, (_, index) => (
          <li key={index} data-book-skeleton aria-hidden="true">
            <span className="contacts-skeleton-line contacts-skeleton-line-long" />
            <span className="contacts-skeleton-line contacts-skeleton-line-short" />
          </li>
        ))}
        {books.map(item => (
          <BookRailItem
            key={item.id}
            item={item}
            selected={item.id === bookId}
            suffix={suffix(item)}
            showSwitch={books.length >= 2 || Boolean(item.lookupExcluded)}
            lookup={lookup}
            onSelect={onSelect}
            onRemoveRequest={onRemoveRequest}
            onDeleteRequest={onDeleteRequest}
            deleteDisabledReason={deleteDisabledReason}
            railRef={panelRef}
            onNotice={onNotice}
          />
        ))}
      </ul>
    </aside>
  );
}

function BookRailItem({ item, selected, suffix, showSwitch, lookup, onSelect, onRemoveRequest, onDeleteRequest, deleteDisabledReason, railRef, onNotice }: {
  item: ContactBookView;
  selected: boolean;
  suffix: string;
  showSwitch: boolean;
  lookup: BookLookup;
  onSelect(bookId: string): void;
  onRemoveRequest(book: ContactBookView, opener: RefObject<HTMLElement | null>): void;
  onDeleteRequest(book: ContactBookView, opener: RefObject<HTMLElement | null>): void;
  deleteDisabledReason?: string;
  railRef: RefObject<HTMLElement | null>;
  onNotice: PostContactActionNotice;
}) {
  const hintId = `contacts-book-use-hint-${item.id}`;
  const use = item.lookupExcluded ? "excluded" : "included";
  const moreButton = useRef<HTMLButtonElement>(null);
  const [menuPoint, setMenuPoint] = useState<BookMenuPoint | null>(null);
  const closeMenu = useCallback(() => setMenuPoint(null), []);
  const railLeft = () => railRef.current?.getBoundingClientRect().left ?? 0;
  const menuItems: BookMenuEntry[] = [
    { action: "remove-book-local", label: "Remove from this computer", onSelect: () => onRemoveRequest(item, moreButton) },
    "separator",
    { action: "delete-book-everywhere", label: "Delete for all devices…", destructive: true, disabledReason: deleteDisabledReason, onSelect: () => onDeleteRequest(item, moreButton) },
  ];
  return (
    <li
      data-book-row=""
      data-book-id={item.id}
      data-book-use={use}
      data-selected={selected || undefined}
      onKeyDown={event => {
        if (lookup.confirmBookId === item.id && event.key === "Escape") { event.preventDefault(); lookup.cancelConfirm(); }
      }}
      onContextMenu={item.sourceRevoked ? event => {
        event.preventDefault();
        setMenuPoint(pointerMenuPoint(event.clientX, event.clientY, railLeft()));
      } : undefined}
    >
      <button
        data-book-id={item.id}
        data-book-use={use}
        className="contacts-book-row"
        aria-current={selected ? "location" : undefined}
        onClick={() => onSelect(item.id)}
      >
        {item.sourceRevoked
          ? <PhoneOff className="contacts-book-removed-icon" aria-hidden="true" size={16} strokeWidth={1.75} />
          : <Smartphone aria-hidden="true" size={16} strokeWidth={1.75} />}
        <span className="contacts-book-text">
          <b>{item.deviceName}</b>
          {suffix && <span className="contacts-book-id">{suffix}</span>}
          {item.lookupExcluded && <span className="contacts-book-hidden">Hidden from names</span>}
          {item.sourceRevoked ? <>
            <small className="contacts-book-removed-text" data-book-state={item.state}>{bookStateText(item)}</small>
            <small>{bookContactCountText(item.contactCount)}</small>
          </> : <small data-book-state={item.state}>{bookSummaryText(item)}</small>}
          {item.defaultAccountLabel && <small className="contacts-book-account">{item.defaultAccountLabel}</small>}
        </span>
      </button>
      {(showSwitch || item.sourceRevoked) && (
        <div className="contacts-book-use">
          {showSwitch && <>
            <Search size={14} aria-hidden />
            <span className="settings-switch">
              <input
                id={bookUseSwitchId(item.id)}
                data-action="toggle-book-use"
                type="checkbox"
                role="switch"
                aria-label={`Use ${item.deviceName} for names and recipient search`}
                aria-describedby={hintId}
                title={BOOK_USE_HINT}
                checked={!item.lookupExcluded}
                disabled={lookup.busy}
                onChange={event => lookup.toggle(item, event.target.checked)}
              />
              <span aria-hidden="true" />
            </span>
            <span id={hintId} className="visually-hidden">{BOOK_USE_HINT}</span>
          </>}
          {item.sourceRevoked && (
            <button
              ref={moreButton}
              type="button"
              className="contacts-book-more"
              data-action="book-menu"
              aria-label={`More actions for ${item.deviceName}`}
              aria-haspopup="menu"
              aria-expanded={menuPoint !== null}
              title="More actions"
              onClick={event => {
                const point = underButtonPoint(event.currentTarget.getBoundingClientRect(), railLeft());
                setMenuPoint(current => current ? null : point);
              }}
            >
              <Ellipsis aria-hidden size={16} strokeWidth={1.75} />
            </button>
          )}
        </div>
      )}
      {menuPoint && (
        <BookRowMenu
          label={`Actions for ${item.deviceName}`}
          items={menuItems}
          point={menuPoint}
          anchorRef={moreButton}
          onClose={closeMenu}
        />
      )}
      {lookup.confirmBookId === item.id && (
        <BookUseConfirm busy={lookup.busy} onTurnOff={() => void lookup.saveLookup(item, false)} onCancel={lookup.cancelConfirm} />
      )}
    </li>
  );
}

/** Full-width lookup strip used when the rail is hidden. */
export function BookLookupStrip({ book, books, onBooksChanged, onNotice, lookupFailure }: {
  book: ContactBookView;
  books: ContactBookView[];
  onBooksChanged(): void;
  onNotice: PostContactActionNotice;
  lookupFailure?: LookupFailure;
}) {
  const lookup = useBookLookup({ books, bookId: book.id, onBooksChanged, onNotice, failure: lookupFailure });
  const { suffix } = bookLabels(books);
  const switchId = bookUseSwitchId(book.id);
  return (
    <section
      id="contacts-book-use"
      aria-label="Contact book use"
      aria-busy={lookup.busy}
      onKeyDown={event => {
        if (lookup.confirmBookId && event.key === "Escape") { event.preventDefault(); lookup.cancelConfirm(); }
      }}
    >
      {books.length < 2 && <div id="contacts-book-use-title"><span>{book.deviceName}</span>{suffix(book) && <span className="contacts-book-id">{suffix(book)}</span>}</div>}
      <label className="contacts-book-use-row" htmlFor={switchId}>
        <span>Use for names and recipient search</span>
        <span className="settings-switch">
          <input
            id={switchId}
            data-action="toggle-book-use"
            type="checkbox"
            role="switch"
            aria-describedby="contacts-book-use-hint"
            checked={!book.lookupExcluded}
            disabled={lookup.busy}
            onChange={event => lookup.toggle(book, event.target.checked)}
          />
          <span aria-hidden="true" />
        </span>
      </label>
      <p id="contacts-book-use-hint">{BOOK_USE_HINT}</p>
      {lookup.confirmBookId === book.id && (
        <BookUseConfirm busy={lookup.busy} onTurnOff={() => void lookup.saveLookup(book, false)} onCancel={lookup.cancelConfirm} />
      )}
    </section>
  );
}

function BookUseConfirm({ busy, onTurnOff, onCancel }: {
  busy: boolean;
  onTurnOff(): void;
  onCancel(): void;
}) {
  return (
    <div id="contacts-book-use-confirm" role="group" aria-label="Confirm turning off the last contact book">
      <p>No contact book will be used for names or recipient search. Conversations will show phone numbers.</p>
      <button type="button" disabled={busy} onClick={onTurnOff}>Turn off</button>
      <button id="contacts-book-use-cancel" type="button" disabled={busy} onClick={onCancel}>Cancel</button>
    </div>
  );
}
