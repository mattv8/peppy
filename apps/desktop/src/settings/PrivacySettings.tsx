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
    try {
      await openPrivacyPolicy();
    } catch {
      setOpenError(true);
    }
  };

  return <section id="settings-privacy" className="settings-card" data-settings-section="privacy" aria-labelledby="settings-privacy-title">
    <h2 id="settings-privacy-title">Privacy</h2>
    <p className="settings-note">{peppyCopy.privacy_policy_scope}</p>
    {browserHost ? (
      <a id="settings-privacy-policy-link" href={PRIVACY_POLICY_URL} target="_blank" rel="noopener noreferrer" className="secondary-button">
        {peppyCopy.privacy_policy}<span className="visually-hidden"> (opens in a new tab)</span>
      </a>
    ) : (
      <button id="settings-privacy-policy-button" className="secondary-button" type="button" onClick={() => void openPolicy()}>
        {peppyCopy.privacy_policy}
      </button>
    )}
    {openError && <p id="settings-privacy-policy-error" className="settings-error" role="alert">Privacy Policy could not be opened.</p>}
    {typingShare && <div id="settings-share-typing-group">
      <label className="settings-check">
        <input id="settings-share-typing" type="checkbox" checked={typingShare.enabled} aria-describedby="settings-share-typing-hint" onChange={(event) => typingShare.onChange(event.target.checked)} />
        {peppyCopy.typing_share_label}
      </label>
      <p id="settings-share-typing-hint" className="settings-field-hint">{peppyCopy.typing_share_hint}</p>
      {typingShare.error && <p id="settings-share-typing-error" className="settings-error" role="alert">{peppyCopy.setting_update_error}</p>}
    </div>}
  </section>;
}
