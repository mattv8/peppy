import { useState } from "react";
import { TriangleAlert } from "@peppy/desktop-ui";
import { peppyCopy } from "./generated/peppyCopy";
import type { SystemRibbonModel } from "./systemRibbonModel";

type SystemRibbonProps = {
  model: SystemRibbonModel;
  openFailed?: boolean;
  onAction?: () => void;
  onDismiss?: () => void;
};

export function SystemRibbon({ model, openFailed = false, onAction, onDismiss }: SystemRibbonProps) {
  return <section id="system-ribbon" className="system-ribbon" data-system-ribbon-kind={model.kind} data-system-ribbon-severity={model.severity}>
    <TriangleAlert size={16} aria-hidden />
    <div className="system-ribbon-text" data-system-ribbon-text>
      <strong>{model.title}</strong>
      <span>{model.body}</span>
      {openFailed && model.actionLabel !== null && <span className="system-ribbon-open-failed" data-system-ribbon-open-failed>{peppyCopy.toggle_android_link_unavailable}</span>}
    </div>
    {(model.actionLabel !== null && onAction) || (model.dismissible && onDismiss) ? <div className="app-notice-actions">
      {model.actionLabel !== null && onAction && <button
        id="system-ribbon-action"
        type="button"
        className="secondary-button"
        aria-label={`${model.actionLabel}, ${peppyCopy.system_banner_opens_in_browser}`}
        onClick={() => onAction()}
      >
        {model.actionLabel}
      </button>}
      {model.dismissible && onDismiss && <button
        id="system-ribbon-dismiss"
        type="button"
        className="app-notice-dismiss"
        onClick={() => onDismiss()}
      >
        {peppyCopy.system_banner_dismiss}
      </button>}
    </div> : null}
  </section>;
}

/** Head panel strip: icon and title only, one line. The head window has no main-window action. */
export function SystemRibbonStrip({ model }: { model: SystemRibbonModel }) {
  return <div id="system-ribbon" className="system-ribbon system-ribbon-strip" title={model.body} data-system-ribbon-compact data-system-ribbon-kind={model.kind} data-system-ribbon-severity={model.severity}>
    <TriangleAlert size={14} aria-hidden />
    <strong>{model.title}</strong>
  </div>;
}

/**
 * Polite live text, updated only when the slot's kind changes, so unchanged re-renders and
 * snooze expiry stay silent. Clears when the slot empties.
 */
export function SystemRibbonAnnouncement({ model }: { model: SystemRibbonModel | null }) {
  const kind = model?.kind ?? null;
  const [announced, setAnnounced] = useState<{ kind: string | null; text: string }>({ kind: null, text: "" });
  if (announced.kind !== kind) setAnnounced({ kind, text: model ? `${model.title}. ${model.body}` : "" });
  return <div id="system-ribbon-announcement" className="visually-hidden" role="status" aria-live="polite" aria-atomic="true">
    {announced.text}
  </div>;
}
