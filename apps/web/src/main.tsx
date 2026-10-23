import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { App } from "../../desktop/src/App";
import { installBrowserBridge } from "./ui-bridge";
import "../../desktop/src/app.css";
import { BrowserBridge } from "./browser-bridge";
import { exchangeHostedCallback, fetchHostedWebConfig, retryHostedAuthorizationAfterSignIn } from "./newhosted-app-auth";
import { InstanceManagement } from "./InstanceManagement";
import "./web.css";
import { startBrowserApplication } from "./browser-startup";
import type { HostedWebConfig } from "./newhosted-app-auth";

function unsupported(reasons: string[]): void {
  document.getElementById("root")!.innerHTML = `<main id="browser-unsupported" data-browser-screen="browser-unsupported" role="main" aria-label="Browser not supported"><section><h1 id="unsupported-heading" tabindex="-1">This browser isn't supported</h1><p>Peppy requires a recent desktop browser with SharedWorker, WebAssembly, IndexedDB, and Web Locks support.</p><ul data-unsupported-reasons>${reasons.map((reason) => `<li>${reason}</li>`).join("")}</ul></section></main>`;
  document.getElementById("unsupported-heading")?.focus();
}

function missingCapabilities(): string[] {
  const checks: Array<[boolean, string]> = [
    [typeof SharedWorker !== "undefined", "SharedWorker is not available"],
    [typeof indexedDB !== "undefined", "IndexedDB is not available"],
    [typeof WebAssembly !== "undefined", "WebAssembly is not available"],
    [typeof navigator.locks !== "undefined", "Web Locks are not available"],
  ];
  return checks.filter(([available]) => !available).map(([, reason]) => reason);
}

function showBooting(): void {
  document.getElementById("root")!.innerHTML = '<main id="browser-booting" data-browser-screen="browser-booting"><span role="status" aria-live="polite" aria-label="Starting Peppy"><span class="browser-boot-spinner" aria-hidden="true"></span></span></main>';
}

function showStopped(reason: string): void {
  const locked = reason === "locked";
  const conflictingWorker = reason === "already-open";
  const heading = locked ? "Peppy is locked" : conflictingWorker ? "Peppy could not use this worker" : "Peppy stopped";
  const message = locked ? "This shared browser session was locked. Reload to reconnect securely." : conflictingWorker ? "Another incompatible Peppy worker owns this browser session. Close that copy, then try again." : "Your running Peppy session stopped. Reload to reconnect securely.";
  document.getElementById("root")!.innerHTML = `<main id="browser-stopped" data-browser-screen="browser-stopped" data-stop-reason="${reason}" role="main" aria-labelledby="browser-stopped-heading"><section><h1 id="browser-stopped-heading" tabindex="-1">${heading}</h1><p>${message}</p><button id="browser-stopped-reload" class="primary-button" type="button">Reload Peppy</button></section></main>`;
  document.getElementById("browser-stopped-reload")?.addEventListener("click", () => location.reload());
  document.getElementById("browser-stopped-heading")?.focus();
}

function showStartupFailure(reason: string): void {
  const memoryFailure = reason === "wasm-memory";
  const heading = memoryFailure ? "Not enough memory" : "Peppy could not start";
  const message = memoryFailure ? "Peppy could not allocate enough memory to start. Close other tabs and try again." : "Peppy could not start its secure browser worker. Check that this browser supports the required features, then reload.";
  document.getElementById("root")!.innerHTML = `<main id="browser-startup-failure" data-browser-screen="${memoryFailure ? "browser-wasm-memory" : "browser-startup-failure"}" data-stop-reason="${reason}" role="main" aria-labelledby="browser-startup-failure-heading"><section><h1 id="browser-startup-failure-heading" tabindex="-1">${heading}</h1><p>${message}</p><button id="browser-startup-retry" class="primary-button" type="button">Try again</button></section></main>`;
  document.getElementById("browser-startup-retry")?.addEventListener("click", () => location.reload());
  document.getElementById("browser-startup-failure-heading")?.focus();
}

function BrowserApp({ bridge, config }: { bridge: BrowserBridge; config: HostedWebConfig | undefined }) {
  const accountUrl = config?.accountUrl;
  const apiOrigin = config?.apiOrigin ?? location.origin;
  const [enrollment, setEnrollment] = useState<{ mode: "community" | "hosted"; owner?: boolean }>();

  useEffect(() => { bridge.setAccountUrl(accountUrl); }, [accountUrl, bridge]);

  useEffect(() => {
    void bridge.enrollment_status().then(({ mode, owner }) => setEnrollment({ mode, owner })).catch(() => undefined);
    return bridge.subscribe(() => { void bridge.enrollment_status().then(({ mode, owner }) => setEnrollment({ mode, owner })).catch(() => undefined); });
  }, [bridge]);

  if (location.pathname === "/manage") return <InstanceManagement bridge={bridge} accountUrl={accountUrl} />;
  return <div id="browser-app"><App hostKind="browser" fixedOrigin={apiOrigin} accountUrl={accountUrl} />{enrollment?.mode === "community" && enrollment.owner && <a id="browser-manage-server-link" href="/manage">Manage server</a>}</div>;
}

function boot(): void {
  const reasons = missingCapabilities();
  if (reasons.length) return unsupported(reasons);
  showBooting();
  try {
    const worker = new SharedWorker("/worker.js", { type: "module", name: "peppy-browser-enrollment-v1" });
    const bridge = new BrowserBridge(worker.port);
    installBrowserBridge(bridge);
    const root = createRoot(document.getElementById("root")!);
    let ready = false;
    let disposed = false;
    bridge.onReady(() => {
      if (ready || disposed) return;
      ready = true;
      void startBrowserApplication({
        fetchConfig: fetchHostedWebConfig,
        exchangeCallback: exchangeHostedCallback,
        setCsrf: csrf => bridge.enrollment_set_hosted_csrf(csrf),
        retryAuthorization: retryHostedAuthorizationAfterSignIn,
        mount: config => root.render(<StrictMode><BrowserApp bridge={bridge} config={config} /></StrictMode>),
        isActive: () => !disposed,
      });
    });
    bridge.onStopped((reason) => { disposed = true; root.unmount(); if (ready) showStopped(reason); else showStartupFailure(reason); });
    worker.onerror = () => bridge.workerError();
    window.addEventListener("pagehide", () => {
      disposed = true;
      bridge.dispose();
      root.unmount();
      document.getElementById("root")!.replaceChildren();
    }, { once: true });
    window.addEventListener("pageshow", (event) => { if (event.persisted) location.reload(); }, { once: true });
  } catch {
    unsupported(["Peppy could not start its browser worker"]);
  }
}

boot();
