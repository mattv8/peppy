import type { ChangeEvent, CSSProperties, KeyboardEvent } from "react";
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { MessageSquarePlus, X } from "lucide-react";
import type { HistorySearchSnapshot, HistorySearchState, LocalSearchHit, LocalSearchPage, LocalSearchRequest, SearchKind } from "./bridge-contracts";
import { formatDateTime } from "./datetime";
import { formatPhoneNumber, normalizePhoneNumber } from "./phone";

export type OmniContactNumber = { address: string; display: string; label?: string };
export type OmniConversationStartOutcome = boolean | "deferred";
type ContactHit = Extract<LocalSearchHit, { kind: "contact" }>;

export type OmniSearchProps = {
  onSearch(request: LocalSearchRequest): Promise<LocalSearchPage>;
  onActivate(hit: LocalSearchHit): void;
  sessionKey: string;
  disabled?: boolean;
  keyboardShortcut?: boolean;
  refreshKey?: number;
  resolveContactNumbers?: (hit: ContactHit, signal: AbortSignal) => Promise<OmniContactNumber[]>;
  onStartConversation?: (address: string, signal: AbortSignal) => Promise<OmniConversationStartOutcome>;
  historySearch?: HistorySearchSnapshot;
  onSearchFurtherBack?: () => Promise<void>;
  onCancelHistorySearch?: () => Promise<void>;
  onOpenChange?: (open: boolean) => void;
};

type CategoryKind = Exclude<SearchKind, "all">;
type SearchAction =
  | { kind: "more"; category: CategoryKind }
  | { kind: "retry" }
  | { kind: "return" }
  | { kind: "retry-start" }
  | { kind: "retry-contact" }
  | { kind: "return-chooser" }
  | { kind: "history"; intent: HistoryIntent };
type SelectableItem =
  | { kind: "hit"; hit: LocalSearchHit }
  | { kind: "contact-action"; hit: ContactHit }
  | { kind: "candidate"; address: string }
  | { kind: "action"; action: SearchAction };

const SEARCH_DEBOUNCE_MS = 100;
const INDEXING_REFRESH_INTERVAL_MS = 1_000;
const INITIAL_GROUP_LIMIT = 6;
const MAX_CATEGORY_PAGE_LIMIT = 50;
const categories: CategoryKind[] = ["conversation", "message", "contact"];

export type HistoryIntent = "search" | "cancel" | "retry";
export type HistoryPresentation = { status: string; intent: HistoryIntent | null; state: HistorySearchState };
const historyActionCopy: Record<HistoryIntent, { label: string; ariaLabel?: string }> = {
  search: { label: "Search further back" },
  cancel: { label: "Cancel", ariaLabel: "Cancel searching further back" },
  retry: { label: "Retry", ariaLabel: "Retry loading history" },
};
type HistoryPresentationInput = {
  snapshot: HistorySearchSnapshot;
  indexing: boolean;
  hasMatches: boolean;
  queryCurrent: boolean;
  hasCancel: boolean;
  startFailed: boolean;
};

