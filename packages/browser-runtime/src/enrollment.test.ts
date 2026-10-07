import { describe, expect, it, vi } from "vitest";
import { BrowserEnrollment } from "./enrollment.js";

const APP = "https://app.example.test";
const API = "https://api.example.test";
const requestId = "11111111-2222-4333-8444-555555555555";
const deviceId = "22222222-2222-4333-8444-555555555555";
const vaultId = "33333333-2222-4333-8444-555555555555";
const fingerprint = "a".repeat(64);
const token = "b".repeat(96);

function core() {
  return { invoke: vi.fn(async ({ command }: { command: string }) => {
    if (command === "_worker_join_start") return { qrPayload: "qr-with-api-origin" };
    if (command === "_worker_join_request") return { joinRequestId: requestId, deviceId };
    if (command === "_worker_join_open_offer") return { intent: "intent", deviceId, publicKey: { ed25519_public_key: "public" } };
    if (command === "_worker_join_confirm") return { apiOrigin: API, challengeToken: "challenge", deviceId, publicKey: { ed25519_public_key: "public" }, vaultId, keyEpoch: 1, profileFingerprint: fingerprint, signature: "signature" };
    return {};
  }) };
}

function vault(): Record<string, unknown> {
  return { vault_id: vaultId, device_id: deviceId, role: "device", key_epoch: 1, profile_fingerprint: fingerprint, public_key_profile: { crypto_suite: 1 }, encrypted_vault_check_header: btoa(JSON.stringify({ profile: {}, check: {} })) };
}

