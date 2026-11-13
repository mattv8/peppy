import { afterEach, describe, expect, it, vi } from "vitest";
import { BrowserEnrollment } from "./enrollment.js";
import { BrowserWorkerHost, type BrowserNetwork, type BrowserPort, type HostSession } from "./host.js";
import type { WebhookInput } from "@peppy/generated/mcp";
import type { OwnerLockManager } from "./owner.js";
import { OriginTransport } from "./transport.js";

const ORIGIN = "https://peppy.test";
const VAULT_ID = "00000000-0000-4000-8000-000000000001";
const DEVICE_ID = "00000000-0000-4000-8000-000000000002";
const DEVICE_TOKEN = "a".repeat(96);
const WEBHOOK_ID = "00000000-0000-4000-8000-000000000003";
const validInput: WebhookInput = {
  name: "Inbox",
  url: "https://hooks.example.test/peppy",
  events: ["message.received"],
  filterMode: "only",
  filter: [{ address: "alice@example.test", label: "Alice", contactId: null, bookId: "book-1" }],
  paused: false,
};
const WEBHOOK_SERVER_CODES = [
  "webhook_url_invalid",
  "webhook_url_insecure",
  "webhook_url_blocked",
  "webhook_name_invalid",
  "webhook_filter_invalid",
  "webhook_limit",
  "webhook_not_found",
  "mcp_unavailable",
  "mcp_device_required",
  "mcp_worker_unavailable",
  "mcp_worker_rejected",
  "mcp_worker_invalid",
  "owner_required",
];

class RestoredSession implements HostSession {
  public phase: "locked" | "ready" | "closed" = "locked";

  public async query(command: string): Promise<Record<string, unknown>> {
    if (command === "_worker_identity_metadata") return { vaultId: VAULT_ID, deviceId: DEVICE_ID };
    return {};
  }

  public async mutate(): Promise<Record<string, never>> { return {}; }

  public async unlock(): Promise<void> { this.phase = "ready"; }

  public async enroll(): Promise<void> { this.phase = "ready"; }

  public shutdown(): void { this.phase = "closed"; }

  public publicIdentity(): { role: "owner" } { return { role: "owner" }; }

  public tokenForTransport(): string { return DEVICE_TOKEN; }

  public async finishEnrollment(): Promise<void> { this.phase = "ready"; }
}

function locks(): OwnerLockManager {
  return {
    async request(name, _options, operation) { return operation({ name }); },
  };
}

function port(): BrowserPort & { messages: unknown[]; emit(message: unknown): void } {
  const result = {
    messages: [] as unknown[],
    onmessage: null as BrowserPort["onmessage"],
    postMessage(message: unknown) { result.messages.push(message); },
    close() {},
    emit(message: unknown) { result.onmessage?.({ data: message } as MessageEvent); },
  };
  return result;
}

async function responseFor(portValue: ReturnType<typeof port>, id: number): Promise<unknown> {
  await vi.waitFor(() => expect(portValue.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === id)).toBe(true));
  return portValue.messages.find(message => typeof message === "object" && message !== null && "id" in message && message.id === id);
}

function rpcError(response: unknown): { code: string; message: string } {
  expect(response).toMatchObject({ ok: false, error: { code: expect.any(String), message: expect.any(String) } });
  return (response as { error: { code: string; message: string } }).error;
}

function expectSanitizedError(response: unknown, code: string): void {
  expect(rpcError(response)).toMatchObject({ code });
  const payload = JSON.stringify(response);
  expect(payload).not.toContain(DEVICE_TOKEN);
  expect(payload).not.toContain(ORIGIN);
}

function restoredNetwork(session: RestoredSession, fetcher: typeof fetch, signal: AbortSignal): BrowserNetwork {
  const enrollment = new BrowserEnrollment({ invoke: async () => ({}) }, session, ORIGIN, fetcher, ORIGIN, signal);
  const transport = new OriginTransport({ origin: ORIGIN, deviceToken: () => session.tokenForTransport(), fetch: fetcher });
  return {
    start: async () => true,
    stop: async () => undefined,
    listDevices: async () => transport.json("/v1/devices", { method: "GET" }),
    mcpStatus: async () => enrollment.mcpStatus(),
  } as BrowserNetwork;
}

