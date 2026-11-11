import { useEffect, useRef, useState } from "react";
import { DEFAULT_NOTIFICATION_PERSISTENCE, MAX_NOTIFICATION_PERSISTENCE_SECONDS, notificationPersistence, type NotificationPersistence, type NotificationPersistenceSettings, type NotificationQuickReplySettings } from "@peppy/desktop-ui/bridge-contracts";
import type { AppFilter, MirroredNotification, NotificationCheck, NotificationPreferences } from "../bridge/bridge";
import { notificationAppName, notificationTuple, notificationSourceName } from "./Notifications";

const PERSISTENCE_PRESETS = [
  { value: 5, label: "5 seconds" },
  { value: 30, label: "30 seconds" },
  { value: 60, label: "1 minute" },
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
  const [persistencePending, setPersistencePending] = useState(false);
  const [customMode, setCustomMode] = useState(false);
  const [customDraft, setCustomDraft] = useState("");
  const [customInvalid, setCustomInvalid] = useState(false);
  const [persistenceError, setPersistenceError] = useState("");
  const [persistenceLoadError, setPersistenceLoadError] = useState("");
  const [quickReply, setQuickReply] = useState<NotificationQuickReplySettings>();
  const [quickReplyPending, setQuickReplyPending] = useState(false);
  const [quickReplyError, setQuickReplyError] = useState("");
  const lastFinitePersistence = useRef(DEFAULT_PERSISTENCE_SECONDS);
  const persistenceSavePending = useRef(false);
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
      setPersistencePending(false);
      setPersistenceError("");
      setPersistenceLoadError("");
      setPersistence(undefined);
      setPersistenceSupported(undefined);
      setCustomMode(false);
      setCustomDraft("");
      setCustomInvalid(false);
    }
    if (!loadPersistence || locked || (!adapterChanged && persistenceSupported !== undefined)) return () => { current = false; };
    setPersistenceLoadError("");
    void loadPersistence().then(value => {
      if (!current) return;
      setPersistenceSupported(value.supported);
      if (!value.supported) return;
      const loaded = notificationPersistence({ seconds: value.seconds }) ?? DEFAULT_NOTIFICATION_PERSISTENCE;
      setPersistence(loaded);
      if (loaded.seconds !== null && !PERSISTENCE_PRESETS.some(preset => preset.value === loaded.seconds)) setCustomMode(true);
      if (loaded.seconds !== null) {
        lastFinitePersistence.current = loaded.seconds;
        setCustomDraft(String(loaded.seconds));
      }
    }).catch(() => {
      if (current) setPersistenceLoadError("Could not load banner duration settings. Reopen Settings to try again.");
    });
    return () => { current = false; };
  }, [loadPersistence, locked, persistenceSupported]);
  // Observed app defaults never override authoritative synchronized mute choices.
  const apps = [...new Map([
    ...notifications.map(n => ({ sourceDeviceId: n.target.sourceDeviceId, packageName: n.packageName, appName: n.appName, muted: false })),
    ...filters,
  ].map(filter => [notificationTuple(filter.sourceDeviceId, filter.packageName), filter])).values()];
  const sourceIds = apps.map(app => app.sourceDeviceId);
  const hasDesktopBanners = preferences.messageBanners || preferences.mirroredBanners;
  const muteWithHeads = preferences.muteWithHeads ?? true;
  const previewMode = preferences.preview === "full" ? "full" : preferences.showSource ? "source" : "hidden";
  const lockedNoteId = locked ? "notification-banner-locked-note" : undefined;
  const headsDescribedBy = locked ? "notification-mute-with-heads-hint notification-banner-locked-note" : "notification-mute-with-heads-hint";
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
  const savePersistence = async (value: NotificationPersistence, onSuccess: () => void, onFailure: () => void) => {
    if (locked || persistenceSavePending.current || !persistenceSettings) return;
    const savedSettings = persistenceSettings;
    const isCurrentSave = () => persistenceMounted.current && persistenceSettingsRef.current === savedSettings;
    const saveToken = ++persistenceSaveToken.current;
    persistenceSavePending.current = true;
    setPersistencePending(true);
    setPersistenceError("");
    try {
      await savedSettings.save(value);
      if (!isCurrentSave()) return;
      setPersistence(value);
      if (value.seconds !== null) lastFinitePersistence.current = value.seconds;
      onSuccess();
    } catch {
      if (!isCurrentSave()) return;
      setPersistenceError("Could not save notification settings. Try again.");
      onFailure();
    } finally {
      if (persistenceSaveToken.current === saveToken) {
        persistenceSavePending.current = false;
        if (persistenceMounted.current) setPersistencePending(false);
      }
    }
  };
  const persistenceSelection = customMode ? "custom" : persistence?.seconds === null ? "indefinite" : String(persistence?.seconds);
  const commitCustomPersistence = () => {
    if (locked || persistenceSavePending.current || !persistence || persistence.seconds === null) return;
    const value = /^\d+$/.test(customDraft) ? notificationPersistence({ seconds: Number(customDraft) }) : undefined;
    if (!value || value.seconds === null) {
      setCustomInvalid(true);
      return;
    }
    setCustomInvalid(false);
    setPersistenceError("");
    if (value.seconds === persistence.seconds) return;
    void savePersistence(value, () => undefined, () => {
      setCustomDraft(String(persistence.seconds));
      setCustomInvalid(false);
    });
  };
  const selectPersistence = (selection: string) => {
    if (locked || persistenceSavePending.current || !persistence) return;
    if (selection === "custom") {
      setCustomInvalid(false);
      setPersistenceError("");
      setCustomMode(true);
      const seconds = persistence.seconds === null ? lastFinitePersistence.current : persistence.seconds;
      setCustomDraft(String(seconds));
      if (persistence.seconds === null) void savePersistence({ seconds }, () => undefined, () => {
        setCustomMode(false);
        setCustomDraft("");
      });
      return;
    }
    const value = selection === "indefinite" ? { seconds: null } : { seconds: Number(selection) };
    void savePersistence(value, () => {
      setCustomMode(false);
      setCustomInvalid(false);
    }, () => {
      if (persistence.seconds !== null) setCustomDraft(String(persistence.seconds));
      setCustomInvalid(false);
    });
  };
  useEffect(() => {
    if (!hasDesktopBanners) setPersistenceError("");
  }, [hasDesktopBanners]);
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
        <div id="notification-preview-row" className="settings-control-row">
          <label htmlFor="notification-preview">Banner preview</label>
          <select id="notification-preview" value={previewMode} disabled={busy || locked} aria-describedby={[lockedNoteId, quickReply?.enabled ? "notification-quick-reply-preview-hint" : undefined].filter(Boolean).join(" ") || undefined}
            onChange={event => {
              const selection = event.target.value;
              savePreferences({ ...preferences, preview: selection === "full" ? "full" : "hidden", showSource: selection === "source" });
            }}>
            <option value="full">Full content</option>
            <option value="source">Sender or app only</option>
            <option value="hidden">Hidden — generic banner only</option>
          </select>
        </div>
        {quickReplySettings && quickReply?.supported !== false && <>
          <div id="notification-quick-reply-row" className="settings-control-row" aria-busy={quickReplyPending || (quickReply === undefined && !quickReplyError) || undefined}>
            <label htmlFor="notification-quick-reply-toggle">Quick reply in notifications</label>
            <span className="settings-switch">
              <input id="notification-quick-reply-toggle" type="checkbox" role="switch" checked={quickReply?.enabled ?? false} aria-checked={quickReply?.enabled ?? false}
                disabled={busy || locked || quickReplyPending || quickReply === undefined} aria-describedby={`notification-quick-reply-hint${lockedNoteId ? ` ${lockedNoteId}` : ""}`}
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
          <p id="notification-quick-reply-hint" className="settings-note">Reply to messages from notifications on this computer. Not available while Peppy is locked. This setting applies only to this computer.</p>
          {quickReplyPending && <p role="status">Saving quick reply setting…</p>}
          {quickReply === undefined && !quickReplyError && <p role="status">Loading quick reply setting…</p>}
          {quickReply?.enabled && <p id="notification-quick-reply-preview-hint" className="settings-note">
            {previewMode === "full" ? "Show the sender and message, with quick reply." : previewMode === "source" ? "Show who it is from. Choose Show message to reveal a message and reply." : "Show a generic banner. Choose Show message to reveal a message and reply."} Quick reply applies to message banners, not mirrored app notifications.
          </p>}
          {quickReplyError && <p id="notification-quick-reply-error" className="settings-error" role="alert">{quickReplyError}</p>}
        </>}
        {persistenceSettings && persistenceLoadError && !persistenceSupported && <p id="notification-persistence-load-error" className="settings-note" role="alert">{persistenceLoadError}</p>}
        {persistenceSettings && persistenceSupported && persistence && <div id="notification-persistence-row" className="settings-control-row" data-persistence-mode={persistenceSelection}>
          <label htmlFor="notification-persistence">Banner duration</label>
          <span id="notification-persistence-controls">
            <select id="notification-persistence" value={persistenceSelection} disabled={locked}
              onChange={event => selectPersistence(event.target.value)}>
              {PERSISTENCE_PRESETS.map(preset => <option key={preset.value} value={preset.value}>{preset.label}</option>)}
              <option value="custom">Custom</option>
              <option value="indefinite">Indefinitely</option>
            </select>
            {customMode && <span id="notification-persistence-custom-field">
              <input id="notification-persistence-custom" type="number" min={1} max={MAX_NOTIFICATION_PERSISTENCE_SECONDS} step={1} inputMode="numeric" aria-label="Custom banner duration in seconds"
                value={customDraft} readOnly={persistencePending || locked} aria-invalid={customInvalid || undefined}
                onChange={event => {
                  const draft = event.target.value;
                  setCustomDraft(draft);
                  const valid = /^\d+$/.test(draft) && notificationPersistence({ seconds: Number(draft) });
                  if (valid) { setCustomInvalid(false); setPersistenceError(""); }
                }}
                onBlur={commitCustomPersistence}
                onKeyDown={event => {
                  if (event.key === "Enter") { event.preventDefault(); commitCustomPersistence(); }
                  if (event.key === "Escape" && persistence?.seconds !== null) {
                    setCustomDraft(String(persistence.seconds));
                    setCustomInvalid(false);
                    setPersistenceError("");
                  }
                }} />
              <span aria-hidden>seconds</span>
            </span>}
          </span>
        </div>}
        <button id="notification-check" className="secondary-button" disabled={busy} onClick={() => void checkPermission()}>
          Send test notification
        </button>
        {(checking || check) && <p id="notification-permission-status" role="status">{checking ? "Sending test notification… If your system asks for permission, choose Allow." : permissionMessage(check!)}</p>}
        {check?.canOpenSettings && <button id="notification-open-settings" className="secondary-button" disabled={busy} onClick={() => void openSettings()}>
          Open notification settings
        </button>}
      </>}
      {hasDesktopBanners && customInvalid && <p id="notification-persistence-validation-error" role="alert" aria-label="Invalid custom banner duration">Enter whole seconds from 1 to 3600.</p>}
      {(persistenceError || error) && <p id="notification-settings-action-error" role="alert" aria-label="Notification settings action failed">{persistenceError || error}</p>}
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
