import { useEffect, useRef, useState } from "react";
import { DEFAULT_NOTIFICATION_PERSISTENCE, notificationPersistence, type NotificationPersistence, type NotificationPersistenceSettings, type NotificationQuickReplySettings } from "@peppy/desktop-ui/bridge-contracts";
import type { AppFilter, MirroredNotification, NotificationCheck, NotificationPreferences } from "../bridge/bridge";
import { notificationAppName, notificationTuple, notificationSourceName } from "./Notifications";
import { NotificationSettingsGroup, NotificationSettingsHelp } from "./NotificationSettingsParts";

const LINUX_PERSISTENCE_PRESETS = [
  { value: 5, label: "5 seconds" },
  { value: 15, label: "15 seconds" },
  { value: 30, label: "30 seconds" },
] as const;
const DEFAULT_PERSISTENCE_SECONDS = DEFAULT_NOTIFICATION_PERSISTENCE.seconds ?? 5;

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

function formatPersistence(seconds: number): string {
  if (seconds < 60) return `${seconds} second${seconds === 1 ? "" : "s"}`;
  const minutes = Math.floor(seconds / 60);
  const remainder = seconds % 60;
  if (!remainder) return `${minutes} minute${minutes === 1 ? "" : "s"}`;
  return `${minutes} min ${remainder} s`;
}

function macosPersistenceStatus(style: NotificationPersistenceSettings["macosStyle"]): string {
  if (style === "banners") return "Current style: Banners. They fade on their own.";
  if (style === "alerts") return "Current style: Alerts. They stay until you dismiss them.";
  if (style === "off") return "Notifications are off for Peppy in macOS.";
  return "Peppy can't read this setting. Check Peppy's page in System Settings.";
}

