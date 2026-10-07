import type { StatusTone } from "@peppy/desktop-ui";
import type { DesktopSnapshot } from "./bridge";

type Connection = DesktopSnapshot["connection"];
type EncryptionState = DesktopSnapshot["encryption"]["state"];

const CONNECTION_LABEL: Record<Connection["state"], string> = {
  connected: "Connected",
  offline: "Offline",
  "missing-native-host": "Native host unavailable",
  error: "Connection error",
};

const CONNECTION_CODE_LABEL: Record<string, string> = {
  "server-required": "configure a server URL",
  "credentials-required": "import device credentials",
  connecting: "connecting…",
  revoked: "this device was revoked; local data is kept",
  "outbox-rejected": "the server rejected queued messages; they stay queued locally",
  "live-timeout": "live connection timed out; reconnecting",
};

export function connectionText(connection: Connection): string {
  const state = CONNECTION_LABEL[connection.state] ?? connection.state;
  if (!connection.errorCode || connection.errorCode === connection.state) return state;
  return `${state} — ${CONNECTION_CODE_LABEL[connection.errorCode] ?? connection.errorCode}`;
}

export function syncStatusText(encryption: EncryptionState): string {
  if (encryption === "unlocked") return "Device sync encrypted";
  if (encryption === "mismatch") return "Device sync key mismatch";
  return "Device sync not unlocked";
}

export function statusTone(connection: Connection, encryption: EncryptionState): StatusTone {
  if (connection.state === "error" || connection.state === "missing-native-host") return "error";
  if (encryption === "mismatch") return "warning";
  return connection.state === "connected" && encryption === "unlocked" ? "ok" : "neutral";
}

export function statusSummary(connection: Connection, encryption: EncryptionState): string {
  return `Connection: ${connectionText(connection)}. ${syncStatusText(encryption)}`;
}
