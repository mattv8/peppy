import { peppyCopy } from "../generated/peppyCopy";

export function RcsFeaturesSettings({ typingShare }: { typingShare: { enabled: boolean; onChange(next: boolean): void; error?: boolean } }) {
  return <section id="settings-rcs-features" className="settings-card" data-settings-section="rcs-features" aria-labelledby="settings-rcs-features-title">
    <h2 id="settings-rcs-features-title">RCS Features</h2>
    <div id="settings-share-typing-group" data-setting="share-typing">
      <div id="settings-share-typing-row" className="settings-check-row">
        <label className="settings-check">
          <input id="settings-share-typing" type="checkbox" aria-describedby={typingShare.error ? "settings-share-typing-error" : undefined} checked={typingShare.enabled} onChange={(event) => typingShare.onChange(event.target.checked)} />
          {peppyCopy.typing_share_label}
        </label>
      </div>
      {typingShare.error && <p id="settings-share-typing-error" className="settings-error" role="alert">{peppyCopy.setting_update_error}</p>}
    </div>
  </section>;
}
