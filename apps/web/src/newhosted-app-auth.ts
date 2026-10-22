const STORAGE_KEY = "peppy.hosted-app-auth.v1";
const RANDOM_BYTES = 32;

export type HostedWebConfig = { apiOrigin: string; accountUrl: string };
type PendingAuthorization = { verifier: string; state: string; retriedAfterSignIn: boolean };

function base64Url(bytes: Uint8Array): string {
  let binary = "";
  bytes.forEach(byte => { binary += String.fromCharCode(byte); });
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

function randomValue(): string {
  const bytes = new Uint8Array(RANDOM_BYTES);
  crypto.getRandomValues(bytes);
  return base64Url(bytes);
}

async function challengeFor(verifier: string): Promise<string> {
  return base64Url(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier))));
}

function readPending(): PendingAuthorization | undefined {
  try {
    const parsed: unknown = JSON.parse(sessionStorage.getItem(STORAGE_KEY) ?? "");
    if (!parsed || typeof parsed !== "object") return undefined;
    const value = parsed as Partial<PendingAuthorization>;
    return typeof value.verifier === "string" && value.verifier.length === 43 && typeof value.state === "string" && value.state.length === 43 && typeof value.retriedAfterSignIn === "boolean" ? value as PendingAuthorization : undefined;
  } catch { return undefined; }
}

function writePending(value: PendingAuthorization): void { sessionStorage.setItem(STORAGE_KEY, JSON.stringify(value)); }
export function clearHostedAuthorization(): void { sessionStorage.removeItem(STORAGE_KEY); }

export function hostedWebConfig(value: unknown): HostedWebConfig | undefined {
  if (!value || typeof value !== "object") return undefined;
  const config = value as { apiOrigin?: unknown; accountUrl?: unknown };
  if (typeof config.apiOrigin !== "string" || typeof config.accountUrl !== "string") return undefined;
  try {
    const api = new URL(config.apiOrigin);
    const account = new URL(config.accountUrl);
    if (api.protocol !== "https:" || account.protocol !== "https:" || api.username || api.password || account.username || account.password) return undefined;
    return { apiOrigin: api.origin, accountUrl: account.href };
  } catch { return undefined; }
}

export async function fetchHostedWebConfig(signal?: AbortSignal): Promise<HostedWebConfig | undefined> {
  try {
    const response = await fetch("/web/config.json", { credentials: "omit", signal });
    return response.ok ? hostedWebConfig(await response.json()) : undefined;
  } catch { return undefined; }
}

export async function beginHostedAuthorization(config: HostedWebConfig, retryAfterSignIn = false): Promise<void> {
  const verifier = randomValue();
  const state = randomValue();
  writePending({ verifier, state, retriedAfterSignIn: retryAfterSignIn });
  const authorize = new URL("/hosted/v1/web/app-authorize", config.accountUrl);
  authorize.searchParams.set("challenge", await challengeFor(verifier));
  authorize.searchParams.set("state", state);
  location.assign(authorize.toString());
}

export async function exchangeHostedCallback(onSession: (csrf: string) => Promise<void>): Promise<"none" | "complete" | "retry"> {
  const url = new URL(location.href);
  const code = url.searchParams.get("enrollment_code");
  if (!code) return "none";
  const state = url.searchParams.get("state");
  const pending = readPending();
  history.replaceState(null, "", `${url.pathname}${url.hash}`);
  if (!pending || state !== pending.state) {
    clearHostedAuthorization();
    return "retry";
  }
  clearHostedAuthorization();
  const response = await fetch("/hosted/v1/web/app-session", {
    method: "POST",
    credentials: "include",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ code, verifier: pending.verifier }),
  });
  if (!response.ok) return "retry";
  const body: unknown = await response.json();
  if (!body || typeof body !== "object" || typeof (body as { csrf_token?: unknown }).csrf_token !== "string") return "retry";
  await onSession((body as { csrf_token: string }).csrf_token);
  return "complete";
}

/** The account host may send an unauthenticated browser through its own sign-in page. */
export async function retryHostedAuthorizationAfterSignIn(config: HostedWebConfig): Promise<boolean> {
  const pending = readPending();
  if (!pending || pending.retriedAfterSignIn) return false;
  await beginHostedAuthorization(config, true);
  return true;
}
