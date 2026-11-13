import { useState } from "react";
import type { DesktopBridge } from "../bridge/bridge";
import { peppyCopy } from "../generated/peppyCopy";

export const PRIVACY_POLICY_URL = "https://peppy.pro/privacy";
const GUIDES_URL = "https://peppy.pro/docs";

export function PrivacySettings({ browserHost, bridge }: { browserHost: boolean; bridge: Pick<DesktopBridge, "open_external_url"> }) {
  const [openError, setOpenError] = useState(false);
  const [guidesError, setGuidesError] = useState(false);

  const openPolicy = async () => {
    setOpenError(false);
    try {
      if (!bridge.open_external_url) throw new Error("missing opener");
      await bridge.open_external_url(PRIVACY_POLICY_URL);
    } catch { setOpenError(true); }
  };
  const openGuides = async () => {
    setGuidesError(false);
    try {
      if (!bridge.open_external_url) throw new Error("missing opener");
      await bridge.open_external_url(GUIDES_URL);
    } catch { setGuidesError(true); }
  };

  const scopeId = "settings-privacy-policy-scope";
  return <>
    <footer id="settings-privacy" data-settings-section="privacy-links">
      {browserHost ? (
        <a id="settings-privacy-policy-link" href={PRIVACY_POLICY_URL} target="_blank" rel="noopener noreferrer" aria-describedby={scopeId}>
          {peppyCopy.privacy_policy}<span className="visually-hidden"> (opens in a new tab)</span>
        </a>
      ) : (
        <button id="settings-privacy-policy-button" type="button" aria-describedby={scopeId} onClick={() => void openPolicy()}>
          {peppyCopy.privacy_policy}
        </button>
      )}
      <span id={scopeId} className="visually-hidden">{peppyCopy.privacy_policy_scope}</span>
      {browserHost ? (
        <a id="settings-guides-link" href={GUIDES_URL} target="_blank" rel="noopener noreferrer">
          {peppyCopy.guides}<span className="visually-hidden"> (opens in a new tab)</span>
        </a>
      ) : (
        <button id="settings-guides-button" type="button" onClick={() => void openGuides()}>{peppyCopy.guides}</button>
      )}
      {openError && <p id="settings-privacy-policy-error" className="settings-error" role="alert">Privacy Policy could not be opened.</p>}
      {guidesError && <p id="settings-guides-error" className="settings-error" role="alert">{peppyCopy.guides_open_failed}</p>}
    </footer>
  </>;

}
