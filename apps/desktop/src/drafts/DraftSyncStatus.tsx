import { useEffect, useRef, useState } from "react";
import { formatDateTime, Info, Modal, type Attachment } from "@peppy/desktop-ui";
import type { Draft, DraftConflictView } from "@peppy/desktop-ui/bridge-contracts";

type DraftSyncStatusProps = {
  draft?: Draft;
  local: { text: string; recipientIds: string[]; attachmentIds: string[] };
  attachments: Record<string, Attachment>;
  offline: boolean;
  localSaveState: "idle" | "saving" | "failed";
  resolving?: boolean;
  onResolveConflict: (branchId: string | null) => void;
  onAcknowledgeNotice: () => void;
};

const DAY_MS = 24 * 60 * 60 * 1000;

function expiryLabel(expiresAt: string | null, now: number): { label: string; nearExpiry: boolean } | undefined {
  if (!expiresAt) return undefined;
  const date = new Date(expiresAt);
  if (Number.isNaN(date.getTime())) return undefined;
  const remaining = date.getTime() - now;
  if (remaining <= 0) return { label: "Draft expired; waiting to sync", nearExpiry: true };
  if (remaining <= DAY_MS) return { label: "Expires within 24 hours", nearExpiry: true };
  return { label: `Expires ${formatDateTime(expiresAt) ?? expiresAt}`, nearExpiry: remaining > 0 && remaining <= DAY_MS };
}

function VersionSummary({ id, label, text, recipientIds, attachmentIds, attachments, gatewayId, simId }: {
  id: string;
  label: string;
  text: string;
  recipientIds: string[];
  attachmentIds: string[];
  attachments: Record<string, Attachment>;
  gatewayId?: string;
  simId?: string;
}) {
  const hasSelectedRoute = Boolean(gatewayId || simId);
  return <li id={id} data-draft-version={label}>
    <strong>{label}</strong>
    <dl>
      <dt>Caption</dt><dd>{text || "No caption"}</dd>
      <dt>Images</dt><dd>{attachmentIds.length ? <ul>{attachmentIds.map(id => <li key={id}>{attachments[id]?.name ?? "Image details loading"}</li>)}</ul> : "No images"}</dd>
      <dt>Recipients</dt><dd>{recipientIds.length ? recipientIds.join(", ") : "No recipients"}</dd>
      <dt>Route</dt><dd>{hasSelectedRoute ? <><span>Selected phone and SIM</span><details id={`${id}-route-details`}><summary>Phone and SIM details</summary><dl><dt>Phone ID</dt><dd>{gatewayId ?? "Not specified"}</dd><dt>SIM ID</dt><dd>{simId ?? "Not specified"}</dd></dl></details></> : "Automatic selection"}</dd>
    </dl>
  </li>;
}

function Conflict({ conflict, local, attachments, onResolve, resolving }: {
  conflict: DraftConflictView;
  local: DraftSyncStatusProps["local"];
  attachments: Record<string, Attachment>;
  onResolve: (branchId: string | null) => void;
  resolving?: boolean;
}) {
  const branchHook = `draft-conflict-${conflict.branchId}`;
  const consequenceId = `${branchHook}-consequence`;
  return <section id={branchHook} data-draft-conflict={conflict.branchId} role="alert">
    <p>Another version of this draft needs review before sending. Choosing a version replaces the other versions.</p>
    <details id={`${branchHook}-review`}>
      <summary>Review versions</summary>
      <div data-draft-conflict-review>
        <ul>
          <VersionSummary id={`${branchHook}-local`} label="This device" {...local} attachments={attachments} />
          <VersionSummary id={`${branchHook}-other`} label="Other version" text={conflict.text} recipientIds={conflict.recipientIds} attachmentIds={conflict.attachmentIds} gatewayId={conflict.gatewayId} simId={conflict.simId} attachments={attachments} />
        </ul>
      </div>
      <p id={consequenceId}>Choosing a version replaces the other versions.</p>
      <div id={`${branchHook}-actions`} data-draft-conflict-actions>
        <button type="button" aria-describedby={consequenceId} disabled={resolving} onClick={() => onResolve(null)}>Keep this version</button>
        <button type="button" aria-describedby={consequenceId} disabled={resolving} onClick={() => onResolve(conflict.branchId)}>Choose other version</button>
      </div>
      {resolving && <p role="status">Resolving draft version…</p>}
    </details>
  </section>;
}

