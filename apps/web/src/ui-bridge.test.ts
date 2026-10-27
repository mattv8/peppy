import { describe, expect, it } from "vitest";
import { renewHostedSession } from "./ui-bridge";

describe("browser UI bridge", () => {
  it("rejects the native-only hosted-session renewal", async () => {
    await expect(renewHostedSession()).rejects.toThrow("unavailable in the browser");
  });
});
