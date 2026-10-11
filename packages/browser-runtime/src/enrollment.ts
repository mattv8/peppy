const MAX_JSON_BYTES = 64 * 1024;
const DEVICE_NAME = "Peppy Browser";

type DeploymentMode = "community" | "hosted";
type EnrollmentStage = "server" | "setup" | "signin" | "locked" | "ready" | "waiting_phone" | "verify_phone" | "phone_connected" | "expired" | "error";
type SessionPhase = "unenrolled" | "locked" | "ready" | "closed";

export type EnrollmentView = { mode: DeploymentMode; stage: EnrollmentStage; origin?: string; owner?: boolean; hasGateway?: boolean; qrPayload?: string; verificationCode?: string; expiresInSeconds?: number; errorCode?: string };
export type EnrollmentSecrets = { setupToken?: string; adminPassword?: string; passphrase?: string };
export type EnrollmentDevice = { device_id: string; name: string; kind: "browser" | "desktop" | "android" | "ios"; role: "owner" | "device" | "gateway"; revoked: boolean; gateway_online: boolean };
export type InstanceManagement = { vault_id: string; devices: EnrollmentDevice[]; storage_bytes: number; quota_bytes: number; replay_retention_days: number };
export type EnrollmentIdentity = { role: "owner" | "device" | "gateway" };
export interface EnrollmentCore { invoke(request: { command: string; args: Record<string, unknown> }): Promise<unknown>; }
export interface EnrollmentSession {
  readonly phase: SessionPhase;
  finishEnrollment(command: "_worker_enrollment_finish_owner" | "_worker_enrollment_finish_manual" | "_worker_enrollment_finish_hosted_owner", responseJson: string, passphrase: string): Promise<void>;
  unlock(passphrase: string): Promise<void>;
  publicIdentity(): EnrollmentIdentity | undefined;
  tokenForTransport(): string;
}

/** Worker-private enrollment transport. Rust owns every credential, proof, and QR key. */
export class BrowserEnrollment {
  private viewValue: EnrollmentView = { mode: "community", stage: "server" };
  private csrf?: string;
  private invitationId?: string;
  private enrollmentClient?: { origin: string; purpose: "fresh" | "owner" };
  private addPhonePending?: Promise<EnrollmentView>;
  private readonly fetchBase: string;

  /** `origin` is the durable API identity; browser fetches remain on the app-origin proxy. */
  public constructor(private readonly core: EnrollmentCore, private readonly session: EnrollmentSession, private readonly origin: string, private readonly fetcher: typeof fetch = fetch.bind(globalThis), fetchBase = origin) { this.fetchBase = fetchBase; }

  public view(): EnrollmentView { return { ...this.viewValue }; }

  public async discover(origin = this.origin): Promise<EnrollmentView> {
    try {
      const info = await this.discoverInfo();
      const mode = deploymentMode(info.mode);
      this.viewValue = { ...this.viewValue, mode };
      const signedIn = mode === "hosted" ? await this.hostedSession() : undefined;
      if (this.session.phase === "locked") {
        this.viewValue = { mode, stage: "locked", origin };
        return this.view();
      }
      if (this.session.phase === "ready") {
        const identity = this.requireIdentity();
        if (identity.role === "owner") await this.initializeExistingOwner(origin);
        this.viewValue = readyView(mode, origin, identity);
        return this.view();
      }
      if (this.session.phase !== "unenrolled") throw new Error("enrollment-state");
      await this.initializeFresh(origin);
      if (mode === "hosted") {
        if (!signedIn) this.viewValue = { mode, stage: "signin", origin };
        else {
          try {
            const account = await this.hostedAccount();
            this.viewValue = hostedStage(account, origin);
          } catch (error) {
            if (!(error instanceof HttpStatusError) || error.status !== 401) throw error;
            this.csrf = undefined;
            this.viewValue = { mode, stage: "signin", origin };
          }
        }
      } else this.viewValue = { mode, stage: info.claimed === true ? "signin" : "setup", origin };
    } catch (error) {
      this.fail(error);
    }
    return this.view();
  }

