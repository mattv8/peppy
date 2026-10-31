import "fake-indexeddb/auto";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CheckpointError, type CheckpointInput, IndexedDbCheckpointStore, RuntimeCoreError } from "./checkpoint.js";
import { CoreRejectedError, type CoreResult } from "./core.js";
import { type EmscriptenFilesystem } from "./filesystem.js";
import { BrowserSession, BrowserSessionError } from "./session.js";
import { BrowserWorkerHost, dispatchBrowserRpc, type BrowserPort } from "./host.js";
import type { OwnerLockManager } from "./owner.js";

const encoder = new TextEncoder();
const id = "123e4567-e89b-12d3-a456-426614174000";
const cipherPath = `client.db.media/cipher/${id}.ppss`;
const previewIds = Array.from({ length: 24 }, (_, index) => `123e4567-e89b-12d3-a456-${String(index + 1).padStart(12, "0")}`);

function cipherPathFor(attachmentId: string): string { return `client.db.media/cipher/${attachmentId}.ppss`; }

type Node = { directory: Set<string> } | { bytes: Uint8Array };

class MemoryFs implements EmscriptenFilesystem {
  private readonly nodes = new Map<string, Node>([["/", { directory: new Set() }]]);
  public readdir(path: string): string[] { const node = this.node(path); if (!("directory" in node)) throw new Error("not directory"); return [".", "..", ...node.directory]; }
  public lstat(path: string): { size: number; mode: number } { const node = this.node(path); return "directory" in node ? { size: 0, mode: 2 } : { size: node.bytes.byteLength, mode: 1 }; }
  public isFile(mode: number): boolean { return mode === 1; }
  public isDir(mode: number): boolean { return mode === 2; }
  public isLink(): boolean { return false; }
  public readFile(path: string): Uint8Array { const node = this.node(path); if ("directory" in node) throw new Error("not file"); return new Uint8Array(node.bytes); }
  public writeFile(path: string, bytes: Uint8Array): void { if (this.nodes.has(path)) throw new Error("exists"); this.parent(path).add(this.name(path)); this.nodes.set(path, { bytes: new Uint8Array(bytes) }); }
  public mkdir(path: string): void { if (this.nodes.has(path)) throw new Error("exists"); this.parent(path).add(this.name(path)); this.nodes.set(path, { directory: new Set() }); }
  public unlink(path: string): void { if (!this.nodes.delete(path)) throw new Error("missing"); this.parent(path).delete(this.name(path)); }
  public seedDatabase(): void { this.mkdir("/peppy"); this.writeFile("/peppy/client.db", encoder.encode("encrypted database")); this.mkdir("/peppy/client.db.media"); this.mkdir("/peppy/client.db.media/cipher"); }
  private node(path: string): Node { const node = this.nodes.get(path); if (!node) throw new Error("missing"); return node; }
  private parent(path: string): Set<string> { const node = this.node(path.slice(0, path.lastIndexOf("/")) || "/"); if (!("directory" in node)) throw new Error("not directory"); return node.directory; }
  private name(path: string): string { return path.slice(path.lastIndexOf("/") + 1); }
}

class TrustedCore {
  public envelope = "envelope-1";
  public tokenCalls = 0;
  public attachmentIds: string[] = [];
  public unlockFailure?: Error;
  public mutationFailure?: Error;
  public finishFailure?: Error;
  public tokenGate?: Promise<void>;
  public publicIdentity = { vaultId: id, deviceId: id, role: "owner", unlocked: true, epoch: "1" };
  public readonly commands: string[] = [];
  public async invoke({ command }: { command: string; args: Record<string, unknown> }): Promise<CoreResult> {
    this.commands.push(command);
    if ((command === "_worker_enrollment_finish_owner" || command === "_worker_enrollment_finish_manual") && this.finishFailure) throw this.finishFailure;
    if (command === "_worker_unlock_identity" && this.unlockFailure) throw this.unlockFailure;
    if (command === "_worker_enrollment_finish_owner" || command === "_worker_enrollment_finish_manual" || command === "_worker_enrollment_finish_computer" || command === "_worker_unlock_identity" || command === "_worker_enroll_identity") return this.publicIdentity;
    if (command === "mutate" && this.mutationFailure) throw this.mutationFailure;
    if (command === "_worker_checkpoint_identity") return { wrappedIdentity: this.envelope };
    if (command === "_worker_local_attachment_ids") return { attachmentIds: this.attachmentIds };
    if (command === "attachment_info") return { state: "ready", plaintextBytes: 3 };
    if (command === "retain_attachment_ciphertext" || command === "touch_attachment_ciphertext" || command === "release_attachment_ciphertext") return {};
    if (command === "_worker_transport_token") { await this.tokenGate; this.tokenCalls++; return { deviceToken: "private-token" }; }
    if (command === "preview_attachment") return { previewUrl: "data:image/png;base64,cHJldmlldw==" };
    if (command === "_worker_rotate_identity") this.envelope = "rotated-envelope";
    return {};
  }
}

