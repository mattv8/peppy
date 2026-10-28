export const UNSUPPORTED_DOWNLOADS_ID = "browser-unsupported-downloads";

/** Names what is missing. It offers no continue action: the browser cannot run Peppy. */
export function unsupportedPageMarkup(reasons: string[]): string {
  return `<main id="browser-unsupported" data-browser-screen="browser-unsupported" role="main" aria-label="Browser not supported"><section><h1 id="unsupported-heading" tabindex="-1">This browser isn't supported</h1><p>Peppy requires a recent desktop browser with SharedWorker, WebAssembly, IndexedDB, and Web Locks support.</p><ul data-unsupported-reasons>${reasons.map((reason) => `<li>${reason}</li>`).join("")}</ul><div id="${UNSUPPORTED_DOWNLOADS_ID}" data-unsupported-downloads></div></section></main>`;
}

/**
 * Links to the configured account page, which renders Peppy's real downloads section.
 * The URL comes from same-origin /web/config.json, already validated as HTTPS; nothing is fetched cross-origin.
 */
export function renderAccountDownloadLink(container: HTMLElement, accountUrl: string | undefined): void {
  if (!accountUrl) return;
  const anchor = document.createElement("a");
  anchor.id = "browser-unsupported-account-link";
  anchor.href = accountUrl;
  anchor.rel = "noopener noreferrer";
  anchor.className = "primary-button";
  anchor.textContent = "Get Peppy downloads";
  container.append(anchor);
}
