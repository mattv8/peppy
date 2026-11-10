import { useState } from "react";
import { peppyCopy } from "../generated/peppyCopy";

export const PRIVACY_POLICY_URL = "https://peppy.pro/privacy";

export async function openPrivacyPolicy(): Promise<void> {
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  await openUrl(PRIVACY_POLICY_URL);
}

export function PrivacySettings({ browserHost, typingShare }: { browserHost: boolean; typingShare?: { enabled: boolean; onChange(next: boolean): void; error?: boolean } }) {
  const [openError, setOpenError] = useState(false);

  const openPolicy = async () => {
    setOpenError(false);
    try { await openPrivacyPolicy(); } catch { setOpenError(true); }
  };

  const scopeId = "settings-privacy-policy-scope";
  return <>
    {typingShare && <section id="settings-sharing" className="settings-card" data-settings-section="sharing" aria-labelledby="settings-sharing-title">
      <h2 id="settings-sharing-title">Sharing</h2>
      <div id="settings-share-typing-group" data-setting="share-typing">
        <div id="settings-share-typing-row" className="settings-check-row">
          <label className="settings-check">
            <input id="settings-share-typing" type="checkbox" checked={typingShare.enabled} onChange={(event) => typingShare.onChange(event.target.checked)} />
            {peppyCopy.typing_share_label}
          </label>
        </div>
        {typingShare.error && <p id="settings-share-typing-error" className="settings-error" role="alert">{peppyCopy.setting_update_error}</p>}
      </div>
    </section>}
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
      {openError && <p id="settings-privacy-policy-error" className="settings-error" role="alert">Privacy Policy could not be opened.</p>}
    </footer>
  </>;

}
