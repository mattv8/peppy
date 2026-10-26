import type { ChangeEvent, CSSProperties, KeyboardEvent } from "react";
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import type { LocalSearchHit, LocalSearchPage, LocalSearchRequest, SearchKind } from "./bridge-contracts";
import { formatDateTime } from "./datetime";

export type OmniSearchProps = {
  onSearch(request: LocalSearchRequest): Promise<LocalSearchPage>;
  onActivate(hit: LocalSearchHit): void;
  sessionKey: string;
  disabled?: boolean;
  keyboardShortcut?: boolean;
  refreshKey?: number;
};

type CategoryKind = Exclude<SearchKind, "all">;
type SearchAction = { kind: "more"; category: CategoryKind } | { kind: "retry" } | { kind: "return" };
type SelectableItem = { kind: "hit"; hit: LocalSearchHit } | { kind: "action"; action: SearchAction };

const SEARCH_DEBOUNCE_MS = 100;
const INDEXING_REFRESH_INTERVAL_MS = 1_000;
const INITIAL_GROUP_LIMIT = 6;
const MAX_CATEGORY_PAGE_LIMIT = 50;
const categories: CategoryKind[] = ["conversation", "message", "contact"];

function hitKey(hit: LocalSearchHit): string {
  return hit.kind === "conversation" ? `conversation-${hit.conversationId}` : hit.kind === "message" ? `message-${hit.conversationId}-${hit.messageId}` : `contact-${hit.bookId}-${hit.contactId}`;
}

function categoryLabel(category: CategoryKind): string {
  return category === "conversation" ? "Conversations" : category === "message" ? "Messages" : "Contacts";
}

function subtitle(hit: LocalSearchHit): string {
  if (hit.kind !== "message") return hit.subtitle;
  const timestamp = formatDateTime(hit.timestamp);
  return timestamp ? `${hit.snippet} · ${timestamp}` : hit.snippet;
}

function appendUnique(items: LocalSearchHit[], additions: LocalSearchHit[]): LocalSearchHit[] {
  const seen = new Set(items.map(hitKey));
  return [...items, ...additions.filter((item) => !seen.has(hitKey(item)))];
}

function selectableItems(page: LocalSearchPage | null, category: CategoryKind | null, error: boolean): SelectableItem[] {
  const items: SelectableItem[] = [];
  if (page) {
    if (category) {
      page.items.forEach((hit) => items.push({ kind: "hit", hit }));
      if (page.hasMore[category]) items.push({ kind: "action", action: { kind: "more", category } });
      items.push({ kind: "action", action: { kind: "return" } });
    } else {
      categories.forEach((kind) => {
        page.items.filter((hit) => hit.kind === kind).forEach((hit) => items.push({ kind: "hit", hit }));
        if (page.hasMore[kind]) items.push({ kind: "action", action: { kind: "more", category: kind } });
      });
    }
  }
  if (error) items.push({ kind: "action", action: { kind: "retry" } });
  return items;
}

