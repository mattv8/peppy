import { useRef, useState, type ReactElement } from "react";
import { Info, Modal } from "@peppy/desktop-ui";

type MmsSizeHelpProps = {
  estimatedSize: string;
  limitLabel: "Carrier limit" | "Default limit";
  limitSize: string;
  limitKilobytes?: number;
  recipientLimit: number;
  resizingImages?: boolean;
};

export function MmsSizeHelp({ estimatedSize, limitLabel, limitSize, limitKilobytes, recipientLimit, resizingImages = false }: MmsSizeHelpProps): ReactElement {
  const [helpOpen, setHelpOpen] = useState(false);
  const opener = useRef<HTMLElement>(null);
  const composerTextarea = useRef<HTMLElement>(null);

  return <>
    <button
      id="mms-size-info-button"
      type="button"
      aria-label="About message size"
      aria-haspopup="dialog"
      title="About message size"
      onClick={(event) => {
        opener.current = event.currentTarget;
        composerTextarea.current = event.currentTarget.closest("#composer-field")?.querySelector<HTMLTextAreaElement>("#composer-textarea") ?? null;
        setHelpOpen(true);
      }}
    >
      <Info size={16} aria-hidden="true" />
    </button>
    {helpOpen && <div data-mms-size-help onKeyDownCapture={(event) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      setHelpOpen(false);
    }}>
      <Modal id="mms-size-info-dialog" title="Message size" opener={opener} focusFallback={composerTextarea} onDismiss={() => setHelpOpen(false)} actions={<button className="secondary-button" type="button" onClick={() => setHelpOpen(false)}>Close</button>}>
        <dl id="mms-size-info-body">
          <div><dt>Estimated size</dt><dd>{estimatedSize}</dd></div>
          <div><dt>{limitLabel}</dt><dd>{limitSize}</dd></div>
          <div><dt>Recipients</dt><dd>up to {recipientLimit}</dd></div>
        </dl>
        {resizingImages && limitKilobytes && <p>Images are resized to fit your carrier&apos;s MMS limit ({limitKilobytes} KB here). When they still don&apos;t fit in one message, Peppy sends your text first, then the images in order.</p>}
      </Modal>
    </div>}
  </>;
}
