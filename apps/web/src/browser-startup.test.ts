// @vitest-environment node
import { describe, expect, it, vi } from "vitest";
import { startBrowserApplication } from "./browser-startup";
import type { HostedWebConfig } from "./newhosted-app-auth";

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  return { promise: new Promise<T>(next => { resolve = next; }), resolve };
}

const hostedConfig: HostedWebConfig = {
  apiOrigin: "https://api.example.test",
  accountUrl: "https://account.example.test/sign-in",
};

describe("browser startup", () => {
  it("waits for callback CSRF storage before mounting with the canonical API origin", async () => {
    const config = deferred<HostedWebConfig | undefined>();
    const csrfStored = deferred<void>();
    const events: string[] = [];
    const mount = vi.fn((next: HostedWebConfig | undefined) => { events.push(`mount:${next?.apiOrigin}`); });

    const startup = startBrowserApplication({
      fetchConfig: () => config.promise,
      exchangeCallback: async setCsrf => {
        events.push("exchange");
        await setCsrf("csrf-token");
        return "complete";
      },
      setCsrf: async () => { events.push("csrf"); await csrfStored.promise; },
      retryAuthorization: vi.fn(),
      mount,
      isActive: () => true,
    });

    expect(mount).not.toHaveBeenCalled();
    config.resolve(hostedConfig);
    await vi.waitFor(() => expect(events).toEqual(["exchange", "csrf"]));
    expect(mount).not.toHaveBeenCalled();
    csrfStored.resolve();
    await startup;

    expect(events).toEqual(["exchange", "csrf", "mount:https://api.example.test"]);
  });

  it("mounts an ordinary anonymous load without attempting authorization", async () => {
    const exchangeCallback = vi.fn();
    const retryAuthorization = vi.fn();
    const mount = vi.fn();

    await startBrowserApplication({
      fetchConfig: async () => undefined,
      exchangeCallback,
      setCsrf: async () => undefined,
      retryAuthorization,
      mount,
      isActive: () => true,
    });

    expect(exchangeCallback).not.toHaveBeenCalled();
    expect(retryAuthorization).not.toHaveBeenCalled();
    expect(mount).toHaveBeenCalledWith(undefined);
  });

  it("retries a pending authorization once when the callback has no completion", async () => {
    const retryAuthorization = vi.fn().mockResolvedValue(true);
    const mount = vi.fn();

    await startBrowserApplication({
      fetchConfig: async () => hostedConfig,
      exchangeCallback: async () => "none",
      setCsrf: async () => undefined,
      retryAuthorization,
      mount,
      isActive: () => true,
    });

    expect(retryAuthorization).toHaveBeenCalledTimes(1);
    expect(retryAuthorization).toHaveBeenCalledWith(hostedConfig);
    expect(mount).toHaveBeenCalledWith(hostedConfig);
  });

  it("does not mount after the browser session stops during callback processing", async () => {
    const callback = deferred<"complete">();
    const mount = vi.fn();
    const exchangeCallback = vi.fn(() => callback.promise);
    let active = true;

    const startup = startBrowserApplication({
      fetchConfig: async () => hostedConfig,
      exchangeCallback,
      setCsrf: async () => undefined,
      retryAuthorization: vi.fn(),
      mount,
      isActive: () => active,
    });

    await vi.waitFor(() => expect(exchangeCallback).toHaveBeenCalledOnce());
    active = false;
    callback.resolve("complete");
    await startup;

    expect(mount).not.toHaveBeenCalled();
  });
});
