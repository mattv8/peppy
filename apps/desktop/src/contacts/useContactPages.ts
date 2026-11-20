import { useCallback, useEffect, useRef, useState } from "react";
import { bridge, type ContactSort, type ContactView } from "../bridge/bridge";
import { contactErrorText } from "./contact-labels";

/** A live refresh re-fetches at most this many pages of the loaded range, then swaps the range in at once. */
const REFRESH_MAX_PAGES = 10;
const NO_CONTACTS: ContactView[] = [];

export type PageScope = { bookId: string; sort: ContactSort };

/** Identity of one list. A different book or sort starts again from the first page. */
export function pageScopeKey({ bookId, sort }: PageScope): string {
  return `${bookId}\u0000${sort.key}\u0000${sort.direction}`;
}

export type ContactPages = {
  contacts: ContactView[];
  total: number | undefined;
  /** The first page of the current list is in flight. */
  loading: boolean;
  loadingMore: boolean;
  loadMoreError: string;
  hasMore: boolean;
  /** The first page of the current list failed, so no rows are shown for it. */
  loadFailed: boolean;
  /** Requests a page; while a refresh runs, the request waits and runs once the refresh ends. */
  loadMore(): void;
  retryLoadMore(): void;
  /** Loads the first page again, after `loadFailed`. */
  retryFirstPage(): void;
  /**
   * Re-fetches the loaded range and swaps it in at once; loaded rows stay until the swap. A call made while a refresh
   * runs is coalesced: one more refresh runs after the current one ends.
   */
  refresh(): void;
  /** Shows contacts a target lookup already fetched for `scope`, without another request. */
  resetTo(page: { contacts: ContactView[]; total: number; nextCursor?: string }, scope: PageScope): void;
};

type Options = PageScope & {
  /** Receives the first-page or refresh error; an empty string clears it. */
  onError(message: string): void;
};

type Operation = "first" | "more" | "refresh";

type PageView = {
  scope: string;
  contacts: ContactView[];
  total: number | undefined;
  nextCursor: string | undefined;
  loading: boolean;
  loadingMore: boolean;
  loadMoreError: string;
  loadFailed: boolean;
};

function fetchPage(bookId: string, sort: ContactSort, cursor?: string) {
  return bridge.list_contacts_page(bookId, { cursor, sort });
}

/**
 * Re-fetches the loaded range one cursor page at a time and stops between pages once `stillCurrent` turns false.
 * When the cap stops the walk early, the loaded rows past the refreshed ones stay after them and the cap's cursor
 * continues the list, so no loaded row disappears; the rows past the cap are only replaced when the user pages.
 */
async function fetchLoadedRange(bookId: string, sort: ContactSort, loaded: ContactView[], stillCurrent: () => boolean) {
  const rows: ContactView[] = [];
  let cursor: string | undefined;
  let total = 0;
  for (let page = 0; page < REFRESH_MAX_PAGES; page += 1) {
    const batch = await fetchPage(bookId, sort, cursor);
    if (!stillCurrent()) break;
    rows.push(...batch.contacts);
    cursor = batch.nextCursor;
    total = batch.total;
    if (cursor === undefined || rows.length >= loaded.length) break;
  }
  const beyondCap = cursor === undefined ? [] : loaded.slice(rows.length);
  return { contacts: mergeById(rows, beyondCap), total, nextCursor: cursor };
}

function mergeById(...lists: ContactView[][]): ContactView[] {
  return Array.from(new Map(lists.flat().map(contact => [contact.id, contact])).values());
}

function idleView(scope: string, loading: boolean): PageView {
  return { scope, contacts: NO_CONTACTS, total: undefined, nextCursor: undefined, loading, loadingMore: false, loadMoreError: "", loadFailed: false };
}

function loadedView(scope: string, page: { contacts: ContactView[]; total: number; nextCursor?: string }): PageView {
  return { ...idleView(scope, false), ...page, contacts: mergeById(page.contacts) };
}

function hasMoreRows(view: PageView): boolean {
  return view.nextCursor !== undefined;
}

/**
 * Pages of the contact list for one book and sort. Rows load on demand through `loadMore`; a refresh or a
 * target lookup never clears rows already shown. Responses from superseded requests are discarded.
 */
