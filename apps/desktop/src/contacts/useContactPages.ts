import { useCallback, useEffect, useRef, useState } from "react";
import { bridge, type ContactSort, type ContactView } from "../bridge/bridge";
import { contactErrorText } from "./contact-labels";

/** A live refresh re-fetches at most this many loaded pages, then swaps the whole range in at once. */
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
  loadMore(): void;
  retryLoadMore(): void;
  /** Re-fetches the loaded range and swaps it in at once; loaded rows stay until the swap. */
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
};

function fetchPage(bookId: string, sort: ContactSort, cursor?: string) {
  return bridge.list_contacts_page(bookId, { cursor, sort });
}

async function fetchLoadedRange(bookId: string, sort: ContactSort, loadedCount: number) {
  const rows: ContactView[] = [];
  let cursor: string | undefined;
  let total = 0;
  for (let page = 0; page < REFRESH_MAX_PAGES; page += 1) {
    const batch = await fetchPage(bookId, sort, cursor);
    rows.push(...batch.contacts);
    cursor = batch.nextCursor;
    total = batch.total;
    if (cursor === undefined || rows.length >= loadedCount) break;
  }
  return { contacts: rows, total, nextCursor: cursor };
}

function mergeById(...lists: ContactView[][]): ContactView[] {
  return Array.from(new Map(lists.flat().map(contact => [contact.id, contact])).values());
}

function idleView(scope: string, loading: boolean): PageView {
  return { scope, contacts: NO_CONTACTS, total: undefined, nextCursor: undefined, loading, loadingMore: false, loadMoreError: "" };
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

  const loadFirst = useCallback(() => {
    const { bookId, sort, scope } = paramsRef.current;
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
        commit({ ...viewRef.current, loading: false });
        onErrorRef.current(contactErrorText(error));
      })
      .finally(() => end(sequence));
  }, [begin, commit, end]);

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
    if (current.scope !== scope || operationRef.current || current.loadMoreError || !hasMoreRows(current)) return;
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
    const sequence = begin("refresh");
    commit({ ...current, loadingMore: false });
    fetchLoadedRange(bookId, sort, current.contacts.length)
      .then(page => {
        if (sequence !== sequenceRef.current) return;
        commit({ ...loadedView(scope, page), loadMoreError: viewRef.current.loadMoreError });
        onErrorRef.current("");
      })
      .catch(error => {
        if (sequence !== sequenceRef.current) return;
        onErrorRef.current(contactErrorText(error));
      })
      .finally(() => end(sequence));
  }, [begin, commit, end]);

  const resetTo = useCallback((page: { contacts: ContactView[]; total: number; nextCursor?: string }, target: PageScope) => {
    sequenceRef.current += 1;
    operationRef.current = null;
    commit(loadedView(pageScopeKey(target), page));
  }, [commit]);

  // A view for another scope is not shown: the list is empty and loading until its first page lands.
  const view = state.scope === scope ? state : idleView(scope, Boolean(bookId));
  return {
    contacts: view.contacts,
    total: view.total,
    loading: view.loading,
    loadingMore: view.loadingMore,
    loadMoreError: view.loadMoreError,
    hasMore: hasMoreRows(view),
    loadMore,
    retryLoadMore,
    refresh,
    resetTo,
  };
}
