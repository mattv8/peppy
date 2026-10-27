const RENEWAL_INTERVAL_MS = 15 * 60 * 1000;
const RETRY_BACKOFF_MS = 30 * 1000;
const AUTH_TRANSITION_TIMEOUT_MS = 5 * 1000;
const SESSION_LIFETIME_SECONDS = 2_592_000;
const ACCOUNT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

type Fetch = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;
type Session = { accountId: string; csrf: string };

export type AppSessionRenewalDependencies = {
  fetch: Fetch;
  now: () => number;
  isVisible?: () => boolean;
  onSessionRejected?: () => void | Promise<void>;
  transitionTimeoutMs?: number;
};

function sessionFrom(value: unknown): Session | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const session = value as { signed_in?: unknown; account_id?: unknown; csrf_token?: unknown };
  if (session.signed_in !== true || typeof session.account_id !== "string" || !ACCOUNT_ID.test(session.account_id) || typeof session.csrf_token !== "string" || !session.csrf_token) return undefined;
  return { accountId: session.account_id, csrf: session.csrf_token };
}

function isValidRenewal(value: unknown, accountId: string): boolean {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const renewal = value as { account_id?: unknown; expires_in_seconds?: unknown };
  return Object.keys(renewal).length === 2
    && renewal.account_id === accountId
    && typeof renewal.expires_in_seconds === "number"
    && Number.isInteger(renewal.expires_in_seconds)
    && renewal.expires_in_seconds >= 1
    && renewal.expires_in_seconds <= SESSION_LIFETIME_SECONDS;
}

export class AppSessionRenewal {
  private inFlight?: Promise<void>;
  private nextAttemptAt = 0;
  private authTransitionDepth = 0;
  private terminalNavigationSuppressions = 0;
  private onSessionRejected?: () => void | Promise<void>;
  private readonly isVisible: () => boolean;
  private readonly interactionListener = (event: Event) => { void this.onInteraction(event); };

  public constructor(private readonly dependencies: AppSessionRenewalDependencies) {
    this.isVisible = dependencies.isVisible ?? (() => document.visibilityState === "visible");
    this.onSessionRejected = dependencies.onSessionRejected;
  }

  public arm(onSessionRejected?: () => void | Promise<void>): () => void {
    this.onSessionRejected = onSessionRejected;
    for (const event of ["pointerdown", "keydown", "touchstart", "wheel"]) window.addEventListener(event, this.interactionListener, { capture: true, passive: true });
    return () => {
      for (const event of ["pointerdown", "keydown", "touchstart", "wheel"]) window.removeEventListener(event, this.interactionListener, true);
    };
  }

  public async onInteraction(event: Pick<Event, "isTrusted">): Promise<void> {
    if (!event.isTrusted || !this.isVisible() || this.authTransitionDepth + this.terminalNavigationSuppressions > 0) return;
    if (this.inFlight) return this.inFlight;
    if (this.dependencies.now() < this.nextAttemptAt) return;
    this.inFlight = this.renew().finally(() => { this.inFlight = undefined; });
    return this.inFlight;
  }

  public async duringAuthTransition<T>(action: () => Promise<T> | T, options: { persistsUntilUnload?: boolean } = {}): Promise<T> {
    this.authTransitionDepth += 1;
    let persistsUntilUnload = false;
    try {
      await this.waitForInFlightRenewal();
      const result = await action();
      persistsUntilUnload = options.persistsUntilUnload === true;
      return result;
    } finally {
      this.authTransitionDepth -= 1;
      if (persistsUntilUnload) this.terminalNavigationSuppressions += 1;
    }
  }

  public resetTerminalNavigationSuppression(): void { this.terminalNavigationSuppressions = 0; }

  private async renew(): Promise<void> {
    try {
      const sessionResponse = await this.dependencies.fetch("/hosted/v1/web/app-session", { credentials: "include" });
      if (sessionResponse.status === 401) return await this.rejectSession();
      if (!sessionResponse.ok) return this.backOff();
      const sessionData = await sessionResponse.json();
      if (isSignedOut(sessionData)) return await this.rejectSession();
      const session = sessionFrom(sessionData);
      if (!session) return this.backOff();
      const renewalResponse = await this.dependencies.fetch("/hosted/v1/web/app-session/renew", {
        method: "POST",
        credentials: "include",
        headers: { "X-Peppy-CSRF": session.csrf },
      });
      if (renewalResponse.status === 401) return await this.rejectSession();
      if (!renewalResponse.ok || !isValidRenewal(await renewalResponse.json(), session.accountId)) return this.backOff();
      this.nextAttemptAt = this.dependencies.now() + RENEWAL_INTERVAL_MS;
    } catch {
      this.backOff();
    }
  }

  private backOff(): void { this.nextAttemptAt = this.dependencies.now() + RETRY_BACKOFF_MS; }
  private async waitForInFlightRenewal(): Promise<void> {
    if (!this.inFlight) return;
    const timeoutMs = this.dependencies.transitionTimeoutMs ?? AUTH_TRANSITION_TIMEOUT_MS;
    await new Promise<void>((resolve, reject) => {
      const timer = window.setTimeout(() => reject(new Error("Session renewal is still in progress. Try again.")), timeoutMs);
      void this.inFlight!.then(
        () => { window.clearTimeout(timer); resolve(); },
        error => { window.clearTimeout(timer); reject(error); },
      );
    });
  }
  private async rejectSession(): Promise<void> {
    this.backOff();
    await this.onSessionRejected?.();
  }
}

export const appSessionRenewal = new AppSessionRenewal({ fetch: (...args) => fetch(...args), now: () => Date.now() });

function isSignedOut(value: unknown): boolean {
  return Boolean(value && typeof value === "object" && !Array.isArray(value) && (value as { signed_in?: unknown }).signed_in === false);
}

export function armHostedSessionRenewal(hostedConfigPresent: boolean, renewal: Pick<AppSessionRenewal, "arm">, onSessionRejected?: () => void | Promise<void>): (() => void) | undefined {
  return hostedConfigPresent ? renewal.arm(onSessionRejected) : undefined;
}

export function restoreAfterPersistedPageShow(event: Pick<PageTransitionEvent, "persisted">, renewal: Pick<AppSessionRenewal, "resetTerminalNavigationSuppression">): void {
  if (event.persisted) renewal.resetTerminalNavigationSuppression();
}
