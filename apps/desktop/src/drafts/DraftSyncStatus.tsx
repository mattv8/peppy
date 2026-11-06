import { useEffect, useState } from "react";
import { formatDateTime, type Attachment } from "@peppy/desktop-ui";
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
  return <li id={id} data-draft-version={label}>
    <strong>{label}</strong>
    <dl>
      <dt>Caption</dt><dd>{text || "No caption"}</dd>
      <dt>Images</dt><dd>{attachmentIds.length ? <ul>{attachmentIds.map(id => <li key={id}>{attachments[id]?.name ?? "Image details loading"}</li>)}</ul> : "No images"}</dd>
      <dt>Recipients</dt><dd>{recipientIds.length ? recipientIds.join(", ") : "No recipients"}</dd>
      <dt>Route</dt><dd>{gatewayId && simId ? `${gatewayId} / ${simId}` : "Automatic route"}</dd>
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
  return <section id={branchHook} data-draft-conflict={conflict.branchId} role="alert">
    <p>Another version of this draft needs review before sending. Choosing a version replaces the other versions.</p>
    <details id={`${branchHook}-review`}>
      <summary>Review versions</summary>
      <ul>
        <VersionSummary id={`${branchHook}-local`} label="This device" {...local} attachments={attachments} />
        <VersionSummary id={`${branchHook}-other`} label="Other version" text={conflict.text} recipientIds={conflict.recipientIds} attachmentIds={conflict.attachmentIds} gatewayId={conflict.gatewayId} simId={conflict.simId} attachments={attachments} />
      </ul>
      <div id={`${branchHook}-actions`} data-draft-conflict-actions>
        <button type="button" disabled={resolving} onClick={() => onResolve(null)}>Keep this version</button>
        <button type="button" disabled={resolving} onClick={() => onResolve(conflict.branchId)}>Choose other version</button>
      </div>
    </details>
  </section>;
}

export function DraftSyncStatus({ draft, local, attachments, offline, localSaveState, resolving, onResolveConflict, onAcknowledgeNotice }: DraftSyncStatusProps) {
  const sync = draft?.sync;
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 60_000);
    return () => window.clearInterval(timer);
  }, []);
  if (!sync) return null;
  const expiry = expiryLabel(sync.expiresAt, now);
  const stateText = localSaveState === "failed" ? "Draft not saved. Retry saving before closing."
    : localSaveState === "saving" ? "Saving draft…"
    : sync.state === "synced" ? "Draft synced"
    : sync.state === "syncing" ? "Syncing draft…"
    : sync.state === "saving" ? "Saving draft…"
    : sync.state === "paused" ? "Saved on this device. Sync paused."
    : sync.state === "failed" ? "Draft not saved. Retry saving before closing."
    : "Saved on this device";
  const notice = sync.notice === "restored-after-expiry" ? "Offline changes restored as a new draft."
    : sync.notice === "kept-after-sent" ? "Your offline edits were kept as a new draft."
    : undefined;
  return <footer id="draft-sync-status" data-draft-sync-state={sync.state} data-draft-sync-offline={offline || undefined}>
    <span data-draft-sync-message aria-live="polite">{stateText}</span>
    {sync.sendRequiresConnection && offline && <span data-draft-send-connection>Reconnect to send this synced draft.</span>}
    {expiry && <span data-draft-expiry-warning={expiry.nearExpiry || undefined}>{expiry.label}<details><summary>About draft expiry</summary><p>Draft-only remote images are permanently deleted 30 days after the server accepts a caption or attachment edit. Opening or retrying does not renew this. Sent media, public copies, and other live drafts are protected.</p></details></span>}
    {sync.notice === "restored-after-expiry" && <span data-draft-expiry-renewed>New 30-day period</span>}
    {notice && <span data-draft-sync-notice>{notice} <button type="button" onClick={onAcknowledgeNotice}>Dismiss</button></span>}
    {sync.conflicts.map(conflict => <Conflict key={conflict.branchId} conflict={conflict} local={local} attachments={attachments} resolving={resolving} onResolve={onResolveConflict} />)}
  </footer>;
}
