import { useCallback, useEffect, useRef, useState } from "react";
import { Modal } from "@peppy/desktop-ui";
import { bridge, type DeviceRosterView, type ManagedDeviceView } from "./bridge";
import { errorCode } from "./bridgeErrors";
import { peppyCopy } from "./generated/peppyCopy";
import "./device-settings.css";

const deviceKind = (kind: string | null) => ({ android: "Android", ios: "iPhone or iPad", desktop: "Desktop", browser: "Browser" }[kind ?? ""] ?? "Unknown platform");
const deviceRole = (role: string) => ({ owner: "Owner", gateway: "Phone gateway", device: "Device" }[role] ?? "Unknown role");
const deviceName = (device: ManagedDeviceView) => device.name.trim() || "Unnamed device";
const presenceLabel = (device: ManagedDeviceView) => {
  if (!device.presence) return "Status unknown";
  if (device.presence.state === "connected") return "Connected";
  if (device.presence.state === "never") return "Never connected";
  const seconds = device.presence.last_seen_age_seconds;
  if (seconds === null) return "Status unknown";
  return `Last seen ${seconds < 60 ? "under a minute" : seconds < 3_600 ? `${Math.floor(seconds / 60)}m` : `${Math.floor(seconds / 3_600)}h`} ago`;
};
/** While the account is read-only, a stale phone is expected: its writes are refused, so the age is neutral. */
const presenceState = (device: ManagedDeviceView, accountPaused: boolean) => {
  if (!device.presence) return "unknown";
  if (device.presence.state === "never") return "never";
  if (device.presence.state === "connected") return "connected";
  return accountPaused ? "paused" : "delayed";
};
const OFFLINE_REMOVE_ERROR = "The device may not have been removed because the server is offline. The list refreshes when Peppy reconnects, or you can retry now.";
const loadError = (error: unknown) => typeof error === "object" && error !== null && "message" in error && typeof error.message === "string" && error.message ? error.message : "The device roster could not be loaded.";
const revokeError = (error: unknown) => {
  const code = typeof error === "object" && error !== null && "code" in error ? String(error.code) : "";
  if (code === "owner_required") return "Only an owner can remove another device.";
  if (code === "last_active_owner") return "At least one active owner must remain.";
  if (code === "device_already_revoked") return "This device was already removed.";
  if (code === "device_not_found") return "This device is no longer enrolled.";
  if (code === "invalid_bearer") return "This device no longer has permission to manage the roster.";
  if (code === "app-locked" || code === "app_lock") return "Unlock this app before managing devices.";
  return "The removal outcome is unknown. The roster was reloaded to check it.";
};

