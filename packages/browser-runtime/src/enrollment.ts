const MAX_JOIN_JSON_BYTES = 64 * 1024;
const JOIN_DEADLINE_MS = 20_000;

export type JoinState = "idle" | "waiting" | "claimed" | "confirm" | "approved" | "expired" | "failed";
export interface JoinView { state: JoinState; qrPayload?: string; expiresInSeconds?: number; sas?: string; origin?: string; errorCode?: string; }
export interface JoinActivation { (metadata: unknown, deviceToken: string, passphrase: string, signal: AbortSignal): Promise<void>; }
interface JoinCore { invoke(request: { command: string; args: Record<string, unknown> }): Promise<unknown>; }
interface Created { join_request_id: string; poll_secret: string; expires_in_seconds: number; }
interface Poll { state: "waiting" | "offered" | "expired"; sealed_intent_token?: string; intent_digest?: string; expires_in_seconds: number; }
interface Claimed { key_digest: string; sas: string; claim_secret: string; expires_in_seconds: number; }
interface Challenge { challenge_token: string; vault_id: string; key_epoch: number; profile_fingerprint: string; requested_role: string; expires_in_seconds: number; }
interface PendingIdentity { metadata: unknown; deviceToken: string; }

/** Worker-private enrollment. All HTTP remains same-origin; apiOrigin is QR/protocol data only. */
export class BrowserEnrollment {
  private state: JoinView = { state: "idle" };
  private pollSecret?: string;
  private intent?: string;
  private claim?: Claimed;
  private pending?: PendingIdentity;
  private generation = 0;
  private active?: AbortController;
  private serial: Promise<void> = Promise.resolve();

  public constructor(private readonly core: JoinCore, private readonly workerOrigin: string, private readonly activate: JoinActivation, private readonly fetcher: typeof fetch = fetch) {}
  public view(): JoinView { return { ...this.state }; }

  public start(signal?: AbortSignal): Promise<JoinView> {
    const generation = this.supersede();
    return this.enqueue(async () => {
      await this.cancelRust();
      if (!this.isCurrent(generation)) return;
      const controller = this.begin(signal);
      try {
        const apiOrigin = await this.configOrigin(controller.signal);
        const request = await anonymousJson<Created>(this.fetcher, this.workerOrigin, "/v1/pairing/join-requests", "POST", undefined, controller.signal);
        requireCreated(request);
        if (!this.isCurrent(generation)) return;
        const joined = object(await this.core.invoke({ command: "_worker_join_start", args: { apiOrigin, joinRequestId: request.join_request_id } }));
        if (!this.isCurrent(generation)) return;
        this.pollSecret = request.poll_secret;
        this.state = { state: "waiting", qrPayload: string(joined.qrPayload), expiresInSeconds: boundedExpiry(request.expires_in_seconds), origin: apiOrigin };
      } catch (error) { this.fail(generation, error); }
    }).then(() => this.view());
  }

  public poll(signal?: AbortSignal): Promise<JoinView> {
    const generation = this.generation;
    return this.enqueue(async () => {
      if (!this.isCurrent(generation) || !["waiting", "claimed"].includes(this.state.state)) return;
      const controller = this.begin(signal);
      try {
        if (this.state.state === "waiting") await this.pollOffer(generation, controller.signal);
        else if (this.state.state === "claimed") await this.pollChallenge(generation, controller.signal);
      } catch (error) { this.fail(generation, error); }
    }).then(() => this.view());
  }

  /** The host passes a secret-only UI phrase directly here; it never becomes a renderer DTO. */
  public confirm(passphrase: string, signal?: AbortSignal): Promise<JoinView> {
    const generation = this.generation;
    return this.enqueue(async () => {
      if (!this.isCurrent(generation) || this.state.state !== "confirm") throw new EnrollmentError("join-state");
      const controller = this.begin(signal);
      try {
        const identity = this.pending ?? await this.consumeIdentity(generation, controller.signal);
        if (!this.isCurrent(generation)) return;
        await this.activate(identity.metadata, identity.deviceToken, passphrase, controller.signal);
        if (!this.isCurrent(generation)) return;
        this.pending = undefined;
        this.state = { state: "approved" };
      } catch (error) {
        if (this.isCurrent(generation) && !isCancelled(error)) throw error;
      }
    }).then(() => this.view());
  }

