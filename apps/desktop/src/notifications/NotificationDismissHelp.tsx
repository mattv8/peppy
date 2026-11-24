import { useRef, useState, type KeyboardEvent, type ReactElement } from "react";
import { Info, Modal } from "@peppy/desktop-ui";
import type { NotificationDismissalBlockReason } from "@peppy/desktop-ui/bridge-contracts";
import { blockerCopy, WAITING_PANEL_TITLE, waitingHelpParagraphs } from "./notificationDismissCopy";

export type NotificationDismissHelpContent =
  | { kind: "waiting"; count: number }
  | { kind: "blocker"; reason: NotificationDismissalBlockReason; phoneName: string };

export type NotificationDismissHelpProps = {
  /** Id of the (i) trigger; the dialog derives its ids from it. */
  triggerId: string;
  triggerLabel: string;
  help: NotificationDismissHelpContent;
};

function helpBody(help: NotificationDismissHelpContent): { title: string; steps: string[]; paragraphs: string[] } {
  if (help.kind === "waiting") return { title: WAITING_PANEL_TITLE, steps: [], paragraphs: waitingHelpParagraphs(help.count) };
  const copy = blockerCopy(help.reason, help.phoneName, 0).help;
  return copy;
}

/** (i) trigger that opens a titled, dismissible dialog about dismissed notifications waiting for a phone. */
export function NotificationDismissHelp({ triggerId, triggerLabel, help }: NotificationDismissHelpProps): ReactElement {
  const [open, setOpen] = useState(false);
  const opener = useRef<HTMLButtonElement>(null);
  const body = helpBody(help);
  const close = () => setOpen(false);
  return <>
    <button id={triggerId} ref={opener} type="button" className="info-button" aria-label={triggerLabel} aria-haspopup="dialog" title={triggerLabel} aria-expanded={open} onClick={() => setOpen(true)}>
      <Info size={14} aria-hidden="true" />
    </button>
    {open && <span onKeyDownCapture={(event: KeyboardEvent<HTMLSpanElement>) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      close();
    }}>
      <Modal id={`${triggerId}-dialog`} title={body.title} opener={opener} onDismiss={close} actions={<button id={`${triggerId}-close`} className="secondary-button" type="button" onClick={close}>Close</button>}>
        <div id={`${triggerId}-body`} className="notif-dismiss-help-body">
          {body.steps.length > 0 && <ol>{body.steps.map(step => <li key={step}>{step}</li>)}</ol>}
          {body.paragraphs.map(paragraph => <p key={paragraph}>{paragraph}</p>)}
        </div>
      </Modal>
    </span>}
  </>;
}
