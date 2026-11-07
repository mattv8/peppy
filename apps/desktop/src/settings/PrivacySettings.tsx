import { useState } from "react";
import { peppyCopy } from "../generated/peppyCopy";

export const PRIVACY_POLICY_URL = "https://peppy.pro/privacy";

export async function openPrivacyPolicy(): Promise<void> {
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  await openUrl(PRIVACY_POLICY_URL);
}

export function PrivacySettings({ browserHost }: { browserHost: boolean }) {
  const [openError, setOpenError] = useState(false);
  const openPolicy = async () => {
    setOpenError(false);
    try { await openPrivacyPolicy(); } catch { setOpenError(true); }
  };
  const scopeId = "settings-privacy-policy-scope";
  return (
    <footer id="settings-privacy" data-settings-section="privacy-links">
      {browserHost ? (
        <a
          id="settings-privacy-policy-link"
          href={PRIVACY_POLICY_URL}
          target="_blank"
          rel="noopener noreferrer"
          aria-describedby={scopeId}
        >
          {peppyCopy.privacy_policy}
          <span className="visually-hidden"> (opens in a new tab)</span>
        </a>
      ) : (
        <button
          id="settings-privacy-policy-button"
          type="button"
          aria-describedby={scopeId}
          onClick={() => void openPolicy()}
        >
          {peppyCopy.privacy_policy}
        </button>
      )}
      <span id={scopeId} className="visually-hidden">{peppyCopy.privacy_policy_scope}</span>
      {openError && <p id="settings-privacy-policy-error" className="settings-error" role="alert">Privacy Policy could not be opened.</p>}
    </footer>
  );
}
