import { useState } from "react";
import { updateApi, useUpdateStatus } from "./updates";

const busyPhase = (phase: string) => ["checking", "downloading", "installing"].includes(phase);
const isLinuxPackage = (packageKind: string) => packageKind === "deb" || packageKind === "rpm";

export async function openReleaseNotes(url: string): Promise<void> {
  const parsed = new URL(url);
  if (parsed.protocol !== "https:" || parsed.hostname !== "github.com" || parsed.port || parsed.username || parsed.password || !parsed.pathname.startsWith("/mattv8/peppy/releases/")) {
    throw new Error("Release notes link was not recognized.");
  }
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  await openUrl(parsed.toString());
}

function statusText(status: NonNullable<ReturnType<typeof useUpdateStatus>["status"]>): string {
  if (status.phase === "disabled") return "Updates are unavailable for this build.";
  if (status.phase === "idle") return status.lastCheckedAt ? "No update is currently ready." : "No update check has run yet.";
  if (status.phase === "checking") return "Checking for updates…";
  if (status.phase === "downloading") return `Downloading ${status.availableVersion ?? "the update"}…`;
  if (status.phase === "ready") return `Version ${status.availableVersion ?? ""} is ready to install.`;
  if (status.phase === "deferred") return `Version ${status.availableVersion ?? ""} is ready. Restart is deferred until ${status.deferredUntil ?? "later"}.`;
  if (status.phase === "authorization_required") return "An operating system authorization prompt is required before this update can install.";
  if (status.phase === "installing") return "Installing update…";
  return "";
}

export function UpdatesSettings() {
  const { status, error, busy, refresh, run } = useUpdateStatus(true);
  const [notesError, setNotesError] = useState("");
  const phaseBusy = busy || Boolean(status && busyPhase(status.phase));
  const actionDisabled = phaseBusy || !status;
  const disabledBuild = status?.phase === "disabled";
  const restartLabel = status?.phase === "authorization_required" ? "Authorize and restart" : "Restart now";

  return <section id="settings-updates" className="settings-card" data-settings-section="updates" aria-labelledby="settings-updates-title" aria-busy={phaseBusy || undefined}>
    <h2 id="settings-updates-title">Updates</h2>
    <p id="update-status" role="status" aria-live="polite">{status ? statusText(status) : !error ? "Loading update status…" : ""}</p>
    {status && <>
      <p><strong>Peppy {status.currentVersion}</strong> · {status.channel}</p>
      {!disabledBuild && <p>Last checked: {status.lastCheckedAt ?? "Never checked"}</p>}
      {!disabledBuild && <label className="settings-check"><input id="automatic-updates" type="checkbox" checked={status.automatic} disabled={actionDisabled} onChange={(event) => void run(() => updateApi.setAutomatic(event.target.checked))} /> Automatically download updates</label>}
      {(status.progressPercent !== null || status.phase === "downloading") && <progress id="update-progress" value={status.progressPercent ?? undefined} max={100} aria-label="Update download progress">{status.progressPercent ?? "Downloading"}</progress>}
      {status.phase === "error" && <p role="alert">{status.message ?? "The update could not be completed."}</p>}
      {status.message && status.phase !== "error" && <p className="settings-note">{status.message}</p>}
      {(status.phase === "ready" || status.phase === "deferred") && !status.canRestart && !status.message && <p className="settings-note">Restart is not available yet.</p>}
      {isLinuxPackage(status.packageKind) && !status.unattended && <p className="settings-note">Package updates require operating system authorization. An administrator can configure Peppy-only unattended updates.</p>}
      {!disabledBuild && <div className="settings-control-row" data-update-actions>
        <button className="secondary-button" type="button" disabled={actionDisabled} onClick={() => void run(updateApi.check)}>{status.phase === "error" ? "Retry" : "Check now"}</button>
        {(status.phase === "ready" || status.phase === "deferred" || status.phase === "authorization_required") && <button className="primary-button" type="button" disabled={actionDisabled || !status.canRestart} onClick={() => void run(updateApi.restart)}>{restartLabel}</button>}
        {status.phase === "ready" && <button className="secondary-button" type="button" disabled={actionDisabled} onClick={() => void run(updateApi.defer)}>Later</button>}
        {status.releaseNotesUrl && <button className="secondary-button" type="button" disabled={phaseBusy} onClick={() => { setNotesError(""); void openReleaseNotes(status.releaseNotesUrl!).catch(() => setNotesError("Release notes could not be opened.")); }}>Release notes</button>}
      </div>}
    </>}
    {error && <p role="alert">{error}</p>}
    {notesError && <p role="alert">{notesError}</p>}
    {error && <button className="secondary-button" type="button" disabled={busy} onClick={() => void refresh(true)}>Retry loading updates</button>}
  </section>;
}
