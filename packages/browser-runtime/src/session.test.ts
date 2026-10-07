import "fake-indexeddb/auto";
import { describe, expect, it, vi } from "vitest";
import { CheckpointError, type CheckpointInput, IndexedDbCheckpointStore } from "./checkpoint.js";
import { CoreRejectedError, type CoreResult } from "./core.js";
import { type EmscriptenFilesystem } from "./filesystem.js";
import { BrowserSession, BrowserSessionError } from "./session.js";

const encoder = new TextEncoder();
const id = "123e4567-e89b-12d3-a456-426614174000";
const cipherPath = `client.db.media/cipher/${id}.ppss`;

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
  public async invoke({ command }: { command: string; args: Record<string, unknown> }): Promise<CoreResult> {
    if (command === "_worker_unlock_identity" && this.unlockFailure) throw this.unlockFailure;
    if (command === "mutate" && this.mutationFailure) throw this.mutationFailure;
    if (command === "_worker_checkpoint_identity") return { wrappedIdentity: this.envelope };
    if (command === "_worker_local_attachment_ids") return { attachmentIds: this.attachmentIds };
    if (command === "_worker_transport_token") { this.tokenCalls++; return { deviceToken: "private-token" }; }
    if (command === "preview_attachment") return { previewUrl: "data:image/png;base64,cHJldmlldw==" };
    if (command === "_worker_rotate_identity") this.envelope = "rotated-envelope";
    return {};
  }
}

function checkpoint(generation: number, envelope = "envelope-1", files: CheckpointInput["files"] = [{ path: "client.db", bytes: encoder.encode("encrypted database") }]): CheckpointInput {
  return { expectedGeneration: generation, files, wrappedCredentials: encoder.encode(envelope) };
}

function sessionOptions(core: TrustedCore, fs: MemoryFs, store: IndexedDbCheckpointStore, onFatal = vi.fn()) {
  return { origin: "https://peppy.test", core, filesystem: fs, checkpoint: store, onFatal };
}

describe("BrowserSession", () => {
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
});
