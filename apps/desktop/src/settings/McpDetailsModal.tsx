import type { RefObject } from "react";
import { Modal } from "@peppy/desktop-ui";
import type { McpDeviceStatus } from "@peppy/desktop-ui/bridge-contracts";
import { McpGrantsSection, type McpGrantsSectionProps } from "./McpGrantsSection";
import { mcpStateLabel } from "./mcpDisplay";

type McpDevice = NonNullable<McpDeviceStatus["device"]>;

export type McpDetailsModalProps = {
  opener: RefObject<HTMLButtonElement | null>;
  focusFallback: RefObject<HTMLLIElement | null>;
  status: McpDeviceStatus;
  device: McpDevice;
  refreshing: boolean;
  copyStatus: string;
  statusNotice: string;
  statusError: string;
  grants: McpGrantsSectionProps;
  onCopy(): void;
  onRefresh(): void;
  onClose(): void;
};

export function McpDetailsModal({ opener, focusFallback, status, device, refreshing, copyStatus, statusNotice, statusError, grants, onCopy, onRefresh, onClose }: McpDetailsModalProps) {
  const endpoint = `${status.serverOrigin}/mcp`;
  return (
    <Modal
      id="mcp-device-details"
      title="Peppy MCP"
      opener={opener}
      focusFallback={focusFallback}
      onDismiss={onClose}
      actions={<button type="button" className="secondary-button" onClick={onClose}>Close</button>}
    >
      <div data-mcp-detail-actions>
        <button type="button" className="secondary-button" onClick={onCopy}>Copy endpoint</button>
        <button type="button" className="secondary-button" aria-busy={refreshing} disabled={refreshing} onClick={onRefresh}>Refresh status</button>
      </div>
      <p id="mcp-copy-status" role="status" aria-live="polite">{copyStatus}</p>
      <p id="mcp-status-notice" role="status" aria-live="polite">{statusNotice}</p>
      {statusError && <p id="mcp-status-stale" role="alert">{statusError}</p>}
      <dl data-mcp-details>
        <div>
          <dt>Endpoint</dt>
          <dd><code id="mcp-endpoint">{endpoint}</code></dd>
        </div>
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
      </section>
      <McpGrantsSection {...grants} />
    </Modal>
  );
}
