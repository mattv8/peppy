import { describe, expect, it } from "vitest";
import type { DesktopSnapshot } from "../bridge/bridge";
import { connectionText, historyStatus, isServerUnreachable, statusSummary, statusTone, syncStatusText } from "./status";

type Connection = DesktopSnapshot["connection"];
type EncryptionState = DesktopSnapshot["encryption"]["state"];

describe("history status", () => {
  const progress = { phase: "receiving" as const, receivedThrough: "0", processedThrough: "0" };

  it("reports valid update counts and falls back for invalid ones", () => {
    expect(historyStatus({ ...progress, phase: "updating", updateCompleted: "14592", updateTotal: "91652" }))
      .toEqual({ phase: "updating", value: "Updating your messages: 14,592 of 91,652 items" });
    expect(historyStatus({ ...progress, phase: "updating", updateCompleted: "2", updateTotal: "1" }))
      .toEqual({ phase: "updating", value: "Updating your messages" });
  });

  it("formats valid snapshot counts", () => {
    expect(historyStatus({ ...progress, snapshotReceived: "38400", snapshotExpected: "108708" }))
      .toEqual({ phase: "receiving", value: `Loading ${new Intl.NumberFormat().format(38400)} of ${new Intl.NumberFormat().format(108708)} items` });
    expect(historyStatus({ ...progress, snapshotReceived: "0", snapshotExpected: "1" }))
      .toEqual({ phase: "receiving", value: "Loading 0 of 1 items" });
  });

  it.each([
    [undefined, undefined], ["1", undefined], [undefined, "1"], ["-1", "1"], ["1", "0"],
    ["2", "1"], ["1.5", "2"], ["1", "2.5"], ["NaN", "2"], ["9007199254740992", "9007199254740992"],
  ])("omits invalid counts %s of %s", (snapshotReceived, snapshotExpected) => {
    expect(historyStatus({ ...progress, snapshotReceived, snapshotExpected }))
      .toEqual({ phase: "receiving", value: "Loading" });
  });

  it("shows applying but not current, waiting for keys, or absent progress", () => {
    expect(historyStatus({ ...progress, phase: "applying" })).toEqual({ phase: "applying", value: "Organizing" });
    expect(historyStatus({ ...progress, phase: "waitingForKeys" })).toBeUndefined();
    expect(historyStatus({ ...progress, phase: "current" })).toBeUndefined();
    expect(historyStatus()).toBeUndefined();
  });

  it("reports incomplete history in the status popover", () => {
    expect(historyStatus({ ...progress, phase: "repairing" })).toEqual({ phase: "repairing", value: "History is incomplete; a repair is waiting to run" });
    expect(historyStatus({ ...progress, phase: "blocked" })).toEqual({ phase: "blocked", value: "Couldn't finish loading history" });
  });
});

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
    expect(connectionText({ state: "offline", errorCode: "server-unavailable" })).toBe("Offline — server offline; reconnecting");
  });

  it("keeps a too-large history import connected and warning-toned", () => {
    const connection = { state: "connected" as const, errorCode: "snapshot-too-large" };
    expect(connectionText(connection)).toBe("Connected — history is too large for this version; update Peppy");
    expect(connectionText(connection)).not.toMatch(/Connection error|Offline/);
    expect(statusTone(connection, "unlocked")).toBe("warning");
  });

  it("labels notification posting failures without treating them as server outages", () => {
    expect(connectionText({ state: "error", errorCode: "notification-post" })).toBe(
      "Connection error — could not show a notification on this computer",
    );
    expect(isServerUnreachable({ state: "error", errorCode: "notification-post" })).toBe(false);
  });

  it("includes the changing native connection and sync state in the summary", () => {
    expect(
      statusSummary(
        { state: "offline", errorCode: "live-timeout" },
        "mismatch",
      ),
    ).toBe("Connection: Offline — live connection timed out; reconnecting. Device sync key mismatch");
  });

  it.each(["offline", "server-unavailable", "live-unavailable", "live-closed", "live-timeout"])("recognizes confirmed %s outages", (errorCode) => {
    expect(isServerUnreachable({ state: "offline", errorCode })).toBe(true);
  });

  it("does not treat connecting, unknown, or connected snapshots as unreachable", () => {
    expect(isServerUnreachable({ state: "offline", errorCode: "connecting" })).toBe(false);
    expect(isServerUnreachable({ state: "offline" })).toBe(false);
    expect(isServerUnreachable({ state: "connected", errorCode: "offline" })).toBe(false);
  });

  it("uses the warning tone only for an enrolled confirmed outage", () => {
    expect(statusTone({ state: "offline", errorCode: "server-unavailable" }, "unlocked", true)).toBe("warning");
    expect(statusTone({ state: "offline", errorCode: "server-required" }, "locked", false)).toBe("neutral");
  });
});