export function historyPresentation({ snapshot, indexing, hasMatches, queryCurrent, hasCancel, startFailed }: HistoryPresentationInput): HistoryPresentation | null {
  const retry = snapshot.canSearchFurther ? "retry" : null;
  const search = snapshot.canSearchFurther ? "search" : null;
  if (startFailed) return { status: "Couldn't load history", intent: search ? "retry" : null, state: "failed" };
  if (snapshot.state === "catchingUp") return { status: "Loading older history…", intent: hasCancel ? "cancel" : null, state: snapshot.state };
  if (snapshot.state === "waitingForPhone") return { status: "Waiting for phone…", intent: hasCancel ? "cancel" : null, state: snapshot.state };
  if (snapshot.state === "indexing") return { status: "Indexing…", intent: hasCancel ? "cancel" : null, state: snapshot.state };
  if (snapshot.state === "blocked" || snapshot.state === "failed") {
    if (snapshot.reason === "unsupported") return hasMatches || !queryCurrent ? null : { status: "No matches", intent: search, state: snapshot.state };
    if (snapshot.reason === "permission") return { status: "Phone needs permission", intent: retry, state: snapshot.state };
    if (snapshot.reason === "capacity" || snapshot.reason === "needsUnlock") return { status: "History paused", intent: retry, state: snapshot.state };
    if (snapshot.reason === "offline" || snapshot.reason === "timeout") return { status: "Phone offline", intent: retry, state: snapshot.state };
    return { status: "Couldn't load history", intent: retry, state: snapshot.state };
  }
  if (!queryCurrent || hasMatches) return snapshot.state === "idle" && indexing ? { status: "Indexing…", intent: null, state: snapshot.state } : null;
  if (snapshot.state === "partial") return { status: "No matches yet", intent: search, state: snapshot.state };
  if (snapshot.state === "cancelled") return { status: "Search stopped", intent: search, state: snapshot.state };
  if (snapshot.state === "completed") return { status: "No matches found", intent: search, state: snapshot.state };
  if (indexing) return { status: "Indexing…", intent: null, state: snapshot.state };
  return { status: "No matches", intent: search, state: snapshot.state };
}

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

function selectableKey(item: SelectableItem): string {
  if (item.kind === "hit") return hitKey(item.hit);
  if (item.kind === "contact-action") return `start-${hitKey(item.hit)}`;
  if (item.kind === "candidate") return `candidate-${item.address}`;
  return item.action.kind === "more" ? `more-${item.action.category}` : item.action.kind;
}

function addressId(address: string): string {
  return encodeURIComponent(address);
}

function selectableItems(page: LocalSearchPage | null, category: CategoryKind | null, error: boolean, candidate: string | null, supportsContactActions: boolean, hasRetryStart: boolean, hasRetryContact: boolean, historyIntent: HistoryIntent | null): SelectableItem[] {
  const items: SelectableItem[] = [];
  if (!category && candidate) items.push({ kind: "candidate", address: candidate });
  if (page) {
    if (category) {
      page.items.forEach((hit) => {
        items.push({ kind: "hit", hit });
        if (supportsContactActions && hit.kind === "contact") items.push({ kind: "contact-action", hit });
      });
      if (page.hasMore[category]) items.push({ kind: "action", action: { kind: "more", category } });
      items.push({ kind: "action", action: { kind: "return" } });
    } else {
      categories.forEach((kind) => {
        page.items.filter((hit) => hit.kind === kind).forEach((hit) => {
          items.push({ kind: "hit", hit });
          if (supportsContactActions && hit.kind === "contact") items.push({ kind: "contact-action", hit });
        });
        if (page.hasMore[kind]) items.push({ kind: "action", action: { kind: "more", category: kind } });
      });
    }
  }
  if (historyIntent) items.push({ kind: "action", action: { kind: "history", intent: historyIntent } });
  if (error) items.push({ kind: "action", action: { kind: "retry" } });
  if (hasRetryStart) items.push({ kind: "action", action: { kind: "retry-start" } });
  if (hasRetryContact) items.push({ kind: "action", action: { kind: "retry-contact" } });
  return items;
}

