import { afterEach, describe, expect, it, vi } from "vitest";
import { BrowserCoreError, CoreRejectedError } from "./core.js";
import { browserRememberedUnlock, BrowserWorkerHost, dispatchBrowserRpc, type BrowserPort, type HostSession } from "./host.js";
import type { OwnerLockManager } from "./owner.js";
import { TransportError } from "./transport.js";
import { RememberedUnlock } from "./remembered-unlock.js";

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

function deferred(): { promise: Promise<void>; release(): void } {
  let release!: () => void;
  return { promise: new Promise<void>(resolve => { release = resolve; }), release };
}

class FakeSession implements HostSession {
  public phase: "unenrolled" | "locked" | "ready" | "closed" = "ready";
  public generation = 0;
  public readonly calls: Array<{ command: string; args: Record<string, unknown>; mutation: boolean }> = [];
  public snapshot: Record<string, unknown> = {};
  public previewAttachment?: (id: string) => Promise<string | undefined>;
  public exportAttachment?: (id: string) => Promise<Record<string, unknown> & { bytes: Uint8Array }>;
  public openMediaAttachment?: (id: string, maxBytes: number) => Promise<Record<string, unknown> & { bytes: Uint8Array }>;
  public clearRememberedUnlock?: () => Promise<void>;
  public validateRememberedUnlock?: () => Promise<void>;
  public async query(command: string, args: Record<string, unknown>): Promise<import("./core.js").CoreResult> {
    this.calls.push({ command, args, mutation: false });
    if (command === "contact_snapshot" && !Array.isArray(args.addresses)) throw { code: "invalid-request" };
    if (command === "_worker_identity_metadata") return { vaultId: "00000000-0000-4000-8000-000000000001", deviceId: "00000000-0000-0000-0000-000000000001" };
    return command === "snapshot" ? this.snapshot : { command };
  }
  public async mutate(command: string, args: Record<string, unknown>): Promise<import("./core.js").CoreResult> { this.calls.push({ command, args, mutation: true }); this.generation += 1; return { command }; }
  public async unlock(_passphrase: string, _signal?: AbortSignal, _options?: { remember?: boolean }) { this.phase = "ready"; }
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
  afterEach(() => { vi.useRealTimers(); });

  it("constructs worker recovery against the app origin when the logical API origin differs", async () => {
    const fetch = vi.fn(async () => new Response(null, { status: 503 })) as unknown as typeof globalThis.fetch;
    const origins = { logicalOrigin: "https://api.peppy.pro", fetchBase: "https://app.peppy.pro" };
    const recovery = browserRememberedUnlock(origins.fetchBase, fetch);

    expect(recovery).toBeInstanceOf(RememberedUnlock);
    await recovery.remember(JSON.stringify({ metadata: { vaultId: "11111111-1111-7111-8111-111111111111" } }), "passphrase");
    expect(fetch).toHaveBeenCalledWith("https://app.peppy.pro/hosted/v1/web/app-session", expect.objectContaining({ credentials: "include" }));
  });

  it.each(["locked", "unenrolled"] as const)("does not start networking for a %s boot", async phase => {
    const session = new FakeSession();
    session.phase = phase;
    const start = vi.fn(async () => undefined);
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: { start, stop: async () => undefined } as unknown as import("./host.js").BrowserNetwork });

    await host.attach(port());

