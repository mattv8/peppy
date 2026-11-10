import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { RcsAttachmentState } from "@peppy/desktop-ui/bridge-contracts";
import type { RcsDownloadRetryBlock } from "./rcsRoute";
import { RCS_RETRY_COOLDOWN_MS, rcsRetryKey } from "./rcsMediaState";

/** "idle": no request; "requested": asked for, waiting; "retry-again": the cooldown ran out with no change. */
export type RcsRetryRequestState = "idle" | "requested" | "retry-again";

export type RcsMediaActionsValue = {
  retryBlock: RcsDownloadRetryBlock;
  requestState(messageId: string, attachmentIndex: number, state: RcsAttachmentState): RcsRetryRequestState;
  /** Marks the attachment requested, then runs the bridge call. Rejects and clears the mark if the call fails. */
  requestRetry(messageId: string, attachmentIndex: number, state: RcsAttachmentState): Promise<void>;
  openStorageSettings(): void;
};

type RetryRecord = { state: RcsAttachmentState; expired: boolean };

const RcsMediaActionsContext = createContext<RcsMediaActionsValue | null>(null);

export function useRcsMediaActions(): RcsMediaActionsValue | null {
  return useContext(RcsMediaActionsContext);
}

type RcsMediaActionsProviderProps = {
  retryBlock: RcsDownloadRetryBlock;
  retryDownload(messageId: string, attachmentIndex: number): Promise<void>;
  openStorageSettings(): void;
  children: ReactNode;
};

/** Keeps retry requests per attachment. A request is tied to the state it was made for, so a changed state ends it. */
export function RcsMediaActionsProvider({ retryBlock, retryDownload, openStorageSettings, children }: RcsMediaActionsProviderProps) {
  const [records, setRecords] = useState<Record<string, RetryRecord>>({});
  const timers = useRef(new Map<string, number>());
  useEffect(() => {
    const pending = timers.current;
    return () => {
      pending.forEach((timer) => window.clearTimeout(timer));
      pending.clear();
    };
  }, []);

  const requestState = useCallback((messageId: string, attachmentIndex: number, state: RcsAttachmentState): RcsRetryRequestState => {
    const record = records[rcsRetryKey(messageId, attachmentIndex)];
    if (!record || record.state !== state) return "idle";
    return record.expired ? "retry-again" : "requested";
  }, [records]);

  const requestRetry = useCallback(async (messageId: string, attachmentIndex: number, state: RcsAttachmentState) => {
    const key = rcsRetryKey(messageId, attachmentIndex);
    window.clearTimeout(timers.current.get(key));
    setRecords((current) => ({ ...current, [key]: { state, expired: false } }));
    const timer = window.setTimeout(() => {
      timers.current.delete(key);
      setRecords((current) => (current[key]?.state === state ? { ...current, [key]: { state, expired: true } } : current));
    }, RCS_RETRY_COOLDOWN_MS);
    timers.current.set(key, timer);
    try {
      await retryDownload(messageId, attachmentIndex);
    } catch (error) {
      window.clearTimeout(timer);
      timers.current.delete(key);
      setRecords((current) => {
        if (current[key]?.state !== state) return current;
        const next = { ...current };
        delete next[key];
        return next;
      });
      throw error;
    }
  }, [retryDownload]);

  const value = useMemo<RcsMediaActionsValue>(() => ({ retryBlock, requestState, requestRetry, openStorageSettings }), [retryBlock, requestState, requestRetry, openStorageSettings]);
  return <RcsMediaActionsContext.Provider value={value}>{children}</RcsMediaActionsContext.Provider>;
}
