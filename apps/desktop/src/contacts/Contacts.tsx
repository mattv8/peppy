import { useCallback, useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import { createPortal } from "react-dom";
import { Check, Clock, ListFilter, ResizeHandle, RotateCw, UPDATE_CONTACTS_NOTE, UPDATE_ERROR, UPDATE_PAUSED_DESCRIPTION, UserPlus, X, contactSeed, seedKey } from "@peppy/desktop-ui";
import { bridge, type ContactBookView, type ContactScanStatus, type ContactSort, type ContactSyncStatus, type ContactView, type RestorableContact } from "../bridge/bridge";
import { errorCode, errorText } from "../bridge/bridgeErrors";
import { ContactForm, type ContactEditDraft } from "./ContactForm";
import { findContactTarget } from "./contact-target";
import { ContactList } from "./ContactList";
import { useContactPages } from "./useContactPages";
import { canCreateContactInBook } from "./conversation-contact";
import { bookLabels, contactErrorText, scanStatusText } from "./contact-labels";
import type { ContactActionNotice } from "./contact-action-notice";
import { BookLookupStrip, BookRail } from "./BookRail";
import { ContactListHeader } from "./ContactListHeader";
import { RemoveBookDialog, RemovedBookStrip } from "./RemovedBookStrip";
import { ContactsActionNotice } from "./ContactsActionNotice";
import { ContactsStatus, SyncStatusStrip, activeContactsStatus, type ContactsStatusStrip } from "./ContactsStatus";
import { persistSortPreference, readSortPreference } from "./contact-sort";
import { persistNameFormat, readNameFormat, type ContactNameFormat } from "./contact-name-format";
import { COMPACT_BOOK_LIST_QUERY, COMPACT_DETAIL_QUERY, useMediaMatch } from "./useMediaMatch";
import { persistLayoutPatch, readContactsBookCollapsed, readContactsBookWidth, readContactsDetailWidth } from "../shell/layout-persistence";
import { IsolatedText } from "../notices/IsolatedText";
import { ContactDetail } from "./ContactDetail";
import { RestorePanel } from "./RestorePanel";
import { defaultLookupFailure, type LookupFailure } from "./useBookLookup";
import "./contacts.css";

/** Core returns at most this many contacts per page (search runs before paging). */
export { CONTACTS_PAGE_SIZE } from "./contact-target";

const DETAIL_DEFAULT_WIDTH = 320;
const DETAIL_MIN_WIDTH = 280;
const BOOK_DEFAULT_WIDTH = 220;
const BOOK_MIN_WIDTH = 180;
const BOOK_MAX_WIDTH = 320;
/** A rail dragged narrower than this collapses instead of shrinking to its minimum. */
const BOOK_COLLAPSE_BELOW = 120;
// A double-click right after a click-restore must not collapse the rail again.
const BOOK_RESTORE_GUARD_MS = 500;
/** Visible width of the rail's resize handle when expanded. */
const BOOK_RESIZE_HANDLE_WIDTH = 1;
/** The center pane keeps at least this much room; below it the rail collapses for space (not persisted). */
const CONTACTS_CENTER_MIN_WIDTH = 240;

function clampBookWidth(width: number) {
  return Math.min(BOOK_MAX_WIDTH, Math.max(BOOK_MIN_WIDTH, width));
}
const DETAIL_MAX_WIDTH = 480;
const CONTACTS_LIST_MIN_WIDTH = 240;
const DETAIL_RESIZE_HANDLE_WIDTH = 1;

export type ContactNavigationGuard = (navigate: () => void, onCancel?: () => void) => boolean | void;
export type ContactTarget =
  | { kind?: "contact"; bookId: string; contactId: string; requestId?: number }
  | { kind: "create"; phone: string; requestId: number };

type CreateContactEditing = "create" | { createPrefill: { phone: string }; requestId: number };

function isCreateEditing(editing: ContactView | null | CreateContactEditing): editing is CreateContactEditing {
  return editing === "create" || (editing !== null && "createPrefill" in editing);
}

export function ContactsView({ books: initialBooks = [], sync, onNavigationGuard, target, onTargetConsumed, toneOverrides = {}, toneDisabled = false, writesPaused = false, pauseNoticeVisible = writesPaused, onUpdateInProgress, pendingToneSeeds = {}, toneErrors = {}, onSelectTone, bookCollapsed: bookCollapsedProp, onBookCollapsedChange, query = "", onClearQuery }: {
  books?: ContactBookView[];
  sync?: ContactSyncStatus;
  onNavigationGuard?(guard: ContactNavigationGuard | null): void;
  target?: ContactTarget | null;
  onTargetConsumed?(requestId: number): void;
  /** The title-bar search text that filters the list; empty shows every contact. */
  query?: string;
  onClearQuery?(): void;
  toneOverrides?: Record<string, number>;
  toneDisabled?: boolean;
  writesPaused?: boolean;
  pauseNoticeVisible?: boolean;
  onUpdateInProgress?(error: unknown): void;
  pendingToneSeeds?: Record<string, true>;
  toneErrors?: Record<string, string>;
  onSelectTone?(seed: string, index: number | null): void;
  /** Controlled collapse of the book rail (App owns and persists it). Uncontrolled when absent. */
  bookCollapsed?: boolean;
  onBookCollapsedChange?(collapsed: boolean): void;
}) {
  // An update-in-progress rejection tells App to refresh the snapshot and shows the warning copy.
  const contactFailure = (error: unknown) => {
    if (errorCode(error) === "update-in-progress") {
      onUpdateInProgress?.(error);
      return errorText(error);
    }
    return contactErrorText(error);
  };
  // The lookup switch keeps its own toast: only an update-in-progress rejection changes its text, and a retry would fail the same way.
  const lookupFailure: LookupFailure = error => errorCode(error) === "update-in-progress"
    ? { message: contactFailure(error), retryable: false }
    : defaultLookupFailure(error);
  const contactsRepairState = sync?.projection?.state === "rebuilding" ? "rebuilding"
    : sync?.projection?.state === "failed" ? (sync.repairRequired ? "repairWaiting" : "failed")
    : sync?.repairRequired ? "needsRepair" : undefined;
  const contactsSyncText = contactsRepairState === "rebuilding" ? "Rebuilding contacts from the server…"
    : contactsRepairState === "repairWaiting" ? "Contacts may be out of date. A repair is waiting to run."
    : contactsRepairState === "failed" ? "Contacts may be out of date. Peppy couldn't finish updating them."
    : "Contacts need a repair from the server.";
  const completedScanText = contactsRepairState === "rebuilding" ? "The phone finished its scan. Contacts are still being repaired locally."
    : contactsRepairState === "repairWaiting" ? "The phone finished its scan. A repair is waiting to run on this computer."
    : contactsRepairState === "failed" ? "The phone finished its scan. Peppy couldn't finish updating contacts on this computer."
    : undefined;
  const [books, setBooks] = useState(initialBooks);
  const [booksLoaded, setBooksLoaded] = useState(initialBooks.length > 0);
  const [bookId, setBookId] = useState(initialBooks[0]?.id ?? "");
  const [selected, setSelected] = useState<ContactView | null>(null);
  const [sort, setSort] = useState<ContactSort>(readSortPreference);
  const [nameFormat, setNameFormat] = useState(readNameFormat);
  const [sortOpen, setSortOpen] = useState(false);
  const [restore, setRestore] = useState<{ bookId: string; items: RestorableContact[] } | null>(null);
  const [editing, setEditing] = useState<ContactView | null | CreateContactEditing>(null);
  const [editorBookId, setEditorBookId] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [loadError, setLoadError] = useState("");
  const [validationError, setValidationError] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const deleteInFlight = useRef(false);
  useEffect(() => { deleteInFlight.current = false; }, [selected?.bookId, selected?.id]);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<ReactNode>("");
  const [actionNotice, setActionNotice] = useState<ContactActionNotice>();
  const [bookRemoval, setBookRemoval] = useState<{ book: ContactBookView; opener: RefObject<HTMLElement | null> }>();
  const pages = useContactPages({ bookId, sort, query, onError: setLoadError });
  const { contacts } = pages;
  useEffect(() => {
    setSelected(current => (current ? contacts.find(item => item.id === current.id) ?? current : current));
  }, [contacts]);
  const [scanStatus, setScanStatus] = useState<ContactScanStatus | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [pendingNavigation, setPendingNavigation] = useState<{ action: () => void; onCancel?: () => void } | null>(null);
  const [targetLoading, setTargetLoading] = useState(false);
  const [targetError, setTargetError] = useState("");
  const [focusTargetId, setFocusTargetId] = useState<string | null>(null);
  const [targetLookupRestart, setTargetLookupRestart] = useState(0);
  const [detailWidth, setDetailWidth] = useState(() => readContactsDetailWidth() ?? DETAIL_DEFAULT_WIDTH);
  const [contactsLayout, setContactsLayout] = useState({ viewWidth: 0, bookWidth: 0 });
  const detailDragWidth = useRef(detailWidth);
  const detailDragActive = useRef(false);
  const contactsViewRef = useRef<HTMLElement>(null);
  const booksPanelRef = useRef<HTMLElement>(null);
  const compactBookList = useMediaMatch(COMPACT_BOOK_LIST_QUERY);
  const [localBookCollapsed, setLocalBookCollapsed] = useState(readContactsBookCollapsed);
  const bookCollapsed = bookCollapsedProp ?? localBookCollapsed;
  const [bookWidth, setBookWidth] = useState(() => clampBookWidth(readContactsBookWidth() ?? BOOK_DEFAULT_WIDTH));
  // Latest width the user chose (≥ min) and the unclamped sash position while a resize gesture runs.
  const bookWidthRef = useRef(bookWidth);
  const bookRawWidth = useRef(bookWidth);
  const bookRestoredAt = useRef(0);
  const spaceCollapsed = contactsLayout.viewWidth > 0 && contactsLayout.viewWidth - bookWidth - BOOK_RESIZE_HANDLE_WIDTH < CONTACTS_CENTER_MIN_WIDTH;
  const railVisible = !compactBookList && !bookCollapsed && !spaceCollapsed;
  const collapsedReason = compactBookList || railVisible ? undefined : bookCollapsed ? "user" : "space";
  const removedBookIdRef = useRef<string | null>(null);
  const sortButtonRef = useRef<HTMLButtonElement>(null);
  const sortMenuRef = useRef<HTMLDivElement>(null);
  const [sortMenuPosition, setSortMenuPosition] = useState<CSSProperties>();
  const restoreTriggerRef = useRef<HTMLButtonElement>(null);
  const newContactTriggerRef = useRef<HTMLButtonElement>(null);
  const detailBackRef = useRef<HTMLButtonElement>(null);
  const scanStatusRequestRef = useRef(0);
  const syncActionRef = useRef(0);
  const currentBookRef = useRef(bookId);
  const booksRef = useRef(books);
  const booksLoadedRef = useRef(initialBooks.length > 0);
  const busyRef = useRef(busy);
  const dirtyRef = useRef(dirty);
  const editingRef = useRef(editing);
  const editorBookIdRef = useRef<string | null>(null);
  const mountedRef = useRef(true);
  const completedTargetRef = useRef<string | undefined>(undefined);
  const targetLookupRef = useRef(0);
  const targetLookupAbortRef = useRef<AbortController | null>(null);
  const pendingNavigationRef = useRef<typeof pendingNavigation>(null);
  const targetRef = useRef(target);
  const sortRef = useRef(sort);
  const onTargetConsumedRef = useRef(onTargetConsumed);

  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);

  booksRef.current = books;
  busyRef.current = busy;
  dirtyRef.current = dirty;
  editingRef.current = editing;
  targetRef.current = target;
  sortRef.current = sort;
  onTargetConsumedRef.current = onTargetConsumed;
  const onClearQueryRef = useRef(onClearQuery);
  onClearQueryRef.current = onClearQuery;
  pendingNavigationRef.current = pendingNavigation;

  const replacePendingNavigation = (next: typeof pendingNavigation) => {
    const previous = pendingNavigationRef.current;
    if (previous !== next) previous?.onCancel?.();
    pendingNavigationRef.current = next;
    setPendingNavigation(next);
  };

  const targetKey = (value: ContactTarget) => value.kind === "create" || value.requestId !== undefined
    ? `request:${value.requestId}`
    : `${value.bookId}:${value.contactId}`;
  const consumeTarget = useCallback((value: ContactTarget) => {
    const key = targetKey(value);
    if (completedTargetRef.current === key) return;
    completedTargetRef.current = key;
    if (value.requestId !== undefined) onTargetConsumedRef.current?.(value.requestId);
  }, []);
  const cancelTargetLookup = useCallback((consume = true) => {
    targetLookupAbortRef.current?.abort();
    targetLookupAbortRef.current = null;
    targetLookupRef.current += 1;
    setTargetLoading(false);
    setTargetError("");
    const current = targetRef.current;
    if (consume && current) consumeTarget(current);
  }, [consumeTarget]);
  useEffect(() => () => {
    targetLookupAbortRef.current?.abort();
    pendingNavigationRef.current?.onCancel?.();
    targetLookupRef.current += 1;
    const unmountedTarget = targetRef.current;
    queueMicrotask(() => {
      if (!mountedRef.current && unmountedTarget && targetRef.current === unmountedTarget) consumeTarget(unmountedTarget);
    });
  }, [consumeTarget]);

  const loadBooks = useCallback(() => {
    void bridge
      .list_contact_books()
      .then(next => {
        if (!mountedRef.current) return;
        setBooks(next);
        if (editorBookIdRef.current && !next.some(book => book.id === editorBookIdRef.current)) {
          setError("This contact book is no longer available. Your draft is still open.");
        }
        setLoadError("");
        booksLoadedRef.current = true;
        setBooksLoaded(true);
        const currentBookId = currentBookRef.current;
        if (currentBookId && next.some(book => book.id === currentBookId)) return;
        const fallbackBookId = next[0]?.id ?? "";
        if (fallbackBookId === currentBookId) return;
        currentBookRef.current = fallbackBookId;
        setBookId(fallbackBookId);
        setRestore(null);
        if (!editingRef.current || !dirtyRef.current) {
          setSelected(null);
          setConfirmDelete(false);
        }
      })
      .catch(() => {
        if (!mountedRef.current) return;
        // A background refresh failure keeps the loaded books and any active target walk.
        if (booksLoadedRef.current) {
          setLoadError("Contacts could not be loaded right now.");
          return;
        }
        booksLoadedRef.current = true;
        setBooksLoaded(true);
        const currentTarget = targetRef.current;
        if (!currentTarget || completedTargetRef.current === targetKey(currentTarget)) {
          setLoadError("Contacts could not be loaded right now.");
          return;
        }
        setTargetError("The requested contact could not be opened because contact books could not be loaded.");
        consumeTarget(currentTarget);
      });
  }, []);

  useEffect(() => {
    if (!sortOpen) return;
    const dismiss = (event: MouseEvent) => {
      if (!sortMenuRef.current?.contains(event.target as Node) && !sortButtonRef.current?.contains(event.target as Node)) setSortOpen(false);
    };
    document.addEventListener("mousedown", dismiss);
    return () => document.removeEventListener("mousedown", dismiss);
  }, [sortOpen]);
  useEffect(() => {
    if (!sortOpen) return;
    const frame = requestAnimationFrame(() => sortMenuRef.current?.querySelector<HTMLButtonElement>('[role="menuitemradio"][aria-checked="true"]')?.focus());
    return () => cancelAnimationFrame(frame);
  }, [sortOpen]);
  useLayoutEffect(() => {
    const view = contactsViewRef.current;
    const booksPanel = booksPanelRef.current;
    if (!view) return;
    const measure = () => setContactsLayout({ viewWidth: view.clientWidth || window.innerWidth, bookWidth: booksPanel?.offsetWidth ?? 0 });
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(view);
    if (booksPanel) observer.observe(booksPanel);
    return () => observer.disconnect();
  }, [railVisible]);
  useLayoutEffect(() => {
    if (!sortOpen || !sortButtonRef.current) return;
    const positionMenu = () => {
      const rect = sortButtonRef.current!.getBoundingClientRect();
      const menuWidth = 208;
      const margin = 8;
      setSortMenuPosition({
        position: "fixed",
        top: Math.min(window.innerHeight - margin, rect.bottom + 4),
        left: Math.max(margin, Math.min(window.innerWidth - menuWidth - margin, rect.right - menuWidth)),
      });
    };
    positionMenu();
    window.addEventListener("resize", positionMenu);
    return () => window.removeEventListener("resize", positionMenu);
  }, [sortOpen]);
  const changeSort = (next: ContactSort) => {
    if (targetLookupAbortRef.current) {
      targetLookupAbortRef.current.abort();
      targetLookupAbortRef.current = null;
      targetLookupRef.current += 1;
      setTargetLoading(false);
      setTargetError("");
      setTargetLookupRestart(current => current + 1);
    }
    setSort(next);
    persistSortPreference(next);
  };

  useEffect(loadBooks, [loadBooks]);

  const book = books.find(item => item.id === bookId);
  const compactDetail = useMediaMatch(COMPACT_DETAIL_QUERY);
  const detailOverlay = compactDetail && Boolean(selected || editing);

  const refreshScanStatus = useCallback(() => {
    const requestedBook = currentBookRef.current;
    if (!requestedBook) return setScanStatus(null);
    const sequence = ++scanStatusRequestRef.current;
    void bridge.get_contact_sync_request(requestedBook).then(status => {
      if (mountedRef.current && sequence === scanStatusRequestRef.current && currentBookRef.current === requestedBook) setScanStatus(status);
    }).catch(() => {
      // A transient status read must not erase the last persisted pending state.
    });
  }, []);
  useEffect(() => {
    currentBookRef.current = bookId;
    refreshScanStatus();
  }, [bookId, refreshScanStatus]);
  useEffect(() => {
    if (restore && restore.bookId !== bookId) setRestore(null);
  }, [bookId, restore]);
  useEffect(() => {
    if (!scanStatus || !["queued", "waiting", "scanning"].includes(scanStatus.state)) return;
    const interval = window.setInterval(refreshScanStatus, 5_000);
    return () => window.clearInterval(interval);
  }, [refreshScanStatus, scanStatus]);
  // Native state hints refresh the visible book without replacing unsaved editor state.
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const dispose = bridge.subscribe(() => {
      clearTimeout(timer);
      timer = setTimeout(() => { loadBooks(); pages.refresh(); refreshScanStatus(); }, 150);
    });
    return () => { clearTimeout(timer); dispose(); };
  }, [loadBooks, pages.refresh, refreshScanStatus]);

  const handleSave = async (draft: ContactEditDraft) => {
    const editorBook = booksRef.current.find(candidate => candidate.id === editorBookIdRef.current);
    if (!editorBook || !editing) {
      setError("This contact book is no longer available. Your draft is still open.");
      return;
    }
    setError("");
    setValidationError("");
    setBusy(true);

    try {
      const isCreate = isCreateEditing(editing);
      const current = isCreate ? undefined : editing;

      // Build patches from draft changes
      const patches: unknown[] = [];

      if (current) {
        // Update case: build patches for changed fields
        if ((current.givenName ?? "") !== (draft.givenName ?? "")) {
          patches.push({ field: "givenName", value: draft.givenName, expectedOld: current.givenName });
        }
        if ((current.familyName ?? "") !== (draft.familyName ?? "")) {
          patches.push({ field: "familyName", value: draft.familyName, expectedOld: current.familyName });
        }
        if ((current.nickname ?? "") !== (draft.nickname ?? "")) {
          patches.push({ field: "nickname", value: draft.nickname, expectedOld: current.nickname });
        }
        if ((current.organization ?? "") !== (draft.organization ?? "")) {
          patches.push({ field: "organization", value: draft.organization, expectedOld: current.organization });
        }
        if ((current.title ?? "") !== (draft.title ?? "")) {
          patches.push({ field: "title", value: draft.title, expectedOld: current.title });
        }
        if ((current.notes ?? "") !== (draft.notes ?? "")) {
          patches.push({ field: "notes", value: draft.notes, expectedOld: current.notes });
        }

        // Phone comparison
        if (JSON.stringify(current.phones) !== JSON.stringify(draft.phones)) {
          patches.push({ field: "phones", value: draft.phones, expectedOld: current.phones });
        }

        // Email comparison
        if (JSON.stringify(current.emails) !== JSON.stringify(draft.emails)) {
          patches.push({ field: "emails", value: draft.emails, expectedOld: current.emails });
        }

        // Address comparison
        if (JSON.stringify(current.addresses) !== JSON.stringify(draft.addresses)) {
          patches.push({ field: "addresses", value: draft.addresses, expectedOld: current.addresses });
        }

        // Birthday comparison
        if (JSON.stringify(current.birthday ?? {}) !== JSON.stringify(draft.birthday ?? {})) {
          patches.push({ field: "birthday", value: draft.birthday, expectedOld: current.birthday });
        }
      } else {
        // Create case: include all fields
        patches.push({ field: "givenName", value: draft.givenName });
        patches.push({ field: "familyName", value: draft.familyName });
        patches.push({ field: "nickname", value: draft.nickname });
        patches.push({ field: "organization", value: draft.organization });
        patches.push({ field: "title", value: draft.title });
        patches.push({ field: "phones", value: draft.phones });
        patches.push({ field: "emails", value: draft.emails });
        patches.push({ field: "addresses", value: draft.addresses });
        patches.push({ field: "birthday", value: draft.birthday });
        patches.push({ field: "notes", value: draft.notes });
      }

      const photoKind = draft.photoKind;
      const photoRequest =
        photoKind === "keep"
          ? { kind: "keep" as const }
          : photoKind === "remove"
            ? { kind: "remove" as const }
            : { kind: "set" as const, croppedDataUrl: draft.photoDataUrl! };

      const outcome = await bridge.submit_contact_edit({
        targetBookId: editorBook.id,
        kind: isCreate ? "create" : "update",
        contactId: current?.id,
        baseRevision: current?.revision,
        patches,
        photo: photoRequest,
      });

      if (outcome.state === "rejected") {
        setError(outcome.reason);
        setValidationError(outcome.reason);
        setBusy(false);
        return;
      }

      if (outcome.state === "conflict") {
        setError(`Conflict: ${outcome.conflictSummary}`);
        setBusy(false);
        return;
      }

      // Sent, not applied: the phone owns the change until its result arrives.
      setEditing(null);
      editorBookIdRef.current = null;
      setEditorBookId(null);
      setDirty(false);
      setSelected(null);
      setError("");
      setValidationError("");
      setNotice(outcome.state === "pending" ? <span>Sent to <IsolatedText text={editorBook.deviceName} />. Waiting for the phone to apply it.</span> : "");
      pages.refresh();
      const pending = pendingNavigationRef.current;
      if (pending) {
        pendingNavigationRef.current = null;
        setPendingNavigation(null);
        pending.action();
      }
    } catch (e) {
      const message = contactFailure(e);
      setError(message);
      setValidationError(message);
    } finally {
      setBusy(false);
    }
  };

  const handleDelete = async () => {
    if (!selected || !book) return;

    if (!confirmDelete) {
      setConfirmDelete(true);
      return;
    }
    if (deleteInFlight.current) return;

    deleteInFlight.current = true;
    setError("");
    setBusy(true);

    try {
      const outcome = await bridge.submit_contact_edit({
        targetBookId: selected.bookId,
        kind: "delete",
        contactId: selected.id,
        baseRevision: selected.revision,
        patches: [],
        photo: { kind: "keep" },
      });

      if (outcome.state === "rejected") {
        setError(outcome.reason);
        return;
      }

      if (outcome.state === "conflict") {
        setError(`Conflict: ${outcome.conflictSummary}`);
        return;
      }

      setSelected(null);
      setConfirmDelete(false);
      setError("");
      setNotice(outcome.state === "pending" ? <span>Delete sent to <IsolatedText text={book.deviceName} />. Waiting for the phone.</span> : "");
      pages.refresh();
    } catch (e) {
      setError(contactFailure(e));
    } finally {
      deleteInFlight.current = false;
      setBusy(false);
    }
  };

  const requestNavigation = useCallback((action: () => void, onCancel?: () => void) => {
    if (busy) {
      onCancel?.();
      return false;
    }
    if (editing && dirty) {
      replacePendingNavigation({ action, onCancel });
      return true;
    }
    action();
    return false;
  }, [busy, dirty, editing]);

  useEffect(() => {
    onNavigationGuard?.(requestNavigation);
    return () => onNavigationGuard?.(null);
  }, [onNavigationGuard, requestNavigation]);

  const activeTargetKey = target ? targetKey(target) : undefined;
  useEffect(() => {
    const requestedTarget = targetRef.current;
    if (!requestedTarget || requestedTarget.kind === "create" || !booksLoaded || targetKey(requestedTarget) !== activeTargetKey) return;
    if (completedTargetRef.current === activeTargetKey) return;
    setTargetError("");
    // A deep link opens the contact in the full list, so a title-bar search does not narrow the lookup.
    onClearQueryRef.current?.();
    const sequence = ++targetLookupRef.current;
    const isCurrent = () => mountedRef.current
      && sequence === targetLookupRef.current
      && targetRef.current !== null
      && targetRef.current !== undefined
      && targetKey(targetRef.current) === activeTargetKey;
    const finish = () => {
      if (!isCurrent()) return;
      setTargetLoading(false);
      consumeTarget(requestedTarget);
    };
    const startLookup = () => {
      const targetBook = booksRef.current.find(candidate => candidate.id === requestedTarget.bookId);
      if (!targetBook) {
        if (isCurrent()) setTargetError("The requested contact book is no longer available.");
        finish();
        return;
      }
      setTargetLoading(true);
      targetLookupAbortRef.current?.abort();
      const controller = new AbortController();
      targetLookupAbortRef.current = controller;
      void (async () => {
        const result = await findContactTarget(requestedTarget, bridge.list_contacts_page, controller.signal, (bookId) => booksRef.current.some((book) => book.id === bookId), sortRef.current);
        if (!isCurrent()) return;
        if (result.state === "cancelled") {
          if (!booksRef.current.some((book) => book.id === requestedTarget.bookId)) {
            setTargetError("The requested contact book is no longer available.");
            finish();
          }
          return;
        }
        targetLookupAbortRef.current = null;
        if (!booksRef.current.some(candidate => candidate.id === requestedTarget.bookId)) {
          setTargetError("The requested contact book is no longer available.");
          finish();
          return;
        }
        if (result.state === "found") {
          const { contact, contacts: results } = result;
          setTargetError("");
          if (currentBookRef.current !== requestedTarget.bookId) {
            ++scanStatusRequestRef.current;
            ++syncActionRef.current;
            setScanStatus(null);
            setSyncing(false);
          }
          currentBookRef.current = requestedTarget.bookId;
          setBookId(requestedTarget.bookId);
          pages.resetTo({ contacts: results, total: result.total, nextCursor: result.nextCursor }, { bookId: requestedTarget.bookId, sort: sortRef.current });
          setRestore(null);
          setSelected(contact);
          setEditing(null);
          setConfirmDelete(false);
          setFocusTargetId(contact.id);
          finish();
          return;
        }
        if (result.state === "unavailable") setTargetError("The requested contact is no longer available.");
        else setTargetError("The requested contact could not be searched because this book is too large.");
        finish();
      })().catch(() => {
        if (isCurrent()) setTargetError("The requested contact could not be searched right now.");
        finish();
      });
    };
    if (busyRef.current) {
      setTargetLoading(false);
      setNotice("Finish saving the current contact before opening the requested contact.");
      consumeTarget(requestedTarget);
      return;
    }
    if (editingRef.current && dirtyRef.current) {
      replacePendingNavigation({ action: startLookup, onCancel: () => consumeTarget(requestedTarget) });
      return;
    }
    startLookup();
  }, [activeTargetKey, booksLoaded, consumeTarget, targetLookupRestart]);

  useEffect(() => {
    const requestedTarget = targetRef.current;
    if (!requestedTarget || requestedTarget.kind !== "create" || targetKey(requestedTarget) !== activeTargetKey) return;
    cancelTargetLookup(false);
    if (!booksLoaded) return;
    if (completedTargetRef.current === activeTargetKey) return;

    const openCreateForm = () => {
      const selectedBook = booksRef.current.find(candidate => candidate.id === currentBookRef.current);
      const targetBook = selectedBook && canCreateContactInBook(selectedBook)
        ? selectedBook
        : booksRef.current.find(canCreateContactInBook);
      if (!targetBook) {
        setTargetError("No writable contact book is available.");
        consumeTarget(requestedTarget);
        return;
      }
      const pinnedBookId = targetBook.id;
      if (currentBookRef.current !== pinnedBookId) {
        currentBookRef.current = pinnedBookId;
        ++scanStatusRequestRef.current;
        ++syncActionRef.current;
        setBookId(pinnedBookId);
        setScanStatus(null);
        setSyncing(false);
        setSelected(null);
        setRestore(null);
      }
      setEditing({ createPrefill: { phone: requestedTarget.phone }, requestId: requestedTarget.requestId });
      editorBookIdRef.current = pinnedBookId;
      setEditorBookId(pinnedBookId);
      setDirty(false);
      setConfirmDelete(false);
      setError("");
      setValidationError("");
      setTargetError("");
      consumeTarget(requestedTarget);
    };

    if (busyRef.current) {
      setNotice("Finish saving the current contact before adding a new one.");
      consumeTarget(requestedTarget);
      return;
    }
    if (editingRef.current && dirtyRef.current) {
      replacePendingNavigation({ action: openCreateForm, onCancel: () => consumeTarget(requestedTarget) });
      return;
    }
    openCreateForm();
  }, [activeTargetKey, booksLoaded, cancelTargetLookup, consumeTarget]);

  useEffect(() => {
    if (!focusTargetId || detailOverlay) return;
    const frame = requestAnimationFrame(() => {
      const row = Array.from(document.querySelectorAll<HTMLButtonElement>("button[data-contact-id]"))
        .find(node => node.dataset.contactId === focusTargetId);
      row?.scrollIntoView({ block: "center" });
      row?.focus();
      setFocusTargetId(null);
    });
    return () => cancelAnimationFrame(frame);
  }, [detailOverlay, focusTargetId]);

  useEffect(() => {
    if (!detailOverlay) return;
    const frame = requestAnimationFrame(() => detailBackRef.current?.focus());
    return () => cancelAnimationFrame(frame);
  }, [detailOverlay]);

  const discardNavigation = () => {
    const pending = pendingNavigationRef.current;
    if (!pending || busy) return;
    setDirty(false);
    const { action } = pending;
    pendingNavigationRef.current = null;
    setPendingNavigation(null);
    action();
  };

  const handleSelectBook = (id: string) => {
    cancelTargetLookup();
    requestNavigation(() => {
      currentBookRef.current = id;
      ++scanStatusRequestRef.current;
      ++syncActionRef.current;
      setBookId(id);
      setScanStatus(null);
      setSyncing(false);
      setSelected(null);
      setEditing(null);
      editorBookIdRef.current = null;
      setEditorBookId(null);
      setRestore(null);
      setConfirmDelete(false);
      setError("");
    });
  };

  // Reloading drops the removed book and selects the first remaining one; focus then moves to that book's list entry.
  const handleBookRemoved = (removedId: string) => {
    removedBookIdRef.current = removedId;
    loadBooks();
  };
  // One removal dialog for the whole view, so it survives the status slot changing and focus returns to its opener.
  const requestRemoveBook = (book: ContactBookView, opener: RefObject<HTMLElement | null>) => setBookRemoval({ book, opener });
  // The request stays armed until the removed book is actually gone, so an earlier books refresh cannot consume it.
  useEffect(() => {
    const removedId = removedBookIdRef.current;
    if (!removedId || books.some(item => item.id === removedId)) return;
    removedBookIdRef.current = null;
    const focusTarget = railVisible
      ? document.querySelector<HTMLElement>("#contacts-books .contacts-book-row[aria-current]")
      : document.getElementById("contacts-book-selector") ?? document.getElementById("contacts-list-title") ?? document.getElementById("contacts-list");
    focusTarget?.focus();
  }, [books, railVisible]);

  const handleCancelConfirm = () => setConfirmDelete(false);

  const handleSelectContact = (contact: ContactView) => {
    cancelTargetLookup();
    requestNavigation(() => {
    setSelected(contact);
    setEditing(null);
    setConfirmDelete(false);
    });
  };

  const handleEditStart = (contact?: ContactView) => {
    cancelTargetLookup();
    requestNavigation(() => {
      if (!bookId) return;
      setEditing(contact ?? "create");
      const pinnedBookId = bookId;
      editorBookIdRef.current = pinnedBookId;
      setEditorBookId(pinnedBookId);
      setDirty(false);
      setConfirmDelete(false);
      setError("");
      setValidationError("");
    });
  };
  const handleDetailBack = () => requestNavigation(() => {
    if (selected) setFocusTargetId(selected.id);
    else requestAnimationFrame(() => newContactTriggerRef.current?.focus());
    setSelected(null);
    setConfirmDelete(false);
    setEditing(null);
    editorBookIdRef.current = null;
    setEditorBookId(null);
    setDirty(false);
    setError("");
    setValidationError("");
  });

  const handleEditCancel = () => requestNavigation(() => {
    setEditing(null);
    editorBookIdRef.current = null;
    setEditorBookId(null);
    setDirty(false);
    setError("");
    setValidationError("");
  });

  // Books have loaded and there are none: no rail, no book-scoped actions, and no "select a book" wording.
  const noBooks = booksLoaded && books.length === 0;
  const noBooksTitle = "Connect a phone to see its contacts.";
  // Title precedence: no books > paused > the rest. With no books nothing can be acted on, so the no-books reason wins.
  const pausedTitle = writesPaused && !noBooks ? UPDATE_PAUSED_DESCRIPTION : undefined;
  const pausedDescribedBy = pausedTitle ? "update-paused-description" : undefined;
  const syncDisabledReason = !book
    ? (booksLoaded && !noBooks ? "Select a contact book to request a scan." : undefined)
    : book.sourceRevoked ? "Can't sync a removed phone"
      : book.state === "retired" ? "This book is retired."
        : book.state === "limited" ? "This phone has limited contact access and cannot complete an authoritative scan."
          : book.state === "unavailable" ? "This phone is unavailable for contact scans."
          : !book.capabilities.canSync ? "This phone does not support remote contact scans."
          : sync?.readiness?.state === "needs_unlock" ? "Unlock sync before requesting a scan."
            : sync?.readiness?.state === "server_unsupported" ? "This server does not support remote contact scans."
              : sync?.readiness?.state === "backfill_pending" ? "Contact sync is preparing historical records."
            : undefined;
  const requestSync = () => {
    if (!book || syncDisabledReason || syncing) return;
    setSyncing(true);
    setError("");
    const requestedBook = book.id;
    const action = ++syncActionRef.current;
    void bridge.request_contact_sync(requestedBook)
      .then(status => {
        if (mountedRef.current && action === syncActionRef.current && currentBookRef.current === requestedBook) {
          setScanStatus(status);
        }
      })
      .catch(error => {
        if (mountedRef.current && action === syncActionRef.current && currentBookRef.current === requestedBook) setError(contactFailure(error));
      })
      .finally(() => {
        if (mountedRef.current && action === syncActionRef.current && currentBookRef.current === requestedBook) setSyncing(false);
      });
  };

  const openRecentlyDeleted = () => {
    if (detailOverlay || editing) return;
    setConfirmDelete(false);
    const restoreBookId = bookId;
    void bridge.list_restorable_contacts(restoreBookId)
      .then(items => {
        if (currentBookRef.current === restoreBookId) setRestore({ bookId: restoreBookId, items });
      })
      .catch(error => setError(contactErrorText(error)));
  };
  const viewLabel = `View options. Sorted by ${sort.key === "family" ? "last name" : "first name"}, ${sort.direction === "asc" ? "ascending" : "descending"}. Names shown as ${nameFormat === "last-first" ? "last name, first name" : "first name then last name"}.`;
  const closeSortMenu = (returnFocus = false) => {
    setSortOpen(false);
    if (returnFocus) requestAnimationFrame(() => sortButtonRef.current?.focus());
  };
  const handleSortMenuKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const menuItems = Array.from(sortMenuRef.current?.querySelectorAll<HTMLButtonElement>('[role="menuitemradio"]') ?? []);
    const index = menuItems.indexOf(document.activeElement as HTMLButtonElement);
    if (event.key === "Escape") {
      event.preventDefault();
      closeSortMenu(true);
    } else if (event.key === "Tab") {
      event.preventDefault();
      closeSortMenu(true);
    } else if (index >= 0 && ["ArrowDown", "ArrowRight", "ArrowUp", "ArrowLeft", "Home", "End"].includes(event.key)) {
      event.preventDefault();
      const nextIndex = event.key === "Home" ? 0 : event.key === "End" ? menuItems.length - 1 : (index + (event.key === "ArrowDown" || event.key === "ArrowRight" ? 1 : -1) + menuItems.length) % menuItems.length;
      menuItems[nextIndex]?.focus();
    }
  };
  const selectSort = (next: ContactSort) => {
    changeSort(next);
    closeSortMenu(true);
  };
  const selectNameFormat = (next: ContactNameFormat) => {
    persistNameFormat(next);
    setNameFormat(next);
    closeSortMenu(true);
  };
  const sortMenu = sortOpen && (
    <div ref={sortMenuRef} id="contacts-sort-menu" role="menu" aria-label="Sort contacts" style={sortMenuPosition} onKeyDown={handleSortMenuKeyDown}>
      <div role="group" aria-label="Sort by"><div role="presentation" className="contacts-sort-heading">Sort by</div>{(["family", "given"] as const).map(key => <button id={`contacts-sort-key-${key}`} type="button" role="menuitemradio" aria-checked={sort.key === key} key={key} onClick={() => selectSort({ ...sort, key })}><span className="contacts-sort-check" aria-hidden="true">{sort.key === key && <Check size={14} aria-hidden />}</span><span className="contacts-sort-label">{key === "family" ? "Last name" : "First name"}</span></button>)}</div>
      <div role="group" aria-label="Order"><div role="presentation" className="contacts-sort-heading">Order</div>{(["asc", "desc"] as const).map(direction => <button id={`contacts-sort-direction-${direction}`} type="button" role="menuitemradio" aria-checked={sort.direction === direction} key={direction} onClick={() => selectSort({ ...sort, direction })}><span className="contacts-sort-check" aria-hidden="true">{sort.direction === direction && <Check size={14} aria-hidden />}</span><span className="contacts-sort-label">{direction === "asc" ? "Ascending" : "Descending"}</span></button>)}</div>
      <div role="group" aria-label="Show names as">
        <div role="presentation" className="contacts-sort-heading">Show names as</div>
        <button id="contacts-name-format-first-last" type="button" role="menuitemradio" aria-checked={nameFormat === "first-last"} onClick={() => selectNameFormat("first-last")}><span className="contacts-sort-check" aria-hidden="true">{nameFormat === "first-last" && <Check size={14} aria-hidden />}</span><span className="contacts-sort-label">First Last<small className="contacts-sort-example">Ada Lovelace</small></span></button>
        <button id="contacts-name-format-last-first" type="button" role="menuitemradio" aria-checked={nameFormat === "last-first"} onClick={() => selectNameFormat("last-first")}><span className="contacts-sort-check" aria-hidden="true">{nameFormat === "last-first" && <Check size={14} aria-hidden />}</span><span className="contacts-sort-label">Last, First<small className="contacts-sort-example">Lovelace, Ada</small></span></button>
      </div>
    </div>
  );
  const sortPortalHost = contactsViewRef.current?.closest<HTMLElement>("#desktop-shell, #composer-shell") ?? document.body;
  const selectBookTitle = "Select a contact book first.";
  const closeDetailTitle = "Close the contact detail first.";
  const availableDetailWidth = contactsLayout.viewWidth - contactsLayout.bookWidth - CONTACTS_LIST_MIN_WIDTH - DETAIL_RESIZE_HANDLE_WIDTH;
  const detailMaxWidth = Math.min(DETAIL_MAX_WIDTH, Math.max(0, availableDetailWidth));
  const detailMinWidth = Math.min(DETAIL_MIN_WIDTH, detailMaxWidth);
  const renderedDetailWidth = Math.max(detailMinWidth, Math.min(detailWidth, detailMaxWidth));
  const setDetailSize = (value: number) => {
    const width = Math.max(detailMinWidth, Math.min(detailMaxWidth, value));
    detailDragWidth.current = width;
    setDetailWidth(width);
  };
  const resizeDetail = (delta: number) => setDetailSize((detailDragActive.current ? detailDragWidth.current : renderedDetailWidth) - delta);
  const resetDetailWidth = () => { detailDragActive.current = false; setDetailWidth(DETAIL_DEFAULT_WIDTH); detailDragWidth.current = DETAIL_DEFAULT_WIDTH; persistLayoutPatch({ contactsDetailWidth: DETAIL_DEFAULT_WIDTH }); };
  const changeBookCollapsed = (collapsed: boolean) => {
    if (onBookCollapsedChange) onBookCollapsedChange(collapsed);
    else setLocalBookCollapsed(collapsed);
  };
  // A sash dragged narrower than BOOK_COLLAPSE_BELOW collapses; wider values are kept between min and max.
  const sizeBook = (width: number) => {
    if (width < BOOK_COLLAPSE_BELOW) {
      bookRawWidth.current = width;
      // Every drag frame below the threshold lands here; only the first one changes (and persists) the state.
      if (!bookCollapsed) changeBookCollapsed(true);
      return;
    }
    const capped = Math.min(width, BOOK_MAX_WIDTH);
    bookRawWidth.current = capped;
    const next = clampBookWidth(capped);
    bookWidthRef.current = next;
    setBookWidth(next);
    if (bookCollapsed) {
      bookRestoredAt.current = Date.now();
      changeBookCollapsed(false);
    }
  };
  const startBookResize = (position: number) => {
    const viewLeft = contactsViewRef.current?.getBoundingClientRect().left ?? 0;
    bookRawWidth.current = bookCollapsed ? position - viewLeft : bookWidthRef.current;
  };
  const resizeBook = (delta: number) => {
    bookRawWidth.current += delta;
    sizeBook(bookRawWidth.current);
  };
  const endBookResize = () => {
    bookRawWidth.current = bookWidthRef.current;
    persistLayoutPatch({ contactsBookWidth: bookWidthRef.current });
  };
  const toggleBookCollapsed = () => {
    if (Date.now() - bookRestoredAt.current < BOOK_RESTORE_GUARD_MS) return;
    changeBookCollapsed(!bookCollapsed);
  };
  // At ≤560px the detail panel overlays the center pane, so banners that concern it must render inside the panel.
  const unsavedBanner = pendingNavigation && (
    <p id="contacts-unsaved-changes" role="status" data-banner-state="unsaved-changes">
      You have unsaved changes.
      <button type="button" className="secondary-button" onClick={() => replacePendingNavigation(null)}>Keep editing</button>
      <button type="button" className="secondary-button" onClick={discardNavigation} disabled={busy}>Discard</button>
    </p>
  );
  const noticeBanner = notice && (
    <p id="contacts-notice" role="status" data-banner-state="notice">
      {notice}
      <button aria-label="Dismiss" onClick={() => setNotice("")}>
        <X size={12} />
      </button>
    </p>
  );
  const visibleError = error || loadError;
  const errorBanner = !editing && visibleError && <p id="contacts-error" role={visibleError === UPDATE_ERROR ? "status" : "alert"} data-banner-state={visibleError === UPDATE_ERROR ? "warning" : "error"}>{visibleError}<button aria-label="Dismiss" onClick={() => { setError(""); setLoadError(""); }}><X size={12} /></button></p>;
  const targetStatus = targetError && <p id="contacts-target-error" role="status" data-target-state="error">{targetError}</p>;
  const targetLoadingStatus = targetLoading && (
    <p id="contacts-target-status" role="status" data-target-state="loading">
      Finding requested contact… <button type="button" className="secondary-button" onClick={() => cancelTargetLookup()}>Cancel</button>
    </p>
  );

  // Priority order: the first present strip is the only one the status slot shows.
  const statusStrips: Array<ContactsStatusStrip | false | null | undefined> = [
    !detailOverlay && unsavedBanner ? { kind: "unsaved", node: unsavedBanner } : null,
    !detailOverlay && !restore && errorBanner ? { kind: "error", node: errorBanner } : null,
    book?.sourceRevoked ? { kind: "removed", node: <RemovedBookStrip book={book} onRemoveRequest={requestRemoveBook} /> } : null,
    // The sync strip carries the completed-scan wording too, so a finished scan is not hidden behind the repair state.
    contactsRepairState ? { kind: "sync", node: <SyncStatusStrip state={contactsRepairState} text={scanStatus?.state === "completed" && completedScanText ? completedScanText : contactsSyncText} /> } : null,
    sync?.readiness && (sync.readiness.state !== "ready" || !sync.readiness.server_active) ? { kind: "readiness", node: (
      <p id="contacts-readiness-status" role="status" data-banner-state={sync.readiness.state}>
        {sync.readiness.state === "server_unsupported" ? "Contact changes need a server that supports contact sync." : sync.readiness.state === "needs_unlock" ? "Contact changes are paused until all required keys are unlocked." : sync.readiness.state === "backfill_pending" ? "Contact changes are preparing historical records." : "Contact sync is ready."}
        {sync.readiness.backfill_unreadable > 0 && " Some historical records remain unreadable."}
        {!sync.readiness.server_active && " History retention is paused until every device is ready."}
      </p>
    ) } : null,
    scanStatus ? { kind: "scan", node: (
      <p id="contacts-scan-status" role="status" data-scan-state={scanStatus.state}>
        {scanStatusText(scanStatus)}
      </p>
    ) } : null,
    syncDisabledReason && !book?.sourceRevoked ? { kind: "unavailable", node: <p id="contacts-sync-unavailable" role="status">{syncDisabledReason}</p> } : null,
  ];

  const activeStatus = activeContactsStatus(statusStrips);
  const headerActions = (
    <div id="contacts-header-actions" role="toolbar" aria-label="Contact actions">
      <div className="contacts-sort-control">
        <button ref={sortButtonRef} type="button" aria-label={viewLabel} title={noBooks ? noBooksTitle : "View options"} disabled={noBooks} aria-haspopup="menu" aria-expanded={sortOpen} aria-controls="contacts-sort-menu" onClick={() => setSortOpen(open => !open)} onKeyDown={event => { if (event.key === "ArrowDown") { event.preventDefault(); setSortOpen(true); } }}><ListFilter size={16} aria-hidden /></button>
      </div>
      <button ref={restoreTriggerRef} type="button" aria-label="Recently deleted contacts" title={noBooks ? noBooksTitle : !book ? selectBookTitle : detailOverlay || editing ? closeDetailTitle : "Recently deleted"} disabled={!book || detailOverlay || Boolean(editing)} onClick={openRecentlyDeleted}><Clock size={16} aria-hidden /></button>
      <button id="contacts-sync-now" type="button" aria-label={syncing ? "Requesting…" : "Sync now"} title={noBooks ? noBooksTitle : pausedTitle ?? syncDisabledReason ?? "Sync now"} aria-busy={syncing} aria-describedby={pausedDescribedBy ?? (activeStatus?.kind === "unavailable" ? "contacts-sync-unavailable" : undefined)} disabled={!book || writesPaused || Boolean(syncDisabledReason) || syncing} onClick={requestSync}><RotateCw className={syncing ? "contacts-sync-icon" : undefined} size={16} aria-hidden /></button>
      <button ref={newContactTriggerRef} type="button" data-action="create-contact" aria-label="New contact" title={noBooks ? noBooksTitle : pausedTitle ?? (book ? "New contact" : selectBookTitle)} aria-describedby={pausedDescribedBy} disabled={!book || writesPaused} onClick={() => handleEditStart()}><UserPlus size={16} strokeWidth={1.75} aria-hidden /></button>
    </div>
  );

  const editorToneSeed = editing && !isCreateEditing(editing)
    ? (() => { const identity = contactSeed(editing.phones, editing.id); return identity.startsWith("contact:") ? undefined : identity; })()
    : undefined;

  return (
    <section ref={contactsViewRef} id="contacts-view" role="region" aria-label="Contacts browser" data-collapsed-reason={collapsedReason}>
      <h1 className="visually-hidden">Contacts</h1>
      {sortMenu && (sortPortalHost ? createPortal(sortMenu, sortPortalHost) : sortMenu)}
      {bookRemoval && (
        <RemoveBookDialog book={bookRemoval.book} opener={bookRemoval.opener} onDismiss={() => setBookRemoval(undefined)} onRemoved={handleBookRemoved} onNotice={setActionNotice} />
      )}
      {railVisible && !noBooks && (
        <BookRail
          books={books}
          loading={!booksLoaded}
          bookId={bookId}
          width={bookWidth}
          panelRef={booksPanelRef}
          onSelect={handleSelectBook}
          onBooksChanged={loadBooks}
          onRemoveRequest={requestRemoveBook}
          onNotice={setActionNotice}
          lookupFailure={lookupFailure}
        />
      )}
      {!compactBookList && !noBooks && (
        <ResizeHandle
          id="contacts-book-resize"
          direction="horizontal"
          ariaLabel="Resize contact books"
          value={bookWidth}
          min={BOOK_MIN_WIDTH}
          max={BOOK_MAX_WIDTH}
          valueUnit="pixels"
          onResizeStart={startBookResize}
          onResize={resizeBook}
          onResizeTo={sizeBook}
          onResizeEnd={endBookResize}
          onDoubleClick={toggleBookCollapsed}
          collapsed={railVisible ? undefined : "before"}
          collapsible={{ side: "before", restoreValue: bookWidth }}
        />
      )}

      <section id="contacts-center-pane" {...(detailOverlay ? { inert: true } : {})}>
        <ContactsActionNotice notice={actionNotice} onDismiss={() => setActionNotice(undefined)} />
        <ContactListHeader books={books.map(item => item.id === bookId ? { ...item, contactCount: pages.total ?? item.contactCount } : item)} bookId={bookId} booksLoaded={booksLoaded} railVisible={railVisible} loaded={contacts.length} loadingMore={pages.loadingMore} query={pages.query} matchCount={pages.query ? pages.total : undefined} onSelectBook={handleSelectBook}>
          {headerActions}
        </ContactListHeader>
        {!railVisible && book && (books.length >= 2 || book.lookupExcluded) && (
          <BookLookupStrip book={book} books={books} onBooksChanged={loadBooks} onNotice={setActionNotice} lookupFailure={lookupFailure} />
        )}
        {!detailOverlay && !restore && noticeBanner}
        {!detailOverlay && targetStatus}
        {!detailOverlay && targetLoadingStatus}
        {pauseNoticeVisible && <p id="contacts-update-paused" role="status">{UPDATE_CONTACTS_NOTE}</p>}
        <ContactsStatus strips={statusStrips} />

        {restore ? (
          <RestorePanel
            list={restore.items}
            writesPaused={writesPaused}
            canRestore={Boolean(books.find(candidate => candidate.id === restore.bookId)?.capabilities.canRestore)}
            notice={noticeBanner}
            error={errorBanner}
            onClose={() => { setRestore(null); setError(""); restoreTriggerRef.current?.focus(); }}
            onRestore={async value => {
              const restoreBookId = restore.bookId;
              try {
                const outcome = await bridge.restore_contact(restoreBookId, value.id);
                if (currentBookRef.current !== restoreBookId) return;
                if (outcome.state === "rejected") {
                  setError(outcome.reason);
                } else if (outcome.state === "conflict") {
                  setError(`Conflict: ${outcome.conflictSummary}`);
                } else {
                  setRestore(current => current ? { ...current, items: current.items.filter(item => item.id !== value.id) } : null);
                  if (outcome.state === "pending") setNotice(<span>Restore of <IsolatedText text={value.displayName} /> sent. Waiting for the phone.</span>);
                  pages.refresh();
                }
              } catch (e) {
                setError(contactFailure(e));
              }
            }}
          />
        ) : (
          <ContactList
            contacts={contacts}
            loading={pages.loading || !booksLoaded}
            noBooks={noBooks}
            loadingMore={pages.loadingMore}
            loadMoreError={pages.loadMoreError}
            hasMore={pages.hasMore}
            loadFailed={pages.loadFailed}
            contactCount={pages.total ?? book?.contactCount}
            deviceName={book?.deviceName}
            deviceLabel={book && bookLabels(books).label(book)}
            canWrite={Boolean(book?.capabilities.canWrite)}
            emptyFailed={contactsRepairState === "failed" || contactsRepairState === "repairWaiting"}
            nameFormat={nameFormat}
            selectedId={selected?.id}
            query={pages.query}
            onSelect={handleSelectContact}
            onLoadMore={pages.loadMore}
            onRetryLoadMore={pages.retryLoadMore}
            onRetryFirstPage={pages.retryFirstPage}
            onClearQuery={() => onClearQuery?.()}
          />
        )}
      </section>

      {(selected || editing) && (
        <>
          <ResizeHandle id="contacts-detail-resize" direction="horizontal" ariaLabel="Resize contact detail" value={renderedDetailWidth} min={detailMinWidth} max={detailMaxWidth} valueUnit="pixels" onResizeStart={() => { detailDragActive.current = true; detailDragWidth.current = renderedDetailWidth; }} onResize={resizeDetail} onResizeTo={setDetailSize} onResizeEnd={() => { detailDragActive.current = false; persistLayoutPatch({ contactsDetailWidth: detailDragWidth.current }); }} onDoubleClick={resetDetailWidth} />
          <aside id="contacts-detail-panel" role="region" aria-label="Contact detail" style={{ "--contacts-detail-width": `${renderedDetailWidth}px` } as CSSProperties}>
            {compactDetail && (
              <button ref={detailBackRef} id="contacts-detail-back" type="button" className="secondary-button" onClick={handleDetailBack} disabled={busy}>
                Back
              </button>
            )}
            {detailOverlay && unsavedBanner}
            {detailOverlay && noticeBanner}
            {detailOverlay && errorBanner}
            {detailOverlay && targetStatus}
            {detailOverlay && targetLoadingStatus}
            {editing ? (
              <ContactForm
                key={editing === "create" ? "create" : isCreateEditing(editing) ? `create:${editing.requestId}` : editing.id}
                initial={isCreateEditing(editing) ? undefined : editing}
                createPrefill={editing !== "create" && isCreateEditing(editing) ? editing.createPrefill : undefined}
                onCancel={handleEditCancel}
                onSave={handleSave}
                onDirtyChange={setDirty}
                error={error}
                validationError={validationError}
                busy={busy}
                bookCapabilities={books.find(candidate => candidate.id === editorBookId)?.capabilities}
                saveDisabled={!books.some(candidate => candidate.id === editorBookId)}
                writesPaused={writesPaused}
                toneControl={isCreateEditing(editing) ? undefined : (() => {
                  return { value: editorToneSeed ? toneOverrides[editorToneSeed] : undefined, pending: Boolean(editorToneSeed && pendingToneSeeds[editorToneSeed]), error: editorToneSeed ? toneErrors[editorToneSeed] : undefined, disabled: toneDisabled, onSelect: (index: number | null) => { if (editorToneSeed) onSelectTone?.(editorToneSeed, index); } };
                })()}
                savedToneSeed={editorToneSeed}
              />
            ) : (
              selected && (
                <ContactDetail
                  key={selected.id}
                  contact={selected}
                  canWrite={Boolean(book?.capabilities.canWrite)}
                  nameFormat={nameFormat}
                  book={books.find(candidate => candidate.id === selected.bookId)}
                  photoLocked={toneDisabled || sync?.readiness?.state === "needs_unlock"}
                  writesPaused={writesPaused}
                  onPhotoSubmitted={() => pages.refresh()}
                  busy={busy}
                  confirm={confirmDelete}
                  onEdit={() => handleEditStart(selected)}
                  onDelete={handleDelete}
                  onCancelConfirm={handleCancelConfirm}
                  onClose={handleDetailBack}
                />
              )
            )}
          </aside>
        </>
      )}
    </section>
  );
}
