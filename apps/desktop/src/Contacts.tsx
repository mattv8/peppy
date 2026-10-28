import { useCallback, useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { Check, ContactAvatar, Clock, ListFilter, ResizeHandle, RotateCw, Smartphone, UserPlus, X, contactSeed, formatPhoneNumber } from "@peppy/desktop-ui";
import { bridge, type ContactBookView, type ContactScanStatus, type ContactSort, type ContactSyncStatus, type ContactView, type RestorableContact } from "./bridge";
import { ContactForm, type ContactEditDraft } from "./ContactForm";
import { CONTACTS_PAGE_SIZE, findContactTarget } from "./contact-target";
import { persistLayoutPatch, readContactsDetailWidth } from "./layout-persistence";
import "./contacts.css";

/** Core returns at most this many contacts per page (search runs before paging). */
export { CONTACTS_PAGE_SIZE } from "./contact-target";

const EDIT_STATE_LABELS: Record<string, string> = {
  pending: "Waiting for the phone",
  "awaiting-approval": "Waiting for approval on the phone",
  "outcome-unknown": "Outcome unknown — check the phone",
  conflict: "Conflict — changed on the phone",
  rejected: "Rejected by the phone",
  failed: "Failed on the phone",
  expired: "Expired before the phone applied it",
};
export const editStateLabel = (state: string) => EDIT_STATE_LABELS[state] ?? state;
const SORT_STORAGE_KEY = "peppy.contacts.sort.v1";
const DEFAULT_SORT: ContactSort = { key: "family", direction: "asc" };
const DETAIL_DEFAULT_WIDTH = 320;
const DETAIL_MIN_WIDTH = 280;
const DETAIL_MAX_WIDTH = 480;
const CONTACTS_LIST_MIN_WIDTH = 240;
const DETAIL_RESIZE_HANDLE_WIDTH = 1;

function readSortPreference(): ContactSort {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(SORT_STORAGE_KEY) ?? "null");
    return typeof value === "object" && value !== null && (value as ContactSort).key !== undefined && (value as ContactSort).direction !== undefined
      && ["given", "family"].includes((value as ContactSort).key) && ["asc", "desc"].includes((value as ContactSort).direction)
      ? { key: (value as ContactSort).key, direction: (value as ContactSort).direction } : DEFAULT_SORT;
  } catch { return DEFAULT_SORT; }
}

const contactErrorText = (error: unknown): string =>
  error instanceof Error
    ? error.message
    : typeof error === "object" && error !== null && "message" in error && typeof error.message === "string"
      ? error.message
      : String(error);

const scanReasonText = (reason?: string) => ({ permission_denied: "The phone has not allowed contact access.", limited_access: "The phone has limited contact access.", sync_disabled: "Contact sync is disabled on the phone.", source_revoked: "The source device was removed.", source_retired: "This book is retired.", needs_unlock: "Unlock sync before requesting a scan.", stale_epoch: "The phone's contact context changed before the scan. Use Sync now to request a fresh scan.", superseded_generation: "A newer contact book version replaced this scan. Use Sync now to request a fresh scan.", incomplete_scan: "The phone's scan did not finish. Use Sync now to try again.", cancelled: "The phone cancelled this scan. Use Sync now to try again." }[reason ?? ""] ?? "The phone could not complete this scan.");
const scanStatusText = (status: ContactScanStatus) => ({
  queued: "Scan request queued locally.", waiting: "Waiting for the phone to scan contacts.", scanning: "The phone is scanning contacts.", completed: "Scan completed.", blocked: `Scan blocked. ${scanReasonText(status.reason)}`, failed: `Scan failed. ${scanReasonText(status.reason)}`, expired: "Scan request expired. Request another scan.",
}[status.state]);

export type ContactNavigationGuard = (navigate: () => void, onCancel?: () => void) => boolean | void;
export type ContactTarget = { bookId: string; contactId: string; requestId?: number };

