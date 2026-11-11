import { useEffect, useRef, useState, type ReactElement } from "react";
import { Info, Modal } from "@peppy/desktop-ui";

type DraftExpiryHelpProps = {
  draftId: string;
  label: string;
  nearExpiry: boolean;
};

export function DraftExpiryHelp({ draftId, label, nearExpiry }: DraftExpiryHelpProps): ReactElement {
  const [helpOpen, setHelpOpen] = useState(false);
  const opener = useRef<HTMLElement>(null);
  const composerTextarea = useRef<HTMLElement>(null);
  useEffect(() => {
    setHelpOpen(false);
  }, [draftId]);
  return <>
    <span data-draft-expiry-warning={nearExpiry || undefined}><span data-inline-status-message title={label}>{label}</span><button id="draft-expiry-info-button" data-draft-expiry-info-button type="button" aria-label="About draft expiry" aria-haspopup="dialog" title="About draft expiry" onClick={(event) => { opener.current = event.currentTarget; composerTextarea.current = event.currentTarget.closest("#composer-field")?.querySelector<HTMLTextAreaElement>("#composer-textarea") ?? null; setHelpOpen(true); }}><Info size={16} aria-hidden="true" /></button></span>
    {helpOpen && <div id="draft-expiry-help" onKeyDownCapture={(event) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      setHelpOpen(false);
    }}><Modal id="draft-expiry-info-dialog" title="Draft expiry" opener={opener} focusFallback={composerTextarea} onDismiss={() => setHelpOpen(false)} actions={<button id="draft-expiry-info-dismiss" className="secondary-button" type="button" onClick={() => setHelpOpen(false)}>Close</button>}>
      <div id="draft-expiry-info-body" data-draft-expiry-info-body>
        <p>Images stored online only for a draft are permanently deleted 30 days after the server saves a change to its caption or images.</p>
        <p>Opening the draft or retrying an upload does not extend this time.</p>
        <p>This expiry does not delete sent images, public copies, or images still used by another active draft.</p>
      </div>
    </Modal></div>}
  </>;
}