  public cancel(): void { this.supersede(); void this.enqueue(() => this.cancelRust()); }

  private async pollOffer(generation: number, signal: AbortSignal): Promise<void> {
    const origin = this.state.origin;
    if (!origin || !this.pollSecret) throw new EnrollmentError("join-state");
    const request = object(await this.core.invoke({ command: "_worker_join_request", args: {} }));
    const response = await anonymousJson<Poll>(this.fetcher, this.workerOrigin, `/v1/pairing/join-requests/${encodeURIComponent(string(request.joinRequestId))}`, "GET", undefined, signal, { "Peppy-Join-Secret": this.pollSecret });
    if (!this.isCurrent(generation)) return;
    if (response.state === "waiting") { this.state = { ...this.state, expiresInSeconds: boundedExpiry(response.expires_in_seconds) }; return; }
    if (response.state === "expired") { this.state = { state: "expired", origin }; return; }
    if (!response.sealed_intent_token || !response.intent_digest) throw new EnrollmentError("join-offer-invalid");
    const opened = object(await this.core.invoke({ command: "_worker_join_open_offer", args: { sealedIntentToken: response.sealed_intent_token, intentDigest: response.intent_digest, apiOrigin: origin } }));
    if (!this.isCurrent(generation)) return;
    const intent = string(opened.intent);
    const claim = await anonymousJson<Claimed>(this.fetcher, this.workerOrigin, `/v1/pairing/intents/${encodeURIComponent(intent)}/claim`, "POST", { device_id: opened.deviceId, public_key: opened.publicKey, requested_role: "device" }, signal);
    if (!this.isCurrent(generation)) return;
    await this.core.invoke({ command: "_worker_join_claim", args: { keyDigest: claim.key_digest, sas: claim.sas } });
    if (!this.isCurrent(generation)) return;
    this.intent = intent;
    this.claim = claim;
    this.state = { state: "claimed", sas: claim.sas, origin, expiresInSeconds: boundedExpiry(claim.expires_in_seconds) };
  }

  private async pollChallenge(generation: number, signal: AbortSignal): Promise<void> {
    const origin = this.state.origin;
    if (!origin || !this.intent || !this.claim) throw new EnrollmentError("join-state");
    const request = object(await this.core.invoke({ command: "_worker_join_request", args: {} }));
    try {
      const challenge = await anonymousJson<Challenge>(this.fetcher, this.workerOrigin, `/v1/pairing/intents/${encodeURIComponent(this.intent)}/challenge`, "POST", { device_id: request.deviceId, key_digest: this.claim.key_digest, claim_secret: this.claim.claim_secret }, signal);
      if (!this.isCurrent(generation)) return;
      if (challenge.requested_role !== "device") throw new EnrollmentError("join-role-invalid");
      await this.core.invoke({ command: "_worker_join_challenge", args: {
        challengeToken: challenge.challenge_token,
        vaultId: challenge.vault_id,
        keyEpoch: challenge.key_epoch,
        profileFingerprint: challenge.profile_fingerprint,
        requestedRole: challenge.requested_role,
      } });
      if (this.isCurrent(generation)) this.state = { state: "confirm", sas: this.claim.sas, origin, expiresInSeconds: boundedExpiry(challenge.expires_in_seconds) };
    } catch (error) {
      if (error instanceof EnrollmentHttpError && error.status === 401 && error.serverCode === "pairing_challenge_unavailable") return;
      throw error;
    }
  }

