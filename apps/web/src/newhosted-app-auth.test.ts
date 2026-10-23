import { afterEach, describe, expect, it, vi } from "vitest";
import {
  exchangeHostedCallback,
  retryHostedAuthorizationAfterSignIn,
} from "./newhosted-app-auth";
import type { HostedWebConfig } from "./newhosted-app-auth";

const STORAGE_KEY = "peppy.hosted-app-auth.v1";
const verifier = "v".repeat(43);
const state = "s".repeat(43);
const hostedConfig: HostedWebConfig = {
  apiOrigin: "https://api.example.test",
  accountUrl: "https://account.example.test/sign-in",
};

function pending(retriedAfterSignIn = false): string {
  return JSON.stringify({ verifier, state, retriedAfterSignIn });
}

function mockBrowser(href: string): {
  assign: ReturnType<typeof vi.fn>;
  replaceState: ReturnType<typeof vi.fn>;
  storage: Map<string, string>;
} {
  const storage = new Map<string, string>();
  const assign = vi.fn();
  const replaceState = vi.fn();
  vi.stubGlobal("sessionStorage", {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => { storage.set(key, value); },
    removeItem: (key: string) => { storage.delete(key); },
  });
  vi.stubGlobal("location", { href, assign });
  vi.stubGlobal("history", { replaceState });
  vi.stubGlobal("crypto", {
    getRandomValues: (bytes: Uint8Array) => bytes.fill(1),
    subtle: { digest: vi.fn(async () => new Uint8Array(32).fill(2).buffer) },
  });
  return { assign, replaceState, storage };
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("hosted app authorization", () => {
  it("retries a pending authorization once and persists the retry guard", async () => {
    const { assign, storage } = mockBrowser("https://app.example.test/");
    storage.set(STORAGE_KEY, pending());

    await expect(retryHostedAuthorizationAfterSignIn(hostedConfig)).resolves.toBe(true);
    await expect(retryHostedAuthorizationAfterSignIn(hostedConfig)).resolves.toBe(false);

    expect(assign).toHaveBeenCalledTimes(1);
    expect(JSON.parse(storage.get(STORAGE_KEY) ?? "")).toMatchObject({ retriedAfterSignIn: true });
  });

  it("clears an invalid callback state without exchanging or resurrecting its retry", async () => {
    const { assign, replaceState, storage } = mockBrowser("https://app.example.test/callback?enrollment_code=code&state=wrong");
    storage.set(STORAGE_KEY, pending());
    const fetch = vi.fn();
    vi.stubGlobal("fetch", fetch);

    await expect(exchangeHostedCallback(vi.fn())).resolves.toBe("retry");
    await expect(retryHostedAuthorizationAfterSignIn(hostedConfig)).resolves.toBe(false);

    expect(storage.has(STORAGE_KEY)).toBe(false);
    expect(fetch).not.toHaveBeenCalled();
    expect(assign).not.toHaveBeenCalled();
    expect(replaceState).toHaveBeenCalledWith(null, "", "/callback");
  });

  it("stores callback CSRF and scrubs callback parameters after a valid exchange", async () => {
    const { replaceState, storage } = mockBrowser(`https://app.example.test/callback?enrollment_code=code&state=${state}#done`);
    storage.set(STORAGE_KEY, pending());
    const fetch = vi.fn(async () => new Response(JSON.stringify({ csrf_token: "csrf-token" }), { status: 200 }));
    const setCsrf = vi.fn(async () => undefined);
    vi.stubGlobal("fetch", fetch);

    await expect(exchangeHostedCallback(setCsrf)).resolves.toBe("complete");

    expect(setCsrf).toHaveBeenCalledWith("csrf-token");
    expect(storage.has(STORAGE_KEY)).toBe(false);
    expect(replaceState).toHaveBeenCalledWith(null, "", "/callback#done");
    expect(fetch).toHaveBeenCalledWith("/hosted/v1/web/app-session", expect.objectContaining({
      method: "POST",
      credentials: "include",
      body: JSON.stringify({ code: "code", verifier }),
    }));
  });
});