describe("BrowserEnrollment", () => {
  it("uses the canonical origin only in the QR while every request remains worker same-origin", async () => {
    const calls: string[] = [];
    let challenge = 0;
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      calls.push(String(input));
      const path = new URL(String(input)).pathname;
      if (path === "/web/config.json") return json({ version: 1, apiOrigin: API });
      if (path === "/v1/pairing/join-requests" && init?.method === "POST") return json({ join_request_id: requestId, poll_secret: "secret", expires_in_seconds: 300 });
      if (path.endsWith(requestId)) return json({ state: "offered", sealed_intent_token: "sealed", intent_digest: "digest", expires_in_seconds: 200 });
      if (path.endsWith("/claim")) return json({ key_digest: "digest", sas: "123456", claim_secret: "claim", expires_in_seconds: 180 });
      if (path.endsWith("/challenge")) return challenge++ === 0 ? json({ code: "pairing_challenge_unavailable" }, 401) : json({ challenge_token: "challenge", vault_id: vaultId, key_epoch: 1, profile_fingerprint: fingerprint, requested_role: "device", expires_in_seconds: 120 });
      if (path === "/v1/pairing/consume") return json({ device_token: token, vault_id: vaultId, device_id: deviceId });
      if (path === "/v1/vault") return json(vault());
      throw new Error(path);
    });
    const activate = vi.fn(async () => undefined);
    const instance = new BrowserEnrollment(core(), APP, activate, fetcher);
    expect(await instance.start()).toMatchObject({ state: "waiting", origin: API, qrPayload: "qr-with-api-origin" });
    expect(await instance.poll()).toMatchObject({ state: "claimed", sas: "123456" });
    expect(await instance.poll()).toMatchObject({ state: "claimed" });
    expect(await instance.poll()).toMatchObject({ state: "confirm" });
    expect(await instance.confirm("phrase")).toEqual({ state: "approved" });
    expect(activate).toHaveBeenCalledOnce();
    expect(calls.every(url => new URL(url).origin === APP)).toBe(true);
    expect(JSON.stringify(instance.view())).not.toContain("secret");
  });

  it("maps the snake-case server challenge into the Rust camel-case deny-unknown-fields contract", async () => {
    const trustedCore = core();
    let challenge = 0;
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const path = new URL(String(input)).pathname;
      if (path === "/web/config.json") return json({ version: 1, apiOrigin: API });
      if (path === "/v1/pairing/join-requests" && init?.method === "POST") return json({ join_request_id: requestId, poll_secret: "secret", expires_in_seconds: 300 });
      if (path.endsWith(requestId)) return json({ state: "offered", sealed_intent_token: "sealed", intent_digest: "digest", expires_in_seconds: 200 });
      if (path.endsWith("/claim")) return json({ key_digest: "digest", sas: "123456", claim_secret: "claim", expires_in_seconds: 180 });
      if (path.endsWith("/challenge")) { challenge += 1; return json({ challenge_token: "challenge", vault_id: vaultId, key_epoch: 1, profile_fingerprint: fingerprint, requested_role: "device", expires_in_seconds: 120 }); }
      throw new Error(path);
    });
    const instance = new BrowserEnrollment(trustedCore, APP, async () => undefined, fetcher);
    await instance.start(); await instance.poll(); await instance.poll();
    expect(challenge).toBe(1);
    expect(trustedCore.invoke).toHaveBeenCalledWith({ command: "_worker_join_challenge", args: { challengeToken: "challenge", vaultId, keyEpoch: 1, profileFingerprint: fingerprint, requestedRole: "device" } });
  });

  it("keeps a consumed private identity for a wrong-phrase activation retry", async () => {
    let activation = 0;
    let consume = 0;
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const path = new URL(String(input)).pathname;
      if (path === "/web/config.json") return json({ version: 1, apiOrigin: API });
      if (path === "/v1/pairing/join-requests" && init?.method === "POST") return json({ join_request_id: requestId, poll_secret: "secret", expires_in_seconds: 300 });
      if (path.endsWith(requestId)) return json({ state: "offered", sealed_intent_token: "sealed", intent_digest: "digest", expires_in_seconds: 200 });
      if (path.endsWith("/claim")) return json({ key_digest: "digest", sas: "123456", claim_secret: "claim", expires_in_seconds: 180 });
      if (path.endsWith("/challenge")) return json({ challenge_token: "challenge", vault_id: vaultId, key_epoch: 1, profile_fingerprint: fingerprint, requested_role: "device", expires_in_seconds: 120 });
      if (path === "/v1/pairing/consume") { consume += 1; return json({ device_token: token, vault_id: vaultId, device_id: deviceId }); }
      if (path === "/v1/vault") return json(vault());
      throw new Error(path);
    });
    const instance = new BrowserEnrollment(core(), APP, async () => { if (activation++ === 0) throw new Error("wrong phrase"); }, fetcher);
    await instance.start(); await instance.poll(); await instance.poll();
    await expect(instance.confirm("wrong")).rejects.toThrow("wrong phrase");
    expect(instance.view()).toMatchObject({ state: "confirm" });
    await instance.confirm("right");
    expect(consume).toBe(1);
    expect(instance.view()).toEqual({ state: "approved" });
  });

  it("does not let an aborted stale start overwrite a newer QR and rejects oversized responses", async () => {
    let release!: () => void;
    const wait = new Promise<void>(resolve => { release = resolve; });
    let starts = 0;
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const path = new URL(String(input)).pathname;
      if (path === "/web/config.json") return json({ version: 1, apiOrigin: API });
      if (path === "/v1/pairing/join-requests") { starts += 1; if (starts === 1) await wait; return json({ join_request_id: requestId, poll_secret: "secret", expires_in_seconds: 300 }); }
      throw new Error(String(init?.method));
    });
    const instance = new BrowserEnrollment(core(), APP, async () => undefined, fetcher);
    const first = instance.start();
    const second = instance.start();
    release();
    await Promise.all([first, second]);
    expect(instance.view()).toMatchObject({ state: "waiting", qrPayload: "qr-with-api-origin" });

    const oversized = new BrowserEnrollment(core(), APP, async () => undefined, vi.fn<typeof fetch>(async () => new Response("x".repeat(64 * 1024 + 1))));
    expect(await oversized.start()).toMatchObject({ state: "failed", errorCode: "join-network" });
  });
});

function json(value: unknown, status = 200): Response { return new Response(JSON.stringify(value), { status, headers: { "content-type": "application/json" } }); }
