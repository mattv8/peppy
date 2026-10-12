import { useCallback, useEffect, useRef, useState } from "react";
// qrcode does not ship TypeScript declarations.
// @ts-expect-error qrcode's runtime API is declared locally for this use.
import QRCode from "qrcode";
import { peppyCopy } from "../../desktop/src/generated/peppyCopy";
import { formatBytes } from "./InstanceManagement";
import { parseVaultRequestSnapshot, type VaultInviteView, type VaultRequestView, type VaultSummary } from "./browser-bridge";

export type FamilyVaultsBridge = {
  enrollment_list_vaults(): Promise<VaultSummary[]>;
  enrollment_create_invite(): Promise<{ inviteId: string; inviteLink: string; expiresInSeconds: number }>;
  enrollment_list_invites(): Promise<VaultInviteView[]>;
  enrollment_revoke_invite(inviteId: string): Promise<void>;
  enrollment_list_vault_requests(): Promise<VaultRequestView[]>;
  enrollment_approve_vault_request(requestId: string): Promise<void>;
  enrollment_deny_vault_request(requestId: string): Promise<void>;
};

const INITIAL_RECONNECT_DELAY_MS = 2_000;
const MAX_RECONNECT_DELAY_MS = 60_000;
const qrOptions = { errorCorrectionLevel: "M" as const, margin: 4, width: 512 };
const unknownDate = "—";

function duration(seconds: number): string {
  return seconds >= 3_600 ? `${Math.ceil(seconds / 3_600)}h` : `${Math.ceil(seconds / 60)}m`;
}

function parseDate(value: string): Date | undefined {
  const date = new Date(value);
  return Number.isFinite(date.getTime()) ? date : undefined;
}

function age(createdAt: string): string {
  const date = parseDate(createdAt);
  if (!date) return unknownDate;
  return new Intl.RelativeTimeFormat(undefined, { numeric: "auto" }).format(Math.round((date.getTime() - Date.now()) / 60_000), "minute");
}

function formattedDate(value: string): string {
  const date = parseDate(value);
  return date ? new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date) : unknownDate;
}

function inviteState(state: VaultInviteView["state"]): string {
  return peppyCopy[`family_vaults_invite_state_${state}`];
}

function requestState(state: VaultRequestView["state"]): string {
  return peppyCopy[`family_vaults_request_state_${state}`];
}

