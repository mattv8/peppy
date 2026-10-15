import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { bridge, type DeviceRosterView, type ManagedDeviceView } from "./bridge";
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
const presenceState = (device: ManagedDeviceView) => !device.presence ? "unknown" : device.presence.state === "never" ? "never" : device.presence.state === "connected" ? "connected" : "delayed";
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

export function DevicesSettings() {
  const [roster, setRoster] = useState<DeviceRosterView | null>(null);
  const [loading, setLoading] = useState(true);
  const [target, setTarget] = useState<ManagedDeviceView | null>(null);
  const [pending, setPending] = useState(false);
  const pendingRef = useRef(false);
  const [error, setError] = useState("");
  const mounted = useRef(true);
  const loadSequence = useRef(0);
  const invoker = useRef<HTMLButtonElement | null>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const dialogHeading = useRef<HTMLHeadingElement>(null);
  const titleHeading = useRef<HTMLHeadingElement>(null);

  const load = useCallback(async () => {
    const sequence = ++loadSequence.current;
    setLoading(true);
    try {
      const next = await bridge.list_devices();
      if (mounted.current && sequence === loadSequence.current) {
        setRoster(next);
        setError("");
      }
      return true;
    } catch (error) {
      if (mounted.current && sequence === loadSequence.current) {
        setRoster(null);
        setError(error instanceof Error ? error.message : "The device roster could not be loaded.");
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
    if (pending && !dialog.current?.contains(document.activeElement)) dialogHeading.current?.focus();
  }, [pending]);
  useEffect(() => {
    if (!target) return;
    const element = dialog.current;
    const supportsModal = typeof element?.showModal === "function";
    if (supportsModal && !element.open) element.showModal();
    if (!supportsModal && element) element.open = true;
    const root = document.getElementById("root");
    if (!supportsModal && root) root.inert = true;
    dialogHeading.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !pendingRef.current) setTarget(null);
      if (event.key !== "Tab" || !element) return;
      const focusable = [dialogHeading.current, ...Array.from(element.querySelectorAll<HTMLElement>("button:not([disabled])"))].filter((item): item is HTMLElement => item !== null);
      const first = focusable[0] ?? dialogHeading.current;
      const last = focusable.at(-1) ?? dialogHeading.current;
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      if (!supportsModal && root) root.inert = false;
      if (supportsModal && element?.open) element.close();
      if (!supportsModal && element) element.open = false;
    };
  }, [target]);
  useEffect(() => {
    if (target || !invoker.current) return;
    const trigger = invoker.current;
    invoker.current = null;
    if (trigger.isConnected) trigger.focus();
    else titleHeading.current?.focus();
  }, [target]);

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
    if (!target || pendingRef.current) return;
    pendingRef.current = true;
    setPending(true);
    const selected = target;
    try {
      await bridge.revoke_device(selected.id);
      if (!await load()) setError("Removal was accepted, but the latest roster could not be loaded. Reload before managing devices again.");
    } catch (error) {
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
    <div><strong>{deviceName(device)}</strong><span data-presence-state={device.revoked ? undefined : presenceState(device)}>{deviceKind(device.kind)} · {deviceRole(device.role)} · {device.revoked ? "Removed" : "Active"}{device.id === roster?.currentDeviceId ? " · This device" : ""}{!device.revoked && ` · ${presenceLabel(device)}`}</span>{device.kind === "ios" && !device.revoked && <small data-ios-sms-note>Can't send SMS through Peppy on iPhone</small>}</div>
    {canRemove(device) ? <button className="secondary-button" type="button" aria-label={`Remove ${deviceName(device)}`} disabled={pending} onClick={event => { invoker.current = event.currentTarget; setTarget(device); }}>Remove</button> : !device.revoked && <small>{restriction(device)}</small>}
  </li>;

  return <section id="settings-devices" className="settings-card" data-settings-section="devices" aria-labelledby="settings-devices-title" aria-busy={loading}>
    <h2 id="settings-devices-title" ref={titleHeading} tabIndex={-1}>Devices</h2>
    <p>Devices can access this encrypted vault. Removing a device revokes server access; it does not erase local data or secrets.</p>
    {error && <p role="alert" className="settings-error">{error}</p>}
    {loading && <p role="status">Loading devices…</p>}
    {!loading && !roster && <button className="secondary-button" type="button" onClick={() => void load()}>Retry loading devices</button>}
    {roster && <div aria-hidden={target ? "true" : undefined} inert={target ? true : undefined}>
      {!rows(false).length && !rows(true).length && <p>No devices enrolled.</p>}
      <ul id="devices-active-list" aria-label="Active devices">{rows(false).map(row)}</ul>
      {rows(true).length > 0 && <details id="devices-removed" className="settings-disclosure"><summary>Previously removed</summary><ul aria-label="Previously removed devices">{rows(true).map(row)}</ul></details>}
    </div>}
    {target && createPortal(<dialog id="device-revoke-dialog" ref={dialog} aria-labelledby="device-revoke-title" onCancel={event => { event.preventDefault(); close(); }}>
      <h3 id="device-revoke-title" tabIndex={-1} ref={dialogHeading}>Remove {deviceName(target)}?</h3>
      <p>Server access is revoked. Local data and secrets are not erased. Reconnecting requires enrollment.</p>
      <button type="button" className="secondary-button" disabled={pending} onClick={close}>Cancel</button>
      <button type="button" className="primary-button" disabled={pending} aria-busy={pending || undefined} onClick={() => void remove()}>Remove device</button>
    </dialog>, document.body)}
  </section>;
}
