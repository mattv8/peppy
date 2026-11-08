import type { McpDeviceStatus, WebhookView } from "@peppy/desktop-ui/bridge-contracts";

export const VAULT_ID = "11111111-1111-4111-8111-111111111111";
export const HOME_ASSISTANT_ID = "00000000-0000-4000-8000-000000000001";
export const ZAPIER_ID = "00000000-0000-4000-8000-000000000002";
export const N8N_ID = "00000000-0000-4000-8000-000000000003";

const minutesAgo = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

export const homeAssistant: WebhookView = {
  id: HOME_ASSISTANT_ID,
  name: "Home Assistant",
  nameIsDerived: true,
  url: "https://ha.example.com/api/webhook/peppy-in",
  events: ["message.received"],
  filterMode: "all",
  filter: [],
  paused: false,
  createdAt: "2026-11-01T10:00:00Z",
  lastDeliveryState: "delivered",
  lastDeliveryAt: minutesAgo(3),
  lastStatusCode: 200,
  lastError: null,
  pendingDeliveries: 0,
};

export const zapier: WebhookView = {
  id: ZAPIER_ID,
  name: "Zapier: Alice & Bob",
  nameIsDerived: false,
  url: "https://hooks.zapier.com/hooks/catch/18/abc123",
  events: ["message.received"],
  filterMode: "only",
  filter: [
    { address: "+12025550101", label: "Alice Chen", contactId: "contact-alice", bookId: "book-1" },
    { address: "+12025550102", label: "Bob Ruiz", contactId: "contact-bob", bookId: "book-1" },
  ],
  paused: false,
  createdAt: "2026-11-02T10:00:00Z",
  lastDeliveryState: "failed",
  lastDeliveryAt: minutesAgo(12),
  lastStatusCode: 503,
  lastError: "http_503",
  pendingDeliveries: 1,
};

export const n8n: WebhookView = {
  id: N8N_ID,
  name: "n8n triage",
  nameIsDerived: false,
  url: "https://n8n.example.org/webhook/7f2c",
  events: ["message.received"],
  filterMode: "except",
  filter: [
    { address: "+12025550125", label: "Mom", contactId: null, bookId: null },
    { address: "+12025550126", label: "Dad", contactId: null, bookId: null },
    { address: "+12025550127", label: null, contactId: null, bookId: null },
  ],
  paused: true,
  createdAt: "2026-11-03T10:00:00Z",
  lastDeliveryState: "never",
  lastDeliveryAt: null,
  lastStatusCode: null,
  lastError: null,
  pendingDeliveries: 0,
};

const serverDevice = { deviceId: "mcp-device", name: "Peppy MCP", state: "online" as const, lastSyncedAt: minutesAgo(0.2), cleanupPending: false };

export const readyStatus: McpDeviceStatus = {
  vaultId: VAULT_ID,
  available: true,
  serverOrigin: "https://vault.example",
  hosted: false,
  device: serverDevice,
};

export const noDeviceStatus: McpDeviceStatus = { ...readyStatus, device: null };

export const offlineStatus: McpDeviceStatus = {
  ...readyStatus,
  device: { ...serverDevice, state: "offline", lastSyncedAt: minutesAgo(5) },
};

export const revokedStatus: McpDeviceStatus = {
  ...readyStatus,
  device: { ...serverDevice, state: "revoked", cleanupPending: false },
};