describe("restored browser MCP lifecycle", () => {
  afterEach(() => vi.useRealTimers());

  it("loads MCP status through the enrollment request after restore and unlock", async () => {
    const paths: string[] = [];
    let mcpSignalAborted: boolean | undefined;
    const fetcher = vi.fn<typeof fetch>(async (input, init) => {
      const url = new URL(String(input));
      paths.push(url.pathname);
      if (url.pathname === "/v1/mcp/device") {
        mcpSignalAborted = init?.signal?.aborted;
        expect(init?.credentials).toBe("include");
        return new Response(JSON.stringify({ vaultId: VAULT_ID, available: true, serverOrigin: ORIGIN, hosted: false, device: null }), { headers: { "content-type": "application/json" } });
      }
      throw new Error(`unexpected request ${url.pathname}`);
    });
    const session = new RestoredSession();
    const lifecycle = new AbortController();
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      network: restoredNetwork(session, fetcher, lifecycle.signal),
    });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "unlock", args: { passphrase: "vault phrase" } });
    expect(await responseFor(attached, 1)).toMatchObject({ ok: true });
    attached.emit({ id: 2, command: "mcp_status", args: {} });

    const mcpResponse = await responseFor(attached, 2);
    expect(mcpSignalAborted).toBe(false);
    expect(paths).toEqual(["/v1/mcp/device"]);
    expect(mcpResponse).toMatchObject({ ok: true, value: { vaultId: VAULT_ID, available: true } });
  });

  it("reports a cancelled enrollment request without leaking credentials", async () => {
    const session = new RestoredSession();
    const lifecycle = new AbortController();
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: restoredNetwork(session, vi.fn(), lifecycle.signal) });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "unlock", args: { passphrase: "vault phrase" } });
    await responseFor(attached, 1);
    lifecycle.abort();
    attached.emit({ id: 2, command: "mcp_status", args: {} });

    expectSanitizedError(await responseFor(attached, 2), "cancelled");
  });

  it("reports an aborted fetch as cancelled without leaking request details", async () => {
    const fetcher = vi.fn<typeof fetch>(() => Promise.reject(new DOMException(`${ORIGIN}?token=${DEVICE_TOKEN}`, "AbortError")));
    const session = new RestoredSession();
    const lifecycle = new AbortController();
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: restoredNetwork(session, fetcher, lifecycle.signal) });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "unlock", args: { passphrase: "vault phrase" } });
    await responseFor(attached, 1);
    attached.emit({ id: 2, command: "mcp_status", args: {} });

    expectSanitizedError(await responseFor(attached, 2), "cancelled");
  });

  it("reports an enrollment timeout without leaking credentials", async () => {
    vi.useFakeTimers();
    const session = new RestoredSession();
    const lifecycle = new AbortController();
    const fetcher = vi.fn<typeof fetch>(() => new Promise<Response>(() => undefined));
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: restoredNetwork(session, fetcher, lifecycle.signal) });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "unlock", args: { passphrase: "vault phrase" } });
    await responseFor(attached, 1);
    attached.emit({ id: 2, command: "mcp_status", args: {} });
    await vi.advanceTimersByTimeAsync(25_000);

    expectSanitizedError(await responseFor(attached, 2), "timeout");
  });

  it("reports a fetch rejection as a network error without leaking request details", async () => {
    const fetcher = vi.fn<typeof fetch>(() => Promise.reject(new TypeError(`Failed to fetch ${ORIGIN}/v1/mcp/device?token=${DEVICE_TOKEN}`)));
    const session = new RestoredSession();
    const lifecycle = new AbortController();
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: restoredNetwork(session, fetcher, lifecycle.signal) });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "unlock", args: { passphrase: "vault phrase" } });
    await responseFor(attached, 1);
    attached.emit({ id: 2, command: "mcp_status", args: {} });

    expectSanitizedError(await responseFor(attached, 2), "network");
  });

  it.each([
    ["a server code", new Response(JSON.stringify({ code: "device_revoked" }), { status: 403, headers: { "content-type": "application/json" } }), "device_revoked"],
    ["an unbounded server code", new Response(JSON.stringify({ code: `${ORIGIN}?token=${DEVICE_TOKEN}` }), { status: 503, headers: { "content-type": "application/json" } }), "http-503"],
  ])("reports %s as a sanitized HTTP error", async (_description, response, expectedCode) => {
    const fetcher = vi.fn<typeof fetch>(() => Promise.resolve(response));
    const session = new RestoredSession();
    const lifecycle = new AbortController();
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: restoredNetwork(session, fetcher, lifecycle.signal) });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "unlock", args: { passphrase: "vault phrase" } });
    await responseFor(attached, 1);
    attached.emit({ id: 2, command: "mcp_status", args: {} });

    expectSanitizedError(await responseFor(attached, 2), expectedCode);
  });
});

