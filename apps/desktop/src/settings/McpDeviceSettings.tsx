import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Modal } from "@peppy/desktop-ui";
import type { McpDeviceStatus, McpGrantView } from "@peppy/desktop-ui/bridge-contracts";
import { bridge, type DeviceRosterView } from "../bridge/bridge";
import { McpDetailsModal } from "./McpDetailsModal";
import { grantShortId, isRemovedDevice, isStartingState, mcpErrorMessage, mcpServerErrorCode, mcpStateLabel, mcpVaultId, unavailableReasonFor } from "./mcpDisplay";

type McpDeviceSettingsProps = {
  roster: DeviceRosterView;
  serverOffline: boolean;
  accountReadOnly: boolean;
  sessionKey: string;
  unlocked?: boolean;
  refreshToken?: number;
  onDeviceId?(deviceId: string | null): void;
  onRemove?(device: DeviceRosterView["devices"][number], opener: HTMLButtonElement): void;
  onMutation?(): Promise<void> | void;
};

const POLL_INTERVAL_MS = 5_000;
const MAX_STATUS_POLLS = 24;
const STALE_STATUS_MESSAGE = "Couldn't refresh status. Showing the last known status.";
const COPY_FAILED_MESSAGE = "Couldn't copy. Select the endpoint below.";
const STILL_OFFLINE_MESSAGE = "Still offline. Select Refresh status to check again.";
const ROW_STILL_OFFLINE_MESSAGE = "Still offline. Open Details to check again.";
const CLEANUP_STILL_WAITING_MESSAGE = "Cleanup is still running. Select Refresh status to check again.";
const ROW_CLEANUP_STILL_WAITING_MESSAGE = "Cleanup is still running. Open Details to check again.";
const CLEANUP_HINT = "Cleanup is running. Status updates automatically.";
const OFFLINE_HINT = "Peppy MCP is offline. Checking for it to connect.";

/** Appends the bridge's sanitized failure message, never raw or untrusted text. */
const staleStatusMessage = (reason: unknown) => {
  const detail = mcpErrorMessage(reason, "");
  return detail ? `${STALE_STATUS_MESSAGE} ${detail}` : STALE_STATUS_MESSAGE;
};

/** The removed notice owns the announcement, so a removed device shows no separate status text. */
const statusHintFor = ({ removed, exhausted, cleanupPending, starting, statusNotice }: { removed: boolean; exhausted: boolean; cleanupPending: boolean; starting: boolean; statusNotice: string }) => {
  if (removed) return "";
  if (exhausted) return cleanupPending ? CLEANUP_STILL_WAITING_MESSAGE : STILL_OFFLINE_MESSAGE;
  if (cleanupPending) return CLEANUP_HINT;
  if (starting) return OFFLINE_HINT;
  return statusNotice;
};