  public async generateSecret(): Promise<string> { return required(await this.core.invoke({ command: "_worker_enrollment_generate_secret", args: {} }), "passphrase"); }

  public async createOwner(secrets: EnrollmentSecrets): Promise<EnrollmentView> {
    try {
      if (!secrets.passphrase) throw new Error("enrollment-state");
      if (this.viewValue.mode === "hosted") return this.createHostedOwner(secrets.passphrase);
      if (!secrets.setupToken || !secrets.adminPassword) throw new Error("enrollment-state");
      const request = await this.rustRequest("_worker_enrollment_create_owner", secrets);
      const response = await this.request("/v1/instance/claim", "POST", request);
      this.captureClaimSession(response);
      await this.session.finishEnrollment("_worker_enrollment_finish_owner", JSON.stringify(response), secrets.passphrase);
      this.enrollmentClient = undefined;
      this.viewValue = readyView(this.viewValue.mode, this.origin, this.requireIdentity());
    } catch (error) {
      this.fail(error);
    }
    return this.view();
  }

  public async signIn(adminPassword: string): Promise<EnrollmentView> {
    try {
      if (this.viewValue.mode === "hosted") return this.view();
      const response = await this.request("/v1/instance/session", "POST", { admin_password: adminPassword, client: "browser" });
      this.captureSession(response);
      this.viewValue = { ...this.viewValue, stage: "locked" };
    } catch (error) {
      this.fail(error);
    }
    return this.view();
  }

  public async connectSharedSecret(passphrase: string): Promise<EnrollmentView> {
    try {
      if (this.session.phase === "locked") {
        await this.session.unlock(passphrase);
        this.viewValue = readyView(this.viewValue.mode, this.viewValue.origin ?? this.origin, this.requireIdentity());
        return this.view();
      }
      if (this.session.phase !== "unenrolled") throw new Error("enrollment-state");
      const challenge = await this.request("/v1/enrollment/challenges", "POST", await this.rustRequest("_worker_enrollment_manual_challenge", { vaultId: undefined }));
      const proof = await this.rustRequest("_worker_enrollment_complete_manual_challenge", { challengeJson: JSON.stringify(challenge), passphrase });
      const id = required(challenge, "enrollment_id");
      const complete = await this.request(`/v1/enrollment/challenges/${encodeURIComponent(id)}/complete`, "POST", proof);
      await this.session.finishEnrollment("_worker_enrollment_finish_manual", JSON.stringify(complete), passphrase);
      this.enrollmentClient = undefined;
      this.viewValue = readyView(this.viewValue.mode, this.viewValue.origin ?? this.origin, this.requireIdentity());
    } catch (error) {
      this.fail(error);
    }
    return this.view();
  }

  public addPhone(method: "transfer" | "shared_secret"): Promise<EnrollmentView> {
    if (this.addPhonePending) return this.addPhonePending;
    const pending = this.createPhoneInvitation(method);
    this.addPhonePending = pending;
    return pending.finally(() => { if (this.addPhonePending === pending) this.addPhonePending = undefined; });
  }

  private async createPhoneInvitation(method: "transfer" | "shared_secret"): Promise<EnrollmentView> {
    try {
      if (this.session.phase !== "ready" || this.requireIdentity().role !== "owner") throw new Error("enrollment-state");
      if (this.invitationId && (this.viewValue.stage === "waiting_phone" || this.viewValue.stage === "verify_phone")) return this.view();
      if (this.invitationId) this.resetOwnerInvitation();
      await this.initializeExistingOwner(this.viewValue.origin ?? this.origin);
      const created = await this.request("/v1/enrollment/invitations", "POST", await this.rustRequest("_worker_enrollment_create_phone_invitation", { method }), true);
      await this.core.invoke({ command: "_worker_enrollment_consume_created_phone_invitation", args: { createdJson: JSON.stringify(created) } });
      const qr = await this.core.invoke({ command: "_worker_enrollment_get_phone_qr", args: {} });
      this.invitationId = required(created, "invitation_id");
      this.viewValue = { ...this.viewValue, stage: "waiting_phone", qrPayload: required(qr, "qr"), expiresInSeconds: safeInteger(created.expires_in_seconds) };
    } catch (error) {
      this.resetOwnerInvitation();
      this.fail(error);
    }
    return this.view();
  }