  private async consumeIdentity(generation: number, signal: AbortSignal): Promise<PendingIdentity> {
    const confirmed = object(await this.core.invoke({ command: "_worker_join_confirm", args: {} }));
    const origin = this.state.origin;
    if (!origin || origin !== string(confirmed.apiOrigin) || !this.isCurrent(generation)) throw new EnrollmentError("join-confirm");
    const consumed = await anonymousJson<{ device_token: string; vault_id: string; device_id: string }>(this.fetcher, this.workerOrigin, "/v1/pairing/consume", "POST", { challenge_token: string(confirmed.challengeToken), device_id: string(confirmed.deviceId), public_key: confirmed.publicKey, profile_fingerprint: string(confirmed.profileFingerprint), key_epoch: confirmed.keyEpoch, signature: string(confirmed.signature) }, signal);
    if (!this.isCurrent(generation) || consumed.vault_id !== string(confirmed.vaultId) || consumed.device_id !== string(confirmed.deviceId)) throw new EnrollmentError("join-consume-invalid");
    const deviceToken = string(consumed.device_token);
    const vault = await authenticatedJson<Record<string, unknown>>(this.fetcher, this.workerOrigin, "/v1/vault", deviceToken, signal);
    if (vault.vault_id !== consumed.vault_id || vault.device_id !== consumed.device_id || vault.role !== "device" || vault.profile_fingerprint !== confirmed.profileFingerprint || vault.key_epoch !== confirmed.keyEpoch) throw new EnrollmentError("join-consume-invalid");
    const metadata = { version: 1, origin: canonicalOrigin(this.workerOrigin), vaultId: consumed.vault_id, deviceId: consumed.device_id, role: "device", profile: vault.public_key_profile, header: decodeHeader(vault.encrypted_vault_check_header) };
    const pending = { metadata, deviceToken };
    this.pending = pending;
    return pending;
  }

  private async configOrigin(signal: AbortSignal): Promise<string> {
    const config = await anonymousJson<{ version: number; apiOrigin: string }>(this.fetcher, this.workerOrigin, "/web/config.json", "GET", undefined, signal);
    if (config.version !== 1) throw new EnrollmentError("join-config");
    return canonicalOrigin(config.apiOrigin);
  }
  private supersede(): number { this.generation += 1; this.active?.abort(); this.active = undefined; this.pollSecret = undefined; this.intent = undefined; this.claim = undefined; this.pending = undefined; this.state = { state: "idle" }; return this.generation; }
  private begin(signal?: AbortSignal): AbortController { const controller = this.active = new AbortController(); if (signal?.aborted) controller.abort(); else signal?.addEventListener("abort", () => controller.abort(), { once: true }); return controller; }
  private async cancelRust(): Promise<void> { try { await this.core.invoke({ command: "_worker_join_cancel", args: {} }); } catch { /* cancellation must not revive stale state */ } }
  private enqueue(action: () => Promise<void>): Promise<void> { const next = this.serial.then(action, action); this.serial = next.catch(() => undefined); return next; }
  private isCurrent(generation: number): boolean { return generation === this.generation && !this.active?.signal.aborted; }
  private fail(generation: number, error: unknown): void { if (this.isCurrent(generation) && !isCancelled(error)) this.state = { state: "failed", errorCode: error instanceof EnrollmentError ? error.code : "join-network" }; }
}

export class EnrollmentError extends Error { public constructor(public readonly code: string) { super(code); } }
class EnrollmentHttpError extends EnrollmentError { public constructor(public readonly status: number, public readonly serverCode?: string) { super("join-network"); } }

