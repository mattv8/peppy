import { useRef, useState } from "react";
import { ExternalLink } from "@peppy/desktop-ui";
import type { DesktopBridge } from "../bridge/bridge";
import { peppyCopy } from "../generated/peppyCopy";
import { isOpenableManageUrl } from "../notices/systemRibbonModel";

const FEEDBACK_PAGE_PATH = "/feedback";

/** Resolves the feedback page on a Peppy Hosted origin; undefined when the origin is not safe to open. */
export function feedbackPageUrl(origin: string | undefined): string | undefined {
  if (!origin || !isOpenableManageUrl(origin)) return undefined;
  const url = new URL(origin);
  if (url.username || url.password) return undefined;
  return new URL(FEEDBACK_PAGE_PATH, origin).href;
}

export function FeedbackSettings({ browserHost, accountUrl, bridge }: { browserHost: boolean; accountUrl?: string; bridge: Pick<DesktopBridge, "hosted_origin_url" | "open_external_url"> }) {
  const [openError, setOpenError] = useState(false);
  const [opening, setOpening] = useState(false);
  const inFlight = useRef(false);
  const browserUrl = browserHost ? feedbackPageUrl(accountUrl) : undefined;

  const open = async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    setOpening(true);
    setOpenError(false);
    try {
      const url = feedbackPageUrl(await bridge.hosted_origin_url());
      if (!url || !bridge.open_external_url) throw new Error("feedback page unavailable");
      await bridge.open_external_url(url);
    } catch { setOpenError(true); }
    finally {
      inFlight.current = false;
      setOpening(false);
    }
  };

  if (browserHost && !browserUrl) return null;

  return <div id="settings-feedback" data-settings-subsection="feedback">
    <h3 id="settings-feedback-title">{peppyCopy.settings_feedback_title}</h3>
    {browserHost
      ? <a id="settings-feedback-link" className="link-button" href={browserUrl} target="_blank" rel="noopener noreferrer" aria-describedby="settings-feedback-note"><ExternalLink size={13} aria-hidden />{peppyCopy.settings_feedback_cta}<span className="visually-hidden"> (opens in a new tab)</span></a>
      : <button id="settings-feedback-link" type="button" className="link-button" aria-describedby="settings-feedback-note" disabled={opening} aria-busy={opening || undefined} onClick={() => void open()}><ExternalLink size={13} aria-hidden />{peppyCopy.settings_feedback_cta}<span className="visually-hidden"> (opens in your browser)</span></button>}
    <p id="settings-feedback-note" className="settings-note">{peppyCopy.settings_feedback_note}</p>
    {openError && <p id="settings-feedback-error" className="settings-error" role="alert">{peppyCopy.settings_feedback_open_failed}</p>}
  </div>;
}
