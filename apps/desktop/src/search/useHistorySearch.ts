import { useCallback, useEffect, useRef, useState } from "react";
import type { DesktopBridge, HistorySearchSnapshot } from "@peppy/desktop-ui/bridge-contracts";

type HistoryBridge = Pick<DesktopBridge, "get_history_search" | "start_history_search" | "cancel_history_search" | "subscribe">;

export type UseHistorySearchInput = {
  sessionKey: string;
  enabled: boolean;
  visible: boolean;
  bridge: HistoryBridge;
};

export type UseHistorySearchResult = {
  historySearch?: HistorySearchSnapshot;
  onSearchFurtherBack: () => Promise<void>;
  onCancelHistorySearch: () => Promise<void>;
};

type Activity = UseHistorySearchInput & { generation: number };
type Owner = {
  sessionKey: string;
  bridge: HistoryBridge;
  mutationSequence: number;
  attemptId?: string;
  operationId?: string;
  start?: Promise<void>;
  cancel?: Promise<void>;
};

const POLL_INTERVAL_MS = 1_000;

function isActive(snapshot: HistorySearchSnapshot | undefined): boolean {
  return snapshot?.state === "catchingUp" || snapshot?.state === "waitingForPhone" || snapshot?.state === "indexing";
}

function isTerminal(snapshot: HistorySearchSnapshot): boolean {
  return snapshot.state === "partial" || snapshot.state === "completed" || snapshot.state === "blocked" || snapshot.state === "failed" || snapshot.state === "cancelled";
}

/** Adopts an active attempt that a view-scoped read reports, so Cancel can act on work this view did not start. */
function adoptActiveAttempt(snapshot: HistorySearchSnapshot, owner: Owner): void {
  if (!isActive(snapshot) || snapshot.attemptId === undefined || owner.start || owner.cancel || owner.attemptId === snapshot.attemptId) return;
  owner.attemptId = snapshot.attemptId;
}

function isSameContext(current: Pick<Owner, "sessionKey" | "bridge">, expected: Pick<Owner, "sessionKey" | "bridge">): boolean {
  return current.sessionKey === expected.sessionKey && current.bridge === expected.bridge;
}

