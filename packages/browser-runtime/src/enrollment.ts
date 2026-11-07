import { ATTACHMENT_RETENTION_PROTOCOL_HEADER_NAME, ATTACHMENT_RETENTION_PROTOCOL_HEADER_VALUE } from "@peppy/generated/attachment-retention";
import { CheckpointError, RuntimeCoreError } from "./checkpoint.js";
import { BrowserCoreError, CoreRejectedError } from "./core.js";
import type { McpAuthorizationDecision, McpAuthorizationView, McpDeviceState, McpDeviceStatus, McpGrantView, McpScope, WebhookCreated, WebhookInput, WebhookList, WebhookSecret, WebhookTestResult, WebhookView } from "@peppy/generated/mcp";

const MAX_JSON_BYTES = 64 * 1024;
// Must exceed the server's 15 s long-poll (?wait=15) plus margin for the bounded body read.
const ENROLLMENT_REQUEST_TIMEOUT_MS = 25_000;
const DEVICE_NAME = "Peppy Browser";
const MAX_MCP_CLIENT_ID_LENGTH = 256;
const MAX_MCP_ORIGIN_LENGTH = 2048;

type DeploymentMode = "community" | "hosted";
type EnrollmentStage = "server" | "setup" | "signin" | "locked" | "ready" | "waiting_phone" | "verify_phone" | "phone_approved" | "phone_connected" | "computer_waiting" | "computer_approved" | "computer_finishing" | "computer_connected" | "computer_expired" | "computer_denied" | "computer_error" | "expired" | "error" | "invite_request" | "invite_waiting" | "invite_approved" | "invite_denied" | "invite_expired";
type SessionPhase = "unenrolled" | "locked" | "ready" | "closed";

export type EnrollmentView = { mode: DeploymentMode; stage: EnrollmentStage; origin?: string; owner?: boolean; hasGateway?: boolean; qrPayload?: string; verificationCode?: string; expiresInSeconds?: number; errorCode?: string; setupCodeRequired?: boolean; vaultOwnerRequired?: boolean; displayName?: string; username?: string; computerClaimant?: boolean; awaitingOwner?: boolean };
export type EnrollmentSecrets = { setupToken?: string; adminPassword?: string; passphrase?: string };
export type EnrollmentDevice = { device_id: string; name: string; kind: "browser" | "desktop" | "android" | "ios"; role: "owner" | "device" | "gateway"; revoked: boolean; gateway_online: boolean; presence?: { state: "connected" | "seen" | "never"; last_seen_age_seconds: number | null }; wake_route?: boolean };
export type InstanceManagement = { vault_id: string; devices: EnrollmentDevice[]; storage_bytes: number; quota_bytes: number; replay_retention_days: number; isAdmin: boolean; username: string };
export type VaultSummary = { vaultId: string; username: string; isAdmin: boolean; deviceCount: number; storageBytes: number; createdAt: string };
export type VaultInviteView = { inviteId: string; state: "active" | "used" | "revoked" | "expired"; expiresInSeconds: number; createdAt: string };
export type VaultRequestView = { requestId: string; displayName: string; client: "browser" | "desktop" | "android" | "ios"; state: "pending" | "approved" | "denied" | "completed" | "expired"; createdAt: string; expiresInSeconds: number };
export type EnrollmentIdentity = { role: "owner" | "device" | "gateway" };
type OwnerPhoneView = { invitationId: string; sas?: string; claimDigest?: string };
export interface EnrollmentCore { invoke(request: { command: string; args: Record<string, unknown> }): Promise<unknown>; }
export interface EnrollmentSession {
  readonly phase: SessionPhase;
  finishEnrollment(command: "_worker_enrollment_finish_owner" | "_worker_enrollment_finish_manual" | "_worker_enrollment_finish_hosted_owner", responseJson: string, passphrase: string): Promise<void>;
  finishComputerEnrollment?(responseJson: string): Promise<void>;
  unlock(passphrase: string): Promise<void>;
  publicIdentity(): EnrollmentIdentity | undefined;
  tokenForTransport(): string;
}

/** Worker-private enrollment transport. Rust owns every credential, proof, and QR key. */
export class BrowserEnrollment {
  private viewValue: EnrollmentView = { mode: "community", stage: "server" };
  private csrf?: string;
  private invitationId?: string;
  private approvedInvitationId?: string;
  private ownerClaimDigest?: string;
  private invitationGeneration = 0;
  private enrollmentClient?: { origin: string; purpose: "fresh" | "owner" };
  private addPhonePending?: Promise<EnrollmentView>;
  private vaultRequestGeneration = 0;
  private vaultRequestStatusPending?: Promise<EnrollmentView>;
  private computerClaimPending?: Promise<EnrollmentView>;
  private mcpEnablePending?: { generation: number; result: Promise<McpDeviceStatus> };
  private mcpAttemptGeneration = 0;
  private signOutsInFlight = 0;
  private computerClaimPreviousView?: EnrollmentView;
  private computerComplete?: string;
  private computerConfirmEndpoint?: string;
  private setupCodeRequired = true;
  private readonly fetchBase: string;

  /** `origin` is the durable API identity; browser fetches remain on the app-origin proxy. */
  public constructor(private readonly core: EnrollmentCore, private readonly session: EnrollmentSession, private readonly origin: string, private readonly fetcher: typeof fetch = fetch.bind(globalThis), fetchBase = origin, private readonly signal?: AbortSignal) { this.fetchBase = fetchBase; }

  public view(): EnrollmentView { return { ...this.viewValue }; }

  public async discover(origin = this.origin): Promise<EnrollmentView> {
    const restoreGeneration = this.mcpAttemptGeneration;
    try {
      this.setupCodeRequired = true;
      const info = await this.discoverInfo();
      const mode = deploymentMode(info.mode);
      if (mode === "community" && typeof info.claimed !== "boolean") throw new Error("enrollment-response");
      this.viewValue = { ...this.viewValue, mode };
      const signedIn = mode === "hosted" ? await this.hostedSession(restoreGeneration) : undefined;
      if (mode === "community" && info.claimed === true) await this.restoreCommunitySession(restoreGeneration);
      if (this.session.phase === "locked") {
        this.viewValue = { mode, stage: "locked", origin };
        return this.view();
      }
      if (this.session.phase === "ready") {
        const identity = this.requireIdentity();
        if (identity.role === "owner") await this.initializeExistingOwner(origin);
        this.viewValue = await this.readyView(mode, origin, identity);
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
      } else {
        this.setupCodeRequired = info.setup_required !== false;
        const pending = await this.pendingVaultRequest();
        if (pending?.origin === origin) this.viewValue = { mode, stage: "invite_waiting", origin, displayName: pending.displayName };
        else this.viewValue = info.claimed === true
          ? { mode, stage: "signin", origin }
          : { mode, stage: "setup", origin, setupCodeRequired: this.setupCodeRequired };
      }
    } catch (error) {
      if (restoreGeneration !== this.mcpAttemptGeneration) return this.view();
      this.fail(error);
    }
    return this.view();
  }

  public async generateSecret(): Promise<string> { return required(await this.core.invoke({ command: "_worker_enrollment_generate_secret", args: {} }), "passphrase"); }

  public async mcpStatus(): Promise<McpDeviceStatus> {
    return mcpDeviceStatus(await this.request("/v1/mcp/device", "GET", undefined, true));
  }

  public async mcpEnable(): Promise<McpDeviceStatus> {
    const generation = this.mcpAttemptGeneration;
    if (this.mcpEnablePending?.generation === generation) return this.mcpEnablePending.result;
    const pending = this.enableMcp(generation);
    this.mcpEnablePending = { generation, result: pending };
    return pending.finally(() => {
      if (this.mcpEnablePending?.result === pending) this.mcpEnablePending = undefined;
    });
  }

