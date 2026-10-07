import { describe, expect, it, vi } from "vitest";
import { BrowserWorkerHost, dispatchBrowserRpc, type BrowserPort, type HostSession } from "./host.js";
import type { OwnerLockManager } from "./owner.js";

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

class FakeSession implements HostSession {
  public phase: "unenrolled" | "locked" | "ready" | "closed" = "ready";
  public readonly calls: Array<{ command: string; args: Record<string, unknown>; mutation: boolean }> = [];
  public snapshot: Record<string, unknown> = {};
  public async query(command: string, args: Record<string, unknown>) {
    this.calls.push({ command, args, mutation: false });
    if (command === "contact_snapshot" && !Array.isArray(args.addresses)) throw { code: "invalid-request" };
    return command === "snapshot" ? this.snapshot : { command };
  }
  public async mutate(command: string, args: Record<string, unknown>) { this.calls.push({ command, args, mutation: true }); return { command }; }
  public async unlock(_passphrase: string) { this.phase = "ready"; }
  public async enroll(_metadata: unknown, _deviceToken: string, _passphrase: string) { this.phase = "ready"; }
  public shutdown() { this.phase = "closed"; }
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

describe("browser host RPC", () => {
  it("does not forward private core commands", async () => {
    const session = new FakeSession();
    await expect(dispatchBrowserRpc(session, { id: 1, command: "_worker_transport_token", args: {} }))
      .resolves.toMatchObject({ ok: false, error: { code: "invalid-request" } });
    expect(session.calls).toEqual([]);
  });

  it.each([
    ["unenrolled", "preview"],
    ["locked", "locked"],
    ["closed", "locked"],
  ] as const)("returns a secret-free safe snapshot for the %s phase without calling core", async (phase, encryptionState) => {
    const session = new FakeSession();
    session.phase = phase;
    const response = await dispatchBrowserRpc(session, { id: 1, command: "load_state", args: { passphrase: "do-not-expose" } });
    expect(response).toEqual({
      id: 1,
      ok: true,
      value: {
        version: "1",
        mode: "browser",
        connection: { state: "offline" },
        encryption: { state: encryptionState },
        gateways: [],
        conversations: [],
        head: { enabled: false, capability: "unsupported" },
        pendingCount: 0,
        quarantineCount: 0,
        notifications: [],
        appFilters: [],
        notificationPreferences: { messageBanners: true, mirroredBanners: true, preview: "full" },
      },
    });
    expect(JSON.stringify(response)).not.toContain("do-not-expose");
    expect(session.calls).toEqual([]);
  });

  it("maps load_state to snapshot and mutations to checkpointed session calls", async () => {
    const session = new FakeSession();
    await dispatchBrowserRpc(session, { id: 1, command: "load_state", args: { conversationId: "c" } });
    await dispatchBrowserRpc(session, { id: 2, command: "save_draft", args: { input: { text: "hello" } } });
    expect(session.calls).toEqual([
      { command: "snapshot", args: { conversationId: "c" }, mutation: false },
      { command: "contact_snapshot", args: { addresses: [] }, mutation: false },
      { command: "save_draft", args: { text: "hello" }, mutation: true },
    ]);
  });

  it("uses the Rust contact_snapshot address contract after loading the snapshot", async () => {
    const session = new FakeSession();
    session.snapshot = {
      conversations: [{ name: "Aurora", participants: ["+15551234567", "Aurora"] }],
      draft: { recipientIds: ["+15557654321"], gatewayId: "gateway-1" },
      notifications: [{ title: "Morgan", target: { sourceDeviceId: "phone-1" } }],
    };
    await expect(dispatchBrowserRpc(session, { id: 1, command: "load_state", args: {} })).resolves.toMatchObject({ ok: true });
    expect(session.calls).toEqual([
      { command: "snapshot", args: {}, mutation: false },
      { command: "contact_snapshot", args: { addresses: [
        { address: "Aurora" },
        { address: "+15551234567" },
        { address: "+15557654321", sourceDeviceId: "gateway-1" },
        { address: "Morgan", sourceDeviceId: "phone-1" },
      ] }, mutation: false },
    ]);
  });

  it("preserves every message and attachment while bounding optional image preview work", async () => {
    const session = new FakeSession();
    const attachments = Array.from({ length: 33 }, (_, index) => ({ id: `image-${index}`, mediaType: "image/png", state: "ready" }));
    session.snapshot = { activeConversationId: "conversation", conversations: [{ id: "conversation", messages: attachments.map((attachment, index) => ({ id: `message-${index}`, attachments: [attachment] })) }] };
    let previews = 0;
    (session as HostSession).previewAttachment = async () => { previews += 1; return "data:image/png;base64,cHJldmlldw=="; };
    const response = await dispatchBrowserRpc(session, { id: 1, command: "load_state", args: {} });
    expect(response).toMatchObject({ ok: true });
    if (!response.ok) throw new Error("expected state");
    const value = response.value as { conversations: Array<{ messages: Array<{ attachments: unknown[] }> }> };
    expect(value.conversations[0].messages).toHaveLength(33);
    expect(value.conversations[0].messages.every(message => message.attachments)).toBe(true);
    expect(previews).toBe(32);
  });

  it("attaches two ports to one session and broadcasts only after a mutation", async () => {
    const session = new FakeSession();
    let boots = 0;
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => { boots++; return session; } });
    const first = port();
    const second = port();
    await Promise.all([host.attach(first), host.attach(second)]);
    first.emit({ id: 1, command: "save_draft", args: { input: {} } });
    await eventually(() => second.messages.some(message => typeof message === "object" && message !== null && "event" in message && message.event === "changed"));
    expect(boots).toBe(1);
    expect(second.messages).toContainEqual({ event: "changed" });
  });