export function ContactsView({ books: initialBooks = [], sync, onNavigationGuard, target, onTargetConsumed, headerHost }: {
  books?: ContactBookView[];
  sync?: ContactSyncStatus;
  onNavigationGuard?(guard: ContactNavigationGuard | null): void;
  target?: ContactTarget | null;
  onTargetConsumed?(requestId: number): void;
  headerHost?: Element | null;
}) {
  const [books, setBooks] = useState(initialBooks);
  const [booksLoaded, setBooksLoaded] = useState(initialBooks.length > 0);
  const [bookId, setBookId] = useState(initialBooks[0]?.id ?? "");
  const [contacts, setContacts] = useState<ContactView[]>([]);
  const [selected, setSelected] = useState<ContactView | null>(null);
  const [sort, setSort] = useState<ContactSort>(readSortPreference);
  const [sortOpen, setSortOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [restore, setRestore] = useState<RestorableContact[] | null>(null);
  const [editing, setEditing] = useState<ContactView | null | "create">(null);
  const [error, setError] = useState("");
  const [validationError, setValidationError] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState("");
  const [offset, setOffset] = useState(0);
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
  const sortButtonRef = useRef<HTMLButtonElement>(null);
  const sortMenuRef = useRef<HTMLDivElement>(null);
  const [sortMenuPosition, setSortMenuPosition] = useState<CSSProperties>();
  const restoreTriggerRef = useRef<HTMLButtonElement>(null);
  // Only the newest list request may apply its result (book/search/page changes and live hints).
  const pageRequestRef = useRef(0);
  const scanStatusRequestRef = useRef(0);
  const syncActionRef = useRef(0);
  const currentBookRef = useRef(bookId);
  const booksRef = useRef(books);
  const booksLoadedRef = useRef(initialBooks.length > 0);
  const busyRef = useRef(busy);
  const dirtyRef = useRef(dirty);
  const editingRef = useRef(editing);
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
  pendingNavigationRef.current = pendingNavigation;

  const replacePendingNavigation = (next: typeof pendingNavigation) => {
    const previous = pendingNavigationRef.current;
    if (previous !== next) previous?.onCancel?.();
    pendingNavigationRef.current = next;
    setPendingNavigation(next);
  };

  const targetKey = (value: ContactTarget) => value.requestId === undefined
    ? `${value.bookId}:${value.contactId}`
    : `request:${value.requestId}`;
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
        booksLoadedRef.current = true;
        setBooksLoaded(true);
        const currentBookId = currentBookRef.current;
        if (currentBookId && next.some(book => book.id === currentBookId)) return;
        const fallbackBookId = next[0]?.id ?? "";
        if (fallbackBookId === currentBookId) return;
        currentBookRef.current = fallbackBookId;
        setOffset(0);
        setBookId(fallbackBookId);
      })
      .catch(() => {
        if (!mountedRef.current) return;
        // A background refresh failure keeps the loaded books and any active target walk.
        if (booksLoadedRef.current) {
          setError("Contacts could not be loaded right now.");
          return;
        }
        booksLoadedRef.current = true;
        setBooksLoaded(true);
        const currentTarget = targetRef.current;
        if (!currentTarget || completedTargetRef.current === targetKey(currentTarget)) {
          setError("Contacts could not be loaded right now.");
          return;
        }
        setTargetError("The requested contact could not be opened because contact books could not be loaded.");
        consumeTarget(currentTarget);
      });
  }, []);

  const loadPage = useCallback((showLoading: boolean) => {
    if (!bookId) return;
    const request = ++pageRequestRef.current;
    if (showLoading) setLoading(true);
    void bridge
      .list_contacts(bookId, "", offset, sort)
      .then(results => {
        if (!mountedRef.current || request !== pageRequestRef.current) return;
        setContacts(results);
        // Keep the open detail current; an open editor keeps its unsaved draft untouched.
        setSelected(current => (current ? results.find(item => item.id === current.id) ?? current : current));
      })
      .catch(e => {
        if (mountedRef.current && request === pageRequestRef.current) setError(String(e?.message ?? e));
      })
      .finally(() => {
        if (mountedRef.current && request === pageRequestRef.current) setLoading(false);
      });
  }, [bookId, offset, sort]);

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
    if (!view || !booksPanel) return;
    const measure = () => setContactsLayout({ viewWidth: view.clientWidth || window.innerWidth, bookWidth: booksPanel.offsetWidth });
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(view);
    observer.observe(booksPanel);
    return () => observer.disconnect();
  }, []);
  useLayoutEffect(() => {
    if (!sortOpen || !sortButtonRef.current) return;
    const positionMenu = () => {
      const rect = sortButtonRef.current!.getBoundingClientRect();
      const menuWidth = 172;
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
    setOffset(0);
    try { localStorage.setItem(SORT_STORAGE_KEY, JSON.stringify(next)); } catch { /* Preferences are optional. */ }
  };

  useEffect(loadBooks, [loadBooks]);
  useEffect(() => loadPage(true), [loadPage]);

  const book = books.find(item => item.id === bookId);

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
    if (!scanStatus || !["queued", "waiting", "scanning"].includes(scanStatus.state)) return;
    const interval = window.setInterval(refreshScanStatus, 5_000);
    return () => window.clearInterval(interval);
  }, [refreshScanStatus, scanStatus]);
  // Native state hints refresh the visible book without replacing unsaved editor state.
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const dispose = bridge.subscribe(() => {
      clearTimeout(timer);
      timer = setTimeout(() => { loadBooks(); loadPage(false); refreshScanStatus(); }, 150);
    });
    return () => { clearTimeout(timer); dispose(); };
  }, [loadBooks, loadPage, refreshScanStatus]);

  const handleSave = async (draft: ContactEditDraft) => {
    if (!book || !editing) return;
    setError("");
    setValidationError("");
    setBusy(true);

    try {
      const isCreate = editing === "create";
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
        targetBookId: book.id,
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
      setDirty(false);
      setSelected(null);
      setError("");
      setValidationError("");
      setNotice(outcome.state === "pending" ? `Sent to ${book.deviceName}. Waiting for the phone to apply it.` : "");
      loadPage(false);
      const pending = pendingNavigationRef.current;
      if (pending) {
        pendingNavigationRef.current = null;
        setPendingNavigation(null);
        pending.action();
      }
    } catch (e) {
      const message = contactErrorText(e);
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

    setError("");
    setBusy(true);

    try {
      const outcome = await bridge.submit_contact_edit({
        targetBookId: book.id,
        kind: "delete",
        contactId: selected.id,
        baseRevision: selected.revision,
        patches: [],
        photo: { kind: "keep" },
      });

      if (outcome.state === "rejected") {
        setError(outcome.reason);
        setBusy(false);
        return;
      }

      if (outcome.state === "conflict") {
        setError(`Conflict: ${outcome.conflictSummary}`);
        setBusy(false);
        return;
      }

      setSelected(null);
      setConfirmDelete(false);
      setError("");
      setNotice(outcome.state === "pending" ? `Delete sent to ${book.deviceName}. Waiting for the phone.` : "");
      loadPage(false);
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
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
    if (!requestedTarget || !booksLoaded || targetKey(requestedTarget) !== activeTargetKey) return;
    if (completedTargetRef.current === activeTargetKey) return;
    setTargetError("");
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
        const result = await findContactTarget(requestedTarget, bridge.list_contacts, controller.signal, (bookId) => booksRef.current.some((book) => book.id === bookId), sortRef.current);
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
          const { contact, offset: pageOffset, page: results } = result;
          setTargetError("");
          ++pageRequestRef.current;
          setLoading(false);
          if (currentBookRef.current !== requestedTarget.bookId) {
            ++scanStatusRequestRef.current;
            ++syncActionRef.current;
            setScanStatus(null);
            setSyncing(false);
          }
          currentBookRef.current = requestedTarget.bookId;
          setBookId(requestedTarget.bookId);
          setOffset(pageOffset);
          setContacts(results);
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
    if (!focusTargetId) return;
    const frame = requestAnimationFrame(() => {
      const row = Array.from(document.querySelectorAll<HTMLButtonElement>("button[data-contact-id]"))
        .find(node => node.dataset.contactId === focusTargetId);
      row?.scrollIntoView({ block: "nearest" });
      row?.focus();
      setFocusTargetId(null);
    });
    return () => cancelAnimationFrame(frame);
  }, [focusTargetId]);

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
      setOffset(0);
      setScanStatus(null);
      setSyncing(false);
      setSelected(null);
      setEditing(null);
      setConfirmDelete(false);
      setError("");
    });
  };

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
    setEditing(contact ?? "create");
    setDirty(false);
    setConfirmDelete(false);
    setError("");
    setValidationError("");
    });
  };

  const handleEditCancel = () => requestNavigation(() => {
    setEditing(null);
    setDirty(false);
    setError("");
    setValidationError("");
  });

  const syncDisabledReason = !book
    ? "Select a contact book to request a scan."
    : book.sourceRevoked ? "This source device was removed."
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
        if (mountedRef.current && action === syncActionRef.current && currentBookRef.current === requestedBook) setError(contactErrorText(error));
      })
      .finally(() => {
        if (mountedRef.current && action === syncActionRef.current && currentBookRef.current === requestedBook) setSyncing(false);
      });
  };

  const openRecentlyDeleted = () => {
    setConfirmDelete(false);
    void bridge.list_restorable_contacts(bookId).then(setRestore).catch(e => setError(String(e.message ?? e)));
  };
  const sortLabel = `Sort: ${sort.key === "family" ? "Last name" : "First name"}, ${sort.direction === "asc" ? "ascending" : "descending"}`;
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
  const sortMenu = sortOpen && (
    <div ref={sortMenuRef} id="contacts-sort-menu" role="menu" aria-label="Sort contacts" style={sortMenuPosition} onKeyDown={handleSortMenuKeyDown}>
      <div role="group" aria-label="Sort by">{(["family", "given"] as const).map(key => <button id={`contacts-sort-key-${key}`} type="button" role="menuitemradio" aria-checked={sort.key === key} key={key} onClick={() => selectSort({ ...sort, key })}><span className="contacts-sort-check" aria-hidden="true">{sort.key === key && <Check size={14} aria-hidden />}</span><span className="contacts-sort-label">{key === "family" ? "Last name" : "First name"}</span></button>)}</div>
      <div role="group" aria-label="Direction">{(["asc", "desc"] as const).map(direction => <button id={`contacts-sort-direction-${direction}`} type="button" role="menuitemradio" aria-checked={sort.direction === direction} key={direction} onClick={() => selectSort({ ...sort, direction })}><span className="contacts-sort-check" aria-hidden="true">{sort.direction === direction && <Check size={14} aria-hidden />}</span><span className="contacts-sort-label">{direction === "asc" ? "Ascending" : "Descending"}</span></button>)}</div>
    </div>
  );
  const sortPortalHost = headerHost?.closest<HTMLElement>("#desktop-shell, #composer-shell") ?? (typeof document === "undefined" ? null : document.body);
  const selectBookTitle = "Select a contact book first.";
  const headerActions = (
    <div id="contacts-header-actions" role="toolbar" aria-label="Contact actions">
      <div className="contacts-sort-control">
        <button ref={sortButtonRef} type="button" aria-label={sortLabel} title={sortLabel} aria-haspopup="menu" aria-expanded={sortOpen} aria-controls="contacts-sort-menu" onClick={() => setSortOpen(open => !open)} onKeyDown={event => { if (event.key === "ArrowDown") { event.preventDefault(); setSortOpen(true); } }}><ListFilter size={16} aria-hidden /></button>
      </div>
      <button ref={restoreTriggerRef} type="button" aria-label="Recently deleted contacts" title={book ? "Recently deleted" : selectBookTitle} disabled={booksLoaded && !book} onClick={openRecentlyDeleted}><Clock size={16} aria-hidden /></button>
      <button id="contacts-sync-now" type="button" aria-label={syncing ? "Requesting…" : "Sync now"} title={syncDisabledReason ?? "Sync now"} aria-busy={syncing} aria-describedby={syncDisabledReason ? "contacts-sync-unavailable" : undefined} disabled={Boolean(syncDisabledReason) || syncing} onClick={requestSync}><RotateCw className={syncing ? "contacts-sync-icon" : undefined} size={16} aria-hidden /></button>
      <button type="button" data-action="create-contact" aria-label="New contact" title={book ? "New contact" : selectBookTitle} disabled={booksLoaded && !book} onClick={() => handleEditStart()}><UserPlus size={16} strokeWidth={1.75} aria-hidden /></button>
    </div>
  );
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

  return (
    <section ref={contactsViewRef} id="contacts-view" role="region" aria-label="Contacts browser">
      {sortMenu && (sortPortalHost ? createPortal(sortMenu, sortPortalHost) : sortMenu)}
      <aside ref={booksPanelRef} id="contacts-book-list" aria-label="Contact books">
        <ul id="contacts-books">
          {books.map(item => (
            <li key={item.id}>
              <button
                data-book-id={item.id}
                className="contacts-book-row"
                aria-current={item.id === bookId ? "location" : undefined}
                onClick={() => handleSelectBook(item.id)}
              >
                  <Smartphone aria-hidden="true" size={16} strokeWidth={1.75} />
                <span>
                  <b>{item.deviceName}</b>
                  <small data-book-state={item.state}>
                    {item.sourceRevoked ? "Source device removed" : item.state === "active" ? "Active" : item.state === "limited" ? "Limited access" : item.state === "retired" ? "Retired" : "Unavailable"}
                    {item.state === "active" ? ` · ${item.contactCount}` : ""}
                    {item.defaultAccountLabel && ` · ${item.defaultAccountLabel}`}
                  </small>
                </span>
              </button>
            </li>
          ))}
        </ul>
      </aside>

      <section id="contacts-center-pane">
        {pendingNavigation && (
          <p id="contacts-unsaved-changes" role="status" data-banner-state="unsaved-changes">
            You have unsaved changes.
            <button type="button" className="secondary-button" onClick={() => replacePendingNavigation(null)}>Keep editing</button>
            <button type="button" className="secondary-button" onClick={discardNavigation} disabled={busy}>Discard</button>
          </p>
        )}
        {sync?.readiness && (sync.readiness.state !== "ready" || !sync.readiness.server_active) && (
          <p id="contacts-readiness-status" role="status" data-banner-state={sync.readiness.state}>
            {sync.readiness.state === "server_unsupported" ? "Contact changes need a server that supports contact sync." : sync.readiness.state === "needs_unlock" ? "Contact changes are paused until all required keys are unlocked." : sync.readiness.state === "backfill_pending" ? "Contact changes are preparing historical records." : "Contact sync is ready."}
            {sync.readiness.backfill_unreadable > 0 && " Some historical records remain unreadable."}
            {!sync.readiness.server_active && " History retention is paused until every device is ready."}
          </p>
        )}
        {(sync?.repairRequired || sync?.projection?.state === "failed" || sync?.projection?.state === "rebuilding") && (
          <p id="contacts-sync-status" role="status" data-sync-state={sync.projection?.state ?? "repair"}>
            {sync.projection?.state === "rebuilding"
              ? "Rebuilding contacts from the server…"
              : sync.projection?.state === "failed"
                ? `Contacts may be out of date (rebuild failed${sync.projection.reason ? `: ${sync.projection.reason.replace(/_/g, " ")}` : ""}).`
                : "Contacts need a repair from the server."}
          </p>
        )}
        {notice && (
          <p id="contacts-notice" role="status" data-banner-state="notice">
            {notice}
            <button aria-label="Dismiss" onClick={() => setNotice("")}>
              <X size={12} />
            </button>
          </p>
        )}
        {targetError && <p id="contacts-target-error" role="status" data-target-state="error">{targetError}</p>}
        {targetLoading && (
          <p id="contacts-target-status" role="status" data-target-state="loading">
            Finding requested contact… <button type="button" className="secondary-button" onClick={() => cancelTargetLookup()}>Cancel</button>
          </p>
        )}
        {headerHost ? createPortal(headerActions, headerHost) : <header id="contacts-center-toolbar">{headerActions}</header>}
        {syncDisabledReason && <p id="contacts-sync-unavailable" role="status">{syncDisabledReason}</p>}
        {scanStatus && <p id="contacts-scan-status" role="status" data-scan-state={scanStatus.state}>
          {scanStatus.state === "completed" && (sync?.repairRequired || sync?.projection?.state === "failed" || sync?.projection?.state === "rebuilding")
            ? "The phone finished its scan. Contacts are still being repaired locally."
            : scanStatusText(scanStatus)}
        </p>}

        {restore ? (
          <Restore
            list={restore}
            canRestore={Boolean(book?.capabilities.canRestore)}
            onClose={() => { setRestore(null); restoreTriggerRef.current?.focus(); }}
            onRestore={async value => {
              try {
                const outcome = await bridge.restore_contact(bookId, value.id);
                if (outcome.state === "rejected") {
                  setError(outcome.reason);
                } else if (outcome.state === "conflict") {
                  setError(`Conflict: ${outcome.conflictSummary}`);
                } else {
                  setRestore(all => all?.filter(item => item.id !== value.id) ?? null);
                  if (outcome.state === "pending") setNotice(`Restore of ${value.displayName} sent. Waiting for the phone.`);
                  loadPage(false);
                }
              } catch (e) {
                setError(String(e instanceof Error ? e.message : e));
              }
            }}
          />
        ) : (
          <ul id="contacts-list" aria-busy={loading}>
            {contacts.map(contact => (
              <li key={contact.id}>
                <button
                  data-contact-id={contact.id}
                  className="contacts-row"
                  aria-label={[contact.displayName, contact.phones[0]?.number && formatPhoneNumber(contact.phones[0].number), contact.pendingEditState && editStateLabel(contact.pendingEditState)].filter(Boolean).join(", ")}
                  aria-current={selected?.id === contact.id ? "location" : undefined}
                  onClick={() => handleSelectContact(contact)}
                >
                  <ContactAvatar
                    seed={contactSeed(contact.phones, contact.id)}
                    name={contact.displayName}
                    photoDataUrl={contact.photoDataUrl}
                  />
                  <span className="contacts-row-copy">
                    <b>{contact.displayName}</b>
                    <small>{contact.phones[0]?.number ? formatPhoneNumber(contact.phones[0].number) : ""}</small>
                  </span>
                  {contact.pendingEditState && (
                    <span
                      className="contacts-row-state-badge"
                      data-state={contact.pendingEditState}
                    >
                      <span className="sr-only">{editStateLabel(contact.pendingEditState)}</span>
                    </span>
                  )}
                </button>
              </li>
            ))}
            {!loading && !contacts.length && (
              <li id="contacts-empty-state">{offset ? "No more contacts" : "No contacts in this book"}</li>
            )}
          </ul>
        )}
        {!restore && (offset > 0 || contacts.length >= CONTACTS_PAGE_SIZE) && (
          <nav id="contacts-pager" aria-label="Contact pages">
            <button
              data-action="contacts-previous-page"
              disabled={offset === 0 || loading}
              onClick={() => { cancelTargetLookup(); setOffset(current => Math.max(0, current - CONTACTS_PAGE_SIZE)); }}
            >
              Previous
            </button>
            <span>{`${offset + 1}–${offset + contacts.length}`}</span>
            <button
              data-action="contacts-next-page"
              disabled={contacts.length < CONTACTS_PAGE_SIZE || loading}
              onClick={() => { cancelTargetLookup(); setOffset(current => current + CONTACTS_PAGE_SIZE); }}
            >
              Next
            </button>
          </nav>
        )}
      </section>

      {(selected || editing) && (
        <>
          <ResizeHandle id="contacts-detail-resize" direction="horizontal" ariaLabel="Resize contact detail" value={renderedDetailWidth} min={detailMinWidth} max={detailMaxWidth} valueUnit="pixels" onResizeStart={() => { detailDragActive.current = true; detailDragWidth.current = renderedDetailWidth; }} onResize={resizeDetail} onResizeTo={setDetailSize} onResizeEnd={() => { detailDragActive.current = false; persistLayoutPatch({ contactsDetailWidth: detailDragWidth.current }); }} onDoubleClick={resetDetailWidth} />
          <aside id="contacts-detail-panel" role="region" aria-label="Contact detail" style={{ "--contacts-detail-width": `${renderedDetailWidth}px` } as CSSProperties}>
          {editing ? (
            <ContactForm
              initial={editing === "create" ? undefined : editing}
              onCancel={handleEditCancel}
              onSave={handleSave}
              onDirtyChange={setDirty}
              error={error}
              validationError={validationError}
              busy={busy}
              bookCapabilities={book?.capabilities}
            />
          ) : (
            selected && (
              <Detail
                contact={selected}
                canWrite={Boolean(book?.capabilities.canWrite)}
                confirm={confirmDelete}
                onEdit={() => handleEditStart(selected)}
                onDelete={handleDelete}
                onCancelConfirm={handleCancelConfirm}
              />
            )
          )}
          </aside>
        </>
      )}
    </section>
  );
}