async function anonymousJson<T>(fetcher: typeof fetch, origin: string, path: string, method: "GET" | "POST", body: unknown, signal: AbortSignal, headers: Record<string, string> = {}): Promise<T> { return requestJson(fetcher, origin, path, method, body, signal, headers); }
async function authenticatedJson<T>(fetcher: typeof fetch, origin: string, path: string, deviceToken: string, signal: AbortSignal): Promise<T> { return requestJson(fetcher, origin, path, "GET", undefined, signal, { Authorization: `Bearer ${deviceToken}` }); }
async function requestJson<T>(fetcher: typeof fetch, origin: string, path: string, method: "GET" | "POST", body: unknown, signal: AbortSignal, headers: Record<string, string>): Promise<T> {
  const url = new URL(path, canonicalOrigin(origin));
  if (url.origin !== canonicalOrigin(origin) || url.search || !url.pathname.startsWith("/v1/") && url.pathname !== "/web/config.json") throw new EnrollmentError("join-network");
  const deadline = new AbortController(); const timer = setTimeout(() => deadline.abort(), JOIN_DEADLINE_MS); const abort = () => deadline.abort(); signal.addEventListener("abort", abort, { once: true });
  try {
    const response = await fetcher(url, { method, body: body === undefined ? undefined : JSON.stringify(body), headers: { ...headers, ...(body === undefined ? {} : { "Content-Type": "application/json" }) }, signal: deadline.signal, credentials: "omit", redirect: "error", cache: "no-store", referrerPolicy: "no-referrer" });
    if (response.redirected || response.url !== "" && response.url !== url.toString()) { cancel(response); throw new EnrollmentError("join-network"); }
    if (!response.ok) { const code = await responseCode(response, deadline.signal); throw new EnrollmentHttpError(response.status, code); }
    const bytes = await bounded(response, deadline.signal); try { return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)) as T; } catch { throw new EnrollmentError("join-network"); }
  } catch (error) { if (signal.aborted) throw new EnrollmentError("cancelled"); if (error instanceof EnrollmentError) throw error; throw new EnrollmentError("join-network"); }
  finally { clearTimeout(timer); signal.removeEventListener("abort", abort); }
}
async function bounded(response: Response, signal: AbortSignal): Promise<Uint8Array> { if (response.body === null) throw new EnrollmentError("join-network"); const reader = response.body.getReader(); const chunks: Uint8Array[] = []; let total = 0; try { while (true) { const part = await reader.read(); if (part.done) break; total += part.value.byteLength; if (total > MAX_JOIN_JSON_BYTES) throw new EnrollmentError("join-network"); chunks.push(part.value); } } catch (error) { await reader.cancel().catch(() => undefined); throw signal.aborted ? new EnrollmentError("cancelled") : error; } finally { reader.releaseLock(); } const bytes = new Uint8Array(total); let offset = 0; for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; } return bytes; }
async function responseCode(response: Response, signal: AbortSignal): Promise<string | undefined> { try { const value = JSON.parse(new TextDecoder().decode(await bounded(response, signal))) as { code?: unknown }; return typeof value.code === "string" && /^[a-z_]{1,64}$/.test(value.code) ? value.code : undefined; } catch { return undefined; } }
function cancel(response: Response): void { void response.body?.cancel().catch(() => undefined); }
function canonicalOrigin(value: string): string { const url = new URL(value); const loopback = url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname); if ((!loopback && url.protocol !== "https:") || url.username || url.password || url.pathname !== "/" || url.search || url.hash) throw new EnrollmentError("join-config"); return url.origin; }
function object(value: unknown): Record<string, unknown> { if (typeof value !== "object" || value === null || Array.isArray(value)) throw new EnrollmentError("join-network"); return value as Record<string, unknown>; }
function string(value: unknown): string { if (typeof value !== "string" || value.length === 0) throw new EnrollmentError("join-network"); return value; }
function boundedExpiry(value: number): number { if (!Number.isSafeInteger(value)) throw new EnrollmentError("join-network"); return Math.max(0, value); }
function requireCreated(value: Created): void { string(value.join_request_id); string(value.poll_secret); boundedExpiry(value.expires_in_seconds); }
function isCancelled(error: unknown): boolean { return error instanceof EnrollmentError && error.code === "cancelled"; }
function decodeHeader(value: unknown): unknown { if (typeof value !== "string") throw new EnrollmentError("join-confirm"); try { const binary = atob(value); const bytes = Uint8Array.from(binary, character => character.charCodeAt(0)); return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)); } catch { throw new EnrollmentError("join-confirm"); } }
