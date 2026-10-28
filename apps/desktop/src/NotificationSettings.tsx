import { useEffect, useState } from "react";
import type { AppFilter, MirroredNotification, NotificationCheck, NotificationPreferences } from "./bridge";
import { notificationAppName, notificationTuple, notificationSourceName } from "./Notifications";

function permissionMessage(check: NotificationCheck): string {
  if (check.status === "denied") return "Notifications are turned off for Peppy in your system settings, so no test was sent.";
  if (check.status === "granted") return check.testSent
    ? "Test notification sent. Peppy is allowed to show notifications. If nothing appears, check Focus or Do Not Disturb."
    : "Peppy is allowed to show notifications, but the test notification could not be sent.";
  if (check.status === "quiet") return check.testSent
    ? "Test notification sent, but Peppy's notifications are set to deliver quietly, so no banner may pop up."
    : "Peppy's notifications are set to deliver quietly, and the test notification could not be sent.";
  return check.testSent
    ? "Test notification sent, but this system doesn't report Peppy's notification permission. If nothing appears, check Peppy's notification settings and Focus or Do Not Disturb."
    : "This system doesn't report Peppy's notification permission, and the test notification could not be sent.";
}

export function NotificationSettings({ notifications, filters, sources, preferences, headsSupported, locked, soundSettings, onPreferences, onMute, onPermission, onOpenSettings }: {
  notifications: MirroredNotification[];
  filters: AppFilter[];
  sources: { id: string; name: string }[];
  preferences: NotificationPreferences;
  headsSupported: boolean;
  locked: boolean;
  soundSettings?: { load(): Promise<boolean>; save(enabled: boolean): Promise<void> };
  onPreferences(value: NotificationPreferences): Promise<void>;
  onMute(filter: AppFilter): Promise<void>;
  onPermission(): Promise<NotificationCheck | void>;
  onOpenSettings(): Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const [checking, setChecking] = useState(false);
  const [error, setError] = useState("");
  const [check, setCheck] = useState<NotificationCheck>();
  const [soundEnabled, setSoundEnabled] = useState<boolean>();
  const loadSound = soundSettings?.load;
  useEffect(() => {
    let current = true;
    setSoundEnabled(undefined);
    if (loadSound && !locked) {
      void loadSound().then(value => {
        if (current) setSoundEnabled(value);
      }).catch(() => {
        if (current) setError("Could not load notification sound settings. Reopen Settings to try again.");
      });
    }
    return () => { current = false; };
  }, [loadSound, locked]);
  // Observed app defaults never override authoritative synchronized mute choices.
  const apps = [...new Map([
    ...notifications.map(n => ({ sourceDeviceId: n.target.sourceDeviceId, packageName: n.packageName, appName: n.appName, muted: false })),
    ...filters,
  ].map(filter => [notificationTuple(filter.sourceDeviceId, filter.packageName), filter])).values()];
  const sourceIds = apps.map(app => app.sourceDeviceId);
  const hasDesktopBanners = preferences.messageBanners || preferences.mirroredBanners;
  const muteWithHeads = preferences.muteWithHeads ?? true;
  const lockedNoteId = locked ? "notification-banner-locked-note" : undefined;
  const headsDescribedBy = locked ? "notification-mute-with-heads-hint notification-banner-locked-note" : "notification-mute-with-heads-hint";
  const save = async (action: () => Promise<void>) => {
    if (busy) return;
    setBusy(true);
    setError("");
    try { await action(); }
    catch { setError("Could not save notification settings. Try again."); }
    finally { setBusy(false); }
  };
  // Vault-synced banner preferences cannot change while locked; per-app mutes are not affected.
  const savePreferences = (value: NotificationPreferences) => {
    if (locked) return;
    void save(() => onPreferences(value));
  };
  const checkPermission = async () => {
    if (busy) return;
    setBusy(true);
    setChecking(true);
    setError("");
    setCheck(undefined);
    try {
      const result = await onPermission();
      if (result) setCheck(result);
    }
    catch { setError("Could not send test notification. Try again."); }
    finally {
      setChecking(false);
      setBusy(false);
    }
  };
  const openSettings = async () => {
    if (busy) return;
    setBusy(true);
    setError("");
    try { await onOpenSettings(); }
    catch { setError("Could not open notification settings."); }
    finally { setBusy(false); }
  };
  return (
    <section id="notification-settings" className="settings-card" data-settings-section="notifications" aria-busy={busy}>
      <h2>Notification mirroring</h2>
      <p>Mirrored app notifications use encrypted device sync. Carrier SMS/MMS has separate security boundaries.</p>
      <h3>Desktop banners</h3>
      <p id="notification-banner-sync-note" className="settings-note">Banner choices apply to every computer and browser in this vault. System notification permission is set on each device.</p>
      {locked && <p id="notification-banner-locked-note" className="settings-note">Unlock Peppy to change banner settings.</p>}
      <div className="settings-control-row">
        <label htmlFor="notification-message-banners">SMS/MMS banners</label>
        <span className="settings-switch">
          <input id="notification-message-banners" type="checkbox" role="switch"
            checked={preferences.messageBanners} aria-checked={preferences.messageBanners} disabled={busy || locked} aria-describedby={lockedNoteId}
            onChange={event => savePreferences({ ...preferences, messageBanners: event.target.checked })} />
          <span aria-hidden />
        </span>
      </div>
      {preferences.messageBanners && headsSupported && <div id="notification-mute-with-heads-row" className="settings-control-row">
        <label htmlFor="notification-mute-with-heads">
          <span id="notification-mute-with-heads-label">Mute while a chat head is open</span>
          <span id="notification-mute-with-heads-hint" className="settings-note">Skip SMS/MMS banners for conversations that have a chat head, collapsed or expanded. Messages and unread counts still appear. Other conversations and app notifications are unaffected.</span>
        </label>
        <span className="settings-switch">
          <input id="notification-mute-with-heads" type="checkbox" role="switch" checked={muteWithHeads} aria-checked={muteWithHeads}
            aria-labelledby="notification-mute-with-heads-label" aria-describedby={headsDescribedBy} disabled={busy || locked}
            onChange={event => savePreferences({ ...preferences, muteWithHeads: event.target.checked })} />
          <span aria-hidden />
        </span>
      </div>}
      <div className="settings-control-row">
        <label htmlFor="notification-mirrored-banners">App notification banners</label>
        <span className="settings-switch">
          <input id="notification-mirrored-banners" type="checkbox" role="switch"
            checked={preferences.mirroredBanners} aria-checked={preferences.mirroredBanners} disabled={busy || locked} aria-describedby={lockedNoteId}
            onChange={event => savePreferences({ ...preferences, mirroredBanners: event.target.checked })} />
          <span aria-hidden />
        </span>
      </div>
      {hasDesktopBanners && <>
        {soundSettings && <div id="notification-sounds-row" className="settings-control-row">
          <label htmlFor="notification-sounds">Notification sounds</label>
          <span className="settings-switch">
            <input id="notification-sounds" type="checkbox" role="switch"
              checked={soundEnabled ?? false} aria-checked={soundEnabled ?? false}
              disabled={busy || locked || soundEnabled === undefined} aria-describedby="notification-sounds-hint"
              onChange={event => {
                if (locked || soundEnabled === undefined) return;
                const enabled = event.target.checked;
                void save(async () => { await soundSettings.save(enabled); setSoundEnabled(enabled); });
              }} />
            <span aria-hidden />
          </span>
        </div>}
        {soundSettings && <p id="notification-sounds-hint" className="settings-note">Play the system notification sound on this computer. System volume and Focus or Do Not Disturb still apply.</p>}
        <div className="settings-control-row">
          <label htmlFor="notification-preview">Banner preview</label>
          <select id="notification-preview" value={preferences.preview} disabled={busy || locked} aria-describedby={lockedNoteId}
            onChange={event => savePreferences({ ...preferences, preview: event.target.value as NotificationPreferences["preview"] })}>
            <option value="full">Full content</option>
            <option value="hidden">Hidden — generic banner only</option>
          </select>
        </div>
        <button id="notification-check" className="secondary-button" disabled={busy} onClick={() => void checkPermission()}>
          Send test notification
        </button>
        {(checking || check) && <p id="notification-permission-status" role="status">{checking ? "Sending test notification… If your system asks for permission, choose Allow." : permissionMessage(check!)}</p>}
        {check?.canOpenSettings && <button id="notification-open-settings" className="secondary-button" disabled={busy} onClick={() => void openSettings()}>
          Open notification settings
        </button>}
      </>}
      {error && <p role="alert">{error}</p>}
      {apps.length ? (
        <details id="notification-app-rules-disclosure" className="settings-disclosure">
          <summary>Per-app mirroring</summary>
          <p>Apps are mirrored by default. Changes apply on the phone&apos;s next sync.</p>
          <p className="settings-note">Notification history shares the vault&apos;s 100,000-record recovery limit. Mute noisy apps to limit storage growth.</p>
          <ul id="notification-app-filter-list" aria-label="App mirroring filters">
            {apps.map(filter => {
              const id = notificationTuple(filter.sourceDeviceId, filter.packageName);
              const phone = notificationSourceName(filter.sourceDeviceId, sources, sourceIds);
              const appName = notificationAppName(filter.appName, filter.packageName);
              return (
                <li key={id} data-device-id={filter.sourceDeviceId} data-package={filter.packageName} className="notif-app-filter-row">
                  <span className="notif-app-letter-avatar" aria-hidden>{appName.slice(0, 1).toUpperCase()}</span>
                  <span className="notif-app-filter-label"><b>{appName}</b><small>{phone}</small></span>
                  <span className="settings-switch">
                    <input id={`filter-${id}`} type="checkbox" role="switch" disabled={busy}
                      aria-label={`Mirror ${appName} from ${phone}`} checked={!filter.muted} aria-checked={!filter.muted}
                      onChange={event => void save(() => onMute({ ...filter, muted: !event.target.checked }))} />
                    <span aria-hidden />
                  </span>
                </li>
              );
            })}
          </ul>
        </details>
      ) : <p>No apps observed yet. Enable mirroring in the Android companion to begin.</p>}
    </section>
  );
}