function Detail({
  contact,
  canWrite,
  confirm,
  onEdit,
  onDelete,
  onCancelConfirm,
}: {
  contact: ContactView;
  canWrite: boolean;
  confirm: boolean;
  onEdit(): void;
  onDelete(): void;
  onCancelConfirm(): void;
}) {
  return (
    <>
      {contact.pendingEditState && (
        <section data-contact-banner={contact.pendingEditState} role="status">
          {contact.pendingEditSummary ?? "Change"} — {editStateLabel(contact.pendingEditState)}
        </section>
      )}
      <header id="contacts-detail-header">
        <ContactAvatar
          seed={contactSeed(contact.phones, contact.id)}
          name={contact.displayName}
          photoDataUrl={contact.photoDataUrl}
          size={64}
        />
        <div>
          <h1>{contact.displayName}</h1>
          <small>{[contact.organization, contact.title].filter(Boolean).join(" · ")}</small>
        </div>
        {canWrite && (
          <div>
            <button
              data-action="edit-contact"
              aria-label="Edit contact"
              onClick={onEdit}
            >
              Edit
            </button>
            <button
              data-action="delete-contact"
              data-confirm-delete={confirm ? "pending" : undefined}
              aria-label={confirm ? "Confirm delete" : "Delete contact"}
              onKeyDown={e => { if (e.key === "Escape" && confirm) { e.preventDefault(); onCancelConfirm(); } }}
              onBlur={confirm ? onCancelConfirm : undefined}
              onClick={onDelete}
            >
              {confirm ? "Confirm delete" : "Delete"}
            </button>
          </div>
        )}
      </header>
      {contact.phones.length > 0 && (
        <section data-contact-section="phones">
          <h3>Phone numbers</h3>
          {contact.phones.map(phone => (
            <p key={`${phone.label}-${phone.number}`}>
              {phone.displayLabel ?? phone.label} · {formatPhoneNumber(phone.number)}
            </p>
          ))}
        </section>
      )}
      {contact.emails.length > 0 && (
        <section data-contact-section="emails">
          <h3>Email</h3>
          {contact.emails.map(email => (
            <p key={email.address}>
              {email.displayLabel ?? email.label} · {email.address}
            </p>
          ))}
        </section>
      )}
      {contact.addresses.length > 0 && (
        <section data-contact-section="addresses">
          <h3>Addresses</h3>
          {contact.addresses.map((addr, idx) => (
            <p key={`${addr.label ?? ""}-${addr.street ?? ""}-${addr.city ?? ""}`}>
              {addr.displayLabel ?? addr.label} · {[addr.street, addr.city, addr.state, addr.postalCode, addr.country]
                .filter(Boolean)
                .join(", ")}
            </p>
          ))}
        </section>
      )}
      {contact.organization && (
        <section data-contact-section="organization">
          <h3>Organization</h3>
          <p>{contact.organization}</p>
        </section>
      )}
      {contact.title && (
        <section data-contact-section="title">
          <h3>Title</h3>
          <p>{contact.title}</p>
        </section>
      )}
      {contact.birthday && (
        <section data-contact-section="birthday">
          <h3>Birthday</h3>
          <p>
            {contact.birthday.month}/{contact.birthday.day}
            {contact.birthday.year && `/${contact.birthday.year}`}
          </p>
        </section>
      )}
      {contact.notes && (
        <section data-contact-section="notes">
          <h3>Notes</h3>
          <p>{contact.notes}</p>
        </section>
      )}
    </>
  );
}

