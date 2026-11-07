import { useEffect, useLayoutEffect, useRef, useState, type MouseEvent, type ReactElement } from "react";
import { X, formatDateTime, type Attachment } from "@peppy/desktop-ui";
import type { Draft } from "@peppy/desktop-ui/bridge-contracts";
import { DraftConflictReviewDialog, type DraftResolutionResult, type DraftVersionContent } from "./DraftConflictReviewDialog";
import { DraftExpiryHelp } from "./DraftExpiryHelp";

type DraftSyncStatusProps = {
  draft?: Draft;
  local: DraftVersionContent;
  attachments: Record<string, Attachment>;
  offline: boolean;
  localSaveState: "idle" | "saving" | "failed";
  onResolveConflict: (branchId: string | null) => Promise<DraftResolutionResult>;
  onAcknowledgeNotice: () => void;
  onAutoAcknowledgeNotice?: () => void;
};

type DraftSyncAttentionInput = Pick<DraftSyncStatusProps, "draft" | "offline" | "localSaveState"> & { now: number };
type DraftSyncNotice = NonNullable<Draft["sync"]>["notice"];

const DAY_MS = 24 * 60 * 60 * 1000;
const OTHER_DEVICE_NOTICE_MS = 4_000;

function expiryLabel(expiresAt: string | null, now: number): { label: string; nearExpiry: boolean } | undefined {
  if (!expiresAt) return undefined;
  const date = new Date(expiresAt);
  if (Number.isNaN(date.getTime())) return undefined;
  const remaining = date.getTime() - now;
  if (remaining <= 0) return { label: "Draft expired; waiting to sync", nearExpiry: true };
  if (remaining <= DAY_MS) return { label: "Expires within 24 hours", nearExpiry: true };
  return { label: `Expires ${formatDateTime(expiresAt) ?? expiresAt}`, nearExpiry: false };
}

/** Returns the current time, re-rendering when an expiry reaches its 24-hour warning or passes. */
export function useDraftExpiryClock(expiresAt: string | null | undefined): number {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const expiry = expiresAt ? Date.parse(expiresAt) : Number.NaN;
    if (Number.isNaN(expiry)) return;
    const current = Date.now();
    const threshold = [expiry - DAY_MS, expiry].find((instant) => instant >= current);
    if (threshold === undefined) return;
    const timer = window.setTimeout(() => setTick((value) => value + 1), Math.min(threshold - current + 1, DAY_MS));
    return () => window.clearTimeout(timer);
  }, [expiresAt, tick]);
  return Date.now();
}

export function draftSyncNeedsAttention({ draft, offline, localSaveState, now }: DraftSyncAttentionInput): boolean {
  const sync = draft?.sync;
  if (!sync) return false;
  return localSaveState === "failed"
    || sync.conflicts.length > 0
    || sync.state === "failed"
    || (sync.sendRequiresConnection && offline)
    || expiryLabel(sync.expiresAt, now)?.nearExpiry === true
    || (sync.notice !== null && sync.notice !== "updated-from-other-device");
}

function primaryMessage(props: Pick<DraftSyncStatusProps, "offline" | "localSaveState"> & { draft: Draft }): string | undefined {
  const sync = props.draft.sync!;
  if (props.localSaveState === "failed") return "Draft not saved on this device. Keep this window open.";
  if (sync.conflicts.length > 0) return "Draft versions need review";
  if (sync.state === "failed") return "Draft sync failed.";
  if (sync.sendRequiresConnection && props.offline) return "Reconnect to check this draft before sending.";
  return undefined;
}

function noticeText(notice: DraftSyncNotice): string | undefined {
  if (notice === "restored-after-expiry") return "Restored your offline changes.";
  if (notice === "kept-after-sent") return "Kept your offline edits as a new draft.";
  return undefined;
}

export function DraftSyncStatus({ draft, local, attachments, offline, localSaveState, onResolveConflict, onAcknowledgeNotice, onAutoAcknowledgeNotice }: DraftSyncStatusProps): ReactElement | null {
  const [reviewOpen, setReviewOpen] = useState(false);
  const reviewOpener = useRef<HTMLElement>(null);
  const composerTextarea = useRef<HTMLElement>(null);
  const reviewDraftId = useRef(draft?.id);
  const acknowledgeNoticeRef = useRef(onAutoAcknowledgeNotice ?? onAcknowledgeNotice);
  const sync = draft?.sync;
  const now = useDraftExpiryClock(sync?.expiresAt);
  const expiry = expiryLabel(sync?.expiresAt ?? null, now);

  useEffect(() => {
    acknowledgeNoticeRef.current = onAutoAcknowledgeNotice ?? onAcknowledgeNotice;
  }, [onAcknowledgeNotice, onAutoAcknowledgeNotice]);

  useLayoutEffect(() => {
    if (reviewDraftId.current === draft?.id) return;
    reviewDraftId.current = draft?.id;
    composerTextarea.current = null;
    setReviewOpen(false);
  }, [draft?.id]);

  useEffect(() => {
    if (sync?.notice !== "updated-from-other-device") return;
    const timer = window.setTimeout(() => acknowledgeNoticeRef.current(), OTHER_DEVICE_NOTICE_MS);
    return () => window.clearTimeout(timer);
  }, [draft?.id, sync?.notice]);

  const updatedFromOtherDevice = sync?.notice === "updated-from-other-device";
  if (!draft || !sync || (!updatedFromOtherDevice && !draftSyncNeedsAttention({ draft, offline, localSaveState, now }))) return null;

  const message = primaryMessage({ draft, offline, localSaveState });
  const notice = noticeText(sync.notice);
  const openReview = (event: MouseEvent<HTMLButtonElement>) => {
    reviewOpener.current = event.currentTarget;
    composerTextarea.current = event.currentTarget.closest("#composer-field")?.querySelector<HTMLElement>("#composer-textarea") ?? null;
    setReviewOpen(true);
  };

  return <footer id="draft-sync-status" data-draft-sync-state={sync.state} data-draft-sync-offline={offline || undefined}>
    {message && <span data-draft-sync-message aria-live="polite">{message}</span>}
    {sync.conflicts.length > 0 && <button id="draft-review-button" className="secondary-button" type="button" aria-haspopup="dialog" onClick={openReview}>Review</button>}
    {expiry?.nearExpiry && <DraftExpiryHelp draftId={draft.id} label={expiry.label} nearExpiry />}
    {updatedFromOtherDevice && <span role="status" data-draft-sync-notice>Updated from your other device.</span>}
    {notice && <span data-draft-sync-notice>{notice} <button type="button" aria-label="Dismiss" onClick={onAcknowledgeNotice}><X size={16} strokeWidth={1.75} aria-hidden /></button></span>}
    {reviewOpen && sync.conflicts.length > 0 && <DraftConflictReviewDialog draftId={draft.id} local={local} conflicts={sync.conflicts} attachments={attachments} opener={reviewOpener} focusFallback={composerTextarea} onResolve={onResolveConflict} onClose={() => setReviewOpen(false)} />}
  </footer>;
}
