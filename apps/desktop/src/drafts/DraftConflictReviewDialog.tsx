import type { ReactElement, RefObject } from "react";
import { useEffect, useRef, useState } from "react";
import { Modal, type Attachment } from "@peppy/desktop-ui";
import type { DraftConflictView } from "@peppy/desktop-ui/bridge-contracts";
import { DraftVersionSummary } from "./DraftVersionSummary";
import "./draftReview.css";

export type DraftVersionContent = {
  text: string;
  recipientIds: string[];
  attachmentIds: string[];
  gatewayId?: string;
  simId?: string;
};

export type DraftResolutionResult = { ok: true } | { ok: false; message: string };

type DraftConflictReviewDialogProps = {
  draftId: string;
  local: DraftVersionContent;
  conflicts: DraftConflictView[];
  attachments: Record<string, Attachment>;
  opener: RefObject<HTMLElement | null>;
  focusFallback: RefObject<HTMLElement | null>;
  onResolve(branchId: string | null): Promise<DraftResolutionResult>;
  onClose(): void;
};

const LOCAL_VERSION = "local";

function contentFingerprint(local: DraftVersionContent, conflicts: DraftConflictView[]): string {
  return JSON.stringify({
    local,
    conflicts: conflicts.map(({ branchId, text, recipientIds, attachmentIds, gatewayId, simId }) => ({ branchId, text, recipientIds, attachmentIds, gatewayId, simId })),
  });
}

function consequenceFor(selection: string | undefined): string {
  if (!selection) return "Nothing is sent until you choose.";
  if (selection === LOCAL_VERSION) return "Keep this device’s version and replace the other versions of this draft. Nothing is sent.";
  return "Replace this device’s caption, images, recipients and phone/SIM selection with this version. Nothing is sent.";
}

export function DraftConflictReviewDialog({
  draftId,
  local,
  conflicts,
  attachments,
  opener,
  focusFallback,
  onResolve,
  onClose,
}: DraftConflictReviewDialogProps): ReactElement {
  const fingerprint = contentFingerprint(local, conflicts);
  const previousFingerprint = useRef(fingerprint);
  const resolving = useRef(false);
  const [selection, setSelection] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [changed, setChanged] = useState(false);
  const noConflicts = conflicts.length === 0;

  useEffect(() => {
    if (previousFingerprint.current === fingerprint) return;
    previousFingerprint.current = fingerprint;
    setSelection(undefined);
    setError(undefined);
    setChanged(true);
  }, [fingerprint]);

  const submitSelection = async () => {
    if (!selection || resolving.current) return;
    resolving.current = true;
    setBusy(true);
    setError(undefined);
    setChanged(false);
    const result = await onResolve(selection === LOCAL_VERSION ? null : selection);
    resolving.current = false;
    setBusy(false);
    if (result.ok) {
      onClose();
      return;
    }
    setSelection(undefined);
    setError(result.message);
  };

  const dismiss = () => {
    if (!busy) onClose();
  };

  const choice = (versionId: string, hookId: string, label: string) => {
    const inputId = `${hookId}-choice`;
    return <>
      <input id={inputId} type="radio" name={`draft-review-${draftId}`} value={versionId} checked={selection === versionId} onChange={() => setSelection(versionId)} />
      <h3><label htmlFor={inputId}>{label}</label></h3>
    </>;
  };

  return <div className="draft-review-dialog" onKeyDownCapture={(event) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    event.stopPropagation();
    dismiss();
  }}>
    <Modal
      id="draft-conflict-review-dialog"
      title="Review draft versions"
      onDismiss={dismiss}
      busy={busy}
      opener={opener}
      focusFallback={focusFallback}
      className="draft-review-modal"
      actions={noConflicts ? <div data-draft-review-actions className="draft-review-actions">
        <div className="draft-review-buttons"><button type="button" className="secondary-button" onClick={dismiss}>Close</button></div>
      </div> : <div data-draft-review-actions className="draft-review-actions">
        {changed && <p role="status" className="draft-review-state">The versions changed. Review them again before choosing.</p>}
        {error && <p role="alert" className="draft-review-state" data-tone="error">{error}</p>}
        {busy && <p role="status" className="draft-review-state">Applying selected version…</p>}
        <p id="draft-review-consequence" className="draft-review-consequence">{consequenceFor(selection)}</p>
        <div className="draft-review-buttons">
          <button type="button" className="secondary-button" onClick={dismiss} disabled={busy}>Cancel</button>
          <button type="button" className="primary-button" onClick={submitSelection} disabled={!selection || busy} aria-describedby="draft-review-consequence">Use selected version</button>
        </div>
      </div>}
    >
      <div data-draft-review-body className="draft-review-body">
        {noConflicts ? <p>These versions no longer need review.</p> : <fieldset className="draft-review-choices" disabled={busy}>
          <legend className="draft-review-instruction">Choose the version to keep in this draft.</legend>
          <div className="draft-review-versions">
            <DraftVersionSummary id="draft-review-version-local" heading={choice(LOCAL_VERSION, "draft-review-version-local", "This device")} {...local} attachments={attachments} />
            {conflicts.map((conflict, index) => {
              const hookId = `draft-review-version-${conflict.branchId}`;
              const label = conflicts.length === 1 ? "Other version" : `Other version ${index + 1}`;
              return <DraftVersionSummary key={conflict.branchId} id={hookId} heading={choice(conflict.branchId, hookId, label)} {...conflict} attachments={attachments} />;
            })}
          </div>
        </fieldset>}
      </div>
    </Modal>
  </div>;
}