export function OmniSearch({ onSearch, onActivate, sessionKey, disabled = false, keyboardShortcut = true, refreshKey }: OmniSearchProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  const rootRef = useRef<HTMLElement>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const onSearchRef = useRef(onSearch);
  const onActivateRef = useRef(onActivate);
  const requestSequenceRef = useRef(0);
  const requestPendingRef = useRef(false);
  const pendingRefreshRef = useRef(false);
  const backgroundRefreshRef = useRef(false);
  const backgroundRefreshTimerRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const userRequestScheduledRef = useRef(false);
  const lastRequestCompletedAtRef = useRef(0);
  const requestedPrefixRef = useRef(INITIAL_GROUP_LIMIT);
  const rebaseAttemptsRef = useRef(0);
  const refreshKeyRef = useRef(refreshKey);
  const selectedHitKeyRef = useRef<string | undefined>(undefined);
  const compositionRef = useRef(false);
  onSearchRef.current = onSearch;
  onActivateRef.current = onActivate;
  const componentId = useId();
  const listId = `omni-search-results-${componentId}`;
  const [query, setQuery] = useState("");
  const [page, setPage] = useState<LocalSearchPage | null>(null);
  const [category, setCategory] = useState<CategoryKind | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState(false);
  const [retry, setRetry] = useState(0);
  const [refreshEpoch, setRefreshEpoch] = useState(0);
  const [queryRevision, setQueryRevision] = useState(0);
  const [activeIndex, setActiveIndex] = useState(0);
  const [open, setOpen] = useState(false);
  const [composing, setComposing] = useState(false);
  const [popoverStyle, setPopoverStyle] = useState<CSSProperties>();
  const trimmedQuery = query.trim();

  const clearBackgroundRefresh = () => {
    if (backgroundRefreshTimerRef.current === undefined) return;
    clearTimeout(backgroundRefreshTimerRef.current);
    backgroundRefreshTimerRef.current = undefined;
  };
  const scheduleBackgroundRefresh = () => {
    if (backgroundRefreshTimerRef.current !== undefined) return;
    const elapsed = Date.now() - lastRequestCompletedAtRef.current;
    const delay = Math.max(0, INDEXING_REFRESH_INTERVAL_MS - elapsed);
    backgroundRefreshTimerRef.current = setTimeout(() => {
      backgroundRefreshTimerRef.current = undefined;
      if (requestPendingRef.current || userRequestScheduledRef.current) {
        pendingRefreshRef.current = true;
        return;
      }
      backgroundRefreshRef.current = true;
      setRefreshEpoch((value) => value + 1);
    }, delay);
  };
  const reset = () => {
    requestSequenceRef.current += 1;
    requestPendingRef.current = false;
    pendingRefreshRef.current = false;
    backgroundRefreshRef.current = false;
    userRequestScheduledRef.current = false;
    clearBackgroundRefresh();
    requestedPrefixRef.current = INITIAL_GROUP_LIMIT;
    rebaseAttemptsRef.current = 0;
    setPage(null); setLoading(false); setError(false); setActiveIndex(0);
  };
  useEffect(() => () => {
    requestSequenceRef.current += 1;
    requestPendingRef.current = false;
    pendingRefreshRef.current = false;
    backgroundRefreshRef.current = false;
    userRequestScheduledRef.current = false;
    clearBackgroundRefresh();
  }, []);
  useEffect(() => {
    reset(); setQuery(""); setCategory(null); setOpen(false); openerRef.current = null;
  }, [disabled, sessionKey]);

  useEffect(() => {
    if (!open || disabled || composing || !trimmedQuery) { reset(); return; }
    const sequence = ++requestSequenceRef.current;
    const backgroundRefresh = backgroundRefreshRef.current;
    backgroundRefreshRef.current = false;
    const startRequest = () => {
      userRequestScheduledRef.current = false;
      if (sequence !== requestSequenceRef.current) return;
      requestPendingRef.current = true;
      if (!backgroundRefresh) { setLoading(true); setError(false); }
      let completedPage: LocalSearchPage | undefined;
      const loadPrefix = async () => {
        const requestedPrefix = category ? requestedPrefixRef.current : INITIAL_GROUP_LIMIT;
        let remaining = requestedPrefix;
        let cursor: string | undefined;
        let prefix: LocalSearchHit[] = [];
        let next: LocalSearchPage;
        do {
          next = await onSearchRef.current({ query: trimmedQuery, kind: category ?? "all", cursor, limit: Math.min(MAX_CATEGORY_PAGE_LIMIT, remaining) });
          if (sequence !== requestSequenceRef.current || next.query !== trimmedQuery) return;
          prefix = appendUnique(prefix, next.items);
          remaining -= next.items.length;
          cursor = next.nextCursor;
        } while (category && requestedPrefix > MAX_CATEGORY_PAGE_LIMIT && remaining > 0 && cursor);
        if (sequence !== requestSequenceRef.current) return;
        const merged = category ? { ...next!, items: prefix } : next!;
        completedPage = merged;
        rebaseAttemptsRef.current = 0;
        setPage(merged);
        if (merged.indexState === "ready") {
          pendingRefreshRef.current = false;
          clearBackgroundRefresh();
        }
        const selectedHitKey = selectedHitKeyRef.current;
        const selectedIndex = selectedHitKey
          ? selectableItems(merged, category, false).findIndex((item) => item.kind === "hit" && hitKey(item.hit) === selectedHitKey)
          : -1;
        setActiveIndex(selectedIndex < 0 ? 0 : selectedIndex);
      };
      void loadPrefix().catch(() => {
        if (!backgroundRefresh && sequence === requestSequenceRef.current) setError(true);
      }).finally(() => {
        if (sequence !== requestSequenceRef.current) return;
        requestPendingRef.current = false;
        lastRequestCompletedAtRef.current = Date.now();
        if (!backgroundRefresh) setLoading(false);
        const refreshRequested = pendingRefreshRef.current;
        pendingRefreshRef.current = false;
        if (refreshRequested && completedPage?.indexState === "indexing") scheduleBackgroundRefresh();
      });
    };
    if (backgroundRefresh) {
      startRequest();
      return;
    }
    clearBackgroundRefresh();
    userRequestScheduledRef.current = true;
    const timer = setTimeout(startRequest, SEARCH_DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
      userRequestScheduledRef.current = false;
    };
  }, [category, composing, disabled, open, queryRevision, refreshEpoch, retry, sessionKey, trimmedQuery]);

  useEffect(() => {
    if (refreshKeyRef.current === refreshKey) return;
    refreshKeyRef.current = refreshKey;
    if (!open || disabled || composing || !trimmedQuery) return;
    if (page?.indexState === "ready") {
      pendingRefreshRef.current = false;
      clearBackgroundRefresh();
      return;
    }
    if (requestPendingRef.current) {
      pendingRefreshRef.current = true;
      return;
    }
    if (page?.indexState !== "indexing") return;
    if (category) requestedPrefixRef.current = Math.max(requestedPrefixRef.current, page.items.length);
    scheduleBackgroundRefresh();
  }, [composing, disabled, open, page, refreshKey, trimmedQuery]);

  useEffect(() => {
    if (!keyboardShortcut || disabled) return;
    const shortcut = (event: globalThis.KeyboardEvent) => {
      if (event.defaultPrevented || event.altKey || event.shiftKey || !(event.metaKey || event.ctrlKey) || event.key.toLowerCase() !== "k") return;
      const target = event.target instanceof HTMLElement ? event.target : null;
      if (target?.closest("[role=dialog], input[type=password], input[autocomplete=current-password], input[autocomplete=new-password], [data-secure-input]")) return;
      event.preventDefault();
      const activeElement = document.activeElement;
      if (activeElement instanceof HTMLElement && activeElement !== inputRef.current) openerRef.current = activeElement;
      inputRef.current?.focus(); inputRef.current?.select(); setOpen(true);
    };
    document.addEventListener("keydown", shortcut);
    return () => document.removeEventListener("keydown", shortcut);
  }, [disabled, keyboardShortcut]);

  useEffect(() => {
    if (!open) return;
    const closeOutside = (event: globalThis.PointerEvent) => {
      if (event.target instanceof Node && !rootRef.current?.contains(event.target)) close();
    };
    document.addEventListener("pointerdown", closeOutside);
    return () => document.removeEventListener("pointerdown", closeOutside);
  }, [open]);

  useLayoutEffect(() => {
    if (!open || !inputRef.current) return;
    const updatePopoverPosition = () => {
      const rect = inputRef.current?.getBoundingClientRect();
      if (!rect) return;
      const width = Math.min(520, Math.max(0, window.innerWidth - 16));
      const candidate = rect.left + rect.width / 2 - width / 2;
      const left = Math.min(Math.max(candidate, 8), Math.max(8, window.innerWidth - width - 8));
      const next = { left, top: rect.bottom + 4, width };
      setPopoverStyle((current) => current?.left === next.left && current.top === next.top && current.width === next.width ? current : next);
    };
    updatePopoverPosition();
    const observer = typeof ResizeObserver === "undefined" ? undefined : new ResizeObserver(updatePopoverPosition);
    observer?.observe(inputRef.current);
    window.addEventListener("resize", updatePopoverPosition);
    window.addEventListener("scroll", updatePopoverPosition, true);
    return () => {
      observer?.disconnect();
      window.removeEventListener("resize", updatePopoverPosition);
      window.removeEventListener("scroll", updatePopoverPosition, true);
    };
  }, [open]);

  const items = selectableItems(page, category, error);
  selectedHitKeyRef.current = items[activeIndex]?.kind === "hit" ? hitKey(items[activeIndex].hit) : undefined;

  const close = (restoreFocus = false) => {
    reset();
    setOpen(false);
    if (restoreFocus) openerRef.current?.focus();
    openerRef.current = null;
  };
  const activateAction = (action: SearchAction) => {
    if (action.kind === "return") { reset(); setCategory(null); return; }
    if (action.kind === "retry") { reset(); setRetry((value) => value + 1); return; }
    if (!category) { reset(); setCategory(action.category); return; }
    if (!page?.nextCursor || loading) return;
    clearBackgroundRefresh();
    pendingRefreshRef.current = false;
    const sequence = ++requestSequenceRef.current;
    requestPendingRef.current = true;
    setLoading(true); setError(false);
    let completedPage: LocalSearchPage | undefined;
    void onSearchRef.current({ query: trimmedQuery, kind: category, cursor: page.nextCursor, limit: INITIAL_GROUP_LIMIT }).then((next) => {
      completedPage = next;
      if (sequence !== requestSequenceRef.current || next.query !== trimmedQuery) return;
      const mergedItems = appendUnique(page.items, next.items);
      requestedPrefixRef.current = Math.max(requestedPrefixRef.current, mergedItems.length);
      setPage({ ...next, items: mergedItems });
      if (next.indexState === "ready") {
        pendingRefreshRef.current = false;
        clearBackgroundRefresh();
      }
    }).catch((failure: unknown) => {
      const code = typeof failure === "object" && failure !== null && "code" in failure ? failure.code : undefined;
      if (sequence !== requestSequenceRef.current) return;
      if (code === "invalid-request" && rebaseAttemptsRef.current === 0) {
        rebaseAttemptsRef.current += 1;
        requestedPrefixRef.current = page.items.length + INITIAL_GROUP_LIMIT;
        pendingRefreshRef.current = false;
        setRefreshEpoch((value) => value + 1);
        return;
      }
      setError(true);
    }).finally(() => {
      if (sequence !== requestSequenceRef.current) return;
      requestPendingRef.current = false;
      lastRequestCompletedAtRef.current = Date.now();
      setLoading(false);
      const refreshRequested = pendingRefreshRef.current;
      pendingRefreshRef.current = false;
      if (refreshRequested && completedPage?.indexState === "indexing") scheduleBackgroundRefresh();
    });
  };
  const activate = (item: SelectableItem) => {
    if (item.kind === "action") { activateAction(item.action); return; }
    if (page?.query !== trimmedQuery) return;
    onActivateRef.current(item.hit); close();
  };
  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229 || compositionRef.current) return;
    if (!open && (event.key === "Enter" || event.key === "ArrowDown")) { event.preventDefault(); setOpen(true); }
    else if (event.key === "ArrowDown" || event.key === "ArrowUp") { if (!items.length) return; event.preventDefault(); setActiveIndex((value) => Math.max(0, Math.min(items.length - 1, value + (event.key === "ArrowDown" ? 1 : -1)))); }
    else if (open && event.key === "Home") { event.preventDefault(); setActiveIndex(0); }
    else if (open && event.key === "End") { event.preventDefault(); setActiveIndex(Math.max(0, items.length - 1)); }
    else if (open && event.key === "Enter" && items[activeIndex]) { event.preventDefault(); activate(items[activeIndex]); }
    else if (event.key === "Escape") { event.preventDefault(); close(true); }
    else if (event.key === "Tab") close();
  };
  const clearQuery = () => { compositionRef.current = false; setComposing(false); reset(); setQuery(""); setCategory(null); setOpen(true); inputRef.current?.focus(); };
  const change = (event: ChangeEvent<HTMLInputElement>) => { reset(); setCategory(null); setQuery(Array.from(event.target.value).slice(0, 128).join("")); setQueryRevision((value) => value + 1); setOpen(true); };
  const actionId = (action: SearchAction) => action.kind === "more" ? `more-${action.category}` : action.kind === "retry" ? "retry-search" : "back-all";
  const selectedId = items[activeIndex]?.kind === "hit" ? `${componentId}-${hitKey(items[activeIndex].hit)}` : items[activeIndex]?.kind === "action" ? `${componentId}-${actionId(items[activeIndex].action)}` : undefined;

  return <section ref={rootRef} id="titlebar-search" className="omni-search" aria-label="Search Peppy" onPointerDownCapture={() => { if (document.activeElement !== inputRef.current) openerRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null; }} onBlur={(event) => { if (!(event.relatedTarget instanceof Node && rootRef.current?.contains(event.relatedTarget))) close(); }}>
    <input ref={inputRef} role="combobox" aria-label="Search Peppy" aria-autocomplete="list" aria-expanded={open} aria-controls={listId} aria-activedescendant={selectedId} autoComplete="off" autoCorrect="off" spellCheck={false} disabled={disabled} value={query} placeholder="Search messages, conversations, contacts" onChange={change} onFocus={(event) => { if (event.relatedTarget instanceof HTMLElement && !rootRef.current?.contains(event.relatedTarget)) openerRef.current = event.relatedTarget; setOpen(true); }} onCompositionStart={() => { compositionRef.current = true; setComposing(true); }} onCompositionEnd={() => { compositionRef.current = false; setComposing(false); }} onKeyDown={onKeyDown} />
    {query.length > 0 && <button id="omni-search-clear" type="button" aria-label="Clear search" title="Clear search" disabled={disabled} onMouseDown={(event) => event.preventDefault()} onClick={clearQuery}><X size={12} strokeWidth={1.75} aria-hidden /></button>}
    {open && <div className="omni-search-results" data-omni-search-results style={popoverStyle} onMouseDown={(event) => event.preventDefault()}>
      {!trimmedQuery && <p className="omni-search-hint">Search locally synced conversations, messages, and contacts.</p>}
      {loading && <p>Searching…</p>}{page?.indexState === "indexing" && <p>Search index is updating; results may be incomplete.</p>}{error && <p>Search could not be completed. Choose Retry search.</p>}{!loading && !error && trimmedQuery && page && !page.items.length && <p>No matches.</p>}
      <div id={listId} role="listbox" aria-label="Search results">
      {categories.map((kind) => { const group = page?.items.filter((hit) => hit.kind === kind) ?? []; const more = items.find((item): item is { kind: "action"; action: Extract<SearchAction, { kind: "more" }> } => item.kind === "action" && item.action.kind === "more" && item.action.category === kind); if ((!category && !group.length) || (category && category !== kind)) return null; return <div key={kind} role="group" aria-label={categoryLabel(kind)} data-search-group={kind}><div role="presentation" className="omni-search-group-label">{categoryLabel(kind)}</div>{group.map((hit) => <div key={hitKey(hit)} id={`${componentId}-${hitKey(hit)}`} role="option" aria-selected={selectedId === `${componentId}-${hitKey(hit)}`} data-search-hit={hitKey(hit)} onClick={() => activate({ kind: "hit", hit })} title={`${hit.title} — ${subtitle(hit)}`}><b>{hit.title}</b><span>{subtitle(hit)}</span></div>)}{more && <div id={`${componentId}-${actionId(more.action)}`} role="option" aria-selected={selectedId === `${componentId}-${actionId(more.action)}`} data-search-action="more" onClick={() => activate(more)}>Show all {categoryLabel(kind).toLowerCase()}</div>}</div>; })}
      {items.filter((item) => item.kind === "action" && item.action.kind !== "more").map((item) => item.kind === "action" && <div key={actionId(item.action)} id={`${componentId}-${actionId(item.action)}`} role="option" aria-selected={selectedId === `${componentId}-${actionId(item.action)}`} data-search-action={item.action.kind} onClick={() => activate(item)}>{item.action.kind === "retry" ? "Retry search" : "Return to all results"}</div>)}
      </div>
    </div>}
    <span className="sr-only" role="status" aria-live="polite">{!loading && trimmedQuery && page ? `${page.items.length} search results` : ""}</span>
  </section>;
}