export function DraftSyncStatus({ draft, local, attachments, offline, localSaveState, resolving, onResolveConflict, onAcknowledgeNotice }: DraftSyncStatusProps) {
  const sync = draft?.sync;
  const [now, setNow] = useState(() => Date.now());
  const [expiryHelpOpen, setExpiryHelpOpen] = useState(false);
  const expiryInfoOpener = useRef<HTMLElement>(null);
  const composerTextarea = useRef<HTMLElement>(null);
  const expiry = expiryLabel(sync?.expiresAt ?? null, now);
  const hasExpiry = expiry !== undefined;
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(timer);
  }, []);
  useEffect(() => {
    setExpiryHelpOpen(false);
  }, [draft?.id, hasExpiry]);
  if (!sync) return null;
  const stateText = localSaveState === "failed" ? "Draft not saved on this device. Keep this window open."
    : localSaveState === "saving" ? "Saving draft…"
    : sync.state === "synced" ? "Draft synced"
    : sync.state === "syncing" ? "Syncing draft…"
    : sync.state === "saving" ? "Saving draft…"
    : sync.state === "paused" ? "Saved on this device. Sync paused."
    : sync.state === "failed" ? "Draft sync failed."
    : "Saved on this device";
  const notice = sync.notice === "restored-after-expiry" ? "Offline changes restored as a new draft."
    : sync.notice === "kept-after-sent" ? "Your offline edits were kept as a new draft."
    : undefined;
  const expiryDialog = expiryHelpOpen && expiry && <div id="draft-expiry-help" onKeyDownCapture={(event) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    setExpiryHelpOpen(false);
  }}><Modal id="draft-expiry-info-dialog" title="Draft expiry" opener={expiryInfoOpener} focusFallback={composerTextarea} onDismiss={() => setExpiryHelpOpen(false)} actions={<button id="draft-expiry-info-dismiss" className="secondary-button" type="button" onClick={() => setExpiryHelpOpen(false)}>Close</button>}>
      <div id="draft-expiry-info-body" data-draft-expiry-info-body>
        <p>Images stored online only for a draft are permanently deleted 30 days after the server saves a change to its caption or images.</p>
        <p>Opening the draft or retrying an upload does not extend this time.</p>
        <p>This expiry does not delete sent images, public copies, or images still used by another active draft.</p>
      </div>
    </Modal></div>;
  return <footer id="draft-sync-status" data-draft-sync-state={sync.state} data-draft-sync-offline={offline || undefined}>
    <span data-draft-sync-message aria-live="polite">{stateText}</span>
    {sync.sendRequiresConnection && offline && <span data-draft-send-connection>Reconnect to send this synced draft.</span>}
    {expiry && <span data-draft-expiry-warning={expiry.nearExpiry || undefined}><span>{expiry.label}</span><button id="draft-expiry-info-button" data-draft-expiry-info-button type="button" aria-label="About draft expiry" aria-haspopup="dialog" title="About draft expiry" onClick={(event) => { expiryInfoOpener.current = event.currentTarget; composerTextarea.current = event.currentTarget.closest("#composer-field")?.querySelector<HTMLTextAreaElement>("#composer-textarea") ?? null; setExpiryHelpOpen(true); }}><Info size={16} aria-hidden="true" /></button></span>}
    {sync.notice === "restored-after-expiry" && <span data-draft-expiry-renewed>New 30-day period</span>}
    {notice && <span data-draft-sync-notice>{notice} <button type="button" onClick={onAcknowledgeNotice}>Dismiss</button></span>}
    {sync.conflicts.map(conflict => <Conflict key={conflict.branchId} conflict={conflict} local={local} attachments={attachments} resolving={resolving} onResolve={onResolveConflict} />)}
    {expiryDialog}
  </footer>;
}
