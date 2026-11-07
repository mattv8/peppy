import type { McpDeviceState, McpDeviceStatus, McpGrantView } from "@peppy/desktop-ui/bridge-contracts";

const stateLabels: Record<McpDeviceState, string> = {
  pending: "Pending",
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

/** Offline and pending are transitional right after enablement, while the worker first syncs. */
export const isStartingState = (state: McpDeviceState | undefined) => state === "offline" || state === "pending";

export const errorMessage = (error: unknown, fallback: string) =>
  error instanceof Error && error.message ? error.message : fallback;

export const mcpVaultId = (status: McpDeviceStatus): string | null =>
  typeof status.vaultId === "string" && VAULT_ID_PATTERN.test(status.vaultId) ? status.vaultId : null;

export const grantShortId = (grant: McpGrantView) => grant.id.slice(0, 8);