describe("webhook RPC input validation", () => {
  async function createWebhook(input: unknown): Promise<{ response: unknown; webhookCreate: ReturnType<typeof vi.fn> }> {
    const session = new RestoredSession();
    const webhookCreate = vi.fn(async () => ({ webhook: {}, secret: "s" }));
    const network = { start: async () => true, stop: async () => undefined, webhookCreate } as unknown as BrowserNetwork;
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "unlock", args: { passphrase: "vault phrase" } });
    expect(await responseFor(attached, 1)).toMatchObject({ ok: true });
    attached.emit({ id: 2, command: "webhook_create", args: { input } });
    return { response: await responseFor(attached, 2), webhookCreate };
  }

  it("passes a well-formed webhook input through as a typed value", async () => {
    const { response, webhookCreate } = await createWebhook(validInput);
    expect(response).toMatchObject({ ok: true });
    expect(webhookCreate).toHaveBeenCalledWith(validInput);
  });

  it("accepts a null name and an empty filter", async () => {
    const { response, webhookCreate } = await createWebhook({ ...validInput, name: null, filterMode: "all", filter: [] });
    expect(response).toMatchObject({ ok: true });
    expect(webhookCreate).toHaveBeenCalledWith({ ...validInput, name: null, filterMode: "all", filter: [] });
  });

  it.each([
    ["unknown top-level key", { ...validInput, extra: true }],
    ["unknown filter entry key", { ...validInput, filter: [{ ...validInput.filter[0], extra: "x" }] }],
    ["wrong filterMode", { ...validInput, filterMode: "some" }],
    ["missing paused", (({ paused: _paused, ...rest }) => rest)(validInput)],
    ["oversize filter", { ...validInput, filter: Array.from({ length: 51 }, (_, index) => ({ address: `a${index}@example.test`, label: null, contactId: null, bookId: null })) }],
    ["bad event", { ...validInput, events: ["message.sent"] }],
    ["empty events", { ...validInput, events: [] }],
    ["url over byte bound", { ...validInput, url: `https://example.test/${"a".repeat(2048)}` }],
    ["non-string label", { ...validInput, filter: [{ ...validInput.filter[0], label: 7 }] }],
  ])("rejects %s before reaching the network", async (_name, input) => {
    const { response, webhookCreate } = await createWebhook(input);
    expect(rpcError(response)).toMatchObject({ code: "invalid-request" });
    expect(webhookCreate).not.toHaveBeenCalled();
  });
});