    expect(start).not.toHaveBeenCalled();
  });

  it("does not admit renderer producers before retention negotiation completes", async () => {
    const session = new FakeSession();
    const gate = deferred();
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: { start: () => gate.promise, stop: async () => undefined } as unknown as import("./host.js").BrowserNetwork });
    const attached = port();
    const pending = host.attach(attached);

    await Promise.resolve();
    expect(attached.messages).not.toContainEqual({ event: "ready", version: 1 });
    expect(attached.onmessage).toBeNull();
    gate.release();
    await pending;
    expect(attached.messages).toContainEqual({ event: "ready", version: 1 });
  });

  it("starts networking once for an already-ready recovered boot", async () => {
    const session = new FakeSession();
    const start = vi.fn(async () => undefined);
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: { start, stop: async () => undefined } as unknown as import("./host.js").BrowserNetwork });

    await Promise.all([host.attach(port()), host.attach(port())]);

    expect(start).toHaveBeenCalledTimes(1);
  });

  it("releases a ready owner after boot negotiation failure so a retry can start", async () => {
    const ownerLocks = locks();
    const failedPort = port();
    const failed = new BrowserWorkerHost({ locks: ownerLocks, boot: async () => new FakeSession(), network: { start: async () => { throw new TransportError("offline"); }, stop: async () => undefined } as unknown as import("./host.js").BrowserNetwork });
    await failed.attach(failedPort);
    expect(failedPort.closed).toBe(true);

    const retryPort = port();
    const retry = new BrowserWorkerHost({ locks: ownerLocks, boot: async () => new FakeSession(), network: { start: async () => true, stop: async () => undefined } as unknown as import("./host.js").BrowserNetwork });
    await retry.attach(retryPort);
    expect(retryPort.messages).toContainEqual({ event: "ready", version: 1 });
  });

  it("gates special attachment producers during the unlock negotiation window", async () => {
    const session = new FakeSession();
    session.phase = "locked";
    session.unlock = async () => { session.phase = "ready"; };
    const gate = deferred();
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: { start: () => gate.promise, stop: async () => undefined, producerReady: () => false } as unknown as import("./host.js").BrowserNetwork });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "unlock", args: { passphrase: "secret" } });
    await vi.waitFor(() => expect(session.phase).toBe("ready"));
    attached.emit({ id: 2, command: "prepare_attachment", args: { bytes: new ArrayBuffer(1), displayName: "a", mediaType: "image/png" } });
    attached.emit({ id: 3, command: "publish_attachment", args: { id: "00000000-0000-0000-0000-000000000001", confirmed: true } });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 3));

    expect(attached.messages).toContainEqual({ id: 2, ok: false, error: expect.objectContaining({ code: "unavailable" }) });
    expect(attached.messages).toContainEqual({ id: 3, ok: false, error: expect.objectContaining({ code: "unavailable" }) });
    expect(session.calls).toEqual([]);
    gate.release();
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
  });

  it("keeps offline-safe mutations available while retention negotiation is unresolved", async () => {
    const session = new FakeSession();
    const host = new BrowserWorkerHost({
      locks: locks(), boot: async () => session,
      network: { start: async () => false, stop: async () => undefined, producerReady: () => false } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "save_draft", args: { input: { text: "offline" } } });
    attached.emit({ id: 2, command: "mark_read_batch", args: { conversationId: "c", throughSequence: "1" } });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 2));

    expect(attached.messages).toContainEqual(expect.objectContaining({ id: 1, ok: true }));
    expect(attached.messages).toContainEqual(expect.objectContaining({ id: 2, ok: true }));
    expect(session.calls).toContainEqual(expect.objectContaining({ command: "save_draft", mutation: true }));
    expect(session.calls).toContainEqual(expect.objectContaining({ command: "mark_read_batch", mutation: true }));
  });

  it("responds and stops cleanly when unlock negotiation is revoked", async () => {
    const session = new FakeSession();
    session.phase = "locked";
    session.unlock = async () => { session.phase = "ready"; };
    const clearRememberedUnlock = vi.fn(async () => undefined);
    session.clearRememberedUnlock = clearRememberedUnlock;
    const stop = vi.fn(async () => undefined);
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: { start: async () => { throw new TransportError("revoked"); }, stop } as unknown as import("./host.js").BrowserNetwork });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "unlock", args: { passphrase: "secret", remember: true } });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));

    expect(attached.messages).toContainEqual({ id: 1, ok: false, error: expect.objectContaining({ code: "unavailable" }) });
    await eventually(() => attached.closed);
    expect(stop).toHaveBeenCalledTimes(1);
    expect(clearRememberedUnlock).toHaveBeenCalledOnce();
  });

  it("forgets remembered recovery when a ready enrollment activation is revoked", async () => {
    const session = new FakeSession();
    const clearRememberedUnlock = vi.fn(async () => undefined);
    session.clearRememberedUnlock = clearRememberedUnlock;
    const start = vi.fn()
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(new TransportError("revoked"));
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      network: {
        start,
        stop: async () => undefined,
        enrollmentCreateOwner: async () => ({ mode: "community" as const, stage: "ready" as const }),
      } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "enrollment_create_owner", args: { passphrase: "phrase" } });

    await eventually(() => attached.closed);
    expect(attached.messages).toContainEqual({ id: 1, ok: false, error: expect.objectContaining({ code: "unavailable" }) });
    expect(clearRememberedUnlock).toHaveBeenCalledOnce();
  });

  it("clears recovery and releases a recovered owner when initial networking is revoked", async () => {
    const ownerLocks = locks();
    const session = new FakeSession();
    const clearRememberedUnlock = vi.fn(async () => undefined);
    session.clearRememberedUnlock = clearRememberedUnlock;
    const failedPort = port();
    const failed = new BrowserWorkerHost({
      locks: ownerLocks,
      boot: async () => session,
      network: { start: async () => { throw new TransportError("revoked"); }, stop: async () => undefined } as unknown as import("./host.js").BrowserNetwork,
    });

    await failed.attach(failedPort);

    expect(clearRememberedUnlock).toHaveBeenCalledOnce();
    expect(session.phase).toBe("closed");
    expect(failedPort.messages).toContainEqual({ id: -1, ok: false, error: expect.objectContaining({ code: "unavailable" }) });

    const retryPort = port();
    const retry = new BrowserWorkerHost({ locks: ownerLocks, boot: async () => new FakeSession(), network: { start: async () => undefined, stop: async () => undefined } as unknown as import("./host.js").BrowserNetwork });
    await retry.attach(retryPort);
    expect(retryPort.messages).toContainEqual({ event: "ready", version: 1 });
  });

  it("does not forward private worker commands", async () => {
    const session = new FakeSession();
    for (const command of ["_worker_transport_token", "_worker_enrollment_finish_owner", "_worker_attachment_retention_prepare", "_worker_attachment_retention_capability_request"]) {
      await expect(dispatchBrowserRpc(session, { id: 1, command, args: {} }))
        .resolves.toMatchObject({ ok: false, error: { code: "invalid-request" } });
    }
    expect(session.calls).toEqual([]);
  });

  it("validates the retention presentation facade before invoking network methods", async () => {
    const session = new FakeSession();
    const acknowledge = vi.fn(async () => "acknowledged" as const);
    const host = new BrowserWorkerHost({
      locks: locks(), boot: async () => session,
      network: { start: async () => undefined, stop: async () => undefined, acknowledgeAttachmentRetentionNotice: acknowledge } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "acknowledge_attachment_retention_notice", args: { scopeId: "", policyVersion: "hosted_attachment_retention_v1" } });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));

    expect(attached.messages).toContainEqual({ id: 1, ok: false, error: expect.objectContaining({ code: "invalid-request" }) });
    expect(acknowledge).not.toHaveBeenCalled();
  });

  it("passes remembered unlock only for an explicit true RPC argument", async () => {
    const session = new FakeSession();
    const unlock = vi.spyOn(session, "unlock");

    await expect(dispatchBrowserRpc(session, { id: 1, command: "unlock", args: { passphrase: "phrase" } })).resolves.toMatchObject({ ok: true });
    await expect(dispatchBrowserRpc(session, { id: 2, command: "unlock", args: { passphrase: "phrase", remember: "true" } })).resolves.toMatchObject({ ok: true });
    await expect(dispatchBrowserRpc(session, { id: 3, command: "unlock", args: { passphrase: "phrase", remember: true } })).resolves.toMatchObject({ ok: true });

    expect(unlock).toHaveBeenNthCalledWith(1, "phrase", undefined, { remember: false });
    expect(unlock).toHaveBeenNthCalledWith(2, "phrase", undefined, { remember: false });
    expect(unlock).toHaveBeenNthCalledWith(3, "phrase", undefined, { remember: true });
  });

  it("keeps notification settings links native-only", async () => {
    const session = new FakeSession();
    await expect(dispatchBrowserRpc(session, { id: 1, command: "open_notification_settings", args: {} }))
      .resolves.toMatchObject({ ok: false, error: { code: "unsupported" } });
    expect(session.calls).toEqual([]);
  });

  it("keeps history attempts scoped to their owning port and makes status read-only", async () => {
    const session = new FakeSession();
    const attemptId = "018f2c5e-6a3b-7c4d-8e5f-0123456789ab";
    session.mutate = async (command, args) => {
      session.calls.push({ command, args, mutation: true });
      session.generation += 1;
      return { attemptId, state: command === "cancel_history_search" ? "cancelled" : "catchingUp" };
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const first = port();
    const second = port();
    await Promise.all([host.attach(first), host.attach(second)]);

    first.emit({ id: 1, command: "start_history_search", args: { operationId: attemptId } });
    await eventually(() => first.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    second.emit({ id: 2, command: "cancel_history_search", args: { attemptId } });
    first.emit({ id: 3, command: "get_history_search", args: {} });
    await eventually(() => second.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 2)
      && first.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 3));

    expect(second.messages).toContainEqual({ id: 2, ok: false, error: expect.objectContaining({ code: "invalid-request" }) });
    expect(session.calls).toContainEqual({ command: "get_history_search", args: { attemptId }, mutation: false });
    expect(session.calls.filter(call => call.command === "cancel_history_search")).toHaveLength(0);
  });

  it("rejects history renderer extras and never exposes worker-only proof commands", async () => {
    const session = new FakeSession();
    await expect(dispatchBrowserRpc(session, { id: 1, command: "_worker_history_search_work", args: {} }))
      .resolves.toMatchObject({ ok: false, error: { code: "invalid-request" } });
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 2, command: "start_history_search", args: { operationId: "018f2c5e-6a3b-7c4d-8e5f-0123456789ab", query: "private" } });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 2));
    expect(session.calls).not.toContainEqual(expect.objectContaining({ command: "start_history_search" }));
  });

  it("allows one bounded deferred-media batch as a checkpointed mutation", async () => {
    const session = new FakeSession();
    const attachmentIds = [
      "00000000-0000-0000-0000-000000000001",
      "00000000-0000-0000-0000-000000000002",
    ];

    await expect(dispatchBrowserRpc(session, { id: 1, command: "media_sync_request", args: { attachmentIds } }))
      .resolves.toMatchObject({ ok: true });
    await expect(dispatchBrowserRpc(session, { id: 2, command: "media_sync_request", args: { attachmentIds: [] } }))
      .resolves.toMatchObject({ ok: false, error: { code: "invalid-request" } });

    expect(session.calls).toEqual([
      { command: "media_sync_request", args: { attachmentIds }, mutation: true },
    ]);
  });

  it("routes conversation pinning through the checkpointed mutation allowlist", async () => {
    const session = new FakeSession();
    const args = { conversationId: "conversation", pinned: true };

    await expect(dispatchBrowserRpc(session, { id: 1, command: "set_conversation_pinned", args }))
      .resolves.toMatchObject({ ok: true });

    expect(session.calls).toEqual([{ command: "set_conversation_pinned", args, mutation: true }]);
  });

  it("routes tone reset through the checkpointed mutation allowlist", async () => {
    const session = new FakeSession();
    const args = { seed: "contact:+15555550100", paletteIndex: null };

    await expect(dispatchBrowserRpc(session, { id: 1, command: "set_conversation_tone", args }))
      .resolves.toMatchObject({ ok: true });

    expect(session.calls).toEqual([{ command: "set_conversation_tone", args, mutation: true }]);
  });

    it("checkpoints media demand and cache release instead of exposing generic core dispatch", async () => {
    const session = new FakeSession();
    const id = "00000000-0000-0000-0000-000000000001";
    await expect(dispatchBrowserRpc(session, { id: 1, command: "request_attachment_download", args: { id } }))
      .resolves.toMatchObject({ ok: true });
    await expect(dispatchBrowserRpc(session, { id: 2, command: "close_media", args: { id } }))
      .resolves.toMatchObject({ ok: true });
    expect(session.calls).toEqual([
      { command: "attachment_info", args: { id }, mutation: false },
      { command: "request_attachment_download", args: { id }, mutation: true },
      { command: "release_attachment_ciphertext", args: { id }, mutation: true },
    ]);
  });

  it("makes Save a durable demand while a remote original is pending", async () => {
    const session = new FakeSession();
    const id = "00000000-0000-0000-0000-000000000001";
    session.exportAttachment = async () => { throw new CoreRejectedError("media-pending"); };
    await expect(dispatchBrowserRpc(session, { id: 1, command: "save_attachment", args: { id } }))
      .resolves.toMatchObject({ ok: false, error: { code: "media-pending" } });
    expect(session.calls).toEqual([
      { command: "attachment_info", args: { id }, mutation: false },
      { command: "request_attachment_download", args: { id }, mutation: true },
    ]);
  });

  it("does not checkpoint a repeated demand that core already recorded", async () => {
    const session = new FakeSession();
    const id = "00000000-0000-0000-0000-000000000001";
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      return { downloadRequested: true };
    };
    await expect(dispatchBrowserRpc(session, { id: 1, command: "request_attachment_download", args: { id } }))
      .resolves.toMatchObject({ ok: true, value: { demanded: false } });
    expect(session.calls).toEqual([{ command: "attachment_info", args: { id }, mutation: false }]);
  });

  it("exports a verified local original through Save without demanding it again", async () => {
    const session = new FakeSession();
    const id = "00000000-0000-0000-0000-000000000001";
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      return command === "attachment_info" ? { state: "ready" } : { command };
    };
    session.exportAttachment = async () => ({ bytes: new Uint8Array([1]), displayName: "file.pdf", type: "application/pdf" });
    await expect(dispatchBrowserRpc(session, { id: 1, command: "save_attachment", args: { id } }))
      .resolves.toMatchObject({ ok: true, value: { displayName: "file.pdf" } });
    expect(session.calls).toEqual([]);
  });

  it("enriches a media page from its eager thumbnail without opening the original", async () => {
    const session = new FakeSession();
    const thumbnail = "00000000-0000-0000-0000-000000000002";
    session.previewAttachment = vi.fn(async () => "data:image/png;base64,cHJldmlldw==");
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      if (command === "media_page") return { items: [{ mediaMeta: { thumbnail: { id: thumbnail, mediaType: "image/png", state: "ready" } } }] };
      return { command };
    };
    const response = await dispatchBrowserRpc(session, { id: 1, command: "media_page", args: { conversationId: "00000000-0000-0000-0000-000000000001" } });
    expect(response).toMatchObject({ ok: true, value: { items: [{ mediaMeta: { thumbnail: { previewUrl: "data:image/png;base64,cHJldmlldw==" } } }] } });
    expect(session.previewAttachment).toHaveBeenCalledWith(thumbnail);
  });

  it.each(["load_state", "load_history"])("enriches v3 timeline thumbnails for %s without demanding the original", async command => {
    const session = new FakeSession();
    const thumbnail = "00000000-0000-0000-0000-000000000002";
    session.previewAttachment = vi.fn(async () => "data:image/png;base64,cHJldmlldw==");
    session.snapshot = { activeConversationId: "conversation", conversations: [{ id: "conversation", messages: [{ attachments: [{ id: "original", mediaType: "image/jpeg", state: "pending", mediaMeta: { thumbnail: { id: thumbnail, mediaType: "image/png", state: "ready" } } }] }] }] };
    session.query = async (coreCommand, args) => {
      session.calls.push({ command: coreCommand, args, mutation: false });
      if (coreCommand === "load_history") return { messages: (session.snapshot.conversations as Array<{ messages: unknown[] }>)[0].messages };
      return coreCommand === "snapshot" ? session.snapshot : { command: coreCommand };
    };
    const response = await dispatchBrowserRpc(session, { id: 1, command, args: command === "load_history" ? { conversationId: "conversation", beforeMessageId: "message" } : {} });
    expect(response).toMatchObject({ ok: true });
    expect(session.previewAttachment).toHaveBeenCalledWith(thumbnail);
    expect(session.previewAttachment).not.toHaveBeenCalledWith("original");
  });

  it.each([
    ["load_state", {}],
    ["load_history", { conversationId: "conversation", beforeMessageId: "message" }],
    ["load_message_window", { direction: "refresh", conversationId: "conversation", messageIds: [] }],
    ["media_page", { conversationId: "00000000-0000-0000-0000-000000000099" }],
    ["media_page_newer", {
      conversationId: "00000000-0000-0000-0000-000000000099",
      cursor: { sortAtMs: "1", localOrder: "1", messageId: "00000000-0000-0000-0000-000000000098", attachmentPosition: 0 },
    }],
    ["media_page_around", {
      conversationId: "00000000-0000-0000-0000-000000000099",
      attachmentId: "00000000-0000-0000-0000-000000000001",
      messageId: "00000000-0000-0000-0000-000000000098",
    }],
  ] as const)("enriches unique attachment retention state once on the %s projection", async (command, args) => {
    const session = new FakeSession();
    const removedId = "00000000-0000-0000-0000-000000000001";
    const localId = "00000000-0000-0000-0000-000000000002";
    const thumbnailId = "00000000-0000-0000-0000-000000000003";
    const thumbnail = { id: thumbnailId, name: "preview.png", mediaType: "image/png", byteSize: 3, state: "ready" };
    const removed = { id: removedId, name: "remote.jpg", mediaType: "image/jpeg", byteSize: 10, state: "pending", retryable: true };
    const local = { id: localId, name: "local.pdf", mediaType: "application/pdf", byteSize: 20, state: "ready", mediaHold: "held" };
    const removedMessageAttachment = { ...removed, mediaMeta: { thumbnail } };
    const localMessageAttachment = { ...local, mediaMeta: { thumbnail } };
    const messages = [{ id: "message-1", attachments: [removedMessageAttachment, removedMessageAttachment] }, { id: "message-2", attachments: [localMessageAttachment] }];
    const snapshot = { activeConversationId: "conversation", conversations: [{ id: "conversation", messages }] };
    const messagePage = { conversationId: "conversation", messages, hasMore: false, hasOlder: false, hasNewer: false };
    const mediaItems = [removed, removed, local].map((attachment, index) => ({ messageId: `message-${index}`, attachment, mediaMeta: { width: 10, thumbnail } }));
    const mediaPage = { items: mediaItems };
    const mediaAround = { target: mediaItems[0], newer: [mediaItems[1]], older: [mediaItems[2]] };
    const retentionQueries: string[] = [];
    session.previewAttachment = vi.fn(async () => "data:image/png;base64,cHJldmlldw==");
    session.query = async (coreCommand, queryArgs) => {
      session.calls.push({ command: coreCommand, args: queryArgs, mutation: false });
      if (coreCommand === "snapshot") return snapshot;
      if (coreCommand === "contact_snapshot") return {};
      if (coreCommand === "load_history" || coreCommand === "load_message_window") return messagePage;
      if (coreCommand === "media_page" || coreCommand === "media_page_newer") return mediaPage;
      if (coreCommand === "media_page_around") return mediaAround;
      if (coreCommand === "attachment_retention_state") {
        const attachmentId = queryArgs.attachmentId as string;
        retentionQueries.push(attachmentId);
        return attachmentId === removedId
          ? { remote: "removed_by_storage_policy", localAvailable: false, deviceToken: "do-not-expose" }
          : { remote: "removed_by_storage_policy", localAvailable: true, deviceToken: "do-not-expose" };
      }
      throw new Error(coreCommand);
    };

    const response = await dispatchBrowserRpc(session, { id: 1, command, args });

    expect(response).toMatchObject({ ok: true });
    if (!response.ok) throw new Error("expected projection");
    const value = response.value as Record<string, unknown>;
    const messageProjection = command === "load_state" || command === "load_history" || command === "load_message_window";
    let projected: unknown[];
    let projectedThumbnail: unknown;
    if (command === "load_state") {
      projected = (value.conversations as Array<{ messages: Array<{ attachments: unknown[] }> }>)[0].messages.flatMap(message => message.attachments);
      projectedThumbnail = (projected[0] as { mediaMeta: { thumbnail: unknown } }).mediaMeta.thumbnail;
    } else if (command === "load_history" || command === "load_message_window") {
      projected = (value.messages as Array<{ attachments: unknown[] }>).flatMap(message => message.attachments);
      projectedThumbnail = (projected[0] as { mediaMeta: { thumbnail: unknown } }).mediaMeta.thumbnail;
    } else if (command === "media_page_around") {
      const target = value.target as { attachment: unknown; mediaMeta: { thumbnail: unknown } };
      projected = [target.attachment, ...(value.newer as Array<{ attachment: unknown }>).map(item => item.attachment), ...(value.older as Array<{ attachment: unknown }>).map(item => item.attachment)];
      projectedThumbnail = target.mediaMeta.thumbnail;
    } else {
      const items = value.items as Array<{ attachment: unknown; mediaMeta: { thumbnail: unknown } }>;
      projected = items.map(item => item.attachment);
      projectedThumbnail = items[0].mediaMeta.thumbnail;
    }
    const expectedRemoved = messageProjection ? removedMessageAttachment : removed;
    const expectedLocal = messageProjection ? localMessageAttachment : local;
    expect(projected[0]).toMatchObject({ ...expectedRemoved, remoteState: "removed_by_storage_policy", localAvailable: false });
    expect(projected[1]).toMatchObject({ ...expectedRemoved, remoteState: "removed_by_storage_policy", localAvailable: false });
    expect(projected[2]).toMatchObject({ ...expectedLocal, remoteState: "removed_by_storage_policy", localAvailable: true });
    expect(projectedThumbnail).toMatchObject({ ...thumbnail, previewUrl: "data:image/png;base64,cHJldmlldw==" });
    expect(session.previewAttachment).toHaveBeenCalledWith(thumbnailId);
    expect(session.previewAttachment).not.toHaveBeenCalledWith(removedId);
    expect(session.previewAttachment).not.toHaveBeenCalledWith(localId);
    expect(retentionQueries).toEqual([removedId, localId]);
    expect(session.calls.every(call => call.mutation === false)).toBe(true);
    expect(JSON.stringify(response)).not.toContain("do-not-expose");
  });

  it("preserves attachment descriptors and ordinary download behavior when retention state is unavailable", async () => {
    const session = new FakeSession();
    const attachment = { id: "00000000-0000-0000-0000-000000000001", name: "photo.jpg", mediaType: "image/jpeg", byteSize: 10, state: "pending", transfer: "download", retryable: true };
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      if (command === "load_history") return { messages: [{ attachments: [attachment] }] };
      if (command === "attachment_retention_state") throw new CoreRejectedError("unavailable");
      throw new Error(command);
    };

    const response = await dispatchBrowserRpc(session, { id: 1, command: "load_history", args: { conversationId: "conversation", beforeMessageId: "message" } });

    expect(response).toMatchObject({ ok: true, value: { messages: [{ attachments: [attachment] }] } });
    expect(JSON.stringify(response)).not.toContain("remoteState");
    expect(JSON.stringify(response)).not.toContain("localAvailable");
  });

  it("bounds retention enrichment to one hundred unique visible attachments", async () => {
    const session = new FakeSession();
    const attachments = Array.from({ length: 101 }, (_, index) => ({
      id: `00000000-0000-0000-0000-${String(index + 1).padStart(12, "0")}`,
      name: `file-${index}`,
      mediaType: "application/octet-stream",
      byteSize: index + 1,
      state: "pending",
    }));
    let retentionQueries = 0;
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      if (command === "load_history") return { messages: attachments.map(attachment => ({ attachments: [attachment] })) };
      if (command === "attachment_retention_state") { retentionQueries += 1; return { remote: "available", localAvailable: false }; }
      throw new Error(command);
    };

    const response = await dispatchBrowserRpc(session, { id: 1, command: "load_history", args: { conversationId: "conversation", beforeMessageId: "message" } });

    expect(response).toMatchObject({ ok: true });
    if (!response.ok) throw new Error("expected history");
    const messages = (response.value as { messages: Array<{ attachments: Array<Record<string, unknown>> }> }).messages;
    expect(retentionQueries).toBe(100);
    expect(messages[99].attachments[0]).toMatchObject({ remoteState: "available", localAvailable: false });
    expect(messages[100].attachments[0]).not.toHaveProperty("remoteState");
    expect(messages[100].attachments[0]).not.toHaveProperty("localAvailable");
  });

  it("uses the session's serialized media read instead of splitting availability and export", async () => {
    const session = new FakeSession();
    const id = "00000000-0000-0000-0000-000000000001";
    session.openMediaAttachment = vi.fn(async () => ({ bytes: new Uint8Array([1]), displayName: "image.jpg", type: "image/jpeg" }));
    const response = await dispatchBrowserRpc(session, { id: 1, command: "open_media", args: { id } });
    expect(response).toMatchObject({ ok: true, value: { state: "ready", byteSize: 1 } });
    expect(session.openMediaAttachment).toHaveBeenCalledWith(id, 32 * 1024 * 1024);
    expect(session.calls).toEqual([]);
  });

  it.each([{}, { metadataOnly: true }])("adds an optional session scope to %j load-state projections when host context supplies one", async args => {
    const session = new FakeSession();
    const response = await dispatchBrowserRpc(
      session,
      { id: 1, command: "load_state", args },
      undefined,
      undefined,
      undefined,
      undefined,
      { sessionScopeId: "018f2c5e-6a3b-7c4d-8e5f-0123456789ab" },
    );

    expect(response).toMatchObject({ ok: true, value: { sessionScopeId: "018f2c5e-6a3b-7c4d-8e5f-0123456789ab" } });
  });

  it("keeps the actual host session scope stable without status HTTP and rotates it when the device binding changes", async () => {
    const session = new FakeSession();
    const firstVaultId = "00000000-0000-4000-8000-000000000001";
    const firstDeviceId = "00000000-0000-0000-0000-000000000001";
    const nextVaultId = "00000000-0000-4000-8000-000000000002";
    const nextDeviceId = "00000000-0000-0000-0000-000000000002";
    let vaultId = firstVaultId;
    let deviceId = firstDeviceId;
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      if (command === "_worker_identity_metadata") return { vaultId, deviceId, deviceToken: "do-not-expose" };
      if (command === "snapshot") return session.snapshot;
      if (command === "contact_snapshot") return {};
      return { command };
    };
    const attachmentRetentionStatus = vi.fn(async () => ({ kind: "temporarily_unavailable" as const }));
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      network: { start: async () => true, stop: async () => undefined, attachmentRetentionStatus } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);
    const loadState = async (id: number, metadataOnly = false): Promise<Record<string, unknown>> => {
      attached.emit({ id, command: "load_state", args: metadataOnly ? { metadataOnly: true } : {} });
      await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === id));
      const response = attached.messages.find(message => typeof message === "object" && message !== null && "id" in message && message.id === id) as { ok: boolean; value: Record<string, unknown> };
      expect(response.ok).toBe(true);
      return response.value;
    };

    const first = await loadState(1);
    const second = await loadState(2, true);
    vaultId = nextVaultId;
    deviceId = nextDeviceId;
    const switched = await loadState(3);

    expect(first.sessionScopeId).toEqual(expect.any(String));
    expect(second.sessionScopeId).toBe(first.sessionScopeId);
    expect(switched.sessionScopeId).not.toBe(first.sessionScopeId);
    expect(attachmentRetentionStatus).not.toHaveBeenCalled();
    expect(session.calls.every(call => call.mutation === false)).toBe(true);
    const payload = JSON.stringify(attached.messages);
    for (const privateValue of [firstVaultId, firstDeviceId, nextVaultId, nextDeviceId, "do-not-expose"]) expect(payload).not.toContain(privateValue);
  });

  it("rotates the actual host session scope when the same device session is locked and reopened", async () => {
    const firstSession = new FakeSession();
    const firstHost = new BrowserWorkerHost({ locks: locks(), boot: async () => firstSession, network: { start: async () => true, stop: async () => undefined } as unknown as import("./host.js").BrowserNetwork });
    const firstPort = port();
    await firstHost.attach(firstPort);
    firstPort.emit({ id: 1, command: "load_state", args: {} });
    await eventually(() => firstPort.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    const firstResponse = firstPort.messages.find(message => typeof message === "object" && message !== null && "id" in message && message.id === 1) as { value: { sessionScopeId?: string } };
    firstPort.emit({ id: 2, command: "lock_sync", args: {} });
    await eventually(() => firstPort.closed);

    const reopenedSession = new FakeSession();
    const reopenedHost = new BrowserWorkerHost({ locks: locks(), boot: async () => reopenedSession, network: { start: async () => true, stop: async () => undefined } as unknown as import("./host.js").BrowserNetwork });
    const reopenedPort = port();
    await reopenedHost.attach(reopenedPort);
    reopenedPort.emit({ id: 3, command: "load_state", args: {} });
    await eventually(() => reopenedPort.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 3));
    const reopenedResponse = reopenedPort.messages.find(message => typeof message === "object" && message !== null && "id" in message && message.id === 3) as { value: { sessionScopeId?: string } };

    expect(firstResponse.value.sessionScopeId).toEqual(expect.any(String));
    expect(reopenedResponse.value.sessionScopeId).toEqual(expect.any(String));
    expect(reopenedResponse.value.sessionScopeId).not.toBe(firstResponse.value.sessionScopeId);
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
        notificationPreferences: { messageBanners: true, mirroredBanners: true, preview: "full", muteWithHeads: true },
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

  it("routes frozen unread batches as a query and each read batch as a mutation", async () => {
    const session = new FakeSession();

    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      return [["message-1"]];
    };
    session.mutate = async (command, args) => {
      session.calls.push({ command, args, mutation: true });
      session.generation += 1;
      return "2";
    };

    await expect(dispatchBrowserRpc(session, { id: 1, command: "unread_message_batches", args: {} }))
      .resolves.toEqual({ id: 1, ok: true, value: [["message-1"]] });
    await expect(dispatchBrowserRpc(session, { id: 2, command: "mark_read_batch", args: { messageIds: ["message-1"] } }))
      .resolves.toEqual({ id: 2, ok: true, value: "2" });
    expect(session.calls).toEqual([
      { command: "unread_message_batches", args: {}, mutation: false },
      { command: "mark_read_batch", args: { messageIds: ["message-1"] }, mutation: true },
    ]);
  });

  it("routes search and bounded history windows as reads and index advancement as a checkpointed mutation", async () => {
    const session = new FakeSession();
    await dispatchBrowserRpc(session, { id: 1, command: "search_local", args: { query: "hello" } });
    await dispatchBrowserRpc(session, { id: 2, command: "advance_search_index", args: { budgetRows: 100 } });
    await dispatchBrowserRpc(session, { id: 3, command: "load_message_window", args: { direction: "refresh", conversationId: "conversation", messageIds: [] } });
    expect(session.calls).toEqual([
      { command: "search_local", args: { query: "hello" }, mutation: false },
      { command: "advance_search_index", args: { budgetRows: 100 }, mutation: true },
      { command: "load_message_window", args: { direction: "refresh", conversationId: "conversation", messageIds: [] }, mutation: false },
    ]);
  });

  it("starts one checkpointed index batch after a locked session unlocks", async () => {
    vi.useFakeTimers();
    const session = new FakeSession();
    session.phase = "locked";
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      return command === "search_local" ? { indexState: "indexing" } : { command };
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "unlock", args: { passphrase: "passphrase" } });
    await vi.advanceTimersByTimeAsync(999);
    expect(session.calls.filter(call => call.command === "advance_search_index")).toHaveLength(0);
    await vi.advanceTimersByTimeAsync(1);
    expect(session.calls).toContainEqual({ command: "advance_search_index", args: {}, mutation: true });
    vi.useRealTimers();
  });

  it("coalesces index searches, broadcasts only ready completion, and stops after ready", async () => {
    vi.useFakeTimers();
    const session = new FakeSession();
    const states = ["indexing", "indexing", "ready"];
    session.mutate = async (command, args) => {
      session.calls.push({ command, args, mutation: true });
      session.generation += 1;
      return { indexState: states.shift() };
    };
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      return command === "search_local" ? { indexState: "indexing" } : { command };
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const attached = port();
    await host.attach(attached);
    for (let id = 1; id <= 10; id += 1) attached.emit({ id, command: "search_local", args: { query: "hello" } });
    await vi.advanceTimersByTimeAsync(3_000);
    expect(session.calls.filter(call => call.command === "advance_search_index")).toHaveLength(3);
    expect(attached.messages.filter(message => JSON.stringify(message) === JSON.stringify({ event: "changed" }))).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(session.calls.filter(call => call.command === "advance_search_index")).toHaveLength(3);
    vi.useRealTimers();
  });

  it("waits three times a slow batch duration after completion before the next batch", async () => {
    vi.useFakeTimers();
    const session = new FakeSession();
    let complete!: () => void;
    session.mutate = vi.fn(async (command, args) => {
      session.calls.push({ command, args, mutation: true });
      await new Promise<void>(resolve => { complete = resolve; });
      return { indexState: "indexing" };
    });
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const attached = port();
    await host.attach(attached);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(session.mutate).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(900);
    complete();
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(999);
    expect(session.mutate).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(4_700);
    expect(session.mutate).toHaveBeenCalledTimes(2);
    vi.useRealTimers();
  });

  it("rate-limits explicit index requests and cancels queued maintenance on lock", async () => {
    vi.useFakeTimers();
    const session = new FakeSession();
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "advance_search_index", args: {} });
    await Promise.resolve();
    expect(attached.messages).toContainEqual({ id: 1, ok: false, error: { code: "busy", message: "Search indexing is already scheduled." } });
    attached.emit({ id: 2, command: "lock_sync", args: {} });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(session.calls.filter(call => call.command === "advance_search_index")).toHaveLength(0);
    expect(vi.getTimerCount()).toBe(0);
    vi.useRealTimers();
  });

  it("answers explicit index requests from a locked session with locked rather than busy", async () => {
    const session = new FakeSession();
    session.phase = "locked";
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "advance_search_index", args: {} });
    await eventually(() => attached.messages.length > 1);
    expect(attached.messages).toContainEqual({ id: 1, ok: false, error: { code: "locked", message: "Search indexing is unavailable." } });
    expect(session.calls.filter(call => call.command === "advance_search_index")).toHaveLength(0);
  });

  it("does not checkpoint maintenance when an already-indexed session unlocks", async () => {
    vi.useFakeTimers();
    const session = new FakeSession();
    session.phase = "locked";
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      return command === "search_local" ? { indexState: "ready" } : { command };
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "unlock", args: { passphrase: "passphrase" } });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(session.calls.filter(call => call.command === "search_local")).toHaveLength(1);
    expect(session.calls.filter(call => call.command === "advance_search_index")).toHaveLength(0);
    vi.useRealTimers();
  });

  it("does not start a maintenance batch while a dispatched request is still in flight", async () => {
    vi.useFakeTimers();
    const session = new FakeSession();
    let releaseRead!: () => void;
    const readGate = new Promise<void>(resolve => { releaseRead = resolve; });
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      if (command === "snapshot") await readGate;
      return command === "snapshot" ? {} : { command };
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "load_state", args: {} });
    await vi.advanceTimersByTimeAsync(5_000);
    expect(session.calls.filter(call => call.command === "advance_search_index")).toHaveLength(0);
    releaseRead();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(session.calls.filter(call => call.command === "advance_search_index")).toHaveLength(1);
    vi.useRealTimers();
  });

  it("retries transient index failures but stops after a closed-session failure", async () => {
    vi.useFakeTimers();
    const transient = new FakeSession();
    let attempts = 0;
    transient.mutate = async (command, args) => {
      transient.calls.push({ command, args, mutation: true });
      attempts += 1;
      if (attempts === 1) throw new CoreRejectedError("unavailable");
      return { indexState: "ready" };
    };
    const transientHost = new BrowserWorkerHost({ locks: locks(), boot: async () => transient });
    await transientHost.attach(port());
    await vi.advanceTimersByTimeAsync(2_000);
    expect(attempts).toBe(2);

    const permanent = new FakeSession();
    permanent.mutate = async (command, args) => {
      permanent.calls.push({ command, args, mutation: true });
      throw new BrowserCoreError("core-error");
    };
    const permanentHost = new BrowserWorkerHost({ locks: locks(), boot: async () => permanent });
    await permanentHost.attach(port());
    await vi.advanceTimersByTimeAsync(10_000);
    expect(permanent.calls.filter(call => call.command === "advance_search_index")).toHaveLength(1);

    const closed = new FakeSession();
    closed.mutate = async (command, args) => {
      closed.calls.push({ command, args, mutation: true });
      closed.phase = "closed";
      throw new BrowserCoreError("core");
    };
    const closedHost = new BrowserWorkerHost({ locks: locks(), boot: async () => closed });
    await closedHost.attach(port());
    await vi.advanceTimersByTimeAsync(10_000);
    expect(closed.calls.filter(call => call.command === "advance_search_index")).toHaveLength(1);
    vi.useRealTimers();
  });

  it.each([
    ["succeeds", false],
    ["commits and then fails as a handled rejection", true],
  ])("does not broadcast a read that finishes behind an index-only checkpoint that %s", async (_outcome, rejects) => {
    vi.useFakeTimers();
    const session = new FakeSession();
    let finishBatch!: () => void;
    const batchGate = new Promise<void>(resolve => { finishBatch = resolve; });
    session.mutate = async (command, args) => {
      session.calls.push({ command, args, mutation: true });
      if (command !== "advance_search_index") {
        session.generation += 1;
        return { command };
      }
      await batchGate;
      session.generation += 1;
      if (rejects) throw new CoreRejectedError("unavailable");
      return { indexState: "indexing" };
    };
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      if (command === "search_local") return { indexState: "indexing" };
      if (command === "snapshot") await batchGate;
      return command === "snapshot" ? {} : { command };
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const first = port();
    const second = port();
    await host.attach(first);
    await host.attach(second);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(session.calls.filter(call => call.command === "advance_search_index")).toHaveLength(1);
    first.emit({ id: 1, command: "load_state", args: {} });
    await vi.advanceTimersByTimeAsync(0);
    finishBatch();
    await vi.advanceTimersByTimeAsync(0);
    expect(first.messages).toContainEqual(expect.objectContaining({ id: 1, ok: true }));
    expect(first.messages).not.toContainEqual({ event: "changed" });
    expect(second.messages).not.toContainEqual({ event: "changed" });
    vi.useRealTimers();
  });

  it("still broadcasts when a read overlaps a durable media mutation", async () => {
    const session = new FakeSession();
    let releaseRead!: () => void;
    const readGate = new Promise<void>(resolve => { releaseRead = resolve; });
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      if (command === "snapshot") await readGate;
      return command === "snapshot" ? {} : { command };
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "load_state", args: {} });
    await eventually(() => session.calls.some(call => call.command === "snapshot"));
    attached.emit({ id: 2, command: "request_attachment_download", args: { id: "00000000-0000-0000-0000-000000000002" } });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && (message as { id?: unknown }).id === 2));
    releaseRead();
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && (message as { id?: unknown }).id === 1));
    expect(attached.messages).toContainEqual({ event: "changed" });
  });

  it("skips preview enrichment for a flat metadata-only snapshot", async () => {
    const session = new FakeSession();
    session.snapshot = { activeConversationId: "conversation", conversations: [{ id: "conversation", messages: [{ attachments: [{ id: "image", mediaType: "image/png", state: "ready" }] }] }] };
    session.previewAttachment = vi.fn(async () => "data:image/png;base64,cHJldmlldw==");
    await expect(dispatchBrowserRpc(session, { id: 1, command: "load_state", args: { metadataOnly: true } })).resolves.toMatchObject({ ok: true });
    expect(session.previewAttachment).not.toHaveBeenCalled();
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

  it("emits one wake and change for an initial demand, then stays quiet for an already-demanded retry", async () => {
    const session = new FakeSession();
    const id = "00000000-0000-0000-0000-000000000001";
    let demanded = false;
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      return command === "attachment_info" ? { downloadRequested: demanded } : { command };
    };
    session.mutate = async (command, args) => { session.calls.push({ command, args, mutation: true }); demanded = true; session.generation += 1; return { command }; };
    const wake = vi.fn();
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session, network: { start: async () => undefined, stop: async () => undefined, wake } as unknown as import("./host.js").BrowserNetwork });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "request_attachment_download", args: { id } });
    await eventually(() => wake.mock.calls.length === 1);
    expect(attached.messages.filter(message => JSON.stringify(message) === JSON.stringify({ event: "changed" }))).toHaveLength(1);
    attached.emit({ id: 2, command: "request_attachment_download", args: { id } });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 2));
    expect(wake).toHaveBeenCalledTimes(1);
    expect(attached.messages.filter(message => JSON.stringify(message) === JSON.stringify({ event: "changed" }))).toHaveLength(1);
  });

  it("wakes only after a committed deferred-media batch accepts an attachment", async () => {
    const session = new FakeSession();
    const attachmentIds = ["00000000-0000-0000-0000-000000000001"];
    const events: string[] = [];
    session.mutate = async (command, args) => {
      session.calls.push({ command, args, mutation: true });
      session.generation += 1;
      return { requested: attachmentIds, alreadyRequested: [] };
    };
    const wake = vi.fn(() => events.push("wake"));
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      network: { start: async () => undefined, stop: async () => undefined, wake } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    const postMessage = attached.postMessage;
    attached.postMessage = message => {
      postMessage(message);
      if (typeof message === "object" && message !== null && "id" in message && message.id === 1) events.push("ack");
    };
    await host.attach(attached);

    attached.emit({ id: 1, command: "media_sync_request", args: { attachmentIds } });
    await eventually(() => wake.mock.calls.length === 1);

    expect(session.calls).toContainEqual({ command: "media_sync_request", args: { attachmentIds }, mutation: true });
    expect(events).toEqual(["ack", "wake"]);
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

  it("balances one retained media resource lease per port close", async () => {
    const session = new FakeSession();
    const id = "00000000-0000-0000-0000-000000000001";
    session.openMediaAttachment = async () => ({ bytes: new Uint8Array([1]), displayName: "image.jpg", type: "image/jpeg" });
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const first = port();
    const second = port();
    await Promise.all([host.attach(first), host.attach(second)]);
    first.emit({ id: 1, command: "open_media", args: { id } });
    second.emit({ id: 2, command: "open_media", args: { id } });
    await eventually(() => first.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1)
      && second.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 2));
    first.emit({ id: 3, command: "close_media", args: { id } });
    second.emit({ id: 4, command: "close_media", args: { id } });
    await eventually(() => session.calls.filter(call => call.command === "release_attachment_ciphertext").length === 2);
    expect(session.calls.filter(call => call.command === "release_attachment_ciphertext")).toEqual([
      { command: "release_attachment_ciphertext", args: { id }, mutation: true },
      { command: "release_attachment_ciphertext", args: { id }, mutation: true },
    ]);
  });

  it("detaches a port even when releasing one of its media leases fails", async () => {
    const session = new FakeSession();
    const id = "00000000-0000-0000-0000-000000000001";
    session.openMediaAttachment = async () => ({ bytes: new Uint8Array([1]), displayName: "image.jpg", type: "image/jpeg" });
    session.mutate = async (command, args) => {
      session.calls.push({ command, args, mutation: true });
      if (command === "release_attachment_ciphertext") throw new CoreRejectedError("media-pending");
      return { command };
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "open_media", args: { id } });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));

    attached.emit({ id: 2, command: "disconnect", args: {} });
    await eventually(() => attached.closed);
    expect(attached.messages).toContainEqual({ event: "stopped", reason: "disconnected" });
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
    const startsAtAttach = start.mock.calls.length;

    attached.emit({ id: 1, command, args: {} });

    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    expect(attached.messages).not.toContainEqual({ event: "changed" });
    expect(start).toHaveBeenCalledTimes(startsAtAttach);
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

  it("clears remembered recovery before sending hosted sign-out", async () => {
    const session = new FakeSession();
    const events: string[] = [];
    session.clearRememberedUnlock = async () => { events.push("clear"); };
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      network: {
        start: async () => undefined,
        stop: async () => undefined,
        enrollmentSignOut: async () => { events.push("signout"); },
      } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "enrollment_sign_out", args: {} });

    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    expect(events).toEqual(["clear", "signout"]);
    expect(session.phase).toBe("ready");
  });

  it("stops after Lock even when remembered recovery cannot clear", async () => {
    const session = new FakeSession();
    session.clearRememberedUnlock = async () => { throw new Error("storage unavailable"); };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const attached = port();
    await host.attach(attached);
    const otherTab = port();
    await host.attach(otherTab);

    attached.emit({ id: 1, command: "lock_sync", args: {} });

    await eventually(() => attached.closed);
    expect(attached.messages).toContainEqual({ id: 1, ok: false, error: { code: "recovery-clear-failed", message: "Browser recovery could not be forgotten." } });
    expect(attached.messages).toContainEqual({ event: "stopped", reason: "locked-recovery-clear-failed" });
    expect(otherTab.messages).toContainEqual({ event: "stopped", reason: "locked-recovery-clear-failed" });
    expect(session.phase).toBe("closed");
  });

  it("forgets recovery when a core operation reports this device revoked", async () => {
    const session = new FakeSession();
    const clearRememberedUnlock = vi.fn(async () => undefined);
    session.clearRememberedUnlock = clearRememberedUnlock;
    const query = session.query.bind(session);
    session.query = async (command, args) => {
      if (command === "snapshot") throw { code: "device-revoked" };
      return query(command, args);
    };
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "load_state", args: {} });

    await eventually(() => attached.messages.some(message => (message as { id?: number }).id === 1));
    expect(clearRememberedUnlock).toHaveBeenCalledOnce();
  });

  it("does not report hosted sign-out success when remembered recovery cannot clear", async () => {
    const session = new FakeSession();
    session.clearRememberedUnlock = async () => { throw new Error("storage unavailable"); };
    const enrollmentSignOut = vi.fn(async () => undefined);
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      network: { start: async () => undefined, stop: async () => undefined, enrollmentSignOut } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "enrollment_sign_out", args: {} });

    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    expect(attached.messages).toContainEqual({ id: 1, ok: false, error: { code: "recovery-clear-failed", message: "Browser recovery could not be forgotten." } });
    expect(enrollmentSignOut).toHaveBeenCalledTimes(1);
  });

  it("validates remembered recovery after enrollment status without stopping active sync", async () => {
    const session = new FakeSession();
    const validate = vi.fn(async () => undefined);
    session.validateRememberedUnlock = validate;
    const stop = vi.fn(async () => undefined);
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      network: {
        start: async () => undefined,
        stop,
        enrollmentStatus: async () => ({ mode: "hosted" as const, stage: "ready" as const }),
      } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "enrollment_status", args: {} });

    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 1));
    expect(validate).toHaveBeenCalledTimes(1);
    expect(stop).not.toHaveBeenCalled();
  });

  it("returns enrollment status before coalesced remembered recovery validation settles", async () => {
    vi.useFakeTimers();
    const session = new FakeSession();
    const gate = deferred();
    const validate = vi.fn(async () => gate.promise);
    session.validateRememberedUnlock = validate;
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      network: { start: async () => undefined, stop: async () => undefined, enrollmentStatus: async () => ({ mode: "hosted" as const, stage: "ready" as const }) } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 1, command: "enrollment_status", args: {} });
    attached.emit({ id: 2, command: "enrollment_status", args: {} });
    await vi.advanceTimersByTimeAsync(0);

    expect(attached.messages).toContainEqual(expect.objectContaining({ id: 1, ok: true }));
    expect(attached.messages).toContainEqual(expect.objectContaining({ id: 2, ok: true }));
    expect(validate).toHaveBeenCalledTimes(1);
    gate.release();
    await Promise.resolve();
    attached.emit({ id: 3, command: "enrollment_status", args: {} });
    await vi.advanceTimersByTimeAsync(0);
    expect(validate).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(60_000);
    attached.emit({ id: 4, command: "enrollment_status", args: {} });
    await vi.advanceTimersByTimeAsync(0);
    expect(validate).toHaveBeenCalledTimes(2);
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
    expect(session.calls.filter(call => call.command !== "search_local")).toEqual([]);
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
    expect(session.calls.filter(call => call.command === "_worker_identity_metadata")).toEqual([
      { command: "_worker_identity_metadata", args: {}, mutation: false },
      { command: "_worker_identity_metadata", args: {}, mutation: false },
    ]);
    expect(session.calls).not.toContainEqual(expect.objectContaining({ command: "list_devices" }));
    expect(session.calls).not.toContainEqual(expect.objectContaining({ command: "revoke_device" }));
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

  it("forgets remembered unlock when this device's credential is rejected", async () => {
    const session = new FakeSession();
    const clearRememberedUnlock = vi.fn(async () => undefined);
    session.clearRememberedUnlock = clearRememberedUnlock;
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      network: {
        start: async () => undefined,
        stop: async () => undefined,
        revokeDevice: async () => { throw new TransportError("status", undefined, "invalid_bearer"); },
      } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);
    attached.emit({ id: 1, command: "revoke_device", args: { deviceId: "00000000-0000-0000-0000-000000000001" } });

    await eventually(() => clearRememberedUnlock.mock.calls.length === 1);
    expect(attached.messages).toContainEqual(expect.objectContaining({ id: 1, ok: false, error: expect.objectContaining({ code: "invalid_bearer" }) }));
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
      { command: "set_notification_preferences", args: { messageBanners: true, mirroredBanners: true, preview: "hidden", muteWithHeads: true }, mutation: true },
      { command: "discard_draft", args: { id: "draft", conversationId: "conversation", expectedRevision: "1" }, mutation: true },
    ]);
  });

  it("writes notification preferences through the checkpointed core session", async () => {
    const session = new FakeSession();
    const response = await dispatchBrowserRpc(session, { id: 1, command: "set_notification_preferences", args: { preferences: { preview: "hidden" } } });
    expect(response).toMatchObject({ ok: true });
    expect(session.calls).toEqual([{ command: "set_notification_preferences", args: { messageBanners: true, mirroredBanners: true, preview: "hidden", muteWithHeads: true }, mutation: true }]);
  });

  it("keeps a failed-clear adoption bound to its original vault", async () => {
    const first = new FakeSession();
    const second = new FakeSession();
    second.query = async (command, args) => {
      second.calls.push({ command, args, mutation: false });
      if (command === "_worker_identity_metadata") return { vaultId: "00000000-0000-4000-8000-000000000002" };
      return {};
    };
    first.query = async (command, args) => {
      first.calls.push({ command, args, mutation: false });
      if (command === "_worker_identity_metadata") return { vaultId: "00000000-0000-4000-8000-000000000001" };
      if (command === "notification_preferences") return null;
      return {};
    };
    first.mutate = async (command, args) => { first.calls.push({ command, args, mutation: true }); return { adopted: true }; };
    const legacy = { claimForVault: vi.fn().mockResolvedValueOnce({ messageBanners: true, mirroredBanners: true, preview: "hidden" }).mockResolvedValueOnce(undefined), clear: vi.fn().mockRejectedValue(new Error("clear failed")) };
    await expect(dispatchBrowserRpc(first, { id: 1, command: "load_state", args: {} }, legacy)).resolves.toMatchObject({ ok: true });
    await expect(dispatchBrowserRpc(second, { id: 2, command: "load_state", args: {} }, legacy)).resolves.toMatchObject({ ok: true });
    expect(second.calls).not.toContainEqual(expect.objectContaining({ command: "adopt_legacy_notification_preferences" }));
    expect(legacy.claimForVault).toHaveBeenNthCalledWith(1, "00000000-0000-4000-8000-000000000001");
    expect(legacy.claimForVault).toHaveBeenNthCalledWith(2, "00000000-0000-4000-8000-000000000002");
  });

  it("retries cleanup after an established vault value without failing load state", async () => {
    const session = new FakeSession();
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      if (command === "_worker_identity_metadata") return { vaultId: "00000000-0000-4000-8000-000000000001" };
      if (command === "notification_preferences") return { messageBanners: true, mirroredBanners: true, preview: "hidden" };
      return {};
    };
    const clear = vi.fn().mockRejectedValueOnce(new Error("clear failed")).mockResolvedValue(undefined);
    const legacy = { claimForVault: vi.fn().mockResolvedValue({ messageBanners: true, mirroredBanners: true, preview: "hidden" }), clear };
    await expect(dispatchBrowserRpc(session, { id: 1, command: "load_state", args: {} }, legacy)).resolves.toMatchObject({ ok: true });
    await expect(dispatchBrowserRpc(session, { id: 2, command: "load_state", args: {} }, legacy)).resolves.toMatchObject({ ok: true });
    expect(clear).toHaveBeenCalledTimes(2);
  });

  it("blocks startup banner delivery until legacy hidden preferences migrate", async () => {
    const session = new FakeSession();
    let adopted = false;
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      if (command === "_worker_identity_metadata") return { vaultId: "00000000-0000-4000-8000-000000000001" };
      if (command === "notification_preferences") return adopted ? { messageBanners: true, mirroredBanners: true, preview: "hidden" } : null;
      if (command === "pending_banner_candidates") return [{ id: "candidate-1", kind: "message", title: "Private title", body: "Private body", createdAt: Date.now() }];
      if (command === "search_local") return { indexState: "ready" };
      return {};
    };
    session.mutate = async (command, args) => {
      session.calls.push({ command, args, mutation: true });
      if (command === "adopt_legacy_notification_preferences") { adopted = true; return { adopted: true }; }
      return {};
    };
    let changed: (() => void) | undefined;
    const wake = vi.fn();
    const shown: Array<{ title: string; body: string; tag: string }> = [];
    const legacy = { claimForVault: vi.fn().mockRejectedValueOnce(new Error("IndexedDB unavailable")).mockResolvedValue({ messageBanners: true, mirroredBanners: true, preview: "hidden" }), clear: vi.fn().mockResolvedValue(undefined) };
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      legacySettings: legacy,
      notification: { available: true, permission: "granted", show: notification => { shown.push(notification); } },
      network: { start: async () => undefined, stop: async () => undefined, wake, subscribeChanges: (listener: () => void) => { changed = listener; return () => undefined; } } as unknown as import("./host.js").BrowserNetwork,
    });
    const attached = port();
    await host.attach(attached);
    changed?.();
    await eventually(() => legacy.claimForVault.mock.calls.length === 1);
    expect(shown).toEqual([]);
    changed?.();
    await eventually(() => shown.length === 1);
    expect(shown).toEqual([{ title: "New message", body: "Open Peppy to view it.", tag: "peppy-candidate-1" }]);
    expect(wake).toHaveBeenCalledOnce();
    expect(attached.messages).toContainEqual({ event: "changed" });
  });

  it("keeps candidates queued when adoption reports unresolved", async () => {
    const session = new FakeSession();
    session.query = async (command, args) => {
      session.calls.push({ command, args, mutation: false });
      if (command === "_worker_identity_metadata") return { vaultId: "00000000-0000-4000-8000-000000000001" };
      if (command === "notification_preferences") return null;
      if (command === "pending_banner_candidates") return [{ id: "candidate-1", kind: "message", title: "Private title", body: "Private body", createdAt: Date.now() }];
      if (command === "search_local") return { indexState: "ready" };
      return {};
    };
    session.mutate = async (command, args) => {
      session.calls.push({ command, args, mutation: true });
      return command === "adopt_legacy_notification_preferences" ? { adopted: false } : {};
    };
    let changed: (() => void) | undefined;
    const shown: unknown[] = [];
    const host = new BrowserWorkerHost({
      locks: locks(),
      boot: async () => session,
      legacySettings: { claimForVault: async () => ({ messageBanners: true, mirroredBanners: true, preview: "hidden" }), clear: async () => undefined },
      notification: { available: true, permission: "granted", show: notification => { shown.push(notification); } },
      network: { start: async () => undefined, stop: async () => undefined, subscribeChanges: (listener: () => void) => { changed = listener; return () => undefined; } } as unknown as import("./host.js").BrowserNetwork,
    });
    await host.attach(port());
    changed?.();
    await eventually(() => session.calls.some(call => call.command === "adopt_legacy_notification_preferences"));
    expect(shown).toEqual([]);
    expect(session.calls).not.toContainEqual(expect.objectContaining({ command: "ack_banner_candidates" }));
  });

  it("retires default legacy settings without checkpointing adoption", async () => {
    const session = new FakeSession();
    const legacy = { claimForVault: vi.fn().mockResolvedValue({ messageBanners: true, mirroredBanners: true, preview: "full", muteWithHeads: true }), clear: vi.fn().mockResolvedValue(undefined) };
    await expect(dispatchBrowserRpc(session, { id: 1, command: "load_state", args: {} }, legacy)).resolves.toMatchObject({ ok: true });
    expect(session.calls).not.toContainEqual(expect.objectContaining({ command: "adopt_legacy_notification_preferences" }));
    expect(legacy.clear).toHaveBeenCalledOnce();
  });

  it("echoes a valid id for an oversized malformed request", async () => {
    const response = await dispatchBrowserRpc(new FakeSession(), { id: 7, command: "load_state", args: { value: "x".repeat(1024 * 1024) } });
    expect(response).toMatchObject({ id: 7, ok: false, error: { code: "invalid-request" } });
  });

  it("serializes an exported-dispatcher request exactly once", async () => {
    let serializations = 0;
    const session = new FakeSession();
    const args = { probe: { toJSON() { serializations += 1; return "probe"; } } };

    await expect(dispatchBrowserRpc(session, { id: 8, command: "load_state", args })).resolves.toMatchObject({ id: 8, ok: true });
    expect(serializations).toBe(1);
  });

  it("serializes an attached-port request exactly once before dispatch", async () => {
    let serializations = 0;
    const session = new FakeSession();
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const attached = port();
    await host.attach(attached);

    attached.emit({ id: 8, command: "load_state", args: { probe: { toJSON() { serializations += 1; return "probe"; } } } });
    await eventually(() => attached.messages.some(message => typeof message === "object" && message !== null && "id" in message && message.id === 8));

    expect(attached.messages).toContainEqual(expect.objectContaining({ id: 8, ok: true }));
    expect(serializations).toBe(1);
  });

  it("rejects an unallowlisted command without touching the session", async () => {
    const session = new FakeSession();
    await expect(dispatchBrowserRpc(session, { id: 9, command: "not_allowlisted", args: {} }))
      .resolves.toMatchObject({ id: 9, ok: false, error: { code: "invalid-request" } });
    expect(session.calls).toEqual([]);
  });

  it("rejects an oversized draft before any session call", async () => {
    const session = new FakeSession();
    await expect(dispatchBrowserRpc(session, { id: 10, command: "save_draft", args: { input: { text: "x".repeat(1024 * 1024) } } }))
      .resolves.toMatchObject({ id: 10, ok: false, error: { code: "invalid-request" } });
    expect(session.calls).toEqual([]);
  });
});