export function useContactPages({ bookId, sort, onError }: Options): ContactPages {
  const scope = pageScopeKey({ bookId, sort });
  const [state, setState] = useState<PageView>(() => idleView(scope, Boolean(bookId)));
  const viewRef = useRef(state);
  const paramsRef = useRef({ bookId, sort, scope });
  paramsRef.current = { bookId, sort, scope };
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;
  const sequenceRef = useRef(0);
  const operationRef = useRef<Operation | null>(null);
  // Requests that arrived while a refresh ran. They run when the refresh ends (see runQueuedRef).
  const refreshQueued = useRef(false);
  const moreQueued = useRef(false);
  const runQueuedRef = useRef<() => void>(() => undefined);

  const commit = useCallback((next: PageView) => {
    viewRef.current = next;
    setState(next);
  }, []);

  const begin = useCallback((operation: Operation): number => {
    operationRef.current = operation;
    sequenceRef.current += 1;
    return sequenceRef.current;
  }, []);

  const end = useCallback((sequence: number) => {
    if (sequence === sequenceRef.current) operationRef.current = null;
  }, []);

  const dropQueued = useCallback(() => {
    refreshQueued.current = false;
    moreQueued.current = false;
  }, []);

  const loadFirst = useCallback(() => {
    const { bookId, sort, scope } = paramsRef.current;
    dropQueued();
    if (!bookId) {
      sequenceRef.current += 1;
      operationRef.current = null;
      commit(idleView(scope, false));
      return;
    }
    const sequence = begin("first");
    commit(idleView(scope, true));
    const list = document.getElementById("contacts-list");
    if (list) list.scrollTop = 0;
    fetchPage(bookId, sort)
      .then(page => {
        if (sequence !== sequenceRef.current) return;
        commit(loadedView(scope, page));
        onErrorRef.current("");
      })
      .catch(error => {
        if (sequence !== sequenceRef.current) return;
        commit({ ...viewRef.current, loading: false, loadFailed: true });
        onErrorRef.current(contactErrorText(error));
      })
      .finally(() => end(sequence));
  }, [begin, commit, dropQueued, end]);

  // Starts the first page for a new book or sort, unless a target lookup already supplied this list.
  useEffect(() => {
    const current = viewRef.current;
    if (current.scope === scope && !current.loading) return;
    loadFirst();
  }, [scope, loadFirst]);

  useEffect(() => () => {
    sequenceRef.current += 1;
    operationRef.current = null;
  }, []);

  const loadMore = useCallback(() => {
    const { bookId, sort, scope } = paramsRef.current;
    const current = viewRef.current;
    if (current.scope !== scope || current.loadMoreError || !hasMoreRows(current)) return;
    if (operationRef.current === "refresh") {
      moreQueued.current = true;
      return;
    }
    if (operationRef.current) return;
    const sequence = begin("more");
    commit({ ...current, loadingMore: true });
    fetchPage(bookId, sort, current.nextCursor)
      .then(page => {
        if (sequence !== sequenceRef.current) return;
        const latest = viewRef.current;
        commit({
          ...latest,
          contacts: mergeById(latest.contacts, page.contacts),
          total: page.total,
          nextCursor: page.nextCursor,
          loadingMore: false,
        });
      })
      .catch(error => {
        if (sequence !== sequenceRef.current) return;
        commit({ ...viewRef.current, loadingMore: false, loadMoreError: contactErrorText(error) });
      })
      .finally(() => end(sequence));
  }, [begin, commit, end]);

  const retryLoadMore = useCallback(() => {
    commit({ ...viewRef.current, loadMoreError: "" });
    loadMore();
  }, [commit, loadMore]);

  const refresh = useCallback(() => {
    const { bookId, sort, scope } = paramsRef.current;
    const current = viewRef.current;
    if (!bookId || current.scope !== scope || current.loading) return;
    if (operationRef.current === "refresh") {
      refreshQueued.current = true;
      return;
    }
    const sequence = begin("refresh");
    commit({ ...current, loadingMore: false });
    fetchLoadedRange(bookId, sort, current.contacts, () => sequence === sequenceRef.current)
      .then(page => {
        if (sequence !== sequenceRef.current) return;
        commit({ ...loadedView(scope, page), loadMoreError: viewRef.current.loadMoreError });
        onErrorRef.current("");
      })
      .catch(error => {
        if (sequence !== sequenceRef.current) return;
        onErrorRef.current(contactErrorText(error));
      })
      .finally(() => {
        end(sequence);
        if (sequence === sequenceRef.current) runQueuedRef.current();
      });
  }, [begin, commit, end]);

  // A queued refresh runs first; a queued page request then runs once no operation is in flight.
  runQueuedRef.current = () => {
    if (refreshQueued.current) {
      refreshQueued.current = false;
      refresh();
    }
    if (moreQueued.current && !operationRef.current) {
      moreQueued.current = false;
      loadMore();
    }
  };

  const resetTo = useCallback((page: { contacts: ContactView[]; total: number; nextCursor?: string }, target: PageScope) => {
    sequenceRef.current += 1;
    operationRef.current = null;
    dropQueued();
    commit(loadedView(pageScopeKey(target), page));
  }, [commit, dropQueued]);

  // A view for another scope is not shown: the list is empty and loading until its first page lands.
  const view = state.scope === scope ? state : idleView(scope, Boolean(bookId));
  return {
    contacts: view.contacts,
    total: view.total,
    loading: view.loading,
    loadingMore: view.loadingMore,
    loadMoreError: view.loadMoreError,
    hasMore: hasMoreRows(view),
    loadFailed: view.loadFailed,
    loadMore,
    retryLoadMore,
    retryFirstPage: loadFirst,
    refresh,
    resetTo,
  };
}
