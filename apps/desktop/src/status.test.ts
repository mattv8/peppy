import { describe, expect, it } from "vitest";
import type { DesktopSnapshot } from "./bridge";
import { connectionText, statusSummary, statusTone, syncStatusText } from "./status";

type Connection = DesktopSnapshot["connection"];
type EncryptionState = DesktopSnapshot["encryption"]["state"];

describe("status severity", () => {
  const connectionStates: Connection["state"][] = ["connected", "offline", "missing-native-host", "error"];
  const encryptionStates: EncryptionState[] = ["locked", "unlocked", "preview", "mismatch"];
  const expectedTones: Record<Connection["state"], Record<EncryptionState, "neutral" | "ok" | "warning" | "error">> = {
    connected: { locked: "neutral", unlocked: "ok", preview: "neutral", mismatch: "warning" },
    offline: { locked: "neutral", unlocked: "neutral", preview: "neutral", mismatch: "warning" },
    "missing-native-host": { locked: "error", unlocked: "error", preview: "error", mismatch: "error" },
    error: { locked: "error", unlocked: "error", preview: "error", mismatch: "error" },
  };

  it.each(connectionStates.flatMap((state) => encryptionStates.map((encryption) => [
    { state }, encryption, expectedTones[state][encryption],
  ] as const)))("prioritizes %o with %s as %s", (connection, encryption, expected) => {
    expect(statusTone(connection, encryption)).toBe(expected);
  });

  it.each([
    ["locked", "Device sync not unlocked"],
    ["unlocked", "Device sync encrypted"],
    ["preview", "Device sync not unlocked"],
    ["mismatch", "Device sync key mismatch"],
  ] as const)("maps %s sync state to its disclosure", (state, expected) => {
    expect(syncStatusText(state)).toBe(expected);
  });

  it("renders known native connection codes and leaves unknown codes readable", () => {
    expect(connectionText({ state: "connected", errorCode: "outbox-rejected" })).toBe(
      "Connected — the server rejected queued messages; they stay queued locally",
    );
    expect(connectionText({ state: "offline", errorCode: "live-x" })).toBe("Offline — live-x");
  });

  it("includes the changing native connection and sync state in the summary", () => {
    expect(
      statusSummary(
        { state: "offline", errorCode: "live-timeout" },
        "mismatch",
      ),
    ).toBe("Connection: Offline — live connection timed out; reconnecting. Device sync key mismatch");
  });
});
