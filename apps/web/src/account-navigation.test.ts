import { afterEach, describe, expect, it, vi } from "vitest";
import { fetchAccountUrl, validateAccountUrl } from "./account-navigation";

describe("account navigation metadata", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("accepts a separate HTTPS account host and preserves its path", () => {
    expect(validateAccountUrl("https://account.example.com/account", "app.example.com")).toBe("https://account.example.com/account");
  });

  it.each([
    undefined,
    "http://account.example.com/account",
    "https://app.example.com/account",
    "https://user@account.example.com/account",
    "https://account.example.com/account?source=app",
    "https://account.example.com/account#billing",
    "https://account.example.com/\naccount",
    "https://account.example.com/%0Aaccount",
  ])("rejects unsafe or absent account URLs: %s", (value) => {
    expect(validateAccountUrl(value, "app.example.com")).toBeUndefined();
  });

  it("reads optional metadata without sending credentials", async () => {
    const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({ accountUrl: "https://account.example.com/account" }), { status: 200 }));
    vi.stubGlobal("fetch", fetch);

    await expect(fetchAccountUrl(new AbortController().signal)).resolves.toBe("https://account.example.com/account");
    expect(fetch).toHaveBeenCalledWith("/web/config.json", expect.objectContaining({ credentials: "omit" }));
  });

  it("hides navigation for old, failed, and malformed config responses", async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(new Response("{}", { status: 200 }))
      .mockResolvedValueOnce(new Response("unavailable", { status: 503 }))
      .mockResolvedValueOnce(new Response("not json", { status: 200 }));
    vi.stubGlobal("fetch", fetch);

    await expect(fetchAccountUrl(new AbortController().signal)).resolves.toBeUndefined();
    await expect(fetchAccountUrl(new AbortController().signal)).resolves.toBeUndefined();
    await expect(fetchAccountUrl(new AbortController().signal)).resolves.toBeUndefined();
  });
});