  public async status(): Promise<EnrollmentView> {
    if (!this.invitationId) return this.view();
    try {
      const status = await this.invitationStatus();
      const expiresInSeconds = safeInteger(status.expires_in_seconds);
      if (status.state === "claimed") {
        const sas = await this.core.invoke({ command: "_worker_enrollment_phone_status", args: { statusJson: JSON.stringify(status) } });
        this.viewValue = { ...this.viewValue, stage: "verify_phone", verificationCode: required(sas, "sas"), expiresInSeconds };
      } else if (status.state === "consumed") {
        this.resetOwnerInvitation();
        this.viewValue = { ...this.viewValue, stage: "phone_connected", expiresInSeconds };
      } else if (status.state === "expired") {
        this.resetOwnerInvitation();
        this.viewValue = { ...this.viewValue, stage: "expired", expiresInSeconds };
      } else if (["waiting", "approved", "confirmed"].includes(String(status.state))) {
        this.viewValue = { ...this.viewValue, stage: "waiting_phone", expiresInSeconds };
      } else if (status.state === "denied") {
        throw new Error("enrollment-state");
      } else {
        throw new Error("enrollment-response");
      }
    } catch (error) {
      this.fail(error);
    }
    return this.view();
  }

  public async approve(passphrase: string): Promise<EnrollmentView> {
    try {
      if (!this.invitationId) throw new Error("enrollment-state");
      const status = await this.invitationStatus();
      if (status.state !== "claimed") throw new Error("enrollment-state");
      const request = await this.rustRequest("_worker_enrollment_approve_phone_invitation", { statusJson: JSON.stringify(status), passphrase });
      await this.request(`/v1/enrollment/invitations/${encodeURIComponent(this.invitationId)}/approve`, "POST", request, true);
      this.viewValue = { ...this.viewValue, stage: "waiting_phone", expiresInSeconds: safeInteger(status.expires_in_seconds) };
    } catch (error) {
      this.fail(error);
    }
    return this.view();
  }

  public async cancel(): Promise<void> {
    if (this.invitationId) await this.request(`/v1/enrollment/invitations/${encodeURIComponent(this.invitationId)}`, "DELETE", undefined, true);
    this.resetOwnerInvitation();
    this.viewValue = { ...this.viewValue, stage: "ready", qrPayload: undefined, verificationCode: undefined, expiresInSeconds: undefined };
  }

  public async signOut(): Promise<void> {
    await this.request(this.viewValue.mode === "hosted" ? "/hosted/v1/web/app-session" : "/v1/instance/session", "DELETE");
    this.csrf = undefined;
    this.viewValue = { ...this.viewValue, stage: "signin" };
  }

  public management(): Promise<InstanceManagement> { return this.managementRequest("/v1/instance/management", "GET"); }

  public revokeDevice(deviceId: string): Promise<InstanceManagement> {
    if (!isDeviceId(deviceId)) return Promise.reject({ code: "invalid-request" });
    return this.managementRequest(`/v1/instance/devices/${encodeURIComponent(deviceId)}/revoke`, "POST");
  }

  public setHostedCsrf(csrf: unknown): void {
    if (typeof csrf !== "string" || !csrf || csrf.length > 4096) throw new Error("enrollment-response");
    this.csrf = csrf;
  }

  private async createHostedOwner(passphrase: string): Promise<EnrollmentView> {
    const account = await this.hostedAccount();
    const accountId = required(account, "account_id");
    const prepared = await this.core.invoke({ command: "_worker_enrollment_prepare_hosted_owner", args: { accountId, passphrase, deviceName: DEVICE_NAME } });
    const grant = await this.request("/hosted/v1/web/provisioning", "POST", JSON.parse(required(prepared, "request")));
    const accepted = await this.core.invoke({ command: "_worker_enrollment_accept_hosted_grant", args: { grantJson: JSON.stringify(grant) } });
    const complete = await this.request("/hosted/v1/web/provisioning/complete", "POST", JSON.parse(required(accepted, "request")));
    await this.session.finishEnrollment("_worker_enrollment_finish_hosted_owner", JSON.stringify(complete), passphrase);
    this.viewValue = readyView("hosted", this.origin, this.requireIdentity());
    return this.view();
  }