class AttachmentCore extends TrustedCore {
  public state: "ready" | "pending" = "ready";
  public readonly attachmentCommands: string[] = [];
  public attachmentInfoGate?: Promise<void>;
  public exportFailure?: CoreRejectedError;
  public touchFailure?: CoreRejectedError;
  public snapshot?: CoreResult;
  public history?: CoreResult;
  public content = encoder.encode("cat");
  public previewFailure?: CoreRejectedError;
  public constructor(private readonly fs: MemoryFs) { super(); }

  public override async invoke(request: { command: string; args: Record<string, unknown> }): Promise<CoreResult> {
    const { command, args } = request;
    this.attachmentCommands.push(command);
    if (command === "attachment_info") {
      await this.attachmentInfoGate;
      return this.state === "ready" ? { state: "ready", plaintextBytes: this.content.byteLength } : { state: "pending" };
    }
    if (command === "preview_attachment" && this.previewFailure) throw this.previewFailure;
    if (command === "retain_attachment_ciphertext") {
      if (this.state !== "ready") throw new CoreRejectedError("media-pending");
      return {};
    }
    if (command === "touch_attachment_ciphertext") {
      if (this.touchFailure) throw this.touchFailure;
      return {};
    }
    if (command === "evict_attachment_ciphertext") {
      this.state = "pending";
      try { this.fs.unlink(`/peppy/${cipherPath}`); } catch {}
      return {};
    }
    if (command === "export_attachment") {
      if (this.state !== "ready") throw new CoreRejectedError("media-pending");
      if (this.exportFailure) throw this.exportFailure;
      const outputId = args.outputId;
      if (typeof outputId !== "string") throw new Error("missing output ID");
      if (!this.fs.readdir("/peppy").includes("worker-output")) this.fs.mkdir("/peppy/worker-output");
      this.fs.writeFile(`/peppy/worker-output/${outputId}`, this.content);
      return { file: `worker-output/${outputId}`, length: this.content.byteLength, displayName: "cat.jpg", type: "image/jpeg" };
    }
    if (command === "public_image") {
      const outputId = args.outputId;
      if (typeof outputId !== "string") throw new Error("missing output ID");
      if (!this.fs.readdir("/peppy").includes("worker-output")) this.fs.mkdir("/peppy/worker-output");
      this.fs.writeFile(`/peppy/worker-output/${outputId}`, encoder.encode("cat"));
      return { file: `worker-output/${outputId}`, byteSize: 3, name: "public.png" };
    }
    if (command === "_worker_attachment_remote") return { remoteObjectId: "remote-object" };
    if (command === "_worker_local_attachment_ids") return { attachmentIds: this.state === "ready" ? this.attachmentIds : [] };
    if (command === "snapshot" && this.snapshot) return this.snapshot;
    if (command === "load_history" && this.history) return this.history;
    return super.invoke(request);
  }
}

function deferred(): { promise: Promise<void>; release(): void } {
  let release!: () => void;
  return { promise: new Promise<void>(resolve => { release = resolve; }), release };
}

async function unlockedAttachmentSession(state: "ready" | "pending", files: CheckpointInput["files"], attachmentIds = [id]): Promise<{ core: AttachmentCore; fs: MemoryFs; session: BrowserSession; store: IndexedDbCheckpointStore }> {
  const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
  await store.commit(checkpoint(0, "envelope-1", files));
  const fs = new MemoryFs();
  const core = new AttachmentCore(fs);
  core.state = state;
  core.attachmentIds = attachmentIds;
  const session = await BrowserSession.boot(sessionOptions(core, fs, store));
  await session.unlock("passphrase");
  return { core, fs, session, store };
}

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

function workerPort(): BrowserPort & { messages: unknown[]; emit(message: unknown): void; closed: boolean } {
  const result = {
    messages: [] as unknown[], closed: false, onmessage: null as BrowserPort["onmessage"],
    postMessage(message: unknown) { result.messages.push(message); },
    close() { result.closed = true; },
    emit(message: unknown) { result.onmessage?.({ data: message } as MessageEvent); },
  };
  return result;
}

function checkpoint(generation: number, envelope = "envelope-1", files: CheckpointInput["files"] = [{ path: "client.db", bytes: encoder.encode("encrypted database") }]): CheckpointInput {
  return { expectedGeneration: generation, files, wrappedCredentials: encoder.encode(envelope) };
}

function sessionOptions(core: TrustedCore, fs: MemoryFs, store: IndexedDbCheckpointStore, onFatal = vi.fn()) {
  return { origin: "https://peppy.test", core, filesystem: fs, checkpoint: store, onFatal };
}

function rememberedUnlock() {
  return {
    remember: vi.fn(async () => undefined),
    recover: vi.fn(async () => undefined as string | undefined),
    clear: vi.fn(async () => undefined),
    validate: vi.fn(async () => undefined),
  };
}

