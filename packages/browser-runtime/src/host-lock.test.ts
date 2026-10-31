import { describe, expect, it, vi } from "vitest";
import { browserHostDependencies, BrowserWorkerHost, type BrowserPort } from "./host.js";
import type { OwnerLockManager } from "./owner.js";

const origin = "https://peppy.test";
const invitationId = "00000000-0000-0000-0000-000000000003";

const hoisted = vi.hoisted(() => ({
  invoke: (async () => ({})) as (request: { command: string; args: Record<string, unknown> }) => Promise<unknown>,
  session: undefined as unknown,
}));

vi.mock("./core.js", async importOriginal => {
  const actual = await importOriginal<typeof import("./core.js")>();
  return {
    ...actual,
    BrowserCore: class {
      public invoke(request: { command: string; args: Record<string, unknown> }): Promise<unknown> { return hoisted.invoke(request); }
    },
  };
});

vi.mock("./session.js", async importOriginal => {
  const actual = await importOriginal<typeof import("./session.js")>();
  return { ...actual, BrowserSession: { boot: async () => hoisted.session } };
});

vi.mock("/core/peppy-browser-core.js", () => ({ default: async () => ({ FS: {} }) }));

function locks(): OwnerLockManager {
  let held = false;
  return {
    async request(name, _options, operation) {
      if (held) return operation(null);
      held = true;
      try { return await operation({ name }); } finally { held = false; }
    },
  };
}

async function eventually(predicate: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 50; attempt += 1) {
    if (predicate()) return;
    await new Promise(resolve => setTimeout(resolve, 0));
  }
  throw new Error("condition was not met");
}

function port(): BrowserPort & { messages: unknown[]; emit(message: unknown): void; closed: boolean } {
  const result = {
    messages: [] as unknown[], closed: false, onmessage: null as BrowserPort["onmessage"],
    postMessage(message: unknown) { result.messages.push(message); },
    close() { result.closed = true; },
    emit(message: unknown) { result.onmessage?.({ data: message } as MessageEvent); },
  };
  return result;
}

function json(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), { status, headers: { "content-type": "application/json" } });
}

function ownerSession() {
  const session = {
    phase: "ready" as "ready" | "closed",
    publicIdentity: () => ({ role: "owner" as const }),
    tokenForTransport: () => "a".repeat(96),
    query: async (command: string) => command === "_worker_attachment_retention_capability_request" ? { method: "PUT", relativePath: "/v1/devices/self/retention-capability", jsonBody: "{}", protocolHeader: null, expectedSuccessStatus: 204 } : {},
    mutate: async () => ({}),
    unlock: async () => undefined,
    enroll: async () => undefined,
    finishEnrollment: async () => undefined,
    shutdown() { session.phase = "closed"; },
    drain: async () => undefined,
  };
  return session;
}

describe("browser host enrollment lock", () => {
  it("aborts an in-flight owner status poll on lock without a remote DELETE or unhandled rejection", async () => {
    const methods: string[] = [];
    let statusSignal: AbortSignal | undefined;
    let terminated = false;
    const rejections: unknown[] = [];
    const onRejection = (reason: unknown) => { rejections.push(reason); };
    const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(String(input), origin);
      const method = init?.method ?? "GET";
      methods.push(method);
      if (url.pathname === "/web/config.json") return new Response(null, { status: 404 });
      if (url.pathname === "/hosted/v1/enrollment/config") return json({}, 404);
      if (url.pathname === "/v1/instance") return json({ mode: "community", claimed: true, setup_required: false, password_auth: true, qr_pairing: true, shared_secret_pairing: true });
      if (url.pathname === "/v1/devices/self/retention-capability") return json({}, 404);
      if (url.pathname === "/v1/enrollment/invitations" && method === "POST") return json({ invitation_id: invitationId, expires_in_seconds: 300 });
      if (url.pathname === `/v1/enrollment/invitations/${invitationId}` && method === "GET") {
        statusSignal = init?.signal ?? undefined;
        return new Promise<Response>(() => undefined);
      }
      if (url.pathname === `/v1/enrollment/invitations/${invitationId}` && method === "DELETE") return new Response(null, { status: 204 });
      throw new Error(`unexpected ${method} ${url.pathname}`);
    });
    hoisted.invoke = async ({ command }) => {
      if (command === "_worker_attachment_retention_capability_request") return { method: "PUT", relativePath: "/v1/devices/self/retention-capability", jsonBody: "{}", protocolHeader: null, expectedSuccessStatus: 204 };
      if (command === "_worker_enrollment_create_phone_invitation") return { request: JSON.stringify({ method: "transfer", transfer_public_key: "key", invitation_token_digest: "digest" }) };
      if (command === "_worker_enrollment_get_phone_qr") return { qr: "local-secret-qr" };
      if (command === "_worker_enrollment_pending_vault_request") return null;
      if (command === "_worker_enrollment_gateway_presence") return {};
      return {};
    };
    hoisted.session = ownerSession();

    vi.stubGlobal("fetch", fetcher);
    vi.stubGlobal("self", { location: { origin } });
    process.on("unhandledRejection", onRejection);
    try {
      const host = new BrowserWorkerHost(browserHostDependencies(locks(), () => { terminated = true; }));
      const attached = port();
      await host.attach(attached);

      attached.emit({ id: 1, command: "enrollment_add_phone", args: { method: "transfer" } });
      await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
      attached.emit({ id: 2, command: "enrollment_status", args: {} });
      await eventually(() => statusSignal !== undefined);
      attached.emit({ id: 3, command: "lock_sync", args: {} });
      await eventually(() => terminated);
      await new Promise(resolve => setTimeout(resolve, 0));

      expect(attached.messages).toContainEqual(expect.objectContaining({ id: 1, ok: true, value: expect.objectContaining({ stage: "waiting_phone" }) }));
      expect(methods).not.toContain("DELETE");
      expect(statusSignal?.aborted).toBe(true);
      expect(rejections).toEqual([]);
    } finally {
      process.off("unhandledRejection", onRejection);
      vi.unstubAllGlobals();
    }
  });
});
