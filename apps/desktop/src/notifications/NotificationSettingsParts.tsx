import { useRef, useState, type ReactElement, type ReactNode } from "react";
import { Info, Modal } from "@peppy/desktop-ui";

type GroupProps = {
  id: string;
  title: string;
  children: ReactNode;
};

type HelpProps = {
  id: string;
  title: string;
  children: ReactNode;
};

export function NotificationSettingsGroup({ id, title, children }: GroupProps): ReactElement {
  return <section id={id} className="notification-settings-group" aria-labelledby={`${id}-title`}>
    <h3 id={`${id}-title`}>{title}</h3>
    {children}
  </section>;
}

export function NotificationSettingsHelp({ id, title, children }: HelpProps): ReactElement {
  const [open, setOpen] = useState(false);
  const opener = useRef<HTMLElement>(null);
  const helpName = `About ${title.charAt(0).toLowerCase()}${title.slice(1)}`;

  return <span id={`${id}-container`} className="notification-setting-help">
    <button
      id={`${id}-trigger`}
      className="info-button"
      type="button"
      aria-label={helpName}
      aria-haspopup="dialog"
      title={helpName}
      onClick={(event) => {
        opener.current = event.currentTarget;
        setOpen(true);
      }}
    >
      <Info size={16} aria-hidden="true" />
    </button>
    {open && <Modal
      id={`${id}-dialog`}
      title={title}
      opener={opener}
      onDismiss={() => setOpen(false)}
      actions={<button className="secondary-button" type="button" onClick={() => setOpen(false)}>Close</button>}
    >
      <div id={`${id}-body`} className="notification-settings-help-body">{children}</div>
    </Modal>}
  </span>;
}