function Restore({
  list,
  canRestore,
  onClose,
  onRestore,
}: {
  list: RestorableContact[];
  onClose(): void;
  canRestore: boolean;
  onRestore(item: RestorableContact): void;
}) {
  const closeRef = useRef<HTMLButtonElement>(null);
  useEffect(() => { closeRef.current?.focus(); }, []);

  return (
    <section
      id="contacts-restore-panel"
      role="dialog"
      aria-modal="true"
      aria-label="Recently deleted contacts"
    >
      <header>
        <button
          ref={closeRef}
          data-action="close-restore"
          aria-label="Close recently deleted"
          onClick={onClose}
        >
          ←
        </button>
        <h2>Recently deleted</h2>
      </header>
      {list.length ? (
        <ul id="contacts-restore-list">
          {list.map(item => (
            <li key={item.id} data-contact-id={item.id}>
              <ContactAvatar
                seed={contactSeed(undefined, item.id)}
                name={item.displayName}
                photoDataUrl={item.photoDataUrl}
              />
              <span>
                {item.displayName}
                <small>Deleted {item.deletedAt}</small>
              </span>
              {canRestore && (
                <button
                  data-action="restore-contact"
                  aria-label={`Restore ${item.displayName}`}
                  onClick={() => onRestore(item)}
                >
                  Restore
                </button>
              )}
            </li>
          ))}
        </ul>
      ) : (
        <p id="contacts-restore-empty">No contacts deleted in the past 90 days.</p>
      )}
      {list.length > 0 && !canRestore && (
        <p id="contacts-restore-owner-only">This book doesn't accept changes from this computer, so deleted contacts can't be restored from here.</p>
      )}
    </section>
  );
}
