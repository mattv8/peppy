import { describe, expect, it } from "vitest";
import { UPDATE_STATUS_SERDE_FIXTURE } from "./update-contract";

describe("UpdateStatus Rust serde parity fixture", () => {
  it("uses exactly the frozen native DTO field names", () => {
    expect(Object.keys(UPDATE_STATUS_SERDE_FIXTURE).sort()).toEqual([
      "automatic", "availableVersion", "canRestart", "channel", "currentVersion",
      "deferredUntil", "lastCheckedAt", "message", "packageKind", "phase",
      "progressPercent", "releaseNotesUrl", "unattended",
    ]);
  });
});
