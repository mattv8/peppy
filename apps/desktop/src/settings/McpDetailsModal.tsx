import { useEffect, useRef, useState, type RefObject } from "react";
import { peppyCopy } from "../generated/peppyCopy";
import { ExternalLink, Modal } from "@peppy/desktop-ui";
import type { McpDeviceStatus } from "@peppy/desktop-ui/bridge-contracts";
import { bridge } from "../bridge/bridge";
import { McpGrantsSection, type McpGrantsSectionProps } from "./McpGrantsSection";
import { hasUsableEndpoint, isRemovedDevice, mcpStateLabel } from "./mcpDisplay";

type McpDevice = NonNullable<McpDeviceStatus["device"]>;

export type McpDetailsModalProps = {
  opener: RefObject<HTMLButtonElement | null>;
  focusFallback: RefObject<HTMLLIElement | null>;
  status: McpDeviceStatus;
  device: McpDevice;
  canReenable: boolean;
  refreshing: boolean;
  copyStatus: string;
  statusNotice: string;
  statusError: string;
  grants: McpGrantsSectionProps;
  browserHost: boolean;
  onCopy(): void;
  onRefresh(): void;
  onClose(): void;
};

const REMOVED_NOTICE = "Peppy MCP was removed.";
const REENABLE_GUIDANCE = "Close this and select Re-enable to set it up again.";
const MCP_GUIDE_URL = "https://peppy.pro/docs/mcp";

export function McpDetailsModal({ opener, focusFallback, status, device, canReenable, refreshing, copyStatus, statusNotice, statusError, grants, browserHost, onCopy, onRefresh, onClose }: McpDetailsModalProps) {
  const endpoint = `${status.serverOrigin}/mcp`;
  const removed = isRemovedDevice(device);
  const endpointUsable = hasUsableEndpoint(device);
  const closeButton = useRef<HTMLButtonElement>(null);
  const refreshButton = useRef<HTMLButtonElement>(null);
  const refreshWasFocused = useRef(false);
  const [guideError, setGuideError] = useState(false);

  /** Refresh unmounts when the device is removed; keep keyboard focus on a control instead of the page body. */
  useEffect(() => {
    if (!removed) return;
    const active = document.activeElement;
    if (!active || active === document.body) closeButton.current?.focus();
  }, [removed]);

  useEffect(() => {
    if (refreshWasFocused.current && !refreshing && document.activeElement === document.body) refreshButton.current?.focus();
  }, [refreshing]);

  const refresh = () => {
    refreshWasFocused.current = document.activeElement === refreshButton.current;
    onRefresh();
  };
  const openGuide = async () => {
    setGuideError(false);
    try {
      if (!bridge.open_external_url) throw new Error("missing opener");
      await bridge.open_external_url(MCP_GUIDE_URL);
    } catch { setGuideError(true); }
  };

  return (
    <Modal
      id="mcp-device-details"
      title="Peppy MCP"
      opener={opener}
      focusFallback={focusFallback}
      onDismiss={onClose}
      actions={<button id="mcp-details-close" ref={closeButton} type="button" className="secondary-button" onClick={onClose}>Close</button>}
    >
      <div data-mcp-detail-actions>
        {endpointUsable && <button id="mcp-copy-endpoint" type="button" className="secondary-button" onClick={onCopy}>Copy endpoint</button>}
        {!removed && <button id="mcp-refresh-status" ref={refreshButton} type="button" className="secondary-button" aria-busy={refreshing} disabled={refreshing} onClick={refresh}>Refresh status</button>}
      </div>
      <p id="mcp-copy-status" role="status" aria-live="polite">{copyStatus}</p>
      <p id="mcp-status-notice" role="status" aria-live="polite">{statusNotice}</p>
      {removed && <p id="mcp-removed-notice" role="status">{canReenable ? `${REMOVED_NOTICE} ${REENABLE_GUIDANCE}` : REMOVED_NOTICE}</p>}
      {statusError && <p id="mcp-status-stale" role="alert">{statusError}</p>}
      <dl id="mcp-details-summary" data-mcp-details>
        {!removed && (
          <div>
            <dt>Endpoint</dt>
            <dd><code id="mcp-endpoint">{endpoint}</code></dd>
          </div>
        )}
        <div>
          <dt>Status</dt>
          <dd>
            {mcpStateLabel(device.state)}
            {device.cleanupPending && " Cleanup pending"}
            {device.state === "needs_approval" && <small> Remove and re-enable Peppy MCP to create a new enrolled client.</small>}
          </dd>
        </div>
        {device.lastSyncedAt && (
          <div>
            <dt>Last sync</dt>
            <dd>{device.lastSyncedAt}</dd>
          </div>
        )}
      </dl>
      {endpointUsable && (
        <section id="mcp-connect-steps" aria-labelledby="mcp-connect-steps-title">
          <h3 id="mcp-connect-steps-title">Connect an AI client</h3>
          <ol id="mcp-connect-step-list">
            <li>Add the endpoint above as a remote MCP server (Streamable HTTP) in your AI client, using OAuth.</li>
            <li>When your client opens Peppy, unlock it on the vault owner device and approve the request.</li>
            <li>Choose only the permissions the client needs.</li>
            <li>
              To verify, ask your client: <code id="mcp-verify-prompt">List the available Peppy tools and their permissions. Do not read messages or send anything.</code>
            </li>
          </ol>
          <small className="settings-note">The AI client must be able to reach this server address. A cloud-hosted client cannot use an address that works only on a private network.</small>
          <div id="mcp-guide-entry">{browserHost ? (
            <a id="mcp-guide-link" className="link-button" href={MCP_GUIDE_URL} target="_blank" rel="noopener noreferrer"><ExternalLink size={13} aria-hidden />{peppyCopy.mcp_guide}<span className="visually-hidden"> (opens in a new tab)</span></a>
          ) : (
            <button id="mcp-guide-link" type="button" className="link-button" onClick={() => void openGuide()}><ExternalLink size={13} aria-hidden />{peppyCopy.mcp_guide}<span className="visually-hidden"> (opens in your browser)</span></button>
          )}
          {guideError && <p id="mcp-guide-error" className="settings-error" role="alert">{peppyCopy.guide_open_failed}</p>}</div>
        </section>
      )}
      {!removed && <McpGrantsSection {...grants} />}
    </Modal>
  );
}
