export const UNSUPPORTED_DOWNLOADS_ID = "browser-unsupported-downloads";

/** Names what is missing. It offers no continue action: the browser cannot run Peppy. */
export function renderUnsupportedPage(container: HTMLElement, reasons: string[]): void {
  const page = document.createElement("main");
  page.id = "browser-unsupported";
  page.dataset.browserScreen = "browser-unsupported";
  page.setAttribute("role", "main");
  page.setAttribute("aria-label", "Browser not supported");
  const section = document.createElement("section");
  const heading = document.createElement("h1");
  heading.id = "unsupported-heading";
  heading.tabIndex = -1;
  heading.textContent = "This browser isn't supported";
  const explanation = document.createElement("p");
  explanation.textContent = "Peppy requires a recent desktop browser with SharedWorker, WebAssembly, IndexedDB, and Web Locks support.";
  const list = document.createElement("ul");
  list.dataset.unsupportedReasons = "";
  reasons.forEach(reason => {
    const item = document.createElement("li");
    item.textContent = reason;
    list.append(item);
  });
  const downloads = document.createElement("div");
  downloads.id = UNSUPPORTED_DOWNLOADS_ID;
  downloads.dataset.unsupportedDownloads = "";
  section.append(heading, explanation, list, downloads);
  page.append(section);
  container.replaceChildren(page);
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
