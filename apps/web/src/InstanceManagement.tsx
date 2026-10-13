import { useCallback, useEffect, useState } from "react";
import type { EnrollmentView, InstanceManagement as InstanceManagementData } from "./browser-bridge";
import { FamilyVaults, type FamilyVaultsBridge } from "./FamilyVaults";
import { peppyCopy } from "../../desktop/src/generated/peppyCopy";

type ManagementBridge = FamilyVaultsBridge & {
  enrollment_discover(origin?: string): Promise<EnrollmentView>;
  enrollment_sign_in(input?: { username?: string }): Promise<EnrollmentView>;
  enrollment_management(): Promise<InstanceManagementData>;
  enrollment_revoke_device(deviceId: string): Promise<InstanceManagementData>;
};

type ScreenState = "loading" | "sign-in" | "ready" | "hosted" | "error";

function isAccessError(error: unknown): boolean {
  return typeof error === "object" && error !== null && "code" in error
    && ((error as { code?: unknown }).code === "http-401" || (error as { code?: unknown }).code === "http-403");
}

export function formatBytes(bytes: number): string {
  return new Intl.NumberFormat(undefined, { style: "unit", unit: "byte", unitDisplay: "short", notation: "compact", maximumFractionDigits: 1 }).format(bytes);
}

function displayDeviceRole(role: InstanceManagementData["devices"][number]["role"]): string {
  return role === "gateway" ? "Phone" : role;
}