describe("BrowserSession", () => {
  afterEach(() => { vi.useRealTimers(); });

  it("boots fresh or restores a locked checkpoint without exposing its identity", async () => {
    const fresh = await BrowserSession.boot(sessionOptions(new TrustedCore(), new MemoryFs(), new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`)));
    expect(fresh.phase).toBe("unenrolled");
    expect(() => fresh.tokenForTransport()).toThrow(BrowserSessionError);

    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0));
    const locked = await BrowserSession.boot(sessionOptions(new TrustedCore(), new MemoryFs(), store));
    expect(locked.phase).toBe("locked");
    expect(locked.generation).toBe(1);
  });

  it("restores a committed checkpoint with a remembered phrase without saving it again", async () => {
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0));
    const recovery = rememberedUnlock();
    recovery.recover.mockResolvedValue("remembered-passphrase");

    const session = await BrowserSession.boot({ ...sessionOptions(new TrustedCore(), new MemoryFs(), store), rememberedUnlock: recovery });

    expect(session.phase).toBe("ready");
    expect(recovery.recover).toHaveBeenCalledWith("envelope-1");
    expect(recovery.remember).not.toHaveBeenCalled();
  });

  it("keeps a restored checkpoint locked when its remembered phrase is rejected", async () => {
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0));
    const recovery = rememberedUnlock();
    recovery.recover.mockResolvedValue("wrong-passphrase");
    const core = new TrustedCore();
    core.unlockFailure = new CoreRejectedError("unlock-failed");

    const session = await BrowserSession.boot({ ...sessionOptions(core, new MemoryFs(), store), rememberedUnlock: recovery });

    expect(session.phase).toBe("locked");
    expect(recovery.clear).toHaveBeenCalledTimes(1);
    await expect(store.load(1)).resolves.toMatchObject({ kind: "present" });
  });

  it("does not unlock when recovery resolves after shutdown", async () => {
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0));
    const recovery = rememberedUnlock();
    const core = new TrustedCore();
    const session = await BrowserSession.boot({ ...sessionOptions(core, new MemoryFs(), store), rememberedUnlock: recovery });
    const gate = deferred();
    recovery.recover.mockImplementation(async () => { await gate.promise; return "remembered-passphrase"; });

    const recovering = (session as unknown as { recoverRememberedUnlock(): Promise<void> }).recoverRememberedUnlock();
    session.shutdown();
    gate.release();

    await expect(recovering).resolves.toBeUndefined();
    expect(core.commands.filter(command => command === "_worker_unlock_identity")).toHaveLength(0);
    expect(session.phase).toBe("closed");
  });

  it("reaches locked boot when remembered recovery hangs and never unlocks after the deadline", async () => {
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0));
    const recovery = rememberedUnlock();
    let release!: (phrase: string) => void;
    recovery.recover.mockImplementation(() => new Promise<string | undefined>(resolve => { release = resolve; }));
    const core = new TrustedCore();
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    try {
      const boot = BrowserSession.boot({ ...sessionOptions(core, new MemoryFs(), store), rememberedUnlock: recovery });
      for (let attempt = 0; attempt < 10 && recovery.recover.mock.calls.length === 0; attempt += 1) {
        await new Promise<void>(resolve => setImmediate(resolve));
      }
      expect(recovery.recover).toHaveBeenCalledOnce();
      await vi.advanceTimersByTimeAsync(3_000);

      await expect(boot).resolves.toMatchObject({ phase: "locked" });
      release("remembered-passphrase");
      await Promise.resolve();
      await Promise.resolve();
      expect(core.commands).not.toContain("_worker_unlock_identity");
    } finally {
      vi.useRealTimers();
    }
  });

  it("awaits durable remembered unlock work after recovery resolves before the deadline", async () => {
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0));
    const recovery = rememberedUnlock();
    recovery.recover.mockResolvedValue("remembered-passphrase");
    const core = new TrustedCore();
    const token = deferred();
    core.tokenGate = token.promise;
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    try {
      let settled = false;
      const boot = BrowserSession.boot({ ...sessionOptions(core, new MemoryFs(), store), rememberedUnlock: recovery }).then(session => {
        settled = true;
        return session;
      });
      await vi.waitFor(() => expect(core.commands).toContain("_worker_transport_token"));

      await vi.advanceTimersByTimeAsync(3_000);
      expect(settled).toBe(false);
      expect(core.commands).toContain("_worker_unlock_identity");

      token.release();
      await expect(boot).resolves.toMatchObject({ phase: "ready" });
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not become ready or expose the transport token before durable enrollment", async () => {
    const fs = new MemoryFs();
    const core = new TrustedCore();
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    const commit = store.commit.bind(store);
    let release!: () => void;
    store.commit = vi.fn(async input => { await new Promise<void>(resolve => { release = resolve; }); return commit(input); });
    const session = await BrowserSession.boot(sessionOptions(core, fs, store));
    fs.seedDatabase();
    const pending = session.enroll({}, "device-token", "passphrase");
    await vi.waitFor(() => expect(release).toBeTypeOf("function"));
    expect(session.phase).toBe("unenrolled");
    expect(() => session.tokenForTransport()).toThrow(BrowserSessionError);
    release();
    await pending;
    expect(session.phase).toBe("ready");
    expect(session.tokenForTransport()).toBe("private-token");
    expect(core.tokenCalls).toBe(1);
  });

  it("finishes verified enrollment before checkpointing and never becomes ready when the Rust finish command rejects", async () => {
    const fs = new MemoryFs();
    const core = new TrustedCore();
    const session = await BrowserSession.boot(sessionOptions(core, fs, new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`)));
    fs.seedDatabase();

    await session.finishEnrollment("_worker_enrollment_finish_owner", "admission-response", "vault-secret");
    expect(core.commands).toEqual(expect.arrayContaining(["_worker_enrollment_finish_owner", "_worker_checkpoint_identity", "_worker_transport_token"]));
    expect(core.commands.indexOf("_worker_enrollment_finish_owner")).toBeLessThan(core.commands.indexOf("_worker_checkpoint_identity"));
    expect(core.commands.indexOf("_worker_checkpoint_identity")).toBeLessThan(core.commands.indexOf("_worker_transport_token"));
    expect(session.phase).toBe("ready");
    expect(session.publicIdentity()).toEqual({ role: "owner" });

    const failedFs = new MemoryFs();
    const failedCore = new TrustedCore();
    failedCore.finishFailure = new CoreRejectedError("invalid-request");
    const failed = await BrowserSession.boot(sessionOptions(failedCore, failedFs, new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`)));
    failedFs.seedDatabase();
    await expect(failed.finishEnrollment("_worker_enrollment_finish_manual", "admission-response", "vault-secret")).rejects.toMatchObject({ code: "invalid-request" });
    expect(failed.phase).toBe("unenrolled");
  });

  it("keeps an old checkpoint after a rejected unlock and permits a retry", async () => {
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0));
    const core = new TrustedCore();
    core.unlockFailure = new CoreRejectedError("unlock-failed");
    const session = await BrowserSession.boot(sessionOptions(core, new MemoryFs(), store));
    await expect(session.unlock("wrong")).rejects.toMatchObject({ code: "unlock-failed" });
    await expect(store.load(1)).resolves.toMatchObject({ kind: "present" });
    expect(session.phase).toBe("locked");
    await expect(session.query("read", {})).rejects.toThrow(BrowserSessionError);
    core.unlockFailure = undefined;
    await expect(session.unlock("correct")).resolves.toBeUndefined();
    expect(session.phase).toBe("ready");
  });

  it("does not remember a manually unlocked phrase by default", async () => {
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0));
    const recovery = rememberedUnlock();
    const session = await BrowserSession.boot({ ...sessionOptions(new TrustedCore(), new MemoryFs(), store), rememberedUnlock: recovery });

    await session.unlock("correct-passphrase");

    expect(recovery.remember).not.toHaveBeenCalled();
  });

  it("remembers an opted-in manually unlocked phrase only after its checkpoint commits", async () => {
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0));
    const commit = store.commit.bind(store);
    const gate = deferred();
    store.commit = vi.fn(async input => { await gate.promise; return commit(input); });
    const recovery = rememberedUnlock();
    const core = new TrustedCore();
    const session = await BrowserSession.boot({ ...sessionOptions(core, new MemoryFs(), store), rememberedUnlock: recovery });

    const unlock = session.unlock("correct-passphrase", undefined, { remember: true });
    await vi.waitFor(() => expect(core.commands).toContain("_worker_checkpoint_identity"));
    expect(recovery.remember).not.toHaveBeenCalled();
    gate.release();
    await unlock;

    expect(recovery.remember).toHaveBeenCalledWith("envelope-1", "correct-passphrase");
  });

  it("completes explicit unlock after checkpointing without waiting for remembered recovery", async () => {
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0));
    const recovery = rememberedUnlock();
    const gate = deferred();
    recovery.remember.mockImplementation(async () => { await gate.promise; return undefined; });
    const session = await BrowserSession.boot({ ...sessionOptions(new TrustedCore(), new MemoryFs(), store), rememberedUnlock: recovery });

    await expect(session.unlock("correct-passphrase", undefined, { remember: true })).resolves.toBeUndefined();

    expect(session.phase).toBe("ready");
    expect(recovery.remember).toHaveBeenCalledWith("envelope-1", "correct-passphrase");
    await expect(store.load(session.generation)).resolves.toMatchObject({ kind: "present" });
    gate.release();
  });

  it("never remembers a rejected manual unlock", async () => {
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0));
    const recovery = rememberedUnlock();
    const core = new TrustedCore();
    core.unlockFailure = new CoreRejectedError("unlock-failed");
    const session = await BrowserSession.boot({ ...sessionOptions(core, new MemoryFs(), store), rememberedUnlock: recovery });

    await expect(session.unlock("wrong-passphrase")).rejects.toMatchObject({ code: "unlock-failed" });

    expect(recovery.remember).not.toHaveBeenCalled();
  });

  it("fences a pending unlock from remembering after sign-out clears recovery", async () => {
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0));
    const recovery = rememberedUnlock();
    const core = new TrustedCore();
    const gate = deferred();
    core.tokenGate = gate.promise;
    const session = await BrowserSession.boot({ ...sessionOptions(core, new MemoryFs(), store), rememberedUnlock: recovery });

    const unlock = session.unlock("correct-passphrase", undefined, { remember: true });
    await vi.waitFor(() => expect(core.commands).toContain("_worker_transport_token"));
    await session.clearRememberedUnlock();
    gate.release();
    await unlock;

    expect(recovery.clear).toHaveBeenCalledTimes(1);
    expect(recovery.remember).not.toHaveBeenCalled();
  });

  it("reports a remembered recovery clear failure without changing the checkpoint", async () => {
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0));
    const recovery = rememberedUnlock();
    recovery.clear.mockRejectedValue(new Error("storage unavailable"));
    const session = await BrowserSession.boot({ ...sessionOptions(new TrustedCore(), new MemoryFs(), store), rememberedUnlock: recovery });

    await expect(session.clearRememberedUnlock()).rejects.toThrow("storage unavailable");
    expect(session.phase).toBe("locked");
    await expect(store.load(1)).resolves.toMatchObject({ kind: "present" });
  });

  it("re-keys the rotated wrapped identity only after recovering the prior opted-in identity", async () => {
    const fs = new MemoryFs();
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    const recovery = rememberedUnlock();
    recovery.recover.mockResolvedValue("first-passphrase");
    const core = new TrustedCore();
    const session = await BrowserSession.boot({ ...sessionOptions(core, fs, store), rememberedUnlock: recovery });
    fs.seedDatabase();
    await session.enroll({}, "device-token", "first-passphrase");
    recovery.remember.mockClear();

    await session.rotate({}, {}, "rotated-passphrase");

    await vi.waitFor(() => expect(recovery.recover).toHaveBeenCalledWith("envelope-1"));
    expect(recovery.remember).toHaveBeenCalledWith("rotated-envelope", "rotated-passphrase");
  });

  it("does not create remembered recovery while rotating without an existing record", async () => {
    const fs = new MemoryFs();
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    const recovery = rememberedUnlock();
    recovery.recover.mockResolvedValue(undefined);
    const session = await BrowserSession.boot({ ...sessionOptions(new TrustedCore(), fs, store), rememberedUnlock: recovery });
    fs.seedDatabase();
    await session.enroll({}, "device-token", "first-passphrase");

    await session.rotate({}, {}, "rotated-passphrase");

    expect(recovery.recover).toHaveBeenCalledWith("envelope-1");
    expect(recovery.remember).not.toHaveBeenCalled();
  });

  it("never remembers enrollment paths", async () => {
    const recovery = rememberedUnlock();
    recovery.recover.mockResolvedValue(undefined);
    const fs = new MemoryFs();
    const enrolled = await BrowserSession.boot({ ...sessionOptions(new TrustedCore(), fs, new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`)), rememberedUnlock: recovery });
    fs.seedDatabase();
    await enrolled.enroll({}, "device-token", "passphrase");
    expect(recovery.remember).not.toHaveBeenCalled();

    const finishedFs = new MemoryFs();
    const finished = await BrowserSession.boot({ ...sessionOptions(new TrustedCore(), finishedFs, new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`)), rememberedUnlock: recovery });
    finishedFs.seedDatabase();
    await finished.finishEnrollment("_worker_enrollment_finish_owner", "response", "passphrase");
    expect(recovery.remember).not.toHaveBeenCalled();

    const computerFs = new MemoryFs();
    const computer = await BrowserSession.boot({ ...sessionOptions(new TrustedCore(), computerFs, new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`)), rememberedUnlock: recovery });
    computerFs.seedDatabase();
    await computer.finishComputerEnrollment("response");
    expect(recovery.remember).not.toHaveBeenCalled();
  });

  it("retains a completed checkpoint when cancellation arrives during its commit", async () => {
    const fs = new MemoryFs();
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    const original = store.commit.bind(store);
    let release!: () => void;
    store.commit = vi.fn(async input => { await new Promise<void>(resolve => { release = resolve; }); return original(input); });
    const session = await BrowserSession.boot(sessionOptions(new TrustedCore(), fs, store));
    fs.seedDatabase();
    const controller = new AbortController();
    const pending = session.enroll({}, "device-token", "passphrase", controller.signal);
    await vi.waitFor(() => expect(release).toBeTypeOf("function"));
    controller.abort();
    release();
    await expect(pending).rejects.toMatchObject({ code: "cancelled" });
    await expect(store.load(1)).resolves.toMatchObject({ kind: "present" });
    expect(session.phase).toBe("closed");
  });

  it("commits a fresh Rust envelope after handled mutation errors and rotation", async () => {
    const fs = new MemoryFs();
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    const core = new TrustedCore();
    const session = await BrowserSession.boot(sessionOptions(core, fs, store));
    fs.seedDatabase();
    const enrollment = session.enroll({}, "device-token", "passphrase");
    await enrollment;
    core.envelope = "envelope-after-error";
    core.mutationFailure = new CoreRejectedError("core");
    await expect(session.mutate("mutate", {})).rejects.toMatchObject({ code: "core" });
    expect(session.generation).toBe(2);
    core.mutationFailure = undefined;
    await session.rotate({}, {}, "passphrase");
    const loaded = await store.load(3);
    expect(loaded).toMatchObject({ kind: "present" });
    if (loaded.kind === "present") expect(new TextDecoder().decode(loaded.checkpoint.manifest.wrappedCredentials)).toBe("rotated-envelope");
  });

  it("closes the owner and reports one fatal error when a mutation checkpoint fails", async () => {
    const fs = new MemoryFs();
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    const onFatal = vi.fn();
    const session = await BrowserSession.boot(sessionOptions(new TrustedCore(), fs, store, onFatal));
    fs.seedDatabase();
    await session.enroll({}, "device-token", "passphrase");
    store.commit = vi.fn(async () => { throw new CheckpointError("storage-unavailable"); });
    await expect(session.mutate("mutate", {})).rejects.toMatchObject({ code: "storage-unavailable" });
    expect(session.phase).toBe("closed");
    expect(onFatal).toHaveBeenCalledTimes(1);
  });

  it("hydrates only a requested cold cipher at the committed generation", async () => {
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0, "envelope-1", [
      { path: "client.db", bytes: encoder.encode("encrypted database") },
      { path: cipherPath, bytes: encoder.encode("cold cipher") },
    ]));
    const fs = new MemoryFs();
    const core = new TrustedCore();
    core.attachmentIds = [id];
    const session = await BrowserSession.boot(sessionOptions(core, fs, store));
    await session.unlock("passphrase");
    await session.hydrate(id);
    expect(new TextDecoder().decode(fs.readFile(`/peppy/${cipherPath}`))).toBe("cold cipher");
  });

  it("hydrates a cold attachment before returning a sanitized preview and clears it on lock", async () => {
    const store = new IndexedDbCheckpointStore(`session-${crypto.randomUUID()}`);
    await store.commit(checkpoint(0, "envelope-1", [
      { path: "client.db", bytes: encoder.encode("encrypted database") },
      { path: cipherPath, bytes: encoder.encode("cold cipher") },
    ]));
    const core = new TrustedCore();
    core.attachmentIds = [id];
    const session = await BrowserSession.boot(sessionOptions(core, new MemoryFs(), store));
    await session.unlock("passphrase");
    await expect(session.previewAttachment(id)).resolves.toBe("data:image/png;base64,cHJldmlldw==");
    session.shutdown();
    await expect(session.previewAttachment(id)).rejects.toThrow(BrowserSessionError);
  });

  it("keeps ready-media availability, hydration, and export atomic when queued eviction races two callers", async () => {
    const { promise, release } = deferred();
    const { core, session, store } = await unlockedAttachmentSession("ready", [
      { path: "client.db", bytes: encoder.encode("encrypted database") },
      { path: cipherPath, bytes: encoder.encode("cold cipher") },
    ]);
    core.attachmentInfoGate = promise;

    const first = session.openMediaAttachment(id, 32 * 1024 * 1024);
    await vi.waitFor(() => expect(core.attachmentCommands.filter(command => command === "attachment_info")).toHaveLength(1));
    const eviction = session.mutate("evict_attachment_ciphertext", { id });
    const second = session.openMediaAttachment(id, 32 * 1024 * 1024);
    const firstFailure = expect(first).rejects.toMatchObject({ code: "media-pending" });
    const secondFailure = expect(second).rejects.toMatchObject({ code: "media-pending" });
    release();

    await expect(eviction).resolves.toEqual({});
    await firstFailure;
    await secondFailure;
    expect(core.attachmentCommands).not.toContain("export_attachment");
    expect(core.attachmentCommands).not.toContain("retain_attachment_ciphertext");
    expect(session.phase).toBe("ready");
    const saved = await store.load(session.generation);
    expect(saved).toMatchObject({ kind: "present" });
    if (saved.kind === "present") expect(saved.checkpoint.manifest.files).not.toContain(cipherPath);
  });

  it("persists an opened cipher after LRU touch and avoids checkpoint work for repeated passive media-pending opens", async () => {
    const ready = await unlockedAttachmentSession("ready", [
      { path: "client.db", bytes: encoder.encode("encrypted database") },
      { path: cipherPath, bytes: encoder.encode("cold cipher") },
    ]);
    await expect(ready.session.openMediaAttachment(id, 32 * 1024 * 1024)).resolves.toMatchObject({ bytes: encoder.encode("cat") });
    expect(ready.core.attachmentCommands).toEqual(expect.arrayContaining(["retain_attachment_ciphertext", "export_attachment", "touch_attachment_ciphertext"]));
    expect(ready.core.attachmentCommands.lastIndexOf("touch_attachment_ciphertext")).toBeLessThan(ready.core.attachmentCommands.lastIndexOf("_worker_checkpoint_identity"));
    const durable = await ready.store.load(ready.session.generation);
    if (durable.kind !== "present") throw new Error("checkpoint missing");
    await expect(durable.checkpoint.readCipher(cipherPath)).resolves.toEqual(encoder.encode("cold cipher"));

    const pending = await unlockedAttachmentSession("pending", [{ path: "client.db", bytes: encoder.encode("encrypted database") }]);
    const commit = vi.spyOn(pending.store, "commit");
    await expect(pending.session.openMediaAttachment(id, 32 * 1024 * 1024)).rejects.toMatchObject({ code: "media-pending" });
    await expect(pending.session.openMediaAttachment(id, 32 * 1024 * 1024)).rejects.toMatchObject({ code: "media-pending" });
    expect(commit).not.toHaveBeenCalled();
    expect(pending.core.attachmentCommands.filter(command => command === "attachment_info")).toHaveLength(2);
    expect(pending.session.phase).toBe("ready");
  });

  it("poisons the session when ready local media is absent from the committed manifest", async () => {
    const setup = await unlockedAttachmentSession("pending", [{ path: "client.db", bytes: encoder.encode("encrypted database") }]);
    setup.core.state = "ready";

    await expect(setup.session.openMediaAttachment(id, 32 * 1024 * 1024)).rejects.toBeInstanceOf(RuntimeCoreError);
    expect(setup.session.phase).toBe("closed");
    await expect(setup.session.openMediaAttachment(id, 32 * 1024 * 1024)).rejects.toThrow(BrowserSessionError);
  });

  it.each(["exportFailure", "touchFailure"] as const)("releases an acquired media lease after a handled %s", async failure => {
    const { core, session } = await unlockedAttachmentSession("ready", [
      { path: "client.db", bytes: encoder.encode("encrypted database") },
      { path: cipherPath, bytes: encoder.encode("cold cipher") },
    ]);
    core[failure] = new CoreRejectedError("media-pending");

    await expect(session.openMediaAttachment(id, 32 * 1024 * 1024)).rejects.toMatchObject({ code: "media-pending" });
    expect(core.attachmentCommands.lastIndexOf("release_attachment_ciphertext")).toBeGreaterThan(core.attachmentCommands.lastIndexOf("retain_attachment_ciphertext"));
    expect(session.phase).toBe("ready");
  });

  it("maps pending open and load RPCs from a real BrowserSession to ready pending values", async () => {
    const { core, session } = await unlockedAttachmentSession("pending", [{ path: "client.db", bytes: encoder.encode("encrypted database") }]);
    await expect(dispatchBrowserRpc(session, { id: 1, command: "open_media", args: { id } })).resolves.toEqual({ id: 1, ok: true, value: { state: "pending" } });
    await expect(dispatchBrowserRpc(session, { id: 2, command: "load_media", args: { id } })).resolves.toEqual({ id: 2, ok: true, value: { state: "pending" } });
    expect(core.attachmentCommands).not.toContain("export_attachment");
    expect(session.phase).toBe("ready");
  });

  it("keeps snapshot and history previews serialized without checkpointing every passive preview", async () => {
    const files = [
      { path: "client.db", bytes: encoder.encode("encrypted database") },
      ...previewIds.map(attachmentId => ({ path: cipherPathFor(attachmentId), bytes: encoder.encode(`cipher-${attachmentId}`) })),
    ];
    const { core, session, store } = await unlockedAttachmentSession("ready", files, previewIds);
    const attachments = (ids: string[]) => ids.map(attachmentId => ({ id: attachmentId, mediaType: "image/png", state: "ready" }));
    core.snapshot = { activeConversationId: "conversation", conversations: [{ id: "conversation", messages: attachments(previewIds.slice(0, 12)).map(attachment => ({ attachments: [attachment] })) }] };
    core.history = { messages: attachments(previewIds.slice(12)).map(attachment => ({ attachments: [attachment] })) };
    const commit = vi.spyOn(store, "commit");

    await expect(dispatchBrowserRpc(session, { id: 1, command: "load_state", args: {} })).resolves.toMatchObject({ ok: true });
    await expect(dispatchBrowserRpc(session, { id: 2, command: "load_history", args: { conversationId: "conversation", beforeMessageId: "message" } })).resolves.toMatchObject({ ok: true });

    expect(core.attachmentCommands.filter(command => command === "preview_attachment")).toHaveLength(24);
    expect(core.attachmentCommands).not.toContain("touch_attachment_ciphertext");
    expect(commit).not.toHaveBeenCalled();
  });

  it("treats an evicted queued preview as transient pending without poisoning or caching it", async () => {
    const { core, session } = await unlockedAttachmentSession("ready", [
      { path: "client.db", bytes: encoder.encode("encrypted database") },
      { path: cipherPath, bytes: encoder.encode("cold cipher") },
    ]);
    await session.mutate("evict_attachment_ciphertext", { id });

    await expect(session.previewAttachment(id)).resolves.toBeUndefined();
    expect(core.attachmentCommands).not.toContain("preview_attachment");
    expect(session.phase).toBe("ready");
    await expect(session.previewAttachment(id)).resolves.toBeUndefined();
    expect(core.attachmentCommands.filter(command => command === "attachment_info")).toHaveLength(2);
  });

  it("poisons a preview read when ready local media is absent from the committed manifest", async () => {
    const { core, session } = await unlockedAttachmentSession("pending", [{ path: "client.db", bytes: encoder.encode("encrypted database") }]);
    core.state = "ready";

    await expect(session.previewAttachment(id)).resolves.toBeUndefined();
    expect(session.phase).toBe("closed");
  });

  it("accepts a zero-byte ready attachment without poisoning preview or save, and still rejects a genuinely missing cipher", async () => {
    const { core, session } = await unlockedAttachmentSession("ready", [
      { path: "client.db", bytes: encoder.encode("encrypted database") },
      { path: cipherPath, bytes: encoder.encode("cold cipher") },
    ]);
    core.content = new Uint8Array();
    core.previewFailure = new CoreRejectedError("unsupported");

    await expect(session.previewAttachment(id)).resolves.toBeUndefined();
    expect(session.phase).toBe("ready");
    await expect(session.exportAttachment(id)).resolves.toMatchObject({ bytes: new Uint8Array(), displayName: "cat.jpg", type: "image/jpeg" });
    await expect(session.openMediaAttachment(id, 32 * 1024 * 1024)).resolves.toMatchObject({ bytes: new Uint8Array() });
    expect(session.phase).toBe("ready");

    const missing = await unlockedAttachmentSession("pending", [{ path: "client.db", bytes: encoder.encode("encrypted database") }]);
    missing.core.state = "ready";
    await expect(missing.session.previewAttachment(id)).resolves.toBeUndefined();
    expect(missing.session.phase).toBe("closed");
  });

  it("releases a completed real open and suppresses its plaintext reply after port disconnect", async () => {
    const { promise, release } = deferred();
    const { core, session } = await unlockedAttachmentSession("ready", [
      { path: "client.db", bytes: encoder.encode("encrypted database") },
      { path: cipherPath, bytes: encoder.encode("cold cipher") },
    ]);
    const host = new BrowserWorkerHost({ locks: locks(), boot: async () => session });
    const attached = workerPort();
    await host.attach(attached);
    core.attachmentInfoGate = promise;

    attached.emit({ id: 1, command: "open_media", args: { id } });
    await vi.waitFor(() => expect(core.attachmentCommands.filter(command => command === "attachment_info")).toHaveLength(1));
    attached.emit({ id: 2, command: "disconnect", args: {} });
    await vi.waitFor(() => expect(attached.closed).toBe(true));
    release();

    await vi.waitFor(() => expect(core.attachmentCommands.filter(command => command === "release_attachment_ciphertext")).toHaveLength(1));
    expect(attached.messages).not.toContainEqual(expect.objectContaining({ id: 1 }));
    expect(session.phase).toBe("ready");
  });

  it.each(["export_attachment", "save_attachment", "publish_attachment"])("keeps a real %s RPC from exporting after queued eviction", async command => {
    const { promise, release } = deferred();
    const { core, session } = await unlockedAttachmentSession("ready", [
      { path: "client.db", bytes: encoder.encode("encrypted database") },
      { path: cipherPath, bytes: encoder.encode("cold cipher") },
    ]);
    core.attachmentInfoGate = promise;
    const upload = vi.fn(async () => ({ token: "share-token", safe_name: "public.png" }));
    const publisher: import("./publication.js").PublicCopyTransport & { origin: string } = { origin: "https://peppy.test", postPublicCopy: upload as unknown as import("./publication.js").PublicCopyTransport["postPublicCopy"] };
    const request = dispatchBrowserRpc(session, { id: 1, command, args: command === "publish_attachment" ? { id, confirmed: true } : { id } }, undefined, publisher);
    await vi.waitFor(() => expect(core.attachmentCommands.filter(value => value === "attachment_info")).toHaveLength(1));
    const eviction = session.mutate("evict_attachment_ciphertext", { id });
    release();

    await expect(eviction).resolves.toEqual({});
    await expect(request).resolves.toMatchObject({ id: 1, ok: false, error: { code: "media-pending" } });
    expect(core.attachmentCommands).not.toContain(command === "publish_attachment" ? "public_image" : "export_attachment");
    expect(session.phase).toBe("ready");
    expect(upload).not.toHaveBeenCalled();
  });
});
