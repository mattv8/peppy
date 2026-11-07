import { describe, expect, it, vi } from "vitest";
import { LiveSupervisor, LiveTypingEngine, type LiveSocket } from "./live.js";
import { OriginTransport } from "./transport.js";

const ticket = "A".repeat(43);
const handle = "A".repeat(11);

class FakeSocket implements LiveSocket {
  public protocol = "";
  public readyState: number = WebSocket.CONNECTING;
  public onopen: ((event: Event) => void) | null = null;
  public onmessage: ((event: MessageEvent<unknown>) => void) | null = null;
  public onclose: ((event: CloseEvent) => void) | null = null;
  public onerror: ((event: Event) => void) | null = null;
  public sent: string[] = [];
  public close = vi.fn(() => { this.readyState = WebSocket.CLOSED; });
  public send = vi.fn((frame: string) => { this.sent.push(frame); });
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function transport(fetch: typeof globalThis.fetch): OriginTransport {
  return new OriginTransport({ origin: "https://app.peppy.test", deviceToken: () => "a".repeat(96), fetch });
}

function authorization(request: RequestInit | undefined): string | null {
  const headers = request?.headers;
  return headers instanceof Headers ? headers.get("Authorization") : null;
}

async function settled(): Promise<void> {
  for (let index = 0; index < 12; index += 1) await Promise.resolve();
}

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(accept => { resolve = accept; });
  return { promise, resolve };
}

describe("LiveSupervisor", () => {
  it("requests a private ticket, offers it only as a subprotocol, and rejects an echo mismatch", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>().mockResolvedValue(json({ ticket, expires_at_ms: 1, lifetime_handle: handle }, 201));
    const socket = new FakeSocket();
    const supervisor = new LiveSupervisor({ transport: transport(fetch), origin: "https://app.peppy.test", socket: (_url, protocol) => { socket.protocol = "wrong"; return socket; }, schedule: vi.fn() });
    supervisor.start();
    await settled();
    socket.onopen?.(new Event("open"));
    await settled();
    expect(fetch.mock.calls[0]?.[0]).toBe("https://app.peppy.test/v1/live/tickets");
    expect((fetch.mock.calls[0]?.[1] as RequestInit).headers).toBeInstanceOf(Headers);
    expect(authorization(fetch.mock.calls[0]?.[1] as RequestInit)).toBe(`Bearer ${"a".repeat(96)}`);
    expect(socket.close).toHaveBeenCalledOnce();
    expect(socket.protocol).not.toContain("/v1/live?");
  });

  it("retires a terminal session and retries issuance with a fresh private id", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>()
      .mockResolvedValueOnce(json({ code: "live_ticket_session_terminal" }, 409))
      .mockResolvedValueOnce(json({ ticket, expires_at_ms: 1, lifetime_handle: handle }, 201));
    const socket = new FakeSocket();
    const supervisor = new LiveSupervisor({ transport: transport(fetch), origin: "https://app.peppy.test", socket: (_url, protocol) => { socket.protocol = protocol; return socket; }, schedule: vi.fn() });
    supervisor.start();
    await settled();
    const first = JSON.parse(String(fetch.mock.calls[0]?.[1]?.body)) as { session_id: string };
    const second = JSON.parse(String(fetch.mock.calls[1]?.[1]?.body)) as { session_id: string };
    expect(second.session_id).not.toBe(first.session_id);
  });

  it("closes and deletes before the owner can clear its transport token", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>().mockResolvedValue(new Response(null, { status: 204 }));
    const supervisor = new LiveSupervisor({ transport: transport(fetch), origin: "https://app.peppy.test", socket: () => new FakeSocket(), schedule: vi.fn() });
    supervisor.start();
    await settled();
    await supervisor.stop();
    expect(fetch.mock.calls.at(-1)?.[0]).toBe("https://app.peppy.test/v1/live/tickets");
    expect(fetch.mock.calls.at(-1)?.[1]).toMatchObject({ method: "DELETE" });
    const request = fetch.mock.calls.at(-1)?.[1] as RequestInit;
    expect(authorization(request)).toBe(`Bearer ${"a".repeat(96)}`);
  });

  it("rejects a hello with unsupported limits and never posts ticket material to a renderer", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>().mockResolvedValue(json({ ticket, expires_at_ms: 1, lifetime_handle: handle }, 201));
    const received = vi.fn();
    const socket = new FakeSocket();
    const supervisor = new LiveSupervisor({ transport: transport(fetch), origin: "https://app.peppy.test", socket: (_url, protocol) => { socket.protocol = protocol; return socket; }, onFrame: received, schedule: vi.fn() });
    supervisor.start();
    await settled();
    socket.onopen?.(new Event("open"));
    socket.onmessage?.(new MessageEvent("message", { data: JSON.stringify({ type: "hello", protocol_version: 2, max_ttl_ms: 15_000, max_ciphertext_bytes: 5_399 }) }));
    socket.onmessage?.(new MessageEvent("message", { data: "opaque frame" }));
    expect(socket.close).toHaveBeenCalledOnce();
    expect(received).not.toHaveBeenCalled();
  });
});

