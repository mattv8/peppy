type WebConfig = { accountUrl?: unknown };

const CONTROL_CHARACTER = /[\u0000-\u001F\u007F]/;

export function validateAccountUrl(value: unknown, appHost = location.hostname): string | undefined {
  if (typeof value !== "string" || CONTROL_CHARACTER.test(value)) return undefined;

  try {
    const url = new URL(value);
    if (
      url.protocol !== "https:" ||
      !url.hostname ||
      url.hostname.toLowerCase() === appHost.toLowerCase() ||
      url.username ||
      url.password ||
      url.search ||
      url.hash
    ) {
      return undefined;
    }
    if (CONTROL_CHARACTER.test(decodeURIComponent(url.href))) return undefined;
    return url.href;
  } catch {
    return undefined;
  }
}

export async function fetchAccountUrl(signal: AbortSignal): Promise<string | undefined> {
  try {
    const response = await fetch("/web/config.json", { credentials: "omit", signal });
    if (!response.ok) return undefined;
    const config = await response.json() as WebConfig;
    return validateAccountUrl(config.accountUrl);
  } catch {
    return undefined;
  }
}