const webhookViewFixture = {
  id: WEBHOOK_ID,
  name: "Inbox",
  nameIsDerived: false,
  url: "https://hooks.example.test/peppy",
  events: ["message.received"],
  filterMode: "only",
  filter: [{ address: "alice@example.test", label: "Alice", contactId: null, bookId: "book-1" }],
  paused: false,
  createdAt: "2026-01-01T00:00:00Z",
  lastDeliveryState: "never",
  lastDeliveryAt: null,
  lastStatusCode: null,
  lastError: null,
  pendingDeliveries: 0,
};

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function webhookEnrollment(fetcher: typeof fetch): BrowserEnrollment {
  return new BrowserEnrollment({ invoke: async () => ({}) }, new RestoredSession(), ORIGIN, fetcher, ORIGIN);
}

/** A JSON object whose serialized form is exactly `bytes` long (ASCII only). */
function paddedJson(bytes: number, body: Record<string, unknown>): Response {
  const base = JSON.stringify({ ...body, padding: "" });
  return jsonResponse({ ...body, padding: "x".repeat(bytes - base.length) });
}

describe("webhook enrollment transport", () => {
  it("maps the webhook list into typed views", async () => {
    const fetcher = vi.fn<typeof fetch>(async () => jsonResponse({ available: true, webhooks: [webhookViewFixture] }));

    await expect(webhookEnrollment(fetcher).webhooksList()).resolves.toEqual({ available: true, webhooks: [webhookViewFixture] });
  });

  it.each([
    ["list", (enrollment: BrowserEnrollment) => enrollment.webhooksList(), "GET", "/v1/mcp/webhooks", undefined, () => jsonResponse({ available: true, webhooks: [] })],
    ["create", (enrollment: BrowserEnrollment) => enrollment.webhookCreate(validInput), "POST", "/v1/mcp/webhooks", validInput, () => jsonResponse({ webhook: webhookViewFixture, secret: "whsec_fixture" })],
    ["update", (enrollment: BrowserEnrollment) => enrollment.webhookUpdate(WEBHOOK_ID, validInput), "PUT", `/v1/mcp/webhooks/${WEBHOOK_ID}`, validInput, () => jsonResponse(webhookViewFixture)],
    ["delete", (enrollment: BrowserEnrollment) => enrollment.webhookDelete(WEBHOOK_ID), "DELETE", `/v1/mcp/webhooks/${WEBHOOK_ID}`, undefined, () => new Response(null, { status: 204 })],
    ["reveal", (enrollment: BrowserEnrollment) => enrollment.webhookRevealSecret(WEBHOOK_ID), "POST", `/v1/mcp/webhooks/${WEBHOOK_ID}/secret`, undefined, () => jsonResponse({ id: WEBHOOK_ID, secret: "whsec_fixture" })],
    ["rotate", (enrollment: BrowserEnrollment) => enrollment.webhookRotateSecret(WEBHOOK_ID), "POST", `/v1/mcp/webhooks/${WEBHOOK_ID}/rotate-secret`, undefined, () => jsonResponse({ id: WEBHOOK_ID, secret: "whsec_fixture" })],
    ["test", (enrollment: BrowserEnrollment) => enrollment.webhookTest(WEBHOOK_ID), "POST", `/v1/mcp/webhooks/${WEBHOOK_ID}/test`, undefined, () => jsonResponse({ ok: true, statusCode: 204, durationMs: 12, error: null })],
  ])("sends the %s request with the device credential", async (_name, call, method, path, body, response) => {
    const fetcher = vi.fn<typeof fetch>(async () => response());

    await call(webhookEnrollment(fetcher));

    expect(fetcher).toHaveBeenCalledTimes(1);
    const [input, init] = fetcher.mock.calls[0];
    expect(new URL(String(input)).pathname).toBe(path);
    expect(init?.method).toBe(method);
    expect(init?.headers).toMatchObject({ Authorization: `Bearer ${DEVICE_TOKEN}` });
    if (body === undefined) expect(init?.body).toBeUndefined();
    else expect(JSON.parse(String(init?.body))).toEqual(body);
  });

  it.each([
    ["webhookUpdate", (enrollment: BrowserEnrollment) => enrollment.webhookUpdate("not-a-uuid", validInput)],
    ["webhookDelete", (enrollment: BrowserEnrollment) => enrollment.webhookDelete("not-a-uuid")],
    ["webhookRevealSecret", (enrollment: BrowserEnrollment) => enrollment.webhookRevealSecret("not-a-uuid")],
    ["webhookRotateSecret", (enrollment: BrowserEnrollment) => enrollment.webhookRotateSecret("not-a-uuid")],
    ["webhookTest", (enrollment: BrowserEnrollment) => enrollment.webhookTest("not-a-uuid")],
  ])("rejects a malformed %s identifier before any request", async (_name, call) => {
    const fetcher = vi.fn<typeof fetch>(async () => jsonResponse({}));

    await expect(call(webhookEnrollment(fetcher))).rejects.toThrow("mcp-response");
    expect(fetcher).not.toHaveBeenCalled();
  });

  it.each(WEBHOOK_SERVER_CODES)("keeps the server code %s on a rejected webhook request", async code => {
    const fetcher = vi.fn<typeof fetch>(async () => jsonResponse({ code }, 422));

    await expect(webhookEnrollment(fetcher).webhookCreate(validInput)).rejects.toMatchObject({ code });
  });

  it("accepts a webhook list of exactly 1 MiB", async () => {
    const fetcher = vi.fn<typeof fetch>(async () => paddedJson(1024 * 1024, { available: true, webhooks: [] }));

    await expect(webhookEnrollment(fetcher).webhooksList()).resolves.toEqual({ available: true, webhooks: [] });
  });

  it("rejects a webhook list above 2 MiB", async () => {
    const fetcher = vi.fn<typeof fetch>(async () => paddedJson(2 * 1024 * 1024 + 1, { available: true, webhooks: [] }));

    await expect(webhookEnrollment(fetcher).webhooksList()).rejects.toThrow("enrollment-response");
  });

  it("keeps the 64 KiB bound on other MCP reads", async () => {
    const fetcher = vi.fn<typeof fetch>(async () => paddedJson(64 * 1024 + 1, { vaultId: VAULT_ID }));

    await expect(webhookEnrollment(fetcher).mcpStatus()).rejects.toThrow("enrollment-response");
  });
});