export function McpDeviceSettings({ roster, serverOffline, accountReadOnly, sessionKey, unlocked = false, refreshToken = 0, onDeviceId, onRemove, onMutation }: McpDeviceSettingsProps) {
  const [status, setStatus] = useState<McpDeviceStatus | null>(null);
  const [grants, setGrants] = useState<McpGrantView[]>([]);
  const [error, setError] = useState("");
  const [enableError, setEnableError] = useState("");
  const [consentOpen, setConsentOpen] = useState(false);
  const [consented, setConsented] = useState(false);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const [revokeTarget, setRevokeTarget] = useState<McpGrantView | null>(null);
  const [pending, setPending] = useState(false);
  const [revokingGrantId, setRevokingGrantId] = useState<string | null>(null);
  const [loadingGrants, setLoadingGrants] = useState(false);
  const [grantError, setGrantError] = useState("");
  const [refreshingStatus, setRefreshingStatus] = useState(false);
  const [statusError, setStatusError] = useState("");
  const [copyStatus, setCopyStatus] = useState("");
  const [statusNotice, setStatusNotice] = useState("");
  const [grantNotice, setGrantNotice] = useState("");
  const [revokeError, setRevokeError] = useState("");
  const [pollingExhausted, setPollingExhausted] = useState(false);
  const [enableSettling, setEnableSettling] = useState(false);
  /** Bumped when a new poll budget starts (enable or Details open) so the polling interval restarts with 24 fresh polls. */
  const [pollRun, setPollRun] = useState(0);
  const sequence = useRef(0);
  const statusSequence = useRef(0);
  const grantRequest = useRef(0);
  const statusInFlight = useRef<number | null>(null);
  const pollActiveRef = useRef(false);
  const detailsGeneration = useRef(0);
  const sessionGeneration = useRef(0);
  const statusRef = useRef<McpDeviceStatus | null>(null);
  const pollCount = useRef(0);
  const revokeTargetRef = useRef<McpGrantView | null>(null);
  const revokeInFlight = useRef(false);
  const onDeviceIdRef = useRef(onDeviceId);
  const context = useRef(sessionKey);
  const mounted = useRef(true);
  const rowRef = useRef<HTMLLIElement>(null);
  const custodyOpener = useRef<HTMLButtonElement>(null);
  const detailsOpener = useRef<HTMLButtonElement>(null);
  const grantsHeading = useRef<HTMLHeadingElement>(null);
  const current = roster.devices.find(device => device.id === roster.currentDeviceId);
  const owner = current?.role === "owner" && !current.revoked;
  const device = status?.device ?? null;
  const statusVaultId = status ? mcpVaultId(status) : null;
  const canEnable = owner && unlocked && !serverOffline && !accountReadOnly && status?.available === true && (!status.device || (status.device.state === "revoked" && !status.device.cleanupPending)) && statusVaultId !== null;
  const canManageGrants = owner && !serverOffline;
  const polling = Boolean(device?.cleanupPending);
  /** Right after a successful enable, offline is transitional and polls even with details closed. */
  const settling = enableSettling && isStartingState(device?.state);
  const pollActive = (detailsOpen && polling) || settling;

  useEffect(() => { onDeviceIdRef.current = onDeviceId; }, [onDeviceId]);

  const isCurrentDetails = useCallback((generation: number) => mounted.current && detailsGeneration.current === generation, []);
  const isCurrentSession = useCallback((generation: number) => mounted.current && sessionGeneration.current === generation, []);

  const applyStatus = useCallback((next: McpDeviceStatus) => {
    statusRef.current = next;
    setStatus(next);
    onDeviceIdRef.current?.(next.device?.deviceId ?? null);
    if (!isStartingState(next.device?.state)) setEnableSettling(false);
  }, []);

  const load = useCallback(async () => {
    const request = ++sequence.current;
    try {
      const next = await bridge.mcp_status();
      if (!mounted.current || request !== sequence.current) return;
      applyStatus(next);
      setError("");
    } catch (reason) {
      if (mounted.current && request === sequence.current) setError(mcpErrorMessage(reason, "MCP status could not be loaded."));
    }
  }, [applyStatus]);

  /** Polls stay silent; only a manual refresh marks the button busy. The sequence drops results that a newer request superseded. */
  const refreshStatus = useCallback(async (isCurrent: () => boolean, manual: boolean) => {
    if (!manual && statusInFlight.current !== null) return;
    const request = ++statusSequence.current;
    statusInFlight.current = request;
    if (manual) setRefreshingStatus(true);
    try {
      const next = await bridge.mcp_status();
      if (!isCurrent() || request !== statusSequence.current) return;
      const previous = statusRef.current?.device;
      applyStatus(next);
      setStatusError("");
      if (previous && next.device && previous.state !== next.device.state) setStatusNotice(`Status is now ${mcpStateLabel(next.device.state)}.`);
      else if (previous?.cleanupPending && !next.device?.cleanupPending) setStatusNotice("Cleanup finished.");
    } catch (reason) {
      if (isCurrent() && request === statusSequence.current) setStatusError(staleStatusMessage(reason));
    } finally {
      if (statusInFlight.current === request) statusInFlight.current = null;
      if (manual && isCurrent()) setRefreshingStatus(false);
      if (pollCount.current >= MAX_STATUS_POLLS && isCurrent() && request === statusSequence.current) setPollingExhausted(true);
    }
  }, [applyStatus]);

  const loadGrants = useCallback(async (generation: number) => {
    if (!canManageGrants) return;
    const request = ++grantRequest.current;
    setLoadingGrants(true);
    setGrantError("");
    try {
      const next = await bridge.mcp_grants();
      if (request === grantRequest.current && isCurrentDetails(generation)) {
        setGrants(next);
        if (revokeTargetRef.current && !next.some(grant => grant.id === revokeTargetRef.current?.id)) {
          revokeTargetRef.current = null;
          setRevokeTarget(null);
          setRevokeError("");
        }
      }
    } catch (reason) {
      if (request === grantRequest.current && isCurrentDetails(generation)) setGrantError(mcpErrorMessage(reason, "MCP client grants could not be loaded."));
    } finally {
      if (request === grantRequest.current && isCurrentDetails(generation)) setLoadingGrants(false);
    }
  }, [canManageGrants, isCurrentDetails]);

  const refreshDetails = useCallback((generation: number) => {
    void refreshStatus(() => isCurrentDetails(generation), true);
    if (!isRemovedDevice(statusRef.current?.device)) void loadGrants(generation);
  }, [isCurrentDetails, loadGrants, refreshStatus]);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; detailsGeneration.current += 1; };
  }, []);

  useEffect(() => {
    context.current = sessionKey;
    void load();
  }, [load, sessionKey, roster.currentDeviceId, refreshToken]);

  useEffect(() => () => { sessionGeneration.current += 1; }, []);

  useEffect(() => {
    detailsGeneration.current += 1;
    sessionGeneration.current += 1;
    pollCount.current = 0;
    revokeTargetRef.current = null;
    setConsentOpen(false);
    setConsented(false);
    setEnableError("");
    setDetailsOpen(false);
    setRevokeTarget(null);
    setGrants([]);
    setGrantError("");
    setLoadingGrants(false);
    setRefreshingStatus(false);
    setStatusError("");
    setCopyStatus("");
    setStatusNotice("");
    setGrantNotice("");
    setRevokeError("");
    setRevokingGrantId(null);
    setPollingExhausted(false);
    setEnableSettling(false);
  }, [sessionKey, roster.currentDeviceId]);

  useEffect(() => {
    if (!pollActive) return;
    const session = sessionGeneration.current;
    const interval = setInterval(() => {
      if (pollCount.current >= MAX_STATUS_POLLS) {
        clearInterval(interval);
        return;
      }
      pollCount.current += 1;
      void refreshStatus(() => isCurrentSession(session), false);
      if (pollCount.current === MAX_STATUS_POLLS) {
        clearInterval(interval);
      }
    }, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [isCurrentSession, pollActive, pollRun, refreshStatus]);

  useEffect(() => {
    if (pollActive && !pollActiveRef.current) {
      pollCount.current = 0;
      setPollingExhausted(false);
      setPollRun(run => run + 1);
    }
    pollActiveRef.current = pollActive;
  }, [pollActive]);

  const enable = async () => {
    if (!canEnable || !consented || pending || !owner || serverOffline || accountReadOnly) return;
    setPending(true);
    const enableContext = sessionKey;
    const enableSession = sessionGeneration.current;
    try {
      const next = await bridge.mcp_enable();
      if (!mounted.current || context.current !== enableContext) return;
      applyStatus(next);
      setEnableSettling(isStartingState(next.device?.state));
      pollCount.current = 0;
      setPollingExhausted(false);
      setPollRun(run => run + 1);
      await onMutation?.();
      setConsentOpen(false);
      setConsented(false);
      setEnableError("");
    } catch (reason) {
      if (isCurrentSession(enableSession)) setEnableError(mcpErrorMessage(reason, "MCP could not be enabled."));
      if (mounted.current) void refreshStatus(() => isCurrentSession(enableSession), false);
    } finally {
      if (mounted.current) setPending(false);
    }
  };

  const openDetails = () => {
    const generation = ++detailsGeneration.current;
    grantRequest.current += 1;
    pollCount.current = 0;
    setPollRun(run => run + 1);
    setDetailsOpen(true);
    setCopyStatus("");
    setStatusError("");
    setStatusNotice("");
    setGrantNotice("");
    setRevokeError("");
    setPollingExhausted(false);
    refreshDetails(generation);
  };

  const openConsent = () => {
    setEnableError("");
    setConsentOpen(true);
  };

  const closeConsent = () => {
    setEnableError("");
    setConsentOpen(false);
    setConsented(false);
  };

  const closeDetails = () => {
    detailsGeneration.current += 1;
    grantRequest.current += 1;
    revokeTargetRef.current = null;
    setDetailsOpen(false);
    setCopyStatus("");
    setStatusError("");
    setStatusNotice("");
    setGrantNotice("");
    setRevokeError("");
    setRevokeTarget(null);
  };

  const copyEndpoint = async () => {
    if (!status) return;
    const generation = detailsGeneration.current;
    setCopyStatus("");
    let message: string;
    try {
      if (!navigator.clipboard?.writeText) throw new Error("clipboard unavailable");
      await navigator.clipboard.writeText(`${status.serverOrigin}/mcp`);
      message = "Endpoint copied.";
    } catch {
      message = COPY_FAILED_MESSAGE;
    }
    if (isCurrentDetails(generation)) setCopyStatus(message);
  };

  const startRevoke = (grant: McpGrantView) => {
    revokeTargetRef.current = grant;
    setRevokeError("");
    setRevokeTarget(grant);
  };

  const cancelRevoke = () => {
    revokeTargetRef.current = null;
    setRevokeTarget(null);
  };

  const confirmRevoke = async () => {
    const target = revokeTargetRef.current;
    if (!target || revokeInFlight.current || !canManageGrants) return;
    const generation = detailsGeneration.current;
    const session = sessionGeneration.current;
    revokeInFlight.current = true;
    setRevokingGrantId(target.id);
    setRevokeError("");
    setGrantNotice(`Revoking ${target.clientName} (${grantShortId(target)})…`);
    try {
      await bridge.mcp_revoke_grant(target.id);
      if (!isCurrentSession(session)) return;
      grantRequest.current += 1;
      setLoadingGrants(false);
      setGrants(currentGrants => currentGrants.filter(grant => grant.id !== target.id));
      if (isCurrentDetails(generation)) setGrantNotice(`Revoked ${target.clientName} (${grantShortId(target)}).`);
      if (isCurrentDetails(generation) && revokeTargetRef.current?.id === target.id) {
        revokeTargetRef.current = null;
        setRevokeTarget(null);
        grantsHeading.current?.focus();
      }
    } catch (reason) {
      if (mcpServerErrorCode(reason) === "mcp_grant_not_found" && isCurrentSession(session)) {
        grantRequest.current += 1;
        setLoadingGrants(false);
        setGrants(currentGrants => currentGrants.filter(grant => grant.id !== target.id));
        if (isCurrentDetails(generation)) setGrantNotice(`${target.clientName} (${grantShortId(target)}) was already revoked.`);
        if (isCurrentDetails(generation) && revokeTargetRef.current?.id === target.id) {
          revokeTargetRef.current = null;
          setRevokeTarget(null);
          grantsHeading.current?.focus();
        }
        return;
      }
      if (isCurrentDetails(generation)) setGrantNotice("");
      if (isCurrentDetails(generation) && revokeTargetRef.current?.id === target.id) {
        setRevokeError(mcpErrorMessage(reason, "The client grant could not be revoked."));
      }
    } finally {
      revokeInFlight.current = false;
      if (isCurrentSession(session)) setRevokingGrantId(null);
    }
  };

  if (!status) {
    return (
      <li id="mcp-device" data-mcp-device-row={error ? "error" : "loading"}>
        <div data-device-details="mcp">
          {error ? (
            <>
              <p id="mcp-device-error" className="settings-error" role="alert">{error}</p>
              <button id="mcp-status-retry" type="button" className="secondary-button" onClick={() => void load()}>Retry MCP status</button>
            </>
          ) : (
            <span>Loading Peppy MCP…</span>
          )}
        </div>
      </li>
    );
  }

  const custodyName = status.hosted ? "Peppy" : "This server";
  const matchedDevice = device ? roster.devices.find(candidate => candidate.id === device.deviceId) : undefined;
  const unavailableReason = unavailableReasonFor({ available: status.available, vaultId: statusVaultId, unlocked, owner, accountReadOnly, serverOffline, state: device?.state });
  const exhausted = pollingExhausted && pollActive;
  const statusHint = statusHintFor({ removed: isRemovedDevice(device), exhausted, cleanupPending: device?.cleanupPending === true, starting: settling, statusNotice });

  return (
    <>
      <li id={device ? `mcp-device-${device.deviceId}` : "mcp-device"} ref={rowRef} tabIndex={-1} data-mcp-device-row data-device-id={device?.deviceId}>
        <div data-device-details="mcp">
          <strong>Peppy MCP</strong>
          <span data-device-metadata="mcp">
            <span data-device-badge="platform">Server client</span>
            {device && <span data-device-badge="presence" data-presence-state={device.state === "online" ? "connected" : "delayed"}>{mcpStateLabel(device.state)}</span>}
            {device?.cleanupPending && <span data-device-badge="cleanup">Cleanup pending</span>}
          </span>
          {device?.lastSyncedAt && <small>Last sync: {device.lastSyncedAt}</small>}
          {unavailableReason && <small data-mcp-unavailable>{unavailableReason}</small>}
          {!detailsOpen && pollActive && <small id="mcp-row-notice" data-mcp-row-notice role="status">{exhausted ? device?.cleanupPending ? ROW_CLEANUP_STILL_WAITING_MESSAGE : ROW_STILL_OFFLINE_MESSAGE : statusHint}</small>}
          {error && <p id="mcp-device-error" className="settings-error" role="alert">{error}</p>}
        </div>
        {device ? (
          <span data-mcp-actions>
            <button id="mcp-details-open" ref={detailsOpener} type="button" className="secondary-button" aria-label="Peppy MCP details" onClick={openDetails}>Details</button>
            {matchedDevice && owner && !serverOffline && (
              <button id="mcp-device-remove" type="button" className="secondary-button" aria-label="Remove Peppy MCP" onClick={event => onRemove?.(matchedDevice, event.currentTarget)}>Remove</button>
            )}
            {canEnable && (
              <button id="mcp-custody-review" ref={custodyOpener} type="button" className="secondary-button" aria-label="Review custody consent for Peppy MCP" onClick={openConsent}>Re-enable</button>
            )}
          </span>
        ) : canEnable && (
          <button id="mcp-custody-review" ref={custodyOpener} type="button" className="secondary-button" aria-label="Review custody consent for Peppy MCP" onClick={openConsent}>Review custody consent</button>
        )}
      </li>
      {typeof document !== "undefined" && createPortal(<>
        {consentOpen && (
          <Modal
            id="mcp-custody-consent"
            title="Enable Peppy MCP"
            busy={pending}
            opener={custodyOpener}
            focusFallback={rowRef}
            onDismiss={() => !pending && closeConsent()}
            actions={<>
              <button id="mcp-custody-consent-cancel" type="button" className="secondary-button" disabled={pending} onClick={closeConsent}>Cancel</button>
              <button id="mcp-custody-consent-enable" type="button" className="primary-button" aria-busy={pending || undefined} disabled={!consented || pending || !canEnable} onClick={() => void enable()}>Enable Peppy MCP</button>
            </>}
          >
            {enableError && <p className="settings-error" role="alert">{enableError}</p>}
            {pending && <p id="mcp-custody-consent-progress" role="status">Setting up Peppy MCP…</p>}
            <p>{custodyName} at {status.serverOrigin} will be able to decrypt vault {statusVaultId} to run its enrolled MCP client. It can send real messages without per-message prompts when you authorize a client.</p>
            <label className="settings-check">
              <input id="mcp-custody-consent-check" type="checkbox" checked={consented} disabled={pending} onChange={event => setConsented(event.target.checked)} />
              <span>{custodyName} can decrypt this vault for Peppy MCP.</span>
            </label>
          </Modal>
        )}
        {detailsOpen && device && (
          <McpDetailsModal
            opener={detailsOpener}
            focusFallback={rowRef}
            status={status}
            device={device}
            canReenable={canEnable}
            refreshing={refreshingStatus}
            copyStatus={copyStatus}
            statusNotice={statusHint}
            statusError={statusError}
            onCopy={() => void copyEndpoint()}
            onRefresh={() => refreshDetails(detailsGeneration.current)}
            onClose={closeDetails}
            grants={{
              headingRef: grantsHeading,
              manageable: canManageGrants,
              offline: serverOffline,
              loading: loadingGrants,
              loadError: grantError,
              grants,
              notice: grantNotice,
              revokeError,
              revokeTargetId: revokeTarget?.id ?? null,
               revoking: revokingGrantId !== null,
               revokingGrantId,
              onRetryLoad: () => void loadGrants(detailsGeneration.current),
              onStartRevoke: startRevoke,
              onCancelRevoke: cancelRevoke,
              onConfirmRevoke: () => void confirmRevoke(),
            }}
          />
        )}
      </>, document.body)}
    </>
  );
}
