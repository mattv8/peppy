import { describe, expect, it } from "vitest";
import { BrowserBridge } from "./browser-bridge";
import * as uiBridge from "./ui-bridge";
import { renewHostedSession } from "./ui-bridge";

describe("browser UI bridge", () => {
  it("rejects native-only claimant discard without reporting a no-op cancellation as success", async () => {
    const sent: unknown[] = [];
    const port = {
      start() {},
      close() {},
      postMessage(message: unknown) { sent.push(message); },
    } as unknown as MessagePort;
    const browserBridge = new BrowserBridge(port);
    uiBridge.installBrowserBridge(browserBridge);

    try {
      expect(uiBridge).toHaveProperty("discardComputerClaimant");
      await expect(uiBridge.discardComputerClaimant()).rejects.toBeInstanceOf(Error);
      expect(sent).toEqual([]);
    } finally {
      browserBridge.dispose();
    }
  });

  it("rejects the native-only hosted-session renewal", async () => {
    await expect(renewHostedSession()).rejects.toThrow("unavailable in the browser");
  });
});
