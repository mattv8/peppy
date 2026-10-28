import { afterEach, describe, expect, it, vi } from "vitest";
import { renderAccountDownloadLink, unsupportedPageMarkup, UNSUPPORTED_DOWNLOADS_ID } from "./browser-unsupported";
import { fetchHostedWebConfig } from "./newhosted-app-auth";

const accountUrl = "https://peppy.pro/account";

function renderPage(reasons: string[]): HTMLElement {
  const root = document.createElement("div");
  root.innerHTML = unsupportedPageMarkup(reasons);
  document.body.append(root);
  return root;
}

describe("unsupported browser page", () => {
  afterEach(() => {
    document.body.replaceChildren();
    vi.unstubAllGlobals();
  });

  it("names each missing capability and offers no continue action", () => {
    const page = renderPage(["Web Locks are not available"]);

    expect(page.querySelector("[data-unsupported-reasons]")?.textContent).toBe("Web Locks are not available");
    expect(page.textContent).not.toMatch(/continue/i);
    expect(page.querySelector("button")).toBeNull();
  });

  it("keeps the download container empty when the account URL is not configured", () => {
    const page = renderPage(["IndexedDB is not available"]);
    const container = page.querySelector<HTMLElement>(`#${UNSUPPORTED_DOWNLOADS_ID}`)!;

    renderAccountDownloadLink(container, undefined);

    expect(container.children).toHaveLength(0);
  });

  it("links the configured account page with no opener", () => {
    const page = renderPage(["SharedWorker is not available"]);
    const container = page.querySelector<HTMLElement>(`#${UNSUPPORTED_DOWNLOADS_ID}`)!;

    renderAccountDownloadLink(container, accountUrl);

    const links = container.querySelectorAll("a");
    expect(links).toHaveLength(1);
    expect(links[0].id).toBe("browser-unsupported-account-link");
    expect(links[0].getAttribute("href")).toBe(accountUrl);
    expect(links[0].getAttribute("rel")).toBe("noopener noreferrer");
  });

  it("takes the link from the same-origin config, never the hosted downloads catalog", async () => {
    const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({ apiOrigin: "https://peppy.pro/", accountUrl, version: 1 }), { status: 200 }));
    vi.stubGlobal("fetch", fetch);
    const page = renderPage(["WebAssembly is not available"]);
    const container = page.querySelector<HTMLElement>(`#${UNSUPPORTED_DOWNLOADS_ID}`)!;

    renderAccountDownloadLink(container, (await fetchHostedWebConfig())?.accountUrl);

    expect(fetch).toHaveBeenCalledTimes(1);
    expect(fetch).toHaveBeenCalledWith("/web/config.json", expect.objectContaining({ credentials: "omit" }));
    expect(container.querySelector("a")?.getAttribute("href")).toBe(accountUrl);
  });

  it("offers no link when the config is absent, failing, or has an unsafe account URL", async () => {
    const fetch = vi.fn()
      .mockResolvedValueOnce(new Response("unavailable", { status: 503 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ apiOrigin: "https://peppy.pro/", accountUrl: "http://peppy.pro/account" }), { status: 200 }))
      .mockRejectedValueOnce(new TypeError("offline"));
    vi.stubGlobal("fetch", fetch);
    const page = renderPage(["Web Locks are not available"]);
    const container = page.querySelector<HTMLElement>(`#${UNSUPPORTED_DOWNLOADS_ID}`)!;

    for (let attempt = 0; attempt < 3; attempt++) {
      renderAccountDownloadLink(container, (await fetchHostedWebConfig())?.accountUrl);
    }

    expect(container.children).toHaveLength(0);
  });
});
