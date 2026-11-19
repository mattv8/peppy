import type { StatusTone } from "@peppy/desktop-ui";
import type { DesktopSnapshot } from "../bridge/bridge";
import { peppyCopy } from "../generated/peppyCopy";

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
  "server-unavailable": "server offline; reconnecting",
  "live-unavailable": "server offline; reconnecting",
  "live-closed": "server offline; reconnecting",
  "live-timeout": "live connection timed out; reconnecting",
  "notification-post": "could not show a notification on this computer",
  "snapshot-too-large": peppyCopy.status_history_too_large,
};

const SERVER_UNREACHABLE_CODES = new Set([
  "offline",
  "server-unavailable",
  "live-unavailable",
  "live-closed",
  "live-timeout",
]);

/** A confirmed transport outage, excluding connecting and stale connected snapshots. */
export function isServerUnreachable(connection: Connection): boolean {
  return connection.state !== "connected" && SERVER_UNREACHABLE_CODES.has(connection.errorCode ?? "");
}

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

export function historyStatus(progress?: DesktopSnapshot["syncProgress"]): { phase: "receiving" | "applying"; value: string } | undefined {
  if (progress?.phase === "applying") return { phase: "applying", value: "Organizing" };
  if (progress?.phase !== "receiving") return undefined;
  const received = Number(progress.snapshotReceived);
  const expected = Number(progress.snapshotExpected);
  if (progress.snapshotReceived === undefined || progress.snapshotExpected === undefined ||
      !Number.isSafeInteger(received) || !Number.isSafeInteger(expected) ||
      received < 0 || expected <= 0 || received > expected) {
    return { phase: "receiving", value: "Loading" };
  }
  const number = new Intl.NumberFormat();
  return { phase: "receiving", value: `Loading ${number.format(received)} of ${number.format(expected)} items` };
}

export function statusTone(connection: Connection, encryption: EncryptionState, serverUnreachable = false): StatusTone {
  if (connection.state === "error" || connection.state === "missing-native-host") return "error";
  if (encryption === "mismatch") return "warning";
  if (serverUnreachable) return "warning";
  if (connection.errorCode === "snapshot-too-large") return "warning";
  return connection.state === "connected" && encryption === "unlocked" ? "ok" : "neutral";
}

export function statusSummary(connection: Connection, encryption: EncryptionState): string {
  return `Connection: ${connectionText(connection)}. ${syncStatusText(encryption)}`;
}
