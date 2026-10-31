export function renderBrowserStopped(container: HTMLElement, reason: string, reload: () => void): void {
  const cleanupFailed = reason === "locked-recovery-clear-failed";
  const locked = reason === "locked" || cleanupFailed;
  const conflictingWorker = reason === "already-open";
  const heading = locked ? "Peppy is locked" : conflictingWorker ? "Peppy could not use this worker" : "Peppy stopped";
  const message = cleanupFailed
    ? "Sync is locked, but this browser may still remember your Vault key phrase."
    : locked ? "This shared browser session was locked. Reload to reconnect securely."
    : conflictingWorker ? "Another incompatible Peppy worker owns this browser session. Close that copy, then try again."
    : "Your running Peppy session stopped. Reload to reconnect securely.";

  container.innerHTML = '<main id="browser-stopped" data-browser-screen="browser-stopped" role="main" aria-labelledby="browser-stopped-heading"><section id="browser-stopped-content"><h1 id="browser-stopped-heading" tabindex="-1"></h1><p id="browser-stopped-message"></p><button id="browser-stopped-reload" class="primary-button" type="button">Reload Peppy</button></section></main>';
  container.querySelector<HTMLElement>("#browser-stopped")!.dataset.stopReason = reason;
  const title = container.querySelector<HTMLElement>("#browser-stopped-heading")!;
  title.textContent = heading;
  container.querySelector("#browser-stopped-message")!.textContent = message;
  container.querySelector("#browser-stopped-reload")!.addEventListener("click", reload);
  title.focus();
}
