import { TriangleAlert } from "@peppy/desktop-ui";
import { useEffect, useState } from "react";
import { peppyCopy } from "../generated/peppyCopy";
import { isTauriUpdateHost, useUpdateStatus } from "../updates/updates";

/**
 * Reads the distribution only while this notice is mounted. `useUpdateStatus` fetches per caller,
 * so mounting it from `App` would add a status read on every launch, not only when history is too large.
 */
export function SyncTooLargeNotice({ browserHost, onCheckForUpdates, onReload }: { browserHost: boolean; onCheckForUpdates(): void; onReload(): void }): React.JSX.Element {
  const { status } = useUpdateStatus(!browserHost);
  const appStore = status?.distribution === "app_store";
  // The Updates card exists only on a Tauri update host and only when updates are not disabled for this build.
  const updatesShown = !browserHost && isTauriUpdateHost() && status !== null && status.phase !== "disabled";
  const [announced, setAnnounced] = useState(false);
  useEffect(() => { setAnnounced(true); }, []);
  const title = peppyCopy.sync_too_large_title;
  const body = browserHost ? peppyCopy.sync_too_large_body_browser : peppyCopy.sync_too_large_body_desktop;
  const action = browserHost
    ? { label: peppyCopy.sync_too_large_action_reload, run: onReload }
    : updatesShown && !appStore ? { label: peppyCopy.sync_too_large_action_updates, run: onCheckForUpdates } : undefined;
  return <section id="sync-too-large" className="sync-progress sync-too-large" role="region" aria-labelledby="sync-too-large-title" data-sync-issue="history-too-large">
    <TriangleAlert className="sync-progress-icon" size={14} strokeWidth={1.75} aria-hidden />
    <div className="sync-progress-main">
      <p id="sync-too-large-title" className="sync-progress-title">{title}</p>
      <p id="sync-too-large-body">{body}</p>
      {browserHost && <p id="sync-too-large-server-note">{peppyCopy.sync_too_large_browser_server}</p>}
      {appStore && <p id="sync-too-large-app-store-note">{peppyCopy.updates_app_store_delivered}</p>}
    </div>
    {action && <button id="sync-too-large-action" className="secondary-button" type="button" onClick={action.run}>
      {action.label}
    </button>}
    <span id="sync-too-large-announcement" className="sr-only" role="status" aria-live="polite">{announced ? title : ""}</span>
  </section>;
}