  private async discoverInfo(): Promise<Record<string, unknown>> {
    try {
      return await this.request("/hosted/v1/enrollment/config", "GET");
    } catch (error) {
      if (!(error instanceof HttpStatusError) || error.status !== 404) throw error;
      return this.request("/v1/instance", "GET");
    }
  }

  private async hostedAccount(): Promise<Record<string, unknown>> {
    return this.request("/hosted/v1/web/app-account", "GET");
  }

  private async hostedSession(): Promise<boolean> {
    const session = await this.request("/hosted/v1/web/app-session", "GET");
    if (typeof session.signed_in !== "boolean") throw new Error("enrollment-response");
    if (!session.signed_in) {
      this.csrf = undefined;
      return false;
    }
    this.setHostedCsrf(session.csrf_token);
    return true;
  }

  private async initializeFresh(origin: string): Promise<void> {
    if (this.enrollmentClient?.origin === origin && this.enrollmentClient.purpose === "fresh") return;
    await this.core.invoke({ command: "_worker_enrollment_new", args: { origin, deviceName: DEVICE_NAME } });
    this.enrollmentClient = { origin, purpose: "fresh" };
  }

  private async initializeExistingOwner(origin: string): Promise<void> {
    if (this.enrollmentClient?.origin === origin && this.enrollmentClient.purpose === "owner") return;
    await this.core.invoke({ command: "_worker_enrollment_new_owner_from_identity", args: { deviceName: DEVICE_NAME } });
    this.enrollmentClient = { origin, purpose: "owner" };
  }

  private resetOwnerInvitation(): void {
    this.invitationId = undefined;
    if (this.enrollmentClient?.purpose === "owner") this.enrollmentClient = undefined;
  }

  private requireIdentity(): EnrollmentIdentity {
    const identity = this.session.publicIdentity();
    if (!identity || !["owner", "device", "gateway"].includes(identity.role)) throw new Error("enrollment-response");
    return identity;
  }

  private invitationStatus(): Promise<Record<string, unknown>> {
    if (!this.invitationId) return Promise.reject(new Error("enrollment-state"));
    return this.request(`/v1/enrollment/invitations/${encodeURIComponent(this.invitationId)}`, "GET", undefined, true);
  }

  private async rustRequest(command: string, args: Record<string, unknown>): Promise<unknown> {
    const result = await this.core.invoke({ command, args });
    return JSON.parse(required(result, "request")) as unknown;
  }

  private async managementRequest(path: string, method: "GET" | "POST"): Promise<InstanceManagement> {
    try {
      return management(await this.request(path, method));
    } catch (error) {
      if (error instanceof HttpStatusError && (error.status === 401 || error.status === 403)) throw { code: `http-${error.status}` };
      throw error;
    }
  }