export function NotificationSettings({ notifications, filters, sources, preferences, headsSupported, locked, soundSettings, persistenceSettings, quickReplySettings, onPreferences, onMute, onPermission, onOpenSettings }: {
  notifications: MirroredNotification[];
  filters: AppFilter[];
  sources: { id: string; name: string }[];
  preferences: NotificationPreferences;
  headsSupported: boolean;
  locked: boolean;
  soundSettings?: { load(): Promise<boolean>; save(enabled: boolean): Promise<void> };
  persistenceSettings?: { load(): Promise<NotificationPersistenceSettings>; save(value: NotificationPersistence): Promise<void> };
  quickReplySettings?: { load(): Promise<NotificationQuickReplySettings>; save(enabled: boolean): Promise<void> };
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
  const [persistence, setPersistence] = useState<NotificationPersistence>();
  const [persistenceSupported, setPersistenceSupported] = useState<boolean>();
  const [persistencePlatform, setPersistencePlatform] = useState<NonNullable<NotificationPersistenceSettings["platform"]>>("linux");
  const [macosStyle, setMacosStyle] = useState<NotificationPersistenceSettings["macosStyle"]>("unknown");
  const [persistenceError, setPersistenceError] = useState("");
  const [persistenceLoadError, setPersistenceLoadError] = useState("");
  const [quickReply, setQuickReply] = useState<NotificationQuickReplySettings>();
  const [quickReplyPending, setQuickReplyPending] = useState(false);
  const [quickReplyError, setQuickReplyError] = useState("");
  const lastFinitePersistence = useRef(DEFAULT_PERSISTENCE_SECONDS);
  const persistenceSavePending = useRef(false);
  const persistenceInitialLoadPending = useRef(false);
  const persistenceRefreshPending = useRef(false);
  const persistenceSaveToken = useRef(0);
  const persistenceSettingsRef = useRef(persistenceSettings);
  const persistenceMounted = useRef(true);
  persistenceSettingsRef.current = persistenceSettings;
  useEffect(() => {
    persistenceMounted.current = true;
    return () => { persistenceMounted.current = false; };
  }, []);
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
  const loadPersistence = persistenceSettings?.load;
  const loadQuickReply = quickReplySettings?.load;
  useEffect(() => {
    let current = true;
    setQuickReply(undefined);
    setQuickReplyError("");
    if (!loadQuickReply || locked) return () => { current = false; };
    void loadQuickReply().then(value => {
      if (current) setQuickReply(value);
    }).catch(() => {
      if (current) setQuickReplyError("Could not load quick reply settings. Reopen Settings to try again.");
    });
    return () => { current = false; };
  }, [loadQuickReply, locked]);
  const persistenceLoadAdapter = useRef<typeof loadPersistence>(undefined);
  useEffect(() => {
    let current = true;
    const adapterChanged = persistenceLoadAdapter.current !== loadPersistence;
    if (adapterChanged) {
      persistenceLoadAdapter.current = loadPersistence;
      persistenceSaveToken.current += 1;
      persistenceSavePending.current = false;
      setPersistenceError("");
      setPersistenceLoadError("");
      setPersistence(undefined);
      setPersistenceSupported(undefined);
    }
    if (!loadPersistence || locked || (!adapterChanged && persistenceSupported !== undefined)) return () => { current = false; };
    setPersistenceLoadError("");
    persistenceInitialLoadPending.current = true;
    void loadPersistence().then(value => {
      if (!current) return;
      setPersistenceSupported(value.supported);
      setPersistencePlatform(value.platform ?? "linux");
      setMacosStyle(value.macosStyle ?? "unknown");
      if (!value.supported) return;
      const loaded = notificationPersistence({ seconds: value.seconds }) ?? DEFAULT_NOTIFICATION_PERSISTENCE;
      setPersistence(loaded);
      if (loaded.seconds !== null) {
        lastFinitePersistence.current = loaded.seconds;
      }
    }).catch(() => {
      if (current) setPersistenceLoadError("Could not load banner settings. Reopen Settings to try again.");
    }).finally(() => { persistenceInitialLoadPending.current = false; });
    return () => { current = false; };
  }, [loadPersistence, locked, persistenceSupported]);
  useEffect(() => {
    if (persistencePlatform !== "macos" || locked || !loadPersistence) return;
    let current = true;
    const refresh = () => {
      if (document.visibilityState === "visible" && !persistenceInitialLoadPending.current && !persistenceRefreshPending.current) {
        persistenceRefreshPending.current = true;
        void loadPersistence().then(value => {
          if (current && value.supported && value.platform === "macos") {
            setMacosStyle(value.macosStyle ?? "unknown");
            setPersistence(notificationPersistence({ seconds: value.seconds }) ?? DEFAULT_NOTIFICATION_PERSISTENCE);
          }
        }).catch(() => undefined).finally(() => { persistenceRefreshPending.current = false; });
      }
    };
    window.addEventListener("focus", refresh);
    document.addEventListener("visibilitychange", refresh);
    return () => {
      current = false;
      window.removeEventListener("focus", refresh);
      document.removeEventListener("visibilitychange", refresh);
    };
  }, [loadPersistence, locked, persistencePlatform]);
  // Observed app defaults never override authoritative synchronized mute choices.
  const apps = [...new Map([
    ...notifications.map(n => ({ sourceDeviceId: n.target.sourceDeviceId, packageName: n.packageName, appName: n.appName, muted: false })),
    ...filters,
  ].map(filter => [notificationTuple(filter.sourceDeviceId, filter.packageName), filter])).values()];
  const sourceIds = apps.map(app => app.sourceDeviceId);
  const hasDesktopBanners = preferences.messageBanners || preferences.mirroredBanners;
  const hasLocalSettings = Boolean(
    (hasDesktopBanners && (soundSettings || (quickReplySettings && quickReply?.supported !== false) || persistenceSupported)) ||
    quickReplyError ||
    persistenceLoadError,
  );
  const hasQuickReplyPreviewHint = Boolean(hasDesktopBanners && quickReplySettings && quickReply?.supported && quickReply.enabled);
  const muteWithHeads = preferences.muteWithHeads ?? true;
  const previewMode = preferences.preview === "full" ? "full" : preferences.showSource ? "source" : "hidden";
  const lockedNoteId = locked ? "notification-banner-locked-note" : undefined;
  const quickReplyDescribedBy = ["notification-quick-reply-hint", lockedNoteId, quickReplyError && "notification-quick-reply-error"].filter(Boolean).join(" ") || undefined;
  const persistenceDescribedBy = ["notification-persistence-hint", lockedNoteId, persistenceError && "notification-persistence-error"].filter(Boolean).join(" ") || undefined;
  const clearErrors = () => {
    setError("");
    setPersistenceError("");
  };
  const save = async (action: () => Promise<void>) => {
    if (busy) return;
    setBusy(true);
    clearErrors();
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
    clearErrors();
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
    clearErrors();
    try { await onOpenSettings(); }
    catch { setError("Could not open notification settings."); }
    finally { setBusy(false); }
  };
  const savePersistence = async (value: NotificationPersistence) => {
    if (locked || persistenceSavePending.current || !persistenceSettings) return;
    const savedSettings = persistenceSettings;
    const isCurrentSave = () => persistenceMounted.current && persistenceSettingsRef.current === savedSettings;
    const saveToken = ++persistenceSaveToken.current;
    persistenceSavePending.current = true;
    setPersistenceError("");
    try {
      await savedSettings.save(value);
      if (!isCurrentSave()) return;
      setPersistence(value);
      if (value.seconds !== null) lastFinitePersistence.current = value.seconds;
    } catch {
      if (!isCurrentSave()) return;
      setPersistenceError("Could not save notification settings. Try again.");
    } finally {
      if (persistenceSaveToken.current === saveToken) {
        persistenceSavePending.current = false;
      }
    }
  };
  const persistenceSelection = persistence?.seconds === null ? "indefinite" : String(persistence?.seconds);
  const windowsSelection = persistence?.seconds === null ? "indefinite" : (persistence?.seconds ?? 0) <= 7 ? "5" : "25";
  const persistenceMode = persistencePlatform === "windows" ? windowsSelection === "5" ? "short" : windowsSelection === "25" ? "long" : "indefinite" : persistenceSelection;
  const selectPersistence = (selection: string) => {
    if (locked || persistenceSavePending.current || !persistence) return;
    const value = selection === "indefinite" ? { seconds: null } : { seconds: Number(selection) };
    if (value.seconds === persistence.seconds) return;
    void savePersistence(value);
  };
  useEffect(() => {
    if (!hasDesktopBanners) setPersistenceError("");
  }, [hasDesktopBanners]);
  return (
    <section id="notification-settings" className="settings-card" data-settings-section="notifications" aria-busy={busy}>
      <h2>Notification mirroring</h2>
      <p>Mirrored app notifications use encrypted device sync. Carrier SMS/MMS has separate security boundaries.</p>
      <NotificationSettingsGroup id="notification-banner-group" title="Desktop banners">
        <p id="notification-banner-sync-note" className="settings-note">Banner choices apply to every computer and browser in this vault. System notification permission is set on each device.</p>
        {locked && <p id="notification-banner-locked-note" className="settings-note">Unlock Peppy to change banner settings.</p>}
        <div className="settings-control-row">
          <span className="notification-setting-label"><label htmlFor="notification-message-banners">SMS/MMS banners</label></span>
          <span className="settings-switch">
            <input id="notification-message-banners" type="checkbox" role="switch"
              checked={preferences.messageBanners} aria-checked={preferences.messageBanners} disabled={busy || locked} aria-describedby={lockedNoteId}
              onChange={event => savePreferences({ ...preferences, messageBanners: event.target.checked })} />
            <span aria-hidden />
          </span>
        </div>
        {preferences.messageBanners && headsSupported && <div id="notification-mute-with-heads-row" className="settings-control-row">
          <span className="notification-setting-label"><label htmlFor="notification-mute-with-heads" id="notification-mute-with-heads-label">Mute while a chat head is open</label><NotificationSettingsHelp id="notification-mute-with-heads-help" title="Mute while a chat head is open"><p>Skip SMS/MMS banners for conversations that have a chat head, collapsed or expanded. Messages and unread counts still appear. Other conversations and app notifications are unaffected.</p></NotificationSettingsHelp></span>
          <span className="settings-switch">
            <input id="notification-mute-with-heads" type="checkbox" role="switch" checked={muteWithHeads} aria-checked={muteWithHeads}
              aria-labelledby="notification-mute-with-heads-label" aria-describedby={lockedNoteId} disabled={busy || locked}
              onChange={event => savePreferences({ ...preferences, muteWithHeads: event.target.checked })} />
            <span aria-hidden />
          </span>
        </div>}
        <div className="settings-control-row">
          <span className="notification-setting-label"><label htmlFor="notification-mirrored-banners">App notification banners</label></span>
          <span className="settings-switch">
            <input id="notification-mirrored-banners" type="checkbox" role="switch"
              checked={preferences.mirroredBanners} aria-checked={preferences.mirroredBanners} disabled={busy || locked} aria-describedby={lockedNoteId}
              onChange={event => savePreferences({ ...preferences, mirroredBanners: event.target.checked })} />
            <span aria-hidden />
          </span>
        </div>
        {hasDesktopBanners && <div id="notification-preview-row" className="settings-control-row">
          <span className="notification-setting-label"><label htmlFor="notification-preview">Banner preview</label><NotificationSettingsHelp id="notification-preview-help" title="Banner preview"><p>Choose whether banners show the full message, only who it is from, or a generic notice.</p></NotificationSettingsHelp></span>
          <select id="notification-preview" value={previewMode} disabled={busy || locked} aria-describedby={[lockedNoteId, hasQuickReplyPreviewHint ? "notification-quick-reply-preview-hint" : undefined].filter(Boolean).join(" ") || undefined}
            onChange={event => {
              const selection = event.target.value;
              savePreferences({ ...preferences, preview: selection === "full" ? "full" : "hidden", showSource: selection === "source" });
            }}>
            <option value="full">Full content</option>
            <option value="source">Sender or app only</option>
            <option value="hidden">Hidden — generic banner only</option>
          </select>
        </div>}
        {hasQuickReplyPreviewHint && <p id="notification-quick-reply-preview-hint" className="settings-note">
          {previewMode === "full" ? "Show the sender and message, with quick reply." : previewMode === "source" ? "Show who it is from. Choose Show message to reveal a message and reply." : "Show a generic banner. Choose Show message to reveal a message and reply."} Quick reply applies to message banners, not mirrored app notifications.
        </p>}
      </NotificationSettingsGroup>
      {hasLocalSettings && <NotificationSettingsGroup id="notification-local-group" title="On this computer">
        <p className="settings-note">These settings apply only to this computer.</p>
        {hasDesktopBanners && soundSettings && <div id="notification-sounds-row" className="settings-control-row">
          <span className="notification-setting-label"><label htmlFor="notification-sounds">Notification sounds</label><NotificationSettingsHelp id="notification-sounds-help" title="Notification sounds"><p>Play the system notification sound on this computer. System volume and Focus or Do Not Disturb still apply.</p></NotificationSettingsHelp></span>
          <span className="settings-switch">
            <input id="notification-sounds" type="checkbox" role="switch"
              checked={soundEnabled ?? false} aria-checked={soundEnabled ?? false}
              disabled={busy || locked || soundEnabled === undefined} aria-describedby={lockedNoteId}
              onChange={event => {
                if (locked || soundEnabled === undefined) return;
                const enabled = event.target.checked;
                void save(async () => { await soundSettings.save(enabled); setSoundEnabled(enabled); });
              }} />
            <span aria-hidden />
          </span>
        </div>}
        {hasDesktopBanners && quickReplySettings && quickReply?.supported !== false && <>
          <div id="notification-quick-reply-row" className="settings-control-row" aria-busy={quickReplyPending || (quickReply === undefined && !quickReplyError) || undefined}>
            <span className="notification-setting-label"><label htmlFor="notification-quick-reply-toggle">Quick reply in notifications</label><NotificationSettingsHelp id="notification-quick-reply-help" title="Quick reply in notifications"><p>Reply to messages from notifications on this computer. This applies only to message banners, not mirrored app notifications.</p></NotificationSettingsHelp></span>
            <span className="settings-switch">
              <input id="notification-quick-reply-toggle" type="checkbox" role="switch" checked={quickReply?.enabled ?? false} aria-checked={quickReply?.enabled ?? false}
                disabled={busy || locked || quickReplyPending || quickReply === undefined} aria-describedby={quickReplyDescribedBy}
                onChange={event => {
                  if (!quickReply || quickReplyPending || locked) return;
                  const enabled = event.target.checked;
                  setQuickReplyPending(true);
                  setQuickReplyError("");
                  void quickReplySettings.save(enabled).then(() => setQuickReply({ ...quickReply, enabled })).catch(() => {
                    setQuickReplyError("Could not save quick reply settings. Try again.");
                  }).finally(() => setQuickReplyPending(false));
                }} />
              <span aria-hidden />
            </span>
          </div>
          <p id="notification-quick-reply-hint" className="settings-note">Message banners only. Not available while Peppy is locked.</p>
          {quickReplyPending && <p role="status">Saving quick reply setting…</p>}
          {quickReply === undefined && !quickReplyError && <p role="status">Loading quick reply setting…</p>}
          {quickReplyError && <p id="notification-quick-reply-error" className="settings-error" role="alert">{quickReplyError}</p>}
        </>}
        {persistenceSettings && persistenceLoadError && !persistenceSupported && <p id="notification-persistence-load-error" className="settings-note" role="alert">{persistenceLoadError}</p>}
        {!hasDesktopBanners && quickReplyError && <p id="notification-quick-reply-error" className="settings-error" role="alert">{quickReplyError}</p>}
        {hasDesktopBanners && persistenceSettings && persistenceSupported && persistence && persistencePlatform === "macos" && <>
          <div id="notification-persistence-row" className="settings-control-row" data-persistence-platform="macos" data-persistence-mode={macosStyle ?? "unknown"} role="group" aria-labelledby="notification-persistence-label">
            <span className="notification-setting-label"><span id="notification-persistence-label">Banner style</span></span>
            <button id="notification-persistence-open-settings" className="secondary-button" type="button" aria-describedby="notification-persistence-status" disabled={busy} onClick={() => void openSettings()}>Open System Settings</button>
          </div>
          <p id="notification-persistence-status" className="settings-note" role="status">{macosPersistenceStatus(macosStyle)}</p>
          <p id="notification-persistence-hint" className="settings-note">macOS decides how long banners stay. Change it under Notifications in System Settings.</p>
        </>}
        {hasDesktopBanners && persistenceSettings && persistenceSupported && persistence && persistencePlatform === "browser" && <>
          <div id="notification-persistence-row" className="settings-control-row" data-persistence-platform="browser" data-persistence-mode={persistenceMode}>
            <span className="notification-setting-label"><label htmlFor="notification-persistence-keep">Keep banners until dismissed</label></span>
            <span className="settings-switch"><input id="notification-persistence-keep" type="checkbox" role="switch" checked={persistence.seconds === null} aria-checked={persistence.seconds === null} disabled={locked} aria-describedby={persistenceDescribedBy}
              onChange={event => selectPersistence(event.target.checked ? "indefinite" : String(lastFinitePersistence.current))} /><span aria-hidden /></span>
          </div>
          <p id="notification-persistence-hint" className="settings-note">Off: your browser decides how long banners stay. Some browsers ignore this.</p>
        </>}
        {hasDesktopBanners && persistenceSettings && persistenceSupported && persistence && (persistencePlatform === "linux" || persistencePlatform === "windows") && <>
          <div id="notification-persistence-row" className="settings-control-row" data-persistence-platform={persistencePlatform} data-persistence-mode={persistenceMode}>
            <span className="notification-setting-label"><label htmlFor="notification-persistence">Banner duration</label></span>
            <select id="notification-persistence" value={persistencePlatform === "windows" ? windowsSelection : persistenceSelection} disabled={locked} aria-describedby={persistenceDescribedBy} onChange={event => selectPersistence(event.target.value)}>
              {persistencePlatform === "windows" ? <>
                <option value="5">Short (about 5 seconds)</option>
                <option value="25">Long (about 25 seconds)</option>
              </> : <>
                {[...LINUX_PERSISTENCE_PRESETS, ...(persistence.seconds !== null && !LINUX_PERSISTENCE_PRESETS.some(preset => preset.value === persistence.seconds) ? [{ value: persistence.seconds, label: `${formatPersistence(persistence.seconds)} (current)` }] : [])].sort((left, right) => left.value - right.value).map(preset => <option key={preset.value} value={preset.value}>{preset.label}</option>)}
              </>}
              <option value="indefinite">Until dismissed</option>
            </select>
          </div>
          <p id="notification-persistence-hint" className="settings-note">{persistencePlatform === "windows" ? "Windows may still hide a banner set to stay until dismissed." : "Your desktop may ignore this and use its own timing."}</p>
        </>}
        {hasDesktopBanners && persistenceError && <p id="notification-persistence-error" role="alert" aria-label="Notification settings action failed">{persistenceError}</p>}
      </NotificationSettingsGroup>}
      {hasDesktopBanners && <NotificationSettingsGroup id="notification-test-group" title="Test notifications">
        <div id="notification-test-actions">
        <button id="notification-check" className="secondary-button" disabled={busy} onClick={() => void checkPermission()}>
          Send test notification
        </button>
        {check?.canOpenSettings && <button id="notification-open-settings" className="secondary-button" disabled={busy} onClick={() => void openSettings()}>
          Open notification settings
        </button>}
        </div>
        {(checking || check) && <p id="notification-permission-status" role="status">{checking ? "Sending test notification… If your system asks for permission, choose Allow." : permissionMessage(check!)}</p>}
      </NotificationSettingsGroup>}
      {error && <p id="notification-settings-action-error" role="alert" aria-label="Notification settings action failed">{error}</p>}
      <NotificationSettingsGroup id="notification-apps-group" title="Mirrored apps">
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
      ) : <p id="notification-apps-empty-state">No apps observed yet. Enable mirroring in the Android companion to begin.</p>}
      </NotificationSettingsGroup>
    </section>
  );
}
