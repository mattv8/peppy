import { describe, expect, it, vi } from "vitest";
import { BrowserEnrollment, type EnrollmentIdentity, type EnrollmentSession } from "./enrollment.js";

const origin = "https://peppy.test";
const vaultId = "00000000-0000-0000-0000-000000000001";
const deviceId = "00000000-0000-0000-0000-000000000002";
const invitationId = "00000000-0000-0000-0000-000000000003";
const challengeId = "00000000-0000-0000-0000-000000000004";

class TestSession implements EnrollmentSession {
  public phase: "unenrolled" | "locked" | "ready" = "unenrolled";
  public identity?: EnrollmentIdentity;
  public readonly finishEnrollment = vi.fn(async (_command: "_worker_enrollment_finish_owner" | "_worker_enrollment_finish_manual", responseJson: string) => {
    const response = JSON.parse(responseJson) as Record<string, unknown>;
    this.phase = "ready";
    this.identity = { role: response.role === "gateway" ? "gateway" : response.role === "device" ? "device" : "owner" };
  });
  public readonly unlock = vi.fn(async () => {
    this.phase = "ready";
    this.identity = { role: "owner" };
  });
  public tokenForTransport(): string { return "device-token"; }
  public publicIdentity(): EnrollmentIdentity | undefined { return this.identity; }
}

function instanceInfo(claimed: boolean, mode: "community" | "hosted" = "community", setupRequired = !claimed) {
  return { mode, claimed, setup_required: setupRequired, password_auth: true, qr_pairing: true, shared_secret_pairing: true };
}

function json(value: unknown, init?: ResponseInit): Response {
  return new Response(JSON.stringify(value), { headers: { "content-type": "application/json" }, ...init });
}

function pathOf(input: RequestInfo | URL): string { return new URL(String(input)).pathname; }

class LifecycleCore {
  public state: "none" | "fresh" | "owner" | "owner-draft" | "owner-invitation" = "none";
  public readonly invoke = vi.fn(async ({ command }: { command: string; args: Record<string, unknown> }) => {
    if (command === "_worker_enrollment_new") {
      if (this.state !== "none") throw new Error("invalid-request");
      this.state = "fresh";
      return {};
    }
    if (command === "_worker_enrollment_create_owner") {
      if (this.state !== "fresh") throw new Error("unavailable");
      return { request: JSON.stringify({ operation_id: vaultId }) };
    }
    if (command === "_worker_enrollment_manual_challenge") {
      if (this.state !== "fresh") throw new Error("unavailable");
      return { request: JSON.stringify({ device: { device_id: deviceId } }) };
    }
    if (command === "_worker_enrollment_complete_manual_challenge") return { request: JSON.stringify({ admission_signature: "proof" }) };
    if (command === "_worker_enrollment_new_owner_from_identity") {
      this.state = "owner";
      return {};
    }
    if (command === "_worker_enrollment_create_phone_invitation") {
      if (this.state !== "owner") throw new Error("unavailable");
      this.state = "owner-draft";
      return { request: JSON.stringify({ method: "transfer", transfer_public_key: "key", invitation_token_digest: "digest" }) };
    }
    if (command === "_worker_enrollment_consume_created_phone_invitation") {
      if (this.state !== "owner-draft") throw new Error("unavailable");
      this.state = "owner-invitation";
      return {};
    }
    if (command === "_worker_enrollment_get_phone_qr") {
      if (this.state !== "owner-invitation") throw new Error("unavailable");
      return { qr: "local-secret-qr" };
    }
    if (command === "_worker_enrollment_pending_vault_request") return null;
    return {};
  });

  public clearAfterFinish(): void { this.state = "none"; }
}