export function InstanceManagement({ bridge, accountUrl }: { bridge: ManagementBridge; accountUrl?: string }) {
  const [screen, setScreen] = useState<ScreenState>("loading");
  const [management, setManagement] = useState<InstanceManagementData>();
  const [pendingRevoke, setPendingRevoke] = useState<InstanceManagementData["devices"][number]>();
  const [error, setError] = useState<string>();
  const [username, setUsername] = useState("");

  const loadManagement = useCallback(async () => {
    try {
      const nextManagement = await bridge.enrollment_management();
      setManagement(nextManagement);
      setError(undefined);
      setScreen("ready");
    } catch (nextError) {
      setScreen(isAccessError(nextError) ? "sign-in" : "error");
      setError(isAccessError(nextError) ? undefined : "Peppy could not load instance management.");
    }
  }, [bridge]);

  useEffect(() => {
    let active = true;
    void (async () => {
      try {
        const enrollment = await bridge.enrollment_discover(location.origin);
        if (!active) return;
        if (enrollment.mode === "hosted") setScreen("hosted");
        else await loadManagement();
      } catch {
        if (active) {
          setScreen("error");
          setError("Peppy could not load instance management.");
        }
      }
    })();
    return () => { active = false; };
  }, [bridge, loadManagement]);

  const signIn = async () => {
    setScreen("loading");
    try {
      const enrollment = await bridge.enrollment_sign_in({ username });
      if (enrollment.mode === "hosted") setScreen("hosted");
      else await loadManagement();
    } catch {
      setError("Peppy could not sign in to this server.");
      setScreen("sign-in");
    }
  };

  const revoke = async () => {
    if (!pendingRevoke) return;
    try {
      setManagement(await bridge.enrollment_revoke_device(pendingRevoke.device_id));
      setPendingRevoke(undefined);
    } catch {
      setError("Peppy could not revoke this device. No device access was changed.");
    }
  };

  if (screen === "hosted") return <main id="instance-management-hosted" data-browser-screen="instance-management-hosted"><section id="instance-management-hosted-card"><h1>Account management</h1><p>Your hosted account is managed through your account portal.</p>{accountUrl && <a id="instance-management-account-link" className="primary-button" href={accountUrl} target="_blank" rel="noopener noreferrer">Open account portal<span className="visually-hidden"> (opens in a new tab)</span></a>}</section></main>;
  if (screen === "loading") return <main id="instance-management-loading" data-browser-screen="instance-management-loading" aria-busy="true"><p role="status">Loading instance management…</p></main>;
  if (screen === "sign-in") return <main id="instance-management-sign-in" data-browser-screen="instance-management-sign-in"><section id="instance-management-sign-in-card"><h1>Sign in to manage this instance</h1><p>Sign in as the instance owner to view devices and instance status.</p>{error && <p id="instance-management-sign-in-error" role="alert">{error}</p>}<form id="instance-management-sign-in-form" onSubmit={(event) => { event.preventDefault(); void signIn(); }}><label htmlFor="instance-management-sign-in-username">{peppyCopy.enrollment_username_label}</label><input id="instance-management-sign-in-username" type="text" value={username} autoComplete="username" aria-describedby="instance-management-sign-in-username-hint" onChange={(event) => setUsername(event.target.value)} /><p id="instance-management-sign-in-username-hint" className="setup-hint">{peppyCopy.enrollment_signin_username_hint}</p><button id="instance-management-sign-in-action" className="primary-button" type="submit">Sign in</button></form></section></main>;
  if (screen === "error" || !management) return <main id="instance-management-error" data-browser-screen="instance-management-error"><section id="instance-management-error-card"><h1>Instance management is unavailable</h1><p role="alert">{error}</p><button id="instance-management-retry" className="secondary-button" type="button" onClick={() => void loadManagement()}>Try again</button></section></main>;

  return <main id="instance-management" data-browser-screen="instance-management"><header id="instance-management-header"><a id="instance-management-back" href="/">Back to Peppy</a><h1>Manage server</h1><p>Instance status and device access</p></header><section id="instance-management-identity" aria-labelledby="instance-management-identity-heading"><h2 id="instance-management-identity-heading">{peppyCopy.instance_management_identity_heading}</h2><dl><div><dt>{peppyCopy.instance_management_username_label}</dt><dd id="instance-management-username">{management.username}</dd></div></dl></section><section id="instance-management-status" aria-labelledby="instance-management-status-heading"><h2 id="instance-management-status-heading">Instance status</h2><dl><div><dt>Vault</dt><dd>{management.vault_id}</dd></div><div><dt>Storage</dt><dd>{formatBytes(management.storage_bytes)} of {formatBytes(management.quota_bytes)}</dd></div><div><dt>Replay retention</dt><dd>{management.replay_retention_days} days</dd></div></dl></section><section id="instance-management-devices" aria-labelledby="instance-management-devices-heading"><h2 id="instance-management-devices-heading">Devices</h2>{error && <p id="instance-management-error-message" role="alert">{error}</p>}<table id="instance-management-devices-table"><thead><tr><th scope="col">Device</th><th scope="col">Role</th><th scope="col">Phone health</th><th scope="col">Access</th><th scope="col"><span className="visually-hidden">Actions</span></th></tr></thead><tbody>{management.devices.map((device) => <tr id={`instance-management-device-${device.device_id}`} key={device.device_id}><th scope="row">{device.name}<span className="instance-management-kind">{device.kind}</span></th><td>{displayDeviceRole(device.role)}</td><td>{device.role === "gateway" ? device.gateway_online ? "Online" : "Offline" : "Not a phone"}</td><td>{device.revoked ? "Revoked" : "Active"}</td><td>{!device.revoked && <button id={`instance-management-revoke-${device.device_id}`} className="secondary-button" type="button" onClick={() => setPendingRevoke(device)}>Revoke</button>}</td></tr>)}</tbody></table></section>{management.isAdmin && <FamilyVaults bridge={bridge} />}{pendingRevoke && <div id="instance-management-revoke-dialog" role="dialog" aria-modal="true" aria-labelledby="instance-management-revoke-heading"><section id="instance-management-revoke-card"><h2 id="instance-management-revoke-heading">Revoke device?</h2><p>{pendingRevoke.name} will lose access to this vault.</p><div><button className="secondary-button" type="button" onClick={() => setPendingRevoke(undefined)}>Cancel</button><button className="primary-button" type="button" onClick={() => void revoke()}>Revoke device</button></div></section></div>}</main>;
}