describe("LiveTypingEngine", () => {
  it("uses core sharing admission and rejects a stale sealed generation at transmit", async () => {
    const mutate = vi.fn()
      .mockResolvedValueOnce({ generation: "7" })
      .mockResolvedValueOnce({})
      .mockResolvedValueOnce({ generation: "8", frameJson: "sealed" });
    const transmit = vi.fn(() => true);
    const engine = new LiveTypingEngine({ mutate }, { transmit }, () => ({ wallMs: 1, monoMs: 1 }), vi.fn());
    await engine.enable({ trustedRoster: ["a"], localGatewayDeviceId: "a", executor: "none" });
    await engine.setSharing(false);
    await expect(engine.send("conversation", "gateway", "stopped")).resolves.toBe(false);
    expect(mutate.mock.calls[1]).toEqual(["_worker_typing_set_sharing", { sharing: false }]);
    expect(transmit).not.toHaveBeenCalled();
  });

  it("publishes only a sanitized display outcome and drops stale inbound generations", async () => {
    const display = vi.fn();
    const mutate = vi.fn()
      .mockResolvedValueOnce({ generation: "7" })
      .mockResolvedValueOnce({ generation: "6", action: { kind: "display", peerDeviceId: "peer", conversationId: "conversation", state: "started", deadlineMono: "9" } })
      .mockResolvedValueOnce({ generation: "7", sessionUpdate: { peerDeviceId: "peer", replacedSessionToken: null, currentSessionToken: "1", validationRevision: "1" }, action: { kind: "display", peerDeviceId: "peer", sessionToken: "1", sequence: "1", conversationId: "conversation", state: "started", deadlineMono: "9" } });
    const engine = new LiveTypingEngine({ mutate }, { transmit: vi.fn(() => true) }, () => ({ wallMs: 1, monoMs: 1 }), display);
    await engine.enable({ trustedRoster: ["a"], localGatewayDeviceId: "a", executor: "none" });
    await engine.receive("raw frame");
    await engine.receive("raw frame");
    expect(display).toHaveBeenCalledExactlyOnceWith({ event: "typing", peerDeviceId: "peer", conversationId: "conversation", state: "started", deadlineMono: "9" });
  });

  it("does not reinstall a delayed enable after disable", async () => {
    const pendingEnable = deferred<unknown>();
    const mutate = vi.fn((command: string) => {
      if (command === "_worker_typing_enable") return pendingEnable.promise;
      if (command === "_worker_typing_seal") return Promise.resolve({ generation: "7", frameJson: "sealed" });
      return Promise.resolve({});
    });
    const transmit = vi.fn(() => true);
    const engine = new LiveTypingEngine({ mutate }, { transmit }, () => ({ wallMs: 1, monoMs: 1 }), vi.fn());

    const enabling = engine.enable({ trustedRoster: ["a"], localGatewayDeviceId: "a", executor: "none" });
    await settled();
    await engine.disable();
    pendingEnable.resolve({ generation: "7" });
    await enabling;

    await expect(engine.send("conversation", "gateway", "started")).resolves.toBe(false);
    expect(mutate).toHaveBeenCalledTimes(2);
    expect(transmit).not.toHaveBeenCalled();
  });

  it("discards a pending seal completion after disable", async () => {
    const pendingSeal = deferred<unknown>();
    const mutate = vi.fn((command: string) => {
      if (command === "_worker_typing_enable") return Promise.resolve({ generation: "7" });
      if (command === "_worker_typing_seal") return pendingSeal.promise;
      return Promise.resolve({});
    });
    const transmit = vi.fn(() => true);
    const engine = new LiveTypingEngine({ mutate }, { transmit }, () => ({ wallMs: 1, monoMs: 1 }), vi.fn());
    await engine.enable({ trustedRoster: ["a"], localGatewayDeviceId: "a", executor: "none" });

    const sending = engine.send("conversation", "gateway", "started");
    await settled();
    await engine.disable();
    pendingSeal.resolve({ generation: "7", frameJson: "sealed" });

    await expect(sending).resolves.toBe(false);
    expect(transmit).not.toHaveBeenCalled();
  });

  it("discards a pending inbound completion after disable", async () => {
    const pendingOpen = deferred<unknown>();
    const mutate = vi.fn((command: string) => {
      if (command === "_worker_typing_enable") return Promise.resolve({ generation: "7" });
      if (command === "_worker_typing_open") return pendingOpen.promise;
      return Promise.resolve({});
    });
    const display = vi.fn();
    const transmit = vi.fn(() => true);
    const engine = new LiveTypingEngine({ mutate }, { transmit }, () => ({ wallMs: 1, monoMs: 1 }), display);
    await engine.enable({ trustedRoster: ["a"], localGatewayDeviceId: "a", executor: "none" });

    const receiving = engine.receive("raw frame");
    await settled();
    await engine.disable();
    pendingOpen.resolve({
      generation: "7",
      sessionUpdate: { peerDeviceId: "peer", replacedSessionToken: null, currentSessionToken: "1", validationRevision: "1" },
      action: { kind: "display", peerDeviceId: "peer", sessionToken: "1", sequence: "1", conversationId: "conversation", state: "started", deadlineMono: "9" },
      replyFrameJson: "reply",
    });
    await receiving;

    expect(display).not.toHaveBeenCalled();
    expect(transmit).not.toHaveBeenCalled();
  });

  it("rejects a delayed action from a replaced peer session", async () => {
    const delayedAction = deferred<unknown>();
    const mutate = vi.fn()
      .mockResolvedValueOnce({ generation: "7" })
      .mockResolvedValueOnce({ generation: "7", sessionUpdate: { peerDeviceId: "peer", replacedSessionToken: null, currentSessionToken: "1", validationRevision: "1" } })
      .mockImplementationOnce(() => delayedAction.promise)
      .mockResolvedValueOnce({ generation: "7", sessionUpdate: { peerDeviceId: "peer", replacedSessionToken: "1", currentSessionToken: "2", validationRevision: "2" } });
    const display = vi.fn();
    const engine = new LiveTypingEngine({ mutate }, { transmit: vi.fn(() => true) }, () => ({ wallMs: 1, monoMs: 1 }), display);
    await engine.enable({ trustedRoster: ["a"], localGatewayDeviceId: "a", executor: "none" });
    await engine.receive("install session 1");

    const delayed = engine.receive("session 1 action");
    await settled();
    await engine.receive("install session 2");
    delayedAction.resolve({ generation: "7", action: { kind: "display", peerDeviceId: "peer", sessionToken: "1", sequence: "2", conversationId: "conversation", state: "started", deadlineMono: "9" } });
    await delayed;

    expect(display).not.toHaveBeenCalled();
  });

  it("rejects a delayed same-session start after a newer stop", async () => {
    const delayedStart = deferred<unknown>();
    const mutate = vi.fn()
      .mockResolvedValueOnce({ generation: "7" })
      .mockResolvedValueOnce({ generation: "7", sessionUpdate: { peerDeviceId: "peer", replacedSessionToken: null, currentSessionToken: "1", validationRevision: "1" } })
      .mockImplementationOnce(() => delayedStart.promise)
      .mockResolvedValueOnce({ generation: "7", action: { kind: "display", peerDeviceId: "peer", sessionToken: "1", sequence: "103", conversationId: "conversation", state: "stopped", deadlineMono: "9" } });
    const display = vi.fn();
    const engine = new LiveTypingEngine({ mutate }, { transmit: vi.fn(() => true) }, () => ({ wallMs: 1, monoMs: 1 }), display);
    await engine.enable({ trustedRoster: ["a"], localGatewayDeviceId: "a", executor: "none" });
    await engine.receive("install session");

    const delayed = engine.receive("start 102");
    await settled();
    await engine.receive("stop 103");
    delayedStart.resolve({ generation: "7", action: { kind: "display", peerDeviceId: "peer", sessionToken: "1", sequence: "102", conversationId: "conversation", state: "started", deadlineMono: "9" } });
    await delayed;

    expect(display).toHaveBeenCalledExactlyOnceWith({ event: "typing", peerDeviceId: "peer", conversationId: "conversation", state: "stopped", deadlineMono: "9" });
  });

  it("rejects an action whose deadline expires while core work is pending", async () => {
    let monoMs = 1;
    const pendingOpen = deferred<unknown>();
    const mutate = vi.fn()
      .mockResolvedValueOnce({ generation: "7" })
      .mockResolvedValueOnce({ generation: "7", sessionUpdate: { peerDeviceId: "peer", replacedSessionToken: null, currentSessionToken: "1", validationRevision: "1" } })
      .mockImplementationOnce(() => pendingOpen.promise);
    const display = vi.fn();
    const engine = new LiveTypingEngine({ mutate }, { transmit: vi.fn(() => true) }, () => ({ wallMs: 1, monoMs }), display);
    await engine.enable({ trustedRoster: ["a"], localGatewayDeviceId: "a", executor: "none" });
    await engine.receive("install session");

    const receiving = engine.receive("pending action");
    await settled();
    monoMs = 9;
    pendingOpen.resolve({ generation: "7", action: { kind: "display", peerDeviceId: "peer", sessionToken: "1", sequence: "1", conversationId: "conversation", state: "started", deadlineMono: "9" } });
    await receiving;

    expect(display).not.toHaveBeenCalled();
  });

  it("ignores an older peer-session revision instead of rolling the token backward", async () => {
    const display = vi.fn();
    const mutate = vi.fn()
      .mockResolvedValueOnce({ generation: "7" })
      .mockResolvedValueOnce({ generation: "7", sessionUpdate: { peerDeviceId: "peer", replacedSessionToken: null, currentSessionToken: "2", validationRevision: "2" } })
      .mockResolvedValueOnce({
        generation: "7",
        sessionUpdate: { peerDeviceId: "peer", replacedSessionToken: null, currentSessionToken: "1", validationRevision: "1" },
        action: { kind: "display", peerDeviceId: "peer", sessionToken: "1", sequence: "1", conversationId: "conversation", state: "started", deadlineMono: "9" },
      });
    const engine = new LiveTypingEngine({ mutate }, { transmit: vi.fn(() => true) }, () => ({ wallMs: 1, monoMs: 1 }), display);
    await engine.enable({ trustedRoster: ["a"], localGatewayDeviceId: "a", executor: "none" });
    await engine.receive("newer session");
    await engine.receive("older session");

    expect(display).not.toHaveBeenCalled();
  });

  it("resets conversation sequence fences when the peer session token changes", async () => {
    const display = vi.fn();
    const mutate = vi.fn()
      .mockResolvedValueOnce({ generation: "7" })
      .mockResolvedValueOnce({
        generation: "7",
        sessionUpdate: { peerDeviceId: "peer", replacedSessionToken: null, currentSessionToken: "1", validationRevision: "1" },
        action: { kind: "display", peerDeviceId: "peer", sessionToken: "1", sequence: "100", conversationId: "conversation", state: "started", deadlineMono: "9" },
      })
      .mockResolvedValueOnce({
        generation: "7",
        sessionUpdate: { peerDeviceId: "peer", replacedSessionToken: "1", currentSessionToken: "2", validationRevision: "2" },
        action: { kind: "display", peerDeviceId: "peer", sessionToken: "2", sequence: "1", conversationId: "conversation", state: "stopped", deadlineMono: "9" },
      });
    const engine = new LiveTypingEngine({ mutate }, { transmit: vi.fn(() => true) }, () => ({ wallMs: 1, monoMs: 1 }), display);
    await engine.enable({ trustedRoster: ["a"], localGatewayDeviceId: "a", executor: "none" });
    await engine.receive("session 1");
    await engine.receive("session 2");

    expect(display).toHaveBeenCalledTimes(2);
  });

});