  it("locks all ports and terminates the worker without restoring a live filesystem", async () => {
    const session = new FakeSession();
    let terminated = false;
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, terminate: () => { terminated = true; } });
    const first = port();
    const second = port();
    await host.attach(first);
    await host.attach(second);
    first.emit({ id: 3, command: "lock_sync", args: {} });
    await eventually(() => terminated);
    expect(session.phase).toBe("closed");
    expect(first.closed).toBe(true);
    expect(second.closed).toBe(true);
    expect(terminated).toBe(true);
  });

  it("stops all ports after a fatal session failure", async () => {
    const session = new FakeSession();
    let reportFatal!: (error: Error) => void;
    let terminated = false;
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async (_signal, onFatal) => { reportFatal = onFatal; return session; },
      terminate: () => { terminated = true; },
    });
    const attached = port();
    await host.attach(attached);
    reportFatal(new Error("trap"));
    await eventually(() => terminated);
    expect(attached.closed).toBe(true);
    expect(terminated).toBe(true);
  });

  it("overlays a pending epoch mismatch and routes a ready-session passphrase to manual rotation", async () => {
    const session = new FakeSession();
    const unlockNewEpoch = vi.fn(async () => undefined);
    const network = {
      start: async () => undefined, stop: async () => undefined, joinStart: async () => ({ state: "idle" as const }), joinPoll: async () => ({ state: "idle" as const }), joinCancel: () => undefined, joinConfirm: async () => ({ state: "idle" as const }),
      epochStatus: () => "needs-unlock" as const, unlockNewEpoch,
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "load_state", args: {} });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    expect(attached.messages).toContainEqual(expect.objectContaining({ id: 1, ok: true, value: expect.objectContaining({ encryption: { state: "mismatch" } }) }));
    expect(session.phase).toBe("ready");
    attached.emit({ id: 2, command: "unlock", args: { passphrase: "manual-passphrase" } });
    await eventually(() => unlockNewEpoch.mock.calls.length === 1);
    expect(unlockNewEpoch).toHaveBeenCalledWith("manual-passphrase");
    expect(session.phase).toBe("ready");
  });

  it("replies to a join cancellation before terminally stopping a closed session", async () => {
    const session = new FakeSession();
    const network = {
      start: async () => undefined, stop: async () => undefined, joinStart: async () => ({ state: "idle" as const }), joinPoll: async () => ({ state: "idle" as const }),
      joinCancel: () => { session.phase = "closed"; }, joinConfirm: async () => ({ state: "idle" as const }),
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "join_cancel", args: {} });
    await eventually(() => attached.closed);
    const reply = attached.messages.findIndex(message => typeof message === "object" && message !== null && "id" in message && message.id === 1);
    const stopped = attached.messages.findIndex(message => typeof message === "object" && message !== null && "event" in message && message.event === "stopped");
    expect(reply).toBeGreaterThanOrEqual(0);
    expect(stopped).toBeGreaterThan(reply);
  });

  it("fails closed when Web Locks are unavailable", async () => {
    const attached = port();
    await new BrowserWorkerHost({ boot: async () => new FakeSession() }).attach(attached);
    expect(attached.messages).toContainEqual({ event: "stopped", reason: "unavailable" });
  });

  it("unwraps DesktopBridge argument objects into Rust's flat command shapes", async () => {
    const session = new FakeSession();
    await dispatchBrowserRpc(session, { id: 1, command: "save_draft", args: { input: { text: "draft" } } });
    await dispatchBrowserRpc(session, { id: 2, command: "set_notification_preferences", args: { preferences: { preview: "hidden" } } });
    expect(session.calls).toEqual([
      { command: "save_draft", args: { text: "draft" }, mutation: true },
      { command: "set_notification_preferences", args: { preview: "hidden" }, mutation: true },
    ]);
  });

  it("does not report notification preferences saved when Worker settings storage fails", async () => {
    const session = new FakeSession();
    const response = await dispatchBrowserRpc(session, { id: 1, command: "set_notification_preferences", args: { preferences: { preview: "hidden" } } }, {
      load: async () => undefined,
      save: async () => { throw new Error("indexeddb unavailable"); },
    });
    expect(response).toMatchObject({ ok: false, error: { code: "unavailable" } });
    expect(session.calls).toEqual([]);
  });

  it("echoes a valid id for an oversized malformed request", async () => {
    const response = await dispatchBrowserRpc(new FakeSession(), { id: 7, command: "load_state", args: { value: "x".repeat(1024 * 1024) } });
    expect(response).toMatchObject({ id: 7, ok: false, error: { code: "invalid-request" } });
  });
});