function lifecycleFetcher(statuses: Array<Record<string, unknown>> = []) {
  return vi.fn<typeof fetch>(async (input, init) => {
    const path = pathOf(input);
    if (path === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
    if (path === "/v1/instance") return json(instanceInfo(statuses.length > 0));
    if (path === "/v1/instance/session" && init?.method === "GET") return json({ code: "invalid_session" }, { status: 401 });
    if (path === "/v1/instance/claim") return json({ vault_id: vaultId, device_id: deviceId, replayed: false, session: { csrf_token: "csrf", expires_in_seconds: 43_200 } });
    if (path === "/v1/enrollment/challenges") return json({ enrollment_id: challengeId });
    if (path.endsWith("/complete")) return json({ vault_id: vaultId, device_id: deviceId, role: "owner", material: {} });
    if (path === "/v1/enrollment/invitations" && init?.method === "POST") return json({ invitation_id: invitationId, expires_in_seconds: 300 });
    if (path === `/v1/enrollment/invitations/${invitationId}` && init?.method === "DELETE") return new Response(null, { status: 204 });
    if (path === `/v1/enrollment/invitations/${invitationId}`) return json(statuses.shift() ?? { state: "waiting", expires_in_seconds: 300 });
    throw new Error(`unexpected ${path}`);
  });
}

describe("BrowserEnrollment", () => {
  it("restores community CSRF before a fresh worker sends a mutation", async () => {
    const core = new LifecycleCore();
    const session = new TestSession();
    session.phase = "ready";
    session.identity = { role: "owner" };
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
      if (path === "/v1/instance") return json(instanceInfo(true));
      if (path === "/v1/instance/session" && init?.method === "GET") return json({ csrf_token: "restored-csrf" });
      if (path === "/v1/instance/invites" && init?.method === "POST") return json({ invite_id: invitationId, invite_token: "invite", expires_in_seconds: 600 });
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await expect(enrollment.discover()).resolves.toMatchObject({ stage: "ready", owner: true });
    await enrollment.createInvite();

    expect(fetcher).toHaveBeenLastCalledWith(expect.any(URL), expect.objectContaining({
      method: "POST",
      headers: expect.objectContaining({ "X-Peppy-CSRF-Token": "restored-csrf" }),
    }));
  });

  it("restores community CSRF while the local identity remains locked", async () => {
    const core = new LifecycleCore();
    const session = new TestSession();
    session.phase = "locked";
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
      if (path === "/v1/instance") return json(instanceInfo(true));
      if (path === "/v1/instance/session" && init?.method === "GET") return json({ csrf_token: "restored-locked-csrf" });
      if (path === "/v1/instance/invites" && init?.method === "POST") return json({ invite_id: invitationId, invite_token: "invite", expires_in_seconds: 600 });
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await expect(enrollment.discover()).resolves.toMatchObject({ stage: "locked" });
    await enrollment.createInvite();

    expect(fetcher).toHaveBeenLastCalledWith(expect.any(URL), expect.objectContaining({
      method: "POST",
      headers: expect.objectContaining({ "X-Peppy-CSRF-Token": "restored-locked-csrf" }),
    }));
  });

  it("does not clear a completed vault request after importing its identity", async () => {
    const core = {
      invoke: vi.fn(async ({ command }: { command: string }) => {
        if (command === "_worker_enrollment_new") return {};
        if (command === "_worker_enrollment_create_vault") return { request: JSON.stringify({ request_id: "request" }) };
        return {};
      }),
    };
    const session = new TestSession();
    const fetcher = vi.fn<typeof fetch>(async (input) => {
      const path = pathOf(input);
      if (path === "/v1/instance/vaults") return json({ vault_id: vaultId, device_id: deviceId, role: "owner", session: {} });
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await expect(enrollment.createVault("taylor", { adminPassword: "password-password", passphrase: "secret" })).resolves.toMatchObject({ stage: "ready" });
    expect(core.invoke).not.toHaveBeenCalledWith({ command: "_worker_enrollment_clear_vault_request", args: {} });
  });

  it("clears a rejected vault request but retains transient submission material", async () => {
    const core = {
      invoke: vi.fn(async ({ command }: { command: string }) => {
        if (command === "_worker_enrollment_parse_vault_invite_link") return { origin, inviteToken: "invite" };
        if (command === "_worker_enrollment_begin_vault_request") return { request: JSON.stringify({ request_id: "request" }) };
        return {};
      }),
    };
    const session = new TestSession();
    const rejected = new BrowserEnrollment(core, session, origin, async () => json({ code: "invite_invalid" }, { status: 400 }));
    await rejected.beginVaultRequest("https://peppy.test/#invite=invite", "Taylor");
    expect(core.invoke).toHaveBeenCalledWith({ command: "_worker_enrollment_clear_vault_request", args: {} });

    core.invoke.mockClear();
    const unavailable = new BrowserEnrollment(core, session, origin, async () => new Response("temporary", { status: 503 }));
    await unavailable.beginVaultRequest("https://peppy.test/#invite=invite", "Taylor");
    expect(core.invoke).not.toHaveBeenCalledWith({ command: "_worker_enrollment_clear_vault_request", args: {} });
  });

  it("keeps an approved request available for a corrected username", async () => {
    const core = {
      invoke: vi.fn(async ({ command }: { command: string }) => {
        if (command === "_worker_enrollment_create_vault") return { request: JSON.stringify({ request_id: "request" }) };
        return {};
      }),
    };
    const session = new TestSession();
    const enrollment = new BrowserEnrollment(core, session, origin, async () => json({ code: "username_taken" }, { status: 409 }));

    await expect(enrollment.createVault("taken", { adminPassword: "password-password", passphrase: "secret" })).resolves.toMatchObject({ stage: "invite_approved", errorCode: "username_taken" });
    expect(core.invoke).toHaveBeenCalledWith(expect.objectContaining({ command: "_worker_enrollment_create_vault", args: expect.objectContaining({ username: "taken" }) }));
  });
  it("allows an explicitly open fresh claim without a setup token", async () => {
    const core = new LifecycleCore();
    const session = new TestSession();
    const fetcher = vi.fn<typeof fetch>(async (input) => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
      if (path === "/v1/instance") return json(instanceInfo(false, "community", false));
      if (path === "/v1/instance/claim") return json({ vault_id: vaultId, device_id: deviceId, role: "owner", session: { csrf_token: "csrf" } });
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await expect(enrollment.discover()).resolves.toMatchObject({ stage: "setup", setupCodeRequired: false });
    await expect(enrollment.createOwner({ adminPassword: "password-password", passphrase: "secret" })).resolves.toMatchObject({ stage: "ready", owner: true });
    expect(core.invoke).toHaveBeenCalledWith({ command: "_worker_enrollment_create_owner", args: { adminPassword: "password-password", passphrase: "secret", username: "" } });
  });

  it("retries an open claim after a transient error without adding a setup token", async () => {
    const core = new LifecycleCore();
    const session = new TestSession();
    let claimAttempts = 0;
    const claimBodies: string[] = [];
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
      if (path === "/v1/instance") return json(instanceInfo(false, "community", false));
      if (path === "/v1/instance/claim") {
        claimBodies.push(String(init?.body));
        claimAttempts += 1;
        return claimAttempts === 1
          ? new Response("temporarily unavailable", { status: 503 })
          : json({ vault_id: vaultId, device_id: deviceId, role: "owner", session: { csrf_token: "csrf" } });
      }
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await enrollment.discover();
    await expect(enrollment.createOwner({ adminPassword: "password-password", passphrase: "secret" })).resolves.toMatchObject({ stage: "error" });
    await expect(enrollment.createOwner({ adminPassword: "password-password", passphrase: "secret" })).resolves.toMatchObject({ stage: "ready", owner: true });
    expect(claimBodies).toEqual([JSON.stringify({ operation_id: vaultId }), JSON.stringify({ operation_id: vaultId })]);
    expect(core.invoke.mock.calls.filter(([request]) => request.command === "_worker_enrollment_create_owner")).toHaveLength(2);
  });

  it("rejects malformed discovery instead of treating it as an open server", async () => {
    const core = new LifecycleCore();
    const session = new TestSession();
    const fetcher = vi.fn<typeof fetch>(async (input) => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
      if (path === "/v1/instance") return json({ mode: "community", setup_required: false });
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await expect(enrollment.discover()).resolves.toMatchObject({ stage: "error", errorCode: "enrollment-response" });
    expect(enrollment.view()).not.toHaveProperty("setupCodeRequired");
  });

  it("surfaces only known JSON claim error codes", async () => {
    const core = new LifecycleCore();
    const session = new TestSession();
    const fetcher = vi.fn<typeof fetch>(async (input) => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
      if (path === "/v1/instance") return json(instanceInfo(false));
      if (path === "/v1/instance/claim") return json({ code: "invalid_setup_token" }, { status: 401 });
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await enrollment.discover();
    await expect(enrollment.createOwner({ setupToken: "wrong", adminPassword: "password-password", passphrase: "secret" })).resolves.toMatchObject({ stage: "error", errorCode: "invalid_setup_token" });
    expect(enrollment.view()).not.toHaveProperty("setupCodeRequired");
  });

  it("keeps unrecognized claim failures generic", async () => {
    const core = new LifecycleCore();
    const session = new TestSession();
    const fetcher = vi.fn<typeof fetch>(async (input) => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
      if (path === "/v1/instance") return json(instanceInfo(false));
      if (path === "/v1/instance/claim") return new Response("unexpected server detail", { status: 401 });
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await enrollment.discover();
    await expect(enrollment.createOwner({ setupToken: "wrong", adminPassword: "password-password", passphrase: "secret" })).resolves.toMatchObject({ stage: "error", errorCode: "http-401" });
  });

  it("completes the self-hosted owner and phone contract without trusting server QR material", async () => {
    const statuses = [
      { state: "claimed", expires_in_seconds: 290, claim: {}, transcript: {}, encrypted_key_package: null, transfer_nonce: null, profile: null },
      { state: "claimed", expires_in_seconds: 285, claim: {}, transcript: {}, encrypted_key_package: null, transfer_nonce: null, profile: null },
      { state: "approved", expires_in_seconds: 280, claim: null, transcript: {}, encrypted_key_package: "cipher", transfer_nonce: "nonce", profile: null },
      { state: "consumed", expires_in_seconds: 275, claim: null, transcript: {}, encrypted_key_package: "cipher", transfer_nonce: "nonce", profile: null },
    ];
    const core = { invoke: vi.fn(async ({ command }: { command: string }) => {
      if (command === "_worker_enrollment_create_owner") return { request: JSON.stringify({ operation_id: vaultId, setup_token: "opaque" }) };
      if (command === "_worker_enrollment_create_phone_invitation") return { request: JSON.stringify({ method: "transfer", transfer_public_key: "key", invitation_token_digest: "digest" }) };
      if (command === "_worker_enrollment_get_phone_qr") return { qr: "local-secret-qr" };
      if (command === "_worker_enrollment_phone_status") return { sas: "123 456" };
      if (command === "_worker_enrollment_approve_phone_invitation") return { request: JSON.stringify({ transcript_hash: "hash", encrypted_key_package: "cipher", transfer_nonce: "nonce" }) };
      if (command === "_worker_enrollment_pending_vault_request") return null;
      return {};
    }) };
    const session = new TestSession();
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json({ code: "not_found" }, { status: 404 });
      if (path === "/v1/instance") return json(instanceInfo(false));
      if (path === "/v1/instance/claim") return json({ vault_id: vaultId, device_id: deviceId, replayed: false, session: { csrf_token: "claim-csrf", expires_in_seconds: 43_200 } });
      if (path === "/v1/enrollment/invitations" && init?.method === "POST") return json({ invitation_id: invitationId, expires_in_seconds: 300 });
      if (path.endsWith("/approve")) return json({});
      if (path === `/v1/enrollment/invitations/${invitationId}`) return json(statuses.shift());
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await expect(enrollment.discover()).resolves.toMatchObject({ mode: "community", stage: "setup" });
    await expect(enrollment.createOwner({ setupToken: "setup", adminPassword: "password-password", passphrase: "secret" })).resolves.toMatchObject({ stage: "ready", owner: true });
    await expect(enrollment.addPhone("transfer")).resolves.toMatchObject({ stage: "waiting_phone", qrPayload: "local-secret-qr", expiresInSeconds: 300 });
    await expect(enrollment.status()).resolves.toMatchObject({ stage: "verify_phone", verificationCode: "123 456" });
    await expect(enrollment.approve("secret")).resolves.toMatchObject({ stage: "waiting_phone" });
    await expect(enrollment.status()).resolves.toMatchObject({ stage: "waiting_phone" });
    await expect(enrollment.status()).resolves.toMatchObject({ stage: "phone_connected" });

    expect(core.invoke).toHaveBeenCalledWith({ command: "_worker_enrollment_get_phone_qr", args: {} });
    expect(session.finishEnrollment).toHaveBeenCalledWith("_worker_enrollment_finish_owner", expect.stringContaining('"session"'), "secret");
    const invitationCall = fetcher.mock.calls.find(([input, init]) => pathOf(input) === "/v1/enrollment/invitations" && init?.method === "POST");
    expect(invitationCall?.[1]).toMatchObject({ headers: expect.objectContaining({ Authorization: "Bearer device-token", "X-Peppy-CSRF-Token": "claim-csrf" }) });
  });

  it("reinitializes an owner invitation client after first-owner activation clears the fresh Rust client", async () => {
    const core = new LifecycleCore();
    const session = new TestSession();
    session.finishEnrollment.mockImplementationOnce(async () => { core.clearAfterFinish(); session.phase = "ready"; session.identity = { role: "owner" }; });
    const fetcher = lifecycleFetcher();
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await enrollment.discover();
    await enrollment.createOwner({ setupToken: "setup", adminPassword: "password-password", passphrase: "secret" });
    await expect(enrollment.addPhone("transfer")).resolves.toMatchObject({ stage: "waiting_phone", qrPayload: "local-secret-qr" });

    expect(core.invoke).toHaveBeenCalledWith({ command: "_worker_enrollment_new_owner_from_identity", args: { deviceName: "Peppy Browser" } });
    expect(fetcher.mock.calls.filter(([input, init]) => pathOf(input) === "/v1/enrollment/invitations" && init?.method === "POST")).toHaveLength(1);
  });

  it("reinitializes an owner invitation client after manual owner activation", async () => {
    const core = new LifecycleCore();
    const session = new TestSession();
    session.finishEnrollment.mockImplementationOnce(async () => { core.clearAfterFinish(); session.phase = "ready"; session.identity = { role: "owner" }; });
    const fetcher = lifecycleFetcher([{ claimed: true }]);
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await enrollment.discover();
    await enrollment.connectSharedSecret("secret");
    await expect(enrollment.addPhone("transfer")).resolves.toMatchObject({ stage: "waiting_phone" });

    expect(core.invoke).toHaveBeenCalledWith({ command: "_worker_enrollment_new_owner_from_identity", args: { deviceName: "Peppy Browser" } });
  });

  it.each(["cancelled", "expired"] as const)("creates a fresh owner invitation client after a %s invitation", async terminal => {
    const core = new LifecycleCore();
    const session = new TestSession();
    session.phase = "ready";
    session.identity = { role: "owner" };
    const fetcher = lifecycleFetcher(terminal === "expired" ? [{ state: "expired", expires_in_seconds: 0 }] : []);
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await enrollment.discover();
    await enrollment.addPhone("transfer");
    if (terminal === "cancelled") await enrollment.cancel();
    else await enrollment.status();
    await expect(enrollment.addPhone("transfer")).resolves.toMatchObject({ stage: "waiting_phone" });

    expect(core.invoke.mock.calls.filter(([request]) => request.command === "_worker_enrollment_new_owner_from_identity")).toHaveLength(2);
    expect(fetcher.mock.calls.filter(([input, init]) => pathOf(input) === "/v1/enrollment/invitations" && init?.method === "POST")).toHaveLength(2);
  });

  it("coalesces concurrent Add phone requests and rejects non-owners before Rust or HTTP", async () => {
    const ownerCore = new LifecycleCore();
    const ownerSession = new TestSession();
    ownerSession.phase = "ready";
    ownerSession.identity = { role: "owner" };
    let releaseInvitation!: () => void;
    const invitationGate = new Promise<void>(resolve => { releaseInvitation = resolve; });
    const ownerFetcher = lifecycleFetcher();
    ownerFetcher.mockImplementation(async (input, init) => {
      if (pathOf(input) === "/v1/enrollment/invitations" && init?.method === "POST") {
        await invitationGate;
        return json({ invitation_id: invitationId, expires_in_seconds: 300 });
      }
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
      if (path === "/v1/instance") return json(instanceInfo(true));
      throw new Error(`unexpected ${path}`);
    });
    const owner = new BrowserEnrollment(ownerCore, ownerSession, origin, ownerFetcher);
    await owner.discover();
    const first = owner.addPhone("transfer");
    const second = owner.addPhone("transfer");
    releaseInvitation();
    await Promise.all([first, second]);
    expect(ownerFetcher.mock.calls.filter(([input, init]) => pathOf(input) === "/v1/enrollment/invitations" && init?.method === "POST")).toHaveLength(1);

    const deviceCore = new LifecycleCore();
    const deviceSession = new TestSession();
    deviceSession.phase = "ready";
    deviceSession.identity = { role: "device" };
    const deviceFetcher = lifecycleFetcher();
    const device = new BrowserEnrollment(deviceCore, deviceSession, origin, deviceFetcher);
    await device.discover();
    await expect(device.addPhone("transfer")).resolves.toMatchObject({ stage: "error", errorCode: "enrollment-state" });
    expect(deviceCore.invoke).not.toHaveBeenCalledWith(expect.objectContaining({ command: "_worker_enrollment_create_phone_invitation" }));
    expect(deviceFetcher.mock.calls.some(([input]) => pathOf(input) === "/v1/enrollment/invitations")).toBe(false);
  });

  it("keeps community enrollment HTTP on the app-origin proxy while preserving the configured API identity", async () => {
    const logicalOrigin = "https://api.peppy.test";
    const fetchBase = "https://app.peppy.test";
    const requestedUrls: string[] = [];
    const core = { invoke: vi.fn(async ({ command }: { command: string }) =>
      command === "_worker_enrollment_create_owner" ? { request: JSON.stringify({ operation_id: vaultId }) }
        : command === "_worker_enrollment_pending_vault_request" ? null : {}) };
    const session = new TestSession();
    const fetcher = vi.fn<typeof fetch>(async input => {
      const url = new URL(String(input));
      requestedUrls.push(url.href);
      if (url.pathname === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
      if (url.pathname === "/v1/instance") return json(instanceInfo(false));
      if (url.pathname === "/v1/instance/claim") return json({ vault_id: vaultId, device_id: deviceId, replayed: false, session: { csrf_token: "csrf", expires_in_seconds: 43_200 } });
      throw new Error(`unexpected ${url.pathname}`);
    });
    const enrollment = new BrowserEnrollment(core, session, logicalOrigin, fetcher, fetchBase);

    await enrollment.discover();
    await enrollment.createOwner({ setupToken: "setup", adminPassword: "password-password", passphrase: "secret" });

    expect(requestedUrls).toEqual([
      `${fetchBase}/hosted/v1/enrollment/config`,
      `${fetchBase}/v1/instance`,
      `${fetchBase}/v1/instance/claim`,
    ]);
    expect(core.invoke).toHaveBeenCalledWith({ command: "_worker_enrollment_new", args: { origin: logicalOrigin, deviceName: "Peppy Browser" } });
  });

  it("initializes a fresh Rust client for a claimed server and derives the completed role from verified session metadata", async () => {
    const core = { invoke: vi.fn(async ({ command }: { command: string }) => {
      if (command === "_worker_enrollment_manual_challenge") return { request: JSON.stringify({ device: { device_id: deviceId }, vault_id: null, credential_digest: "digest" }) };
      if (command === "_worker_enrollment_complete_manual_challenge") return { request: JSON.stringify({ admission_signature: "proof", device_signature: "proof", credential_digest: "digest" }) };
      if (command === "_worker_enrollment_pending_vault_request") return null;
      return {};
    }) };
    const session = new TestSession();
    session.finishEnrollment.mockImplementationOnce(async () => { session.phase = "ready"; session.identity = { role: "device" }; });
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
      if (path === "/v1/instance") return json(instanceInfo(true));
      if (path === "/v1/instance/session" && init?.method === "GET") return json({ code: "invalid_session" }, { status: 401 });
      if (path === "/v1/enrollment/challenges") return json({ enrollment_id: challengeId, origin, vault_id: vaultId, profile: {}, fingerprint: "a".repeat(64), key_epoch: 1, device: {}, requested_role: "device", nonce: "nonce", credential_digest: "digest", expires_in_seconds: 300 });
      return json({ vault_id: vaultId, device_id: deviceId, role: "device", material: { profile: {}, check_header: "header", fingerprint: "a".repeat(64), key_epoch: 1, admission_public_key: "key" } });
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await expect(enrollment.discover()).resolves.toMatchObject({ stage: "signin" });
    expect(core.invoke).toHaveBeenCalledWith({ command: "_worker_enrollment_new", args: { origin, deviceName: "Peppy Browser" } });
    await expect(enrollment.connectSharedSecret("secret")).resolves.toEqual(expect.objectContaining({ stage: "ready", owner: false }));
    expect(enrollment.view()).not.toHaveProperty("hasGateway");
  });

  it("restores community CSRF for an unenrolled browser on /manage before an invite mutation", async () => {
    const core = new LifecycleCore();
    const session = new TestSession();
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
      if (path === "/v1/instance") return json(instanceInfo(true));
      if (path === "/v1/instance/session" && init?.method === "GET") return json({ csrf_token: "fresh-restored-csrf" });
      if (path === "/v1/instance/invites" && init?.method === "POST") return json({ invite_id: invitationId, invite_token: "invite", expires_in_seconds: 600 });
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await expect(enrollment.discover()).resolves.toMatchObject({ stage: "signin" });
    await enrollment.createInvite();

    expect(fetcher).toHaveBeenLastCalledWith(expect.any(URL), expect.objectContaining({
      method: "POST",
      headers: expect.objectContaining({ "X-Peppy-CSRF-Token": "fresh-restored-csrf" }),
    }));
  });

  it("clears transient CSRF without a header when a fresh browser has no community session cookie", async () => {
    const core = new LifecycleCore();
    const session = new TestSession();
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
      if (path === "/v1/instance") return json(instanceInfo(true));
      if (path === "/v1/instance/session" && init?.method === "GET") return json({ code: "invalid_session" }, { status: 401 });
      if (path === "/v1/instance/invites" && init?.method === "POST") return json({ invite_id: invitationId, invite_token: "invite", expires_in_seconds: 600 });
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await expect(enrollment.discover()).resolves.toMatchObject({ stage: "signin" });
    await enrollment.createInvite();

    const headers = fetcher.mock.calls.at(-1)?.[1]?.headers as Record<string, string>;
    expect(headers).not.toHaveProperty("X-Peppy-CSRF-Token");
  });

  it("reuses an unlocked owner and unlocks a locked local identity without starting new-device admission", async () => {
    const readyCore = { invoke: vi.fn(async () => ({})) };
    const readySession = new TestSession();
    readySession.phase = "ready";
    readySession.identity = { role: "owner" };
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
      if (path === "/v1/instance/session") return json({ code: "invalid_session" }, { status: 401 });
      return json(instanceInfo(true));
    });
    const ready = new BrowserEnrollment(readyCore, readySession, origin, fetcher);

    await expect(ready.discover()).resolves.toEqual(expect.objectContaining({ stage: "ready", owner: true }));
    expect(readyCore.invoke).toHaveBeenCalledWith({ command: "_worker_enrollment_new_owner_from_identity", args: { deviceName: "Peppy Browser" } });
    expect(readyCore.invoke).not.toHaveBeenCalledWith(expect.objectContaining({ command: "_worker_enrollment_new" }));

    const lockedCore = { invoke: vi.fn(async () => ({})) };
    const lockedSession = new TestSession();
    lockedSession.phase = "locked";
    const locked = new BrowserEnrollment(lockedCore, lockedSession, origin, fetcher);
    await expect(locked.discover()).resolves.toMatchObject({ stage: "locked" });
    await expect(locked.connectSharedSecret("secret")).resolves.toMatchObject({ stage: "ready", owner: true });
    expect(lockedSession.unlock).toHaveBeenCalledWith("secret");
    expect(lockedCore.invoke).not.toHaveBeenCalledWith(expect.objectContaining({ command: "_worker_enrollment_manual_challenge" }));
    expect(lockedCore.invoke).not.toHaveBeenCalledWith(expect.objectContaining({ command: "_worker_enrollment_new" }));
  });

  it("does not finish or fabricate ready state when Rust rejects the manual proof", async () => {
    const core = { invoke: vi.fn(async ({ command }: { command: string }) => {
      if (command === "_worker_enrollment_manual_challenge") return { request: "{}" };
      if (command === "_worker_enrollment_complete_manual_challenge") throw new Error("invalid-request");
      return {};
    }) };
    const session = new TestSession();
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json({}, { status: 404 });
      if (path === "/v1/instance") return json(instanceInfo(true));
      return json({ enrollment_id: challengeId });
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await enrollment.discover();
    await expect(enrollment.connectSharedSecret("wrong")).resolves.toMatchObject({ stage: "error", errorCode: "invalid-request" });
    expect(session.finishEnrollment).not.toHaveBeenCalled();
  });

  it("uses the anonymous app session before the protected account endpoint for an unenrolled hosted browser", async () => {
    const core = { invoke: vi.fn(async () => ({})) };
    const session = new TestSession();
    const requestedPaths: string[] = [];
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = pathOf(input);
      requestedPaths.push(path);
      if (path === "/hosted/v1/enrollment/config") return json(instanceInfo(true, "hosted"));
      if (path === "/hosted/v1/web/app-session") return json({ signed_in: false });
      if (path === "/hosted/v1/web/app-account") return json({ error: "unauthenticated" }, { status: 401 });
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await expect(enrollment.discover()).resolves.toMatchObject({ mode: "hosted", stage: "signin" });

    expect(requestedPaths).toEqual(["/hosted/v1/enrollment/config", "/hosted/v1/web/app-session"]);
    expect(core.invoke).toHaveBeenCalledWith({ command: "_worker_enrollment_new", args: { origin, deviceName: "Peppy Browser" } });
  });

  it.each([
    [{ classification: "eligible" }, "setup"],
    [{ classification: "existing", vault_id: vaultId }, "locked"],
  ] as const)("classifies signed-in hosted account %o as %s", async (account, stage) => {
    const core = { invoke: vi.fn(async () => ({})) };
    const session = new TestSession();
    const requestedPaths: string[] = [];
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = pathOf(input);
      requestedPaths.push(path);
      if (path === "/hosted/v1/enrollment/config") return json(instanceInfo(true, "hosted"));
      if (path === "/hosted/v1/web/app-session") return json({ signed_in: true, csrf_token: "restored-csrf" });
      if (path === "/hosted/v1/web/app-account") return json(account);
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await expect(enrollment.discover()).resolves.toMatchObject({ mode: "hosted", stage });
    expect(requestedPaths).toEqual(["/hosted/v1/enrollment/config", "/hosted/v1/web/app-session", "/hosted/v1/web/app-account"]);
  });

  it.each([
    { signed_in: "true", csrf_token: "csrf" },
    { signed_in: true },
    { signed_in: true, csrf_token: "" },
    { signed_in: true, csrf_token: "x".repeat(4097) },
  ])("fails closed for malformed hosted app sessions", async appSession => {
    const core = { invoke: vi.fn(async () => ({})) };
    const session = new TestSession();
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json(instanceInfo(true, "hosted"));
      if (path === "/hosted/v1/web/app-session") return json(appSession);
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await expect(enrollment.discover()).resolves.toMatchObject({ mode: "hosted", stage: "error", errorCode: "enrollment-response" });
    expect(fetcher.mock.calls.some(([input]) => pathOf(input) === "/hosted/v1/web/app-account")).toBe(false);
  });

  it.each(["ready", "locked"] as const)("restores hosted CSRF for an already %s browser without requesting the account", async phase => {
    const core = { invoke: vi.fn(async () => ({})) };
    const session = new TestSession();
    session.phase = phase;
    session.identity = phase === "ready" ? { role: "owner" } : undefined;
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json(instanceInfo(true, "hosted"));
      if (path === "/hosted/v1/web/app-session") return json({ signed_in: true, csrf_token: "restored-csrf" });
      if (path.includes("/revoke")) return json({ vault_id: vaultId, devices: [], storage_bytes: 0, quota_bytes: 1, replay_retention_days: 30 });
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await expect(enrollment.discover()).resolves.toMatchObject({ mode: "hosted", stage: phase });
    await enrollment.revokeDevice(deviceId);

    expect(fetcher.mock.calls.some(([input]) => pathOf(input) === "/hosted/v1/web/app-account")).toBe(false);
    expect(fetcher.mock.calls.at(-1)?.[1]).toMatchObject({ headers: { "X-Peppy-CSRF": "restored-csrf" } });
    expect(session.phase).toBe(phase);
  });

  it("clears restored CSRF when a hosted browser is signed out", async () => {
    const core = { invoke: vi.fn(async () => ({})) };
    const session = new TestSession();
    const appSessions = [{ signed_in: true, csrf_token: "restored-csrf" }, { signed_in: false }];
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json(instanceInfo(true, "hosted"));
      if (path === "/hosted/v1/web/app-session") return json(appSessions.shift());
      if (path === "/hosted/v1/web/app-account") return json({ classification: "existing", vault_id: vaultId });
      if (path.includes("/revoke")) return json({ vault_id: vaultId, devices: [], storage_bytes: 0, quota_bytes: 1, replay_retention_days: 30 });
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await enrollment.discover();
    await expect(enrollment.discover()).resolves.toMatchObject({ stage: "signin" });
    await enrollment.revokeDevice(deviceId);

    expect(fetcher.mock.calls.at(-1)?.[1]?.headers).toEqual({});
  });

  it("returns hosted signin when the account session expires before the protected account request", async () => {
    const core = { invoke: vi.fn(async () => ({})) };
    const session = new TestSession();
    const fetcher = vi.fn<typeof fetch>(async input => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json(instanceInfo(true, "hosted"));
      if (path === "/hosted/v1/web/app-session") return json({ signed_in: true, csrf_token: "restored-csrf" });
      if (path === "/hosted/v1/web/app-account") return json({ error: "unauthenticated" }, { status: 401 });
      if (path.includes("/revoke")) return json({ vault_id: vaultId, devices: [], storage_bytes: 0, quota_bytes: 1, replay_retention_days: 30 });
      throw new Error(`unexpected ${path}`);
    });
    const enrollment = new BrowserEnrollment(core, session, origin, fetcher);

    await expect(enrollment.discover()).resolves.toMatchObject({ mode: "hosted", stage: "signin" });
    await enrollment.revokeDevice(deviceId);

    expect(fetcher.mock.calls.at(-1)?.[1]?.headers).toEqual({});
  });

  it("uses the mode-specific CSRF header and bounds streamed JSON before parsing", async () => {
    const core = { invoke: vi.fn(async () => ({})) };
    const session = new TestSession();
    const oversized = new ReadableStream<Uint8Array>({ start(controller) { controller.enqueue(new Uint8Array(65 * 1024)); controller.close(); } });
    const hostedFetcher = vi.fn<typeof fetch>(async input => {
      const path = pathOf(input);
      if (path === "/hosted/v1/enrollment/config") return json(instanceInfo(true, "hosted"));
      if (path === "/hosted/v1/web/app-session") return json({ signed_in: false });
      if (path.includes("/revoke")) return json({ vault_id: vaultId, devices: [], storage_bytes: 0, quota_bytes: 1, replay_retention_days: 30 });
      throw new Error(`unexpected ${path}`);
    });
    const hosted = new BrowserEnrollment(core, session, origin, hostedFetcher);
    await hosted.discover();
    hosted.setHostedCsrf("hosted-csrf");
    await hosted.revokeDevice(deviceId);
    expect(hostedFetcher.mock.calls.at(-1)?.[1]).toMatchObject({ headers: { "X-Peppy-CSRF": "hosted-csrf" } });

    const boundedFetcher = vi.fn<typeof fetch>(async input => pathOf(input) === "/hosted/v1/enrollment/config" ? new Response(oversized, { headers: { "content-length": String(65 * 1024) } }) : json(instanceInfo(true)));
    const bounded = new BrowserEnrollment(core, session, origin, boundedFetcher);
    await expect(bounded.discover()).resolves.toMatchObject({ stage: "error", errorCode: "enrollment-response" });
  });

  it("keeps the default global fetch bound through a same-origin community claim", async () => {
    const fixtureOrigin = "https://127.0.0.1:17443";
    let claimBoundary: Record<string, unknown> | undefined;
    const defaultFetch = async function (this: unknown, input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
      if (this !== globalThis) throw new TypeError("Failed to execute 'fetch' on 'WorkerGlobalScope': Illegal invocation");
      const request = new Request(input, init);
      if (request.url === `${fixtureOrigin}/hosted/v1/enrollment/config`) return json({ code: "not_found" }, { status: 404 });
      if (request.url === `${fixtureOrigin}/v1/instance`) return json(instanceInfo(false));
      if (request.url === `${fixtureOrigin}/v1/instance/claim`) {
        const headerNames: string[] = [];
        request.headers.forEach((_value, name) => headerNames.push(name));
        claimBoundary = {
          url: request.url,
          method: request.method,
          credentials: request.credentials,
          headerNames,
          bodyKeys: Object.keys(JSON.parse(await request.text()) as Record<string, unknown>).sort(),
        };
        return json({ vault_id: vaultId, device_id: deviceId, replayed: false, session: { csrf_token: "csrf", expires_in_seconds: 43_200 } });
      }
      throw new Error(`unexpected ${request.url}`);
    };
    const ownerRequest = {
      operation_id: vaultId, setup_token: "setup", admin_password: "password-password",
      device: { device_id: deviceId, name: "Peppy Browser", kind: "browser", signing_public_key: "public-key" },
      material: { profile: {}, check_header: "header", fingerprint: "a".repeat(64), key_epoch: 1, admission_public_key: "admission-key" },
      credential_digest: "b".repeat(64),
    };
    const core = { invoke: vi.fn(async ({ command }: { command: string }) =>
      command === "_worker_enrollment_create_owner" ? { request: JSON.stringify(ownerRequest) }
        : command === "_worker_enrollment_pending_vault_request" ? null : {}) };
    vi.stubGlobal("fetch", defaultFetch);
    try {
      const session = new TestSession();
      const enrollment = new BrowserEnrollment(core, session, fixtureOrigin);

      await expect(enrollment.discover()).resolves.toMatchObject({ stage: "setup", origin: fixtureOrigin });
      await expect(enrollment.createOwner({ setupToken: "setup", adminPassword: "password-password", passphrase: "secret" })).resolves.toMatchObject({ stage: "ready" });
      expect(claimBoundary).toEqual({
        url: `${fixtureOrigin}/v1/instance/claim`,
        method: "POST",
        credentials: "include",
        headerNames: ["content-type"],
        bodyKeys: ["admin_password", "credential_digest", "device", "material", "operation_id", "setup_token"],
      });
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