export function OmniSearch({ onSearch, onActivate, sessionKey, disabled = false, keyboardShortcut = true, refreshKey, resolveContactNumbers, onStartConversation, historySearch, onSearchFurtherBack, onCancelHistorySearch, onOpenChange }: OmniSearchProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  const rootRef = useRef<HTMLElement>(null);
  const openerRef = useRef<HTMLElement | null>(null);
  const onSearchRef = useRef(onSearch);
  const onActivateRef = useRef(onActivate);
  const onOpenChangeRef = useRef(onOpenChange);
  const reportedOpenRef = useRef(false);
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
  const historyIndexVersionRef = useRef(historySearch?.indexVersion);
  const historyIntentRef = useRef<HistoryIntent | null>(null);
  const historyActionPendingRef = useRef(false);
  const historyActionSequenceRef = useRef(0);
  const pendingHistoryIndexRefreshRef = useRef(false);
  const selectedItemKeyRef = useRef<string | undefined>(undefined);
  const compositionRef = useRef(false);
  const recipientAbortRef = useRef<AbortController | null>(null);
  const recipientSequenceRef = useRef(0);
  const recipientPendingRef = useRef(false);
  const chooserRef = useRef<OmniContactNumber[] | null>(null);
  onSearchRef.current = onSearch;
  onActivateRef.current = onActivate;
  onOpenChangeRef.current = onOpenChange;
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
  const [chooser, setChooser] = useState<OmniContactNumber[] | null>(null);
  const [recipientPending, setRecipientPending] = useState(false);
  const [recipientError, setRecipientError] = useState<string | null>(null);
  const [retryAddress, setRetryAddress] = useState<string | null>(null);
  const [retryContact, setRetryContact] = useState<ContactHit | null>(null);
  const [chooserContact, setChooserContact] = useState<ContactHit | null>(null);
  const [historyActionPending, setHistoryActionPending] = useState(false);
  const [startFailed, setStartFailed] = useState(false);
  const trimmedQuery = query.trim();
  const candidate = onStartConversation ? normalizePhoneNumber(trimmedQuery) : null;
  const supportsContactActions = Boolean(resolveContactNumbers && onStartConversation);
  const historyEnabled = Boolean(historySearch && onSearchFurtherBack);
  const history = historyEnabled && historySearch && page && !chooser && trimmedQuery && !composing && !error
    ? historyPresentation({
      snapshot: historySearch,
      indexing: page.indexState === "indexing",
      hasMatches: page.items.length > 0,
      queryCurrent: page.query === trimmedQuery,
      hasCancel: Boolean(onCancelHistorySearch),
      startFailed,
    })
    : null;
  historyIntentRef.current = history?.intent ?? null;
  chooserRef.current = chooser;

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
    recipientAbortRef.current?.abort();
    recipientAbortRef.current = null;
    recipientSequenceRef.current += 1;
    requestSequenceRef.current += 1;
    requestPendingRef.current = false;
    pendingRefreshRef.current = false;
    backgroundRefreshRef.current = false;
    userRequestScheduledRef.current = false;
    clearBackgroundRefresh();
    requestedPrefixRef.current = INITIAL_GROUP_LIMIT;
    rebaseAttemptsRef.current = 0;
    historyActionSequenceRef.current += 1;
    historyActionPendingRef.current = false;
    pendingHistoryIndexRefreshRef.current = false;
    recipientPendingRef.current = false;
    setPage(null); setLoading(false); setError(false); setActiveIndex(0); setChooser(null); setChooserContact(null); setRecipientPending(false); setRecipientError(null); setRetryAddress(null); setRetryContact(null); setHistoryActionPending(false); setStartFailed(false);
  };
  useEffect(() => () => {
    recipientAbortRef.current?.abort();
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
    setStartFailed(false);
  }, [historySearch?.progressVersion]);

  useEffect(() => {
    if (reportedOpenRef.current === open) return;
    reportedOpenRef.current = open;
    onOpenChangeRef.current?.(open);
  }, [open]);

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
        if (merged.indexState === "ready" && !pendingHistoryIndexRefreshRef.current) {
          pendingRefreshRef.current = false;
          clearBackgroundRefresh();
        }
        if (chooserRef.current) return;
        const selectedItemKey = selectedItemKeyRef.current;
        const selectedIndex = selectedItemKey
          ? selectableItems(merged, category, false, candidate, supportsContactActions, Boolean(retryAddress), Boolean(retryContact), historyIntentRef.current).findIndex((item) => selectableKey(item) === selectedItemKey)
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
        if (refreshRequested && (completedPage?.indexState === "indexing" || pendingHistoryIndexRefreshRef.current)) {
          pendingHistoryIndexRefreshRef.current = false;
          scheduleBackgroundRefresh();
        }
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
  }, [candidate, category, composing, disabled, open, queryRevision, refreshEpoch, retry, retryAddress, retryContact, sessionKey, supportsContactActions, trimmedQuery]);

  useEffect(() => {
    const refreshChanged = refreshKeyRef.current !== refreshKey;
    refreshKeyRef.current = refreshKey;
    const indexVersionChanged = historyEnabled && historyIndexVersionRef.current !== historySearch?.indexVersion;
    historyIndexVersionRef.current = historySearch?.indexVersion;
    if (!refreshChanged && !indexVersionChanged) return;
    if (!open || disabled || composing || !trimmedQuery) return;
    if (indexVersionChanged) {
      if (requestPendingRef.current) {
        pendingHistoryIndexRefreshRef.current = true;
        pendingRefreshRef.current = true;
        return;
      }
      if (category && page) requestedPrefixRef.current = Math.max(requestedPrefixRef.current, page.items.length);
      scheduleBackgroundRefresh();
      return;
    }
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
  }, [composing, disabled, historyEnabled, historySearch?.indexVersion, open, page, refreshKey, trimmedQuery]);

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

  const items = chooser
    ? [...chooser.map((number) => ({ kind: "candidate" as const, address: number.address })), { kind: "action" as const, action: { kind: "return-chooser" as const } }, ...(retryAddress ? [{ kind: "action" as const, action: { kind: "retry-start" as const } }] : [])]
    : selectableItems(page, category, error, candidate, supportsContactActions, Boolean(retryAddress), Boolean(retryContact), history?.intent ?? null);
  selectedItemKeyRef.current = items[activeIndex] ? selectableKey(items[activeIndex]) : undefined;

  useLayoutEffect(() => {
    if (activeIndex >= items.length) setActiveIndex(Math.max(0, items.length - 1));
  }, [activeIndex, items.length]);

  const close = (restoreFocus = false) => {
    reset();
    setOpen(false);
    if (restoreFocus) openerRef.current?.focus();
    openerRef.current = null;
  };
  const returnToSearchResults = () => {
    const originKey = chooserContact && hitKey(chooserContact);
    const searchItems = selectableItems(page, category, error, candidate, supportsContactActions, Boolean(retryAddress), Boolean(retryContact), history?.intent ?? null);
    setChooser(null);
    setChooserContact(null);
    setActiveIndex(originKey ? Math.max(0, searchItems.findIndex((item) => item.kind === "contact-action" && hitKey(item.hit) === originKey)) : 0);
  };
  const activateAction = (action: SearchAction) => {
    if (action.kind === "history") {
      if (historyActionPendingRef.current) return;
      const callback = action.intent === "cancel" ? onCancelHistorySearch : onSearchFurtherBack;
      if (!callback) return;
      const sequence = ++historyActionSequenceRef.current;
      if (action.intent !== "cancel") setStartFailed(false);
      historyActionPendingRef.current = true;
      setHistoryActionPending(true);
      void Promise.resolve().then(callback).catch(() => {
        if (sequence === historyActionSequenceRef.current && action.intent !== "cancel") setStartFailed(true);
      }).finally(() => {
        if (sequence !== historyActionSequenceRef.current) return;
        historyActionPendingRef.current = false;
        setHistoryActionPending(false);
      });
      return;
    }
    if (action.kind === "return-chooser") { returnToSearchResults(); return; }
    if (action.kind === "retry-start") { if (retryAddress) startConversation(retryAddress); return; }
    if (action.kind === "retry-contact") { if (retryContact) startContactConversation(retryContact); return; }
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
  const startConversation = (address: string) => {
    if (!onStartConversation || recipientPendingRef.current) return;
    recipientAbortRef.current?.abort();
    const controller = new AbortController();
    const sequence = ++recipientSequenceRef.current;
    recipientAbortRef.current = controller;
    recipientPendingRef.current = true;
    setRecipientPending(true);
    setRecipientError(null);
    setRetryAddress(null);
    setRetryContact(null);
    void onStartConversation(address, controller.signal).then((outcome) => {
      if (controller.signal.aborted || sequence !== recipientSequenceRef.current) return;
      if (outcome === true || outcome === "deferred") { setQuery(""); setCategory(null); close(); }
    }).catch(() => {
      if (controller.signal.aborted || sequence !== recipientSequenceRef.current) return;
      setRecipientError("Could not start the conversation.");
      setRetryAddress(address);
    }).finally(() => {
      if (sequence === recipientSequenceRef.current) { recipientPendingRef.current = false; setRecipientPending(false); }
    });
  };
  const startContactConversation = (hit: ContactHit) => {
    if (!resolveContactNumbers || !onStartConversation || recipientPendingRef.current) return;
    recipientAbortRef.current?.abort();
    const controller = new AbortController();
    const sequence = ++recipientSequenceRef.current;
    recipientAbortRef.current = controller;
    recipientPendingRef.current = true;
    setRecipientPending(true);
    setRecipientError(null);
    setRetryAddress(null);
    setRetryContact(null);
    void resolveContactNumbers(hit, controller.signal).then((numbers) => {
      if (controller.signal.aborted || sequence !== recipientSequenceRef.current) return;
      const uniqueNumbers = Array.from(new Map(numbers.filter((number) => number.address.trim()).map((number) => [number.address, number])).values());
      if (!uniqueNumbers.length) {
        setRecipientError("This contact has no phone numbers.");
        return;
      }
      if (uniqueNumbers.length === 1) {
        recipientPendingRef.current = false;
        startConversation(uniqueNumbers[0].address);
      }
      else {
        setChooser(uniqueNumbers);
        setChooserContact(hit);
        setActiveIndex(0);
      }
    }).catch((failure: unknown) => {
      if (controller.signal.aborted || sequence !== recipientSequenceRef.current) return;
      const code = typeof failure === "object" && failure !== null && "code" in failure ? failure.code : undefined;
      setRecipientError(code === "contact-unavailable" ? "This contact is no longer available." : code === "contact-oversized" ? "This contact book is too large to search." : "Could not load contact phone numbers.");
      setRetryContact(hit);
    }).finally(() => {
      if (sequence === recipientSequenceRef.current) { recipientPendingRef.current = false; setRecipientPending(false); }
    });
  };
  const activate = (item: SelectableItem) => {
    if (item.kind === "action") { activateAction(item.action); return; }
    if (item.kind === "candidate") { startConversation(item.address); return; }
    if (item.kind === "contact-action") { startContactConversation(item.hit); return; }
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
    else if (event.key === "Escape") {
      event.preventDefault();
      if (chooser) {
        recipientAbortRef.current?.abort();
        recipientAbortRef.current = null;
        recipientSequenceRef.current += 1;
        recipientPendingRef.current = false;
        setRecipientPending(false);
        returnToSearchResults();
      }
      else close(true);
    }
    else if (event.key === "Tab") close();
  };
  const clearQuery = () => { compositionRef.current = false; setComposing(false); reset(); setQuery(""); setCategory(null); setOpen(true); inputRef.current?.focus(); };
  const change = (event: ChangeEvent<HTMLInputElement>) => { reset(); setCategory(null); setQuery(Array.from(event.target.value).slice(0, 128).join("")); setQueryRevision((value) => value + 1); setOpen(true); };
  const actionId = (action: SearchAction) => action.kind === "more" ? `more-${action.category}` : action.kind === "retry" ? "retry-search" : action.kind === "return" ? "back-all" : action.kind;
  const selectedId = items[activeIndex]?.kind === "hit" ? `${componentId}-${hitKey(items[activeIndex].hit)}`
    : items[activeIndex]?.kind === "contact-action" ? `${componentId}-start-${hitKey(items[activeIndex].hit)}`
      : items[activeIndex]?.kind === "candidate" ? `${componentId}-candidate-${addressId(items[activeIndex].address)}`
        : items[activeIndex]?.kind === "action" ? `${componentId}-${actionId(items[activeIndex].action)}` : undefined;

  return <section ref={rootRef} id="titlebar-search" className="omni-search" aria-label="Search Peppy" onPointerDownCapture={() => { if (document.activeElement !== inputRef.current) openerRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null; }} onBlur={(event) => { if (!(event.relatedTarget instanceof Node && rootRef.current?.contains(event.relatedTarget))) close(); }}>
    <input ref={inputRef} role="combobox" aria-label="Search Peppy" aria-autocomplete="list" aria-expanded={open} aria-controls={listId} aria-activedescendant={selectedId} autoComplete="off" autoCorrect="off" spellCheck={false} disabled={disabled} value={query} placeholder="Search messages, conversations, contacts" onChange={change} onFocus={(event) => { if (event.relatedTarget instanceof HTMLElement && !rootRef.current?.contains(event.relatedTarget)) openerRef.current = event.relatedTarget; setOpen(true); }} onCompositionStart={() => { compositionRef.current = true; setComposing(true); }} onCompositionEnd={() => { compositionRef.current = false; setComposing(false); }} onKeyDown={onKeyDown} />
    {query.length > 0 && <button id="omni-search-clear" type="button" aria-label="Clear search" title="Clear search" disabled={disabled} onMouseDown={(event) => event.preventDefault()} onClick={clearQuery}><X size={12} strokeWidth={1.75} aria-hidden /></button>}
    {open && <div className="omni-search-results" data-omni-search-results style={popoverStyle} onMouseDown={(event) => event.preventDefault()}>
      {!trimmedQuery && <p className="omni-search-hint">Search locally synced conversations, messages, and contacts.</p>}
       {loading && <p>Searching…</p>}{recipientPending && <p role="status">Starting conversation…</p>}{recipientError && <p role="alert">{recipientError}</p>}{!historyEnabled && page?.indexState === "indexing" && <p>Search index is updating; results may be incomplete.</p>}{error && <p>Search could not be completed. Choose Retry search.</p>}{!historyEnabled && !loading && !error && (!candidate || category) && trimmedQuery && page && !page.items.length && <p>No matches.</p>}
       <div id={listId} role="listbox" aria-label={chooserContact ? `Choose a number for ${chooserContact.title}` : "Search results"} aria-busy={recipientPending}>
        {chooser ? <div role="group" aria-label={`Choose a number for ${chooserContact?.title ?? "contact"}`} data-omni-number-chooser><div role="presentation" className="omni-search-group-label">Choose a number for {chooserContact?.title}</div>{chooser.map((number) => <div key={number.address} id={`${componentId}-candidate-${addressId(number.address)}`} role="option" aria-label={number.label ? `${number.display}, ${number.label}` : number.display} aria-selected={selectedId === `${componentId}-candidate-${addressId(number.address)}`} data-omni-number-choice onClick={() => startConversation(number.address)}><b>{number.display}</b>{number.label && <span>{number.label}</span>}</div>)}{items.filter((item) => item.kind === "action").map((item) => item.kind === "action" && <div key={actionId(item.action)} id={`${componentId}-${actionId(item.action)}`} role="option" aria-selected={selectedId === `${componentId}-${actionId(item.action)}`} data-search-action={item.action.kind} onClick={() => activate(item)}>{item.action.kind === "return-chooser" ? "Return to search results" : "Retry starting conversation"}</div>)}</div> : <>
        {!category && candidate && <div id={`${componentId}-candidate-${addressId(candidate)}`} role="option" aria-selected={selectedId === `${componentId}-candidate-${addressId(candidate)}`} data-omni-number-candidate title={`Start conversation with ${formatPhoneNumber(candidate)}`} onClick={() => startConversation(candidate)}><span><b>Start conversation with {formatPhoneNumber(candidate)}</b><small>{candidate}</small></span><MessageSquarePlus size={16} aria-hidden /></div>}
       {categories.map((kind) => { const group = page?.items.filter((hit) => hit.kind === kind) ?? []; const more = items.find((item): item is { kind: "action"; action: Extract<SearchAction, { kind: "more" }> } => item.kind === "action" && item.action.kind === "more" && item.action.category === kind); if ((!category && !group.length) || (category && category !== kind)) return null; return <div key={kind} role="group" aria-label={categoryLabel(kind)} data-search-group={kind}><div role="presentation" className="omni-search-group-label">{categoryLabel(kind)}</div>{group.map((hit) => <div key={hitKey(hit)} className="omni-search-hit-row"><div id={`${componentId}-${hitKey(hit)}`} role="option" aria-selected={selectedId === `${componentId}-${hitKey(hit)}`} data-search-hit={hitKey(hit)} onClick={() => activate({ kind: "hit", hit })} title={`${hit.title} — ${subtitle(hit)}`}><b>{hit.title}</b><span>{subtitle(hit)}</span></div>{hit.kind === "contact" && resolveContactNumbers && onStartConversation && <div id={`${componentId}-start-${hitKey(hit)}`} role="option" aria-label={`Start conversation with ${hit.title}`} aria-selected={selectedId === `${componentId}-start-${hitKey(hit)}`} data-omni-contact-action title={`Start conversation with ${hit.title}`} onClick={() => startContactConversation(hit)}><MessageSquarePlus size={16} aria-hidden /></div>}</div>)}{more && <div id={`${componentId}-${actionId(more.action)}`} role="option" aria-selected={selectedId === `${componentId}-${actionId(more.action)}`} data-search-action="more" onClick={() => activate(more)}>Show all {categoryLabel(kind).toLowerCase()}</div>}</div>; })}
       {items.filter((item) => item.kind === "action" && item.action.kind === "return").map((item) => item.kind === "action" && <div key={actionId(item.action)} id={`${componentId}-${actionId(item.action)}`} role="option" aria-selected={selectedId === `${componentId}-${actionId(item.action)}`} data-search-action={item.action.kind} onClick={() => activate(item)}>Return to all results</div>)}
       {history && <div id="omni-search-history" role="group" aria-labelledby="omni-search-history-status" className="omni-search-history" data-omni-history data-history-state={history.state}>
         <div id="omni-search-history-status" role="presentation" className="omni-search-history-status">{history.status}</div>
         {history.intent && <div id={`${componentId}-history`} role="option" aria-label={historyActionCopy[history.intent].ariaLabel} aria-selected={selectedId === `${componentId}-history`} aria-disabled={historyActionPending || undefined} className="omni-search-history-action" data-search-action="history" data-history-intent={history.intent} onClick={() => activate({ kind: "action", action: { kind: "history", intent: history.intent! } })}>{historyActionCopy[history.intent].label}</div>}
       </div>}
       {items.filter((item) => item.kind === "action" && item.action.kind !== "more" && item.action.kind !== "return" && item.action.kind !== "history").map((item) => item.kind === "action" && <div key={actionId(item.action)} id={`${componentId}-${actionId(item.action)}`} role="option" aria-selected={selectedId === `${componentId}-${actionId(item.action)}`} data-search-action={item.action.kind} onClick={() => activate(item)}>{item.action.kind === "retry" ? "Retry search" : item.action.kind === "retry-start" ? "Retry starting conversation" : item.action.kind === "retry-contact" ? "Retry contact phone lookup" : "Return to all results"}</div>)}
       </>}
      </div>
    </div>}
    <span className="sr-only" role="status" aria-live="polite">{!loading && trimmedQuery && page ? `${page.items.length} search results` : ""}</span>
    {historyEnabled && open && <span className="sr-only" id="omni-search-history-announcement" role="status" aria-live="polite">{history?.status ?? ""}</span>}
  </section>;
}