  private async enableMcp(generation: number): Promise<McpDeviceStatus> {
    this.assertMcpAttempt(generation);
    await this.core.invoke({ command: "_worker_mcp_owner_begin", args: { deviceName: DEVICE_NAME } });
    this.assertMcpAttempt(generation);
    await this.restoreCustodySession(generation);
    this.assertMcpAttempt(generation);
    const vaultId = mcpId(required(record(await this.core.invoke({ command: "_worker_identity_metadata", args: {} })), "vaultId"));
    this.assertMcpAttempt(generation);
    if ((await this.mcpStatus()).vaultId !== vaultId) throw new Error("mcp-response");
    this.assertMcpAttempt(generation);
    const preparation = record(await this.request("/v1/mcp/device/prepare", "POST", undefined, true));
    this.assertMcpAttempt(generation);
    const operationId = required(preparation, "operation_id");
    const pairing = record(preparation.pairing);
    const invitationId = required(pairing, "invitation_id");
    const create = record(await this.core.invoke({ command: "_worker_mcp_owner_create", args: { pairingJson: JSON.stringify(pairing) } }));
    this.assertMcpAttempt(generation);
    const created = await this.request("/v1/enrollment/computers", "POST", JSON.parse(required(create, "request")), true);
    this.assertMcpAttempt(generation);
    await this.core.invoke({ command: "_worker_mcp_owner_created", args: { createdJson: JSON.stringify(created) } });
    this.assertMcpAttempt(generation);
    const approval = record(await this.core.invoke({ command: "_worker_mcp_owner_approve", args: {} }));
    this.assertMcpAttempt(generation);
    const status = await this.request(`/v1/enrollment/computers/${encodeURIComponent(invitationId)}/approve`, "POST", JSON.parse(required(approval, "request")), true);
    this.assertMcpAttempt(generation);
    const activation = record(await this.request("/v1/mcp/device/activate", "POST", { operation_id: operationId, status }, true));
    this.assertMcpAttempt(generation);
    const confirmation = record(activation.confirm);
    const admission = await this.request(`/v1/enrollment/computers/${encodeURIComponent(invitationId)}/confirm`, "POST", confirmation, true);
    this.assertMcpAttempt(generation);
    const result = mcpDeviceStatus(await this.request("/v1/mcp/device/complete", "POST", { operation_id: operationId, admission }, true));
    this.assertMcpAttempt(generation);
    if (result.vaultId !== vaultId) throw new Error("mcp-response");
    return result;
  }

  public async mcpGrants(): Promise<McpGrantView[]> {
    const value = await this.request("/v1/mcp/grants", "GET", undefined, true);
    if (!Array.isArray(value)) throw new Error("mcp-response");
    return value.map(mcpGrant);
  }

  public async mcpRevokeGrant(id: string): Promise<void> {
    await this.request(`/v1/mcp/grants/${encodeURIComponent(mcpId(id))}/revoke`, "POST", undefined, true);
  }

  public async webhooksList(): Promise<WebhookList> {
    return webhookList(await this.request("/v1/mcp/webhooks", "GET", undefined, true));
  }

  public async webhookCreate(input: WebhookInput): Promise<WebhookCreated> {
    return webhookCreated(await this.request("/v1/mcp/webhooks", "POST", input, true));
  }

  public async webhookUpdate(id: string, input: WebhookInput): Promise<WebhookView> {
    return webhookView(await this.request(`/v1/mcp/webhooks/${encodeURIComponent(mcpId(id))}`, "PUT", input, true));
  }

  public async webhookDelete(id: string): Promise<void> {
    await this.request(`/v1/mcp/webhooks/${encodeURIComponent(mcpId(id))}`, "DELETE", undefined, true);
  }

  public async webhookRevealSecret(id: string): Promise<WebhookSecret> {
    return webhookSecret(await this.request(`/v1/mcp/webhooks/${encodeURIComponent(mcpId(id))}/secret`, "POST", undefined, true));
  }

  public async webhookRotateSecret(id: string): Promise<WebhookSecret> {
    return webhookSecret(await this.request(`/v1/mcp/webhooks/${encodeURIComponent(mcpId(id))}/rotate-secret`, "POST", undefined, true));
  }

  public async webhookTest(id: string): Promise<WebhookTestResult> {
    return webhookTestResult(await this.request(`/v1/mcp/webhooks/${encodeURIComponent(mcpId(id))}/test`, "POST", undefined, true));
  }

  public async mcpAuthorization(id: string): Promise<McpAuthorizationView> {
    return mcpAuthorization(await this.request(`/v1/mcp/authorizations/${encodeURIComponent(mcpAuthorizationId(id))}`, "GET", undefined, true));
  }

  public async mcpApproveAuthorization(id: string, scopes: McpScope[]): Promise<McpAuthorizationDecision> {
    return mcpDecision(await this.request(`/v1/mcp/authorizations/${encodeURIComponent(mcpAuthorizationId(id))}/approve`, "POST", { scopes: mcpScopes(scopes) }, true));
  }

  public async mcpDenyAuthorization(id: string): Promise<McpAuthorizationDecision> {
    return mcpDecision(await this.request(`/v1/mcp/authorizations/${encodeURIComponent(mcpAuthorizationId(id))}/deny`, "POST", undefined, true));
  }

  public async createOwner(secrets: EnrollmentSecrets, username?: string): Promise<EnrollmentView> {
    try {
      if (!secrets.passphrase) throw new Error("enrollment-state");
      if (this.viewValue.mode === "hosted") return this.createHostedOwner(secrets.passphrase);
      if (!secrets.adminPassword || (this.setupCodeRequired && !secrets.setupToken)) throw new Error("enrollment-state");
      const request = await this.rustRequest("_worker_enrollment_create_owner", { ...secrets, username: username ?? "" });
      const response = await this.request("/v1/instance/claim", "POST", request);
      this.captureClaimSession(response);
      await this.session.finishEnrollment("_worker_enrollment_finish_owner", JSON.stringify(response), secrets.passphrase);
      this.enrollmentClient = undefined;
      this.viewValue = await this.readyView(this.viewValue.mode, this.origin, this.requireIdentity());
    } catch (error) {
      this.fail(error);
    }
    return this.view();
  }

  public async signIn(adminPassword: string, username?: string): Promise<EnrollmentView> {
    try {
      if (this.viewValue.mode === "hosted") return this.view();
      const response = await this.request("/v1/instance/session", "POST", { username: username ?? "", admin_password: adminPassword, client: "browser" });
      this.captureSession(response);
      this.viewValue = { ...this.viewValue, stage: "locked" };
    } catch (error) {
      this.fail(error);
    }
    return this.view();
  }

