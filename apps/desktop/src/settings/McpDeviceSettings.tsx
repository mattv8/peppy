import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Modal } from "@peppy/desktop-ui";
import type { McpDeviceStatus, McpGrantView } from "@peppy/desktop-ui/bridge-contracts";
import { bridge, type DeviceRosterView } from "../bridge/bridge";
import { McpDetailsModal } from "./McpDetailsModal";
import { errorMessage, grantShortId, isStartingState, mcpStateLabel, mcpVaultId } from "./mcpDisplay";

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
const STILL_WAITING_MESSAGE = "Still waiting. Select Refresh status to check again.";
const ROW_STILL_WAITING_MESSAGE = "Still waiting. Open Details to check again.";
const CLEANUP_HINT = "Cleanup is running. Status updates automatically.";
const PENDING_HINT = "Peppy MCP is starting. Status updates automatically.";

/** Appends the bridge's sanitized failure message, never raw or untrusted text. */
const staleStatusMessage = (reason: unknown) => {
  const detail = errorMessage(reason, "");
  return detail ? `${STALE_STATUS_MESSAGE} ${detail}` : STALE_STATUS_MESSAGE;
};

export function McpDeviceSettings({ roster, serverOffline, accountReadOnly, sessionKey, unlocked = false, refreshToken = 0, onDeviceId, onRemove, onMutation }: McpDeviceSettingsProps) {
  const [status, setStatus] = useState<McpDeviceStatus | null>(null);
  const [grants, setGrants] = useState<McpGrantView[]>([]);
  const [error, setError] = useState("");
  const [consentOpen, setConsentOpen] = useState(false);
  const [consented, setConsented] = useState(false);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const [revokeTarget, setRevokeTarget] = useState<McpGrantView | null>(null);
  const [pending, setPending] = useState(false);
  const [revoking, setRevoking] = useState(false);
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
  const polling = Boolean(device && (device.state === "pending" || device.cleanupPending));
  /** Right after a successful enable, offline or pending is transitional and polls even with details closed. */
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
      if (mounted.current && request === sequence.current) setError(errorMessage(reason, "MCP status could not be loaded."));
    }
  }, [applyStatus]);

  /** Polls stay silent; only a manual refresh marks the button busy. The sequence drops results that a newer request superseded. */
  const refreshStatus = useCallback(async (isCurrent: () => boolean, manual: boolean) => {
    const request = ++statusSequence.current;
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
      if (manual && isCurrent()) setRefreshingStatus(false);
    }
  }, [applyStatus]);

  const loadGrants = useCallback(async (generation: number) => {
    if (!canManageGrants) return;
    setLoadingGrants(true);
    setGrantError("");
    try {
      const next = await bridge.mcp_grants();
      if (isCurrentDetails(generation)) setGrants(next);
    } catch (reason) {
      if (isCurrentDetails(generation)) setGrantError(errorMessage(reason, "MCP client grants could not be loaded."));
    } finally {
      if (isCurrentDetails(generation)) setLoadingGrants(false);
    }
  }, [canManageGrants, isCurrentDetails]);

  const refreshDetails = useCallback((generation: number) => {
    void refreshStatus(() => isCurrentDetails(generation), true);
    void loadGrants(generation);
  }, [isCurrentDetails, loadGrants, refreshStatus]);

  useEffect(() => {
    context.current = sessionKey;
    mounted.current = true;
    void load();
    return () => { mounted.current = false; detailsGeneration.current += 1; };
  }, [load, sessionKey, roster.currentDeviceId, refreshToken]);

  useEffect(() => () => { sessionGeneration.current += 1; }, []);

  useEffect(() => {
    detailsGeneration.current += 1;
    sessionGeneration.current += 1;
    pollCount.current = 0;
    revokeTargetRef.current = null;
    setConsentOpen(false);
    setConsented(false);
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
    setRevoking(false);
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
        if (isCurrentSession(session)) setPollingExhausted(true);
      }
    }, POLL_INTERVAL_MS);
    return () => clearInterval(interval);
  }, [isCurrentSession, pollActive, pollRun, refreshStatus]);

  const enable = async () => {
    if (!canEnable || !consented || pending || !owner || serverOffline || accountReadOnly) return;
    setPending(true);
    const enableContext = sessionKey;
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
      setError("");
    } catch (reason) {
      if (mounted.current) setError(errorMessage(reason, "MCP could not be enabled."));
    } finally {
      if (mounted.current) setPending(false);
    }
  };

  const openDetails = () => {
    const generation = ++detailsGeneration.current;
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

  const closeDetails = () => {
    detailsGeneration.current += 1;
    revokeTargetRef.current = null;
    setDetailsOpen(false);
    setCopyStatus("");
    setStatusError("");
    setStatusNotice("");
    setGrantNotice("");
    setRevokeError("");
    setRevokeTarget(null);
    setRefreshingStatus(false);
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
    revokeInFlight.current = true;
    setRevoking(true);
    setRevokeError("");
    try {
      await bridge.mcp_revoke_grant(target.id);
      if (!isCurrentDetails(generation)) return;
      setGrants(currentGrants => currentGrants.filter(grant => grant.id !== target.id));
      if (revokeTargetRef.current?.id === target.id) {
        revokeTargetRef.current = null;
        setRevokeTarget(null);
      }
      setGrantNotice(`Revoked ${target.clientName} (${grantShortId(target)}).`);
      grantsHeading.current?.focus();
    } catch (reason) {
      if (isCurrentDetails(generation) && revokeTargetRef.current?.id === target.id) {
        setRevokeError(errorMessage(reason, "The client grant could not be revoked."));
      }
    } finally {
      revokeInFlight.current = false;
      if (isCurrentDetails(generation)) setRevoking(false);
    }
  };

  if (!status) {
    return (
      <li data-mcp-device-row={error ? "error" : "loading"}>
        <div data-device-details="mcp">
          {error ? (
            <>
              <p role="alert">{error}</p>
              <button type="button" className="secondary-button" onClick={() => void load()}>Retry MCP status</button>
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
  const unavailableReason = !status.available
    ? "MCP is not configured on this server."
    : !statusVaultId
      ? "MCP cannot be enabled because the authenticated vault context is unavailable."
      : !unlocked
        ? "Unlock this vault before enabling Peppy MCP."
        : !owner
          ? "Only an owner can enable or manage Peppy MCP."
          : accountReadOnly
            ? "MCP enablement is unavailable while this account is read-only."
            : serverOffline
              ? "MCP enablement is unavailable while the server is offline."
              : device?.state === "needs_approval"
                ? "Approval is needed. Remove and re-enable Peppy MCP to create a new enrolled client."
                : "";
  const exhausted = pollingExhausted && pollActive;
  const statusHint = exhausted
    ? STILL_WAITING_MESSAGE
    : device?.cleanupPending
      ? CLEANUP_HINT
      : device?.state === "pending" || settling
        ? PENDING_HINT
        : statusNotice;

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
          {!detailsOpen && pollActive && <small data-mcp-row-notice role="status">{exhausted ? ROW_STILL_WAITING_MESSAGE : statusHint}</small>}
          {error && <p role="alert">{error}</p>}
        </div>
        {device ? (
          <span data-mcp-actions>
            <button ref={detailsOpener} type="button" className="secondary-button" aria-label="Peppy MCP details" onClick={openDetails}>Details</button>
            {matchedDevice && owner && !serverOffline && (
              <button type="button" className="secondary-button" aria-label="Remove Peppy MCP" onClick={event => onRemove?.(matchedDevice, event.currentTarget)}>Remove</button>
            )}
            {canEnable && (
              <button ref={custodyOpener} type="button" className="secondary-button" aria-label="Review custody consent for Peppy MCP" onClick={() => setConsentOpen(true)}>Re-enable</button>
            )}
          </span>
        ) : canEnable && (
          <button ref={custodyOpener} type="button" className="secondary-button" aria-label="Review custody consent for Peppy MCP" onClick={() => setConsentOpen(true)}>Review custody consent</button>
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
            onDismiss={() => !pending && (setConsentOpen(false), setConsented(false))}
            actions={<>
              <button type="button" className="secondary-button" disabled={pending} onClick={() => { setConsentOpen(false); setConsented(false); }}>Cancel</button>
              <button type="button" className="primary-button" disabled={!consented || pending} onClick={() => void enable()}>Enable Peppy MCP</button>
            </>}
          >
            {error && <p role="alert">{error}</p>}
            <p>{custodyName} at {status.serverOrigin} will be able to decrypt vault {statusVaultId} to run its enrolled MCP client. It can send real messages without per-message prompts when you authorize a client.</p>
            <label className="settings-check">
              <input type="checkbox" checked={consented} disabled={pending} onChange={event => setConsented(event.target.checked)} />
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
              revoking,
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
