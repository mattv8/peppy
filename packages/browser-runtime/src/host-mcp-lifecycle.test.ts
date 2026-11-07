import { afterEach, describe, expect, it, vi } from "vitest";
import { BrowserEnrollment } from "./enrollment.js";
import { BrowserWorkerHost, type BrowserNetwork, type BrowserPort, type HostSession } from "./host.js";
import type { OwnerLockManager } from "./owner.js";
import { OriginTransport } from "./transport.js";

const ORIGIN = "https://peppy.test";
const VAULT_ID = "00000000-0000-4000-8000-000000000001";
const DEVICE_ID = "00000000-0000-4000-8000-000000000002";
const DEVICE_TOKEN = "a".repeat(96);

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