export function FamilyVaults({ bridge }: { bridge: FamilyVaultsBridge }) {
  const [invites, setInvites] = useState<VaultInviteView[]>([]);
  const [requests, setRequests] = useState<VaultRequestView[]>([]);
  const [vaults, setVaults] = useState<VaultSummary[]>([]);
  const [invite, setInvite] = useState<{ inviteLink: string; expiresInSeconds: number }>();
  const [qr, setQr] = useState("");
  const [copied, setCopied] = useState(false);
  const [creatingInvite, setCreatingInvite] = useState(false);
  const [paused, setPaused] = useState(false);
  const [error, setError] = useState("");
  const [confirmRevoke, setConfirmRevoke] = useState<string>();
  const [confirmDeny, setConfirmDeny] = useState<string>();
  const source = useRef<EventSource | undefined>(undefined);
  const reconnectTimer = useRef<number | undefined>(undefined);
  const delay = useRef(INITIAL_RECONNECT_DELAY_MS);
  const active = useRef(true);
  const inviteCreationInFlight = useRef(false);

  const refresh = useCallback(async () => {
    const [nextInvites, nextRequests, nextVaults] = await Promise.all([
      bridge.enrollment_list_invites(),
      bridge.enrollment_list_vault_requests(),
      bridge.enrollment_list_vaults(),
    ]);
    if (!active.current) return;
    setInvites(nextInvites);
    setRequests(nextRequests);
    setVaults(nextVaults);
  }, [bridge]);

  const closeSource = useCallback(() => {
    source.current?.close();
    source.current = undefined;
  }, []);

  const connect = useCallback(() => {
    if (!active.current || source.current) return;
    if (typeof EventSource === "undefined") {
      setPaused(true);
      return;
    }
    const nextSource = new EventSource("/v1/instance/events");
    source.current = nextSource;
    nextSource.addEventListener("snapshot", (event) => {
      if (active.current) setRequests(parseVaultRequestSnapshot((event as MessageEvent<string>).data));
    });
    nextSource.onopen = () => {
      if (!active.current) return;
      delay.current = INITIAL_RECONNECT_DELAY_MS;
      setPaused(false);
      setError("");
    };
    nextSource.onerror = () => {
      closeSource();
      void bridge.enrollment_list_vault_requests().then((nextRequests) => {
        if (!active.current) return;
        setRequests(nextRequests);
        setPaused(true);
        const currentDelay = delay.current;
        delay.current = Math.min(delay.current * 2, MAX_RECONNECT_DELAY_MS);
        reconnectTimer.current = window.setTimeout(connect, currentDelay);
      }).catch(() => {
        if (active.current) setError("Peppy could not refresh live vault requests.");
      });
    };
  }, [bridge, closeSource]);

  useEffect(() => {
    active.current = true;
    void refresh().catch(() => setError("Peppy could not load family vaults."));
    connect();
    return () => {
      active.current = false;
      closeSource();
      window.clearTimeout(reconnectTimer.current);
    };
  }, [closeSource, connect, refresh]);

  useEffect(() => {
    if (!invite) {
      setQr("");
      return;
    }
    let alive = true;
    void QRCode.toDataURL(invite.inviteLink, qrOptions).then((url: string) => {
      if (alive) setQr(url);
    }).catch(() => {
      if (alive) setQr("");
    });
    return () => { alive = false; };
  }, [invite]);

  const createInvite = async () => {
    if (inviteCreationInFlight.current) return;
    inviteCreationInFlight.current = true;
    setCreatingInvite(true);
    try {
      const next = await bridge.enrollment_create_invite();
      setInvite(next);
      await refresh();
    } catch {
      setError("Peppy could not create an invite.");
    } finally {
      inviteCreationInFlight.current = false;
      setCreatingInvite(false);
    }
  };

  const copyInvite = async () => {
    if (!invite) return;
    try {
      await navigator.clipboard.writeText(invite.inviteLink);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2_000);
    } catch {
      setError("Peppy could not copy the invite link.");
    }
  };

  const revokeInvite = async (inviteId: string) => {
    try {
      await bridge.enrollment_revoke_invite(inviteId);
      setConfirmRevoke(undefined);
      await refresh();
    } catch {
      setError("Peppy could not revoke this invite.");
    }
  };

  const actOnRequest = async (requestId: string, action: "approve" | "deny") => {
    try {
      if (action === "approve") await bridge.enrollment_approve_vault_request(requestId);
      else await bridge.enrollment_deny_vault_request(requestId);
      setConfirmDeny(undefined);
      await refresh();
    } catch {
      setError(peppyCopy.family_vaults_error_request_not_pending);
    }
  };

  const manualRefresh = () => {
    window.clearTimeout(reconnectTimer.current);
    closeSource();
    delay.current = INITIAL_RECONNECT_DELAY_MS;
    setPaused(false);
    void refresh().catch(() => setError("Peppy could not refresh family vaults.")).finally(connect);
  };

  return <section id="family-vaults" aria-labelledby="family-vaults-heading">
    <h2 id="family-vaults-heading">Family vaults</h2>
    {error && <p id="family-vaults-error" role="alert">{error}</p>}
    <section id="family-vaults-invite" className="family-vaults-card">
      <h3>{invite ? "Share this invite link" : "Invite a family member"}</h3>
      {invite ? <>
        <p>{peppyCopy.family_vaults_invite_share_body}</p>
        <p className="setup-hint">{peppyCopy.family_vaults_invite_expires_in} {duration(invite.expiresInSeconds)}.</p>
        <div id="family-vaults-invite-link-display"><code id="family-vaults-invite-link-text">{invite.inviteLink}</code><button id="family-vaults-copy-invite-button" className="secondary-button" type="button" aria-label="Copy invite link" onClick={() => void copyInvite()}>{peppyCopy.family_vaults_copy_invite}</button>{copied && <span id="family-vaults-copy-confirm" aria-live="polite">{peppyCopy.family_vaults_invite_copied}</span>}</div>
        <div id="family-vaults-invite-qr" role="img" aria-label={peppyCopy.family_vaults_invite_qr_accessibility}>{qr && <img id="family-vaults-invite-qr-image" src={qr} alt="" aria-hidden="true" />}</div>
        <button id="family-vaults-done-invite-button" className="secondary-button" type="button" onClick={() => setInvite(undefined)}>{peppyCopy.family_vaults_invite_done}</button>
      </> : <>
        <p>{peppyCopy.family_vaults_invite_body}</p>
        <button id="family-vaults-create-invite-button" className="primary-button" type="button" disabled={creatingInvite} onClick={() => void createInvite()}>Create invite link</button>
      </>}
    </section>
    <section id="family-vaults-active-invites" className="family-vaults-card" aria-labelledby="family-vaults-active-invites-heading">
      <h3 id="family-vaults-active-invites-heading">Active invites</h3>
      {invites.length === 0 ? <p id="family-vaults-no-active-invites">{peppyCopy.family_vaults_no_active_invites}</p> : <ul id="family-vaults-invite-list" aria-label="Active invites">{invites.map((item) => <li key={item.inviteId} data-invite-id={item.inviteId}><span className="family-vaults-invite-meta">{peppyCopy.family_vaults_invite_expires_in} {duration(item.expiresInSeconds)} · {inviteState(item.state)}</span><button id={`family-vaults-revoke-invite-${item.inviteId}`} className="secondary-button" type="button" onClick={() => setConfirmRevoke(item.inviteId)}>{peppyCopy.family_vaults_revoke_invite}</button>{confirmRevoke === item.inviteId && <div id={`family-vaults-revoke-confirm-${item.inviteId}`} role="dialog" aria-modal="false" aria-labelledby={`family-vaults-revoke-confirm-label-${item.inviteId}`}><span id={`family-vaults-revoke-confirm-label-${item.inviteId}`}>{peppyCopy.family_vaults_revoke_confirm_label}</span><button id={`family-vaults-revoke-confirm-yes-${item.inviteId}`} className="primary-button" type="button" onClick={() => void revokeInvite(item.inviteId)}>{peppyCopy.family_vaults_revoke_confirm_yes}</button><button id={`family-vaults-revoke-confirm-no-${item.inviteId}`} className="secondary-button" type="button" onClick={() => setConfirmRevoke(undefined)}>{peppyCopy.family_vaults_revoke_confirm_no}</button></div>}</li>)}</ul>}
    </section>
    <section id="family-vaults-requests" className="family-vaults-card" aria-labelledby="family-vaults-requests-heading">
      <h3 id="family-vaults-requests-heading">Vault requests</h3>
      {paused && <div id="family-vaults-live-paused" role="status" aria-live="polite"><p>{peppyCopy.family_vaults_live_paused_body}</p><button id="family-vaults-live-refresh" className="secondary-button" type="button" onClick={manualRefresh}>{peppyCopy.family_vaults_live_refresh}</button></div>}
      <div id="family-vaults-requests-live" aria-live="polite" aria-label="Live vault requests">{requests.length === 0 ? <p id="family-vaults-no-requests">{peppyCopy.family_vaults_no_requests}</p> : requests.map((request) => <article id={`family-vaults-request-${request.requestId}`} key={request.requestId} data-request-id={request.requestId} data-request-state={request.state}><h4>{request.displayName}</h4><dl className="family-vaults-request-meta"><div><dt>{peppyCopy.family_vaults_request_client}</dt><dd>{request.client}</dd></div><div><dt>{peppyCopy.family_vaults_request_age}</dt><dd>{age(request.createdAt)}</dd></div><div><dt>{peppyCopy.family_vaults_request_state}</dt><dd>{requestState(request.state)}</dd></div><div><dt>{peppyCopy.family_vaults_request_expires}</dt><dd>{peppyCopy.family_vaults_invite_expires_in} {duration(request.expiresInSeconds)}</dd></div></dl>{request.state === "pending" && <div className="enrollment-actions"><button id={`family-vaults-approve-${request.requestId}`} className="primary-button" type="button" onClick={() => void actOnRequest(request.requestId, "approve")}>{peppyCopy.family_vaults_approve_request}</button><button id={`family-vaults-deny-${request.requestId}`} className="secondary-button" type="button" onClick={() => setConfirmDeny(request.requestId)}>{peppyCopy.family_vaults_deny_request}</button>{confirmDeny === request.requestId && <div id={`family-vaults-deny-confirm-${request.requestId}`} role="dialog" aria-modal="false" aria-labelledby={`family-vaults-deny-confirm-label-${request.requestId}`}><span id={`family-vaults-deny-confirm-label-${request.requestId}`}>{peppyCopy.family_vaults_deny_confirm_label}</span><p className="setup-hint">{peppyCopy.family_vaults_deny_confirm_body}</p><button id={`family-vaults-deny-confirm-yes-${request.requestId}`} className="primary-button" type="button" onClick={() => void actOnRequest(request.requestId, "deny")}>{peppyCopy.family_vaults_deny_confirm_yes}</button><button id={`family-vaults-deny-confirm-no-${request.requestId}`} className="secondary-button" type="button" onClick={() => setConfirmDeny(undefined)}>{peppyCopy.family_vaults_deny_confirm_no}</button></div>}</div>}</article>)}</div>
    </section>
    <section id="family-vaults-list" className="family-vaults-card" aria-labelledby="family-vaults-list-heading"><h3 id="family-vaults-list-heading">Vaults</h3><table id="family-vaults-table" aria-label="Family vaults"><thead><tr><th scope="col">{peppyCopy.family_vaults_col_username}</th><th scope="col">{peppyCopy.family_vaults_col_role}</th><th scope="col">{peppyCopy.family_vaults_col_devices}</th><th scope="col">{peppyCopy.family_vaults_col_storage}</th><th scope="col">{peppyCopy.family_vaults_col_created}</th></tr></thead><tbody>{vaults.map((vault) => <tr id={`family-vaults-vault-${vault.vaultId}`} key={vault.vaultId} data-vault-id={vault.vaultId}><td>{vault.username}{vault.isAdmin && <span className="family-vaults-admin-badge"> ({peppyCopy.family_vaults_admin_badge_label})</span>}</td><td>{vault.isAdmin ? "Admin" : "Member"}</td><td>{vault.deviceCount}</td><td>{formatBytes(vault.storageBytes)}</td><td>{formattedDate(vault.createdAt)}</td></tr>)}</tbody></table></section>
  </section>;
}