/** Observes host-owned history retrieval without initiating work until the user asks. */
export function useHistorySearch({ sessionKey, enabled, visible, bridge }: UseHistorySearchInput): UseHistorySearchResult {
  const [historySearch, setHistorySearch] = useState<HistorySearchSnapshot>();
  const [documentVisible, setDocumentVisible] = useState(() => document.visibilityState === "visible");
  const mountedRef = useRef(true);
  const activityRef = useRef<Activity>({ sessionKey, enabled, visible, bridge, generation: 0 });
  const snapshotRef = useRef<HistorySearchSnapshot | undefined>(undefined);
  const ownerRef = useRef<Owner>({ sessionKey, bridge, mutationSequence: 0 });
  const readRef = useRef<{ generation: number; pending: boolean; queued: boolean }>({ generation: 0, pending: false, queued: false });

  const canUpdateView = useCallback((generation: number): boolean => {
    const activity = activityRef.current;
    return mountedRef.current && activity.generation === generation && activity.enabled && activity.visible && document.visibilityState === "visible";
  }, []);

  const settleOwnedTerminal = useCallback((snapshot: HistorySearchSnapshot, owner: Owner) => {
    if (!isTerminal(snapshot) || owner.attemptId === undefined || owner.attemptId !== snapshot.attemptId) return;
    owner.attemptId = undefined;
    delete owner.operationId;
  }, []);

  const applyRead = useCallback((snapshot: HistorySearchSnapshot, generation: number, mutationSequence: number) => {
    const activity = activityRef.current;
    const owner = ownerRef.current;
    if (!canUpdateView(generation) || !isSameContext(owner, activity) || owner.mutationSequence !== mutationSequence) return;
    adoptActiveAttempt(snapshot, owner);
    snapshotRef.current = snapshot;
    settleOwnedTerminal(snapshot, owner);
    setHistorySearch(snapshot);
  }, [canUpdateView, settleOwnedTerminal]);

  const readSnapshot = useCallback((generation: number) => {
    const activity = activityRef.current;
    if (!canUpdateView(generation) || readRef.current.pending && readRef.current.generation === generation) return;
    const mutationSequence = ownerRef.current.mutationSequence;
    readRef.current = { generation, pending: true, queued: false };
    void activity.bridge.get_history_search()
      .then(snapshot => applyRead(snapshot, generation, mutationSequence))
      .catch(() => undefined)
      .finally(() => {
        if (readRef.current.generation !== generation) return;
        const queued = readRef.current.queued;
        readRef.current.pending = false;
        readRef.current.queued = false;
        if (queued) readSnapshot(generation);
      });
  }, [applyRead, canUpdateView]);

  const scheduleRead = useCallback((generation: number) => {
    if (!canUpdateView(generation)) return;
    if (readRef.current.pending && readRef.current.generation === generation) {
      readRef.current.queued = true;
      return;
    }
    if (readRef.current.queued && readRef.current.generation === generation) return;
    readRef.current = { generation, pending: false, queued: true };
    queueMicrotask(() => {
      if (readRef.current.generation === generation && readRef.current.queued) readSnapshot(generation);
    });
  }, [canUpdateView, readSnapshot]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    const updateDocumentVisibility = () => {
      const nowVisible = document.visibilityState === "visible";
      setDocumentVisible(nowVisible);
      if (nowVisible) readSnapshot(activityRef.current.generation);
    };
    document.addEventListener("visibilitychange", updateDocumentVisibility);
    return () => document.removeEventListener("visibilitychange", updateDocumentVisibility);
  }, [readSnapshot]);

  useEffect(() => {
    const previous = activityRef.current;
    const generation = previous.generation + 1;
    const activity = { sessionKey, enabled, visible, bridge, generation };
    activityRef.current = activity;
    if (!isSameContext(ownerRef.current, activity)) {
      ownerRef.current = { sessionKey, bridge, mutationSequence: 0 };
      snapshotRef.current = undefined;
      setHistorySearch(undefined);
    }
    if (!enabled || !visible) return;

    readSnapshot(generation);
    const unsubscribe = bridge.subscribe(() => scheduleRead(generation));
    return unsubscribe;
  }, [bridge, enabled, readSnapshot, scheduleRead, sessionKey, visible]);

  useEffect(() => {
    const activity = activityRef.current;
    if (!enabled || !visible || !documentVisible || !isActive(historySearch)) return;
    const timer = window.setInterval(() => readSnapshot(activity.generation), POLL_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [documentVisible, enabled, historySearch, readSnapshot, visible]);

  const onSearchFurtherBack = useCallback((): Promise<void> => {
    const activity = activityRef.current;
    const owner = ownerRef.current;
    const bound = { sessionKey, bridge };
    if (!activity.enabled || !activity.visible || !isSameContext(activity, bound) || !isSameContext(owner, bound)) return Promise.resolve();
    if (owner.start) return owner.start;
    if (!snapshotRef.current || !snapshotRef.current.canSearchFurther || isActive(snapshotRef.current)) return Promise.resolve();

    const sequence = ++owner.mutationSequence;
    const operationId = owner.operationId ?? crypto.randomUUID();
    owner.operationId = operationId;
    const start = bridge.start_history_search({ operationId })
      .then(snapshot => {
        if (!isSameContext(ownerRef.current, bound) || ownerRef.current.mutationSequence !== sequence) return;
        owner.mutationSequence += 1;
        if (isTerminal(snapshot)) {
          owner.attemptId = undefined;
          delete owner.operationId;
        } else {
          owner.attemptId = snapshot.attemptId;
        }
        if (!canUpdateView(activity.generation)) return;
        snapshotRef.current = snapshot;
        setHistorySearch(snapshot);
      })
      .finally(() => {
        if (owner.start === start) owner.start = undefined;
      });
    owner.start = start;
    return start;
  }, [bridge, canUpdateView, sessionKey, settleOwnedTerminal]);

  const onCancelHistorySearch = useCallback((): Promise<void> => {
    const activity = activityRef.current;
    const owner = ownerRef.current;
    const snapshot = snapshotRef.current;
    const bound = { sessionKey, bridge };
    if (!activity.enabled || !activity.visible || !isSameContext(activity, bound) || !isSameContext(owner, bound) || !owner.attemptId || snapshot?.attemptId !== owner.attemptId || !isActive(snapshot)) return Promise.resolve();
    if (owner.cancel) return owner.cancel;

    const attemptId = owner.attemptId;
    const sequence = ++owner.mutationSequence;
    const cancel = bridge.cancel_history_search({ attemptId })
      .then(next => {
        if (!isSameContext(ownerRef.current, bound) || ownerRef.current.mutationSequence !== sequence || owner.attemptId !== attemptId) return;
        owner.mutationSequence += 1;
        settleOwnedTerminal(next, owner);
        if (!canUpdateView(activity.generation)) return;
        snapshotRef.current = next;
        setHistorySearch(next);
      })
      .finally(() => {
        if (owner.cancel === cancel) owner.cancel = undefined;
      });
    owner.cancel = cancel;
    return cancel;
  }, [bridge, canUpdateView, sessionKey, settleOwnedTerminal]);

  return { historySearch, onSearchFurtherBack, onCancelHistorySearch };
}
