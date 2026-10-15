import { describe, expect, it, vi } from "vitest";
import { CoreRejectedError } from "./core.js";
import { BrowserWorkerHost, dispatchBrowserRpc, type BrowserPort, type HostSession } from "./host.js";
import type { OwnerLockManager } from "./owner.js";
import { TransportError } from "./transport.js";

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
  public previewAttachment?: (id: string) => Promise<string | undefined>;
  public async query(command: string, args: Record<string, unknown>) {
    this.calls.push({ command, args, mutation: false });
    if (command === "contact_snapshot" && !Array.isArray(args.addresses)) throw { code: "invalid-request" };
    if (command === "_worker_identity_metadata") return { deviceId: "00000000-0000-0000-0000-000000000001" };
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
  it("does not forward private worker commands", async () => {
    const session = new FakeSession();
    for (const command of ["_worker_transport_token", "_worker_enrollment_finish_owner"]) {
      await expect(dispatchBrowserRpc(session, { id: 1, command, args: {} }))
        .resolves.toMatchObject({ ok: false, error: { code: "invalid-request" } });
    }
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

  it("maps load_state to snapshot and draft mutations to checkpointed session calls", async () => {
    const session = new FakeSession();
    await dispatchBrowserRpc(session, { id: 1, command: "load_state", args: { conversationId: "c" } });
    await dispatchBrowserRpc(session, { id: 2, command: "save_draft", args: { input: { text: "hello" } } });
    await dispatchBrowserRpc(session, { id: 3, command: "discard_draft", args: { input: { id: "draft", conversationId: "conversation", expectedRevision: "1" } } });
    expect(session.calls).toEqual([
      { command: "snapshot", args: { conversationId: "c" }, mutation: false },
      { command: "contact_snapshot", args: { addresses: [] }, mutation: false },
      { command: "save_draft", args: { text: "hello" }, mutation: true },
      { command: "discard_draft", args: { id: "draft", conversationId: "conversation", expectedRevision: "1" }, mutation: true },
    ]);
  });

  it("checkpoints scan requests but reads scan status without a mutation", async () => {
    const session = new FakeSession();
    await dispatchBrowserRpc(session, { id: 1, command: "request_contact_sync", args: { bookId: "book" } });
    await dispatchBrowserRpc(session, { id: 2, command: "get_contact_sync_request", args: { bookId: "book" } });
    expect(session.calls).toEqual([
      { command: "request_contact_sync", args: { bookId: "book" }, mutation: true },
      { command: "get_contact_sync_request", args: { bookId: "book" }, mutation: false },
    ]);
  });

  it("does not broadcast a change for a scan status read, but wakes after an enqueued request", async () => {
    const session = new FakeSession();
    const wake = vi.fn();
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      network: { start: async () => undefined, stop: async () => undefined, wake } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "get_contact_sync_request", args: { bookId: "book" } });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    expect(attached.messages).not.toContainEqual({ event: "changed" });
    expect(wake).not.toHaveBeenCalled();

    attached.emit({ id: 2, command: "request_contact_sync", args: { bookId: "book" } });
    await eventually(() => wake.mock.calls.length === 1);
    expect(attached.messages).toContainEqual({ event: "changed" });
  });

  it("forwards history pages as a read-only core request", async () => {
    const session = new FakeSession();
    await expect(dispatchBrowserRpc(session, {
      id: 1,
      command: "load_history",
      args: { conversationId: "conversation", beforeMessageId: "message" },
    })).resolves.toMatchObject({ id: 1, ok: true });
    expect(session.calls).toEqual([{
      command: "load_history",
      args: { conversationId: "conversation", beforeMessageId: "message" },
      mutation: false,
    }]);
  });

  it("does not query history from a locked browser session", async () => {
    const session = new FakeSession();
    session.phase = "locked";
    await expect(dispatchBrowserRpc(session, {
      id: 1,
      command: "load_history",
      args: { conversationId: "conversation", beforeMessageId: "message" },
    })).resolves.toMatchObject({ ok: false, error: { code: "locked" } });
    expect(session.calls).toEqual([]);
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

  it("preserves sanctioned draft error details through the browser RPC", async () => {
    const session = new FakeSession();
    session.mutate = async () => { throw new CoreRejectedError("draft-moved", { conversationId: "conversation-2", currentRevision: "2", draftId: "draft-2" }); };
    await expect(dispatchBrowserRpc(session, { id: 1, command: "save_draft", args: { input: {} } })).resolves.toEqual({
      id: 1,
      ok: false,
      error: { code: "draft-moved", message: "Browser operation failed.", conversationId: "conversation-2", currentRevision: "2", draftId: "draft-2" },
    });
  });

  it("does not expose malformed draft error details through the browser RPC", async () => {
    const session = new FakeSession();
    session.mutate = async () => { throw new CoreRejectedError("draft-moved", { conversationId: "conversation\n2", currentRevision: "01", draftId: "x".repeat(129) }); };
    await expect(dispatchBrowserRpc(session, { id: 1, command: "save_draft", args: { input: {} } })).resolves.toEqual({
      id: 1,
      ok: false,
      error: { code: "draft-moved", message: "Browser operation failed." },
    });
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

  it("uses the smaller preview budget for an older history page", async () => {
    const session = new FakeSession();
    const attachments = Array.from({ length: 13 }, (_, index) => ({ id: `image-${index}`, mediaType: "image/png", state: "ready" }));
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      return command === "load_history" ? { messages: attachments.map((attachment, index) => ({ id: `message-${index}`, attachments: [attachment] })) } : { command };
    };
    let previews = 0;
    session.previewAttachment = async () => { previews += 1; return "data:image/png;base64,cHJldmlldw=="; };
    await expect(dispatchBrowserRpc(session, { id: 1, command: "load_history", args: { conversationId: "conversation", beforeMessageId: "message" } })).resolves.toMatchObject({ ok: true });
    expect(previews).toBe(12);
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

  it("broadcasts draft deletion after its checkpointed mutation succeeds", async () => {
    const session = new FakeSession();
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const first = port();
    const second = port();
    await Promise.all([host.attach(first), host.attach(second)]);
    first.emit({ id: 1, command: "discard_draft", args: { input: { id: "draft", conversationId: "conversation", expectedRevision: "1" } } });
    await eventually(() => second.messages.some(message => typeof message === "object" && message !== null && "event" in message && message.event === "changed"));
    expect(session.calls).toContainEqual({ command: "discard_draft", args: { id: "draft", conversationId: "conversation", expectedRevision: "1" }, mutation: true });
  });

  it("notifies other windows after a persisted draft move rejection", async () => {
    const session = new FakeSession();
    session.mutate = async (command, args) => {
      session.calls.push({ command, args, mutation: true });
      throw new CoreRejectedError("draft-moved", { conversationId: "conversation-2", currentRevision: "2", draftId: "draft-2" });
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const first = port();
    const second = port();
    await Promise.all([host.attach(first), host.attach(second)]);
    first.emit({ id: 1, command: "send_draft", args: { input: {} } });
    await eventually(() => second.messages.some(message => typeof message === "object" && message !== null && "event" in message && message.event === "changed"));
    expect(first.messages).toContainEqual(expect.objectContaining({ id: 1, ok: false, error: expect.objectContaining({ code: "draft-moved" }) }));
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
      start: async () => undefined, stop: async () => undefined,
      epochStatus: () => "needs-unlock" as const, unlockNewEpoch,
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: network as unknown as import("./host.js").BrowserNetwork });
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

  it.each([
    "enrollment_discover",
    "enrollment_status",
    "enrollment_generate_secret",
    "enrollment_management",
  ])("does not broadcast or start sync after the read-only %s request", async command => {
    const session = new FakeSession();
    const start = vi.fn(async () => undefined);
    const network = {
      start,
      stop: async () => undefined,
      enrollmentDiscover: async () => ({ mode: "community" as const, stage: "ready" as const }),
      enrollmentStatus: async () => ({ mode: "community" as const, stage: "ready" as const }),
      enrollmentGenerateSecret: async () => "generated-passphrase",
      enrollmentManagement: async () => ({ vault_id: "00000000-0000-0000-0000-000000000001", devices: [], storage_bytes: 0, quota_bytes: 1, replay_retention_days: 30 }),
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: network as unknown as import("./host.js").BrowserNetwork });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command, args: {} });

    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    expect(attached.messages).not.toContainEqual({ event: "changed" });
    expect(start).not.toHaveBeenCalled();
  });

  it("lets a subscribed UI reload enrollment and app state once after an enrollment mutation", async () => {
    const session = new FakeSession();
    session.snapshot = { version: "1", connection: { state: "connected" }, conversations: [] };
    const network = {
      start: async () => undefined,
      stop: async () => undefined,
      enrollmentAddPhone: async () => ({ mode: "community" as const, stage: "waiting_phone" as const }),
      enrollmentStatus: async () => ({ mode: "community" as const, stage: "waiting_phone" as const }),
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: network as unknown as import("./host.js").BrowserNetwork });
    const attached = port();
    const originalPostMessage = attached.postMessage.bind(attached);
    let changedEvents = 0;
    attached.postMessage = message => {
      originalPostMessage(message);
      if (typeof message !== "object" || message === null || !("event" in message) || message.event !== "changed") return;
      changedEvents += 1;
      if (changedEvents <= 5) {
        attached.emit({ id: 100 + changedEvents * 2, command: "enrollment_status", args: {} });
        attached.emit({ id: 101 + changedEvents * 2, command: "load_state", args: {} });
      }
    };
    await host.attach(attached);

    attached.emit({ id: 1, command: "enrollment_add_phone", args: { method: "shared_secret" } });

    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 103));
    expect(changedEvents).toBe(1);
    expect(attached.messages).toContainEqual({
      id: 103,
      ok: true,
      value: expect.objectContaining({ version: "1", connection: { state: "connected" } }),
    });
  });

  it("routes approval without accepting or forwarding a passphrase", async () => {
    const session = new FakeSession();
    const enrollmentApprove = vi.fn(async () => ({ mode: "community" as const, stage: "waiting_phone" as const }));
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      network: { start: async () => undefined, stop: async () => undefined, enrollmentApprove } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "enrollment_approve", args: { passphrase: "must-not-reach-network" } });

    await eventually(() => enrollmentApprove.mock.calls.length === 1);
    expect(enrollmentApprove).toHaveBeenCalledWith();
  });

  it("validates and forwards waiting-only enrollment cancellation", async () => {
    const session = new FakeSession();
    const enrollmentCancel = vi.fn(async (options?: { waitingOnly?: boolean }) => options?.waitingOnly ? { mode: "community" as const, stage: "verify_phone" as const } : undefined);
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      network: { start: async () => undefined, stop: async () => undefined, enrollmentCancel } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "enrollment_cancel", args: { waitingOnly: true } });
    attached.emit({ id: 2, command: "enrollment_cancel", args: { waitingOnly: "true" } });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 2));

    expect(enrollmentCancel).toHaveBeenCalledWith({ waitingOnly: true });
    expect(attached.messages).toContainEqual({ id: 1, ok: true, value: { mode: "community", stage: "verify_phone" } });
    expect(attached.messages).toContainEqual({ id: 2, ok: false, error: expect.objectContaining({ code: "invalid-request" }) });
  });

  it.each([
    ["enrollment_create_owner", { passphrase: "manual-passphrase" }],
    ["enrollment_connect_shared_secret", { passphrase: "manual-passphrase" }],
    ["enrollment_create_vault", { username: "taylor", adminPassword: "password-password", passphrase: "manual-passphrase" }],
  ])("starts sync when %s activates a ready session", async (command, args) => {
    const session = new FakeSession();
    session.phase = "unenrolled";
    const start = vi.fn(async () => undefined);
    const activate = async () => {
      session.phase = "ready";
      return { mode: "community" as const, stage: "ready" as const };
    };
    const network = {
      start,
      stop: async () => undefined,
      enrollmentCreateOwner: activate,
      enrollmentConnectSharedSecret: activate,
      enrollmentCreateVault: activate,
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: network as unknown as import("./host.js").BrowserNetwork });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command, args });

    await eventually(() => start.mock.calls.length === 1);
    expect(attached.messages).toContainEqual({ event: "changed" });
  });

  it("does not start sync when an activating enrollment mutation returns an error view", async () => {
    const session = new FakeSession();
    session.phase = "unenrolled";
    const start = vi.fn(async () => undefined);
    const network = {
      start,
      stop: async () => undefined,
      enrollmentCreateOwner: async () => {
        session.phase = "ready";
        return { mode: "community" as const, stage: "error" as const, errorCode: "activation-failed" };
      },
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: network as unknown as import("./host.js").BrowserNetwork });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "enrollment_create_owner", args: { passphrase: "manual-passphrase" } });

    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    expect(attached.messages).toContainEqual(expect.objectContaining({ id: 1, ok: true, value: expect.objectContaining({ stage: "error" }) }));
    expect(start).not.toHaveBeenCalled();
  });

  it("does not start sync or broadcast when an activating enrollment mutation fails", async () => {
    const session = new FakeSession();
    session.phase = "unenrolled";
    const start = vi.fn(async () => undefined);
    const network = {
      start,
      stop: async () => undefined,
      enrollmentCreateOwner: async () => { throw new Error("failed activation"); },
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: network as unknown as import("./host.js").BrowserNetwork });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "enrollment_create_owner", args: { passphrase: "manual-passphrase" } });

    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    expect(attached.messages).toContainEqual(expect.objectContaining({ id: 1, ok: false }));
    expect(attached.messages).not.toContainEqual({ event: "changed" });
    expect(start).not.toHaveBeenCalled();
  });

  it("replies to enrollment cancellation before terminally stopping a closed session", async () => {
    const session = new FakeSession();
    const network = {
      start: async () => undefined, stop: async () => undefined,
      enrollmentCancel: async () => { session.phase = "closed"; },
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: network as unknown as import("./host.js").BrowserNetwork });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "enrollment_cancel", args: {} });
    await eventually(() => attached.closed);
    const reply = attached.messages.findIndex(message => typeof message === "object" && message !== null && "id" in message && message.id === 1);
    const stopped = attached.messages.findIndex(message => typeof message === "object" && message !== null && "event" in message && message.event === "stopped");
    expect(reply).toBeGreaterThanOrEqual(0);
    expect(stopped).toBeGreaterThan(reply);
  });

  it("routes management RPCs only through the browser network adapter", async () => {
    const session = new FakeSession();
    const enrollmentManagement = vi.fn(async () => ({ vault_id: "00000000-0000-0000-0000-000000000001", devices: [], storage_bytes: 0, quota_bytes: 1, replay_retention_days: 30 }));
    const enrollmentRevokeDevice = vi.fn(async () => ({ vault_id: "00000000-0000-0000-0000-000000000001", devices: [], storage_bytes: 0, quota_bytes: 1, replay_retention_days: 30 }));
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: { start: async () => undefined, stop: async () => undefined, enrollmentManagement, enrollmentRevokeDevice } as unknown as import("./host.js").BrowserNetwork });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "enrollment_management", args: {} });
    attached.emit({ id: 2, command: "enrollment_revoke_device", args: { deviceId: "00000000-0000-0000-0000-000000000002" } });
    await eventually(() => enrollmentRevokeDevice.mock.calls.length === 1);
    expect(enrollmentManagement).toHaveBeenCalledOnce();
    expect(enrollmentRevokeDevice).toHaveBeenCalledWith("00000000-0000-0000-0000-000000000002");
    expect(session.calls).toEqual([]);
  });

  it("routes roster reads and revocation through the device-authenticated network adapter", async () => {
    const session = new FakeSession();
    const listDevices = vi.fn(async () => ({ devices: [] }));
    const revokeDevice = vi.fn(async () => undefined);
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      network: { start: async () => undefined, stop: async () => undefined, listDevices, revokeDevice } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "list_devices", args: {} });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    expect(attached.messages).toContainEqual({ id: 1, ok: true, value: { currentDeviceId: "00000000-0000-0000-0000-000000000001", devices: [] } });
    attached.emit({ id: 2, command: "revoke_device", args: { deviceId: "00000000-0000-0000-0000-000000000002" } });
    await eventually(() => revokeDevice.mock.calls.length === 1);
    expect(listDevices).toHaveBeenCalledOnce();
    expect(revokeDevice).toHaveBeenCalledWith("00000000-0000-0000-0000-000000000002");
    expect(session.calls).toEqual([{ command: "_worker_identity_metadata", args: {}, mutation: false }]);
  });

  it("forwards optional raw roster presence without requiring older servers to provide it", async () => {
    const session = new FakeSession();
    const host = new BrowserWorkerHost({
      locks: locks(), boot: async () => session,
      network: { start: async () => undefined, stop: async () => undefined, listDevices: async () => ({ devices: [{ device_id: "00000000-0000-0000-0000-000000000002", name: "Pixel 8", kind: "android", role: "owner", revoked: false, presence: { state: "seen", last_seen_age_seconds: 300 }, wake_route: true }] }) } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "list_devices", args: {} });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    expect(attached.messages).toContainEqual({ id: 1, ok: true, value: { currentDeviceId: "00000000-0000-0000-0000-000000000001", devices: [{ id: "00000000-0000-0000-0000-000000000002", name: "Pixel 8", kind: "android", role: "owner", revoked: false, presence: { state: "seen", last_seen_age_seconds: 300 }, wakeRoute: true }] } });
  });

  it("drops an unrecognized optional presence state while retaining the roster", async () => {
    const session = new FakeSession();
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: { start: async () => undefined, stop: async () => undefined, listDevices: async () => ({ devices: [{ device_id: "00000000-0000-0000-0000-000000000002", name: "Pixel 8", kind: "android", role: "owner", revoked: false, presence: { state: "future", last_seen_age_seconds: 1 } }] }) } as unknown as import("./host.js").BrowserNetwork });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "list_devices", args: {} });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    expect(attached.messages).toContainEqual({ id: 1, ok: true, value: { currentDeviceId: "00000000-0000-0000-0000-000000000001", devices: [{ id: "00000000-0000-0000-0000-000000000002", name: "Pixel 8", kind: "android", role: "owner", revoked: false }] } });
  });

  it("preserves device-management server codes without changing unrelated transport failures", async () => {
    const session = new FakeSession();
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      network: {
        start: async () => undefined,
        stop: async () => undefined,
        listDevices: async () => { throw new TransportError("revoked"); },
      } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "list_devices", args: {} });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    expect(attached.messages).toContainEqual({ id: 1, ok: false, error: expect.objectContaining({ code: "invalid_bearer" }) });

    session.query = async () => { throw new TransportError("revoked"); };
    const response = await dispatchBrowserRpc(session, { id: 2, command: "list_contact_books", args: {} });
    expect(response).toMatchObject({ ok: false, error: { code: "unavailable" } });
  });

  it("fails closed when Web Locks are unavailable", async () => {
    const attached = port();
    await new BrowserWorkerHost({ boot: async () => new FakeSession() }).attach(attached);
    expect(attached.messages).toContainEqual({ event: "stopped", reason: "unavailable" });
  });

  it("unwraps DesktopBridge draft argument objects into Rust's flat command shapes", async () => {
    const session = new FakeSession();
    await dispatchBrowserRpc(session, { id: 1, command: "save_draft", args: { input: { text: "draft" } } });
    await dispatchBrowserRpc(session, { id: 2, command: "set_notification_preferences", args: { preferences: { preview: "hidden" } } });
    await dispatchBrowserRpc(session, { id: 3, command: "discard_draft", args: { input: { id: "draft", conversationId: "conversation", expectedRevision: "1" } } });
    expect(session.calls).toEqual([
      { command: "save_draft", args: { text: "draft" }, mutation: true },
      { command: "set_notification_preferences", args: { preview: "hidden" }, mutation: true },
      { command: "discard_draft", args: { id: "draft", conversationId: "conversation", expectedRevision: "1" }, mutation: true },
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