  public async connectSharedSecret(passphrase: string, vaultOwner?: string): Promise<EnrollmentView> {
    try {
      if (this.session.phase === "locked") {
        await this.session.unlock(passphrase);
        this.viewValue = await this.readyView(this.viewValue.mode, this.viewValue.origin ?? this.origin, this.requireIdentity());
        return this.view();
      }
      if (this.session.phase !== "unenrolled") throw new Error("enrollment-state");
      const challenge = await this.request("/v1/enrollment/challenges", "POST", await this.rustRequest("_worker_enrollment_manual_challenge", { vaultId: undefined, vaultOwner }));
      const proof = await this.rustRequest("_worker_enrollment_complete_manual_challenge", { challengeJson: JSON.stringify(challenge), passphrase });
      const id = required(challenge, "enrollment_id");
      const complete = await this.request(`/v1/enrollment/challenges/${encodeURIComponent(id)}/complete`, "POST", proof);
      await this.session.finishEnrollment("_worker_enrollment_finish_manual", JSON.stringify(complete), passphrase);
      this.enrollmentClient = undefined;
      this.viewValue = await this.readyView(this.viewValue.mode, this.viewValue.origin ?? this.origin, this.requireIdentity());
    } catch (error) {
      if (error instanceof HttpStatusError && error.code === "vault_owner_required") {
        this.viewValue = { ...this.viewValue, stage: "locked", vaultOwnerRequired: true };
        return this.view();
      }
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

  public connectComputer(): Promise<EnrollmentView> {
    if (this.computerClaimPending) return this.computerClaimPending;
    const pending = this.beginComputerClaimant();
    this.computerClaimPending = pending;
    return pending.finally(() => { if (this.computerClaimPending === pending) this.computerClaimPending = undefined; });
  }

  private async beginComputerClaimant(): Promise<EnrollmentView> {
    try {
      if (this.session.phase !== "unenrolled") throw new Error("enrollment-state");
      if (this.canRestartComputerClaimant()) {
        await this.cancelComputerClaimant();
      } else if (this.viewValue.computerClaimant) {
        throw new Error("enrollment-state");
      }
      await this.initializeFresh(this.viewValue.origin ?? this.origin);
      this.computerClaimPreviousView = { ...this.viewValue };
      this.computerComplete = undefined;
      this.computerConfirmEndpoint = undefined;
      const qr = await this.core.invoke({ command: "_worker_enrollment_begin_computer_claimant", args: {} });
      this.viewValue = { ...this.viewValue, stage: "computer_waiting", qrPayload: required(qr, "qr"), verificationCode: undefined, computerClaimant: true, awaitingOwner: true };
    } catch (error) {
      this.fail(error);
    }
    return this.view();
  }

  public async computerStatus(): Promise<EnrollmentView> {
    try {
      if (!this.viewValue.computerClaimant) throw new Error("enrollment-state");
      const request = privateRequest(await this.core.invoke({ command: "_worker_enrollment_computer_status_request", args: {} }));
      let status: Record<string, unknown>;
      try {
        status = record(await this.privateEnrollmentRequest(request));
      } catch (error) {
        if (!hasHttpStatus(error, 404)) throw error;
        const waiting = record(await this.core.invoke({ command: "_worker_enrollment_computer_awaiting_owner_view", args: {} }));
        this.viewValue = this.computerView(waiting, "computer_waiting");
        return this.view();
      }
      const claimant = record(await this.core.invoke({ command: "_worker_enrollment_computer_claimant_view", args: { statusJson: JSON.stringify(status) } }));
      const state = required(claimant, "state");
      if (state === "approved") this.viewValue = this.computerView(claimant, "computer_approved");
      else if (state === "waiting") this.viewValue = this.computerView(claimant, "computer_waiting");
      else if (state === "expired") this.viewValue = this.computerView(claimant, "computer_expired");
      else if (state === "denied") this.viewValue = this.computerView(claimant, "computer_denied");
      else if (state === "consumed") this.setComputerError("computer_remote_consumed");
      else throw new Error("enrollment-response");
    } catch (error) {
      if (this.viewValue.stage === "computer_waiting" && isTransientComputerFailure(error)) {
        this.viewValue = { ...this.viewValue, errorCode: "computer_transient" };
        return this.view();
      }
      if (this.viewValue.stage !== "computer_finishing") this.setComputerError(computerFailureCode(error));
    }
    return this.view();
  }

  public async confirmComputer(): Promise<EnrollmentView> {
    try {
      if (this.viewValue.stage !== "computer_approved" && this.viewValue.stage !== "computer_finishing") throw new Error("enrollment-state");
      if (!this.computerComplete) {
        const { endpoint, confirmation } = this.viewValue.stage === "computer_approved"
          ? await this.prepareComputerConfirmation()
          : await this.retryComputerConfirmation();
        this.viewValue = { ...this.viewValue, stage: "computer_finishing", qrPayload: undefined, awaitingOwner: false };
        this.computerConfirmEndpoint = endpoint;
        this.computerComplete = JSON.stringify(await this.request(endpoint, "POST", confirmation));
      }
      if (!this.session.finishComputerEnrollment) throw new Error("enrollment-state");
      await this.session.finishComputerEnrollment(this.computerComplete);
      this.enrollmentClient = undefined;
      this.computerComplete = undefined;
      this.computerConfirmEndpoint = undefined;
      this.computerClaimPreviousView = undefined;
      this.viewValue = { ...(await this.readyView(this.viewValue.mode, this.viewValue.origin ?? this.origin, this.requireIdentity())), computerClaimant: true, awaitingOwner: false, stage: "computer_connected" };
    } catch (error) {
      if (isFatalComputerActivationFailure(error)) this.setComputerError("computer_reopen_required");
      else if (this.viewValue.stage === "computer_finishing") this.viewValue = { ...this.viewValue, errorCode: "computer_transient" };
      else if (this.viewValue.stage === "computer_approved" && isTransientComputerFailure(error)) this.viewValue = { ...this.viewValue, errorCode: "computer_transient" };
      else this.setComputerError(computerFailureCode(error));
    }
    return this.view();
  }

  public async cancelComputer(): Promise<EnrollmentView> {
    if (!this.viewValue.computerClaimant || !["computer_waiting", "computer_expired", "computer_denied", "computer_error"].includes(this.viewValue.stage)) return this.view();
    await this.cancelComputerClaimant();
    return this.view();
  }

  private async cancelComputerClaimant(): Promise<void> {
    await this.core.invoke({ command: "_worker_enrollment_cancel_computer_claimant", args: {} });
    this.enrollmentClient = undefined;
    this.computerComplete = undefined;
    this.computerConfirmEndpoint = undefined;
    this.viewValue = this.computerClaimPreviousView ?? { mode: this.viewValue.mode, stage: "locked", origin: this.viewValue.origin ?? this.origin };
    this.computerClaimPreviousView = undefined;
  }

  private async prepareComputerConfirmation(): Promise<{ endpoint: string; confirmation: unknown }> {
    const statusRequest = privateRequest(await this.core.invoke({ command: "_worker_enrollment_computer_status_request", args: {} }));
    const status = await this.privateEnrollmentRequest(statusRequest);
    return {
      endpoint: computerEndpoint(statusRequest, "confirm"),
      confirmation: await this.rustRequest("_worker_enrollment_confirm_computer", { statusJson: JSON.stringify(status) }),
    };
  }

  private async retryComputerConfirmation(): Promise<{ endpoint: string; confirmation: unknown }> {
    if (!this.computerConfirmEndpoint) throw new Error("enrollment-state");
    return {
      endpoint: this.computerConfirmEndpoint,
      confirmation: await this.rustRequest("_worker_enrollment_confirm_computer", { statusJson: "{}" }),
    };
  }

  private canRestartComputerClaimant(): boolean {
    return ["computer_expired", "computer_denied"].includes(this.viewValue.stage)
      || (this.viewValue.stage === "computer_error" && !this.computerConfirmEndpoint);
  }

  private setComputerError(errorCode: string): void {
    this.viewValue = {
      ...this.viewValue,
      stage: "computer_error",
      qrPayload: undefined,
      verificationCode: undefined,
      expiresInSeconds: undefined,
      errorCode,
      computerClaimant: true,
      awaitingOwner: false,
    };
  }

  private async createPhoneInvitation(method: "transfer" | "shared_secret"): Promise<EnrollmentView> {
    let invitationGeneration = this.invitationGeneration;
    try {
      if (this.session.phase !== "ready" || this.requireIdentity().role !== "owner") throw new Error("enrollment-state");
      if (this.invitationId && (this.viewValue.stage === "waiting_phone" || this.viewValue.stage === "verify_phone" || this.isInvitationApproved(this.invitationId))) return this.view();
      if (this.invitationId) this.resetOwnerInvitation();
      invitationGeneration = this.invitationGeneration;
      await this.initializeExistingOwner(this.viewValue.origin ?? this.origin);
      const ownerClient = this.enrollmentClient;
      if (!this.isCurrentInvitationCreation(invitationGeneration)) return this.discardOwnerInvitationClient(ownerClient);
      const request = await this.rustRequest("_worker_enrollment_create_phone_invitation", { method });
      if (!this.isCurrentInvitationCreation(invitationGeneration)) return this.discardOwnerInvitationClient(ownerClient);
      const created = await this.request("/v1/enrollment/invitations", "POST", request, true);
      if (!this.isCurrentInvitationCreation(invitationGeneration)) return this.discardOwnerInvitationClient(ownerClient);
      await this.core.invoke({ command: "_worker_enrollment_consume_created_phone_invitation", args: { createdJson: JSON.stringify(created) } });
      if (!this.isCurrentInvitationCreation(invitationGeneration)) return this.discardOwnerInvitationClient(ownerClient);
      const qr = await this.core.invoke({ command: "_worker_enrollment_get_phone_qr", args: {} });
      if (!this.isCurrentInvitationCreation(invitationGeneration)) return this.discardOwnerInvitationClient(ownerClient);
      this.invitationId = required(created, "invitation_id");
      const { errorCode: _errorCode, ...view } = this.viewValue;
      this.viewValue = { ...view, stage: "waiting_phone", qrPayload: required(qr, "qr"), expiresInSeconds: safeInteger(created.expires_in_seconds) };
    } catch (error) {
      if (this.invitationGeneration !== invitationGeneration) return this.discardOwnerInvitationClient();
      this.resetOwnerInvitation();
      this.fail(error);
    }
    return this.view();
  }

  public async status(): Promise<EnrollmentView> {
    const invitationId = this.invitationId;
    const invitationGeneration = this.invitationGeneration;
    if (!invitationId) {
      if (this.viewValue.stage === "ready") await this.refreshGatewayPresence();
      return this.view();
    }
    try {
      const status = await this.invitationStatus(invitationId);
      if (!this.isActiveOwnerInvitation(invitationId, invitationGeneration)) return this.view();
      const expiresInSeconds = safeInteger(status.expires_in_seconds);
      if (status.state === "claimed") {
        if (this.isInvitationApproved(invitationId)) {
          this.viewValue = { ...this.viewValue, expiresInSeconds };
          return this.view();
        }
        const ownerView = await this.ownerPhoneView(status);
        if (!this.isActiveOwnerInvitation(invitationId, invitationGeneration)) return this.view();
        const verificationCode = this.verifyOwnerClaim(invitationId, ownerView);
        this.pinOwnerClaim(ownerView.claimDigest!);
        this.viewValue = { ...this.viewValue, stage: "verify_phone", verificationCode, expiresInSeconds };
      } else if (status.state === "confirmed" || status.state === "consumed") {
        this.resetOwnerInvitation();
        this.viewValue = { ...this.viewValue, stage: "phone_connected", expiresInSeconds };
      } else if (status.state === "expired") {
        if (this.isInvitationApproved(invitationId)) this.viewValue = { ...this.viewValue, stage: "phone_approved", qrPayload: undefined, verificationCode: undefined, expiresInSeconds };
        else {
          this.resetOwnerInvitation();
          this.viewValue = { ...this.viewValue, stage: "expired", expiresInSeconds };
        }
      } else if (status.state === "approved") {
        this.approvedInvitationId = invitationId;
        this.viewValue = { ...this.viewValue, stage: "phone_approved", qrPayload: undefined, verificationCode: undefined, expiresInSeconds };
      } else if (status.state === "waiting") {
        this.viewValue = this.isInvitationApproved(invitationId)
          ? { ...this.viewValue, stage: "phone_approved", qrPayload: undefined, verificationCode: undefined, expiresInSeconds }
          : { ...this.viewValue, stage: "waiting_phone", expiresInSeconds };
      } else if (status.state === "denied") {
        throw new Error("enrollment-state");
      } else {
        throw new Error("enrollment-response");
      }
    } catch (error) {
      if (!this.isActiveOwnerInvitation(invitationId, invitationGeneration)) return this.view();
      if (this.isInvitationApproved(invitationId)) {
        this.viewValue = { ...this.viewValue, stage: "phone_approved", qrPayload: undefined, verificationCode: undefined };
        return this.view();
      }
      this.fail(error);
    }
    return this.view();
  }

  public async approve(): Promise<EnrollmentView> {
    const invitationId = this.invitationId;
    const invitationGeneration = this.invitationGeneration;
    try {
      if (!invitationId) throw new Error("enrollment-state");
      const status = await this.invitationStatus(invitationId);
      if (!this.isActiveOwnerInvitation(invitationId, invitationGeneration)) return this.view();
      if (status.state !== "claimed") throw new Error("enrollment-state");
      if (this.viewValue.stage !== "verify_phone" || !this.ownerClaimDigest) throw new Error("enrollment-state");
      this.verifyPinnedOwnerClaim(invitationId, await this.ownerPhoneView(status));
      if (!this.isActiveOwnerInvitation(invitationId, invitationGeneration)) return this.view();
      const request = await this.rustRequest("_worker_enrollment_approve_phone_invitation", { statusJson: JSON.stringify(status) });
      if (!this.isActiveOwnerInvitation(invitationId, invitationGeneration)) return this.view();
      await this.request(`/v1/enrollment/invitations/${encodeURIComponent(invitationId)}/approve`, "POST", request, true);
      if (!this.isActiveOwnerInvitation(invitationId, invitationGeneration)) return this.view();
      this.approvedInvitationId = invitationId;
      this.viewValue = { ...this.viewValue, stage: "phone_approved", qrPayload: undefined, verificationCode: undefined, expiresInSeconds: safeInteger(status.expires_in_seconds) };
    } catch (error) {
      if (!invitationId || this.isActiveOwnerInvitation(invitationId, invitationGeneration)) this.fail(error);
    }
    return this.view();
  }

  public async cancel(options: { waitingOnly?: boolean } = {}): Promise<EnrollmentView | undefined> {
    const invitationId = this.invitationId;
    this.invitationGeneration += 1;
    this.vaultRequestGeneration += 1;
    if (await this.pendingVaultRequest()) {
      await this.core.invoke({ command: "_worker_enrollment_clear_vault_request", args: {} });
      this.viewValue = { mode: this.viewValue.mode, stage: "signin", origin: this.origin };
      return options.waitingOnly ? this.view() : undefined;
    }
    if (!invitationId) {
      if (this.viewValue.stage === "expired") this.viewValue = { ...this.viewValue, stage: "ready", qrPayload: undefined, verificationCode: undefined, expiresInSeconds: undefined };
      return options.waitingOnly ? this.view() : undefined;
    }
    const invitationGeneration = this.invitationGeneration;
    const status = await this.invitationStatus(invitationId);
    if (!this.isActiveOwnerInvitation(invitationId, invitationGeneration)) return options.waitingOnly ? this.view() : undefined;
    if (["approved", "confirmed", "consumed"].includes(String(status.state))) {
      if (status.state === "approved") {
        this.approvedInvitationId = invitationId;
        this.viewValue = { ...this.viewValue, stage: "phone_approved", qrPayload: undefined, verificationCode: undefined, expiresInSeconds: safeInteger(status.expires_in_seconds) };
      } else {
        this.resetOwnerInvitation();
        this.viewValue = { ...this.viewValue, stage: "phone_connected", expiresInSeconds: safeInteger(status.expires_in_seconds) };
      }
      if (options.waitingOnly) return this.view();
      throw new Error("enrollment-state");
    }
    if (!["waiting", "expired", "claimed"].includes(String(status.state))) throw new Error("enrollment-response");
    if (options.waitingOnly && status.state === "claimed") {
      const ownerView = await this.ownerPhoneView(status);
      if (!this.isActiveOwnerInvitation(invitationId, invitationGeneration)) return this.view();
      const verificationCode = this.verifyOwnerClaim(invitationId, ownerView);
      this.pinOwnerClaim(ownerView.claimDigest!);
      this.viewValue = { ...this.viewValue, stage: "verify_phone", verificationCode, expiresInSeconds: safeInteger(status.expires_in_seconds) };
      return this.view();
    }
    await this.request(`/v1/enrollment/invitations/${encodeURIComponent(invitationId)}`, "DELETE", undefined, true);
    if (!this.isActiveOwnerInvitation(invitationId, invitationGeneration)) return options.waitingOnly ? this.view() : undefined;
    this.resetOwnerInvitation();
    this.viewValue = { ...this.viewValue, stage: "ready", qrPayload: undefined, verificationCode: undefined, expiresInSeconds: undefined };
    return options.waitingOnly ? this.view() : undefined;
  }

  public async signOut(): Promise<void> {
    this.invalidateMcpAttempts();
    this.signOutsInFlight += 1;
    try {
      await this.request(this.viewValue.mode === "hosted" ? "/hosted/v1/web/app-session" : "/v1/instance/session", "DELETE");
      this.csrf = undefined;
      this.viewValue = { ...this.viewValue, stage: "signin" };
    } finally { this.signOutsInFlight -= 1; }
  }

  public management(): Promise<InstanceManagement> { return this.managementRequest("/v1/instance/management", "GET"); }

  public revokeDevice(deviceId: string): Promise<InstanceManagement> {
    if (!isDeviceId(deviceId)) return Promise.reject({ code: "invalid-request" });
    return this.managementRequest(`/v1/instance/devices/${encodeURIComponent(deviceId)}/revoke`, "POST");
  }

  public async beginVaultRequest(inviteLink: string, displayName: string): Promise<EnrollmentView> {
    try {
      this.vaultRequestGeneration += 1;
      const invite = record(await this.core.invoke({ command: "_worker_enrollment_parse_vault_invite_link", args: { link: inviteLink } }));
      const inviteOrigin = required(invite, "origin");
      const request = await this.rustRequest("_worker_enrollment_begin_vault_request", { origin: inviteOrigin, inviteToken: required(invite, "inviteToken"), displayName });
      const created = await this.request("/v1/instance/vault-requests", "POST", request);
      await this.core.invoke({ command: "_worker_enrollment_accept_vault_request_created", args: { json: JSON.stringify(created) } });
      this.viewValue = { mode: "community", stage: "invite_waiting", origin: inviteOrigin, displayName, expiresInSeconds: safeInteger(record(created).expires_in_seconds) };
    } catch (error) {
      if (isDefinitiveVaultRequestRejection(error)) await this.core.invoke({ command: "_worker_enrollment_clear_vault_request", args: {} });
      this.fail(error);
    }
    return this.view();
  }

  public async vaultRequestStatus(): Promise<EnrollmentView> {
    if (this.vaultRequestStatusPending) return this.vaultRequestStatusPending;
    const generation = this.vaultRequestGeneration;
    const poll = this.pollVaultRequestStatus(generation);
    this.vaultRequestStatusPending = poll;
    try {
      return await poll;
    } finally {
      if (this.vaultRequestStatusPending === poll) this.vaultRequestStatusPending = undefined;
    }
  }

  private async pollVaultRequestStatus(generation: number): Promise<EnrollmentView> {
    try {
      const pending = record(await this.core.invoke({ command: "_worker_enrollment_pending_vault_request", args: {} }));
      const requestId = required(pending, "requestId");
      const header = required(await this.core.invoke({ command: "_worker_enrollment_vault_request_header", args: {} }), "header");
      const status = await this.request(`/v1/instance/vault-requests/${encodeURIComponent(requestId)}?wait=15`, "GET", undefined, false, { "Peppy-Vault-Request": header, "Peppy-Known-State": this.viewValue.stage === "invite_approved" ? "approved" : "pending" });
      const parsed = required(await this.core.invoke({ command: "_worker_enrollment_parse_vault_request_status", args: { json: JSON.stringify(status) } }), "state");
      const expiresInSeconds = safeInteger(record(status).expires_in_seconds);
      const stage = ({ pending: "invite_waiting", approved: "invite_approved", denied: "invite_denied", expired: "invite_expired", completed: "invite_denied" } as const)[parsed as "pending" | "approved" | "denied" | "expired" | "completed"];
      if (!stage) throw new Error("enrollment-response");
      if (generation !== this.vaultRequestGeneration) return this.view();
      if (parsed === "completed") {
        this.viewValue = { ...this.viewValue, stage: "error", errorCode: "vault_request_completed", expiresInSeconds };
        return this.view();
      }
      this.viewValue = { ...this.viewValue, stage, expiresInSeconds };
      if (parsed === "denied" || parsed === "expired") await this.core.invoke({ command: "_worker_enrollment_clear_vault_request", args: {} });
    } catch (error) {
      if (generation !== this.vaultRequestGeneration) return this.view();
      if (isTransientVaultRequestFailure(error)) throw error;
      this.fail(error);
    }
    return this.view();
  }

  public async createVault(username: string, secrets: Pick<EnrollmentSecrets, "adminPassword" | "passphrase">): Promise<EnrollmentView> {
    try {
      if (!secrets.adminPassword || !secrets.passphrase) throw new Error("enrollment-state");
      const request = await this.rustRequest("_worker_enrollment_create_vault", { username, adminPassword: secrets.adminPassword, passphrase: secrets.passphrase });
      const response = await this.request("/v1/instance/vaults", "POST", request);
      this.captureClaimSession(response);
      await this.session.finishEnrollment("_worker_enrollment_finish_owner", JSON.stringify(response), secrets.passphrase);
      this.enrollmentClient = undefined;
      this.viewValue = await this.readyView("community", this.origin, this.requireIdentity());
    } catch (error) {
      if (error instanceof HttpStatusError && (error.code === "username_taken" || error.code === "invalid_username")) {
        this.viewValue = { ...this.viewValue, stage: "invite_approved", errorCode: error.code };
      } else {
        this.fail(error);
      }
    }
    return this.view();
  }

  public async pendingVaultRequest(): Promise<{ origin: string; requestId: string; displayName: string } | null> {
    const pending = await this.core.invoke({ command: "_worker_enrollment_pending_vault_request", args: {} });
    if (pending === null) return null;
    const value = record(pending);
    return { origin: required(value, "origin"), requestId: required(value, "requestId"), displayName: required(value, "displayName") };
  }

  public async listVaults(): Promise<VaultSummary[]> { return array(await this.request("/v1/instance/vaults", "GET")).map(vaultSummary); }
  public async createInvite(): Promise<{ inviteId: string; inviteLink: string; expiresInSeconds: number }> {
    const value = record(await this.request("/v1/instance/invites", "POST"));
    const inviteToken = required(value, "invite_token");
    return { inviteId: required(value, "invite_id"), inviteLink: `${this.origin}/#invite=${inviteToken}`, expiresInSeconds: requiredInteger(value, "expires_in_seconds") };
  }
  public async listInvites(): Promise<VaultInviteView[]> { return array(await this.request("/v1/instance/invites", "GET")).map(vaultInvite); }
  public revokeInvite(inviteId: string): Promise<void> { return this.request(`/v1/instance/invites/${encodeURIComponent(inviteId)}`, "DELETE", undefined, false).then(() => undefined); }
  public async listVaultRequests(): Promise<VaultRequestView[]> { return array(await this.request("/v1/instance/vault-requests", "GET")).map(vaultRequest); }
  public approveVaultRequest(requestId: string): Promise<void> { return this.request(`/v1/instance/vault-requests/${encodeURIComponent(requestId)}/approve`, "POST").then(() => undefined); }
  public denyVaultRequest(requestId: string): Promise<void> { return this.request(`/v1/instance/vault-requests/${encodeURIComponent(requestId)}/deny`, "POST").then(() => undefined); }

  public setHostedCsrf(csrf: unknown): void {
    this.setCsrf(csrf, true);
  }

  private setCsrf(csrf: unknown, invalidateMcpAttempt: boolean): void {
    if (typeof csrf !== "string" || !csrf || csrf.length > 4096) throw new Error("enrollment-response");
    if (invalidateMcpAttempt && this.csrf !== csrf) this.invalidateMcpAttempts();
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
    this.viewValue = await this.readyView("hosted", this.origin, this.requireIdentity());
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

  private async restoreCommunitySession(expectedGeneration: number): Promise<void> {
    try {
      const session = await this.request("/v1/instance/session", "GET");
      this.assertMcpRestore(expectedGeneration);
      this.observeCsrf(session.csrf_token);
    } catch (error) {
      if (!(error instanceof HttpStatusError) || error.status !== 401) throw error;
      this.assertMcpRestore(expectedGeneration);
      this.observeSessionLoss();
    }
  }

  private async restoreCustodySession(expectedGeneration: number): Promise<void> {
    if (this.viewValue.mode === "hosted") {
      if (!await this.hostedSession(expectedGeneration)) throw new Error("session-expired");
      return;
    }
    await this.restoreCommunitySession(expectedGeneration);
    if (!this.csrf) throw new Error("session-expired");
  }

  private async hostedSession(expectedGeneration: number): Promise<boolean> {
    const session = await this.request("/hosted/v1/web/app-session", "GET");
    this.assertMcpRestore(expectedGeneration);
    if (typeof session.signed_in !== "boolean") throw new Error("enrollment-response");
    if (!session.signed_in) {
      this.observeSessionLoss();
      return false;
    }
    this.observeCsrf(session.csrf_token);
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
    this.invitationGeneration += 1;
    this.invitationId = undefined;
    this.approvedInvitationId = undefined;
    this.ownerClaimDigest = undefined;
    if (this.enrollmentClient?.purpose === "owner") this.enrollmentClient = undefined;
  }

  private invalidateMcpAttempts(): void { this.mcpAttemptGeneration += 1; }

  private assertMcpRestore(expectedGeneration: number): void {
    if (expectedGeneration !== this.mcpAttemptGeneration) throw new Error("enrollment-cancelled");
  }

  private observeCsrf(csrf: unknown): void {
    if (typeof csrf !== "string" || !csrf || csrf.length > 4096) throw new Error("enrollment-response");
    if (this.csrf && this.csrf !== csrf) this.invalidateMcpAttempts();
    this.csrf = csrf;
  }

  private observeSessionLoss(): void {
    if (this.csrf) this.invalidateMcpAttempts();
    this.csrf = undefined;
  }

  private assertMcpAttempt(generation: number): void {
    if (generation !== this.mcpAttemptGeneration) throw new Error("enrollment-cancelled");
    if (this.signOutsInFlight > 0) throw new Error("enrollment-cancelled");
    if (this.session.phase !== "ready" || this.requireIdentity().role !== "owner") throw new Error("owner_required");
  }

  private requireIdentity(): EnrollmentIdentity {
    const identity = this.session.publicIdentity();
    if (!identity || !["owner", "device", "gateway"].includes(identity.role)) throw new Error("enrollment-response");
    return identity;
  }

  private async readyView(mode: DeploymentMode, origin: string, identity: EnrollmentIdentity): Promise<EnrollmentView> {
    const hasGateway = await this.gatewayPresence();
    return { mode, stage: "ready", origin, owner: identity.role === "owner", ...(hasGateway === undefined ? {} : { hasGateway }) };
  }

  private async refreshGatewayPresence(): Promise<void> {
    const hasGateway = await this.gatewayPresence();
    const { hasGateway: _hasGateway, ...view } = this.viewValue;
    this.viewValue = { ...view, ...(hasGateway === undefined ? {} : { hasGateway }) };
  }

  private async gatewayPresence(): Promise<boolean | undefined> {
    return optionalBoolean(record(await this.core.invoke({ command: "_worker_enrollment_gateway_presence", args: {} })), "hasGateway");
  }

  private invitationStatus(invitationId = this.invitationId): Promise<Record<string, unknown>> {
    if (!invitationId) return Promise.reject(new Error("enrollment-state"));
    return this.request(`/v1/enrollment/invitations/${encodeURIComponent(invitationId)}`, "GET", undefined, true);
  }

  private isActiveOwnerInvitation(invitationId: string, invitationGeneration: number): boolean {
    return this.invitationId === invitationId
      && this.invitationGeneration === invitationGeneration
      && this.session.phase === "ready"
      && this.session.publicIdentity()?.role === "owner";
  }

  private isInvitationApproved(invitationId: string): boolean { return this.approvedInvitationId === invitationId; }

  private async ownerPhoneView(status: Record<string, unknown>): Promise<OwnerPhoneView> {
    const view = record(await this.core.invoke({ command: "_worker_enrollment_phone_status", args: { statusJson: JSON.stringify(status) } }));
    const invitationId = required(view, "invitationId");
    const claimDigest = typeof view.claimDigest === "string" && view.claimDigest ? view.claimDigest : undefined;
    const sas = typeof view.sas === "string" && view.sas ? view.sas : undefined;
    return { invitationId, ...(sas ? { sas } : {}), ...(claimDigest ? { claimDigest } : {}) };
  }

  private verifyOwnerClaim(invitationId: string, view: OwnerPhoneView): string {
    if (view.invitationId !== invitationId || !view.claimDigest) throw new Error("enrollment-response");
    const verificationCode = required(view, "sas");
    if (this.ownerClaimDigest !== undefined && this.ownerClaimDigest !== view.claimDigest) {
      this.resetOwnerInvitation();
      this.viewValue = { ...this.viewValue, stage: "expired", qrPayload: undefined, verificationCode: undefined, expiresInSeconds: undefined, errorCode: "phone_claim_changed" };
      throw new Error("enrollment-state");
    }
    return verificationCode;
  }

  private verifyPinnedOwnerClaim(invitationId: string, view: OwnerPhoneView): void {
    if (view.invitationId !== invitationId || !view.claimDigest || this.ownerClaimDigest !== view.claimDigest) {
      this.resetOwnerInvitation();
      this.viewValue = { ...this.viewValue, stage: "expired", qrPayload: undefined, verificationCode: undefined, expiresInSeconds: undefined, errorCode: "phone_claim_changed" };
      throw new Error("enrollment-state");
    }
  }

  private pinOwnerClaim(claimDigest: string): void {
    this.ownerClaimDigest ??= claimDigest;
  }

  private isCurrentInvitationCreation(invitationGeneration: number): boolean {
    return this.invitationGeneration === invitationGeneration
      && !this.invitationId
      && this.session.phase === "ready"
      && this.session.publicIdentity()?.role === "owner";
  }

  private discardOwnerInvitationClient(client = this.enrollmentClient): EnrollmentView {
    if (client?.purpose === "owner" && this.enrollmentClient === client) this.enrollmentClient = undefined;
    return this.view();
  }

  private async rustRequest(command: string, args: Record<string, unknown>): Promise<unknown> {
    const result = await this.core.invoke({ command, args });
    return JSON.parse(required(result, "request")) as unknown;
  }

  private computerView(value: Record<string, unknown>, stage: Extract<EnrollmentStage, "computer_waiting" | "computer_approved" | "computer_finishing" | "computer_expired" | "computer_denied">): EnrollmentView {
    const { errorCode: _errorCode, ...view } = this.viewValue;
    return { ...view, stage, qrPayload: stage === "computer_waiting" ? this.viewValue.qrPayload : undefined, verificationCode: stage === "computer_approved" && typeof value.sas === "string" ? value.sas : undefined, expiresInSeconds: safeInteger(value.expiresInSeconds), computerClaimant: true, awaitingOwner: value.awaitingOwner === true };
  }

  private async privateEnrollmentRequest(request: Record<string, unknown>): Promise<unknown> {
    const path = required(request, "path");
    const method = required(request, "method");
    if (method !== "POST" || !path.startsWith("/v1/enrollment/computers/") || typeof request.body !== "object" || request.body === null || Array.isArray(request.body)) throw new Error("enrollment-response");
    return this.request(path, "POST", request.body);
  }

  private async managementRequest(path: string, method: "GET" | "POST"): Promise<InstanceManagement> {
    try {
      return management(await this.request(path, method));
    } catch (error) {
      if (error instanceof HttpStatusError && (error.status === 401 || error.status === 403)) throw { code: `http-${error.status}` };
      throw error;
    }
  }

  private async request<T = Record<string, unknown>>(path: string, method: "GET" | "POST" | "PUT" | "DELETE", body?: unknown, authenticated = false, extraHeaders: Record<string, string> = {}): Promise<T> {
    if (this.signal?.aborted) throw new Error("enrollment-cancelled");
    const token = authenticated ? this.session.tokenForTransport() : undefined;
    const csrfHeader = this.viewValue.mode === "hosted" ? "X-Peppy-CSRF" : "X-Peppy-CSRF-Token";
    const controller = new AbortController();
    let abort!: (error: Error) => void;
    const cancelled = new Promise<never>((_resolve, reject) => {
      abort = error => {
        controller.abort();
        reject(error);
      };
    });
    const timeout = setTimeout(() => abort(new Error("enrollment-timeout")), ENROLLMENT_REQUEST_TIMEOUT_MS);
    const cancel = () => abort(new Error("enrollment-cancelled"));
    this.signal?.addEventListener("abort", cancel, { once: true });
    try {
      const pending = this.fetcher(new URL(path, this.fetchBase), {
        method,
        credentials: "include",
        headers: {
          ...(body === undefined ? {} : { "Content-Type": "application/json" }),
          ...(token ? { Authorization: `Bearer ${token}` } : {}),
          ...(this.csrf && method !== "GET" ? { [csrfHeader]: this.csrf } : {}),
          ...(method === "GET" ? {} : { [ATTACHMENT_RETENTION_PROTOCOL_HEADER_NAME]: ATTACHMENT_RETENTION_PROTOCOL_HEADER_VALUE }),
          ...extraHeaders,
        },
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: controller.signal,
      });
      return await Promise.race([readResponse<T>(pending, cancelled), cancelled]);
    } finally {
      clearTimeout(timeout);
      this.signal?.removeEventListener("abort", cancel);
    }
  }

  private captureClaimSession(response: Record<string, unknown>): void {
    const session = record(response.session);
    this.captureSession(session);
  }

  private captureSession(response: Record<string, unknown>): void {
    const csrf = response.csrf_token;
    if (typeof csrf === "string" && csrf) this.setCsrf(csrf, true);
  }

  private fail(error: unknown): void {
    const errorCode = error instanceof HttpStatusError ? error.code ?? `http-${error.status}` : error instanceof CoreRejectedError ? error.code : error instanceof Error ? error.message : "enrollment-error";
    const { setupCodeRequired: _setupCodeRequired, ...view } = this.viewValue;
    this.viewValue = { ...view, stage: "error", errorCode };
  }
}

function isDefinitiveVaultRequestRejection(error: unknown): boolean {
  return error instanceof HttpStatusError && error.status >= 400 && error.status < 500 && error.status !== 408 && error.status !== 429;
}

function mcpDeviceStatus(value: unknown): McpDeviceStatus {
  const status = record(value);
  const device = status.device === null ? null : mcpDevice(record(status.device));
  return {
    vaultId: mcpId(required(status, "vaultId")),
    available: requiredBoolean(status, "available"),
    serverOrigin: required(status, "serverOrigin"),
    hosted: requiredBoolean(status, "hosted"),
    device,
  };
}

function mcpDevice(value: Record<string, unknown>): McpDeviceStatus["device"] {
  return {
    deviceId: mcpId(required(value, "deviceId")),
    name: required(value, "name"),
    state: mcpDeviceState(required(value, "state")),
    lastSyncedAt: optionalString(value, "lastSyncedAt"),
    cleanupPending: requiredBoolean(value, "cleanupPending"),
  };
}

function mcpGrant(value: unknown): McpGrantView {
  const grant = record(value);
  const scopes = grant.scopes;
  if (!Array.isArray(scopes)) throw new Error("mcp-response");
  return {
    id: mcpId(required(grant, "id")),
    clientName: required(grant, "clientName"),
    scopes: mcpScopes(scopes),
    createdAt: required(grant, "createdAt"),
    lastUsedAt: optionalString(grant, "lastUsedAt"),
  };
}

function webhookList(value: unknown): WebhookList {
  const list = record(value);
  if (!Array.isArray(list.webhooks)) throw new Error("mcp-response");
  return { available: requiredBoolean(list, "available"), webhooks: list.webhooks.map(webhookView) };
}

function webhookCreated(value: unknown): WebhookCreated {
  const created = record(value);
  return { webhook: webhookView(created.webhook), secret: required(created, "secret") };
}

function webhookSecret(value: unknown): WebhookSecret {
  const secret = record(value);
  return { id: mcpId(required(secret, "id")), secret: required(secret, "secret") };
}

function webhookTestResult(value: unknown): WebhookTestResult {
  const result = record(value);
  const statusCode = optionalNumber(result, "statusCode");
  const durationMs = requiredNumber(result, "durationMs");
  if (statusCode !== null && (!Number.isInteger(statusCode) || statusCode < 100 || statusCode > 599) || !Number.isSafeInteger(durationMs) || durationMs < 0) throw new Error("mcp-response");
  return { ok: requiredBoolean(result, "ok"), statusCode, durationMs, error: optionalString(result, "error") };
}

function webhookView(value: unknown): WebhookView {
  const webhook = record(value);
  const events = webhook.events;
  const filter = webhook.filter;
  if (!Array.isArray(events) || !Array.isArray(filter)) throw new Error("mcp-response");
  const lastStatusCode = optionalNumber(webhook, "lastStatusCode");
  const pendingDeliveries = requiredNumber(webhook, "pendingDeliveries");
  if (lastStatusCode !== null && (!Number.isInteger(lastStatusCode) || lastStatusCode < 100 || lastStatusCode > 599) || !Number.isSafeInteger(pendingDeliveries) || pendingDeliveries < 0) throw new Error("mcp-response");
  return { id: mcpId(required(webhook, "id")), name: required(webhook, "name"), nameIsDerived: requiredBoolean(webhook, "nameIsDerived"), url: required(webhook, "url"), events: events.map(webhookEvent), filterMode: webhookFilterMode(required(webhook, "filterMode")), filter: filter.map(webhookFilterEntry), paused: requiredBoolean(webhook, "paused"), createdAt: required(webhook, "createdAt"), lastDeliveryState: webhookDeliveryState(required(webhook, "lastDeliveryState")), lastDeliveryAt: optionalString(webhook, "lastDeliveryAt"), lastStatusCode, lastError: optionalString(webhook, "lastError"), pendingDeliveries };
}

function webhookEvent(value: unknown): "message.received" { if (value !== "message.received") throw new Error("mcp-response"); return value; }
function webhookFilterMode(value: unknown): "all" | "only" | "except" { if (value !== "all" && value !== "only" && value !== "except") throw new Error("mcp-response"); return value; }
function webhookDeliveryState(value: unknown): "never" | "delivered" | "retrying" | "failed" { if (value !== "never" && value !== "delivered" && value !== "retrying" && value !== "failed") throw new Error("mcp-response"); return value; }
function webhookFilterEntry(value: unknown): WebhookView["filter"][number] { const entry = record(value); return { address: required(entry, "address"), label: optionalString(entry, "label"), contactId: optionalString(entry, "contactId"), bookId: optionalString(entry, "bookId") }; }

function mcpAuthorization(value: unknown): McpAuthorizationView {
  const authorization = record(value);
  const scopes = authorization.requestedScopes;
  if (!Array.isArray(scopes)) throw new Error("mcp-response");
  return {
    id: mcpAuthorizationId(required(authorization, "id")),
    clientName: required(authorization, "clientName"),
    clientId: mcpOpaqueString(authorization, "clientId", MAX_MCP_CLIENT_ID_LENGTH),
    requestedScopes: mcpScopes(scopes),
    serverOrigin: required(authorization, "serverOrigin"),
    resource: required(authorization, "resource"),
    redirectOrigin: mcpOrigin(authorization, "redirectOrigin"),
    redirectIsLoopback: requiredBoolean(authorization, "redirectIsLoopback"),
  };
}

function mcpOpaqueString(value: Record<string, unknown>, field: string, maxLength: number): string {
  const candidate = value[field];
  if (typeof candidate !== "string" || candidate.length === 0 || candidate.length > maxLength || /[\u0000-\u001F\u007F]/.test(candidate)) throw new Error("mcp-response");
  return candidate;
}

function mcpOrigin(value: Record<string, unknown>, field: string): string {
  const candidate = mcpOpaqueString(value, field, MAX_MCP_ORIGIN_LENGTH);
  let origin: URL;
  try {
    origin = new URL(candidate);
  } catch {
    throw new Error("mcp-response");
  }
  if ((origin.protocol !== "http:" && origin.protocol !== "https:") || origin.origin !== candidate) throw new Error("mcp-response");
  return candidate;
}

function mcpDecision(value: unknown): McpAuthorizationDecision {
  return { redirectUri: required(record(value), "redirectUri") };
}

function mcpScopes(values: unknown[]): McpScope[] {
  return values.map(value => {
    if (value === "conversations:read" || value === "messages:send" || value === "media:send") return value;
    throw new Error("mcp-response");
  });
}

function mcpDeviceState(value: string): McpDeviceState {
  if (value === "online" || value === "offline" || value === "needs_approval" || value === "revoked") return value;
  throw new Error("mcp-response");
}

function mcpId(value: string): string {
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value)) throw new Error("mcp-response");
  return value;
}

function mcpAuthorizationId(value: string): string {
  // A 32-byte unpadded base64url value has two unused and four significant final sextet bits.
  if (!/^[A-Za-z0-9_-]{42}[AEIMQUYcgkosw048]$/.test(value)) throw new Error("mcp-response");
  return value;
}

function requiredBoolean(value: Record<string, unknown>, field: string): boolean {
  if (typeof value[field] !== "boolean") throw new Error("mcp-response");
  return value[field];
}

function optionalString(value: Record<string, unknown>, field: string): string | null {
  const candidate = value[field];
  if (candidate === null) return null;
  if (typeof candidate !== "string") throw new Error("mcp-response");
  return candidate;
}

function requiredNumber(value: Record<string, unknown>, field: string): number {
  if (typeof value[field] !== "number") throw new Error("mcp-response");
  return value[field];
}

function optionalNumber(value: Record<string, unknown>, field: string): number | null {
  const candidate = value[field];
  if (candidate === null) return null;
  if (typeof candidate !== "number") throw new Error("mcp-response");
  return candidate;
}

function isTransientVaultRequestFailure(error: unknown): boolean {
  return !(error instanceof HttpStatusError) || error.status === 408 || error.status === 429 || error.status >= 500;
}

function isTransientComputerFailure(error: unknown): boolean {
  return error instanceof TypeError
    || error instanceof Error && error.message === "enrollment-timeout"
    || error instanceof HttpStatusError && (error.status === 408 || error.status === 429 || error.status >= 500);
}

function isFatalComputerActivationFailure(error: unknown): boolean {
  return error instanceof CheckpointError || error instanceof RuntimeCoreError || error instanceof BrowserCoreError && !(error instanceof CoreRejectedError);
}

function computerFailureCode(error: unknown): string {
  if (error instanceof HttpStatusError && error.code === "foreign_vault") return "foreign_vault";
  return "computer_verification_failed";
}

function hasHttpStatus(error: unknown, status: number): boolean {
  return typeof error === "object" && error !== null && "status" in error && error.status === status;
}

function computerEndpoint(request: Record<string, unknown>, action: "confirm"): string {
  const path = required(request, "path");
  if (!/^\/v1\/enrollment\/computers\/[0-9a-f-]{36}\/status$/.test(path)) throw new Error("enrollment-response");
  return path.replace("/status", `/${action}`);
}

function privateRequest(value: unknown): Record<string, unknown> {
  return record(JSON.parse(required(value, "request")) as unknown);
}

async function readResponse<T>(pending: Promise<Response>, interrupted: Promise<never>): Promise<T> {
  const response = await pending;
  if (!response.ok) throw await HttpStatusError.fromResponse(response, interrupted);
  if (response.status === 204) return {} as T;
  return JSON.parse(await readBoundedText(response, interrupted)) as T;
}

class HttpStatusError extends Error {
  public constructor(public readonly status: number, public readonly code?: string) { super(code ?? `http-${status}`); }