  private async request<T = Record<string, unknown>>(path: string, method: "GET" | "POST" | "DELETE", body?: unknown, authenticated = false): Promise<T> {
    const token = authenticated ? this.session.tokenForTransport() : undefined;
    const csrfHeader = this.viewValue.mode === "hosted" ? "X-Peppy-CSRF" : "X-Peppy-CSRF-Token";
    const response = await this.fetcher(new URL(path, this.fetchBase), {
      method,
      credentials: "include",
      headers: {
        ...(body === undefined ? {} : { "Content-Type": "application/json" }),
        ...(token ? { Authorization: `Bearer ${token}` } : {}),
        ...(this.csrf && method !== "GET" ? { [csrfHeader]: this.csrf } : {}),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!response.ok) throw new HttpStatusError(response.status);
    if (response.status === 204) return {} as T;
    return JSON.parse(await readBoundedText(response)) as T;
  }

  private captureClaimSession(response: Record<string, unknown>): void {
    const session = record(response.session);
    this.captureSession(session);
  }

  private captureSession(response: Record<string, unknown>): void {
    const csrf = response.csrf_token;
    if (typeof csrf === "string" && csrf) this.csrf = csrf;
  }

  private fail(error: unknown): void {
    const errorCode = error instanceof HttpStatusError ? `http-${error.status}` : error instanceof Error ? error.message : "enrollment-error";
    this.viewValue = { ...this.viewValue, stage: "error", errorCode };
  }
}

class HttpStatusError extends Error {
  public constructor(public readonly status: number) { super(`http-${status}`); }
}

function readyView(mode: DeploymentMode, origin: string, identity: EnrollmentIdentity): EnrollmentView {
  return { mode, stage: "ready", origin, owner: identity.role === "owner" };
}
function hostedStage(account: Record<string, unknown>, origin: string): EnrollmentView {
  const classification = account.classification;
  const access = account.access;
  const hasVault = typeof account.vault_id === "string" && account.vault_id.length > 0;
  if (access === "readonly" && !hasVault) return { mode: "hosted", stage: "error", origin, errorCode: "billing-required" };
  if (!hasVault && (classification === "incomplete" || classification === "eligible")) return { mode: "hosted", stage: "setup", origin };
  return { mode: "hosted", stage: "locked", origin };
}

async function readBoundedText(response: Response): Promise<string> {
  const declaredLength = response.headers.get("content-length");
  if (declaredLength !== null && (!/^\d+$/.test(declaredLength) || Number(declaredLength) > MAX_JSON_BYTES)) throw new Error("enrollment-response");
  if (response.body === null) throw new Error("enrollment-response");
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      const next = await reader.read();
      if (next.done) break;
      total += next.value.byteLength;
      if (total > MAX_JSON_BYTES) throw new Error("enrollment-response");
      chunks.push(next.value);
    }
  } catch (error) {
    try { await reader.cancel(); } catch {}
    throw error;
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  try { return new TextDecoder("utf-8", { fatal: true }).decode(bytes); } catch { throw new Error("enrollment-response"); }
}

function deploymentMode(value: unknown): DeploymentMode {
  if (value !== "community" && value !== "hosted") throw new Error("enrollment-response");
  return value;
}
function record(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error("enrollment-response");
  return value as Record<string, unknown>;
}
function required(value: unknown, key: string): string {
  const output = record(value)[key];
  if (typeof output !== "string" || !output) throw new Error("enrollment-response");
  return output;
}
function safeInteger(value: unknown): number | undefined { return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : undefined; }
function isDeviceId(value: string): boolean { return /^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/.test(value); }
function management(value: Record<string, unknown>): InstanceManagement {
  const devices = value.devices;
  if (!isDeviceId(required(value, "vault_id")) || !Array.isArray(devices) || safeInteger(value.storage_bytes) === undefined || safeInteger(value.quota_bytes) === undefined || safeInteger(value.replay_retention_days) === undefined) throw new Error("enrollment-response");
  return { vault_id: required(value, "vault_id"), devices: devices.map(device), storage_bytes: value.storage_bytes as number, quota_bytes: value.quota_bytes as number, replay_retention_days: value.replay_retention_days as number };
}
function device(value: unknown): EnrollmentDevice {
  const entry = record(value);
  const kind = required(entry, "kind");
  const role = required(entry, "role");
  if (!isDeviceId(required(entry, "device_id")) || !["browser", "desktop", "android", "ios"].includes(kind) || !["owner", "device", "gateway"].includes(role) || typeof entry.name !== "string" || typeof entry.revoked !== "boolean" || typeof entry.gateway_online !== "boolean") throw new Error("enrollment-response");
  return { device_id: required(entry, "device_id"), name: entry.name, kind: kind as EnrollmentDevice["kind"], role: role as EnrollmentDevice["role"], revoked: entry.revoked, gateway_online: entry.gateway_online };
}