export function DevicesSettings({ serverOffline = false, accountReadOnly = false }: { serverOffline?: boolean; accountReadOnly?: boolean }) {
  const [roster, setRoster] = useState<DeviceRosterView | null>(null);
  const [loading, setLoading] = useState(true);
  const [target, setTarget] = useState<ManagedDeviceView | null>(null);
  const [pending, setPending] = useState(false);
  const pendingRef = useRef(false);
  const [error, setError] = useState("");
  const [offlineFromFailure, setOfflineFromFailure] = useState(false);
  const [loadFailed, setLoadFailed] = useState(false);
  const mounted = useRef(true);
  const loadSequence = useRef(0);
  const invoker = useRef<HTMLButtonElement | null>(null);
  const titleHeading = useRef<HTMLHeadingElement>(null);
  const previousServerOffline = useRef(serverOffline);
  const offline = serverOffline || offlineFromFailure;
  /* Server-offline takes precedence: the offline wording stays exactly as before. */
  const accountPaused = accountReadOnly && !serverOffline;

  const load = useCallback(async () => {
    const sequence = ++loadSequence.current;
    setLoading(true);
    try {
      const next = await bridge.list_devices();
      if (mounted.current && sequence === loadSequence.current) {
        setRoster(next);
        setError("");
        setOfflineFromFailure(false);
        setLoadFailed(false);
      }
      return true;
    } catch (error) {
      if (mounted.current && sequence === loadSequence.current) {
        setLoadFailed(true);
        if (errorCode(error) === "offline") {
          setOfflineFromFailure(true);
          setError("");
        } else setError(loadError(error));
      }
      return false;
    } finally {
      if (mounted.current && sequence === loadSequence.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    void load();
    return () => { mounted.current = false; };
  }, [load]);
  useEffect(() => {
    if (previousServerOffline.current && !serverOffline) void load();
    previousServerOffline.current = serverOffline;
  }, [load, serverOffline]);
  useEffect(() => {
    if (offline && target && !pending) setTarget(null);
  }, [offline, pending, target]);

  const current = roster?.devices.find(device => device.id === roster.currentDeviceId);
  const activeOwners = roster?.devices.filter(device => !device.revoked && device.role === "owner") ?? [];
  const canRemove = (device: ManagedDeviceView) => current?.role === "owner" && !current.revoked && device.id !== roster?.currentDeviceId && !device.revoked && (device.role !== "owner" || activeOwners.length > 1);
  const restriction = (device: ManagedDeviceView) => {
    if (current?.revoked) return "This device no longer has access to manage the roster.";
    if (!current) return "This device is not listed in the roster.";
    if (device.id === roster?.currentDeviceId) return "This device cannot remove itself here.";
    if (current?.role !== "owner") return "Only an owner can remove another device.";
    if (device.role === "owner" && activeOwners.length <= 1) return "At least one active owner must remain.";
    return "";
  };
  const close = () => { if (!pendingRef.current) setTarget(null); };
  const remove = async () => {
    if (!target || pendingRef.current || offline) return;
    pendingRef.current = true;
    setPending(true);
    const selected = target;
    try {
      await bridge.revoke_device(selected.id);
      if (!await load()) setError("Removal was accepted, but the latest roster could not be loaded. Reload before managing devices again.");
    } catch (error) {
      if (errorCode(error) === "offline") {
        if (mounted.current) {
          setOfflineFromFailure(true);
          setError(OFFLINE_REMOVE_ERROR);
        }
        return;
      }
      const message = revokeError(error);
      const reloaded = await load();
      if (mounted.current) setError(reloaded ? message : `${message} The latest roster could not be loaded.`);
    } finally {
      pendingRef.current = false;
      if (mounted.current) { setPending(false); setTarget(null); }
    }
  };
  const rows = (revoked: boolean) => roster?.devices.filter(device => device.revoked === revoked) ?? [];
  const row = (device: ManagedDeviceView) => <li key={device.id} data-device-id={device.id}>
    <div data-device-details={device.id}>
      <strong>{deviceName(device)}</strong>
      <span data-device-metadata={device.id}>
        <span data-device-badge="platform">{deviceKind(device.kind)}</span>
        <span data-device-badge="role">{deviceRole(device.role)}</span>
        <span data-device-badge="enrollment">{device.revoked ? "Removed" : "Active"}</span>
        {device.id === roster?.currentDeviceId && <span data-device-badge="current">This device</span>}
        {!device.revoked && <span data-device-badge="presence" data-presence-state={presenceState(device, accountPaused)}>{presenceLabel(device)}</span>}
        {accountPaused && device.role === "gateway" && !device.revoked && <span data-device-badge="account-paused">{peppyCopy.devices_sending_paused_badge}</span>}
      </span>
      {device.kind === "ios" && !device.revoked && <small data-ios-sms-note>Can't send SMS through Peppy on iPhone</small>}
    </div>
    {canRemove(device) ? offline ? <small data-remove-offline>Unavailable while offline</small> : <button className="secondary-button" type="button" aria-label={`Remove ${deviceName(device)}`} disabled={pending} onClick={event => { invoker.current = event.currentTarget; setTarget(device); }}>Remove</button> : !device.revoked && <small>{restriction(device)}</small>}
  </li>;

  return <section id="settings-devices" className="settings-card" data-settings-section="devices" aria-labelledby="settings-devices-title" aria-busy={loading}>
    <h2 id="settings-devices-title" ref={titleHeading} tabIndex={-1}>Devices</h2>
    <p>Devices can access this encrypted vault. Removing a device revokes server access; it does not erase local data or secrets.</p>
    {offline && <p id="devices-offline-notice" data-devices-offline-notice role="status" className="settings-note">{roster ? "Server offline. Showing the last loaded list; removing devices is unavailable." : "Server offline. The device list will load when the server is back."}</p>}
    {error && <p role="alert" className="settings-error" data-devices-remove-error={error === OFFLINE_REMOVE_ERROR || undefined}>{error}</p>}
    {accountPaused && roster && <p id="devices-read-only-caption" data-devices-read-only-caption className="settings-note">{peppyCopy.devices_read_only_caption}</p>}
    {loading && <p role="status">Loading devices…</p>}
    {!loading && (!roster || offline || loadFailed) && <button id="devices-retry" className="secondary-button" type="button" onClick={() => void load()}>Retry loading devices</button>}
    {roster && <div aria-hidden={target ? "true" : undefined} inert={target ? true : undefined}>
      {!rows(false).length && !rows(true).length && <p>No devices enrolled.</p>}
      <ul id="devices-active-list" aria-label="Active devices">{rows(false).map(row)}</ul>
      {rows(true).length > 0 && <details id="devices-removed" className="settings-disclosure"><summary>Previously removed</summary><ul aria-label="Previously removed devices">{rows(true).map(row)}</ul></details>}
    </div>}
    {target && <Modal id="device-revoke-dialog" title={`Remove ${deviceName(target)}?`} busy={pending} onDismiss={close} opener={invoker} focusFallback={titleHeading} actions={<><button type="button" className="secondary-button" disabled={pending} onClick={close}>Cancel</button><button type="button" className="primary-button" disabled={pending} aria-busy={pending || undefined} onClick={() => void remove()}>Remove device</button></>}><p>Server access is revoked. Local data and secrets are not erased. Reconnecting requires enrollment.</p></Modal>}
  </section>;
}
