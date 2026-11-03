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

  const page = document.createElement("main");
  page.id = "browser-stopped";
  page.dataset.browserScreen = "browser-stopped";
  page.dataset.stopReason = reason;
  page.setAttribute("role", "main");
  page.setAttribute("aria-labelledby", "browser-stopped-heading");
  const section = document.createElement("section");
  section.id = "browser-stopped-content";
  const title = document.createElement("h1");
  title.id = "browser-stopped-heading";
  title.tabIndex = -1;
  const messageElement = document.createElement("p");
  messageElement.id = "browser-stopped-message";
  const reloadButton = document.createElement("button");
  reloadButton.id = "browser-stopped-reload";
  reloadButton.className = "primary-button";
  reloadButton.type = "button";
  reloadButton.textContent = "Reload Peppy";
  section.append(title, messageElement, reloadButton);
  page.append(section);
  container.replaceChildren(page);
  title.textContent = heading;
  messageElement.textContent = message;
  reloadButton.addEventListener("click", reload);
  title.focus();
}
