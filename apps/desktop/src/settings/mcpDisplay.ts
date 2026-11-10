import type { McpDeviceState, McpDeviceStatus, McpGrantView } from "@peppy/desktop-ui/bridge-contracts";
import { errorCode } from "../bridge/bridgeErrors";

const stateLabels: Record<McpDeviceState, string> = {
  online: "Online",
  offline: "Offline",
  needs_approval: "Needs approval",
  revoked: "Revoked",
};

const VAULT_ID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export const mcpStateLabel = (state: McpDeviceState) => stateLabels[state];

/** A removed device finished cleanup and has no endpoint to connect to; Re-enable sets it up again. */
export const isRemovedDevice = (device: McpDeviceStatus["device"] | undefined) => device?.state === "revoked" && !device.cleanupPending;

/** Cleanup in progress and removed devices both leave the endpoint unusable. */
export const hasUsableEndpoint = (device: NonNullable<McpDeviceStatus["device"]>) => !device.cleanupPending && !isRemovedDevice(device);

/** Offline is transitional right after enablement, while the worker first syncs. */
export const isStartingState = (state: McpDeviceState | undefined) => state === "offline";

const MCP_ERROR_MESSAGES: Record<string, string> = {
  mcp_enable_in_progress: "Peppy MCP setup is already running. Wait a few minutes, then try again.",
  mcp_device_conflict: "Peppy MCP is already set up. Close this to see its current status.",
  mcp_operation_not_found: "This setup attempt expired or was replaced. Try enabling again.",
  mcp_operation_invalid: "This setup attempt expired or was replaced. Try enabling again.",
  mcp_device_not_enrolled: "Peppy MCP setup didn't finish. Try again.",
  mcp_unavailable: "Peppy MCP isn't reachable right now. Try again in a moment.",
  mcp_worker_unavailable: "Peppy MCP isn't reachable right now. Try again in a moment.",
  mcp_worker_invalid: "Peppy MCP isn't reachable right now. Try again in a moment.",
  owner_required: "Only an owner can enable Peppy MCP.",
  "hosted-session-required": "Your Peppy Hosted sign-in has expired. Sign in again, then select Enable.",
  "session-changed": "Your session changed. Close this and try again.",
  offline: "Couldn't reach the server. Check the connection and try again.",
};

const MCP_GRANT_NOT_FOUND = "mcp_grant_not_found";
const MCP_AUTHORIZATION_NOT_FOUND = "mcp_authorization_not_found";

const messageFor = (reason: unknown) =>
  typeof reason === "object" && reason !== null && "message" in reason && typeof reason.message === "string" && reason.message
    ? reason.message
    : undefined;

export const mcpServerErrorCode = (reason: unknown) => {
  const message = messageFor(reason);
  const directCode = errorCode(reason);
  const embeddedCode = message?.match(/\b(mcp_[a-z_]+)\b/)?.[1];
  const knownCode = (code: string | undefined) => code && (MCP_ERROR_MESSAGES[code] || code === MCP_GRANT_NOT_FOUND || code === MCP_AUTHORIZATION_NOT_FOUND) ? code : undefined;
  return knownCode(directCode) ?? knownCode(embeddedCode);
};

export const mcpErrorMessage = (reason: unknown, fallback: string) => {
  const message = messageFor(reason);
  const code = mcpServerErrorCode(reason);
  return code && MCP_ERROR_MESSAGES[code] ? MCP_ERROR_MESSAGES[code] : message ?? fallback;
};

export const unavailableReasonFor = ({ available, vaultId, unlocked, owner, accountReadOnly, serverOffline, state }: {
  available: boolean;
  vaultId: string | null;
  unlocked: boolean;
  owner: boolean;
  accountReadOnly: boolean;
  serverOffline: boolean;
  state: McpDeviceState | undefined;
}) => {
  if (!available) return "MCP is not configured on this server.";
  if (!vaultId) return "MCP cannot be enabled because the authenticated vault context is unavailable.";
  if (!unlocked) return "Unlock this vault before enabling Peppy MCP.";
  if (!owner) return "Only an owner can enable or manage Peppy MCP.";
  if (accountReadOnly) return "MCP enablement is unavailable while this account is read-only.";
  if (serverOffline) return "MCP enablement is unavailable while the server is offline.";
  if (state === "needs_approval") return "Approval is needed. Remove and re-enable Peppy MCP to create a new enrolled client.";
  return "";
};

export const mcpVaultId = (status: McpDeviceStatus): string | null =>
  typeof status.vaultId === "string" && VAULT_ID_PATTERN.test(status.vaultId) ? status.vaultId : null;

export const grantShortId = (grant: McpGrantView) => grant.id.slice(0, 8);