  public static async fromResponse(response: Response, interrupted: Promise<never>): Promise<HttpStatusError> {
    try {
      const body: unknown = JSON.parse(await readBoundedText(response, interrupted));
      const parsed = record(body);
      const code = typeof parsed.code === "string" ? parsed.code : parsed.error;
      if (typeof code === "string") return new HttpStatusError(response.status, code);
    } catch {}
    return new HttpStatusError(response.status);
  }
}

function hostedStage(account: Record<string, unknown>, origin: string): EnrollmentView {
  const classification = account.classification;
  const access = account.access;
  const hasVault = typeof account.vault_id === "string" && account.vault_id.length > 0;
  if (access === "readonly" && !hasVault) return { mode: "hosted", stage: "error", origin, errorCode: "billing-required" };
  if (!hasVault && (classification === "incomplete" || classification === "eligible")) return { mode: "hosted", stage: "setup", origin };
  return { mode: "hosted", stage: "locked", origin };
}

/** Reads at most MAX_JSON_BYTES. `interrupted` (deadline or local stop) wins over a body that never ends. */
async function readBoundedText(response: Response, interrupted: Promise<never>): Promise<string> {
  const declaredLength = response.headers.get("content-length");
  if (declaredLength !== null && (!/^\d+$/.test(declaredLength) || Number(declaredLength) > MAX_JSON_BYTES)) throw new Error("enrollment-response");
  if (response.body === null) throw new Error("enrollment-response");
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      const next = await Promise.race([reader.read(), interrupted]);
      if (next.done) break;
      total += next.value.byteLength;
      if (total > MAX_JSON_BYTES) throw new Error("enrollment-response");
      chunks.push(next.value);
    }
  } catch (error) {
    // Best-effort cancel only: an uncooperative source may never settle it, and the caller is already bounded.
    void reader.cancel().catch(() => undefined);
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
function optionalBoolean(value: Record<string, unknown>, key: string): boolean | undefined {
  if (!(key in value)) return undefined;
  if (typeof value[key] !== "boolean") throw new Error("enrollment-response");
  return value[key] as boolean;
}
function safeInteger(value: unknown): number | undefined { return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : undefined; }
function isDeviceId(value: string): boolean { return /^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/.test(value); }
function management(value: Record<string, unknown>): InstanceManagement {
  const devices = value.devices;
  if (!isDeviceId(required(value, "vault_id")) || !Array.isArray(devices) || safeInteger(value.storage_bytes) === undefined || safeInteger(value.quota_bytes) === undefined || safeInteger(value.replay_retention_days) === undefined) throw new Error("enrollment-response");
  return { vault_id: required(value, "vault_id"), devices: devices.map(device), storage_bytes: value.storage_bytes as number, quota_bytes: value.quota_bytes as number, replay_retention_days: value.replay_retention_days as number, isAdmin: value.is_admin === true, username: typeof value.username === "string" ? value.username : "" };
}
function device(value: unknown): EnrollmentDevice {
  const entry = record(value);
  const kind = required(entry, "kind");
  const role = required(entry, "role");
  if (!isDeviceId(required(entry, "device_id")) || !["browser", "desktop", "android", "ios"].includes(kind) || !["owner", "device", "gateway"].includes(role) || typeof entry.name !== "string" || typeof entry.revoked !== "boolean" || typeof entry.gateway_online !== "boolean") throw new Error("enrollment-response");
  const presence = optionalPresence(entry.presence);
  const wakeRoute = optionalBoolean(entry, "wake_route");
  return { device_id: required(entry, "device_id"), name: entry.name, kind: kind as EnrollmentDevice["kind"], role: role as EnrollmentDevice["role"], revoked: entry.revoked, gateway_online: entry.gateway_online, ...(presence ? { presence } : {}), ...(wakeRoute === undefined ? {} : { wake_route: wakeRoute }) };
}
function optionalPresence(value: unknown): EnrollmentDevice["presence"] {
  if (value === undefined) return undefined;
  const entry = record(value);
  if (entry.state !== "connected" && entry.state !== "seen" && entry.state !== "never") return undefined;
  if (entry.last_seen_age_seconds !== null && safeInteger(entry.last_seen_age_seconds) === undefined) throw new Error("enrollment-response");
  return { state: entry.state, last_seen_age_seconds: entry.last_seen_age_seconds as number | null };
}
function array(value: unknown): unknown[] { if (!Array.isArray(value)) throw new Error("enrollment-response"); return value; }
function requiredInteger(value: Record<string, unknown>, key: string): number { const output = safeInteger(value[key]); if (output === undefined) throw new Error("enrollment-response"); return output; }
function vaultSummary(value: unknown): VaultSummary { const entry = record(value); return { vaultId: required(entry, "vault_id"), username: required(entry, "username"), isAdmin: boolean(entry, "is_admin"), deviceCount: requiredInteger(entry, "device_count"), storageBytes: requiredInteger(entry, "storage_bytes"), createdAt: required(entry, "created_at") }; }
function vaultInvite(value: unknown): VaultInviteView { const entry = record(value); const state = required(entry, "state"); if (!["active", "used", "revoked", "expired"].includes(state)) throw new Error("enrollment-response"); return { inviteId: required(entry, "invite_id"), state: state as VaultInviteView["state"], expiresInSeconds: requiredInteger(entry, "expires_in_seconds"), createdAt: required(entry, "created_at") }; }
function vaultRequest(value: unknown): VaultRequestView { const entry = record(value); const client = required(entry, "client"); const state = required(entry, "state"); if (!["browser", "desktop", "android", "ios"].includes(client) || !["pending", "approved", "denied", "completed", "expired"].includes(state)) throw new Error("enrollment-response"); return { requestId: required(entry, "request_id"), displayName: required(entry, "display_name"), client: client as VaultRequestView["client"], state: state as VaultRequestView["state"], createdAt: required(entry, "created_at"), expiresInSeconds: requiredInteger(entry, "expires_in_seconds") }; }
function boolean(value: Record<string, unknown>, key: string): boolean { if (typeof value[key] !== "boolean") throw new Error("enrollment-response"); return value[key] as boolean; }