describe("webhook host change notifications", () => {
  const changedEvent = JSON.stringify({ event: "changed" });

  function webhookNetwork(): BrowserNetwork {
    return {
      start: async () => true,
      stop: async () => undefined,
      webhooksList: async () => ({ available: true, webhooks: [] }),
      webhookCreate: async () => ({ webhook: webhookViewFixture, secret: "whsec_fixture" }),
      webhookUpdate: async () => webhookViewFixture,
      webhookDelete: async () => undefined,
      webhookRevealSecret: async () => ({ id: WEBHOOK_ID, secret: "whsec_fixture" }),
      webhookRotateSecret: async () => ({ id: WEBHOOK_ID, secret: "whsec_rotated" }),
      webhookTest: async () => ({ ok: true, statusCode: 204, durationMs: 1, error: null }),
    } as unknown as BrowserNetwork;
  }

  it.each([
    ["webhook_create", { input: validInput }, true],
    ["webhook_update", { id: WEBHOOK_ID, input: validInput }, true],
    ["webhook_delete", { id: WEBHOOK_ID }, true],
    ["webhook_rotate_secret", { id: WEBHOOK_ID }, true],
    ["webhook_reveal_secret", { id: WEBHOOK_ID }, false],
    ["webhook_test", { id: WEBHOOK_ID }, false],
    ["webhooks_list", {}, false],
  ])("notifies for %s only when it changes stored state (%s)", async (command, args, changes) => {
    const session = new RestoredSession();
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: webhookNetwork() });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "unlock", args: { passphrase: "vault phrase" } });
    await responseFor(attached, 1);
    const before = attached.messages.length;

    attached.emit({ id: 2, command, args });

    expect(await responseFor(attached, 2)).toMatchObject({ ok: true });
    expect(attached.messages.slice(before).filter(message => JSON.stringify(message) === changedEvent)).toHaveLength(changes ? 1 : 0);
  });
});
