import { useState } from "react";
import { bridge } from "../bridge/bridge";
import { peppyCopy } from "../generated/peppyCopy";
import { updateApi, useUpdateStatus } from "./updates";

const busyPhase = (phase: string) => ["checking", "downloading", "installing"].includes(phase);
const isLinuxPackage = (packageKind: string) => packageKind === "deb" || packageKind === "rpm";

export async function openReleaseNotes(url: string): Promise<void> {
  const parsed = new URL(url);
  if (parsed.protocol !== "https:" || parsed.hostname !== "github.com" || parsed.port || parsed.username || parsed.password || !parsed.pathname.startsWith("/mattv8/peppy/releases/")) {
    throw new Error("Release notes link was not recognized.");
  }
  if (!bridge.open_external_url) throw new Error("External links are unavailable.");
  await bridge.open_external_url(parsed.toString());
}

function statusText(status: NonNullable<ReturnType<typeof useUpdateStatus>["status"]>): string {
  if (status.phase === "disabled") return "Updates are unavailable for this build.";
  if (status.phase === "idle") return status.lastCheckedAt ? "No update is currently ready." : "No update check has run yet.";
  if (status.phase === "checking") return "Checking for updates…";
  if (status.phase === "downloading") return `Downloading ${status.availableVersion ?? "the update"}…`;
  if (status.phase === "ready") return `Version ${status.availableVersion ?? ""} is ready to install.`;
  if (status.phase === "installed") return `Version ${status.availableVersion ?? ""} is installed. Restart Peppy to use it.`;
  if (status.phase === "deferred") return `Restart into version ${status.availableVersion ?? ""} is deferred until ${status.deferredUntil ?? "later"}.`;
  if (status.phase === "authorization_required") return "An operating system authorization prompt is required before this update can install.";
  if (status.phase === "installing") return "Installing update…";
  return "";
}

function AppStoreUpdatesSettings({ currentVersion, error }: { currentVersion: string; error: string }) {
  return <section id="settings-updates" className="settings-card" data-settings-section="updates" data-distribution="app_store" aria-labelledby="settings-updates-title">
    <h2 id="settings-updates-title">Updates</h2>
    <p id="update-status" role="status" aria-live="polite">{peppyCopy.updates_app_store_delivered}</p>
    <p id="update-version"><strong>Peppy {currentVersion}</strong></p>
    {error && <p className="settings-error" role="alert">{error}</p>}
  </section>;
}

export function UpdatesSettings() {
  const { status, error, busy, refresh, run } = useUpdateStatus(true);
  const [notesError, setNotesError] = useState("");
  if (status?.distribution === "app_store") return <AppStoreUpdatesSettings currentVersion={status.currentVersion} error={error} />;
  const phaseBusy = busy || Boolean(status && busyPhase(status.phase));
  const actionDisabled = phaseBusy || !status;
  const disabledBuild = status?.phase === "disabled";
  const disabledChannel = status?.channel === "disabled";
  const restartLabel = status?.phase === "authorization_required" ? "Authorize and restart" : "Restart now";

  return <section id="settings-updates" className="settings-card" data-settings-section="updates" aria-labelledby="settings-updates-title" aria-busy={phaseBusy || undefined}>
    <h2 id="settings-updates-title">Updates</h2>
    <p id="update-status" role="status" aria-live="polite">{status ? statusText(status) : !error ? "Loading update status…" : ""}</p>
    {status && <>
      <p><strong>Peppy {status.currentVersion}</strong> · {status.channel}</p>
      {!disabledChannel && <p>Last checked: {status.lastCheckedAt ?? "Never checked"}</p>}
      {!disabledChannel && <div id="automatic-updates-row" className="settings-control-row"><label htmlFor="automatic-updates">Automatically download updates</label><span className="settings-switch"><input id="automatic-updates" type="checkbox" role="switch" checked={status.automatic} aria-checked={status.automatic} disabled={actionDisabled} onChange={(event) => void run(() => updateApi.setAutomatic(event.target.checked))} /><span aria-hidden /></span></div>}
      {(status.progressPercent !== null || status.phase === "downloading") && <progress id="update-progress" value={status.progressPercent ?? undefined} max={100} aria-label="Update download progress">{status.progressPercent ?? "Downloading"}</progress>}
      {status.phase === "error" && <p className="settings-error" role="alert">{status.message ?? "The update could not be completed."}</p>}
      {status.message && status.phase !== "error" && status.phase !== "installed" && <p className="settings-note">{status.message}</p>}
      {status.phase === "installed" && status.message && status.message !== statusText(status) && <p className="settings-error" role="alert">{status.message}</p>}
      {(status.phase === "ready" || status.phase === "deferred" || status.phase === "installed") && !status.canRestart && !status.message && <p className="settings-note">Restart is not available yet.</p>}
      {isLinuxPackage(status.packageKind) && !status.unattended && <p className="settings-note">Package updates require operating system authorization. An administrator can configure Peppy-only unattended updates.</p>}
      {!disabledBuild && <div className="settings-control-row" data-update-actions>
        {!disabledChannel && <button className="secondary-button" type="button" disabled={actionDisabled} onClick={() => void run(updateApi.check)}>{status.phase === "error" ? "Retry" : "Check now"}</button>}
        {(status.phase === "ready" || status.phase === "installed" || status.phase === "deferred" || status.phase === "authorization_required") && <button className="primary-button" type="button" disabled={actionDisabled || !status.canRestart} onClick={() => void run(updateApi.restart)}>{restartLabel}</button>}
        {(status.phase === "ready" || status.phase === "installed") && <button className="secondary-button" type="button" disabled={actionDisabled} onClick={() => void run(updateApi.defer)}>Later</button>}
        {status.releaseNotesUrl && <button className="secondary-button" type="button" disabled={phaseBusy} onClick={() => { setNotesError(""); void openReleaseNotes(status.releaseNotesUrl!).catch(() => setNotesError("Release notes could not be opened.")); }}>Release notes</button>}
      </div>}
    </>}
    {error && <p className="settings-error" role="alert">{error}</p>}
    {notesError && <p className="settings-error" role="alert">{notesError}</p>}
    {error && !status && <button className="secondary-button" type="button" disabled={busy} onClick={() => void refresh(true)}>Retry loading updates</button>}
  </section>;
}
